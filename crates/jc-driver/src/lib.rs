//! A2 드라이버: Claude Code를 `-p --input-format stream-json --output-format stream-json`로
//! 상주시켜 구조화 입출력으로 몬다.
//!
//! 스파이크가 실측으로 정한 것만 넣는다([`docs/architecture/candidates.md`] 「A2 드라이버 계약」).
//! - 격리 플래그: `--setting-sources "" --strict-mcp-config`로 사용자 전역 지침·훅·커넥터 MCP를 뺀다(S1 H4).
//! - 자식 환경변수는 화이트리스트(S1·S4). 부모의 `CLAUDECODE`·`CLAUDE_CODE_*`를 끊는다.
//! - 세션 id는 서버가 만든 uuid. 처음은 `--session-id`, 이후 재시작은 같은 폴더에서 `--resume`.
//! - 턴 경계는 서버가 쥔다. 한가할 때(`result`) 넣고, 급한 것만 즉시 넣는다.
//! - `cancel()`은 `control_request{subtype:interrupt}`.
//! - 종료는 SIGTERM → 5초 → SIGKILL(프로세스 그룹까지).

pub mod acp;
pub mod agent;
pub mod any;
pub mod pi;
pub mod proc;
pub mod spec;
pub mod stream;

pub use acp::{AcpDriver, AcpHost, AcpPermission};
pub use agent::{AgentKind, Protocol};
pub use any::{resolve_bin, AgentDriver, SpawnHooks};
pub use pi::PiDriver;
pub use proc::{Driver, DriverConfig, DriverState, TurnOutcome};
pub use spec::{BotSpec, Isolation};
pub use stream::{AssistantBlock, DriverEvent, Usage};
