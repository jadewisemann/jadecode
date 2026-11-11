//! 서버 본체: 쓰기 스레드, WebSocket JSON-RPC 연결, 일지 구독, `task.md` 감시.

use crate::rpc::{self, error_codes, Error, Method, Request};
use anyhow::{bail, Context, Result};
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
    /// 새로 만든 카드를 색인에 등록한다(사건 없음).
    RegisterCard {
        card: String,
        reply: oneshot::Sender<Result<(), String>>,
    },
    /// 인스턴스 관리자가 카드 상태를 바꾼다(권한 대기 등). `task.md`가 없으면 무시한다.
    SetFrontRaw {
        card: String,
        status: Option<String>,
        stage: Option<String>,
        kind: String,
        body: Value,
        reply: oneshot::Sender<Result<Vec<u64>, String>>,
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
    pub instances: crate::instances::InstanceManager,
    /// 백엔드별 실행 파일 경로(설정에서 온 것).
    pub agent_bins: std::sync::Mutex<std::collections::BTreeMap<String, String>>,
}

impl Shared {
    /// 이벤트를 일지에 넣는다(관리자·RPC 공통 경로).
    pub async fn append(&self, events: Vec<NewEvent>) -> Result<Vec<u64>, Error> {
        let (tx, rx) = oneshot::channel();
        self.cmd
            .send(Cmd::Append { events, reply: tx })
            .await
            .map_err(|_| Error::internal("writer gone"))?;
        rx.await
            .map_err(|_| Error::internal("writer gone"))?
            .map_err(Error::internal)
    }
}

#[derive(Debug, Clone)]
pub struct ServerConfig {
    pub fsync: Fsync,
    pub watch: bool,
    pub debounce: Duration,
    /// 인스턴스가 붙을 MCP 소켓 경로(서버가 여는 소켓과 같아야 한다).
    pub mcp_socket: PathBuf,
    /// `jadecode-mcp` 바이너리 경로(없으면 PATH에서 찾는다).
    pub mcp_bin: Option<PathBuf>,
    /// `claude` 바이너리 경로(없으면 PATH).
    pub claude_bin: Option<String>,
    /// 오케스트레이터 깨우기 모으기 간격.
    pub wake_interval: Duration,
    /// 백엔드별 실행 파일 경로(`jadecode.toml`의 `[agents]`).
    pub bins: std::collections::BTreeMap<String, String>,
}

impl Default for ServerConfig {
    fn default() -> Self {
        Self {
            fsync: Fsync::Batch,
            watch: true,
            debounce: Duration::from_millis(30),
            mcp_socket: PathBuf::new(),
            mcp_bin: None,
            claude_bin: None,
            wake_interval: Duration::from_millis(300),
            bins: Default::default(),
        }
    }
}

/// `jadecode-mcp`를 찾는다: 환경변수 → 실행 파일 옆 → PATH.
fn default_mcp_bin() -> PathBuf {
    if let Some(p) = std::env::var_os("JADECODE_MCP") {
        return PathBuf::from(p);
    }
    if let Ok(exe) = std::env::current_exe() {
        let sib = exe.with_file_name("jadecode-mcp");
        if sib.exists() {
            return sib;
        }
    }
    PathBuf::from("jadecode-mcp")
}

/// 쓰기 스레드: `Store`를 혼자 소유한다. 여러 명령을 묶어 한 번에 커밋한다(묶음 쓰기).
fn writer_thread(mut st: Store, mut rx: mpsc::Receiver<Cmd>, head: watch::Sender<u64>) {
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
                Cmd::SetFront {
                    card,
                    front,
                    kind,
                    body,
                    reply,
                } => {
                    flush(
                        &mut st,
                        &mut pend,
                        &mut pend_reply,
                        &mut batches,
                        &mut appended,
                    );
                    let r = st
                        .set_front(&card, &front, &kind, body)
                        .map_err(|e| e.to_string());
                    let _ = reply.send(r);
                }
                Cmd::RegisterCard { card, reply } => {
                    flush(
                        &mut st,
                        &mut pend,
                        &mut pend_reply,
                        &mut batches,
                        &mut appended,
                    );
                    let _ = reply.send(st.register_card(&card).map_err(|e| e.to_string()));
                }
                Cmd::SetFrontRaw {
                    card,
                    status,
                    stage,
                    kind,
                    body,
                    reply,
                } => {
                    flush(
                        &mut st,
                        &mut pend,
                        &mut pend_reply,
                        &mut batches,
                        &mut appended,
                    );
                    let r = set_front_raw(
                        &mut st,
                        &card,
                        status.as_deref(),
                        stage.as_deref(),
                        &kind,
                        body,
                    )
                    .map_err(|e| e.to_string());
                    let _ = reply.send(r);
                }
                Cmd::CheckCard { card } => {
                    flush(
                        &mut st,
                        &mut pend,
                        &mut pend_reply,
                        &mut batches,
                        &mut appended,
                    );
                    watch_checks += 1;
                    if let Err(e) = st.check_card(&card, "watch") {
                        eprintln!("check_card {card}: {e}");
                    }
                }
                Cmd::Stats { reply } => {
                    flush(
                        &mut st,
                        &mut pend,
                        &mut pend_reply,
                        &mut batches,
                        &mut appended,
                    );
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
        flush(
            &mut st,
            &mut pend,
            &mut pend_reply,
            &mut batches,
            &mut appended,
        );
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
                        let Ok(rel) = p.strip_prefix(&root) else {
                            continue;
                        };
                        let parts: Vec<String> = rel
                            .iter()
                            .map(|s| s.to_string_lossy().to_string())
                            .collect();
                        // work/<p>/<c> 새 카드 폴더
                        if parts.len() == 3
                            && parts[0] == "work"
                            && ev.kind.is_create()
                            && p.is_dir()
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
pub async fn serve<F>(root: PathBuf, bind: Bind, config: ServerConfig, on_ready: F) -> Result<()>
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
        json!({"pid": std::process::id(), "endpoint": bound.endpoint(), "started": jc_core::event::now_ms(),
               "socket": match &bound { Bound::Unix{path,..} => path.display().to_string(), _ => String::new() }})
            .to_string(),
    );

    // MCP 소켓은 인스턴스가 붙을 곳이다. 지정이 없으면 연 소켓을 그대로 쓴다(Unix일 때).
    let socket_for_mcp = if config.mcp_socket.as_os_str().is_empty() {
        match &bound {
            Bound::Unix { path, .. } => path.clone(),
            Bound::Tcp { .. } => PathBuf::new(),
        }
    } else {
        config.mcp_socket.clone()
    };
    let claude_bin = config
        .claude_bin
        .clone()
        .or_else(|| std::env::var("JADECODE_CLAUDE").ok())
        .unwrap_or_else(|| "claude".to_string());
    let mcp_bin = config
        .mcp_bin
        .clone()
        .or_else(|| std::env::var("JADECODE_MCP").ok().map(PathBuf::from))
        .unwrap_or_else(default_mcp_bin);
    let instances = crate::instances::InstanceManager::spawn(
        root.clone(),
        cmd_tx.clone(),
        claude_bin,
        mcp_bin,
        socket_for_mcp,
        config.wake_interval,
    );
    let shared = Arc::new(Shared {
        root: root.clone(),
        head: head_rx,
        cmd: cmd_tx,
        started: t0,
        sent: AtomicU64::new(0),
        instances,
        agent_bins: std::sync::Mutex::new(config.bins.clone()),
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
                // macOS의 유닉스 소켓 경로 한도(sun_path 104바이트)를 넘으면 bind가
                // 알 수 없는 오류로 실패한다. 원인을 바로 알 수 있게 먼저 검사한다.
                let len = path.as_os_str().len();
                let limit = if cfg!(target_os = "macos") { 103 } else { 107 };
                if len > limit {
                    bail!(
                        "유닉스 소켓 경로가 너무 길다({len}자 > {limit}자): {}. \
                         더 짧은 경로를 --socket 으로 주거나 워크스페이스를 짧은 곳에 두라 \
                         (예: /tmp/jc / ~/jc)",
                        path.display()
                    );
                }
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
pub async fn serve_unix(root: PathBuf, socket: PathBuf, config: ServerConfig) -> Result<()> {
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
                    .send(rpc::err(
                        &Value::Null,
                        Error::new(error_codes::PARSE, e.to_string()),
                    ))
                    .await;
                continue;
            }
        };
        let Some(method) = Method::parse(&req.method) else {
            let _ = out_tx
                .send(rpc::err(
                    &req.id,
                    Error::new(
                        error_codes::METHOD_NOT_FOUND,
                        format!("no method {}", req.method),
                    ),
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
                    .map(|a| {
                        a.iter()
                            .filter_map(|x| x.as_str().map(String::from))
                            .collect()
                    })
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
                let _ = out
                    .send(rpc::ok(&id, json!({"seq": *sh.head.borrow()})))
                    .await;
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
            Method::CardAdd => {
                let sh2 = sh.clone();
                tokio::spawn(async move {
                    let resp = match card_add(sh2, &params).await {
                        Ok(v) => rpc::ok(&id, v),
                        Err(e) => rpc::err(&id, e),
                    };
                    let _ = out.send(resp).await;
                });
            }
            Method::ProjectAdd => {
                let sh2 = sh.clone();
                tokio::spawn(async move {
                    let resp = match project_add(sh2, &params).await {
                        Ok(v) => rpc::ok(&id, v),
                        Err(e) => rpc::err(&id, e),
                    };
                    let _ = out.send(resp).await;
                });
            }
            Method::InstancesList => {
                let sh2 = sh.clone();
                tokio::spawn(async move {
                    let mut v = json!({"instances": sh2.instances.list().await});
                    v["head_seq"] = json!(*sh2.head.borrow());
                    let _ = out.send(rpc::ok(&id, v)).await;
                });
            }
            Method::InstanceStart => {
                let sh2 = sh.clone();
                tokio::spawn(async move {
                    let resp = match instance_start(sh2, &params).await {
                        Ok(v) => rpc::ok(&id, v),
                        Err(e) => rpc::err(&id, e),
                    };
                    let _ = out.send(resp).await;
                });
            }
            Method::InstanceSend => {
                let sh2 = sh.clone();
                tokio::spawn(async move {
                    let resp = match instance_send(&sh2, &params).await {
                        Ok(v) => rpc::ok(&id, v),
                        Err(e) => rpc::err(&id, e),
                    };
                    let _ = out.send(resp).await;
                });
            }
            Method::InstanceStop => {
                let sh2 = sh.clone();
                tokio::spawn(async move {
                    let inst = match require_str(&params, "instance") {
                        Ok(x) => x,
                        Err(e) => {
                            let _ = out.send(rpc::err(&id, e)).await;
                            return;
                        }
                    };
                    let resp = match sh2.instances.stop(&inst).await {
                        Ok(()) => rpc::ok(&id, json!({"instance": inst, "stopped": true})),
                        Err(e) => rpc::err(&id, Error::refused(e.to_string(), "instance")),
                    };
                    let _ = out.send(resp).await;
                });
            }
            Method::PermissionRequest => {
                let sh2 = sh.clone();
                tokio::spawn(async move {
                    let resp = match permission_request(sh2, &params).await {
                        Ok(v) => rpc::ok(&id, v),
                        Err(e) => rpc::err(&id, e),
                    };
                    let _ = out.send(resp).await;
                });
            }
            Method::PermissionResolve => {
                let sh2 = sh.clone();
                tokio::spawn(async move {
                    let seq = params["request_seq"].as_u64().unwrap_or(0);
                    let allow = params["allow"].as_bool().unwrap_or(false);
                    let msg = params["message"].as_str().unwrap_or("").to_string();
                    let resp = match sh2.instances.resolve_permission(seq, allow, &msg).await {
                        Ok(()) => rpc::ok(&id, json!({"request_seq": seq, "allow": allow})),
                        Err(e) => rpc::err(&id, Error::refused(e.to_string(), "permission")),
                    };
                    let _ = out.send(resp).await;
                });
            }
            Method::BotsList => {
                let sh2 = sh.clone();
                let resp = bots_list(&sh2)
                    .map(|v| rpc::ok(&id, v))
                    .unwrap_or_else(|e| rpc::err(&id, e));
                let _ = out.send(resp).await;
            }
            Method::AgentsList => {
                let sh2 = sh.clone();
                let resp = agents_list(&sh2)
                    .map(|v| rpc::ok(&id, v))
                    .unwrap_or_else(|e| rpc::err(&id, e));
                let _ = out.send(resp).await;
            }
            Method::PolicyGet => {
                let sh2 = sh.clone();
                let resp = match policy_get(&sh2) {
                    Ok(v) => rpc::ok(&id, v),
                    Err(e) => rpc::err(&id, e),
                };
                let _ = out.send(resp).await;
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

/// 카드 상태·단계 칸을 바꾸고 사건을 남긴다. 인스턴스 관리자·권한 처리에서 쓴다.
fn set_front_raw(
    st: &mut Store,
    card: &str,
    status: Option<&str>,
    stage: Option<&str>,
    kind: &str,
    body: Value,
) -> Result<Vec<u64>> {
    let p = st.root.join(card).join("task.md");
    let content = std::fs::read_to_string(&p)?;
    let cs = card_state(&content)?;
    let mut front = cs.front.clone();
    if let Some(s) = status {
        front.status = s.to_string();
    }
    if let Some(s) = stage {
        front.stage = s.to_string();
        if let Some(i) = front.path.iter().position(|x| x == s) {
            front.path_index = i;
        }
    }
    st.set_front(card, &front, kind, body)
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
        .map(|a| {
            a.iter()
                .filter_map(|x| x.as_str().map(String::from))
                .collect()
        })
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
        .filter(|r| {
            project
                .map(|p| r.path.starts_with(&format!("work/{p}/")))
                .unwrap_or(true)
        })
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
        return Err(Error::invalid_params(
            "kind must start with card./msg./instance.",
        ));
    }
    let body = params.get("body").cloned().unwrap_or(Value::Null);
    let seqs = append(
        sh.clone(),
        vec![NewEvent::new(card_topic(&rel), kind.clone(), body)],
    )
    .await?;
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
    const OK: &[&str] = &[
        "idle",
        "working",
        "blocked",
        "waiting_human",
        "done",
        "failed",
    ];
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
            let n = cards
                .iter()
                .filter(|c| c.path.starts_with(&format!("{p}/")))
                .count();
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
            let rows: Vec<(u64, String)> =
                tokio::task::spawn_blocking(move || -> Result<Vec<(u64, String)>> {
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
                })
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

// ---------- 인스턴스·권한·봇·정책 RPC ----------

/// `instance.start {role, project, card?, bot, model?, id?}`
async fn instance_start(sh: Arc<Shared>, params: &Value) -> Result<Value, Error> {
    let role = crate::instances::Role::parse(
        params
            .get("role")
            .and_then(|v| v.as_str())
            .unwrap_or("worker"),
    );
    let project = require_str(params, "project")?;
    let bot = params
        .get("bot")
        .and_then(|v| v.as_str())
        .unwrap_or("worker")
        .to_string();
    let card = match role {
        crate::instances::Role::Worker => Some(resolve_card(&sh, &require_str(params, "card")?)?),
        crate::instances::Role::Orchestrator => None,
    };
    let model = params
        .get("model")
        .and_then(|v| v.as_str())
        .map(String::from);
    let agent_override = params.get("agent").and_then(|v| v.as_str());
    let id = params
        .get("id")
        .and_then(|v| v.as_str())
        .map(String::from)
        .unwrap_or_else(|| {
            let tag = match role {
                crate::instances::Role::Worker => "w",
                crate::instances::Role::Orchestrator => "o",
            };
            format!("{tag}-{}", jc_core::ulid::new_id().to_lowercase())
        });
    let mut spec = crate::instances::build_spec(&sh.root, id, role, project, card, &bot, model)
        .map_err(|e| Error::invalid_params(e.to_string()))?;
    if let Some(a) = agent_override {
        spec.agent =
            jc_driver::AgentKind::parse(a).map_err(|e| Error::invalid_params(e.to_string()))?;
    }
    let info = sh
        .instances
        .start(spec)
        .await
        .map_err(|e| Error::internal(e.to_string()))?;
    Ok(json!({"instance": info}))
}

/// `instance.send {instance, text}`
async fn instance_send(sh: &Shared, params: &Value) -> Result<Value, Error> {
    let inst = require_str(params, "instance")?;
    let text = require_str(params, "text")?;
    sh.instances
        .send(&inst, &text)
        .await
        .map_err(|e| Error::refused(e.to_string(), "instance"))?;
    Ok(json!({"instance": inst, "sent": true}))
}

/// `permission.request {tool_name, input, tool_use_id, instance}`
/// Claude Code의 `--permission-prompt-tool`이 부른다. 응답 계약을 그대로 돌려준다.
async fn permission_request(sh: Arc<Shared>, params: &Value) -> Result<Value, Error> {
    let inst = require_str(params, "instance")?;
    let tool = params
        .get("tool_name")
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .to_string();
    let input = params.get("input").cloned().unwrap_or(json!({}));
    let tuid = params
        .get("tool_use_id")
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .to_string();
    // AskUserQuestion은 권한 RPC가 곧 질문 RPC다(S2). 질문 메시지로 바꾼다.
    if tool == "AskUserQuestion" {
        return ask_user_question(sh, &inst, &input).await;
    }
    let d = sh
        .instances
        .permission(&inst, &tool, input.clone(), &tuid)
        .await;
    Ok(if d.allow {
        json!({"behavior": "allow", "updatedInput": input})
    } else {
        json!({"behavior": "deny", "message": d.message})
    })
}

/// AskUserQuestion → `msg.ask` 이벤트 → 사람/오케스트레이터가 `permission.resolve`로 답하면
/// `updatedInput.answers`로 돌려준다. 지금은 답이 오면 allow, 시간 초과면 deny.
async fn ask_user_question(sh: Arc<Shared>, inst: &str, input: &Value) -> Result<Value, Error> {
    let qs = input
        .get("questions")
        .and_then(|v| v.as_array())
        .cloned()
        .unwrap_or_default();
    let first = qs.first().cloned().unwrap_or(json!({}));
    let q = first
        .get("question")
        .and_then(|v| v.as_str())
        .unwrap_or("질문")
        .to_string();
    let options: Vec<String> = first
        .get("options")
        .and_then(|v| v.as_array())
        .map(|a| {
            a.iter()
                .filter_map(|o| o.get("label").and_then(|l| l.as_str()).map(String::from))
                .collect()
        })
        .unwrap_or_default();
    let (topic, card) = {
        let list = sh.instances.list().await;
        match list.into_iter().find(|i| i.id == inst) {
            Some(i) => (
                match &i.card {
                    Some(c) => jc_core::topic::card_topic(c),
                    None => format!("project/{}", i.project),
                },
                i.card,
            ),
            None => ("global".to_string(), None),
        }
    };
    let seqs = sh
        .append(vec![NewEvent::new(
            topic,
            "msg.ask",
            json!({"question": q, "options": options, "via":"AskUserQuestion","instance":inst,
                   "actor":{"role":"worker","instance":inst,"card":card}}),
        )])
        .await?;
    let seq = *seqs.first().unwrap_or(&0);
    // 사람 답은 `permission.resolve`로 온다. 여기서는 구현을 단순하게: 바로 deny하고
    // 질문 이벤트로 남긴다(답은 다음 턴으로 전달 — S3가 시험한 방식).
    let _ = seq;
    Ok(json!({
        "behavior": "deny",
        "message": "이 질문은 jadecode가 이벤트로 남겼다. 답이 오면 새 메시지로 전달된다. 지금은 하던 일을 계속하거나 기다려라.",
    }))
}

/// `bots.list`
fn bots_list(sh: &Shared) -> Result<Value, Error> {
    let dir = sh.root.join("bots");
    let mut out = vec![];
    if dir.is_dir() {
        let mut names: Vec<PathBuf> = std::fs::read_dir(&dir)
            .map_err(|e| Error::internal(e.to_string()))?
            .filter_map(|e| e.ok())
            .map(|e| e.path())
            .filter(|p| p.is_dir())
            .collect();
        names.sort();
        for p in names {
            let name = p
                .file_name()
                .unwrap_or_default()
                .to_string_lossy()
                .to_string();
            match jc_driver::spec::BotSpec::load(&p) {
                Ok(spec) => out.push(json!({
                    "name": spec.name, "dir": name, "title": spec.title,
                    "agent": spec.agent, "model": spec.model, "policy": spec.policy,
                    "instructions": spec.instructions, "skills": spec.skills,
                })),
                Err(e) => out.push(json!({"name": name, "dir": name, "error": e.to_string()})),
            }
        }
    }
    Ok(json!({"bots": out}))
}

/// `policy.get` — 사람이 정한 값(봇 수 상한·예산·사람 절차·권한 시간 상한).
fn policy_get(sh: &Shared) -> Result<Value, Error> {
    let p = sh.root.join("policy/policy.toml");
    let text = std::fs::read_to_string(&p).unwrap_or_default();
    let table: toml::Table = text.parse().unwrap_or_default();
    let v = serde_json::to_value(table).map_err(|e| Error::internal(e.to_string()))?;
    let mut out = json!({"policy": v, "path": p.display().to_string()});
    out["defaults"] = json!({
        "permission_timeout_min": 25,
        "human_procedures": ["merge_protected_branch","deploy","external_message","budget_exceeded","project_delete"],
        "bots_max": 10,
    });
    Ok(out)
}

// ---------- 생성 RPC(서버가 돌 때 CLI가 쓴다) ----------

/// `card.add {project, title, path?, repos?, depends_on?, parent?, body?}`
async fn card_add(sh: Arc<Shared>, params: &Value) -> Result<Value, Error> {
    let project = require_str(params, "project")?;
    let title = require_str(params, "title")?;
    let path: Vec<String> = params["path"]
        .as_array()
        .map(|a| {
            a.iter()
                .filter_map(|x| x.as_str().map(String::from))
                .collect()
        })
        .unwrap_or_else(|| {
            ["refine", "implement", "verify", "pr", "report"]
                .iter()
                .map(|s| s.to_string())
                .collect()
        });
    let repos: Vec<jc_core::taskmd::RepoRef> = params["repos"]
        .as_array()
        .map(|a| {
            a.iter()
                .filter_map(|r| serde_json::from_value(r.clone()).ok())
                .collect()
        })
        .unwrap_or_default();
    let depends_on: Vec<String> = params["depends_on"]
        .as_array()
        .map(|a| {
            a.iter()
                .filter_map(|x| x.as_str().map(String::from))
                .collect()
        })
        .unwrap_or_default();
    let parent = params
        .get("parent")
        .and_then(|v| v.as_str())
        .map(String::from);
    let body = params
        .get("body")
        .and_then(|v| v.as_str())
        .map(String::from);
    let nc = jc_core::NewCard {
        project,
        title,
        path,
        repos,
        depends_on,
        parent,
        body,
    };
    let root = sh.root.clone();
    let (rel, id, front) =
        tokio::task::spawn_blocking(move || jc_core::card::materialize(&root, &nc))
            .await
            .map_err(|e| Error::internal(e.to_string()))?
            .map_err(|e| Error::invalid_params(e.to_string()))?;
    let (rtx, rrx) = oneshot::channel();
    sh.cmd
        .send(Cmd::RegisterCard {
            card: rel.clone(),
            reply: rtx,
        })
        .await
        .map_err(|_| Error::internal("writer gone"))?;
    let _ = rrx.await;
    let seqs = sh
        .append(vec![
            NewEvent::new(
                card_topic(&rel),
                "card.created",
                json!({"id":id,"title":front.title,"project":front.project,"path":front.path,
                       "repos":front.repos,"depends_on":front.depends_on,"parent":front.parent}),
            ),
            NewEvent::new(
                project_topic(&format!("work/{}", front.project)),
                "card.added",
                json!({"card":rel,"id":id,"title":front.title}),
            ),
        ])
        .await?;
    Ok(json!({"path": rel, "id": id, "seqs": seqs}))
}

/// `project.add {name, repo?}`
async fn project_add(sh: Arc<Shared>, params: &Value) -> Result<Value, Error> {
    let name = require_str(params, "name")?;
    let repo = params
        .get("repo")
        .and_then(|v| v.as_str())
        .map(String::from);
    let root = sh.root.clone();
    let slug: String = name
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() {
                c.to_ascii_lowercase()
            } else {
                '-'
            }
        })
        .collect::<String>()
        .split('-')
        .filter(|s| !s.is_empty())
        .collect::<Vec<_>>()
        .join("-");
    if slug.is_empty() {
        return Err(Error::invalid_params(
            "project name needs ascii letters or digits",
        ));
    }
    let rel = format!("work/{slug}");
    let abs = root.join(&rel);
    if abs.exists() {
        return Err(Error::refused(format!("project exists: {slug}"), "exists"));
    }
    tokio::fs::create_dir_all(&abs)
        .await
        .map_err(|e| Error::internal(e.to_string()))?;
    let repos = match &repo {
        Some(r) => format!("[[repos]]\nname = \"{r}\"\n"),
        None => String::new(),
    };
    tokio::fs::write(
        abs.join("project.toml"),
        format!(
            "# jadecode 프로젝트\nname = \"{}\"\nslug = \"{slug}\"\n{repos}",
            name.replace('"', "'")
        ),
    )
    .await
    .map_err(|e| Error::internal(e.to_string()))?;
    let seqs = sh
        .append(vec![NewEvent::new(
            project_topic(&rel),
            "project.created",
            json!({"name":name,"slug":slug,"repos":repo}),
        )])
        .await?;
    Ok(json!({"path": rel, "slug": slug, "seqs": seqs}))
}

/// `agents.list` — 지원 백엔드와 설치 여부.
fn agents_list(sh: &Shared) -> Result<Value, Error> {
    let bins = sh.agent_bins.lock().unwrap().clone();
    let rows: Vec<Value> = jc_driver::AgentKind::all()
        .iter()
        .map(|k| {
            let explicit = bins.get(k.as_str()).map(|s| s.as_str());
            jc_driver::any::probe(*k, explicit)
        })
        .collect();
    Ok(json!({"agents": rows, "default": "claude"}))
}
