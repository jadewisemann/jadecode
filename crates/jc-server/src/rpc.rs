//! JSON-RPC 2.0 요청/응답 형태.

use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

#[derive(Debug, Clone, Deserialize)]
pub struct Request {
    #[serde(default)]
    pub jsonrpc: String,
    #[serde(default)]
    pub id: Value,
    pub method: String,
    #[serde(default)]
    pub params: Value,
}

#[derive(Debug, Clone, Serialize)]
pub struct Error {
    pub code: i64,
    pub message: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub data: Option<Value>,
}

pub mod error_codes {
    pub const PARSE: i64 = -32700;
    pub const INVALID_REQUEST: i64 = -32600;
    pub const METHOD_NOT_FOUND: i64 = -32601;
    pub const INVALID_PARAMS: i64 = -32602;
    pub const INTERNAL: i64 = -32603;
    /// 서버가 거절함(정책·상태). `data.reason`에 이유.
    pub const REFUSED: i64 = 4000;
}

impl Error {
    pub fn new(code: i64, message: impl Into<String>) -> Self {
        Self {
            code,
            message: message.into(),
            data: None,
        }
    }
    pub fn refused(message: impl Into<String>, reason: &str) -> Self {
        Self {
            code: error_codes::REFUSED,
            message: message.into(),
            data: Some(json!({ "reason": reason })),
        }
    }
    pub fn invalid_params(message: impl Into<String>) -> Self {
        Self::new(error_codes::INVALID_PARAMS, message)
    }
    pub fn internal(message: impl Into<String>) -> Self {
        Self::new(error_codes::INTERNAL, message)
    }
}

pub fn ok(id: &Value, result: Value) -> String {
    json!({"jsonrpc":"2.0","id":id,"result":result}).to_string()
}

pub fn err(id: &Value, e: Error) -> String {
    json!({"jsonrpc":"2.0","id":id,"error":e}).to_string()
}

pub fn notify(method: &str, params: Value) -> String {
    json!({"jsonrpc":"2.0","method":method,"params":params}).to_string()
}

/// 지원 메서드 이름(자기 서술·계약 테스트용).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Method {
    ServerInfo,
    ServerStats,
    EventsAppend,
    EventsSubscribe,
    EventsHead,
    EventsTail,
    CardsList,
    CardGet,
    CardSetStatus,
    CardAdvance,
    CardAppendEvent,
    CardAdd,
    ProjectAdd,
    ProjectsList,
    InstancesList,
    InstanceStart,
    InstanceSend,
    InstanceStop,
    PermissionRequest,
    PermissionResolve,
    BotsList,
    AgentsList,
    PolicyGet,
    /// 수집함·커넥터(외부 사이클) 메서드. 이름을 그대로 들고 있다.
    Ext(&'static str),
}

/// 외부 사이클 메서드 이름.
pub const EXT_METHODS: &[&str] = &[
    "intake.add",
    "intake.list",
    "intake.get",
    "intake.triage",
    "intake.close",
    "intake.reply",
    "intake.sync_github",
    "connectors.list",
    "connectors.presets",
    "connectors.save",
    "connectors.remove",
    "connectors.sync",
    "connectors.test",
];

impl Method {
    pub fn parse(s: &str) -> Option<Self> {
        Some(match s {
            "server.info" => Method::ServerInfo,
            "server.stats" => Method::ServerStats,
            "events.append" => Method::EventsAppend,
            "events.subscribe" => Method::EventsSubscribe,
            "events.head" => Method::EventsHead,
            "events.tail" => Method::EventsTail,
            "cards.list" => Method::CardsList,
            "card.get" => Method::CardGet,
            "card.set_status" => Method::CardSetStatus,
            "card.advance" => Method::CardAdvance,
            "card.append_event" => Method::CardAppendEvent,
            "card.add" => Method::CardAdd,
            "project.add" => Method::ProjectAdd,
            "projects.list" => Method::ProjectsList,
            "instances.list" => Method::InstancesList,
            "instance.start" => Method::InstanceStart,
            "instance.send" => Method::InstanceSend,
            "instance.stop" => Method::InstanceStop,
            "permission.request" => Method::PermissionRequest,
            "permission.resolve" => Method::PermissionResolve,
            "bots.list" => Method::BotsList,
            "agents.list" => Method::AgentsList,
            "policy.get" => Method::PolicyGet,
            other => Method::Ext(EXT_METHODS.iter().find(|m| **m == other)?),
        })
    }

    pub fn names() -> Vec<&'static str> {
        let mut v: Vec<&'static str> = vec![
            "server.info",
            "server.stats",
            "events.append",
            "events.subscribe",
            "events.head",
            "events.tail",
            "cards.list",
            "card.get",
            "card.set_status",
            "card.advance",
            "card.append_event",
            "card.add",
            "project.add",
            "projects.list",
            "instances.list",
            "instance.start",
            "instance.send",
            "instance.stop",
            "permission.request",
            "permission.resolve",
            "bots.list",
            "agents.list",
            "policy.get",
        ];
        v.extend_from_slice(EXT_METHODS);
        v
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn names_parse() {
        for n in &Method::names() {
            assert!(Method::parse(n).is_some(), "{n}");
        }
        assert!(Method::parse("nope").is_none());
    }
}
