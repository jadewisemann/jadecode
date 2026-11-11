//! 화면에 쓰는 작은 서식 도구. UI와 분리해 테스트한다.

use crate::board::{EventRow, Instance};

/// 단계 이름을 화면용으로(영문 키 → 한국어 라벨, 모르면 그대로).
pub fn stage_label(s: &str) -> String {
    match s {
        "refine" | "구체화" => "구체화".into(),
        "implement" | "구현" => "구현".into(),
        "verify" | "검증" => "검증".into(),
        "pr" | "PR" => "PR".into(),
        "report" | "보고" => "보고".into(),
        "done" | "완료" => "완료".into(),
        other => other.to_string(),
    }
}

/// 상태 배지(아이콘 + 색 키). 색은 UI가 정한다.
pub fn status_badge(s: &str) -> (&'static str, &'static str) {
    match s {
        "working" => ("●", "working"),
        "blocked" => ("■", "blocked"),
        "waiting_human" => ("◐", "waiting"),
        "done" => ("✓", "done"),
        "failed" => ("✕", "failed"),
        _ => ("○", "idle"),
    }
}

pub fn status_label(s: &str) -> String {
    match s {
        "working" => "작업 중".into(),
        "blocked" => "막힘".into(),
        "waiting_human" => "사람 대기".into(),
        "done" => "끝남".into(),
        "failed" => "실패".into(),
        "idle" => "대기".into(),
        other => other.to_string(),
    }
}

pub fn agent_label(a: &str) -> String {
    match a {
        "claude" => "Claude Code".into(),
        "opencode" => "OpenCode".into(),
        "omp" => "omp".into(),
        "pi" => "pi".into(),
        other => other.to_string(),
    }
}

/// 인스턴스 한 줄(상태·에이전트·턴).
pub fn instance_line(i: &Instance) -> String {
    format!(
        "{} {} · {}턴",
        agent_label(&i.agent),
        status_label(&i.state),
        i.turns
    )
}

/// 이벤트 한 줄(대화 목록).
pub fn event_line(e: &EventRow) -> (String, String) {
    let who = e.body["actor"]["instance"].as_str().unwrap_or("");
    let kind = match e.kind.as_str() {
        "msg.report_done" => "보고(완료)",
        "msg.report" => "보고",
        "msg.ask" => "질문",
        "msg.propose" => "제안",
        "msg.error" => "오류",
        "msg.escalation" => "사람에게 올림",
        "msg.reply" => "답",
        "msg.instruction" => "지시",
        "msg.ack" => "확인",
        "instance.output" => "출력",
        "instance.wake_digest" => "깨우기",
        other => other,
    }
    .to_string();
    let text = e.body["summary"]
        .as_str()
        .or_else(|| e.body["question"].as_str())
        .or_else(|| e.body["text"].as_str())
        .or_else(|| e.body["message"].as_str())
        .unwrap_or("")
        .to_string();
    let head = if who.is_empty() {
        kind
    } else {
        format!("{kind} · {who}")
    };
    (head, text)
}

/// 초 → 사람이 읽는 시간.
pub fn ago(now_ms: u64, ts_ms: u64) -> String {
    if ts_ms == 0 || now_ms <= ts_ms {
        return "방금".into();
    }
    let s = (now_ms - ts_ms) / 1000;
    match s {
        0..=9 => "방금".into(),
        10..=59 => format!("{s}초 전"),
        60..=3599 => format!("{}분 전", s / 60),
        3600..=86399 => format!("{}시간 전", s / 3600),
        _ => format!("{}일 전", s / 86400),
    }
}

/// 권한 요청 한 줄(무엇을 승인하는지).
pub fn permission_line(e: &EventRow) -> String {
    let tool = e.body["tool"].as_str().unwrap_or("");
    let input = &e.body["input"];
    let detail = input["command"]
        .as_str()
        .or_else(|| input["title"].as_str())
        .or_else(|| input["file_path"].as_str())
        .unwrap_or("");
    if detail.is_empty() {
        format!("#{} {tool}", e.seq)
    } else {
        format!("#{} {tool}: {detail}", e.seq)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn labels() {
        assert_eq!(stage_label("implement"), "구현");
        assert_eq!(stage_label("custom"), "custom");
        assert_eq!(status_label("waiting_human"), "사람 대기");
        assert_eq!(agent_label("opencode"), "OpenCode");
        assert_eq!(status_badge("blocked").0, "■");
    }

    #[test]
    fn instance_line_shape() {
        let i = Instance {
            id: "i1".into(),
            role: "worker".into(),
            agent: "omp".into(),
            state: "working".into(),
            bot: "worker".into(),
            card: Some("work/p/c".into()),
            project: "p".into(),
            turns: 2,
        };
        assert_eq!(instance_line(&i), "omp 작업 중 · 2턴");
    }

    #[test]
    fn event_lines() {
        let e = EventRow {
            seq: 5,
            ts: 0,
            topic: "t".into(),
            kind: "msg.report_done".into(),
            body: json!({"summary":"끝났다","actor":{"instance":"w-1"}}),
        };
        let (head, text) = event_line(&e);
        assert_eq!(head, "보고(완료) · w-1");
        assert_eq!(text, "끝났다");
    }

    #[test]
    fn permission_text() {
        let e = EventRow {
            seq: 42,
            ts: 0,
            topic: "t".into(),
            kind: "permission.requested".into(),
            body: json!({"tool":"Bash","input":{"command":"chmod +x du.sh"}}),
        };
        assert_eq!(permission_line(&e), "#42 Bash: chmod +x du.sh");
    }

    #[test]
    fn times() {
        assert_eq!(ago(1000, 1000), "방금");
        assert_eq!(ago(30_000, 1000), "29초 전");
        assert_eq!(ago(120_000, 1000), "1분 전");
        assert_eq!(ago(7_200_000, 1000), "1시간 전");
        assert_eq!(ago(200_000_000, 1000), "2일 전");
    }
}
