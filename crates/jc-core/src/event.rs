//! 일지 한 줄과 새 이벤트.

use serde::{Deserialize, Serialize};

/// 일지에 쓰는 한 줄(`events.jsonl`).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Event {
    pub seq: u64,
    pub ts: u64,
    pub topic: String,
    pub kind: String,
    pub body: serde_json::Value,
}

#[derive(Clone, Debug)]
pub struct NewEvent {
    pub topic: String,
    pub kind: String,
    pub body: serde_json::Value,
}

impl NewEvent {
    pub fn new(topic: impl Into<String>, kind: impl Into<String>, body: serde_json::Value) -> Self {
        Self {
            topic: topic.into(),
            kind: kind.into(),
            body,
        }
    }
}

#[derive(Deserialize)]
struct LineHead<'a> {
    seq: u64,
    #[serde(borrow)]
    topic: std::borrow::Cow<'a, str>,
    #[serde(borrow)]
    kind: std::borrow::Cow<'a, str>,
}

/// 줄 머리만 빠르게 읽는다(색인 채우기·복구에서 본문 파싱을 미룬다).
pub fn parse_line_head(line: &str) -> anyhow::Result<(u64, String, String)> {
    let h: LineHead = serde_json::from_str(line)
        .map_err(|e| anyhow::anyhow!("bad journal line ({}): {e}", &line[..line.len().min(120)]))?;
    Ok((h.seq, h.topic.into_owned(), h.kind.into_owned()))
}

pub fn now_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}
