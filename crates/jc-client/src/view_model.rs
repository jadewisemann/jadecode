//! 화면이 그리기 전에 데이터를 고르고 묶는 순수 함수들.
//!
//! GPUI 없이 테스트할 수 있도록 화면(`ui`)과 나눴다. 화면은 여기서 나온 값을 그리기만 한다.

use crate::board::{BoardSnapshot, Card, EventRow};
use std::path::Path;

/// 기본 단계 순서. 프로젝트가 다른 단계를 쓰면 그 뒤에 등장 순서대로 붙는다.
pub const DEFAULT_STAGES: [&str; 5] = ["refine", "implement", "verify", "pr", "report"];

/// 카드가 사람에게 얼마나 급한가(목록 묶음 순서).
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Attention {
    /// 승인·답·확인이 필요하다.
    NeedsYou,
    /// 에이전트가 일하는 중.
    Running,
    /// 아무도 일하지 않는다.
    Idle,
    /// 끝남.
    Done,
}

impl Attention {
    pub fn label(self) -> &'static str {
        match self {
            Attention::NeedsYou => "확인 필요",
            Attention::Running => "진행 중",
            Attention::Idle => "대기",
            Attention::Done => "끝남",
        }
    }
}

/// `work/<p>/<c>` → 이벤트 주제 `project/<p>/card/<c>`.
pub fn card_topic(card_path: &str) -> String {
    jc_core::topic::card_topic(card_path)
}

fn topic_is_card(topic: &str, card_path: &str) -> bool {
    jc_core::topic::topic_matches(&card_topic(card_path), topic)
}

/// 카드에 걸린 미해결 권한 요청 수.
pub fn pending_permissions_of(s: &BoardSnapshot, card_path: &str) -> usize {
    s.pending_permissions
        .iter()
        .filter(|p| topic_is_card(&p.topic, card_path))
        .count()
}

pub fn attention(s: &BoardSnapshot, c: &Card) -> Attention {
    let q: Vec<String> = open_questions(s).into_iter().map(|e| e.topic).collect();
    attention_with(s, c, &q)
}

fn attention_with(s: &BoardSnapshot, c: &Card, question_topics: &[String]) -> Attention {
    if c.status == "done" {
        return Attention::Done;
    }
    if matches!(c.status.as_str(), "blocked" | "waiting_human" | "failed")
        || pending_permissions_of(s, &c.path) > 0
        || question_topics.iter().any(|t| topic_is_card(t, &c.path))
    {
        return Attention::NeedsYou;
    }
    if c.status == "working" || !s.instances_of(&c.path).is_empty() {
        return Attention::Running;
    }
    Attention::Idle
}

/// 모든 카드의 급한 정도(카드 경로 → 묶음). 질문 목록을 한 번만 계산한다.
pub fn attention_map(s: &BoardSnapshot) -> std::collections::HashMap<String, Attention> {
    let q: Vec<String> = open_questions(s).into_iter().map(|e| e.topic).collect();
    s.cards
        .iter()
        .map(|c| (c.path.clone(), attention_with(s, c, &q)))
        .collect()
}

/// 목록 보기: 급한 순서로 묶은 카드.
pub fn group_cards(s: &BoardSnapshot, project: Option<&str>) -> Vec<(Attention, Vec<Card>)> {
    let mut groups: Vec<(Attention, Vec<Card>)> = vec![
        (Attention::NeedsYou, vec![]),
        (Attention::Running, vec![]),
        (Attention::Idle, vec![]),
        (Attention::Done, vec![]),
    ];
    let map = attention_map(s);
    for c in &s.cards {
        if let Some(p) = project {
            if c.project != p {
                continue;
            }
        }
        let a = map.get(&c.path).copied().unwrap_or(Attention::Idle);
        if let Some(g) = groups.iter_mut().find(|g| g.0 == a) {
            g.1.push(c.clone());
        }
    }
    for g in &mut groups {
        // 최근 카드가 위(ULID는 시간 순이다).
        g.1.sort_by(|a, b| b.id.cmp(&a.id));
    }
    groups.retain(|g| !g.1.is_empty());
    groups
}

/// 칸반 열: 기본 단계 순서를 먼저, 그 밖의 단계는 등장 순서대로.
pub fn ordered_stages(s: &BoardSnapshot, project: Option<&str>) -> Vec<String> {
    let seen = s.stages(project);
    let mut out: Vec<String> = DEFAULT_STAGES
        .iter()
        .map(|x| x.to_string())
        .filter(|x| seen.contains(x))
        .collect();
    for st in seen {
        if !out.contains(&st) {
            out.push(st);
        }
    }
    if out.is_empty() {
        out = DEFAULT_STAGES.iter().map(|x| x.to_string()).collect();
    }
    out
}

/// 사람의 답을 기다리는 질문·올림·오류(뒤에 사람 말이나 답이 없는 것).
pub fn open_questions(s: &BoardSnapshot) -> Vec<EventRow> {
    let mut out: Vec<EventRow> = vec![];
    let mut all: Vec<&EventRow> = s.messages.iter().chain(s.recent.iter()).collect();
    all.sort_by_key(|e| e.seq);
    all.dedup_by_key(|e| e.seq);
    for e in &all {
        match e.kind.as_str() {
            "msg.ask" | "msg.escalation" | "msg.error" | "msg.propose" => {
                out.push((*e).clone());
            }
            "msg.human" | "msg.reply" | "msg.instruction" => {
                out.retain(|q| q.topic != e.topic);
            }
            // 카드가 끝났으면 질문도 닫힌 것으로 본다.
            "card.status_changed" if e.body["to"].as_str() == Some("done") => {
                out.retain(|q| q.topic != e.topic);
            }
            _ => {}
        }
    }
    out
}

/// 받은편지함 한 줄.
#[derive(Debug, Clone, PartialEq)]
pub enum InboxItem {
    Permission(EventRow),
    Question(EventRow),
}

impl InboxItem {
    pub fn event(&self) -> &EventRow {
        match self {
            InboxItem::Permission(e) | InboxItem::Question(e) => e,
        }
    }
}

/// 받은편지함: 권한 승인 먼저, 그다음 질문(최근 것이 위).
pub fn inbox(s: &BoardSnapshot) -> Vec<InboxItem> {
    let mut v: Vec<InboxItem> = s
        .pending_permissions
        .iter()
        .cloned()
        .map(InboxItem::Permission)
        .collect();
    let mut q = open_questions(s);
    q.reverse();
    v.extend(q.into_iter().map(InboxItem::Question));
    v
}

/// 주제의 카드 경로(`project/p/card/c/...` → `work/p/c`). 카드 주제가 아니면 `None`.
pub fn card_of_topic(topic: &str) -> Option<String> {
    let seg: Vec<&str> = topic.split('/').collect();
    match seg.as_slice() {
        ["project", p, "card", c, ..] => Some(format!("work/{p}/{c}")),
        _ => None,
    }
}

/// 스레드에 보이는 이벤트 종류인가.
pub fn is_thread_event(kind: &str) -> bool {
    kind.starts_with("msg.")
        || matches!(
            kind,
            "instance.output"
                | "instance.tool_use"
                | "instance.turn_end"
                | "instance.turn_error"
                | "instance.started"
                | "instance.exited"
                | "instance.failed"
                | "instance.restarted"
                | "card.created"
                | "card.stage_changed"
                | "card.status_changed"
                | "permission.requested"
                | "permission.resolved"
                | "instance.file_written"
        )
}

/// 이력(서버에서 받은 것) + 그 뒤 실시간 이벤트를 합친 스레드.
/// `exact`이면 주제가 정확히 같은 것만(프로젝트 대화는 하위 카드 주제를 빼야 한다).
pub fn thread(
    history: &[EventRow],
    recent: &[EventRow],
    topic: &str,
    exact: bool,
) -> Vec<EventRow> {
    let matches = |e: &EventRow| {
        if exact {
            e.topic == topic
        } else {
            jc_core::topic::topic_matches(topic, &e.topic)
        }
    };
    let last = history.iter().map(|e| e.seq).max().unwrap_or(0);
    let mut out: Vec<EventRow> = history
        .iter()
        .filter(|e| matches(e) && is_thread_event(&e.kind))
        .filter(|e| !(e.kind == "msg.report" && e.body["auto"].as_bool() == Some(true)))
        .cloned()
        .collect();
    for e in recent {
        if e.seq > last
            && matches(e)
            && is_thread_event(&e.kind)
            && !(e.kind == "msg.report" && e.body["auto"].as_bool() == Some(true))
        {
            out.push(e.clone());
        }
    }
    out.sort_by_key(|e| e.seq);
    out.dedup_by_key(|e| e.seq);
    out
}

/// 스레드 한 칸의 모양.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Bubble {
    /// 사람이 보낸 말(오른쪽).
    Human,
    /// 에이전트 본문.
    Agent,
    /// 도구 호출 한 줄.
    Tool,
    /// 보고·질문·제안·오류 같은 구조화 메시지.
    Notice,
    /// 상태 변화 같은 시스템 줄.
    System,
}

#[derive(Debug, Clone, PartialEq)]
pub struct ThreadItem {
    pub seq: u64,
    pub bubble: Bubble,
    /// 머리글(누가·무엇).
    pub head: String,
    pub text: String,
    /// 색 키: "ok" "warn" "err" "info" "muted".
    pub tone: &'static str,
}

fn short(s: &str, n: usize) -> String {
    let one: String = s.split_whitespace().collect::<Vec<_>>().join(" ");
    if one.chars().count() <= n {
        one
    } else {
        let mut t: String = one.chars().take(n).collect();
        t.push('…');
        t
    }
}

/// 도구 호출 입력에서 사람이 알아볼 한 줄을 뽑는다.
pub fn tool_summary(tool: &str, input: &serde_json::Value) -> String {
    let detail = input["command"]
        .as_str()
        .or_else(|| input["file_path"].as_str())
        .or_else(|| input["path"].as_str())
        .or_else(|| input["pattern"].as_str())
        .or_else(|| input["url"].as_str())
        .or_else(|| input["summary"].as_str())
        .or_else(|| input["question"].as_str())
        .or_else(|| input["text"].as_str())
        .unwrap_or("");
    let name = tool
        .strip_prefix("mcp__jadecode__")
        .or_else(|| tool.strip_prefix("jadecode_"))
        .unwrap_or(tool);
    if detail.is_empty() {
        name.to_string()
    } else {
        format!("{name} · {}", short(detail, 140))
    }
}

pub fn thread_item(e: &EventRow) -> ThreadItem {
    let b = &e.body;
    let txt = |k: &str| b[k].as_str().unwrap_or("").to_string();
    let who = b["actor"]["instance"]
        .as_str()
        .or_else(|| b["instance"].as_str())
        .unwrap_or("")
        .to_string();
    let (bubble, head, text, tone) = match e.kind.as_str() {
        "msg.human" => (Bubble::Human, "나".to_string(), txt("text"), "info"),
        "instance.output" => (Bubble::Agent, who.clone(), txt("text"), "muted"),
        "instance.tool_use" => (
            Bubble::Tool,
            String::new(),
            tool_summary(b["tool"].as_str().unwrap_or(""), &b["input"]),
            "muted",
        ),
        "instance.file_written" => (
            Bubble::Tool,
            String::new(),
            format!("파일 씀 · {}", b["path"].as_str().unwrap_or("")),
            "muted",
        ),
        "instance.turn_end" => {
            let u = &b["usage"];
            let tokens = u["input"].as_u64().unwrap_or(0) + u["output"].as_u64().unwrap_or(0);
            let cost = u["cost_usd"].as_f64().unwrap_or(0.0);
            let mut t = format!("턴 {} 끝", b["turn"].as_u64().unwrap_or(0));
            if tokens > 0 {
                t.push_str(&format!(" · {tokens} 토큰"));
            }
            if cost > 0.0 {
                t.push_str(&format!(" · ${cost:.3}"));
            }
            let tone = if b["is_error"].as_bool() == Some(true) {
                "err"
            } else {
                "muted"
            };
            (Bubble::System, String::new(), t, tone)
        }
        "instance.turn_error" => (
            Bubble::System,
            String::new(),
            format!("턴 오류: {}", txt("error")),
            "err",
        ),
        "instance.started" => (
            Bubble::System,
            String::new(),
            format!(
                "{} 시작 · {}",
                b["bot"].as_str().unwrap_or("봇"),
                crate::format::agent_label(b["agent"].as_str().unwrap_or("claude"))
            ),
            "info",
        ),
        "instance.exited" => (
            Bubble::System,
            String::new(),
            "인스턴스 끝남".into(),
            "muted",
        ),
        "instance.failed" => (
            Bubble::System,
            String::new(),
            format!("인스턴스 실패: {}", txt("error")),
            "err",
        ),
        "instance.restarted" => (Bubble::System, String::new(), "다시 띄움".into(), "warn"),
        "card.created" => (
            Bubble::System,
            String::new(),
            format!("카드 생성 · {}", txt("title")),
            "muted",
        ),
        "card.stage_changed" => (
            Bubble::System,
            String::new(),
            format!(
                "단계 {} → {}",
                crate::format::stage_label(b["from"].as_str().unwrap_or("")),
                crate::format::stage_label(b["to"].as_str().unwrap_or(""))
            ),
            "info",
        ),
        "card.status_changed" => (
            Bubble::System,
            String::new(),
            format!(
                "상태 → {}",
                crate::format::status_label(b["to"].as_str().unwrap_or(""))
            ),
            "muted",
        ),
        "permission.requested" => (
            Bubble::Notice,
            "권한 요청".to_string(),
            crate::format::permission_line(e),
            "warn",
        ),
        "permission.resolved" => (
            Bubble::System,
            String::new(),
            format!(
                "권한 #{} {}",
                b["request_seq"].as_u64().unwrap_or(0),
                if b["allow"].as_bool() == Some(true) {
                    "승인됨"
                } else {
                    "거절됨"
                }
            ),
            "muted",
        ),
        k => {
            let (head, text) = crate::format::event_line(e);
            let tone = match k {
                "msg.ask" | "msg.propose" | "msg.escalation" => "warn",
                "msg.error" => "err",
                "msg.report_done" => "ok",
                _ => "info",
            };
            (Bubble::Notice, head, text, tone)
        }
    };
    ThreadItem {
        seq: e.seq,
        bubble,
        head,
        text,
        tone,
    }
}

/// 마크다운 블록(화면이 쓰는 최소 집합).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum MdBlock {
    Heading(u8, String),
    Para(String),
    Bullet(u8, String),
    Check(bool, String),
    Code(Vec<String>),
    Quote(String),
    Rule,
}

/// 앞머리(`---` … `---`)를 떼고 블록으로 나눈다.
pub fn markdown_blocks(md: &str) -> Vec<MdBlock> {
    let mut lines = md.lines().peekable();
    // 앞머리
    if md.starts_with("---") {
        lines.next();
        for l in lines.by_ref() {
            if l.trim() == "---" {
                break;
            }
        }
    }
    let mut out: Vec<MdBlock> = vec![];
    let mut para: Vec<String> = vec![];
    let flush = |para: &mut Vec<String>, out: &mut Vec<MdBlock>| {
        if !para.is_empty() {
            out.push(MdBlock::Para(para.join(" ")));
            para.clear();
        }
    };
    while let Some(l) = lines.next() {
        let t = l.trim_end();
        let trimmed = t.trim_start();
        if trimmed.starts_with("```") {
            flush(&mut para, &mut out);
            let mut code = vec![];
            for c in lines.by_ref() {
                if c.trim_start().starts_with("```") {
                    break;
                }
                code.push(c.to_string());
            }
            out.push(MdBlock::Code(code));
            continue;
        }
        if trimmed.is_empty() {
            flush(&mut para, &mut out);
            continue;
        }
        if let Some(h) = heading(trimmed) {
            flush(&mut para, &mut out);
            out.push(h);
            continue;
        }
        if trimmed == "---" || trimmed == "***" {
            flush(&mut para, &mut out);
            out.push(MdBlock::Rule);
            continue;
        }
        let indent = ((t.len() - trimmed.len()) / 2).min(4) as u8;
        if let Some(rest) = trimmed
            .strip_prefix("- ")
            .or_else(|| trimmed.strip_prefix("* "))
            .or_else(|| trimmed.strip_prefix("+ "))
        {
            flush(&mut para, &mut out);
            if let Some(x) = rest.strip_prefix("[ ] ") {
                out.push(MdBlock::Check(false, x.to_string()));
            } else if let Some(x) = rest
                .strip_prefix("[x] ")
                .or_else(|| rest.strip_prefix("[X] "))
            {
                out.push(MdBlock::Check(true, x.to_string()));
            } else {
                out.push(MdBlock::Bullet(indent, rest.to_string()));
            }
            continue;
        }
        if let Some(pos) = trimmed.find(". ") {
            if pos > 0 && pos <= 3 && trimmed[..pos].chars().all(|c| c.is_ascii_digit()) {
                flush(&mut para, &mut out);
                out.push(MdBlock::Bullet(indent, trimmed.to_string()));
                continue;
            }
        }
        if let Some(q) = trimmed.strip_prefix("> ") {
            flush(&mut para, &mut out);
            out.push(MdBlock::Quote(q.to_string()));
            continue;
        }
        para.push(trimmed.to_string());
    }
    flush(&mut para, &mut out);
    out
}

fn heading(l: &str) -> Option<MdBlock> {
    let n = l.chars().take_while(|c| *c == '#').count();
    if (1..=6).contains(&n) && l[n..].starts_with(' ') {
        Some(MdBlock::Heading(n as u8, l[n + 1..].trim().to_string()))
    } else {
        None
    }
}

/// 인라인 서식(`**굵게**`, `` `코드` ``)을 화면에서 지운다. 글자는 그대로 둔다.
pub fn strip_inline(s: &str) -> String {
    s.replace("**", "").replace("__", "")
}

/// diff 줄 종류.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DiffKind {
    File,
    Hunk,
    Add,
    Del,
    Context,
    Meta,
}

pub fn diff_lines(text: &str) -> Vec<(DiffKind, String)> {
    text.lines()
        .map(|l| {
            let k = if l.starts_with("diff --git") || l.starts_with("## ") {
                DiffKind::File
            } else if l.starts_with("@@") {
                DiffKind::Hunk
            } else if l.starts_with("+++") || l.starts_with("---") || l.starts_with("index ") {
                DiffKind::Meta
            } else if l.starts_with('+') {
                DiffKind::Add
            } else if l.starts_with('-') {
                DiffKind::Del
            } else if l.starts_with("new file")
                || l.starts_with("deleted file")
                || l.starts_with("?? ")
            {
                DiffKind::Meta
            } else {
                DiffKind::Context
            };
            (k, l.to_string())
        })
        .collect()
}

/// diff 요약: 바뀐 파일 수·더한 줄·뺀 줄.
pub fn diff_stat(lines: &[(DiffKind, String)]) -> (usize, usize, usize) {
    let files = lines
        .iter()
        .filter(|(k, l)| *k == DiffKind::File && l.starts_with("diff --git"))
        .count();
    let add = lines.iter().filter(|(k, _)| *k == DiffKind::Add).count();
    let del = lines.iter().filter(|(k, _)| *k == DiffKind::Del).count();
    (files, add, del)
}

fn git(dir: &Path, args: &[&str]) -> Option<String> {
    let out = std::process::Command::new("git")
        .arg("-C")
        .arg(dir)
        .args(args)
        .output()
        .ok()?;
    if !out.status.success() {
        return None;
    }
    Some(String::from_utf8_lossy(&out.stdout).to_string())
}

/// 카드 폴더 안의 git 저장소(워크트리)를 찾아 바뀐 내용을 모은다.
/// 카드 폴더 자체가 저장소 안에 있으면 그 폴더 범위만 본다.
pub fn collect_diff(card_dir: &Path, max_lines: usize) -> String {
    let mut repos: Vec<std::path::PathBuf> = vec![];
    if let Ok(rd) = std::fs::read_dir(card_dir) {
        let mut entries: Vec<_> = rd.filter_map(|e| e.ok()).map(|e| e.path()).collect();
        entries.sort();
        for p in entries {
            if p.is_dir() && p.join(".git").exists() {
                repos.push(p);
            }
        }
    }
    let mut out = String::new();
    let mut push_repo = |label: &str, dir: &Path, scope: Option<&str>| {
        let mut args_status = vec!["status", "--porcelain"];
        let mut args_diff = vec!["diff", "HEAD", "--no-color"];
        if let Some(s) = scope {
            args_status.extend(["--", s]);
            args_diff.extend(["--", s]);
        }
        let status = git(dir, &args_status).unwrap_or_default();
        let diff = git(dir, &args_diff)
            .or_else(|| git(dir, &["diff", "--no-color"]))
            .unwrap_or_default();
        if status.trim().is_empty() && diff.trim().is_empty() {
            return;
        }
        out.push_str(&format!("## {label}\n"));
        for l in status.lines().filter(|l| l.starts_with("??")) {
            out.push_str(l);
            out.push('\n');
        }
        out.push_str(&diff);
        if !out.ends_with('\n') {
            out.push('\n');
        }
    };
    for r in &repos {
        let label = r
            .file_name()
            .map(|n| n.to_string_lossy().to_string())
            .unwrap_or_default();
        push_repo(&label, r, None);
    }
    if repos.is_empty() && git(card_dir, &["rev-parse", "--is-inside-work-tree"]).is_some() {
        push_repo("카드 폴더", card_dir, Some("."));
    }
    let lines: Vec<&str> = out.lines().collect();
    if lines.len() > max_lines {
        let mut t = lines[..max_lines].join("\n");
        t.push_str(&format!(
            "\n… {}줄 더 있음 (터미널에서 git diff로 보세요)\n",
            lines.len() - max_lines
        ));
        return t;
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::board::Instance;
    use serde_json::json;

    fn ev(seq: u64, topic: &str, kind: &str, body: serde_json::Value) -> EventRow {
        EventRow {
            seq,
            ts: 0,
            topic: topic.into(),
            kind: kind.into(),
            body,
        }
    }

    fn card(path: &str, status: &str, stage: &str) -> Card {
        Card {
            path: path.into(),
            id: path.into(),
            title: path.into(),
            project: path.split('/').nth(1).unwrap_or("").into(),
            stage: stage.into(),
            status: status.into(),
        }
    }

    #[test]
    fn groups_by_attention() {
        let mut s = BoardSnapshot::default();
        s.cards.push(card("work/a/1", "idle", "refine"));
        s.cards.push(card("work/a/2", "blocked", "implement"));
        s.cards.push(card("work/a/3", "done", "report"));
        s.cards.push(card("work/a/4", "idle", "implement"));
        s.cards.push(card("work/b/5", "idle", "implement"));
        s.instances.push(Instance {
            id: "w1".into(),
            card: Some("work/a/4".into()),
            ..Default::default()
        });
        s.pending_permissions
            .push(ev(9, "project/a/card/1", "permission.requested", json!({})));
        let g = group_cards(&s, Some("a"));
        let labels: Vec<_> = g.iter().map(|x| x.0).collect();
        assert_eq!(
            labels,
            vec![Attention::NeedsYou, Attention::Running, Attention::Done]
        );
        assert_eq!(g[0].1.len(), 2);
        assert_eq!(
            group_cards(&s, None)
                .iter()
                .map(|x| x.1.len())
                .sum::<usize>(),
            5
        );
    }

    #[test]
    fn stages_keep_default_order() {
        let mut s = BoardSnapshot::default();
        s.cards.push(card("work/a/1", "idle", "verify"));
        s.cards.push(card("work/a/2", "idle", "custom"));
        s.cards.push(card("work/a/3", "idle", "refine"));
        assert_eq!(ordered_stages(&s, None), vec!["refine", "verify", "custom"]);
        assert_eq!(ordered_stages(&BoardSnapshot::default(), None).len(), 5);
    }

    #[test]
    fn questions_close_after_human_reply() {
        let mut s = BoardSnapshot::default();
        s.messages.push(ev(
            1,
            "project/a/card/1",
            "msg.ask",
            json!({"question":"q?"}),
        ));
        s.messages.push(ev(
            2,
            "project/a/card/2",
            "msg.ask",
            json!({"question":"r?"}),
        ));
        s.recent.push(ev(
            3,
            "project/a/card/1",
            "msg.human",
            json!({"text":"yes"}),
        ));
        let q = open_questions(&s);
        assert_eq!(q.len(), 1);
        assert_eq!(q[0].seq, 2);
        assert_eq!(inbox(&s).len(), 1);
    }

    #[test]
    fn thread_merges_history_and_live() {
        let hist = vec![
            ev(1, "project/a/card/1", "card.created", json!({"title":"t"})),
            ev(
                2,
                "project/a/card/1",
                "instance.output",
                json!({"text":"hi"}),
            ),
        ];
        let recent = vec![
            ev(
                2,
                "project/a/card/1",
                "instance.output",
                json!({"text":"hi"}),
            ),
            ev(3, "project/a/card/1", "msg.human", json!({"text":"go"})),
            ev(4, "project/a/card/2", "msg.human", json!({"text":"other"})),
            ev(5, "project/a/card/1", "card.file_changed", json!({})),
            ev(6, "project/a", "msg.human", json!({"text":"orch"})),
        ];
        let t = thread(&hist, &recent, "project/a/card/1", false);
        assert_eq!(t.iter().map(|e| e.seq).collect::<Vec<_>>(), vec![1, 2, 3]);
        let o = thread(&[], &recent, "project/a", true);
        assert_eq!(o.iter().map(|e| e.seq).collect::<Vec<_>>(), vec![6]);
        assert_eq!(thread_item(&t[2]).bubble, Bubble::Human);
    }

    #[test]
    fn tool_lines_are_short() {
        let s = tool_summary("Bash", &json!({"command": "cargo test\n  --all"}));
        assert_eq!(s, "Bash · cargo test --all");
        assert_eq!(
            tool_summary("mcp__jadecode__report", &json!({"summary":"done"})),
            "report · done"
        );
    }

    #[test]
    fn markdown_parses_common_blocks() {
        let md = "---\nid: x\n---\n# 제목\n\n본문 한 줄\n이어짐\n\n- 하나\n  - 둘\n- [x] 끝\n1. 첫째\n```\ncode\n```\n> 인용\n";
        let b = markdown_blocks(md);
        assert_eq!(
            b,
            vec![
                MdBlock::Heading(1, "제목".into()),
                MdBlock::Para("본문 한 줄 이어짐".into()),
                MdBlock::Bullet(0, "하나".into()),
                MdBlock::Bullet(1, "둘".into()),
                MdBlock::Check(true, "끝".into()),
                MdBlock::Bullet(0, "1. 첫째".into()),
                MdBlock::Code(vec!["code".into()]),
                MdBlock::Quote("인용".into()),
            ]
        );
    }

    #[test]
    fn diff_classifies_lines() {
        let d = "## repo\ndiff --git a/x b/x\n--- a/x\n+++ b/x\n@@ -1 +1 @@\n-old\n+new\n ctx\n";
        let l = diff_lines(d);
        assert_eq!(diff_stat(&l), (1, 1, 1));
        assert_eq!(l[4].0, DiffKind::Hunk);
    }

    #[test]
    fn collect_diff_in_card_repo() {
        let root = std::env::temp_dir().join(format!("jc-vm-diff-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        let repo = root.join("repo");
        std::fs::create_dir_all(&repo).unwrap();
        let g = |args: &[&str]| {
            std::process::Command::new("git")
                .arg("-C")
                .arg(&repo)
                .args(args)
                .output()
                .unwrap()
        };
        if g(&["init", "-q"]).status.success() {
            std::fs::write(repo.join("a.txt"), "one\n").unwrap();
            g(&["add", "."]);
            g(&[
                "-c",
                "user.email=t@t",
                "-c",
                "user.name=t",
                "commit",
                "-qm",
                "init",
            ]);
            std::fs::write(repo.join("a.txt"), "two\n").unwrap();
            std::fs::write(repo.join("b.txt"), "new\n").unwrap();
            let d = collect_diff(&root, 1000);
            assert!(d.contains("## repo"), "{d}");
            assert!(d.contains("+two"), "{d}");
            assert!(d.contains("?? b.txt"), "{d}");
        }
        let _ = std::fs::remove_dir_all(&root);
    }
}
