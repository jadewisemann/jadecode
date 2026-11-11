//! 에이전트 백엔드 선택과 공통 인터페이스.
//!
//! 실측(2026-11-11)으로 확인한 실제 지원 범위:
//!
//! | 에이전트 | 구동 방식 | 구조화 실행 | MCP 도구 | 파일 쓰기 |
//! |---|---|---|---|---|
//! | Claude Code | `claude -p --input-format stream-json` | 있음(자체) | 있음 | 자기 도구 |
//! | OpenCode | `opencode acp` (ACP) | 있음(ACP) | ACP `mcpServers` | 클라이언트 fs |
//! | omp | `omp acp` (ACP) | 있음(ACP) | ACP `mcpServers` | 클라이언트 fs |
//! | pi | `pi --mode rpc` (JSONL RPC) | 있음(자체 RPC) | **없음** → 확장으로 도구 주입 | 자기 도구 |
//!
//! 셋 다 "한 프로세스에 여러 턴"과 "세션 id로 이어 붙이기"가 된다. Claude만 프로토콜이 다르고,
//! OpenCode·omp는 ACP로 같으며, pi는 자체 RPC라 별도 구현이 필요하다.

use anyhow::{bail, Result};
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum AgentKind {
    /// Claude Code `claude -p` stream-json (S1~S4 실측)
    Claude,
    /// OpenCode ACP (`opencode acp`)
    OpenCode,
    /// omp ACP (`omp acp`)
    Omp,
    /// pi RPC (`pi --mode rpc`)
    Pi,
}

impl AgentKind {
    pub fn parse(s: &str) -> Result<Self> {
        Ok(match s.to_ascii_lowercase().as_str() {
            "claude" | "claude-code" | "claude_code" => AgentKind::Claude,
            "opencode" | "open-code" | "oc" => AgentKind::OpenCode,
            "omp" => AgentKind::Omp,
            "pi" => AgentKind::Pi,
            other => bail!("unknown agent {other:?} (claude|opencode|omp|pi)"),
        })
    }

    pub fn as_str(&self) -> &'static str {
        match self {
            AgentKind::Claude => "claude",
            AgentKind::OpenCode => "opencode",
            AgentKind::Omp => "omp",
            AgentKind::Pi => "pi",
        }
    }

    /// 기본 실행 파일 이름.
    pub fn default_bin(&self) -> &'static str {
        match self {
            AgentKind::Claude => "claude",
            AgentKind::OpenCode => "opencode",
            AgentKind::Omp => "omp",
            AgentKind::Pi => "pi",
        }
    }

    /// 전송 방식.
    pub fn protocol(&self) -> Protocol {
        match self {
            AgentKind::Claude => Protocol::ClaudeStreamJson,
            AgentKind::OpenCode | AgentKind::Omp => Protocol::Acp,
            AgentKind::Pi => Protocol::PiRpc,
        }
    }

    /// 구조화된 MCP 도구를 줄 수 있는가(pi는 확장으로 대신한다).
    pub fn supports_mcp(&self) -> bool {
        matches!(self, AgentKind::Claude | AgentKind::OpenCode | AgentKind::Omp)
    }

    /// 파일 쓰기를 클라이언트가 대신 하는가(ACP는 클라이언트 fs).
    pub fn client_writes_files(&self) -> bool {
        matches!(self, AgentKind::OpenCode | AgentKind::Omp)
    }

    pub fn all() -> &'static [AgentKind] {
        &[
            AgentKind::Claude,
            AgentKind::OpenCode,
            AgentKind::Omp,
            AgentKind::Pi,
        ]
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Protocol {
    ClaudeStreamJson,
    Acp,
    PiRpc,
}

impl Protocol {
    pub fn as_str(&self) -> &'static str {
        match self {
            Protocol::ClaudeStreamJson => "claude-stream-json",
            Protocol::Acp => "acp",
            Protocol::PiRpc => "pi-rpc",
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_aliases() {
        assert_eq!(AgentKind::parse("claude").unwrap(), AgentKind::Claude);
        assert_eq!(AgentKind::parse("OpenCode").unwrap(), AgentKind::OpenCode);
        assert_eq!(AgentKind::parse("omp").unwrap(), AgentKind::Omp);
        assert_eq!(AgentKind::parse("pi").unwrap(), AgentKind::Pi);
        assert!(AgentKind::parse("gemini").is_err());
    }

    #[test]
    fn protocols_and_capabilities() {
        assert_eq!(AgentKind::Claude.protocol(), Protocol::ClaudeStreamJson);
        assert_eq!(AgentKind::OpenCode.protocol(), Protocol::Acp);
        assert_eq!(AgentKind::Omp.protocol(), Protocol::Acp);
        assert_eq!(AgentKind::Pi.protocol(), Protocol::PiRpc);
        assert!(!AgentKind::Pi.supports_mcp());
        assert!(AgentKind::Omp.supports_mcp());
        assert!(AgentKind::OpenCode.client_writes_files());
        assert!(!AgentKind::Claude.client_writes_files());
    }
}
