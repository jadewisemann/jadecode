//! ACP(Agent Client Protocol) 클라이언트. OpenCode·omp가 이 프로토콜을 쓴다.
//!
//! 흐름: `initialize` → `session/new` → `session/prompt`.
//! 클라이언트(우리)가 해야 하는 일:
//! - `session/request_permission`에 답한다(권한 게이트 → 여기서 사람/정책).
//! - `fs/read_text_file`·`fs/write_text_file`을 처리한다(ACP 에이전트는 파일을 직접 만지지 않는다).
//! - `session/update`를 `DriverEvent`로 옮긴다.
//!
//! 쓰기는 쓰기 태스크 하나가 소유하고, 요청은 채널로 보낸다.

use crate::stream::{AssistantBlock, DriverEvent, Usage};
use anyhow::{bail, Context, Result};
use serde_json::{json, Value};
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::sync::Arc;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::process::Child;
use tokio::sync::{mpsc, oneshot};

#[derive(Debug, Clone)]
pub struct AcpPermission {
    pub title: String,
    pub tool_kind: String,
    pub tool_call: Value,
    pub options: Vec<Value>,
}

pub type PermissionFn = Arc<dyn Fn(AcpPermission) -> oneshot::Receiver<(bool, String)> + Send + Sync>;
pub type FileEventFn = Arc<dyn Fn(&str, bool) + Send + Sync>;

/// ACP 클라이언트 쪽 콜백 묶음.
#[derive(Clone)]
pub struct AcpHost {
    pub root: PathBuf,
    pub add_dirs: Vec<PathBuf>,
    pub permission: PermissionFn,
    /// 파일 쓰기 결과(`path`, 성공 여부).
    pub on_file: FileEventFn,
    pub events: mpsc::Sender<DriverEvent>,
}

pub struct AcpDriver {
    child: Child,
    pub pid: u32,
    pub session_id: String,
    pub agent_info: Value,
    req_tx: mpsc::Sender<String>,
    rx: mpsc::Receiver<DriverEvent>,
    reader: Option<tokio::task::JoinHandle<()>>,
    writer: Option<tokio::task::JoinHandle<()>>,
    stderr: Option<tokio::task::JoinHandle<()>>,
    pub stderr_path: PathBuf,
    next_id: u64,
    /// 요청 id → 응답 대기자. 읽기 태스크가 응답을 받아 여기로 보낸다.
    pending: Arc<tokio::sync::Mutex<std::collections::HashMap<u64, oneshot::Sender<Value>>>>,
    /// 턴 끝(Result)을 이벤트 흐름에 넣는 통로.
    result_tx: mpsc::Sender<DriverEvent>,
}

impl AcpDriver {
    /// 띄우고 `initialize` + `session/new`까지 끝낸다.
    pub async fn spawn(
        bin: &str,
        args: &[String],
        cwd: &Path,
        env: &BTreeMap<String, String>,
        host: AcpHost,
        mcp_servers: Value,
        stderr_path: PathBuf,
    ) -> Result<Self> {
        let mut cmd = tokio::process::Command::new(bin);
        cmd.args(args)
            .current_dir(cwd)
            .env_clear()
            .envs(env)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .kill_on_drop(false);
        unsafe {
            cmd.pre_exec(|| {
                if libc::setpgid(0, 0) != 0 {
                    return Err(std::io::Error::last_os_error());
                }
                Ok(())
            });
        }
        let mut child = cmd
            .spawn()
            .with_context(|| format!("spawn {bin} {args:?}"))?;
        let pid = child.id().unwrap_or(0);
        let mut stdin = child.stdin.take().context("no stdin")?;
        let stdout = child.stdout.take().context("no stdout")?;
        let stderr = child.stderr.take();

        let (ev_tx, ev_rx) = mpsc::channel::<DriverEvent>(1024);
        let (req_tx, mut req_rx) = mpsc::channel::<String>(128);
        let (resp_tx, mut resp_rx) =
            mpsc::channel::<(u64, Value)>(64);

        // 쓰기 태스크
        let writer = tokio::spawn(async move {
            while let Some(line) = req_rx.recv().await {
                if stdin.write_all(line.as_bytes()).await.is_err() {
                    break;
                }
                if stdin.write_all(b"\n").await.is_err() {
                    break;
                }
                if stdin.flush().await.is_err() {
                    break;
                }
            }
        });

        // 읽기 태스크: 응답은 resp 채널로, 서버 요청은 여기서 답한다.
        let host_r = host.clone();
        let ev_tx2 = ev_tx.clone();

        let req_tx2 = req_tx.clone();
        let reader = tokio::spawn(async move {
            let mut lines = BufReader::new(stdout).lines();
            while let Ok(Some(line)) = lines.next_line().await {
                if line.trim().is_empty() {
                    continue;
                }
                let Ok(v) = serde_json::from_str::<Value>(&line) else {
                    continue;
                };
                let idv = v.get("id").and_then(|x| x.as_u64());
                let has_result = v.get("result").is_some() || v.get("error").is_some();
                if let (Some(id), true) = (idv, has_result) {
                    if v.get("method").is_none() {
                        let _ = resp_tx.send((id, v)).await;
                        continue;
                    }
                }
                let Some(method) = v.get("method").and_then(|m| m.as_str()) else {
                    continue;
                };
                let params = v.get("params").cloned().unwrap_or(Value::Null);
                let rid = v.get("id").cloned().unwrap_or(Value::Null);
                match method {
                    "session/update" => {
                        for ev in map_update(&params) {
                            let _ = ev_tx2.send(ev).await;
                        }
                    }
                    "fs/read_text_file" => {
                        let p = params["path"].as_str().unwrap_or("").to_string();
                        let content =
                            read_allowed(&host_r.root, &host_r.add_dirs, &p).unwrap_or_default();
                        let resp =
                            json!({"jsonrpc":"2.0","id":rid,"result":{"content":content}});
                        let _ = req_tx2.send(resp.to_string()).await;
                    }
                    "fs/write_text_file" => {
                        let p = params["path"].as_str().unwrap_or("").to_string();
                        let content = params["content"].as_str().unwrap_or("");
                        let ok = write_allowed(&host_r.root, &host_r.add_dirs, &p, content);
                        (host_r.on_file)(&p, ok);
                        let resp = json!({"jsonrpc":"2.0","id":rid,"result":null});
                        let _ = req_tx2.send(resp.to_string()).await;
                    }
                    "session/request_permission" => {
                        let opts = params["options"].as_array().cloned().unwrap_or_default();
                        let tc = params.get("toolCall").cloned().unwrap_or(Value::Null);
                        let req = AcpPermission {
                            title: tc
                                .get("title")
                                .and_then(|x| x.as_str())
                                .unwrap_or("permission")
                                .to_string(),
                            tool_kind: tc
                                .get("kind")
                                .and_then(|x| x.as_str())
                                .unwrap_or("other")
                                .to_string(),
                            tool_call: tc,
                            options: opts.clone(),
                        };
                        let dec = (host_r.permission)(req);
                        let rq = req_tx2.clone();
                        tokio::spawn(async move {
                            let (allow, _msg) = dec.await.unwrap_or((false, "no answer".into()));
                            let want: [&str; 2] = if allow {
                                ["allow_once", "allow_always"]
                            } else {
                                ["reject_once", "reject_always"]
                            };
                            let pick = opts
                                .iter()
                                .find(|o| {
                                    o.get("kind")
                                        .and_then(|k| k.as_str())
                                        .map(|k| want.contains(&k))
                                        .unwrap_or(false)
                                })
                                .or_else(|| if allow { opts.first() } else { None });
                            let result = match pick {
                                Some(o) => json!({"outcome":{"outcome":"selected",
                                    "optionId": o["optionId"]}}),
                                None => json!({"outcome":{"outcome":"cancelled"}}),
                            };
                            let resp = json!({"jsonrpc":"2.0","id":rid,"result":result});
                            let _ = rq.send(resp.to_string()).await;
                        });
                    }
                    _ => {
                        if !rid.is_null() {
                            let resp = json!({"jsonrpc":"2.0","id":rid,
                                "error":{"code":-32601,"message":format!("unsupported {method}")}});
                            let _ = req_tx2.send(resp.to_string()).await;
                        }
                    }
                }
            }
        });

        let sp = stderr_path.clone();
        let stderr_task = stderr.map(|s| {
            tokio::spawn(async move {
                let mut lines = BufReader::new(s).lines();
                let mut f = tokio::fs::File::create(&sp).await.ok();
                while let Ok(Some(l)) = lines.next_line().await {
                    if let Some(f) = f.as_mut() {
                        let _ = f.write_all(l.as_bytes()).await;
                        let _ = f.write_all(b"\n").await;
                    }
                }
            })
        });

        let pending: Arc<tokio::sync::Mutex<std::collections::HashMap<u64, oneshot::Sender<Value>>>> =
            Arc::new(tokio::sync::Mutex::new(Default::default()));
        let result_tx = ev_tx.clone();
        let mut d = Self {
            child,
            pid,
            session_id: String::new(),
            agent_info: Value::Null,
            req_tx,
            rx: ev_rx,
            reader: Some(reader),
            writer: Some(writer),
            stderr: stderr_task,
            stderr_path,
            next_id: 1,
            pending: pending.clone(),
            result_tx,
        };
        // 응답 대기: 별도 태스크가 resp_rx를 받아 요청 id별로 돌려준다.
        let pend = pending.clone();
        tokio::spawn(async move {
            while let Some((id, v)) = resp_rx.recv().await {
                if let Some(tx) = pend.lock().await.remove(&id) {
                    let _ = tx.send(v);
                }
            }
        });

        let init = d
            .rpc(
                "initialize",
                json!({"protocolVersion":1,
                       "clientCapabilities":{"fs":{"readTextFile":true,"writeTextFile":true}}}),
            )
            .await?;
        d.agent_info = init.get("agentInfo").cloned().unwrap_or(Value::Null);
        let name = d
            .agent_info
            .get("name")
            .and_then(|x| x.as_str())
            .unwrap_or("agent")
            .to_string();
        let version = d
            .agent_info
            .get("version")
            .and_then(|x| x.as_str())
            .unwrap_or("")
            .to_string();
        let _ = host
            .events
            .send(DriverEvent::Init {
                session_id: String::new(),
                model: format!("{name} {version}").trim().to_string(),
                mcp_servers: vec![],
                tools: vec![],
            })
            .await;

        let new = d
            .rpc(
                "session/new",
                json!({"cwd": cwd.display().to_string(), "mcpServers": mcp_servers}),
            )
            .await?;
        d.session_id = new
            .get("sessionId")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string();
        if d.session_id.is_empty() {
            bail!("acp: session/new gave no sessionId: {new}");
        }
        Ok(d)
    }

    async fn rpc(&mut self, method: &str, params: Value) -> Result<Value> {
        let v = self.call(method, params).await?;
        if let Some(e) = v.get("error") {
            bail!("acp {method}: {}", e["message"].as_str().unwrap_or("error"));
        }
        Ok(v.get("result").cloned().unwrap_or(Value::Null))
    }

    /// 응답 원문을 그대로 받는다(오류도 값으로).
    async fn call(&mut self, method: &str, params: Value) -> Result<Value> {
        let id = self.next_id;
        self.next_id += 1;
        let (tx, rx) = oneshot::channel();
        self.pending.lock().await.insert(id, tx);
        let line = json!({"jsonrpc":"2.0","id":id,"method":method,"params":params}).to_string();
        self.req_tx.send(line).await.context("acp writer gone")?;
        let v = tokio::time::timeout(std::time::Duration::from_secs(180), rx)
            .await
            .context("acp response timeout")?
            .context("acp connection closed")?;
        Ok(v)
    }

    /// 한 턴. 끝(`session/prompt` 응답)은 `DriverEvent::Result`로 이벤트 흐름에 들어온다.
    pub async fn prompt(&mut self, text: &str) -> Result<()> {
        let sid = self.session_id.clone();
        let id = self.next_id;
        self.next_id += 1;
        let (tx, rx) = oneshot::channel();
        self.pending.lock().await.insert(id, tx);
        let line = json!({"jsonrpc":"2.0","id":id,"method":"session/prompt",
            "params":{"sessionId":sid,"prompt":[{"type":"text","text":text}]}})
        .to_string();
        self.req_tx.send(line).await.context("acp writer gone")?;
        // 턴 끝 응답을 기다리는 태스크: Result 이벤트로 이벤트 흐름에 넣는다.
        let ev_tx = self.result_tx.clone();
        tokio::spawn(async move {
            let Ok(Ok(v)) = tokio::time::timeout(std::time::Duration::from_secs(3600), rx).await
            else {
                let _ = ev_tx
                    .send(DriverEvent::Result {
                        subtype: "timeout".into(),
                        is_error: true,
                        text: Some("턴 응답 시간 초과".into()),
                        usage: Default::default(),
                        num_turns: 1,
                        session_id: None,
                        permission_denials: vec![],
                    })
                    .await;
                return;
            };
            let ev = prompt_response_to_result(v.get("result").unwrap_or(&v));
            let _ = ev_tx.send(ev).await;
        });
        Ok(())
    }

    pub async fn next_event(&mut self) -> Option<DriverEvent> {
        self.rx.recv().await
    }

    pub async fn cancel(&mut self) -> Result<()> {
        let sid = self.session_id.clone();
        self.rpc("session/cancel", json!({"sessionId": sid}))
            .await
            .map(|_| ())
    }

    pub async fn terminate(mut self) -> Result<()> {
        unsafe {
            libc::kill(-(self.pid as i32), libc::SIGTERM);
        }
        match tokio::time::timeout(std::time::Duration::from_secs(5), self.child.wait()).await {
            Ok(_) => {}
            Err(_) => {
                unsafe {
                    libc::kill(-(self.pid as i32), libc::SIGKILL);
                }
                let _ = self.child.wait().await;
            }
        }
        for t in [self.reader.take(), self.writer.take(), self.stderr.take()]
            .into_iter()
            .flatten()
        {
            t.abort();
        }
        Ok(())
    }
}

/// `session/prompt` 응답 → `DriverEvent::Result`.
pub fn prompt_response_to_result(v: &Value) -> DriverEvent {
    let usage = v.get("usage").cloned().unwrap_or(Value::Null);
    DriverEvent::Result {
        subtype: v
            .get("stopReason")
            .and_then(|x| x.as_str())
            .unwrap_or("end_turn")
            .to_string(),
        is_error: v.get("stopReason").and_then(|x| x.as_str()) == Some("cancelled"),
        text: None,
        usage: Usage {
            input_tokens: usage.get("inputTokens").and_then(|x| x.as_u64()).unwrap_or(0),
            output_tokens: usage
                .get("outputTokens")
                .and_then(|x| x.as_u64())
                .unwrap_or(0),
            cache_read_input_tokens: usage
                .get("cachedReadTokens")
                .and_then(|x| x.as_u64())
                .unwrap_or(0),
            cache_creation_input_tokens: 0,
            cost_usd: 0.0,
            model: None,
        },
        num_turns: 1,
        session_id: None,
        permission_denials: vec![],
    }
}

/// `session/update` → 이벤트.
pub fn map_update(params: &Value) -> Vec<DriverEvent> {
    let u = &params["update"];
    let kind = u.get("sessionUpdate").and_then(|x| x.as_str()).unwrap_or("");
    let mut out = vec![];
    match kind {
        "agent_message_chunk" => {
            if let Some(t) = u["content"].get("text").and_then(|x| x.as_str()) {
                if !t.is_empty() {
                    out.push(DriverEvent::Assistant {
                        blocks: vec![AssistantBlock::Text {
                            text: t.to_string(),
                        }],
                        raw: u.clone(),
                    });
                }
            }
        }
        "tool_call" => {
            let title = u.get("title").and_then(|x| x.as_str()).unwrap_or("");
            let tool_kind = u.get("kind").and_then(|x| x.as_str()).unwrap_or("other");
            out.push(DriverEvent::Assistant {
                blocks: vec![AssistantBlock::ToolUse {
                    id: u
                        .get("toolCallId")
                        .and_then(|x| x.as_str())
                        .unwrap_or("")
                        .to_string(),
                    name: format!("{tool_kind}:{title}"),
                    input: u.get("rawInput").cloned().unwrap_or(Value::Null),
                }],
                raw: u.clone(),
            });
        }
        "tool_call_update" => {
            if let Some(st) = u.get("status").and_then(|x| x.as_str()) {
                if st == "failed" || st == "completed" {
                    out.push(DriverEvent::ToolResults {
                        blocks: vec![AssistantBlock::ToolResult {
                            tool_use_id: u
                                .get("toolCallId")
                                .and_then(|x| x.as_str())
                                .unwrap_or("")
                                .to_string(),
                            content: u.get("content").cloned().unwrap_or(Value::Null),
                            is_error: st == "failed",
                        }],
                    });
                }
            }
        }
        _ => {}
    }
    out
}

fn in_scope(root: &Path, add: &[PathBuf], p: &Path) -> bool {
    let cand = if p.exists() {
        std::fs::canonicalize(p).ok()
    } else {
        p.parent()
            .and_then(|d| std::fs::canonicalize(d).ok())
            .map(|d| d.join(p.file_name().unwrap_or_default()))
    };
    let Some(pc) = cand else { return false };
    if let Ok(r) = std::fs::canonicalize(root) {
        if pc.starts_with(&r) {
            return true;
        }
    }
    add.iter().any(|a| {
        std::fs::canonicalize(a)
            .map(|ac| pc.starts_with(&ac))
            .unwrap_or(false)
    })
}

fn read_allowed(root: &Path, add: &[PathBuf], p: &str) -> Option<String> {
    let path = Path::new(p);
    if !in_scope(root, add, path) {
        return None;
    }
    std::fs::read_to_string(path).ok()
}

fn write_allowed(root: &Path, add: &[PathBuf], p: &str, content: &str) -> bool {
    let path = Path::new(p);
    if !in_scope(root, add, path) {
        return false;
    }
    if let Some(d) = path.parent() {
        let _ = std::fs::create_dir_all(d);
    }
    std::fs::write(path, content).is_ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn scope_limits_writes() {
        let root = std::env::temp_dir().join(format!("jc-acp-{}", jc_core::ulid::new_id()));
        std::fs::create_dir_all(&root).unwrap();
        let inside = root.join("a.txt");
        assert!(write_allowed(&root, &[], inside.to_str().unwrap(), "x"));
        assert_eq!(std::fs::read_to_string(&inside).unwrap(), "x");
        let outside = std::path::Path::new("/tmp/jc-should-not-write.txt");
        let _ = std::fs::remove_file(outside);
        assert!(!write_allowed(&root, &[], outside.to_str().unwrap(), "x"));
        assert!(!outside.exists());
        assert!(read_allowed(&root, &[], "/etc/passwd").is_none());
    }

    #[test]
    fn maps_updates() {
        let evs = map_update(&serde_json::json!({"update":{"sessionUpdate":"agent_message_chunk",
            "content":{"type":"text","text":"hi"}}}));
        match &evs[0] {
            DriverEvent::Assistant { blocks, .. } => {
                assert!(matches!(&blocks[0], AssistantBlock::Text { text } if text == "hi"))
            }
            _ => panic!(),
        }
        let evs = map_update(&serde_json::json!({"update":{"sessionUpdate":"tool_call",
            "title":"write","kind":"edit","toolCallId":"t1"}}));
        match &evs[0] {
            DriverEvent::Assistant { blocks, .. } => assert!(blocks[0].is_tool_use("edit:write")),
            _ => panic!(),
        }
    }

    #[test]
    fn prompt_result_has_usage() {
        let ev = prompt_response_to_result(&serde_json::json!({"stopReason":"end_turn",
            "usage":{"inputTokens":10,"outputTokens":2,"cachedReadTokens":5}}));
        match ev {
            DriverEvent::Result { usage, is_error, .. } => {
                assert_eq!(usage.input_tokens, 10);
                assert_eq!(usage.cache_read_input_tokens, 5);
                assert!(!is_error);
            }
            _ => panic!(),
        }
    }
}
