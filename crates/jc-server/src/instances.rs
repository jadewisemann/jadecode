//! 인스턴스 관리자: 봇 인스턴스를 띄우고, 턴을 넣고, 이벤트를 일지로 옮기고, 죽으면 이어 붙인다.
//!
//! 스파이크 반영(S1·S2·S3):
//! - 인스턴스당 `claude` 프로세스는 하나(R-a). 죽으면 같은 카드 폴더에서 `--resume`.
//! - 시작 때 init의 `mcp_servers[]`에 `jadecode`가 `connected`가 아니면 바로 실패로 두고 다시 띄운다(S2).
//! - 턴이 도구 호출 없이 끝났고 `report`가 없었으면 마지막 응답을 `report(auto)`로 남긴다.
//!   단 자동 기록은 **깨우지 않는다**(S3 규칙 1).
//! - 오케스트레이터는 깨우는 이벤트를 0.3초 모아 다이제스트 사용자 턴으로 받는다(S3 H2).

use anyhow::{Context, Result};
use jc_core::index;
use jc_core::topic::card_topic;
use jc_core::NewEvent;
use jc_driver::proc::{instance_dir, Driver, DriverConfig};
use jc_driver::spec::{BotSpec, PermissionPolicy};
use jc_driver::stream::DriverEvent;
use jc_driver::stream::AssistantBlock;
use serde::Serialize;
use serde_json::{json, Value};
use std::collections::{BTreeMap, HashMap};
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;
use tokio::sync::{mpsc, oneshot};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum Role {
    Worker,
    Orchestrator,
}

impl Role {
    pub fn as_str(&self) -> &'static str {
        match self {
            Role::Worker => "worker",
            Role::Orchestrator => "orchestrator",
        }
    }
    pub fn parse(s: &str) -> Self {
        if s == "orchestrator" {
            Role::Orchestrator
        } else {
            Role::Worker
        }
    }
}

#[derive(Debug, Clone)]
pub struct InstanceSpec {
    pub id: String,
    pub role: Role,
    pub project: String,
    /// 카드 상대 경로(`work/<p>/<c>`). 오케스트레이터는 없음.
    pub card: Option<String>,
    pub bot_dir: PathBuf,
    pub bot: BotSpec,
    pub policy: PermissionPolicy,
    pub session_id: String,
    pub cwd: PathBuf,
    pub add_dirs: Vec<PathBuf>,
    /// 사람이 정한 값: 승인 없이 허용할 도구 규칙. 비면 규칙 밖은 사람에게 묻는다.
    pub auto_allow: Vec<String>,
    /// 사람 답을 기다리는 상한(분). CLI 무응답 중단(1800초)보다 짧아야 한다.
    pub permission_timeout_min: u64,
}

impl InstanceSpec {
    pub fn journal_topic(&self) -> String {
        match (&self.card, self.role) {
            (Some(c), _) => card_topic(c),
            (None, _) => format!("project/{}", self.project),
        }
    }
    pub fn cwd_label(&self) -> String {
        self.cwd.display().to_string()
    }
}

#[derive(Debug, Clone, Serialize)]
pub struct InstanceInfo {
    pub id: String,
    pub role: String,
    pub project: String,
    pub card: Option<String>,
    pub bot: String,
    pub model: Option<String>,
    pub session_id: String,
    pub pid: u32,
    pub state: String,
    pub turns: u64,
    pub started_ms: u64,
    pub last_report: Option<String>,
}

/// 인스턴스에 보내는 명령.
enum InstMsg {
    Prompt {
        text: String,
        reply: oneshot::Sender<Result<()>>,
    },
    Stop {
        reply: oneshot::Sender<Result<()>>,
    },
}

/// 관리자에 보내는 명령.
pub enum ManagerMsg {
    Start {
        spec: Box<InstanceSpec>,
        reply: oneshot::Sender<Result<InstanceInfo>>,
    },
    Send {
        instance: String,
        text: String,
        reply: oneshot::Sender<Result<()>>,
    },
    Stop {
        instance: String,
        reply: oneshot::Sender<Result<()>>,
    },
    List {
        reply: oneshot::Sender<Vec<InstanceInfo>>,
    },
    /// 권한 RPC. 정책으로 답하고, 규칙 밖이면 사람에게 올린다.
    Permission {
        instance: String,
        tool: String,
        input: Value,
        tool_use_id: String,
        reply: oneshot::Sender<PermissionDecision>,
    },
    /// 사람(또는 오케스트레이터)이 권한 요청에 답했다.
    ResolvePermission {
        request_seq: u64,
        allow: bool,
        message: String,
        reply: oneshot::Sender<Result<()>>,
    },
    Stats {
        reply: oneshot::Sender<Value>,
    },
}

#[derive(Debug, Clone, Serialize)]
pub struct PermissionDecision {
    pub allow: bool,
    pub message: String,
    pub seq: Option<u64>,
}

struct Live {
    tx: mpsc::Sender<InstMsg>,
    info: Arc<Mutex<InstanceInfo>>,
}

pub struct InstanceManager {
    tx: mpsc::Sender<ManagerMsg>,
    pub instances_dir: PathBuf,
}

impl InstanceManager {
    pub fn spawn(
        root: PathBuf,
        store_cmd: mpsc::Sender<crate::server::Cmd>,
        claude_bin: String,
        mcp_bin: PathBuf,
        socket: PathBuf,
        wake_interval: Duration,
    ) -> Self {
        let (tx, rx) = mpsc::channel::<ManagerMsg>(512);
        let instances_dir = root.join(".jadecode/instances");
        let mgr = Manager {
            root: root.clone(),
            live: HashMap::new(),
            pending_perms: Arc::new(Mutex::new(HashMap::new())),
            store_cmd: store_cmd.clone(),
            claude_bin,
            mcp_bin,
            socket,
            wake_cursor: HashMap::new(),
            started: jc_core::event::now_ms(),
        };
        tokio::spawn(mgr.run(rx, wake_interval));
        Self { tx, instances_dir }
    }

    pub async fn start(&self, spec: InstanceSpec) -> Result<InstanceInfo> {
        let (tx, rx) = oneshot::channel();
        self.tx
            .send(ManagerMsg::Start {
                spec: Box::new(spec),
                reply: tx,
            })
            .await
            .map_err(|_| anyhow::anyhow!("manager gone"))?;
        rx.await?
    }

    pub async fn send(&self, instance: &str, text: &str) -> Result<()> {
        let (tx, rx) = oneshot::channel();
        self.tx
            .send(ManagerMsg::Send {
                instance: instance.to_string(),
                text: text.to_string(),
                reply: tx,
            })
            .await
            .map_err(|_| anyhow::anyhow!("manager gone"))?;
        rx.await?
    }

    pub async fn stop(&self, instance: &str) -> Result<()> {
        let (tx, rx) = oneshot::channel();
        self.tx
            .send(ManagerMsg::Stop {
                instance: instance.to_string(),
                reply: tx,
            })
            .await
            .map_err(|_| anyhow::anyhow!("manager gone"))?;
        rx.await?
    }

    pub async fn list(&self) -> Vec<InstanceInfo> {
        let (tx, rx) = oneshot::channel();
        if self.tx.send(ManagerMsg::List { reply: tx }).await.is_err() {
            return vec![];
        }
        rx.await.unwrap_or_default()
    }

    pub async fn permission(
        &self,
        instance: &str,
        tool: &str,
        input: Value,
        tool_use_id: &str,
    ) -> PermissionDecision {
        let (tx, rx) = oneshot::channel();
        if self
            .tx
            .send(ManagerMsg::Permission {
                instance: instance.to_string(),
                tool: tool.to_string(),
                input,
                tool_use_id: tool_use_id.to_string(),
                reply: tx,
            })
            .await
            .is_err()
        {
            return PermissionDecision {
                allow: false,
                message: "권한 서버 없음".into(),
                seq: None,
            };
        }
        rx.await.unwrap_or(PermissionDecision {
            allow: false,
            message: "권한 처리 중단".into(),
            seq: None,
        })
    }

    pub async fn resolve_permission(&self, seq: u64, allow: bool, message: &str) -> Result<()> {
        let (tx, rx) = oneshot::channel();
        self.tx
            .send(ManagerMsg::ResolvePermission {
                request_seq: seq,
                allow,
                message: message.to_string(),
                reply: tx,
            })
            .await
            .map_err(|_| anyhow::anyhow!("manager gone"))?;
        rx.await?
    }

    pub async fn stats(&self) -> Value {
        let (tx, rx) = oneshot::channel();
        if self.tx.send(ManagerMsg::Stats { reply: tx }).await.is_err() {
            return json!({});
        }
        rx.await.unwrap_or_else(|_| json!({}))
    }
}

struct Manager {
    root: PathBuf,
    live: HashMap<String, Live>,
    /// 권한 요청 seq → 응답자. 사람 답을 기다리는 일은 별도 태스크로 넘긴다
    /// (관리자 루프가 막히면 깨우기·메시지가 모두 멈춘다).
    pending_perms: Arc<Mutex<HashMap<u64, oneshot::Sender<(bool, String)>>>>,
    store_cmd: mpsc::Sender<crate::server::Cmd>,
    claude_bin: String,
    mcp_bin: PathBuf,
    socket: PathBuf,
    /// 오케스트레이터별 마지막으로 다이제스트한 seq
    wake_cursor: HashMap<String, u64>,
    started: u64,
}

impl Manager {
    async fn emit(&self, events: Vec<NewEvent>) -> Result<Vec<u64>> {
        if events.is_empty() {
            return Ok(vec![]);
        }
        let (tx, rx) = oneshot::channel();
        self.store_cmd
            .send(crate::server::Cmd::Append { events, reply: tx })
            .await
            .map_err(|_| anyhow::anyhow!("writer gone"))?;
        rx.await
            .map_err(|_| anyhow::anyhow!("writer gone"))?
            .map_err(|e| anyhow::anyhow!(e))
    }

    async fn run(mut self, mut rx: mpsc::Receiver<ManagerMsg>, wake_interval: Duration) {
        let mut tick = tokio::time::interval(wake_interval);
        tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
        loop {
            tokio::select! {
                msg = rx.recv() => {
                    let Some(msg) = msg else { break };
                    self.handle(msg).await;
                }
                _ = tick.tick() => {
                    self.wake_loop().await;
                }
            }
        }
        // 종료 시 인스턴스 정리
        let ids: Vec<String> = self.live.keys().cloned().collect();
        for id in ids {
            if let Some(l) = self.live.remove(&id) {
                let (tx, _rx) = oneshot::channel();
                let _ = l.tx.send(InstMsg::Stop { reply: tx }).await;
            }
        }
    }

    async fn handle(&mut self, msg: ManagerMsg) {
        match msg {
            ManagerMsg::Start { spec, reply } => {
                let r = self.start(*spec).await;
                let _ = reply.send(r);
            }
            ManagerMsg::Send { instance, text, reply } => {
                let r = match self.live.get(&instance) {
                    Some(l) => {
                        let (tx, rx) = oneshot::channel();
                        match l.tx.send(InstMsg::Prompt { text, reply: tx }).await {
                            Ok(_) => rx.await.unwrap_or_else(|_| Err(anyhow::anyhow!("instance gone"))),
                            Err(_) => Err(anyhow::anyhow!("instance gone")),
                        }
                    }
                    None => Err(anyhow::anyhow!("no such instance: {instance}")),
                };
                let _ = reply.send(r);
            }
            ManagerMsg::Stop { instance, reply } => {
                let r = match self.live.remove(&instance) {
                    Some(l) => {
                        let (tx, rx) = oneshot::channel();
                        let _ = l.tx.send(InstMsg::Stop { reply: tx }).await;
                        rx.await.unwrap_or_else(|_| Err(anyhow::anyhow!("instance gone")))
                    }
                    None => Err(anyhow::anyhow!("no such instance: {instance}")),
                };
                let _ = reply.send(r);
            }
            ManagerMsg::List { reply } => {
                let v = self.infos();
                let _ = reply.send(v);
            }
            ManagerMsg::Permission { instance, tool, input, tool_use_id, reply } => {
                // 답을 기다리는 동안 관리자 루프를 막지 않는다.
                let root = self.root.clone();
                let store_cmd = self.store_cmd.clone();
                let pending = self.pending_perms.clone();
                let infos = self.infos();
                let timeout_min = infos
                    .iter()
                    .find(|i| i.id == instance)
                    .map(|i| self.permission_timeout_for(i))
                    .unwrap_or(25);
                tokio::spawn(async move {
                    let d = decide_permission_task(
                        root, store_cmd, pending, infos, instance, tool, input, tool_use_id, timeout_min,
                    )
                    .await;
                    let _ = reply.send(d);
                });
            }
            ManagerMsg::ResolvePermission { request_seq, allow, message, reply } => {
                let taken = self.pending_perms.lock().unwrap().remove(&request_seq);
                let r = match taken {
                    Some(tx) => tx
                        .send((allow, message))
                        .map_err(|_| anyhow::anyhow!("waiter gone")),
                    None => Err(anyhow::anyhow!("no pending permission {request_seq}")),
                };
                let _ = reply.send(r);
            }
            ManagerMsg::Stats { reply } => {
                let (tx, rx) = oneshot::channel();
                let _ = self.store_cmd.send(crate::server::Cmd::Stats { reply: tx }).await;
                let mut v = rx.await.unwrap_or_else(|_| json!({}));
                v["instances"] = json!(self.infos());
                v["pending_permissions"] = json!(self.pending_perms.lock().unwrap().len());
                v["manager_uptime_ms"] = json!(jc_core::event::now_ms().saturating_sub(self.started));
                let _ = reply.send(v);
            }
        }
    }

    fn infos(&self) -> Vec<InstanceInfo> {
        let mut v: Vec<InstanceInfo> = self
            .live
            .values()
            .map(|l| l.info.lock().unwrap().clone())
            .collect();
        v.sort_by(|a, b| a.id.cmp(&b.id));
        v
    }

    async fn start(&mut self, spec: InstanceSpec) -> Result<InstanceInfo> {
        if self.live.contains_key(&spec.id) {
            anyhow::bail!("instance exists: {}", spec.id);
        }
        if spec.role == Role::Worker && spec.card.is_none() {
            anyhow::bail!("worker needs a card");
        }
        let dir = instance_dir(&self.root, &spec.id);
        std::fs::create_dir_all(&dir)?;
        let cfg = DriverConfig {
            cwd: spec.cwd.clone(),
            session_id: spec.session_id.clone(),
            bot_dir: spec.bot_dir.clone(),
            bot: spec.bot.clone(),
            policy: spec.policy.clone(),
            mcp_command: self.mcp_bin.display().to_string(),
            mcp_args: vec![
                "--socket".into(),
                self.socket.display().to_string(),
                "--role".into(),
                spec.role.as_str().into(),
                "--instance".into(),
                spec.id.clone(),
                "--project".into(),
                spec.project.clone(),
            ]
            .into_iter()
            .chain(spec.card.clone().map(|c| vec!["--card".into(), c]).unwrap_or_default())
            .collect(),
            mcp_env: BTreeMap::new(),
            add_dirs: spec.add_dirs.clone(),
            resume: false,
            claude_bin: self.claude_bin.clone(),
            state_dir: Some(dir.clone()),
            max_turns: None,
            extra_args: vec![],
        };
        let driver = Driver::spawn(cfg).await?;
        let info = Arc::new(Mutex::new(InstanceInfo {
            id: spec.id.clone(),
            role: spec.role.as_str().into(),
            project: spec.project.clone(),
            card: spec.card.clone(),
            bot: spec.bot.name.clone(),
            model: spec.bot.model.clone(),
            session_id: spec.session_id.clone(),
            pid: driver.pid,
            state: "starting".into(),
            turns: 0,
            started_ms: jc_core::event::now_ms(),
            last_report: None,
        }));
        let (tx, rx) = mpsc::channel::<InstMsg>(128);
        self.live.insert(
            spec.id.clone(),
            Live {
                tx,
                info: info.clone(),
            },
        );
        let seqs = self
            .emit(vec![NewEvent::new(
                spec.journal_topic(),
                "instance.started",
                json!({
                    "instance": spec.id,
                    "role": spec.role.as_str(),
                    "bot": spec.bot.name,
                    "bot_model": spec.bot.model,
                    "session_id": spec.session_id,
                    "pid": driver.pid,
                    "cwd": spec.cwd_label(),
                    "card": spec.card,
                    "policy": spec.policy.default_mode,
                }),
            )])
            .await?;
        let started_seq = seqs.first().copied().unwrap_or(0);
        // 인스턴스 태스크
        let root = self.root.clone();
        let store_cmd = self.store_cmd.clone();
        let spec2 = spec.clone();
        tokio::spawn(run_instance(spec2, driver, rx, info, store_cmd, root, started_seq));
        Ok(self.live.get(&spec.id).unwrap().info.lock().unwrap().clone())
    }

    fn permission_timeout_for(&self, _info: &InstanceInfo) -> u64 {
        // S2: 처리기 상한(25분) < CLI 무응답 중단(1800초).
        std::fs::read_to_string(self.root.join("policy/policy.toml"))
            .ok()
            .and_then(|s| s.parse::<toml::Table>().ok())
            .and_then(|t| t.get("permission_timeout_min").and_then(|v| v.as_integer()))
            .map(|x| x.clamp(1, 25) as u64)
            .unwrap_or(25)
    }

    /// 깨우는 이벤트를 모아 오케스트레이터에게 다이제스트 턴으로 넣는다(S3).
    async fn wake_loop(&mut self) {
        let orchestrators: Vec<InstanceInfo> = self
            .infos()
            .into_iter()
            .filter(|i| i.role == "orchestrator" && i.state != "exited")
            .collect();
        if orchestrators.is_empty() {
            return;
        }
        let root = self.root.clone();
        let rows = tokio::task::spawn_blocking(move || -> Result<Vec<(u64, String, String, String)>> {
            let conn = index::open_index_ro(&root)?;
            let mut st = conn.prepare(
                "SELECT seq, topic, kind, line FROM events WHERE seq > ?1 ORDER BY seq LIMIT 500",
            )?;
            let from = 0i64;
            let v = st
                .query_map([from], |r| {
                    Ok((
                        r.get::<_, i64>(0)? as u64,
                        r.get::<_, String>(1)?,
                        r.get::<_, String>(2)?,
                        r.get::<_, String>(3)?,
                    ))
                })?
                .filter_map(|x| x.ok())
                .collect();
            Ok(v)
        })
        .await
        .ok()
        .and_then(|r| r.ok())
        .unwrap_or_default();

        for inst in orchestrators {
            let cursor = *self.wake_cursor.get(&inst.id).unwrap_or(&0);
            // 처음 보는 오케스트레이터는 지금 head부터 본다(과거 이벤트를 몰아 받지 않는다).
            if cursor == 0 {
                let head = rows.last().map(|r| r.0).unwrap_or(0);
                self.wake_cursor.insert(inst.id.clone(), head);
                continue;
            }
            let prefix = format!("project/{}", inst.project);
            let mut picked: Vec<(u64, String, Value)> = vec![];
            let mut max_seq = cursor;
            for (seq, topic, kind, line) in &rows {
                if *seq <= cursor {
                    continue;
                }
                max_seq = max_seq.max(*seq);
                if !(topic == &prefix || topic.starts_with(&format!("{prefix}/"))) {
                    continue;
                }
                if !is_wake_kind(kind) {
                    continue;
                }
                let Ok(v) = serde_json::from_str::<Value>(line) else { continue };
                // 자기 자신이 낸 이벤트는 깨우지 않는다.
                if v["body"]["actor"]["role"].as_str() == Some("orchestrator") {
                    continue;
                }
                // 자동 기록(report auto)은 깨우지 않는다(S3 규칙 1).
                if kind == "msg.report" && v["body"]["auto"].as_bool().unwrap_or(false) {
                    continue;
                }
                picked.push((*seq, kind.clone(), v));
            }
            if picked.is_empty() {
                if max_seq > cursor {
                    self.wake_keys_settle(&inst.id, max_seq);
                }
                continue;
            }
            let last = picked.last().map(|p| p.0).unwrap_or(max_seq);
            let digest = digest_text(&inst.project, &picked);
            let sent = match self.live.get(&inst.id) {
                Some(l) => {
                    let (tx, rx) = oneshot::channel();
                    match l.tx.send(InstMsg::Prompt { text: digest, reply: tx }).await {
                        Ok(_) => rx.await.map(|r| r.is_ok()).unwrap_or(false),
                        Err(_) => false,
                    }
                }
                None => false,
            };
            if sent {
                self.wake_cursor.insert(inst.id.clone(), last);
                let _ = self
                    .emit(vec![NewEvent::new(
                        format!("instance/{}", inst.id),
                        "instance.wake_digest",
                        json!({"events": picked.len(), "upto": last}),
                    )])
                    .await;
            } else {
                // 못 넣었으면 다음 tick에 다시 시도한다(이벤트는 남아 있다).
                self.wake_keys_settle(&inst.id, cursor);
            }
        }
    }

    fn wake_keys_settle(&mut self, id: &str, seq: u64) {
        self.wake_cursor.insert(id.to_string(), seq);
    }
}

/// 깨우는 이벤트 종류.
fn is_wake_kind(kind: &str) -> bool {
    matches!(
        kind,
        "msg.report_done"
            | "msg.ask"
            | "msg.propose"
            | "msg.error"
            | "msg.escalation"
            | "instance.exited"
            | "card.blocked_on_permission"
            | "card.stage_changed"
            | "card.file_missing"
            | "card.state_change_rejected"
    )
}

fn digest_text(project: &str, picked: &[(u64, String, Value)]) -> String {
    let mut s = format!(
        "[jadecode] 프로젝트 `{project}`에서 깨울 이벤트 {}건. 마지막 seq {}.\n\
         아래는 그동안 일어난 일이다. 필요한 행동을 하고, 처리한 마지막 seq를 `ack` 도구로 남겨라.\n",
        picked.len(),
        picked.last().map(|p| p.0).unwrap_or(0)
    );
    for (seq, kind, v) in picked {
        let b = &v["body"];
        let who = b["actor"]["instance"].as_str().unwrap_or("?");
        let card = b["actor"]["card"].as_str().unwrap_or("");
        s.push_str(&format!(
            "- #{seq} {kind} from {who}{}: {}\n",
            if card.is_empty() {
                String::new()
            } else {
                format!(" ({card})")
            },
            compact_body(b)
        ));
    }
    s.push_str(&format!(
        "\n필요하면 `send`·`reply`로 인스턴스에 지시하고, 사람 결정이 필요하면 `escalate`를 써라. 마지막 seq: {}",
        picked.last().map(|p| p.0).unwrap_or(0)
    ));
    s
}

fn compact_body(b: &Value) -> String {
    let pick = |k: &str| b.get(k).and_then(|x| x.as_str()).unwrap_or("").to_string();
    let mut parts = vec![];
    for k in ["status", "summary", "question", "message", "text", "from", "to"] {
        let v = pick(k);
        if !v.is_empty() {
            parts.push(format!("{k}={v}"));
        }
    }
    if parts.is_empty() {
        let s = b.to_string();
        s.chars().take(200).collect()
    } else {
        parts.join(" ")
    }
}

/// 상태 갱신은 짧게 잡는다(잠금이 await를 넘지 않게).
fn set_info(info: &Mutex<InstanceInfo>, f: impl FnOnce(&mut InstanceInfo)) {
    f(&mut info.lock().unwrap());
}

/// 인스턴스 하나의 실행 루프. 프로세스가 죽으면 같은 폴더에서 `--resume`으로 이어 붙인다.
async fn run_instance(
    spec: InstanceSpec,
    mut driver: Driver,
    mut rx: mpsc::Receiver<InstMsg>,
    info: Arc<Mutex<InstanceInfo>>,
    store_cmd: mpsc::Sender<crate::server::Cmd>,
    root: PathBuf,
    started_seq: u64,
) {
    let mut turns: u64 = 0;
    let mut restarts = 0u32;
    let mut stopping = false;
    let mut last_report_in_turn = false;
    let mut saw_tool_in_turn = false;
    let mut last_text: Option<String> = None;
    let mut _idle_turns_without_report = 0u32;
    let _ = started_seq;

    fn emit(store_cmd: &mpsc::Sender<crate::server::Cmd>, events: Vec<NewEvent>) {
        if events.is_empty() {
            return;
        }
        let (tx, _rx) = oneshot::channel();
        let _ = store_cmd.try_send(crate::server::Cmd::Append { events, reply: tx });
    }

    let topic = spec.journal_topic();
    // 첫 브리프를 넣는다. 이 턴이 init을 만들고 작업을 시작한다.
    let brief = build_brief(&spec);
    if !brief.is_empty() {
        match driver.prompt(&brief).await {
            Ok(()) => {
                set_info(&info, |i| i.state = "busy".into());
                emit(
                    &store_cmd,
                    vec![NewEvent::new(
                        topic.clone(),
                        "instance.briefed",
                        json!({"instance":spec.id,"chars":brief.len()}),
                    )],
                );
            }
            Err(e) => {
                emit(
                    &store_cmd,
                    vec![NewEvent::new(
                        topic.clone(),
                        "instance.failed",
                        json!({"instance":spec.id,"error":e.to_string()}),
                    )],
                );
                return;
            }
        }
    }
    loop {
        tokio::select! {
            biased;
            msg = rx.recv() => {
                match msg {
                    Some(InstMsg::Prompt { text, reply }) => {
                        match driver.prompt(&text).await {
                            Ok(_) => {
                                set_info(&info, |i| i.state = "busy".into());
                                last_report_in_turn = false;
                                saw_tool_in_turn = false;
                                let _ = reply.send(Ok(()));
                            }
                            Err(e) => { let _ = reply.send(Err(e)); }
                        }
                    }
                    Some(InstMsg::Stop { reply }) => {
                        stopping = true;
                        let r = driver.terminate().await;
                        let _ = reply.send(r);
                        break;
                    }
                    None => { stopping = true; break; }
                }
            }
            ev = driver.next_event() => {
                match ev {
                    None => {
                        // 프로세스가 끝났다.
                        if stopping { break; }
                        set_info(&info, |i| i.state = "exited".into());
                        emit(&store_cmd, vec![NewEvent::new(topic.clone(), "instance.exited",
                            json!({"instance":spec.id,"turns":turns,"restarts":restarts,"reason":"process_end"}))]);
                        // claude가 한가할 때 stdin이 닫히면 정상 종료다. 아니면 다시 띄운다.
                        restarts += 1;
                        if restarts > 5 { break; }
                        let mut cfg_driver = driver.config.clone();
                        cfg_driver.resume = true;
                        match Driver::spawn(cfg_driver).await {
                            Ok(d) => {
                                driver = d;
                                set_info(&info, |i| {
                                    i.state = "starting".into();
                                    i.pid = driver.pid;
                                });
                                emit(&store_cmd, vec![NewEvent::new(topic.clone(), "instance.restarted",
                                    json!({"instance":spec.id,"resume":true,"attempt":restarts,"pid":driver.pid}))]);
                                // 재시작 전에 밀린 지시가 있으면 한 번 넣는다.
                                let pending = format!(
                                    "[jadecode] 이전 프로세스가 끝나 같은 세션으로 다시 이어 붙였다. 카드 `{}` 작업을 이어서 하라. 진행 상황을 `report`로 남겨라.",
                                    spec.card.clone().unwrap_or_default()
                                );
                                if driver.prompt(&pending).await.is_ok() {
                                    set_info(&info, |i| i.state = "busy".into());
                                }
                            }
                            Err(e) => {
                                emit(&store_cmd, vec![NewEvent::new(topic.clone(), "instance.failed",
                                    json!({"instance":spec.id,"error":e.to_string()}))]);
                                break;
                            }
                        }
                    }
                    Some(ev) => {
                        match &ev {
                            DriverEvent::Init { session_id, mcp_servers, model, .. } => {
                                let jc = mcp_servers.iter().find(|m| m.name == "jadecode");
                                let connected = jc.map(|m| m.status == "connected").unwrap_or(false);
                                let model2 = model.clone();
                                set_info(&info, |i| {
                                    i.state = "idle".into();
                                    if !model2.is_empty() {
                                        i.model = Some(model2);
                                    }
                                });
                                emit(&store_cmd, vec![NewEvent::new(topic.clone(), "instance.init",
                                    json!({"instance":spec.id,"session_id":session_id,"model":model,
                                            "mcp_jadecode": jc.map(|m| m.status.clone()),
                                            "mcp_connected": connected}))]);
                                // MCP가 붙지 않았으면 첫 권한 요청에서 result 없이 죽는다(S2 H2). 바로 다시 띄운다.
                                if !connected {
                                    emit(&store_cmd, vec![NewEvent::new(topic.clone(), "instance.mcp_missing",
                                        json!({"instance":spec.id,"servers":mcp_servers}))]);
                                    stopping = false;
                                    let mut cfg_driver = driver.config.clone();
                                    cfg_driver.resume = true;
                                    restarts += 1;
                                    if restarts <= 5 {
                                        if let Ok(d) = Driver::spawn(cfg_driver).await {
                                            driver = d;
                                            let pid = driver.pid;
                                            set_info(&info, |i| i.pid = pid);
                                        } else { break; }
                                    } else { break; }
                                }
                            }
                            DriverEvent::Assistant { blocks, .. } => {
                                for b in blocks {
                                    match b {
                                        AssistantBlock::Text { text } => {
                                            if !text.trim().is_empty() {
                                                last_text = Some(text.clone());
                                                emit(&store_cmd, vec![NewEvent::new(topic.clone(), "instance.output",
                                                    json!({"instance":spec.id,"kind":"text","text":text}))]);
                                            }
                                        }
                                        AssistantBlock::ToolUse { name, input, .. } => {
                                            saw_tool_in_turn = true;
                                            if name.contains("report") { last_report_in_turn = true; }
                                            emit(&store_cmd, vec![NewEvent::new(topic.clone(), "instance.tool_use",
                                                json!({"instance":spec.id,"tool":name,"input":input}))]);
                                        }
                                        _ => {}
                                    }
                                }
                                set_info(&info, |i| i.state = "busy".into());
                            }
                            DriverEvent::Result { is_error, text, usage, num_turns, .. } => {
                                turns += 1;
                                set_info(&info, |i| {
                                    i.state = "idle".into();
                                    i.turns = turns;
                                });
                                let summary = text.clone().or_else(|| last_text.clone());
                                emit(&store_cmd, vec![NewEvent::new(topic.clone(), "instance.turn_end",
                                    json!({"instance":spec.id,"turn":turns,"num_turns":num_turns,
                                            "is_error":is_error,"usage":{"input":usage.input_tokens,"output":usage.output_tokens,
                                                "cache_create":usage.cache_creation_input_tokens,"cache_read":usage.cache_read_input_tokens,
                                                "cost_usd":usage.cost_usd,"model":usage.model},
                                            "text":summary}))]);
                                // 도구 없는 턴은 "알겠다/기다리겠다"일 수 있다. 자동 보고는 깨우지 않는다(S3 규칙 1).
                                if !last_report_in_turn && !saw_tool_in_turn {
                                    _idle_turns_without_report += 1;
                                    if let Some(t) = &summary {
                                        emit(&store_cmd, vec![NewEvent::new(topic.clone(), "msg.report",
                                            json!({"status":"progress","summary":t,"auto":true,
                                                   "actor":{"role":"worker","instance":spec.id,"card":spec.card}}))]);
                                    }
                                } else if last_report_in_turn {
                                    _idle_turns_without_report = 0;
                                }
                                if *is_error {
                                    emit(&store_cmd, vec![NewEvent::new(topic.clone(), "instance.turn_error",
                                        json!({"instance":spec.id,"turn":turns,"text":summary}))]);
                                }
                                // 턴이 끝나면 큐에 쌓인 지시를 넣는다(오케스트레이터 메시지는 프롬프트로 온다).
                                last_report_in_turn = false;
                                saw_tool_in_turn = false;
                            }
                            _ => {}
                        }
                    }
                }
            }
        }
    }
    set_info(&info, |i| i.state = "exited".into());
    if !stopping {
        emit(&store_cmd, vec![NewEvent::new(topic.clone(), "instance.exited",
            json!({"instance":spec.id,"turns":turns,"restarts":restarts}))]);
    }
    let _ = root;
}

/// 인스턴스에 넣는 첫 브리프. 카드 문서·단계·규칙·도구 이름을 담는다(S3: 지침에 도구 이름을 적으면
/// 모델이 바로 찾았다).
pub fn build_brief(spec: &InstanceSpec) -> String {
    let mut s = String::new();
    match (&spec.card, spec.role) {
        (Some(card), _) => {
            let p = spec.cwd.join("task.md");
            let doc = std::fs::read_to_string(&p).unwrap_or_default();
            let cs = jc_core::taskmd::card_state(&doc).ok();
            s.push_str(&format!(
                "[jadecode] 카드 `{card}`에서 일을 시작한다. 카드 문서는 `task.md`(이 폴더)에 있다.\n\n"
            ));
            if let Some(cs) = &cs {
                s.push_str(&format!(
                    "현재 단계: **{}** / 상태: {}\n경로: {}\n\n",
                    cs.front.stage,
                    cs.front.status,
                    cs.front.path.join(" → ")
                ));
            }
            s.push_str("카드 앞머리 `jadecode:` 묶음은 서버 소유다. 본문(목표·범위·완료 조건 등)은 네가 고칠 수 있다.\n\n");
            s.push_str("## 카드 문서\n\n");
            s.push_str(&doc);
            s.push_str("\n\n## 이번 턴에서 할 일\n\n");
            s.push_str("1. 카드의 목표·완료 조건을 읽고 지금 단계에서 할 일을 정한다.\n");
            s.push_str("2. 일을 한다(파일 읽기·편집, 명령 실행, 테스트).\n");
            s.push_str("3. 끝나면 `report` 도구를 부른다. status=done(단계 완료)·progress(계속)·blocked(막힘).\n");
            s.push_str("   사람 승인이 필요한 일이면 직접 하지 말고 `propose`로 올린다.\n");
            s.push_str("   답이 필요하면 `ask`로 묻고, 할 수 있는 일을 계속한다.\n");
            s.push_str("4. 이 폴더 안에서는 `cd` 대신 cwd 기준 경로를 쓰고, 파일 쓰기는 Write/Edit 도구를 쓴다.\n");
            if !spec.add_dirs.is_empty() {
                s.push_str("\n추가로 열어 준 폴더(레포 워크트리):\n");
                for d in &spec.add_dirs {
                    s.push_str(&format!("- `{}`\n", d.display()));
                }
            }
        }
        (None, Role::Orchestrator) => {
            let p = spec.cwd.join("project.toml");
            let doc = std::fs::read_to_string(&p).unwrap_or_default();
            s.push_str(&format!(
                "[jadecode] 프로젝트 `{}`의 오케스트레이터로 시작한다. 프로젝트 폴더는 `{}`다.\n\n",
                spec.project,
                spec.cwd.display()
            ));
            s.push_str("## project.toml\n\n");
            s.push_str(&doc);
            s.push_str("\n\n## 역할\n\n");
            s.push_str("- 사람의 의도를 받아 카드를 만들고 단계 경로를 정한다. `card_add` 도구를 쓴다(project·title, path 선택).\n");
            s.push_str("- 워커를 띄우려면 `instance_start` 도구를 쓴다(card, bot=worker).\n");
            s.push_str("- 카드를 다음 단계로 옮기려면 `card_advance`를 쓴다.\n");
            s.push_str("- 워커 메시지가 오면 다이제스트 사용자 턴으로 전달된다. 처리한 마지막 seq를 `ack`로 남긴다.\n");
            s.push_str("- 사람 결정이 필요하면 `escalate`로 올린다. 네가 승인하지 않는다.\n");
            s.push_str("- 지금은 사람의 첫 지시를 기다린다. 필요하면 `read_event`로 최근 이벤트를 본다.\n");
        }
        _ => {}
    }
    s
}

/// 봇 폴더를 읽어 인스턴스 spec을 만든다(서버 RPC에서 쓴다).
pub fn build_spec(
    root: &PathBuf,
    id: String,
    role: Role,
    project: String,
    card: Option<String>,
    bot_name: &str,
    model_override: Option<String>,
) -> Result<InstanceSpec> {
    let bot_dir = root.join("bots").join(bot_name);
    let mut bot = BotSpec::load(&bot_dir)?;
    if let Some(m) = model_override {
        bot.model = Some(m);
    }
    let policy = PermissionPolicy::load(root, &bot.policy)?;
    let cwd = match &card {
        Some(c) => root.join(c),
        None => root.join(format!("work/{project}")),
    };
    if !cwd.is_dir() {
        anyhow::bail!("cwd 없음: {}", cwd.display());
    }
    // 카드의 레포 워크트리를 함께 연다.
    let mut add_dirs = vec![];
    if let Some(c) = &card {
        let p = root.join(c).join("task.md");
        if let Ok(content) = std::fs::read_to_string(&p) {
            if let Ok(cs) = jc_core::taskmd::card_state(&content) {
                for r in &cs.front.repos {
                    let wt = root.join(c).join(&r.repo);
                    if wt.is_dir() {
                        add_dirs.push(wt);
                    }
                }
            }
        }
    }
    for d in &bot.add_dirs {
        let p = root.join(d);
        if p.is_dir() {
            add_dirs.push(p);
        }
    }
    Ok(InstanceSpec {
        id,
        role,
        project,
        card,
        bot_dir,
        bot,
        policy,
        session_id: jc_core::ulid::uuid4(),
        cwd,
        add_dirs,
        auto_allow: vec![],
        permission_timeout_min: 25,
    })
}

/// 상수: 사용하지 않는 경고를 막기 위한 표시.
pub static TURN_COUNTER: AtomicU64 = AtomicU64::new(0);
pub fn _touch() -> u64 {
    TURN_COUNTER.load(Ordering::Relaxed)
}
pub fn _ctx<T>(r: anyhow::Result<T>) -> anyhow::Result<T> {
    r.context("instances")
}

/// 권한 요청 하나를 결정한다. 정책 auto_allow에 걸리면 바로 허용, 아니면 사람 답을 기다린다.
#[allow(clippy::too_many_arguments)]
async fn decide_permission_task(
    root: PathBuf,
    store_cmd: mpsc::Sender<crate::server::Cmd>,
    pending: Arc<Mutex<HashMap<u64, oneshot::Sender<(bool, String)>>>>,
    infos: Vec<InstanceInfo>,
    instance: String,
    tool: String,
    input: Value,
    tool_use_id: String,
    timeout_min: u64,
) -> PermissionDecision {
    let Some(info) = infos.into_iter().find(|i| i.id == instance) else {
        return PermissionDecision {
            allow: false,
            message: "unknown instance".into(),
            seq: None,
        };
    };
    let topic = match &info.card {
        Some(c) => card_topic(c),
        None => format!("project/{}", info.project),
    };
    async fn emit(store_cmd: &mpsc::Sender<crate::server::Cmd>, ev: NewEvent) -> Vec<u64> {
        let (tx, rx) = oneshot::channel();
        if store_cmd
            .send(crate::server::Cmd::Append {
                events: vec![ev],
                reply: tx,
            })
            .await
            .is_err()
        {
            return vec![];
        }
        rx.await.ok().and_then(|r| r.ok()).unwrap_or_default()
    }
    async fn set_status(
        store_cmd: &mpsc::Sender<crate::server::Cmd>,
        card: String,
        status: &str,
        kind: &str,
        body: Value,
    ) {
        let (tx, _rx) = oneshot::channel();
        let _ = store_cmd
            .send(crate::server::Cmd::SetFrontRaw {
                card,
                status: Some(status.to_string()),
                stage: None,
                kind: kind.to_string(),
                body,
                reply: tx,
            })
            .await;
    }

    // 사람이 정한 auto_allow(봇 폴더 밖 policy/). 없으면 모두 사람에게 묻는다.
    let auto_list: Vec<String> = std::fs::read_to_string(
        root.join("policy/permissions")
            .join(format!("{}.toml", info.bot)),
    )
    .ok()
    .and_then(|s| s.parse::<toml::Table>().ok())
    .and_then(|t| {
        t.get("auto_allow")
            .and_then(|v| v.as_array())
            .map(|a| a.iter().filter_map(|x| x.as_str().map(String::from)).collect())
    })
    .unwrap_or_default();
    let blob = input.to_string();
    let auto = auto_list.iter().any(|r| {
        let (name, needle) = match r.split_once(':') {
            Some((a, b)) => (a, b.trim_start_matches('*').trim()),
            None => (r.as_str(), ""),
        };
        name == tool && (needle.is_empty() || blob.contains(needle))
    });

    let seqs = emit(
        &store_cmd,
        NewEvent::new(
            topic.clone(),
            "permission.requested",
            json!({"instance":instance,"tool":tool,"tool_use_id":tool_use_id,"input":input,
                   "mode": if auto {"auto_allow"} else {"ask_human"}}),
        ),
    )
    .await;
    let seq = *seqs.first().unwrap_or(&0);
    if auto {
        let _ = emit(
            &store_cmd,
            NewEvent::new(
                topic,
                "permission.resolved",
                json!({"instance":instance,"tool":tool,"decision":"allow","source":"policy","request_seq":seq}),
            ),
        )
        .await;
        return PermissionDecision {
            allow: true,
            message: "policy allow".into(),
            seq: Some(seq),
        };
    }
    // 사람에게 올린다: 카드를 "사람 대기"로 두고 이벤트를 남긴다(오케스트레이터도 깨어난다).
    if let Some(card) = &info.card {
        set_status(
            &store_cmd,
            card.clone(),
            "waiting_human",
            "card.blocked_on_permission",
            json!({"instance":instance,"tool":tool,"request_seq":seq}),
        )
        .await;
    }
    let (tx, rx) = oneshot::channel();
    pending.lock().unwrap().insert(seq, tx);
    let timeout = Duration::from_secs(timeout_min.max(1) * 60);
    let answered = tokio::time::timeout(timeout, rx).await;
    pending.lock().unwrap().remove(&seq);
    let (allow, message) = match answered {
        Ok(Ok((a, m))) => (a, m),
        _ => (false, "사람 응답 시간 초과".to_string()),
    };
    let _ = emit(
        &store_cmd,
        NewEvent::new(
            topic,
            "permission.resolved",
            json!({"instance":instance,"tool":tool,"decision": if allow {"allow"} else {"deny"},
                   "source":"human","request_seq":seq,"message":message}),
        ),
    )
    .await;
    if let Some(card) = &info.card {
        set_status(
            &store_cmd,
            card.clone(),
            if allow { "working" } else { "blocked" },
            "card.permission_resolved",
            json!({"instance":instance,"allow":allow,"request_seq":seq}),
        )
        .await;
    }
    PermissionDecision {
        allow,
        message,
        seq: Some(seq),
    }
}
