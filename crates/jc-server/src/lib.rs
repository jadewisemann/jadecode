//! jadecode 서버: JSON-RPC 2.0 (WebSocket 전송), 일지 구독, `task.md` 감시.
//!
//! 구조(아키텍처 ② B2):
//! - 쓰기는 **단일 쓰기 스레드**가 맡는다(`Store`를 소유). seq 부여 → 일지 fsync → 색인 커밋 → 응답.
//! - 구독은 구독자별 버퍼를 두지 않고 색인에서 `from_seq` 뒤를 읽어 보낸다(느린 구독자가 서버 메모리를 키우지 않는다).
//! - `task.md` 감시는 "다시 확인하라는 신호"로만 쓴다. 읽기 알림은 무시한다.

pub mod rpc;
pub mod server;

pub use rpc::{error_codes, Method};
pub use server::{serve, serve_unix, Cmd, ServerConfig, Shared};
