//! 보드 데이터: 서버 스냅숏 + 이벤트 구독.
//!
//! 클라이언트는 정본을 갖지 않는다. 시작할 때 `cards.list`·`instances.list`·`projects.list`로
//! 스냅숏을 받고, `events.subscribe`의 `from_seq`로 그 뒤를 이어 받는다(S5 계약).

use anyhow::{Context, Result};
use futures_util::{SinkExt, StreamExt};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::path::{Path, PathBuf};
use tokio_tungstenite::tungstenite::Message;

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct Card {
    pub path: String,
    pub id: String,
    pub title: String,
    pub project: String,
    pub stage: String,
    pub status: String,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct Instance {
    pub id: String,
    pub role: String,
    pub agent: String,
    pub state: String,
    pub bot: String,
    pub card: Option<String>,
    pub project: String,
    pub turns: u64,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct Project {
    pub slug: String,
    pub name: String,
    pub cards: u64,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct EventRow {
    pub seq: u64,
    pub ts: u64,
    pub topic: String,
    pub kind: String,
    pub body: Value,
}

/// 화면이 그리는 한 장면.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct BoardSnapshot {
    pub projects: Vec<Project>,
    pub cards: Vec<Card>,
    pub instances: Vec<Instance>,
    pub head_seq: u64,
    /// 오케스트레이터 대화(최근 메시지).
    pub messages: Vec<EventRow>,
    /// 사람 답을 기다리는 권한 요청.
    pub pending_permissions: Vec<EventRow>,
    /// 최근 이벤트 전부(카드 스레드·오케스트레이터 대화가 이력 뒤에 이어 붙인다).
    #[serde(default)]
    pub recent: Vec<EventRow>,
}

/// `recent`에 남기는 최대 이벤트 수.
pub const RECENT_CAP: usize = 1000;

/// 스냅숏 사본을 값싸게 만든다.
///
/// 그냥 `clone()`은 카드·메시지 문자열을 전부 복제한다. 카드 1,000개·메시지 500개면
/// 프레임마다 1.2ms(16ms 예산의 7.8%)를 썼다. 화면은 한 프레임 동안만 읽으므로
/// `Arc`로 감싸 참조 계수만 올린다(측정: 1,242us → 1us 미만).
#[derive(Debug, Clone, Default)]
pub struct SnapshotHandle(std::sync::Arc<BoardSnapshot>);

impl SnapshotHandle {
    pub fn new(s: BoardSnapshot) -> Self {
        Self(std::sync::Arc::new(s))
    }
    pub fn get(&self) -> &BoardSnapshot {
        &self.0
    }
    /// 사본(값). 이벤트 반영처럼 고쳐야 할 때만 쓴다.
    pub fn clone_inner(&self) -> BoardSnapshot {
        (*self.0).clone()
    }
}

impl std::ops::Deref for SnapshotHandle {
    type Target = BoardSnapshot;
    fn deref(&self) -> &BoardSnapshot {
        &self.0
    }
}

impl BoardSnapshot {
    /// 단계 목록(보드 열). 프로젝트별로 다를 수 있으므로 등장 순서를 보존한다.
    pub fn stages(&self, project: Option<&str>) -> Vec<String> {
        let mut out: Vec<String> = vec![];
        for c in &self.cards {
            if let Some(p) = project {
                if c.project != p {
                    continue;
                }
            }
            if !out.contains(&c.stage) {
                out.push(c.stage.clone());
            }
        }
        out
    }

    /// 그 단계의 카드.
    pub fn stage_cards(&self, project: Option<&str>, stage: &str) -> Vec<&Card> {
        let mut v: Vec<&Card> = self
            .cards
            .iter()
            .filter(|c| c.stage == stage && project.map(|p| c.project == p).unwrap_or(true))
            .collect();
        v.sort_by(|a, b| a.id.cmp(&b.id));
        v
    }

    pub fn card_by_path(&self, path: &str) -> Option<&Card> {
        self.cards.iter().find(|c| c.path == path)
    }

    /// 카드에서 일하는 인스턴스.
    pub fn instances_of(&self, card: &str) -> Vec<&Instance> {
        self.instances
            .iter()
            .filter(|i| i.card.as_deref() == Some(card))
            .collect()
    }
}

/// 카드 상세 화면에 필요한 것.
#[derive(Debug, Clone, Default)]
pub struct CardDetail {
    pub path: String,
    pub markdown: String,
    /// 카드 경로(거칠 단계).
    pub stages: Vec<String>,
    pub repos: Vec<String>,
}

#[derive(Debug, Clone, Default)]
pub struct AgentInfo {
    pub name: String,
    pub installed: bool,
    pub version: String,
    pub detail: String,
}

#[derive(Debug, Clone, Default)]
pub struct BotInfo {
    pub name: String,
    pub agent: String,
    pub model: String,
}

/// 수집 항목(외부 사이클).
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
pub struct IntakeItem {
    #[serde(default)]
    pub id: String,
    #[serde(default)]
    pub project: String,
    #[serde(default)]
    pub source: String,
    #[serde(default)]
    pub title: String,
    #[serde(default)]
    pub body: String,
    #[serde(default)]
    pub url: String,
    #[serde(default)]
    pub from: String,
    #[serde(default)]
    pub connector: String,
    /// new | carded | archived | reply_pending | closed
    #[serde(default)]
    pub state: String,
    #[serde(default)]
    pub card: Option<String>,
    #[serde(default)]
    pub seq: u64,
    #[serde(default)]
    pub ts: u64,
}

/// 커넥터 한 줄(비밀은 서버가 가린다).
#[derive(Debug, Clone, Default)]
pub struct ConnectorInfo {
    pub name: String,
    pub project: String,
    pub preset: String,
    pub enabled: bool,
    pub can_reply: bool,
    /// 마지막 동기화: (성공, 설명)
    pub status: Option<(bool, String)>,
}

/// 서버 연결 설정.
#[derive(Debug, Clone)]
pub struct Connection {
    pub socket: PathBuf,
}

impl Connection {
    /// 워크스페이스에서 소켓 경로를 찾는다(`JADECODE_SOCKET` 우선).
    pub fn discover(workspace: Option<&Path>) -> Result<Self> {
        if let Ok(p) = std::env::var("JADECODE_SOCKET") {
            return Ok(Self {
                socket: PathBuf::from(p),
            });
        }
        let ws = match workspace {
            Some(w) => w.to_path_buf(),
            None => find_workspace(&std::env::current_dir()?)?,
        };
        // 서버가 남긴 실제 소켓 경로를 먼저 본다(--socket 으로 바꿨을 수 있다).
        if let Ok(t) = std::fs::read_to_string(ws.join(".jadecode/server.json")) {
            if let Ok(v) = serde_json::from_str::<Value>(&t) {
                if let Some(p) = v["socket"].as_str().filter(|s| !s.is_empty()) {
                    return Ok(Self {
                        socket: PathBuf::from(p),
                    });
                }
            }
        }
        Ok(Self {
            socket: ws.join(".jadecode/jadecode.sock"),
        })
    }

    /// 스냅숏 + 구독. 돌려주는 `Board`가 이벤트를 받아 스냅숏을 갱신한다.
    pub async fn open(&self) -> Result<Board> {
        let stream = tokio::net::UnixStream::connect(&self.socket)
            .await
            .with_context(|| format!("서버에 붙을 수 없다: {}", self.socket.display()))?;
        let (ws, _) = tokio_tungstenite::client_async("ws://localhost/jadecode", stream).await?;
        let mut board = Board {
            conn: self.clone(),
            snapshot: BoardSnapshot::default(),
            ws: Some(ws),
            next_id: 100,
        };
        board.refresh().await?;
        board.subscribe().await?;
        Ok(board)
    }
}

/// 위로 올라가며 `jadecode.toml`이 있는 폴더를 찾는다.
pub fn find_workspace(start: &Path) -> Result<PathBuf> {
    let mut d = start.to_path_buf();
    loop {
        if d.join("jadecode.toml").exists() {
            return Ok(d);
        }
        if !d.pop() {
            anyhow::bail!("jadecode 워크스페이스를 찾을 수 없다(위로 올라가도 jadecode.toml 없음)");
        }
    }
}

pub struct Board {
    pub conn: Connection,
    pub snapshot: BoardSnapshot,
    ws: Option<tokio_tungstenite::WebSocketStream<tokio::net::UnixStream>>,
    next_id: u64,
}

impl Board {
    /// 연결 없이 스냅숏만 다루는 보드(성능 측정·시험용).
    pub fn new_for_bench() -> Self {
        Self {
            conn: Connection {
                socket: std::path::PathBuf::new(),
            },
            snapshot: BoardSnapshot::default(),
            ws: None,
            next_id: 0,
        }
    }

    pub fn snapshot(&self) -> &BoardSnapshot {
        &self.snapshot
    }

    async fn next_id(&mut self) -> u64 {
        self.next_id += 1;
        self.next_id
    }

    /// RPC 한 번. 요청 id로 응답을 고른다(구독 알림은 무시).
    pub async fn call(&mut self, method: &str, params: Value) -> Result<Value> {
        let id = self.next_id().await;
        let ws = self.ws.as_mut().context("연결 없음")?;
        ws.send(Message::text(
            json!({"jsonrpc":"2.0","id":id,"method":method,"params":params}).to_string(),
        ))
        .await?;
        loop {
            let msg = tokio::time::timeout(std::time::Duration::from_secs(30), ws.next())
                .await
                .context("응답 시간 초과")?
                .context("연결 닫힘")??;
            let Message::Text(t) = msg else { continue };
            let v: Value = serde_json::from_str(&t)?;
            if v.get("id").and_then(|x| x.as_u64()) == Some(id) {
                if let Some(e) = v.get("error") {
                    anyhow::bail!(
                        "{}: {}",
                        e["code"],
                        e["message"].as_str().unwrap_or("error")
                    );
                }
                return Ok(v.get("result").cloned().unwrap_or(Value::Null));
            }
        }
    }

    /// 스냅숏을 새로 받는다.
    pub async fn refresh(&mut self) -> Result<()> {
        let projects = self.call("projects.list", json!({})).await?;
        let cards = self.call("cards.list", json!({})).await?;
        let instances = self.call("instances.list", json!({})).await?;
        let head = cards["head_seq"].as_u64().unwrap_or(0);

        let projects: Vec<Project> = projects["projects"]
            .as_array()
            .cloned()
            .unwrap_or_default()
            .into_iter()
            .filter_map(|v| {
                Some(Project {
                    slug: v["slug"].as_str()?.to_string(),
                    name: v["name"].as_str().unwrap_or("").to_string(),
                    cards: v["cards"].as_u64().unwrap_or(0),
                })
            })
            .collect();
        let cards: Vec<Card> = cards["cards"]
            .as_array()
            .cloned()
            .unwrap_or_default()
            .into_iter()
            .filter_map(|v| {
                Some(Card {
                    path: v["path"].as_str()?.to_string(),
                    id: v["id"].as_str().unwrap_or("").to_string(),
                    title: v["title"].as_str().unwrap_or("").to_string(),
                    project: v["project"].as_str().unwrap_or("").to_string(),
                    stage: v["stage"].as_str().unwrap_or("").to_string(),
                    status: v["status"].as_str().unwrap_or("idle").to_string(),
                })
            })
            .collect();
        let instances: Vec<Instance> = instances["instances"]
            .as_array()
            .cloned()
            .unwrap_or_default()
            .into_iter()
            .map(|v| Instance {
                id: v["id"].as_str().unwrap_or("").to_string(),
                role: v["role"].as_str().unwrap_or("").to_string(),
                agent: v["agent"].as_str().unwrap_or("").to_string(),
                state: v["state"].as_str().unwrap_or("").to_string(),
                bot: v["bot"].as_str().unwrap_or("").to_string(),
                card: v["card"].as_str().map(String::from),
                project: v["project"].as_str().unwrap_or("").to_string(),
                turns: v["turns"].as_u64().unwrap_or(0),
            })
            .collect();

        // 최근 대화·권한 대기는 이벤트 꼬리에서 만든다.
        let tail = self
            .call("events.tail", json!({"limit": 200}))
            .await
            .unwrap_or(json!({"events":[]}));
        let rows: Vec<EventRow> = tail["events"]
            .as_array()
            .cloned()
            .unwrap_or_default()
            .into_iter()
            .filter_map(|v| serde_json::from_value(v).ok())
            .collect();
        let mut resolved: std::collections::HashSet<u64> = Default::default();
        let mut pending: Vec<EventRow> = vec![];
        for r in &rows {
            if r.kind == "permission.resolved" {
                if let Some(s) = r.body["request_seq"].as_u64() {
                    resolved.insert(s);
                }
            }
        }
        for r in &rows {
            if r.kind == "permission.requested" && !resolved.contains(&r.seq) {
                pending.push(r.clone());
            }
        }
        let mut recent: Vec<EventRow> = rows.clone();
        recent.sort_by_key(|r| r.seq);
        let messages: Vec<EventRow> = rows
            .into_iter()
            .filter(|r| {
                r.kind.starts_with("msg.")
                    || r.kind.starts_with("instance.output")
                    || r.kind == "instance.wake_digest"
            })
            .filter(|r| !(r.kind == "msg.report" && r.body["auto"].as_bool() == Some(true)))
            .collect();

        self.snapshot = BoardSnapshot {
            projects,
            cards,
            instances,
            head_seq: head,
            messages,
            pending_permissions: pending,
            recent,
        };
        Ok(())
    }

    /// 이후 이벤트를 구독한다(현재 head부터).
    pub async fn subscribe(&mut self) -> Result<()> {
        let head = self.snapshot.head_seq;
        self.call("events.subscribe", json!({"from_seq": head, "topics": []}))
            .await?;
        Ok(())
    }

    /// 다음 이벤트를 하나 읽어 스냅숏에 반영한다. 이벤트가 없으면 `None`.
    pub async fn poll(&mut self) -> Option<Vec<EventRow>> {
        let mut rows: Vec<EventRow> = vec![];
        {
            let ws = self.ws.as_mut()?;
            let msg = match tokio::time::timeout(std::time::Duration::from_millis(120), ws.next())
                .await
            {
                Ok(Some(Ok(Message::Text(t)))) => t.to_string(),
                Ok(None) | Ok(Some(Err(_))) | Ok(Some(Ok(Message::Close(_)))) => {
                    // 서버가 닫았다. 다시 붙는 것은 부르는 쪽이 한다.
                    self.ws = None;
                    return None;
                }
                _ => return None,
            };
            let v: Value = serde_json::from_str(&msg).ok()?;
            if v.get("method").and_then(|m| m.as_str()) == Some("events.event") {
                if let Ok(ev) = serde_json::from_value::<EventRow>(v["params"]["event"].clone()) {
                    rows.push(ev);
                }
            }
            // 같은 배치에 더 있으면 함께 (블로킹 없이)
            while let Ok(Some(Ok(Message::Text(t)))) =
                tokio::time::timeout(std::time::Duration::from_millis(0), ws.next()).await
            {
                let t: String = t.to_string();
                if let Ok(v) = serde_json::from_str::<Value>(&t) {
                    if v.get("method").and_then(|m| m.as_str()) == Some("events.event") {
                        if let Ok(e) =
                            serde_json::from_value::<EventRow>(v["params"]["event"].clone())
                        {
                            rows.push(e);
                        }
                    }
                }
            }
        }
        if rows.is_empty() {
            return None;
        }
        for e in &rows {
            self.apply(e);
        }
        Some(rows)
    }

    /// 이벤트 하나를 스냅숏에 반영한다(순수 함수 — 테스트 대상).
    pub fn apply(&mut self, ev: &EventRow) {
        self.snapshot.head_seq = self.snapshot.head_seq.max(ev.seq);
        if ev.kind != "card.file_changed" {
            self.snapshot.recent.push(ev.clone());
            if self.snapshot.recent.len() > RECENT_CAP {
                let cut = self.snapshot.recent.len() - RECENT_CAP;
                self.snapshot.recent.drain(0..cut);
            }
        }
        match ev.kind.as_str() {
            "card.created" => {
                let id = ev.body["id"].as_str().unwrap_or("").to_string();
                if id.is_empty() {
                    return;
                }
                let path = ev.body["card"]
                    .as_str()
                    .map(String::from)
                    .unwrap_or_else(|| card_path_from_topic(&ev.topic));
                if self.snapshot.cards.iter().any(|c| c.path == path) {
                    return;
                }
                self.snapshot.cards.push(Card {
                    path,
                    id,
                    title: ev.body["title"].as_str().unwrap_or("").to_string(),
                    project: ev.body["project"].as_str().unwrap_or("").to_string(),
                    stage: ev.body["path"]
                        .as_array()
                        .and_then(|a| a.first())
                        .and_then(|x| x.as_str())
                        .unwrap_or("")
                        .to_string(),
                    status: "idle".into(),
                });
            }
            "card.stage_changed" => {
                if let Some(c) = self.card_mut(&ev.topic) {
                    c.stage = ev.body["to"].as_str().unwrap_or(&c.stage).to_string();
                    c.status = "idle".into();
                }
            }
            "card.status_changed" => {
                if let Some(c) = self.card_mut(&ev.topic) {
                    c.status = ev.body["to"].as_str().unwrap_or(&c.status).to_string();
                }
            }
            "card.file_changed" | "card.reconciled" => {
                // 본문 변경은 보드에 영향 없다(상세 화면이 다시 읽는다).
            }
            "card.state_change_rejected" => {
                if let Some(c) = self.card_mut(&ev.topic) {
                    if let Some(k) = ev.body["kept"]["status"].as_str() {
                        c.status = k.to_string();
                    }
                }
            }
            "instance.started" => {
                let id = ev.body["instance"].as_str().unwrap_or("").to_string();
                if id.is_empty() || self.snapshot.instances.iter().any(|i| i.id == id) {
                    return;
                }
                self.snapshot.instances.push(Instance {
                    id,
                    role: ev.body["role"].as_str().unwrap_or("worker").into(),
                    agent: ev.body["agent"].as_str().unwrap_or("claude").into(),
                    state: "starting".into(),
                    bot: ev.body["bot"].as_str().unwrap_or("").into(),
                    card: ev.body["card"].as_str().map(String::from),
                    project: ev.body["project"].as_str().unwrap_or("").into(),
                    turns: 0,
                });
            }
            "instance.exited" => {
                if let Some(id) = ev.body["instance"].as_str() {
                    self.snapshot.instances.retain(|i| i.id != id);
                }
            }
            "instance.init" => {
                if let Some(id) = ev.body["instance"].as_str() {
                    if let Some(i) = self.snapshot.instances.iter_mut().find(|i| i.id == id) {
                        i.state = "idle".into();
                    }
                }
            }
            "instance.turn_end" => {
                if let Some(id) = ev.body["instance"].as_str() {
                    if let Some(i) = self.snapshot.instances.iter_mut().find(|i| i.id == id) {
                        i.state = "idle".into();
                        i.turns = ev.body["turn"].as_u64().unwrap_or(i.turns);
                    }
                }
            }
            "permission.requested" => {
                self.snapshot.pending_permissions.push(ev.clone());
            }
            "permission.resolved" => {
                if let Some(s) = ev.body["request_seq"].as_u64() {
                    self.snapshot.pending_permissions.retain(|p| p.seq != s);
                }
            }
            "project.created" => {
                let slug = ev.body["slug"].as_str().unwrap_or("").to_string();
                if !slug.is_empty() && !self.snapshot.projects.iter().any(|p| p.slug == slug) {
                    self.snapshot.projects.push(Project {
                        name: ev.body["name"].as_str().unwrap_or(&slug).to_string(),
                        slug,
                        cards: 0,
                    });
                }
            }
            k if k.starts_with("msg.") || k == "instance.output" => {
                if k == "msg.report" && ev.body["auto"].as_bool() == Some(true) {
                    return;
                }
                self.snapshot.messages.push(ev.clone());
                if self.snapshot.messages.len() > 500 {
                    let cut = self.snapshot.messages.len() - 500;
                    self.snapshot.messages.drain(0..cut);
                }
            }
            _ => {}
        }
    }

    fn card_mut(&mut self, topic: &str) -> Option<&mut Card> {
        let path = card_path_from_topic(topic);
        let idx =
            self.snapshot.cards.iter().position(|c| {
                c.path == path || c.path.ends_with(&path) || topic.ends_with(&c.path)
            })?;
        self.snapshot.cards.get_mut(idx)
    }

    // ---------- 사람이 쓰는 동작 ----------

    pub async fn set_status(&mut self, card: &str, status: &str) -> Result<()> {
        self.call("card.set_status", json!({"card": card, "status": status}))
            .await?;
        Ok(())
    }

    pub async fn advance(&mut self, card: &str) -> Result<String> {
        let v = self.call("card.advance", json!({"card": card})).await?;
        Ok(v["stage"].as_str().unwrap_or("").to_string())
    }

    /// 카드를 경로 안의 다른 단계로 옮긴다(칸반 끌어 놓기).
    pub async fn move_to_stage(&mut self, card: &str, stage: &str) -> Result<String> {
        let v = self
            .call("card.advance", json!({"card": card, "to": stage}))
            .await?;
        Ok(v["stage"].as_str().unwrap_or("").to_string())
    }

    /// 프로젝트의 오케스트레이터에게 보낸다. 떠 있지 않으면 먼저 띄운다.
    pub async fn send_to_orchestrator(&mut self, project: &str, text: &str) -> Result<()> {
        let inst = self.orchestrator_of(project).await?;
        self.call("instance.send", json!({"instance": inst, "text": text}))
            .await?;
        Ok(())
    }

    /// 오케스트레이터 인스턴스 id. 없으면 띄운다.
    pub async fn orchestrator_of(&mut self, project: &str) -> Result<String> {
        let list = self.call("instances.list", json!({})).await?;
        let found = list["instances"].as_array().and_then(|a| {
            a.iter()
                .find(|i| i["role"] == "orchestrator" && i["project"] == project)
                .and_then(|i| i["id"].as_str().map(String::from))
        });
        if let Some(id) = found {
            return Ok(id);
        }
        let v = self
            .call(
                "instance.start",
                json!({"role":"orchestrator","project":project,"bot":"orchestrator"}),
            )
            .await
            .context("오케스트레이터를 띄우지 못했다")?;
        v["instance"]["id"]
            .as_str()
            .map(String::from)
            .context("instance.start 응답에 id가 없다")
    }

    /// 인스턴스에 직접 말한다.
    pub async fn send_to_instance(&mut self, instance: &str, text: &str) -> Result<()> {
        self.call("instance.send", json!({"instance": instance, "text": text}))
            .await?;
        Ok(())
    }

    pub async fn stop_instance(&mut self, instance: &str) -> Result<()> {
        self.call("instance.stop", json!({"instance": instance}))
            .await?;
        Ok(())
    }

    /// 새 프로젝트. 돌려주는 값은 slug.
    pub async fn add_project(&mut self, name: &str, repo: Option<&str>) -> Result<String> {
        let mut p = json!({"name": name});
        if let Some(r) = repo.filter(|r| !r.trim().is_empty()) {
            p["repo"] = json!(r.trim());
        }
        let v = self.call("project.add", p).await?;
        let slug = v["slug"].as_str().unwrap_or("").to_string();
        // projects.list는 이벤트로 오지 않으므로 바로 넣는다.
        if !slug.is_empty() && !self.snapshot.projects.iter().any(|p| p.slug == slug) {
            self.snapshot.projects.push(Project {
                slug: slug.clone(),
                name: name.to_string(),
                cards: 0,
            });
        }
        Ok(slug)
    }

    /// 새 카드. 돌려주는 값은 카드 경로(`work/<p>/<c>`).
    pub async fn add_card(
        &mut self,
        project: &str,
        title: &str,
        body: Option<&str>,
    ) -> Result<String> {
        let mut p = json!({"project": project, "title": title});
        if let Some(b) = body.filter(|b| !b.trim().is_empty()) {
            p["body"] = json!(b);
        }
        let v = self.call("card.add", p).await?;
        v["path"]
            .as_str()
            .map(String::from)
            .context("card.add 응답에 path가 없다")
    }

    /// 카드 상세(task.md 원문 + 앞머리).
    pub async fn card_detail(&mut self, card: &str) -> Result<CardDetail> {
        let v = self.call("card.get", json!({"card": card})).await?;
        let stages = v["front"]["path"]
            .as_array()
            .map(|a| {
                a.iter()
                    .filter_map(|x| x.as_str().map(String::from))
                    .collect()
            })
            .unwrap_or_default();
        let repos = v["front"]["repos"]
            .as_array()
            .map(|a| {
                a.iter()
                    .filter_map(|r| r["name"].as_str().map(String::from))
                    .collect()
            })
            .unwrap_or_default();
        Ok(CardDetail {
            path: v["path"].as_str().unwrap_or(card).to_string(),
            markdown: v["markdown"].as_str().unwrap_or("").to_string(),
            stages,
            repos,
        })
    }

    /// 주제의 지난 이벤트(오래된 것부터).
    pub async fn history(&mut self, topics: &[String], limit: u64) -> Result<Vec<EventRow>> {
        let v = self
            .call("events.tail", json!({"topics": topics, "limit": limit}))
            .await?;
        let mut rows: Vec<EventRow> = v["events"]
            .as_array()
            .cloned()
            .unwrap_or_default()
            .into_iter()
            .filter_map(|v| serde_json::from_value(v).ok())
            .collect();
        rows.sort_by_key(|r| r.seq);
        Ok(rows)
    }

    /// 에이전트 백엔드 설치 상태.
    pub async fn agents(&mut self) -> Result<Vec<AgentInfo>> {
        let v = self.call("agents.list", json!({})).await?;
        Ok(v["agents"]
            .as_array()
            .cloned()
            .unwrap_or_default()
            .into_iter()
            .map(|a| AgentInfo {
                name: a["agent"]
                    .as_str()
                    .or_else(|| a["name"].as_str())
                    .unwrap_or("")
                    .to_string(),
                installed: a["available"].as_bool().unwrap_or(false),
                version: a["version"].as_str().unwrap_or("").to_string(),
                detail: a["binary"]
                    .as_str()
                    .or_else(|| a["error"].as_str())
                    .unwrap_or("")
                    .to_string(),
            })
            .collect())
    }

    /// 봇 정의 목록(이름·에이전트).
    pub async fn bots(&mut self) -> Result<Vec<BotInfo>> {
        let v = self.call("bots.list", json!({})).await?;
        Ok(v["bots"]
            .as_array()
            .cloned()
            .unwrap_or_default()
            .into_iter()
            .map(|b| BotInfo {
                name: b["name"].as_str().unwrap_or("").to_string(),
                agent: b["agent"].as_str().unwrap_or("").to_string(),
                model: b["model"].as_str().unwrap_or("").to_string(),
            })
            .collect())
    }

    // ---------- 외부 사이클 ----------

    pub async fn intake_list(&mut self, project: Option<&str>) -> Result<Vec<IntakeItem>> {
        let mut p = json!({});
        if let Some(pr) = project {
            p["project"] = json!(pr);
        }
        let v = self.call("intake.list", p).await?;
        Ok(v["items"]
            .as_array()
            .cloned()
            .unwrap_or_default()
            .into_iter()
            .filter_map(|x| serde_json::from_value(x).ok())
            .collect())
    }

    pub async fn intake_add(&mut self, project: &str, title: &str, body: &str) -> Result<String> {
        let v = self
            .call(
                "intake.add",
                json!({"project": project, "title": title, "body": body, "source": "memo", "from": "나"}),
            )
            .await?;
        Ok(v["id"].as_str().unwrap_or("").to_string())
    }

    /// 카드로 만들거나(`brief`가 카드 본문) 보관한다. 카드면 경로를 돌려준다.
    pub async fn intake_triage(
        &mut self,
        id: &str,
        decision: &str,
        title: Option<&str>,
        brief: Option<&str>,
    ) -> Result<Option<String>> {
        let mut p = json!({"id": id, "decision": decision});
        if let Some(t) = title {
            p["title"] = json!(t);
        }
        if let Some(b) = brief {
            p["brief"] = json!(b);
        }
        let v = self.call("intake.triage", p).await?;
        Ok(v["card"].as_str().map(String::from))
    }

    pub async fn intake_close(&mut self, id: &str) -> Result<()> {
        self.call("intake.close", json!({"id": id})).await?;
        Ok(())
    }

    pub async fn intake_reply(&mut self, id: &str, text: &str) -> Result<()> {
        self.call("intake.reply", json!({"id": id, "text": text}))
            .await?;
        Ok(())
    }

    pub async fn connectors(&mut self) -> Result<Vec<ConnectorInfo>> {
        let v = self.call("connectors.list", json!({})).await?;
        Ok(v["connectors"]
            .as_array()
            .cloned()
            .unwrap_or_default()
            .into_iter()
            .map(|c| ConnectorInfo {
                name: c["name"].as_str().unwrap_or("").to_string(),
                project: c["project"].as_str().unwrap_or("").to_string(),
                preset: c["preset"].as_str().unwrap_or("").to_string(),
                enabled: c["enabled"].as_bool().unwrap_or(true),
                can_reply: !c["reply"].is_null(),
                status: if c["status"].is_null() {
                    None
                } else if c["status"]["ok"].as_bool() == Some(true) {
                    Some((
                        true,
                        format!("새 항목 {}개", c["status"]["added"].as_u64().unwrap_or(0)),
                    ))
                } else {
                    Some((
                        false,
                        c["status"]["error"].as_str().unwrap_or("실패").to_string(),
                    ))
                },
            })
            .collect())
    }

    pub async fn connector_save(
        &mut self,
        name: &str,
        project: &str,
        preset: &str,
        fields: Value,
    ) -> Result<()> {
        self.call(
            "connectors.save",
            json!({"name": name, "project": project, "preset": preset, "fields": fields}),
        )
        .await?;
        Ok(())
    }

    pub async fn connector_remove(&mut self, name: &str) -> Result<()> {
        self.call("connectors.remove", json!({"name": name}))
            .await?;
        Ok(())
    }

    /// 동기화. 돌려주는 값은 (새 항목 수, 오류들).
    pub async fn connector_sync(&mut self, name: Option<&str>) -> Result<(u64, Vec<String>)> {
        let mut p = json!({});
        if let Some(n) = name {
            p["name"] = json!(n);
        }
        let v = self.call("connectors.sync", p).await?;
        let mut added = 0;
        let mut errs = vec![];
        for r in v["results"].as_array().cloned().unwrap_or_default() {
            added += r["added"].as_u64().unwrap_or(0);
            if let Some(e) = r["error"].as_str() {
                errs.push(format!("{}: {e}", r["name"].as_str().unwrap_or("?")));
            }
        }
        Ok((added, errs))
    }

    pub async fn connector_test(&mut self, name: &str) -> Result<Vec<String>> {
        let v = self.call("connectors.test", json!({"name": name})).await?;
        Ok(v["tools"]
            .as_array()
            .map(|a| {
                a.iter()
                    .filter_map(|x| x.as_str().map(String::from))
                    .collect()
            })
            .unwrap_or_default())
    }

    /// 서버가 쓰는 워크스페이스 절대 경로.
    pub async fn workspace(&mut self) -> Result<PathBuf> {
        let v = self.call("server.info", json!({})).await?;
        v["workspace"]
            .as_str()
            .map(PathBuf::from)
            .context("server.info에 workspace가 없다")
    }

    pub async fn resolve_permission(&mut self, seq: u64, allow: bool, message: &str) -> Result<()> {
        self.call(
            "permission.resolve",
            json!({"request_seq": seq, "allow": allow, "message": message}),
        )
        .await?;
        Ok(())
    }

    /// 워커를 띄우고 인스턴스 id를 돌려준다.
    pub async fn start_worker(
        &mut self,
        project: &str,
        card: &str,
        agent: Option<&str>,
    ) -> Result<String> {
        let mut p = json!({"role":"worker","project":project,"card":card,"bot":"worker"});
        if let Some(a) = agent {
            p["agent"] = json!(a);
        }
        let v = self.call("instance.start", p).await?;
        Ok(v["instance"]["id"].as_str().unwrap_or("").to_string())
    }

    /// 연결이 살아 있는가(끊기면 구독 스레드가 다시 붙는다).
    pub fn is_connected(&self) -> bool {
        self.ws.is_some()
    }

    pub async fn card_markdown(&mut self, card: &str) -> Result<String> {
        let v = self.call("card.get", json!({"card": card})).await?;
        Ok(v["markdown"].as_str().unwrap_or("").to_string())
    }
}

/// `project/p/card/c` → `work/p/c`
pub fn card_path_from_topic(topic: &str) -> String {
    let seg: Vec<&str> = topic.split('/').collect();
    match seg.as_slice() {
        ["project", p, "card", c, ..] => format!("work/{p}/{c}"),
        _ => topic.to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ev(seq: u64, topic: &str, kind: &str, body: Value) -> EventRow {
        EventRow {
            seq,
            ts: 0,
            topic: topic.to_string(),
            kind: kind.to_string(),
            body,
        }
    }

    fn board() -> Board {
        Board {
            conn: Connection {
                socket: PathBuf::from("/nonexistent"),
            },
            snapshot: BoardSnapshot::default(),
            ws: None,
            next_id: 0,
        }
    }

    #[test]
    fn applies_card_lifecycle() {
        let mut b = board();
        b.apply(&ev(
            1,
            "project/api/card/c1",
            "card.created",
            json!({"id":"01A","title":"로그인","project":"api","path":["refine","implement"]}),
        ));
        assert_eq!(b.snapshot.cards.len(), 1);
        let c = &b.snapshot.cards[0];
        assert_eq!(c.stage, "refine");
        assert_eq!(c.status, "idle");
        assert_eq!(c.path, "work/api/c1");
        // 중복 생성은 무시
        b.apply(&ev(
            2,
            "project/api/card/c1",
            "card.created",
            json!({"id":"01A"}),
        ));
        assert_eq!(b.snapshot.cards.len(), 1);
        // 단계/상태 변화
        b.apply(&ev(
            3,
            "project/api/card/c1",
            "card.stage_changed",
            json!({"from":"refine","to":"implement"}),
        ));
        assert_eq!(b.snapshot.cards[0].stage, "implement");
        b.apply(&ev(
            4,
            "project/api/card/c1",
            "card.status_changed",
            json!({"from":"idle","to":"working"}),
        ));
        assert_eq!(b.snapshot.cards[0].status, "working");
    }

    #[test]
    fn stages_and_columns() {
        let mut s = BoardSnapshot::default();
        for (id, stage, project) in [
            ("a", "refine", "api"),
            ("b", "implement", "api"),
            ("c", "refine", "web"),
        ] {
            s.cards.push(Card {
                path: format!("work/{project}/{id}"),
                id: id.into(),
                title: id.into(),
                project: project.into(),
                stage: stage.into(),
                status: "idle".into(),
            });
        }
        assert_eq!(s.stages(Some("api")), vec!["refine", "implement"]);
        assert_eq!(s.stages(None), vec!["refine", "implement"]);
        assert_eq!(s.stage_cards(Some("api"), "refine").len(), 1);
        assert_eq!(s.stage_cards(None, "refine").len(), 2);
        assert!(s.card_by_path("work/api/a").is_some());
    }

    #[test]
    fn instances_track_state() {
        let mut b = board();
        b.apply(&ev(1, "instance/i1", "instance.started",
            json!({"instance":"i1","role":"worker","agent":"omp","bot":"worker","card":"work/api/c1","project":"api"})));
        assert_eq!(b.snapshot.instances[0].agent, "omp");
        assert_eq!(b.snapshot.instances[0].state, "starting");
        b.apply(&ev(
            2,
            "instance/i1",
            "instance.init",
            json!({"instance":"i1"}),
        ));
        assert_eq!(b.snapshot.instances[0].state, "idle");
        b.apply(&ev(
            3,
            "instance/i1",
            "instance.turn_end",
            json!({"instance":"i1","turn":3}),
        ));
        assert_eq!(b.snapshot.instances[0].turns, 3);
        assert_eq!(b.snapshot.instances_of("work/api/c1").len(), 1);
        b.apply(&ev(
            4,
            "instance/i1",
            "instance.exited",
            json!({"instance":"i1"}),
        ));
        assert!(b.snapshot.instances.is_empty());
    }

    #[test]
    fn permission_gate_roundtrip() {
        let mut b = board();
        b.apply(&ev(10, "project/api/card/c1", "permission.requested",
            json!({"instance":"i1","tool":"Bash","input":{"command":"chmod +x x"},"request_seq":10})));
        assert_eq!(b.snapshot.pending_permissions.len(), 1);
        b.apply(&ev(
            11,
            "project/api/card/c1",
            "permission.resolved",
            json!({"request_seq":10,"decision":"allow"}),
        ));
        assert!(b.snapshot.pending_permissions.is_empty());
    }

    #[test]
    fn messages_ignore_auto_reports() {
        let mut b = board();
        b.apply(&ev(
            1,
            "t",
            "msg.report",
            json!({"summary":"x","auto":true}),
        ));
        assert!(b.snapshot.messages.is_empty());
        b.apply(&ev(2, "t", "msg.report", json!({"summary":"real"})));
        b.apply(&ev(
            3,
            "t",
            "msg.report_done",
            json!({"status":"done","summary":"ok"}),
        ));
        assert_eq!(b.snapshot.messages.len(), 2);
    }

    #[test]
    fn caps_messages() {
        let mut b = board();
        for i in 0..600 {
            b.apply(&ev(
                i + 1,
                "t",
                "msg.report",
                json!({"summary": format!("{i}")}),
            ));
        }
        assert_eq!(b.snapshot.messages.len(), 500);
        assert_eq!(b.snapshot.messages.last().unwrap().seq, 600);
    }

    #[test]
    fn topic_to_path() {
        assert_eq!(card_path_from_topic("project/api/card/c1"), "work/api/c1");
        assert_eq!(card_path_from_topic("global"), "global");
    }
}

#[cfg(test)]
mod handle_tests {
    use super::*;

    #[test]
    fn handle_is_cheap_and_shares() {
        let mut s = BoardSnapshot::default();
        for i in 0..1000 {
            s.cards.push(Card {
                path: format!("work/p/c{i}"),
                id: format!("{i:026}"),
                title: format!("카드 {i}"),
                project: "p".into(),
                stage: "구현".into(),
                status: "idle".into(),
            });
        }
        let h = SnapshotHandle::new(s);
        let h2 = h.clone();
        // 참조만 늘어난다(같은 주소).
        assert!(std::ptr::eq(h.get(), h2.get()));
        assert_eq!(h2.get().cards.len(), 1000);
        // 고쳐야 할 때만 값 사본.
        let mut copy = h.clone_inner();
        copy.cards.clear();
        assert_eq!(h.get().cards.len(), 1000);
    }
}
