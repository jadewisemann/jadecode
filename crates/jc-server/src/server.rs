//! 서버 본체: 쓰기 스레드, WebSocket JSON-RPC 연결, 일지 구독, `task.md` 감시.

use crate::rpc::{self, error_codes, Error, Method, Request};
use anyhow::{Context, Result};
use futures_util::{SinkExt, StreamExt};
use jc_core::index::{self, CardRow};
use jc_core::store::{Fsync, Store};
use jc_core::taskmd::{card_state, Front};
use jc_core::topic::{card_topic, project_topic};
use jc_core::{NewEvent, Workspace};
use notify::{RecursiveMode, Watcher};
use serde_json::{json, Value};
use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};
use tokio::sync::{mpsc, oneshot, watch};
use tokio_tungstenite::tungstenite::Message;

/// 쓰기 스레드에 보내는 명령.
pub enum Cmd {
    Append {
        events: Vec<NewEvent>,
        reply: oneshot::Sender<Result<Vec<u64>, String>>,
    },
    /// 서버 소유 앞머리 칸 쓰기(`card.set_status`, `card.advance`).
    SetFront {
        card: String,
        front: Box<Front>,
        kind: String,
        body: Value,
        reply: oneshot::Sender<Result<Vec<u64>, String>>,
    },
    CheckCard {
        card: String,
    },
    Stats {
        reply: oneshot::Sender<Value>,
    },
}

pub struct Shared {
    pub root: PathBuf,
    pub head: watch::Receiver<u64>,
    pub cmd: mpsc::Sender<Cmd>,
    pub started: Instant,
    pub sent: AtomicU64,
}

#[derive(Debug, Clone)]
pub struct ServerConfig {
    pub fsync: Fsync,
    pub watch: bool,
    pub debounce: Duration,
}

impl Default for ServerConfig {
    fn default() -> Self {
        Self {
            fsync: Fsync::Batch,
            watch: true,
            debounce: Duration::from_millis(30),
        }
    }
}

/// 쓰기 스레드: `Store`를 혼자 소유한다. 여러 명령을 묶어 한 번에 커밋한다(묶음 쓰기).
fn writer_thread(
    mut st: Store,
    mut rx: mpsc::Receiver<Cmd>,
    head: watch::Sender<u64>,
) {
    let mut batches = 0u64;
    let mut appended = 0u64;
    let mut watch_checks = 0u64;
    while let Some(first) = rx.blocking_recv() {
        let mut cmds = vec![first];
        while cmds.len() < 512 {
            match rx.try_recv() {
                Ok(c) => cmds.push(c),
                Err(_) => break,
            }
        }
        let mut pend: Vec<NewEvent> = vec![];
        let mut pend_reply: Vec<oneshot::Sender<Result<Vec<u64>, String>>> = vec![];
        fn flush(
            st: &mut Store,
            ev: &mut Vec<NewEvent>,
            rp: &mut Vec<oneshot::Sender<Result<Vec<u64>, String>>>,
            batches: &mut u64,
            appended: &mut u64,
        ) {
            if ev.is_empty() {
                rp.clear();
                return;
            }
            let n = ev.len();
            match st.append(std::mem::take(ev)) {
                Ok(seqs) => {
                    for r in rp.drain(..) {
                        // 묶음 안에서 한 요청이 여러 이벤트를 넣을 수 있다. 마지막 seq를 알려 준다.
                        let _ = r.send(Ok(seqs.clone()));
                    }
                }
                Err(e) => {
                    for r in rp.drain(..) {
                        let _ = r.send(Err(e.to_string()));
                    }
                }
            }
            *batches += 1;
            *appended += n as u64;
        }
        for c in cmds {
            match c {
                Cmd::Append { events, reply } => {
                    pend.extend(events);
                    pend_reply.push(reply);
                }
                Cmd::SetFront { card, front, kind, body, reply } => {
                    flush(&mut st, &mut pend, &mut pend_reply, &mut batches, &mut appended);
                    let r = st
                        .set_front(&card, &front, &kind, body)
                        .map_err(|e| e.to_string());
                    let _ = reply.send(r);
                }
                Cmd::CheckCard { card } => {
                    flush(&mut st, &mut pend, &mut pend_reply, &mut batches, &mut appended);
                    watch_checks += 1;
                    if let Err(e) = st.check_card(&card, "watch") {
                        eprintln!("check_card {card}: {e}");
                    }
                }
                Cmd::Stats { reply } => {
                    flush(&mut st, &mut pend, &mut pend_reply, &mut batches, &mut appended);
                    let _ = reply.send(json!({
                        "batches": batches,
                        "appended": appended,
                        "last_seq": st.last_seq(),
                        "watch_checks": watch_checks,
                        "self_write_suppressed": st.stats_self_suppressed,
                        "cas_retries": st.cas_retries,
                    }));
                }
            }
        }
        flush(&mut st, &mut pend, &mut pend_reply, &mut batches, &mut appended);
        let _ = head.send(st.last_seq());
    }
}

/// `task.md` 감시: 프로젝트 폴더(새 카드)와 카드 폴더를 비재귀로 본다(워크트리 안은 안 본다).
fn start_watcher(root: PathBuf, cmd: mpsc::Sender<Cmd>, debounce: Duration) -> Result<()> {
    let (tx, rx) = std::sync::mpsc::channel::<notify::Result<notify::Event>>();
    let mut w = notify::recommended_watcher(tx)?;
    let layout = index::scan_layout(&root)?;
    let work = root.join("work");
    let mut nwatch = 0;
    if work.exists() {
        for p in std::fs::read_dir(&work)? {
            let p = p?;
            if p.file_type()?.is_dir() {
                w.watch(&p.path(), RecursiveMode::NonRecursive)?;
                nwatch += 1;
            }
        }
    }
    for c in &layout.cards {
        w.watch(&root.join(c), RecursiveMode::NonRecursive)?;
        nwatch += 1;
    }
    eprintln!("watcher: {nwatch} dirs");
    std::thread::spawn(move || {
        let mut w = w;
        let mut due: HashMap<String, Instant> = HashMap::new();
        loop {
            let timeout = due
                .values()
                .min()
                .map(|d| d.saturating_duration_since(Instant::now()))
                .unwrap_or(Duration::from_secs(3600));
            match rx.recv_timeout(timeout) {
                Ok(Ok(ev)) => {
                    // 읽기(open/close-nowrite/access)는 무시: 서버 자신의 읽기가 되먹임을 만든다.
                    if let notify::EventKind::Access(a) = ev.kind {
                        if a != notify::event::AccessKind::Close(notify::event::AccessMode::Write) {
                            continue;
                        }
                    }
                    for p in ev.paths {
                        let Ok(rel) = p.strip_prefix(&root) else { continue };
                        let parts: Vec<String> =
                            rel.iter().map(|s| s.to_string_lossy().to_string()).collect();
                        // work/<p>/<c> 새 카드 폴더
                        if parts.len() == 3 && parts[0] == "work" && ev.kind.is_create() && p.is_dir()
                        {
                            let _ = w.watch(&p, RecursiveMode::NonRecursive);
                            due.insert(parts.join("/"), Instant::now() + debounce);
                            continue;
                        }
                        if parts.len() == 4 && parts[0] == "work" && parts[3] == "task.md" {
                            due.insert(parts[..3].join("/"), Instant::now() + debounce);
                        }
                    }
                }
                Ok(Err(e)) => eprintln!("watch error: {e}"),
                Err(std::sync::mpsc::RecvTimeoutError::Timeout) => {}
                Err(_) => break,
            }
            let now = Instant::now();
            let ready: Vec<String> = due
                .iter()
                .filter(|(_, d)| **d <= now)
                .map(|(k, _)| k.clone())
                .collect();
            for k in ready {
                due.remove(&k);
                let _ = cmd.blocking_send(Cmd::CheckCard { card: k });
            }
        }
    });
    Ok(())
}

/// 서버를 띄운다. `listener`가 준비되면 `on_ready`를 부른다(테스트·CLI가 포트를 알 수 있게).
pub async fn serve<F>(
    root: PathBuf,
    bind: Bind,
    config: ServerConfig,
    on_ready: F,
) -> Result<()>
where
    F: FnOnce(&Bound),
{
    let t0 = Instant::now();
    let root = std::fs::canonicalize(&root)
        .with_context(|| format!("no such workspace: {}", root.display()))?;
    // 워크스페이스인지 확인(정본 파일이 있는 곳에서만 돈다).
    Workspace::open(&root)?;

    let r2 = root.clone();
    let fs = config.fsync;
    let (mut st, stats, reconcile) =
        tokio::task::spawn_blocking(move || Store::open(&r2, fs)).await??;
    eprintln!("recovery: {}", serde_json::to_string(&stats)?);
    let (cmd_tx, cmd_rx) = mpsc::channel::<Cmd>(4096);
    let (head_tx, head_rx) = watch::channel(st.last_seq());
    if !reconcile.is_empty() {
        st.append(reconcile)?;
        let _ = head_tx.send(st.last_seq());
    }
    std::thread::spawn(move || writer_thread(st, cmd_rx, head_tx));
    if config.watch {
        start_watcher(root.clone(), cmd_tx.clone(), config.debounce)?;
    }

    let bound = bind.listen().await?;
    on_ready(&bound);
    eprintln!(
        "jadecode server listening on {} pid={} ready_ms={}",
        bound.describe(),
        std::process::id(),
        t0.elapsed().as_millis()
    );
    let _ = std::fs::create_dir_all(root.join(".jadecode"));
    let _ = std::fs::write(
        root.join(".jadecode/server.json"),
        json!({"pid": std::process::id(), "endpoint": bound.endpoint(), "started": jc_core::event::now_ms()})
            .to_string(),
    );

    let shared = Arc::new(Shared {
        root: root.clone(),
        head: head_rx,
        cmd: cmd_tx,
        started: t0,
        sent: AtomicU64::new(0),
    });

    match bound {
        Bound::Tcp { listener, .. } => loop {
            let (sock, _) = listener.accept().await?;
            let _ = sock.set_nodelay(true);
            let sh = shared.clone();
            tokio::spawn(async move {
                match tokio_tungstenite::accept_async(sock).await {
                    Ok(ws) => {
                        if let Err(e) = handle_ws(sh, ws).await {
                            eprintln!("conn end: {e}");
                        }
                    }
                    Err(e) => eprintln!("handshake: {e}"),
                }
            });
        },
        Bound::Unix { listener, .. } => loop {
            let (sock, _) = listener.accept().await?;
            let sh = shared.clone();
            tokio::spawn(async move {
                match tokio_tungstenite::accept_async(sock).await {
                    Ok(ws) => {
                        if let Err(e) = handle_ws(sh, ws).await {
                            eprintln!("conn end: {e}");
                        }
                    }
                    Err(e) => eprintln!("handshake: {e}"),
                }
            });
        },
    }
}

/// 어디에 붙을지.
#[derive(Debug, Clone)]
pub enum Bind {
    Tcp(String, u16),
    Unix(PathBuf),
}

/// 준비된 리스너.
pub enum Bound {
    Tcp {
        listener: tokio::net::TcpListener,
        addr: std::net::SocketAddr,
    },
    Unix {
        listener: tokio::net::UnixListener,
        path: PathBuf,
    },
}

impl Bind {
    async fn listen(self) -> Result<Bound> {
        Ok(match self {
            Bind::Tcp(host, port) => {
                let l = tokio::net::TcpListener::bind((host.as_str(), port))
                    .await
                    .with_context(|| format!("bind {host}:{port}"))?;
                let addr = l.local_addr()?;
                Bound::Tcp { listener: l, addr }
            }
            Bind::Unix(path) => {
                if let Some(d) = path.parent() {
                    std::fs::create_dir_all(d)?;
                }
                let _ = std::fs::remove_file(&path);
                let l = tokio::net::UnixListener::bind(&path)?;
                use std::os::unix::fs::PermissionsExt;
                std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600))?;
                Bound::Unix { listener: l, path }
            }
        })
    }
}

impl Bound {
    pub fn describe(&self) -> String {
        match self {
            Bound::Tcp { addr, .. } => addr.to_string(),
            Bound::Unix { path, .. } => path.display().to_string(),
        }
    }
    pub fn endpoint(&self) -> Value {
        match self {
            Bound::Tcp { addr, .. } => json!({"transport":"tcp","addr":addr.to_string()}),
            Bound::Unix { path, .. } => {
                json!({"transport":"unix","path":path.display().to_string()})
            }
        }
    }
}

/// 편의 함수: 유닉스 소켓 하나로 서버를 띄운다(첫 버전 기본).
pub async fn serve_unix(
    root: PathBuf,
    socket: PathBuf,
    config: ServerConfig,
) -> Result<()> {
    serve(root, Bind::Unix(socket), config, |_| {}).await
}

async fn handle_ws<S>(sh: Arc<Shared>, ws: tokio_tungstenite::WebSocketStream<S>) -> Result<()>
where
    S: tokio::io::AsyncRead + tokio::io::AsyncWrite + Unpin + Send + 'static,
{
    let (mut sink, mut stream) = ws.split();
    // 연결당 보내기 줄 하나. 작게 묶어 느린 클라이언트가 서버 메모리를 키우지 않게 한다(TCP 역압).
    let (out_tx, mut out_rx) = mpsc::channel::<String>(64);
    let sender = tokio::spawn(async move {
        while let Some(s) = out_rx.recv().await {
            if sink.send(Message::text(s)).await.is_err() {
                break;
            }
        }
    });

    let mut subs: Vec<tokio::task::JoinHandle<()>> = vec![];
    let mut next_sub = 1u64;
    while let Some(msg) = stream.next().await {
        let msg = msg?;
        let Message::Text(t) = msg else {
            if let Message::Close(_) = msg {
                break;
            }
            continue;
        };
        let t: String = t.to_string();
        let req: Request = match serde_json::from_str(&t) {
            Ok(r) => r,
            Err(e) => {
                let _ = out_tx
                    .send(rpc::err(&Value::Null, Error::new(error_codes::PARSE, e.to_string())))
                    .await;
                continue;
            }
        };
        let Some(method) = Method::parse(&req.method) else {
            let _ = out_tx
                .send(rpc::err(
                    &req.id,
                    Error::new(error_codes::METHOD_NOT_FOUND, format!("no method {}", req.method)),
                ))
                .await;
            continue;
        };
        let id = req.id.clone();
        let params = req.params.clone();
        let sh2 = sh.clone();
        let out = out_tx.clone();

        match method {
            Method::EventsSubscribe => {
                let topics: Vec<String> = params["topics"]
                    .as_array()
                    .map(|a| a.iter().filter_map(|x| x.as_str().map(String::from)).collect())
                    .unwrap_or_default();
                let from_seq = params["from_seq"].as_u64().unwrap_or(0);
                let sid = next_sub;
                next_sub += 1;
                let head = *sh.head.borrow();
                if from_seq > head {
                    let _ = out
                        .send(rpc::err(
                            &id,
                            Error::invalid_params(format!("from_seq {from_seq} > head {head}")),
                        ))
                        .await;
                    continue;
                }
                let _ = out
                    .send(rpc::ok(
                        &id,
                        json!({"subscription": sid, "head_seq": head, "from_seq": from_seq}),
                    ))
                    .await;
                subs.push(tokio::spawn(async move {
                    if let Err(e) = subscription(sh2, sid, topics, from_seq, out).await {
                        eprintln!("sub {sid} end: {e}");
                    }
                }));
            }
            Method::EventsAppend => {
                let topic = params["topic"].as_str().unwrap_or("").to_string();
                let kind = params["kind"].as_str().unwrap_or("msg").to_string();
                let body = params.get("body").cloned().unwrap_or(Value::Null);
                tokio::spawn(async move {
                    let resp = match append(sh2, vec![NewEvent::new(topic, kind, body)]).await {
                        Ok(seqs) => rpc::ok(&id, json!({"seqs": seqs, "seq": seqs.last()})),
                        Err(e) => rpc::err(&id, e),
                    };
                    let _ = out.send(resp).await;
                });
            }
            Method::EventsHead => {
                let _ = out.send(rpc::ok(&id, json!({"seq": *sh.head.borrow()}))).await;
            }
            Method::EventsTail => {
                let sh2 = sh.clone();
                tokio::spawn(async move {
                    let resp = match events_tail(&sh2, &params).await {
                        Ok(v) => rpc::ok(&id, v),
                        Err(e) => rpc::err(&id, e),
                    };
                    let _ = out.send(resp).await;
                });
            }
            Method::ServerInfo => {
                let _ = out
                    .send(rpc::ok(
                        &id,
                        json!({
                            "name": "jadecode",
                            "version": env!("CARGO_PKG_VERSION"),
                            "methods": Method::names(),
                            "workspace": sh.root.display().to_string(),
                            "transport": "websocket+jsonrpc2",
                        }),
                    ))
                    .await;
            }
            Method::ServerStats => {
                tokio::spawn(async move {
                    let (tx, rx) = oneshot::channel();
                    let _ = sh2.cmd.send(Cmd::Stats { reply: tx }).await;
                    let mut v = rx.await.unwrap_or(json!({}));
                    v["sent_notifications"] = json!(sh2.sent.load(Ordering::Relaxed));
                    v["uptime_ms"] = json!(sh2.started.elapsed().as_millis() as u64);
                    let _ = out.send(rpc::ok(&id, v)).await;
                });
            }
            Method::ProjectsList => {
                let sh2 = sh.clone();
                tokio::spawn(async move {
                    let resp = match projects_list(&sh2) {
                        Ok(v) => rpc::ok(&id, v),
                        Err(e) => rpc::err(&id, e),
                    };
                    let _ = out.send(resp).await;
                });
            }
            Method::CardsList => {
                let sh2 = sh.clone();
                tokio::spawn(async move {
                    let resp = match cards_list(&sh2, &params) {
                        Ok(v) => rpc::ok(&id, v),
                        Err(e) => rpc::err(&id, e),
                    };
                    let _ = out.send(resp).await;
                });
            }
            Method::CardGet => {
                let sh2 = sh.clone();
                tokio::spawn(async move {
                    let resp = match card_get(&sh2, &params) {
                        Ok(v) => rpc::ok(&id, v),
                        Err(e) => rpc::err(&id, e),
                    };
                    let _ = out.send(resp).await;
                });
            }
            Method::CardAppendEvent => {
                let sh2 = sh.clone();
                tokio::spawn(async move {
                    let resp = match card_append(sh2.clone(), &params).await {
                        Ok(v) => rpc::ok(&id, v),
                        Err(e) => rpc::err(&id, e),
                    };
                    let _ = out.send(resp).await;
                });
            }
            Method::CardSetStatus => {
                let sh2 = sh.clone();
                tokio::spawn(async move {
                    let resp = match card_set_status(&sh2, &params).await {
                        Ok(v) => rpc::ok(&id, v),
                        Err(e) => rpc::err(&id, e),
                    };
                    let _ = out.send(resp).await;
                });
            }
            Method::CardAdvance => {
                let sh2 = sh.clone();
                tokio::spawn(async move {
                    let resp = match card_advance(&sh2, &params).await {
                        Ok(v) => rpc::ok(&id, v),
                        Err(e) => rpc::err(&id, e),
                    };
                    let _ = out.send(resp).await;
                });
            }
        }
    }
    for s in subs {
        s.abort();
    }
    sender.abort();
    Ok(())
}

/// 쓰기 스레드에 이벤트를 넣고 커밋을 기다린다(승인 시점 = 돌려준 seq).
async fn append(sh: Arc<Shared>, events: Vec<NewEvent>) -> Result<Vec<u64>, Error> {
    let (tx, rx) = oneshot::channel();
    sh.cmd
        .send(Cmd::Append { events, reply: tx })
        .await
        .map_err(|_| Error::internal("writer gone"))?;
    rx.await
        .map_err(|_| Error::internal("writer gone"))?
        .map_err(Error::internal)
}

async fn events_tail(sh: &Shared, params: &Value) -> Result<Value, Error> {
    let topics: Vec<String> = params["topics"]
        .as_array()
        .map(|a| a.iter().filter_map(|x| x.as_str().map(String::from)).collect())
        .unwrap_or_default();
    let limit = params["limit"].as_u64().unwrap_or(50).min(1000);
    let before = params["before_seq"].as_u64();
    let root = sh.root.clone();
    let (cond, args) = jc_core::topic::topic_sql(&topics, 2);
    let rows = tokio::task::spawn_blocking(move || -> Result<(Vec<Value>, u64)> {
        let conn = index::open_index_ro(&root)?;
        // 감시 채널(head)은 묶음 끝에 갱신되므로, 색인이 아는 마지막 seq를 직접 읽는다(승인된 seq).
        let last: u64 = conn
            .query_row(
                "SELECT COALESCE((SELECT v FROM meta WHERE k='last_seq'),0)",
                [],
                |r| r.get::<_, i64>(0),
            )
            .unwrap_or(0) as u64;
        let head = match before {
            Some(b) => b.saturating_sub(1),
            None => last,
        };
        let sql = format!(
            "SELECT line FROM events WHERE seq <= ?1 AND ({cond}) ORDER BY seq DESC LIMIT {limit}"
        );
        let mut st = conn.prepare(&sql)?;
        let mut p: Vec<Box<dyn rusqlite::ToSql>> = vec![Box::new(head as i64)];
        for a in &args {
            p.push(Box::new(a.clone()));
        }
        let pr: Vec<&dyn rusqlite::ToSql> = p.iter().map(|b| b.as_ref()).collect();
        let v: Vec<Value> = st
            .query_map(pr.as_slice(), |r| r.get::<_, String>(0))?
            .filter_map(|x| x.ok())
            .filter_map(|l| serde_json::from_str::<Value>(&l).ok())
            .collect();
        Ok((v.into_iter().rev().collect(), last))
    })
    .await
    .map_err(|e| Error::internal(e.to_string()))?
    .map_err(|e| Error::internal(e.to_string()))?;
    Ok(json!({"events": rows.0, "head_seq": rows.1}))
}

fn require_str(params: &Value, key: &str) -> Result<String, Error> {
    params
        .get(key)
        .and_then(|v| v.as_str())
        .map(String::from)
        .ok_or_else(|| Error::invalid_params(format!("params.{key} required")))
}

/// 카드 폴더를 찾는다(상대 경로·폴더 이름·ULID·ULID 앞부분).
fn resolve_card(sh: &Shared, needle: &str) -> Result<String, Error> {
    let needle = needle.trim_end_matches('/');
    if needle.is_empty() {
        return Err(Error::invalid_params("card required"));
    }
    let layout = index::scan_layout(&sh.root).map_err(|e| Error::internal(e.to_string()))?;
    let mut hits: Vec<String> = vec![];
    for c in layout.cards {
        let dir = c.rsplit('/').next().unwrap_or("").to_string();
        if dir == needle || c == needle {
            hits.push(c);
            continue;
        }
        let p = sh.root.join(&c).join("task.md");
        if let Ok(content) = std::fs::read_to_string(p) {
            if let Ok(cs) = card_state(&content) {
                if cs.front.id == needle || cs.front.id.starts_with(needle) {
                    hits.push(c);
                }
            }
        }
    }
    match hits.len() {
        1 => Ok(hits.remove(0)),
        0 => Err(Error::invalid_params(format!("no card matches {needle:?}"))),
        n => Err(Error::invalid_params(format!(
            "{n} cards match {needle:?}; be more specific"
        ))),
    }
}

fn card_rows(sh: &Shared) -> Result<Vec<CardRow>, Error> {
    let conn = index::open_index_ro(&sh.root).map_err(|e| Error::internal(e.to_string()))?;
    index::card_rows(&conn).map_err(|e| Error::internal(e.to_string()))
}

fn cards_list(sh: &Shared, params: &Value) -> Result<Value, Error> {
    let project = params.get("project").and_then(|v| v.as_str());
    let stage = params.get("stage").and_then(|v| v.as_str());
    let rows: Vec<Value> = card_rows(sh)?
        .into_iter()
        .filter(|r| project.map(|p| r.path.starts_with(&format!("work/{p}/"))).unwrap_or(true))
        .filter(|r| stage.map(|s| r.stage == s).unwrap_or(true))
        .map(|r| {
            json!({"path":r.path,"id":r.id,"title":r.title,"project":r.project,
                   "stage":r.stage,"status":r.status})
        })
        .collect();
    Ok(json!({"cards": rows, "head_seq": *sh.head.borrow()}))
}

fn card_get(sh: &Shared, params: &Value) -> Result<Value, Error> {
    let needle = require_str(params, "card")?;
    let rel = resolve_card(sh, &needle)?;
    let content = std::fs::read_to_string(sh.root.join(&rel).join("task.md"))
        .map_err(|e| Error::internal(e.to_string()))?;
    let cs = card_state(&content).map_err(|e| Error::internal(e.to_string()))?;
    let row = card_rows(sh)?.into_iter().find(|r| r.path == rel);
    Ok(json!({
        "path": rel,
        "id": cs.front.id,
        "title": cs.front.title,
        "project": cs.front.project,
        "front": cs.front,
        "stage": cs.front.stage,
        "status": cs.front.status,
        "indexed": row.map(|r| json!({"stage":r.stage,"status":r.status,"file_hash":r.file_hash})),
        "markdown": content,
        "head_seq": *sh.head.borrow(),
    }))
}

async fn card_append(sh: Arc<Shared>, params: &Value) -> Result<Value, Error> {
    let needle = require_str(params, "card")?;
    let rel = resolve_card(&sh, &needle)?;
    let kind = params
        .get("kind")
        .and_then(|v| v.as_str())
        .unwrap_or("card.note")
        .to_string();
    if !kind.starts_with("card.") && !kind.starts_with("msg.") && !kind.starts_with("instance.") {
        return Err(Error::invalid_params("kind must start with card./msg./instance."));
    }
    let body = params.get("body").cloned().unwrap_or(Value::Null);
    let seqs = append(sh.clone(), vec![NewEvent::new(card_topic(&rel), kind.clone(), body)]).await?;
    Ok(json!({"path": rel, "kind": kind, "seqs": seqs}))
}

/// `task.md` 앞머리의 서버 소유 칸을 새로 쓴다.
async fn set_front_cmd(
    sh: &Shared,
    rel: &str,
    front: Front,
    kind: &str,
    body: Value,
) -> Result<Vec<u64>, Error> {
    let (tx, rx) = oneshot::channel();
    sh.cmd
        .send(Cmd::SetFront {
            card: rel.to_string(),
            front: Box::new(front),
            kind: kind.to_string(),
            body,
            reply: tx,
        })
        .await
        .map_err(|_| Error::internal("writer gone"))?;
    rx.await
        .map_err(|_| Error::internal("writer gone"))?
        .map_err(Error::internal)
}

async fn card_set_status(sh: &Shared, params: &Value) -> Result<Value, Error> {
    let needle = require_str(params, "card")?;
    let status = require_str(params, "status")?;
    const OK: &[&str] = &["idle", "working", "blocked", "waiting_human", "done", "failed"];
    if !OK.contains(&status.as_str()) {
        return Err(Error::invalid_params(format!(
            "status must be one of {OK:?}"
        )));
    }
    let rel = resolve_card(sh, &needle)?;
    let content = std::fs::read_to_string(sh.root.join(&rel).join("task.md"))
        .map_err(|e| Error::internal(e.to_string()))?;
    let cs = card_state(&content).map_err(|e| Error::internal(e.to_string()))?;
    let from = cs.front.status.clone();
    let mut front = cs.front.clone();
    front.status = status.clone();
    let seqs = set_front_cmd(
        sh,
        &rel,
        front,
        "card.status_changed",
        json!({"from": from, "to": status, "source": "rpc"}),
    )
    .await?;
    Ok(json!({"path": rel, "status": status, "seqs": seqs}))
}

/// 다음 단계로 옮긴다. 경로 끝이면 거절한다(완료는 별도 사람 절차).
async fn card_advance(sh: &Shared, params: &Value) -> Result<Value, Error> {
    let needle = require_str(params, "card")?;
    let rel = resolve_card(sh, &needle)?;
    let content = std::fs::read_to_string(sh.root.join(&rel).join("task.md"))
        .map_err(|e| Error::internal(e.to_string()))?;
    let cs = card_state(&content).map_err(|e| Error::internal(e.to_string()))?;
    let Some(next) = cs.front.next_stage() else {
        return Err(Error::refused(
            format!("card at end of path ({})", cs.front.stage),
            "path_end",
        ));
    };
    let next = next.to_string();
    let from = cs.front.stage.clone();
    let mut front = cs.front.clone();
    front.path_index += 1;
    front.stage = next.clone();
    front.status = "idle".into();
    let seqs = set_front_cmd(
        sh,
        &rel,
        front,
        "card.stage_changed",
        json!({"from": from, "to": next, "source": "rpc"}),
    )
    .await?;
    Ok(json!({"path": rel, "stage": next, "seqs": seqs}))
}

fn projects_list(sh: &Shared) -> Result<Value, Error> {
    let layout = index::scan_layout(&sh.root).map_err(|e| Error::internal(e.to_string()))?;
    let cards = card_rows(sh)?;
    let rows: Vec<Value> = layout
        .projects
        .iter()
        .map(|p| {
            let slug = p.rsplit('/').next().unwrap_or("").to_string();
            let toml = std::fs::read_to_string(sh.root.join(p).join("project.toml")).ok();
            let name = toml
                .as_deref()
                .and_then(|s| s.parse::<toml::Table>().ok())
                .and_then(|t| t.get("name").and_then(|v| v.as_str().map(String::from)))
                .unwrap_or_else(|| slug.clone());
            let n = cards.iter().filter(|c| c.path.starts_with(&format!("{p}/"))).count();
            json!({"path":p,"slug":slug,"topic":project_topic(p),"name":name,"cards":n})
        })
        .collect();
    Ok(json!({"projects": rows}))
}

/// 구독: `from_seq` 뒤 이벤트를 색인에서 읽어 보낸다. 구독자별 버퍼가 없다.
async fn subscription(
    sh: Arc<Shared>,
    sid: u64,
    topics: Vec<String>,
    from_seq: u64,
    out: mpsc::Sender<String>,
) -> Result<()> {
    const LIMIT: i64 = 512;
    let root = sh.root.clone();
    let conn = Arc::new(Mutex::new(index::open_index_ro(&root)?));
    let (cond, args) = jc_core::topic::topic_sql(&topics, 3);
    let sql = format!(
        "SELECT seq, line FROM events WHERE seq > ?1 AND seq <= ?2 AND ({cond}) ORDER BY seq LIMIT {LIMIT}"
    );
    let mut cursor = from_seq;
    let mut head_rx = sh.head.clone();
    loop {
        let head = *head_rx.borrow_and_update();
        if head > cursor {
            let c = conn.clone();
            let sql2 = sql.clone();
            let args2 = args.clone();
            let rows: Vec<(u64, String)> = tokio::task::spawn_blocking(
                move || -> Result<Vec<(u64, String)>> {
                    let c = c.lock().unwrap();
                    let mut st = c.prepare_cached(&sql2)?;
                    let mut p: Vec<Box<dyn rusqlite::ToSql>> =
                        vec![Box::new(cursor as i64), Box::new(head as i64)];
                    for a in &args2 {
                        p.push(Box::new(a.clone()));
                    }
                    let pr: Vec<&dyn rusqlite::ToSql> = p.iter().map(|b| b.as_ref()).collect();
                    let v = st
                        .query_map(pr.as_slice(), |r| {
                            Ok((r.get::<_, i64>(0)? as u64, r.get::<_, String>(1)?))
                        })?
                        .collect::<Result<Vec<_>, _>>()?;
                    Ok(v)
                },
            )
            .await??;
            let n = rows.len();
            let mut last = cursor;
            for (seq, line) in rows {
                let msg = format!(
                    "{{\"jsonrpc\":\"2.0\",\"method\":\"events.event\",\"params\":{{\"subscription\":{sid},\"event\":{line}}}}}"
                );
                out.send(msg).await.map_err(|_| anyhow::anyhow!("closed"))?;
                sh.sent.fetch_add(1, Ordering::Relaxed);
                last = seq;
            }
            cursor = if (n as i64) < LIMIT { head } else { last };
            continue;
        }
        head_rx.changed().await?;
    }
}
