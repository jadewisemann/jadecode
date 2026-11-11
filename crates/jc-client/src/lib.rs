//! jadecode 데스크톱 클라이언트(GPUI).
//!
//! 구조: `board`(데이터) + `ui`(화면). 데이터 계층은 서버와 WebSocket JSON-RPC로 말하고,
//! UI는 그 데이터를 그린다. UI를 못 띄우는 환경(헤드리스)에서도 데이터 계층은 테스트할 수 있다.

pub mod board;
pub mod format;
pub mod ui;

pub use board::{Board, BoardSnapshot, Card, Connection};
