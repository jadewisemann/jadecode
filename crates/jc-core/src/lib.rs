//! jadecode 코어.
//!
//! 구조는 세 가지뿐이다.
//!
//! - **프로젝트**: 워크스페이스 아래 폴더 하나(`<ws>/<slug>/project.toml`). 코드 레포 하나와 봇 대화 하나를 갖는다.
//! - **칸반**: 프로젝트 폴더 안의 `todo/` `doing/` `review/` `done/` 폴더가 열이고, 그 안의 `.md` 파일 하나가 항목(메모)이다.
//!   메모 내용이 곧 그 작업의 맥락이다. 열을 옮기는 것은 파일을 옮기는 것이다.
//! - **실행**: 봇 대화와 메모 작업은 Claude Code(`claude -p`)를 한 턴씩 띄워 돌린다. 세션 id와 기록은
//!   `<ws>/.jadecode/` 아래에 따로 두어서 메모 파일을 깨끗하게 유지한다.

pub mod agent;
pub mod board;
pub mod engine;
pub mod log;
pub mod markdown;
pub mod update;
pub mod version;
pub mod workspace;
pub mod worktree;

pub use board::{Column, Memo};
pub use engine::{Engine, Key};
pub use workspace::{Project, Workspace};

/// 지금 시각(유닉스 밀리초).
pub fn now_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}

/// 무작위 UUID v4(Claude Code 세션 id는 UUID여야 한다).
pub fn uuid4() -> String {
    let mut b = [0u8; 16];
    fill_random(&mut b);
    b[6] = (b[6] & 0x0f) | 0x40;
    b[8] = (b[8] & 0x3f) | 0x80;
    let h: String = b.iter().map(|x| format!("{x:02x}")).collect();
    format!(
        "{}-{}-{}-{}-{}",
        &h[0..8],
        &h[8..12],
        &h[12..16],
        &h[16..20],
        &h[20..32]
    )
}

fn fill_random(buf: &mut [u8]) {
    use std::io::Read;
    if let Ok(mut f) = std::fs::File::open("/dev/urandom") {
        if f.read_exact(buf).is_ok() {
            return;
        }
    }
    // /dev/urandom이 없으면 시각과 주소로 섞는다(충돌만 피하면 된다).
    let mut x = now_ms() ^ (buf.as_ptr() as u64) ^ (std::process::id() as u64) << 32;
    for b in buf.iter_mut() {
        x ^= x << 13;
        x ^= x >> 7;
        x ^= x << 17;
        *b = x as u8;
    }
}

/// 임시 파일에 쓰고 이름을 바꿔서, 읽는 쪽이 반쯤 쓰인 파일을 보지 않게 한다.
pub fn write_atomic(path: &std::path::Path, data: &[u8]) -> std::io::Result<()> {
    if let Some(d) = path.parent() {
        std::fs::create_dir_all(d)?;
    }
    let tmp = path.with_extension(format!("tmp-{}", std::process::id()));
    std::fs::write(&tmp, data)?;
    std::fs::rename(&tmp, path)
}

#[cfg(test)]
mod tests {
    #[test]
    fn uuid_shape() {
        let u = super::uuid4();
        assert_eq!(u.len(), 36);
        assert_eq!(&u[14..15], "4");
        assert_ne!(u, super::uuid4());
    }
}
