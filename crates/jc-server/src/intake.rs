//! 외부 사이클: 코드 밖에서 들어온 일(메일·메신저·이슈·메모)을 모으고, 분류해 카드로 만든다.
//!
//! 흐름: `intake.add`(수집) → `intake.triage`(카드로 / 보관) → 카드가 끝나면 "회신 대기" →
//! `intake.close`(회신했거나 닫음).
//!
//! 정본은 프로젝트 일지의 `project/<p>/intake` 주제 이벤트다. 상태는 이벤트를 접어서 만든다.
//!
//! - `intake.received {id, source, title, body, url, from, external_id}`
//! - `intake.triaged {id, decision: "card"|"archive", card?, brief?}`
//! - `intake.closed {id, note?}`
//!
//! 카드에는 원문 전체가 아니라 **짧은 요약(brief)과 완료 조건, 출처 링크**만 넣는다.
//! 내부 사이클(코드 작업 에이전트)로 넘어가는 맥락을 작게 유지하기 위해서다.

use crate::rpc::Error;
use crate::server::Shared;
use jc_core::index;
use jc_core::NewEvent;
use serde::Serialize;
use serde_json::{json, Value};
use std::sync::Arc;

/// 수집 주제.
pub fn intake_topic(project: &str) -> String {
    format!("project/{project}/intake")
}

#[derive(Debug, Clone, Serialize, PartialEq)]
pub struct IntakeItem {
    pub id: String,
    pub project: String,
    pub source: String,
    pub title: String,
    pub body: String,
    pub url: String,
    pub from: String,
    pub external_id: String,
    /// 가져온 커넥터 이름(직접 넣은 메모면 빈칸).
    pub connector: String,
    /// 커넥터 안에서의 원래 id(답장할 때 쓴다).
    pub raw_id: String,
    /// new | carded | archived | reply_pending | closed
    pub state: String,
    pub card: Option<String>,
    pub brief: String,
    pub seq: u64,
    pub ts: u64,
}

/// 이벤트(오래된 것부터)를 접어 항목 목록을 만든다. 카드 상태는 `card_done`으로 묻는다.
pub fn fold(events: &[Value], card_done: impl Fn(&str) -> bool) -> Vec<IntakeItem> {
    let mut items: Vec<IntakeItem> = vec![];
    for e in events {
        let b = &e["body"];
        let id = b["id"].as_str().unwrap_or("").to_string();
        if id.is_empty() {
            continue;
        }
        let s = |k: &str| b[k].as_str().unwrap_or("").to_string();
        match e["kind"].as_str().unwrap_or("") {
            "intake.received" => {
                if items.iter().any(|i| i.id == id) {
                    continue;
                }
                let topic = e["topic"].as_str().unwrap_or("");
                let project = topic.split('/').nth(1).unwrap_or("").to_string();
                items.push(IntakeItem {
                    id,
                    project,
                    source: s("source"),
                    title: s("title"),
                    body: s("body"),
                    url: s("url"),
                    from: s("from"),
                    external_id: s("external_id"),
                    connector: s("connector"),
                    raw_id: s("raw_id"),
                    state: "new".into(),
                    card: None,
                    brief: String::new(),
                    seq: e["seq"].as_u64().unwrap_or(0),
                    ts: e["ts"].as_u64().unwrap_or(0),
                });
            }
            "intake.triaged" => {
                if let Some(i) = items.iter_mut().find(|i| i.id == id) {
                    match b["decision"].as_str() {
                        Some("card") => {
                            i.state = "carded".into();
                            i.card = b["card"].as_str().map(String::from);
                            i.brief = s("brief");
                        }
                        Some("archive") => i.state = "archived".into(),
                        _ => {}
                    }
                }
            }
            "intake.closed" => {
                if let Some(i) = items.iter_mut().find(|i| i.id == id) {
                    i.state = "closed".into();
                }
            }
            _ => {}
        }
    }
    for i in &mut items {
        if i.state == "carded" {
            if let Some(c) = &i.card {
                if card_done(c) {
                    i.state = "reply_pending".into();
                }
            }
        }
    }
    items
}

/// 카드 본문: 짧은 요약 + 완료 조건 + 출처 링크. 원문은 넣지 않는다.
pub fn card_body(item: &IntakeItem, brief: &str, done_when: &str) -> String {
    let mut s = String::new();
    s.push_str("## 할 일\n\n");
    s.push_str(brief.trim());
    s.push_str("\n\n");
    if !done_when.trim().is_empty() {
        s.push_str("## 완료 조건\n\n");
        s.push_str(done_when.trim());
        s.push_str("\n\n");
    }
    s.push_str("## 출처\n\n");
    s.push_str(&format!(
        "- {} · {}{}\n",
        item.source,
        if item.from.is_empty() {
            "보낸 사람 모름".to_string()
        } else {
            item.from.clone()
        },
        if item.url.is_empty() {
            String::new()
        } else {
            format!(" · {}", item.url)
        }
    ));
    s.push_str(&format!(
        "- 수집 항목 `{}` (원문은 `intake_get`으로 읽는다)\n",
        item.id
    ));
    s
}

fn read_items(sh: &Shared, project: Option<&str>) -> Result<Vec<IntakeItem>, Error> {
    let conn = index::open_index_ro(&sh.root).map_err(|e| Error::internal(e.to_string()))?;
    let (sql, arg) = match project {
        Some(p) => (
            "SELECT line FROM events WHERE topic = ?1 ORDER BY seq",
            intake_topic(p),
        ),
        None => (
            "SELECT line FROM events WHERE topic GLOB ?1 ORDER BY seq",
            "project/*/intake".to_string(),
        ),
    };
    let mut st = conn
        .prepare(sql)
        .map_err(|e| Error::internal(e.to_string()))?;
    let rows: Vec<Value> = st
        .query_map([arg], |r| r.get::<_, String>(0))
        .map_err(|e| Error::internal(e.to_string()))?
        .filter_map(|x| x.ok())
        .filter_map(|l| serde_json::from_str::<Value>(&l).ok())
        .collect();
    let cards = index::card_rows(&conn).map_err(|e| Error::internal(e.to_string()))?;
    Ok(fold(&rows, |c| {
        cards.iter().any(|r| r.path == c && r.status == "done")
    }))
}

fn find(sh: &Shared, id: &str) -> Result<IntakeItem, Error> {
    read_items(sh, None)?
        .into_iter()
        .find(|i| i.id == id)
        .ok_or_else(|| Error::invalid_params(format!("no intake item {id}")))
}

/// `intake.list {project?, state?}`
pub fn list(sh: &Shared, params: &Value) -> Result<Value, Error> {
    let project = params.get("project").and_then(|v| v.as_str());
    let state = params.get("state").and_then(|v| v.as_str());
    let items: Vec<IntakeItem> = read_items(sh, project)?
        .into_iter()
        .filter(|i| state.map(|s| i.state == s).unwrap_or(true))
        .collect();
    Ok(json!({"items": items}))
}

/// `intake.get {id}` — 원문 포함.
pub fn get(sh: &Shared, params: &Value) -> Result<Value, Error> {
    let id = params["id"]
        .as_str()
        .ok_or_else(|| Error::invalid_params("params.id required"))?;
    Ok(json!({"item": find(sh, id)?}))
}

/// `intake.add {project, source, title, body?, url?, from?, external_id?}`
/// 같은 `external_id`가 이미 있으면 새로 넣지 않는다(동기화를 여러 번 해도 안전하다).
pub async fn add(sh: Arc<Shared>, params: &Value) -> Result<Value, Error> {
    let s = |k: &str| params[k].as_str().unwrap_or("").trim().to_string();
    let project = s("project");
    let title = s("title");
    if project.is_empty() || title.is_empty() {
        return Err(Error::invalid_params(
            "params.project and params.title required",
        ));
    }
    if !sh.root.join("work").join(&project).is_dir() {
        return Err(Error::invalid_params(format!("no project {project}")));
    }
    let external_id = s("external_id");
    if !external_id.is_empty() {
        if let Some(i) = read_items(&sh, Some(&project))?
            .into_iter()
            .find(|i| i.external_id == external_id)
        {
            return Ok(json!({"id": i.id, "duplicate": true}));
        }
    }
    let id = format!("in-{}", jc_core::ulid::new_id().to_lowercase());
    let source = if s("source").is_empty() {
        "memo".to_string()
    } else {
        s("source")
    };
    let seqs = sh
        .append(vec![NewEvent::new(
            intake_topic(&project),
            "intake.received",
            json!({"id": id, "source": source, "title": title, "body": params["body"].as_str().unwrap_or(""),
                   "url": s("url"), "from": s("from"), "external_id": external_id,
                   "connector": s("connector"), "raw_id": s("raw_id")}),
        )])
        .await?;
    Ok(json!({"id": id, "seqs": seqs}))
}

/// `intake.triage {id, decision: "card"|"archive", title?, brief?, done_when?, start?}`
pub async fn triage(sh: Arc<Shared>, params: &Value) -> Result<Value, Error> {
    let id = params["id"]
        .as_str()
        .ok_or_else(|| Error::invalid_params("params.id required"))?
        .to_string();
    let item = find(&sh, &id)?;
    if item.state != "new" {
        return Err(Error::refused(
            format!("intake {id} is already {}", item.state),
            "state",
        ));
    }
    let decision = params["decision"].as_str().unwrap_or("card");
    match decision {
        "archive" => {
            let seqs = sh
                .append(vec![NewEvent::new(
                    intake_topic(&item.project),
                    "intake.triaged",
                    json!({"id": id, "decision": "archive", "note": params["note"].as_str().unwrap_or("")}),
                )])
                .await?;
            Ok(json!({"id": id, "decision": "archive", "seqs": seqs}))
        }
        "card" => {
            let title = params["title"]
                .as_str()
                .filter(|s| !s.trim().is_empty())
                .unwrap_or(&item.title)
                .to_string();
            let brief = params["brief"]
                .as_str()
                .filter(|s| !s.trim().is_empty())
                .map(String::from)
                .unwrap_or_else(|| short(&item.body, 600));
            let done_when = params["done_when"].as_str().unwrap_or("");
            let body = card_body(&item, &brief, done_when);
            let created = crate::server::card_add(
                sh.clone(),
                &json!({"project": item.project, "title": title, "body": body}),
            )
            .await?;
            let card = created["path"].as_str().unwrap_or("").to_string();
            let seqs = sh
                .append(vec![
                    NewEvent::new(
                        intake_topic(&item.project),
                        "intake.triaged",
                        json!({"id": id, "decision": "card", "card": card, "brief": brief}),
                    ),
                    NewEvent::new(
                        jc_core::topic::card_topic(&card),
                        "card.from_intake",
                        json!({"intake": id, "source": item.source, "url": item.url, "from": item.from}),
                    ),
                ])
                .await?;
            Ok(json!({"id": id, "decision": "card", "card": card, "seqs": seqs}))
        }
        other => Err(Error::invalid_params(format!("unknown decision {other}"))),
    }
}

/// `intake.close {id, note?}` — 회신했거나 더 할 것이 없다.
pub async fn close(sh: Arc<Shared>, params: &Value) -> Result<Value, Error> {
    let id = params["id"]
        .as_str()
        .ok_or_else(|| Error::invalid_params("params.id required"))?
        .to_string();
    let item = find(&sh, &id)?;
    let seqs = sh
        .append(vec![NewEvent::new(
            intake_topic(&item.project),
            "intake.closed",
            json!({"id": id, "note": params["note"].as_str().unwrap_or("")}),
        )])
        .await?;
    Ok(json!({"id": id, "seqs": seqs}))
}

/// `intake.reply {id, text}` — 사람이 승인한 답장을 원래 출처로 보내고 항목을 닫는다.
pub async fn reply(sh: Arc<Shared>, params: &Value) -> Result<Value, Error> {
    let id = params["id"]
        .as_str()
        .ok_or_else(|| Error::invalid_params("params.id required"))?
        .to_string();
    let text = params["text"].as_str().unwrap_or("").trim().to_string();
    if text.is_empty() {
        return Err(Error::invalid_params("params.text required"));
    }
    let item = find(&sh, &id)?;
    if item.connector.is_empty() {
        return Err(Error::refused(
            "직접 넣은 항목이라 보낼 곳이 없다",
            "no_connector",
        ));
    }
    let list = crate::connectors::load(&sh.root).map_err(|e| Error::internal(e.to_string()))?;
    let c = list
        .iter()
        .find(|c| c.name == item.connector)
        .ok_or_else(|| {
            Error::refused(format!("커넥터 {}가 없다", item.connector), "no_connector")
        })?;
    crate::connectors::reply(c, &item.raw_id, &text)
        .await
        .map_err(|e| Error::refused(format!("{e:#}"), "reply_failed"))?;
    let seqs = sh
        .append(vec![NewEvent::new(
            intake_topic(&item.project),
            "intake.closed",
            json!({"id": id, "note": "replied", "reply": text, "via": item.connector}),
        )])
        .await?;
    Ok(json!({"id": id, "sent": true, "seqs": seqs}))
}

/// 커넥터 하나를 한 번 동기화한다. 새로 넣은 수를 돌려준다.
pub async fn sync_connector(
    sh: Arc<Shared>,
    c: &crate::connectors::Connector,
) -> Result<usize, Error> {
    let fetched = crate::connectors::fetch(c)
        .await
        .map_err(|e| Error::refused(format!("{e:#}"), "fetch_failed"))?;
    let mut added = 0;
    for f in fetched {
        let r = add(
            sh.clone(),
            &json!({
                "project": c.project,
                "source": c.name,
                "title": f.title,
                "body": f.body,
                "url": f.url,
                "from": f.from,
                "external_id": format!("{}:{}", c.name, f.raw_id),
                "connector": c.name,
                "raw_id": f.raw_id,
            }),
        )
        .await?;
        if r["duplicate"].as_bool() != Some(true) {
            added += 1;
        }
    }
    Ok(added)
}

/// `intake.sync_github {project, repo, limit?}` — 열린 이슈를 수집한다(`gh` CLI 사용).
pub async fn sync_github(sh: Arc<Shared>, params: &Value) -> Result<Value, Error> {
    let project = params["project"]
        .as_str()
        .ok_or_else(|| Error::invalid_params("params.project required"))?
        .to_string();
    let repo = params["repo"]
        .as_str()
        .filter(|r| r.contains('/'))
        .ok_or_else(|| Error::invalid_params("params.repo는 owner/name 형식이어야 한다"))?
        .to_string();
    let limit = params["limit"].as_u64().unwrap_or(30).min(200).to_string();
    let out = tokio::process::Command::new("gh")
        .args([
            "issue",
            "list",
            "--repo",
            &repo,
            "--state",
            "open",
            "--limit",
            &limit,
            "--json",
            "number,title,body,url,author",
        ])
        .output()
        .await
        .map_err(|e| {
            Error::refused(
                format!(
                    "gh CLI를 실행하지 못했다({e}). `brew install gh && gh auth login`이 필요하다"
                ),
                "gh_missing",
            )
        })?;
    if !out.status.success() {
        return Err(Error::refused(
            format!(
                "gh issue list 실패: {}",
                String::from_utf8_lossy(&out.stderr).trim()
            ),
            "gh_failed",
        ));
    }
    let issues: Vec<Value> = serde_json::from_slice(&out.stdout).unwrap_or_default();
    let mut added = 0;
    for i in &issues {
        let r = add(
            sh.clone(),
            &json!({
                "project": project,
                "source": "github",
                "title": format!("#{} {}", i["number"], i["title"].as_str().unwrap_or("")),
                "body": i["body"].as_str().unwrap_or(""),
                "url": i["url"].as_str().unwrap_or(""),
                "from": i["author"]["login"].as_str().unwrap_or(""),
                "external_id": format!("github:{repo}#{}", i["number"]),
            }),
        )
        .await?;
        if r["duplicate"].as_bool() != Some(true) {
            added += 1;
        }
    }
    Ok(json!({"seen": issues.len(), "added": added}))
}

fn short(s: &str, n: usize) -> String {
    let t = s.trim();
    if t.chars().count() <= n {
        t.to_string()
    } else {
        let mut x: String = t.chars().take(n).collect();
        x.push('…');
        x
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ev(seq: u64, kind: &str, body: Value) -> Value {
        json!({"seq": seq, "ts": 0, "topic": "project/p/intake", "kind": kind, "body": body})
    }

    #[test]
    fn folds_lifecycle() {
        let evs = vec![
            ev(
                1,
                "intake.received",
                json!({"id":"a","source":"memo","title":"A"}),
            ),
            ev(
                2,
                "intake.received",
                json!({"id":"b","source":"github","title":"B"}),
            ),
            ev(
                3,
                "intake.received",
                json!({"id":"c","source":"email","title":"C"}),
            ),
            ev(
                4,
                "intake.received",
                json!({"id":"d","source":"email","title":"D"}),
            ),
            ev(
                5,
                "intake.triaged",
                json!({"id":"a","decision":"card","card":"work/p/a1","brief":"짧게"}),
            ),
            ev(6, "intake.triaged", json!({"id":"b","decision":"archive"})),
            ev(
                7,
                "intake.triaged",
                json!({"id":"c","decision":"card","card":"work/p/c1"}),
            ),
            ev(8, "intake.closed", json!({"id":"d"})),
        ];
        let items = fold(&evs, |c| c == "work/p/c1");
        let st: Vec<_> = items
            .iter()
            .map(|i| (i.id.as_str(), i.state.as_str()))
            .collect();
        assert_eq!(
            st,
            vec![
                ("a", "carded"),
                ("b", "archived"),
                ("c", "reply_pending"),
                ("d", "closed")
            ]
        );
        assert_eq!(items[0].project, "p");
        assert_eq!(items[0].brief, "짧게");
    }

    #[test]
    fn card_body_keeps_context_small() {
        let item = IntakeItem {
            id: "in-1".into(),
            project: "p".into(),
            source: "email".into(),
            title: "t".into(),
            body: "아주 긴 원문".repeat(500),
            url: "https://x".into(),
            from: "kim".into(),
            external_id: String::new(),
            connector: String::new(),
            raw_id: String::new(),
            state: "new".into(),
            card: None,
            brief: String::new(),
            seq: 1,
            ts: 0,
        };
        let b = card_body(&item, "로그인 오류를 고친다", "테스트 통과");
        assert!(b.contains("로그인 오류를 고친다"));
        assert!(b.contains("## 완료 조건"));
        assert!(b.contains("https://x"));
        assert!(!b.contains("아주 긴 원문"));
        assert!(b.len() < 400);
    }
}
