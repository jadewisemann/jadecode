//! pi RPC 드라이버 (`pi --mode rpc`).
//!
//! pi는 MCP를 지원하지 않는다(문서: "does not include built-in MCP"). 그래서 jadecode 도구는
//! **확장(extension)** 파일로 넣는다. 확장은 `pi.registerTool()`로 LLM 도구를 등록하고
//! `pi.on("tool_call")`로 위험한 도구를 막을 수 있다.
//!
//! 프로토콜(실측 2026-11-11, pi 0.73.1): stdin에 JSONL 명령, stdout에 JSONL 이벤트.
//! - `{"type":"prompt","message":"..."}` → `{"type":"response","command":"prompt","success":true}`
//! - `{"type":"abort"}`, `{"type":"get_state"}`
//! - 이벤트: `agent_start`, `turn_start`, `message_start/update/end`, `tool_execution_start/end`,
//!   `turn_end`, `agent_end`

use crate::stream::{AssistantBlock, DriverEvent, Usage};
use anyhow::{Context, Result};
use serde_json::{json, Value};
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::sync::atomic::{AtomicI64, Ordering};
use std::sync::Arc;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::process::Child;
use tokio::sync::mpsc;

pub struct PiDriver {
    child: Child,
    pub pid: u32,
    stdin: Option<tokio::process::ChildStdin>,
    rx: mpsc::Receiver<DriverEvent>,
    reader: Option<tokio::task::JoinHandle<()>>,
    stderr: Option<tokio::task::JoinHandle<()>>,
    pub stderr_path: PathBuf,
    /// 턴별로 모은 텍스트(마지막 응답).
    last_text: Arc<std::sync::Mutex<Vec<String>>>,
}

/// pi 확장 파일을 만든다. 도구 호출을 jadecode 서버로 넘긴다.
///
/// pi에는 MCP가 없으므로, `report`/`ask`/`propose`/`error`를 **셸 도구 호출**로 바꾼다.
/// 확장은 TypeScript지만 런타임에 ts를 그대로 받으므로 파일 하나로 충분하다.
pub fn write_extension(
    path: &Path,
    socket: &Path,
    instance: &str,
    card: Option<&str>,
    project: &str,
    jadecode_bin: &str,
    workspace: &Path,
) -> Result<()> {
    let card_js = match card {
        Some(c) => format!("\"{c}\""),
        None => "null".to_string(),
    };
    let src = format!(
        r#"// jadecode 확장 (자동 생성). pi에는 MCP가 없어 도구를 여기에 등록한다.
import type {{ ExtensionAPI }} from "@mariozechner/pi-coding-agent";
import {{ Type }} from "typebox";
import {{ spawnSync }} from "node:child_process";

const SOCKET = {socket:?};
const JADECODE_BIN = {bin:?};
const WORKSPACE = {ws:?};
const INSTANCE = {instance:?};
const CARD = {card_js};
const PROJECT = {project:?};

// jadecode CLI로 서버에 넣는다(WebSocket 핸드셰이크를 직접 하지 않는다 — 검증된 경로).
// 실행 파일은 절대 경로로 고정하고, 작업 폴더를 JADECODE_WORKSPACE로 넘긴다.
function rpc(method: string, params: unknown): string {{
  const r = spawnSync(JADECODE_BIN, ["rpc", method, JSON.stringify(params), "--socket", SOCKET], {{
    encoding: "utf8",
    env: {{ ...process.env, JADECODE_WORKSPACE: WORKSPACE }},
  }});
  const out = (r.stdout || "").trim();
  if (!out) {{
    const err = (r.stderr || "").trim();
    return "error: " + (err || "no output from " + JADECODE_BIN);
  }}
  return out;
}}

const actor = {{ role: "worker", instance: INSTANCE, card: CARD }};

export default function (pi: ExtensionAPI) {{
  pi.registerTool({{
    name: "jadecode_report",
    label: "jadecode 보고",
    description: "카드 작업 상황을 오케스트레이터에게 보고한다. 단계를 끝냈으면 status=done, 계속이면 progress, 막혔으면 blocked.",
    promptGuidelines: ["Use jadecode_report when you finish the card stage (status=done), are blocked (blocked), or have progress worth reporting."],
    parameters: Type.Object({{
      status: Type.Union([Type.Literal("done"), Type.Literal("progress"), Type.Literal("blocked")]),
      summary: Type.String({{ description: "한두 문장 요약" }}),
    }}),
    async execute(_id, params) {{
      if (CARD) {{
        const st = params.status === "done" ? "waiting_human" : params.status === "blocked" ? "blocked" : "working";
        rpc("card.set_status", {{ card: CARD, status: st }});
      }}
      const kind = params.status === "done" ? "msg.report_done" : "msg.report";
      const out = rpc("events.append", {{
        topic: CARD ? "project/" + PROJECT + "/card/" + CARD.split("/").slice(-1)[0] : "project/" + PROJECT,
        kind,
        body: {{ status: params.status, summary: params.summary, actor }},
      }});
      return {{ content: [{{ type: "text", text: "reported: " + out }}], details: {{}} }};
    }},
  }});

  pi.registerTool({{
    name: "jadecode_ask",
    label: "jadecode 질문",
    description: "사람이나 오케스트레이터에게 질문한다. 답은 나중에 새 메시지로 온다.",
    promptGuidelines: ["Use jadecode_ask when you need a decision you cannot make yourself."],
    parameters: Type.Object({{ question: Type.String() }}),
    async execute(_id, params) {{
      const out = rpc("events.append", {{
        topic: CARD ? "project/" + PROJECT + "/card/" + CARD.split("/").slice(-1)[0] : "project/" + PROJECT,
        kind: "msg.ask",
        body: {{ question: params.question, actor }},
      }});
      return {{ content: [{{ type: "text", text: "asked: " + out }}], details: {{}} }};
    }},
  }});

  pi.registerTool({{
    name: "jadecode_propose",
    label: "jadecode 제안",
    description: "사람 승인이 필요한 일(보호 브랜치 병합·배포·외부 메시지·예산 초과·프로젝트 삭제)을 제안한다. 직접 하지 않는다.",
    promptGuidelines: ["Use jadecode_propose instead of merging to a protected branch, deploying, or sending anything outside."],
    parameters: Type.Object({{ summary: Type.String(), detail: Type.Optional(Type.String()) }}),
    async execute(_id, params) {{
      const out = rpc("events.append", {{
        topic: CARD ? "project/" + PROJECT + "/card/" + CARD.split("/").slice(-1)[0] : "project/" + PROJECT,
        kind: "msg.propose",
        body: {{ summary: params.summary, detail: params.detail || "", kind: "human_procedure", actor }},
      }});
      return {{ content: [{{ type: "text", text: "proposed: " + out }}], details: {{}} }};
    }},
  }});

  pi.registerTool({{
    name: "jadecode_read_card",
    label: "jadecode 카드 읽기",
    description: "이 카드의 task.md 전문을 읽는다.",
    parameters: Type.Object({{}}),
    async execute() {{
      const out = rpc("card.get", {{ card: CARD }});
      return {{ content: [{{ type: "text", text: out }}], details: {{}} }};
    }},
  }});

  // 위험한 명령 차단(권한 정책의 최소선). 나머지는 pi 자체 승인 모드에 맡긴다.
  pi.on("tool_call", async (event: any) => {{
    const name = event.toolName || "";
    const input = JSON.stringify(event.input || {{}});
    if (name === "bash") {{
      for (const pat of ["git push", "rm -rf /", "sudo "]) {{
        if (input.includes(pat)) return {{ block: true, reason: "jadecode 정책: " + pat + " 금지" }};
      }}
    }}
    if (name === "write" || name === "edit") {{
      const p = event.input?.path || event.input?.file_path || "";
      if (p && CARD && !p.startsWith(CARD) && !p.startsWith("/tmp/")) {{
        // 카드 밖 쓰기는 pi 승인 모드가 막는다. 여기서는 막지 않고 통과시킨다.
      }}
    }}
  }});
}}
"#,
        socket = socket.display().to_string(),
        bin = jadecode_bin,
        ws = workspace.display().to_string(),
        instance = instance,
        card_js = card_js,
        project = project
    );
    if let Some(d) = path.parent() {
        std::fs::create_dir_all(d)?;
    }
    std::fs::write(path, src)?;
    Ok(())
}

impl PiDriver {
    pub async fn spawn(
        bin: &str,
        cwd: &Path,
        env: &BTreeMap<String, String>,
        extension: &Path,
        stderr_path: PathBuf,
        extra_args: &[String],
    ) -> Result<Self> {
        let mut cmd = tokio::process::Command::new(bin);
        cmd.arg("--mode")
            .arg("rpc")
            .arg("--no-session")
            .arg("-e")
            .arg(extension)
            .args(extra_args)
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
        let mut child = cmd.spawn().with_context(|| format!("spawn {bin}"))?;
        let pid = child.id().unwrap_or(0);
        let stdin = child.stdin.take().context("no stdin")?;
        let stdout = child.stdout.take().context("no stdout")?;
        let stderr = child.stderr.take();

        let (tx, rx) = mpsc::channel::<DriverEvent>(1024);
        let last_text = Arc::new(std::sync::Mutex::new(Vec::<String>::new()));
        let lt = last_text.clone();
        let tx_reader = tx.clone();
        let reader = tokio::spawn(async move {
            let tx = tx_reader;
            let mut lines = BufReader::new(stdout).lines();
            let mut buf = String::new();
            while let Ok(Some(line)) = lines.next_line().await {
                if line.trim().is_empty() {
                    continue;
                }
                let Ok(v) = serde_json::from_str::<Value>(&line) else {
                    continue;
                };
                for ev in map_pi_event(&v, &mut buf, &lt) {
                    if tx.send(ev).await.is_err() {
                        return;
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

        let _ = tx_init(&tx).await;
        Ok(Self {
            child,
            pid,
            stdin: Some(stdin),
            rx,
            reader: Some(reader),
            stderr: stderr_task,
            stderr_path,
            last_text,
        })
    }

    pub async fn prompt(&mut self, text: &str) -> Result<()> {
        let stdin = self.stdin.as_mut().context("pi stdin 닫힘")?;
        let line = json!({"type":"prompt","message":text}).to_string();
        stdin.write_all(line.as_bytes()).await?;
        stdin.write_all(b"\n").await?;
        stdin.flush().await?;
        Ok(())
    }

    pub async fn next_event(&mut self) -> Option<DriverEvent> {
        self.rx.recv().await
    }

    pub async fn cancel(&mut self) -> Result<()> {
        let stdin = self.stdin.as_mut().context("pi stdin 닫힘")?;
        stdin.write_all(b"{\"type\":\"abort\"}\n").await?;
        stdin.flush().await?;
        Ok(())
    }

    pub fn last_text(&self) -> Option<String> {
        self.last_text
            .lock()
            .ok()
            .and_then(|v| v.last().cloned())
    }

    pub async fn terminate(mut self) -> Result<()> {
        self.stdin.take();
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
        for t in [self.reader.take(), self.stderr.take()].into_iter().flatten() {
            t.abort();
        }
        Ok(())
    }
}

async fn tx_init(tx: &mpsc::Sender<DriverEvent>) -> Result<()> {
    tx.send(DriverEvent::Init {
        session_id: String::new(),
        model: "pi".into(),
        mcp_servers: vec![],
        tools: vec![],
    })
    .await?;
    Ok(())
}

/// pi RPC 이벤트 → `DriverEvent`.
pub fn map_pi_event(
    v: &Value,
    buf: &mut String,
    last: &Arc<std::sync::Mutex<Vec<String>>>,
) -> Vec<DriverEvent> {
    let ty = v.get("type").and_then(|x| x.as_str()).unwrap_or("");
    match ty {
        "message_update" => {
            let delta = v
                .get("assistantMessageEvent")
                .and_then(|e| e.get("delta").or_else(|| e.get("text")))
                .and_then(|x| x.as_str())
                .map(String::from)
                .unwrap_or_default();
            if delta.is_empty() {
                return vec![];
            }
            buf.push_str(&delta);
            vec![DriverEvent::Assistant {
                blocks: vec![AssistantBlock::Text { text: delta }],
                raw: v.clone(),
            }]
        }
        "tool_execution_start" => {
            let name = v
                .get("toolName")
                .and_then(|x| x.as_str())
                .unwrap_or("tool")
                .to_string();
            vec![DriverEvent::Assistant {
                blocks: vec![AssistantBlock::ToolUse {
                    id: v
                        .get("toolCallId")
                        .and_then(|x| x.as_str())
                        .unwrap_or("")
                        .to_string(),
                    name,
                    input: v.get("args").cloned().unwrap_or(Value::Null),
                }],
                raw: v.clone(),
            }]
        }
        "tool_execution_end" => vec![DriverEvent::ToolResults {
            blocks: vec![AssistantBlock::ToolResult {
                tool_use_id: v
                    .get("toolCallId")
                    .and_then(|x| x.as_str())
                    .unwrap_or("")
                    .to_string(),
                content: v.get("result").cloned().unwrap_or(Value::Null),
                is_error: v.get("isError").and_then(|x| x.as_bool()).unwrap_or(false),
            }],
        }],
        "turn_end" => {
            let text = std::mem::take(buf);
            if !text.trim().is_empty() {
                if let Ok(mut l) = last.lock() {
                    l.push(text.clone());
                }
            }
            let usage = v
                .get("message")
                .and_then(|m| m.get("usage"))
                .cloned()
                .unwrap_or(Value::Null);
            vec![DriverEvent::Result {
                subtype: "end_turn".into(),
                is_error: false,
                text: Some(text),
                usage: Usage {
                    input_tokens: usage.get("input").and_then(|x| x.as_u64()).unwrap_or(0),
                    output_tokens: usage.get("output").and_then(|x| x.as_u64()).unwrap_or(0),
                    cache_read_input_tokens: usage
                        .get("cacheRead")
                        .and_then(|x| x.as_u64())
                        .unwrap_or(0),
                    cache_creation_input_tokens: usage
                        .get("cacheWrite")
                        .and_then(|x| x.as_u64())
                        .unwrap_or(0),
                    cost_usd: usage
                        .get("cost")
                        .and_then(|c| c.get("total"))
                        .and_then(|x| x.as_f64())
                        .unwrap_or(0.0),
                    model: v
                        .get("message")
                        .and_then(|m| m.get("model"))
                        .and_then(|x| x.as_str())
                        .map(String::from),
                },
                num_turns: 1,
                session_id: v
                    .get("sessionId")
                    .and_then(|x| x.as_str())
                    .map(String::from),
                permission_denials: vec![],
            }]
        }
        "response" => {
            if v.get("success").and_then(|x| x.as_bool()) == Some(false) {
                let err = v.get("error").and_then(|x| x.as_str()).unwrap_or("");
                return vec![DriverEvent::Result {
                    subtype: "error".into(),
                    is_error: true,
                    text: Some(err.to_string()),
                    usage: Default::default(),
                    num_turns: 0,
                    session_id: None,
                    permission_denials: vec![],
                }];
            }
            vec![]
        }
        _ => vec![],
    }
}

pub static PI_TURN: AtomicI64 = AtomicI64::new(0);
pub fn touch() -> i64 {
    PI_TURN.fetch_add(1, Ordering::Relaxed)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn maps_text_and_turn_end() {
        let last = Arc::new(std::sync::Mutex::new(vec![]));
        let mut buf = String::new();
        let evs = map_pi_event(
            &json!({"type":"message_update","assistantMessageEvent":{"type":"text_delta","delta":"hi"}}),
            &mut buf,
            &last,
        );
        assert!(matches!(&evs[0], DriverEvent::Assistant { blocks, .. }
            if matches!(&blocks[0], AssistantBlock::Text { text } if text == "hi")));
        let evs = map_pi_event(
            &json!({"type":"turn_end","message":{"usage":{"input":7,"output":3,"cost":{"total":0.01}}}}),
            &mut buf,
            &last,
        );
        match &evs[0] {
            DriverEvent::Result { text, usage, .. } => {
                assert_eq!(text.as_deref(), Some("hi"));
                assert_eq!(usage.input_tokens, 7);
                assert!((usage.cost_usd - 0.01).abs() < 1e-9);
            }
            _ => panic!(),
        }
        assert_eq!(last.lock().unwrap().len(), 1);
    }

    #[test]
    fn maps_tool_and_error() {
        let last = Arc::new(std::sync::Mutex::new(vec![]));
        let mut buf = String::new();
        let evs = map_pi_event(
            &json!({"type":"tool_execution_start","toolName":"bash","toolCallId":"t","args":{"command":"ls"}}),
            &mut buf,
            &last,
        );
        assert!(matches!(&evs[0], DriverEvent::Assistant { blocks, .. }
            if blocks[0].is_tool_use("bash")));
        let evs = map_pi_event(
            &json!({"type":"response","command":"prompt","success":false,"error":"no key"}),
            &mut buf,
            &last,
        );
        assert!(matches!(&evs[0], DriverEvent::Result { is_error: true, .. }));
    }

    #[test]
    fn extension_file_is_written() {
        let d = std::env::temp_dir().join(format!("jc-pi-{}", jc_core::ulid::new_id()));
        std::fs::create_dir_all(&d).unwrap();
        let p = d.join("jc-extension.ts");
        write_extension(
            &p,
            Path::new("/tmp/x.sock"),
            "w-1",
            Some("work/p/c"),
            "p",
            "/usr/bin/jadecode",
            Path::new("/tmp/ws"),
        )
        .unwrap();
        let s = std::fs::read_to_string(&p).unwrap();
        assert!(s.contains("jadecode_report"));
        assert!(s.contains("/tmp/x.sock"));
        assert!(s.contains("w-1"));
        assert!(s.contains("/usr/bin/jadecode"));
        assert!(s.contains("work/p/c"));
    }
}
