//! 주제(topic) — 구독 단위이자 일지 선택자.
//!
//! 주제는 경로다: `project/<p>`, `project/<p>/card/<c>`, `instance/<i>`, `bot/<b>`, `global`.
//! 구독은 **마디 단위 앞부분 일치**다. `project/p1`은 `project/p1/card/...`에 맞고 `project/p10`에는 맞지 않는다.

use anyhow::{bail, Result};

/// 주제 → 일지 상대 경로.
///
/// - `project/<p>/card/<c>/...` → `work/<p>/<c>/events.jsonl`
/// - `project/<p>/...` → `work/<p>/events.jsonl`
/// - 그 밖(`global`, `bot/...`, `instance/...`) → `events/global.jsonl`
pub fn journal_for_topic(topic: &str) -> Result<String> {
    let seg: Vec<&str> = topic.split('/').collect();
    for s in &seg {
        if s.is_empty() || *s == "." || *s == ".." || s.contains('\\') {
            bail!("bad topic {topic:?}");
        }
    }
    Ok(match seg.as_slice() {
        ["project", p, "card", c, ..] => format!("work/{p}/{c}/events.jsonl"),
        ["project", p, ..] => format!("work/{p}/events.jsonl"),
        _ => "events/global.jsonl".to_string(),
    })
}

/// 마디 단위 앞부분 일치. 빈 접두사는 모두에 맞는다.
pub fn topic_matches(prefix: &str, topic: &str) -> bool {
    let base = prefix.trim_end_matches('/');
    if base.is_empty() {
        return true;
    }
    topic == base || (topic.starts_with(base) && topic.as_bytes().get(base.len()) == Some(&b'/'))
}

/// `work/<p>/<c>` → `project/<p>/card/<c>`
pub fn card_topic(card_rel: &str) -> String {
    let s: Vec<&str> = card_rel.split('/').collect();
    if s.len() < 3 || s[0] != "work" {
        return format!("card/{card_rel}");
    }
    format!("project/{}/card/{}", s[1], s[2])
}

/// `work/<p>` → `project/<p>`
pub fn project_topic(project_rel: &str) -> String {
    let s: Vec<&str> = project_rel.split('/').collect();
    match s.as_slice() {
        ["work", p] => format!("project/{p}"),
        _ => format!("project/{project_rel}"),
    }
}

/// SQL에서 쓸 `(조건, 인자)`. `topic` 컬럼과 비교한다.
/// `first_index`는 이 조건이 쓰기 시작할 물음표 번호다(앞의 조건이 `?1`.. 을 이미 썼으면 그다음부터).
pub fn topic_sql(topics: &[String], first_index: usize) -> (String, Vec<String>) {
    if topics.is_empty() {
        return ("1".into(), vec![]);
    }
    let mut parts = vec![];
    let mut args: Vec<String> = vec![];
    for t in topics {
        let base = t.trim_end_matches('/').to_string();
        if base.is_empty() {
            return ("1".into(), vec![]);
        }
        // GLOB 메타문자 이스케이프: [x] 로 감싼다.
        let esc: String = base
            .chars()
            .map(|c| {
                if "*?[".contains(c) {
                    format!("[{c}]")
                } else {
                    c.to_string()
                }
            })
            .collect();
        let i = first_index + args.len();
        parts.push(format!("(topic = ?{i} OR topic GLOB ?{})", i + 1));
        args.push(base);
        args.push(format!("{esc}/*"));
    }
    (parts.join(" OR "), args)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn journals() {
        assert_eq!(
            journal_for_topic("project/p1/card/01ABC/instance/i1").unwrap(),
            "work/p1/01ABC/events.jsonl"
        );
        assert_eq!(journal_for_topic("project/p1").unwrap(), "work/p1/events.jsonl");
        assert_eq!(journal_for_topic("global").unwrap(), "events/global.jsonl");
        assert_eq!(journal_for_topic("instance/i1").unwrap(), "events/global.jsonl");
        assert!(journal_for_topic("project/../x").is_err());
        assert!(journal_for_topic("project//card/c").is_err());
    }

    #[test]
    fn matching_is_segment_wise() {
        assert!(topic_matches("project/p1", "project/p1"));
        assert!(topic_matches("project/p1", "project/p1/card/c1"));
        assert!(!topic_matches("project/p1", "project/p10"));
        assert!(!topic_matches("project/p1", "project/p1x/card/c1"));
        assert!(topic_matches("project/p1/", "project/p1/card/c1"));
        assert!(topic_matches("", "anything"));
    }

    #[test]
    fn sql_uses_glob_and_escapes() {
        let (cond, args) = topic_sql(&["project/p1".into()], 1);
        assert!(cond.contains("topic GLOB"), "{cond}");
        assert_eq!(args, vec!["project/p1".to_string(), "project/p1/*".to_string()]);
        let (_c, args) = topic_sql(&["weird*name".into()], 1);
        assert_eq!(args[1], "weird[*]name/*");
    }

    #[test]
    fn rel_topics() {
        assert_eq!(card_topic("work/p1/01CARD"), "project/p1/card/01CARD");
        assert_eq!(project_topic("work/p1"), "project/p1");
    }
}
