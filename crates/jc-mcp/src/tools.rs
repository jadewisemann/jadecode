//! MCP 도구 정의와 처리. 전송(stdio)과 분리해 단위 테스트할 수 있게 한다.

use crate::rpc_client;
use anyhow::Result;
use serde_json::{json, Value};
use std::path::PathBuf;
use std::sync::{Arc, Mutex};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Role {
    Worker,
    Orchestrator,
}

impl Role {
    pub fn parse(s: &str) -> Self {
        match s {
            "orchestrator" => Role::Orchestrator,
            _ => Role::Worker,
        }
    }
    pub fn as_str(&self) -> &'static str {
        match self {
            Role::Worker => "worker",
            Role::Orchestrator => "orchestrator",
        }
    }
}

/// 한 인스턴스의 문맥: 누구인지, 어느 카드인지, 어디로 말하는지.
#[derive(Debug, Clone)]
pub struct Handle {
    pub role: Role,
    pub instance_id: String,
    pub card: Option<String>,
    pub project: String,
    pub socket: PathBuf,
}

pub struct Tools {
    pub handle: Handle,
    _lock: Mutex<()>,
}

fn tool(name: &str, description: &str, schema: Value) -> Value {
    json!({"name": name, "description": description, "inputSchema": schema})
}

impl Tools {
    pub fn new(handle: Handle) -> Self {
        Self {
            handle,
            _lock: Mutex::new(()),
        }
    }

    /// 역할별 도구 목록. 권한 도구는 두 역할 모두에게 준다(권한 RPC가 여기로 온다).
    pub fn list(&self) -> Vec<Value> {
        let mut out = vec![];
        if self.handle.role == Role::Worker {
            out.push(tool(
                "report",
                "카드 작업 상황을 오케스트레이터에게 보고한다. 단계를 끝냈으면 status=done, 계속 일하는 중이면 progress, 막혔으면 blocked.",
                json!({
                    "type": "object",
                    "properties": {
                        "status": {"type": "string", "enum": ["done", "progress", "blocked"],
                                   "description": "done=단계 완료, progress=진행 중, blocked=막힘"},
                        "summary": {"type": "string", "description": "한두 문장 요약"},
                        "evidence": {"type": "array", "items": {"type": "string"},
                                     "description": "근거 파일·명령·링크"}
                    },
                    "required": ["status", "summary"]
                }),
            ));
            out.push(tool(
                "ask",
                "사람이나 오케스트레이터에게 질문한다. 답은 나중에 새 메시지로 온다. 묻고 나면 할 수 있는 일을 계속하거나 멈춘다.",
                json!({
                    "type": "object",
                    "properties": {
                        "question": {"type": "string"},
                        "options": {"type": "array", "items": {"type": "string"},
                                    "description": "고를 수 있는 답(선택)"}
                    },
                    "required": ["question"]
                }),
            ));
            out.push(tool(
                "propose",
                "사람 절차가 필요한 일을 제안한다(보호 브랜치 병합, 배포, 외부로 나가는 메시지, 예산 초과, 프로젝트 삭제). 직접 하지 말고 제안만 한다.",
                json!({
                    "type": "object",
                    "properties": {
                        "summary": {"type": "string", "description": "무엇을 승인받으려는가"},
                        "detail": {"type": "string", "description": "근거·명령·링크"}
                    },
                    "required": ["summary"]
                }),
            ));
            out.push(tool(
                "error",
                "진행할 수 없는 오류를 보고한다(도구 실패, 환경 문제).",
                json!({
                    "type": "object",
                    "properties": {"message": {"type": "string"}},
                    "required": ["message"]
                }),
            ));
            out.push(tool(
                "read_card",
                "이 카드의 task.md 전문을 읽는다.",
                json!({"type": "object", "properties": {}}),
            ));
        } else {
            out.push(tool(
                "ack",
                "받은 이벤트를 여기까지 처리했다고 기록한다. 다이제스트의 마지막 seq를 넣는다.",
                json!({
                    "type": "object",
                    "properties": {"upto": {"type": "integer", "description": "마지막으로 처리한 seq"}},
                    "required": ["upto"]
                }),
            ));
            out.push(tool(
                "send",
                "워커 인스턴스에게 메시지를 보낸다(다음 사용자 턴으로 들어간다).",
                json!({
                    "type": "object",
                    "properties": {
                        "instance": {"type": "string", "description": "인스턴스 id"},
                        "text": {"type": "string"},
                        "reply_to": {"type": "integer", "description": "응답하는 원래 메시지 seq(선택)"}
                    },
                    "required": ["instance", "text"]
                }),
            ));
            out.push(tool(
                "reply",
                "워커의 질문·보고에 답한다.",
                json!({
                    "type": "object",
                    "properties": {
                        "instance": {"type": "string"},
                        "text": {"type": "string"},
                        "reply_to": {"type": "integer"}
                    },
                    "required": ["instance", "text"]
                }),
            ));
            out.push(tool(
                "escalate",
                "사람 결정이 필요한 일을 올린다.",
                json!({
                    "type": "object",
                    "properties": {
                        "summary": {"type": "string"},
                        "detail": {"type": "string"},
                        "escalated_from": {"type": "integer"}
                    },
                    "required": ["summary"]
                }),
            ));
            out.push(tool(
                "read_event",
                "이벤트 이력을 읽는다.",
                json!({
                    "type": "object",
                    "properties": {
                        "topics": {"type": "array", "items": {"type": "string"}},
                        "limit": {"type": "integer"}
                    }
                }),
            ));
        }
        out.push(tool(
            "permission",
            "권한 요청 처리기(Claude Code가 부른다). jadecode 서버가 정책으로 답한다.",
            json!({
                "type": "object",
                "properties": {
                    "tool_name": {"type": "string"},
                    "input": {"type": "object"},
                    "tool_use_id": {"type": "string"}
                },
                "required": ["tool_name", "input"]
            }),
        ));
        out
    }

    /// 서버 호출 한 번. 연결은 호출마다 새로 연다.
    fn rpc(&self, method: &str, params: Value) -> Result<Value> {
        rpc_client::call(&self.handle.socket, method, params)
    }

    /// 호출 하나를 처리하고 MCP `content` 응답을 만든다.
    pub fn call(&self, name: &str, args: &Value) -> Value {
        match self.call_inner(name, args) {
            Ok(v) => json!({"content": [{"type": "text", "text": serde_json::to_string(&v).unwrap_or_default()}],
                            "isError": false}),
            Err(e) => json!({"content": [{"type": "text", "text": format!("error: {e}")}], "isError": true}),
        }
    }

    fn topic(&self) -> String {
        match (&self.handle.card, self.handle.role) {
            (Some(c), _) => jc_core::topic::card_topic(c),
            (None, Role::Orchestrator) => format!("project/{}", self.handle.project),
            (None, Role::Worker) => "global".to_string(),
        }
    }

    fn call_inner(&self, name: &str, args: &Value) -> Result<Value> {
        let actor = json!({
            "role": self.handle.role.as_str(),
            "instance": self.handle.instance_id,
            "card": self.handle.card,
        });
        match name {
            "report" => {
                let status = args["status"].as_str().unwrap_or("progress");
                if !["done", "progress", "blocked"].contains(&status) {
                    anyhow::bail!("status must be done|progress|blocked");
                }
                let body = json!({
                    "status": status,
                    "summary": args["summary"].as_str().unwrap_or(""),
                    "evidence": args.get("evidence").cloned().unwrap_or(json!([])),
                    "actor": actor,
                });
                let kind = if status == "done" { "msg.report_done" } else { "msg.report" };
                // 보고가 곧 상태 신호다. 카드 상태 칸도 함께 움직인다.
                if let Some(card) = &self.handle.card {
                    let st = match status {
                        "done" => "waiting_human",
                        "blocked" => "blocked",
                        _ => "working",
                    };
                    let _ = self.rpc("card.set_status", json!({"card": card, "status": st}));
                }
                self.rpc("events.append", json!({"topic": self.topic(), "kind": kind, "body": body}))
            }
            "ask" => {
                let q = args["question"].as_str().unwrap_or("");
                if q.is_empty() {
                    anyhow::bail!("question required");
                }
                let body = json!({
                    "question": q,
                    "options": args.get("options").cloned().unwrap_or(json!([])),
                    "actor": actor,
                });
                self.rpc("events.append", json!({"topic": self.topic(), "kind": "msg.ask", "body": body}))
            }
            "propose" => {
                let s = args["summary"].as_str().unwrap_or("");
                if s.is_empty() {
                    anyhow::bail!("summary required");
                }
                let body = json!({
                    "summary": s,
                    "detail": args["detail"].as_str().unwrap_or(""),
                    "kind": "human_procedure",
                    "actor": actor,
                });
                self.rpc("events.append", json!({"topic": self.topic(), "kind": "msg.propose", "body": body}))
            }
            "error" => {
                let m = args["message"].as_str().unwrap_or("");
                self.rpc(
                    "events.append",
                    json!({"topic": self.topic(), "kind": "msg.error", "body": {"message": m, "actor": actor}}),
                )
            }
            "read_card" => {
                let Some(card) = &self.handle.card else {
                    anyhow::bail!("no card bound");
                };
                let v = self.rpc("card.get", json!({"card": card}))?;
                Ok(json!({"markdown": v["markdown"], "path": v["path"]}))
            }
            "permission" => {
                // 권한 RPC: 서버가 정책으로 답한다. 서버가 없거나 실패하면 도구 오류로 끝난다(S2 H2).
                self.rpc(
                    "permission.request",
                    json!({
                        "tool_name": args["tool_name"],
                        "input": args["input"],
                        "tool_use_id": args["tool_use_id"],
                        "instance": self.handle.instance_id,
                        "card": self.handle.card,
                    }),
                )
            }
            "ack" => {
                let upto = args["upto"].as_u64().unwrap_or(0);
                let body = json!({"upto": upto, "actor": actor});
                self.rpc("events.append", json!({"topic": self.topic(), "kind": "msg.ack", "body": body}))
            }
            "send" | "reply" => {
                let inst = args["instance"].as_str().unwrap_or("");
                let text = args["text"].as_str().unwrap_or("");
                let kind = if name == "reply" { "msg.reply" } else { "msg.instruction" };
                let body = json!({
                    "to": inst,
                    "text": text,
                    "reply_to": args.get("reply_to").cloned().unwrap_or(Value::Null),
                    "actor": actor,
                });
                let seq = self.rpc("events.append", json!({"topic": self.topic(), "kind": kind, "body": body}))?;
                let _ = self.rpc("instance.send", json!({"instance": inst, "text": text}));
                Ok(seq)
            }
            "escalate" => {
                let body = json!({
                    "summary": args["summary"],
                    "detail": args.get("detail").cloned().unwrap_or(json!("")),
                    "escalated_from": args.get("escalated_from").cloned().unwrap_or(Value::Null),
                    "actor": actor,
                });
                self.rpc("events.append", json!({"topic": self.topic(), "kind": "msg.escalation", "body": body}))
            }
            "read_event" => self.rpc(
                "events.tail",
                json!({
                    "topics": args.get("topics").cloned().unwrap_or(json!([])),
                    "limit": args.get("limit").cloned().unwrap_or(json!(30)),
                }),
            ),
            other => anyhow::bail!("unknown tool {other}"),
        }
    }
}

/// 권한 응답 형태(Claude Code `--permission-prompt-tool` 계약).
pub fn allow_response(input: &Value, extra: Option<Value>) -> Value {
    let mut updated = input.clone();
    if let (Some(e), Some(o)) = (extra, updated.as_object_mut()) {
        if let Some(m) = e.as_object() {
            for (k, v) in m {
                o.insert(k.clone(), v.clone());
            }
        }
    }
    json!({"behavior": "allow", "updatedInput": updated})
}

pub fn deny_response(message: &str) -> Value {
    json!({"behavior": "deny", "message": message})
}

pub fn arc_tools(handle: Handle) -> Arc<Tools> {
    Arc::new(Tools::new(handle))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn worker() -> Tools {
        Tools::new(Handle {
            role: Role::Worker,
            instance_id: "i1".into(),
            card: Some("work/p1/c1".into()),
            project: "p1".into(),
            socket: PathBuf::from("/nonexistent"),
        })
    }

    #[test]
    fn worker_tools_include_messaging_and_permission() {
        let t = worker();
        let names: Vec<String> = t
            .list()
            .iter()
            .map(|x| x["name"].as_str().unwrap().to_string())
            .collect();
        for n in ["report", "ask", "propose", "error", "read_card", "permission"] {
            assert!(names.contains(&n.to_string()), "{names:?}");
        }
    }

    #[test]
    fn orchestrator_tools_differ() {
        let t = Tools::new(Handle {
            role: Role::Orchestrator,
            instance_id: "o1".into(),
            card: None,
            project: "p1".into(),
            socket: PathBuf::from("/nonexistent"),
        });
        let names: Vec<String> = t
            .list()
            .iter()
            .map(|x| x["name"].as_str().unwrap().to_string())
            .collect();
        for n in ["ack", "send", "reply", "escalate", "read_event", "permission"] {
            assert!(names.contains(&n.to_string()), "{names:?}");
        }
        assert!(!names.contains(&"report".to_string()));
    }

    #[test]
    fn permission_responses_follow_contract() {
        let a = allow_response(&json!({"command":"ls"}), Some(json!({"answers":{"q":"a"}})));
        assert_eq!(a["behavior"], "allow");
        assert_eq!(a["updatedInput"]["command"], "ls");
        assert_eq!(a["updatedInput"]["answers"]["q"], "a");
        let d = deny_response("not allowed");
        assert_eq!(d["behavior"], "deny");
        assert_eq!(d["message"], "not allowed");
    }

    #[test]
    fn bad_report_status_is_error() {
        let t = worker();
        let r = t.call("report", &json!({"status": "nope", "summary": "x"}));
        assert_eq!(r["isError"], true);
    }

    #[test]
    fn unknown_tool_is_error() {
        let t = worker();
        let r = t.call("nope", &json!({}));
        assert_eq!(r["isError"], true);
    }
}
