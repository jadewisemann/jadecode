//! jadecode 코어: 워크스페이스 레이아웃, 이벤트 일지, 재생성 가능한 색인, `task.md` 정본.
//!
//! 정본은 파일이다(아키텍처 ② B2).
//! - 카드 폴더 `task.md`(앞머리 YAML) + `events.jsonl`(덧붙이기만)
//! - 프로젝트 폴더 `project.toml` + `events.jsonl`
//! - 워크스페이스 `events/global.jsonl`
//! 색인(`.jadecode/index.sqlite`)은 언제든 지우고 일지에서 다시 만들 수 있다.

pub mod card;
pub mod scaffold;
pub mod event;
pub mod index;
pub mod store;
pub mod taskmd;
pub mod topic;
pub mod ulid;
pub mod workspace;

pub use card::NewCard;
pub use event::NewEvent;
pub use store::{Fsync, RecoveryStats, Store};
pub use taskmd::CardState;
pub use topic::{journal_for_topic, topic_matches};
pub use workspace::{short_temp_root, socket_path_len_ok, Workspace};
