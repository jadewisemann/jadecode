//! stream-json 이벤트 해석.
//!
//! `claude -p --input-format stream-json --output-format stream-json --verbose`가 내는 줄.
//! 판마다 바뀔 수 있으므로 모르는 것은 `Unknown`으로 두고 버린다(계약 테스트가 잡는다).

use serde::{Deserialize, Serialize};
use serde_json::Value;

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct Usage {
    #[serde(default)]
    pub input_tokens: u64,
    #[serde(default)]
    pub output_tokens: u64,
    #[serde(default)]
    pub cache_creation_input_tokens: u64,
    #[serde(default)]
    pub cache_read_input_tokens: u64,
    /// 공시 단가 환산 금액(구독이라 실제 청구 아님).
    #[serde(default)]
    pub cost_usd: f64,
    #[serde(default)]
    pub model: Option<String>,
}

impl Usage {
    pub fn total_tokens(&self) -> u64 {
        self.input_tokens + self.output_tokens + self.cache_creation_input_tokens + self.cache_read_input_tokens
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "kebab-case")]
pub enum AssistantBlock {
    Text { text: String },
    Thinking { thinking: String },
    ToolUse { id: String, name: String, input: Value },
    ToolResult { tool_use_id: String, content: Value, is_error: bool },
    Other(Value),
}

#[derive(Debug, Clone)]
pub enum DriverEvent {
    /// 턴 시작. 턴마다 다시 나온다(S3).
    Init {
        session_id: String,
        model: String,
        mcp_servers: Vec<McpServerState>,
        tools: Vec<String>,
    },
    Assistant {
        blocks: Vec<AssistantBlock>,
        raw: Value,
    },
    /// 도구 결과(사용자 역할로 돌아오는 줄).
    ToolResults {
        blocks: Vec<AssistantBlock>,
    },
    /// 턴 끝 = 한가함. `is_error`면 실패.
    Result {
        subtype: String,
        is_error: bool,
        text: Option<String>,
        usage: Usage,
        num_turns: u64,
        session_id: Option<String>,
        permission_denials: Vec<Value>,
    },
    /// `control_request`에 대한 응답(interrupt 확인 등).
    ControlResponse {
        request_id: String,
        ok: bool,
        payload: Value,
    },
    Other(Value),
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct McpServerState {
    pub name: String,
    pub status: String,
}

fn s(v: &Value, k: &str) -> Option<String> {
    v.get(k).and_then(|x| x.as_str()).map(String::from)
}

fn usage_of(v: &Value) -> Usage {
    let u = v.get("usage").unwrap_or(&Value::Null);
    let mut out = Usage {
        input_tokens: u.get("input_tokens").and_then(|x| x.as_u64()).unwrap_or(0),
        output_tokens: u.get("output_tokens").and_then(|x| x.as_u64()).unwrap_or(0),
        cache_creation_input_tokens: u
            .get("cache_creation_input_tokens")
            .and_then(|x| x.as_u64())
            .unwrap_or(0),
        cache_read_input_tokens: u.get("cache_read_input_tokens").and_then(|x| x.as_u64()).unwrap_or(0),
        cost_usd: v.get("total_cost_usd").and_then(|x| x.as_f64()).unwrap_or(0.0),
        model: v
            .get("modelUsage")
            .and_then(|m| m.as_object())
            .and_then(|m| m.keys().next().cloned()),
    };
    // modelUsage 에는 모델별 사용량이 온다. 첫 모델의 토큰을 보조로 쓴다.
    if out.total_tokens() == 0 {
        if let Some(mu) = v.get("modelUsage").and_then(|m| m.as_object()) {
            for (name, m) in mu {
                out.model = Some(name.clone());
                out.input_tokens = m.get("inputTokens").and_then(|x| x.as_u64()).unwrap_or(0);
                out.output_tokens = m.get("outputTokens").and_then(|x| x.as_u64()).unwrap_or(0);
                out.cache_creation_input_tokens = m
                    .get("cacheCreationInputTokens")
                    .and_then(|x| x.as_u64())
                    .unwrap_or(0);
                out.cache_read_input_tokens =
                    m.get("cacheReadInputTokens").and_then(|x| x.as_u64()).unwrap_or(0);
                break;
            }
        }
    }
    out
}

fn blocks_of(content: &Value) -> Vec<AssistantBlock> {
    let arr = match content.as_array() {
        Some(a) => a,
        None => return vec![],
    };
    arr.iter()
        .map(|b| match b.get("type").and_then(|t| t.as_str()).unwrap_or("") {
            "text" => AssistantBlock::Text {
                text: b.get("text").and_then(|t| t.as_str()).unwrap_or("").to_string(),
            },
            "thinking" => AssistantBlock::Thinking {
                thinking: b.get("thinking").and_then(|t| t.as_str()).unwrap_or("").to_string(),
            },
            "tool_use" => AssistantBlock::ToolUse {
                id: s(b, "id").unwrap_or_default(),
                name: s(b, "name").unwrap_or_default(),
                input: b.get("input").cloned().unwrap_or(Value::Null),
            },
            "tool_result" => AssistantBlock::ToolResult {
                tool_use_id: s(b, "tool_use_id").unwrap_or_default(),
                content: b.get("content").cloned().unwrap_or(Value::Null),
                is_error: b.get("is_error").and_then(|x| x.as_bool()).unwrap_or(false),
            },
            _ => AssistantBlock::Other(b.clone()),
        })
        .collect()
}

impl AssistantBlock {
    pub fn is_tool_use(&self, name: &str) -> bool {
        matches!(self, AssistantBlock::ToolUse { name: n, .. } if n == name)
    }
}

/// stdout 한 줄 → 이벤트. 해석할 수 없으면 `None`.
pub fn parse_line(line: &str) -> Option<DriverEvent> {
    let v: Value = serde_json::from_str(line).ok()?;
    let ty = v.get("type").and_then(|x| x.as_str()).unwrap_or("");
    Some(match ty {
        "system" => {
            let sub = v.get("subtype").and_then(|x| x.as_str()).unwrap_or("");
            if sub != "init" {
                return Some(DriverEvent::Other(v));
            }
            let mcp_servers = v
                .get("mcp_servers")
                .and_then(|x| x.as_array())
                .map(|a| {
                    a.iter()
                        .filter_map(|m| {
                            Some(McpServerState {
                                name: s(m, "name")?,
                                status: s(m, "status").unwrap_or_else(|| "unknown".into()),
                            })
                        })
                        .collect()
                })
                .unwrap_or_default();
            let tools = v
                .get("tools")
                .and_then(|x| x.as_array())
                .map(|a| a.iter().filter_map(|t| t.as_str().map(String::from)).collect())
                .unwrap_or_default();
            DriverEvent::Init {
                session_id: s(&v, "session_id").unwrap_or_default(),
                model: s(&v, "model").unwrap_or_default(),
                mcp_servers,
                tools,
            }
        }
        "assistant" => {
            let content = v
                .get("message")
                .and_then(|m| m.get("content"))
                .cloned()
                .unwrap_or(Value::Null);
            DriverEvent::Assistant {
                blocks: blocks_of(&content),
                raw: v,
            }
        }
        "user" => {
            let content = v
                .get("message")
                .and_then(|m| m.get("content"))
                .cloned()
                .unwrap_or(Value::Null);
            DriverEvent::ToolResults {
                blocks: blocks_of(&content),
            }
        }
        "result" => DriverEvent::Result {
            subtype: s(&v, "subtype").unwrap_or_default(),
            is_error: v.get("is_error").and_then(|x| x.as_bool()).unwrap_or(false),
            text: s(&v, "result").or_else(|| s(&v, "error")),
            usage: usage_of(&v),
            num_turns: v.get("num_turns").and_then(|x| x.as_u64()).unwrap_or(0),
            session_id: s(&v, "session_id"),
            permission_denials: v
                .get("permission_denials")
                .and_then(|x| x.as_array())
                .cloned()
                .unwrap_or_default(),
        },
        "control_response" => {
            let resp = v.get("response").cloned().unwrap_or(Value::Null);
            DriverEvent::ControlResponse {
                request_id: resp
                    .get("request_id")
                    .and_then(|x| x.as_str())
                    .or_else(|| v.get("request_id").and_then(|x| x.as_str()))
                    .unwrap_or("")
                    .to_string(),
                ok: resp.get("subtype").and_then(|x| x.as_str()) == Some("success"),
                payload: resp,
            }
        }
        _ => DriverEvent::Other(v),
    })
}

/// stdin에 쓰는 사용자 턴 한 줄.
pub fn user_turn(text: &str, session_id: &str) -> String {
    serde_json::json!({
        "type": "user",
        "message": {"role": "user", "content": text},
        "parent_tool_use_id": null,
        "session_id": session_id,
    })
    .to_string()
}

/// 제어 요청 id 하나(짧은 무작위).
pub fn interrupt_request_id() -> String {
    use rand::Rng;
    let n: u64 = rand::rng().random();
    format!("{n:016x}")
}

/// interrupt 제어 요청 한 줄.
pub fn interrupt(request_id: &str) -> String {
    serde_json::json!({
        "type": "control_request",
        "request_id": request_id,
        "request": {"subtype": "interrupt"},
    })
    .to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_init() {
        let line = r#"{"type":"system","subtype":"init","session_id":"abc","model":"claude-haiku","mcp_servers":[{"name":"jadecode","status":"connected"}],"tools":["Bash","Read"]}"#;
        match parse_line(line).unwrap() {
            DriverEvent::Init { session_id, mcp_servers, tools, .. } => {
                assert_eq!(session_id, "abc");
                assert_eq!(mcp_servers[0].status, "connected");
                assert!(tools.contains(&"Bash".to_string()));
            }
            other => panic!("{other:?}"),
        }
    }

    #[test]
    fn parses_assistant_tool_use() {
        let line = r#"{"type":"assistant","message":{"content":[{"type":"text","text":"hi"},{"type":"tool_use","id":"t1","name":"mcp__jadecode__report","input":{"status":"done"}}]}}"#;
        match parse_line(line).unwrap() {
            DriverEvent::Assistant { blocks, .. } => {
                assert_eq!(blocks.len(), 2);
                assert!(blocks[1].is_tool_use("mcp__jadecode__report"));
            }
            other => panic!("{other:?}"),
        }
    }

    #[test]
    fn parses_result_usage() {
        let line = r#"{"type":"result","subtype":"success","is_error":false,"result":"ok","num_turns":2,"session_id":"s","usage":{"input_tokens":10,"output_tokens":5},"total_cost_usd":0.01}"#;
        match parse_line(line).unwrap() {
            DriverEvent::Result { is_error, text, usage, num_turns, .. } => {
                assert!(!is_error);
                assert_eq!(text.as_deref(), Some("ok"));
                assert_eq!(usage.total_tokens(), 15);
                assert!((usage.cost_usd - 0.01).abs() < 1e-9);
                assert_eq!(num_turns, 2);
            }
            other => panic!("{other:?}"),
        }
    }

    #[test]
    fn unknown_line_is_other() {
        assert!(matches!(parse_line(r#"{"type":"weird"}"#).unwrap(), DriverEvent::Other(_)));
        assert!(parse_line("not json").is_none());
    }

    #[test]
    fn input_lines_are_json() {
        let t = user_turn("hello", "sid");
        let v: Value = serde_json::from_str(&t).unwrap();
        assert_eq!(v["type"], "user");
        assert_eq!(v["message"]["content"], "hello");
        let i: Value = serde_json::from_str(&interrupt("r1")).unwrap();
        assert_eq!(i["request"]["subtype"], "interrupt");
    }
}
