//! 대화 기록(jsonl). 봇 대화와 메모 작업마다 파일 하나.

use serde::{Deserialize, Serialize};
use std::io::Write;
use std::path::Path;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Kind {
    /// 사람이 보낸 말.
    User,
    /// 에이전트의 글.
    Assistant,
    /// 에이전트의 도구 호출 한 줄 요약.
    Tool,
    /// 앱이 남기는 알림(시작, 보드 소식 등).
    Note,
    /// 실패.
    Error,
    /// 턴 끝(걸린 시간·비용).
    Turn,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Entry {
    pub ts: u64,
    pub kind: Kind,
    pub text: String,
}

impl Entry {
    pub fn new(kind: Kind, text: impl Into<String>) -> Self {
        Self {
            ts: crate::now_ms(),
            kind,
            text: text.into(),
        }
    }
}

pub fn append(path: &Path, e: &Entry) -> std::io::Result<()> {
    if let Some(d) = path.parent() {
        std::fs::create_dir_all(d)?;
    }
    let mut f = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(path)?;
    let mut line = serde_json::to_string(e).map_err(std::io::Error::other)?;
    line.push('\n');
    f.write_all(line.as_bytes())
}

/// 전부 읽는다. 깨진 줄(앱이 쓰다 죽은 끝 줄 등)은 건너뛴다.
pub fn read(path: &Path) -> Vec<Entry> {
    let Ok(text) = std::fs::read_to_string(path) else {
        return vec![];
    };
    text.lines()
        .filter(|l| !l.trim().is_empty())
        .filter_map(|l| serde_json::from_str(l).ok())
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn roundtrip_skips_torn_line() {
        let d = crate::board::tests::tmpdir("log");
        let p = d.join("a/log.jsonl");
        append(&p, &Entry::new(Kind::User, "안녕")).unwrap();
        append(&p, &Entry::new(Kind::Assistant, "네")).unwrap();
        std::fs::OpenOptions::new()
            .append(true)
            .open(&p)
            .unwrap()
            .write_all(b"{\"ts\":1,\"ki")
            .unwrap();
        let v = read(&p);
        assert_eq!(v.len(), 2);
        assert_eq!(v[0].kind, Kind::User);
        assert_eq!(v[1].text, "네");
        let _ = std::fs::remove_dir_all(&d);
    }
}
