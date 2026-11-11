//! jadecode MCP 서버(stdio). 인스턴스마다 하나 붙는다(`--mcp-config --strict-mcp-config`).
//!
//! 도구(S3 H1 실측: 완료 카드 26/26이 지침 없이 `report(done)`을 불렀다):
//! - `report(status, summary)` — 보고. status: done | progress | blocked
//! - `ask(question, options?)` — 질문. 답은 나중에 사용자 턴으로 온다(막지 않는다)
//! - `propose(summary, detail?)` — 사람 절차 제안(병합·배포 등)
//! - `error(message)` — 오류 보고
//! - `read_card()` — 카드 문서 읽기
//! - `permission(tool_name, input, tool_use_id)` — Claude Code 권한 RPC
//!
//! 모든 호출은 JSON-RPC로 jadecode 서버에 넘긴다(`JADECODE_RPC` 소켓).

pub mod rpc_client;
pub mod tools;

pub use tools::{Handle, Role, Tools};

/// MCP 프로토콜 판. Claude Code가 요청한 값을 그대로 돌려준다.
pub const PROTOCOL_VERSION: &str = "2024-11-05";
