//! 카드 생성·조회.

use crate::event::NewEvent;
use crate::taskmd::{self, Front, RepoRef, DEFAULT_BODY};
use crate::topic::{card_topic, project_topic};
use crate::ulid;
use crate::workspace::Workspace;
use anyhow::{bail, Context, Result};
use std::path::PathBuf;

#[derive(Debug, Clone, Default)]
pub struct NewCard {
    pub project: String,
    pub title: String,
    pub path: Vec<String>,
    pub repos: Vec<RepoRef>,
    pub depends_on: Vec<String>,
    pub parent: Option<String>,
    /// 본문을 직접 줄 때(없으면 기본 양식).
    pub body: Option<String>,
}

/// 폴더 이름: `<slug>-<ulid 뒤 6자>`. slug가 없으면 ULID 전체(소문자).
pub fn card_dir_name(title: &str, id: &str) -> String {
    let slug: String = title
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() {
                c.to_ascii_lowercase()
            } else if c.is_whitespace() || c == '-' || c == '_' {
                '-'
            } else {
                '\u{0}' // CJK 등은 slug에 넣지 않는다
            }
        })
        .filter(|c| *c != '\u{0}')
        .collect::<String>()
        .split('-')
        .filter(|s| !s.is_empty())
        .collect::<Vec<_>>()
        .join("-");
    let slug = slug.chars().take(40).collect::<String>();
    let tail = &id[id.len() - 6..];
    if slug.is_empty() {
        // 제목이 비-ASCII(한글 등)면 slug 없이 만든다.
        format!("card-{}", tail.to_lowercase())
    } else {
        format!("{slug}-{}", tail.to_lowercase())
    }
}

/// 카드 폴더와 `task.md`만 만든다(이벤트는 호출자가 일지에 넣는다).
/// 서버의 쓰기 스레드와 CLI가 같은 코드를 쓰게 하려고 분리했다.
pub fn materialize(ws_root: &std::path::Path, c: &NewCard) -> Result<(String, String, Front)> {
    if c.project.is_empty() {
        bail!("project required");
    }
    let project_rel = format!("work/{}", c.project);
    if !ws_root.join(&project_rel).is_dir() {
        bail!("no such project: {}", c.project);
    }
    let id = ulid::new_id();
    let dir = card_dir_name(&c.title, &id);
    let rel = format!("{project_rel}/{dir}");
    let abs = ws_root.join(&rel);
    if abs.exists() {
        bail!("card dir exists: {}", abs.display());
    }
    let front = Front {
        id: id.clone(),
        title: c.title.clone(),
        project: c.project.clone(),
        created: crate::ulid::date_from_ms(crate::event::now_ms()),
        repos: c.repos.clone(),
        path: c.path.clone(),
        stage: c.path.first().cloned().unwrap_or_default(),
        status: "idle".into(),
        path_index: 0,
        depends_on: c.depends_on.clone(),
        parent: c.parent.clone(),
    };
    let body = c.body.clone().unwrap_or_else(|| DEFAULT_BODY.to_string());
    let doc = taskmd::render_new_card(&front, &body)?;
    std::fs::create_dir_all(&abs).with_context(|| format!("create {}", abs.display()))?;
    std::fs::write(abs.join("task.md"), doc)?;
    Ok((rel, id, front))
}

impl Workspace {
    /// 카드를 만든다. 폴더·`task.md`를 쓰고 `card.created` 이벤트를 남긴다.
    /// **서버가 돌고 있으면 쓰지 않는다** — 그때는 RPC(`card.add`)를 써야 seq가 어긋나지 않는다.
    pub fn add_card(&mut self, c: NewCard) -> Result<PathBuf> {
        if c.project.is_empty() {
            bail!("project required");
        }
        let project_rel = format!("work/{}", c.project);
        if !self.root.join(&project_rel).is_dir() {
            bail!("no such project: {}", c.project);
        }
        let id = ulid::new_id();
        let dir = card_dir_name(&c.title, &id);
        let rel = format!("{project_rel}/{dir}");
        let abs = self.root.join(&rel);
        if abs.exists() {
            bail!("card dir exists: {}", abs.display());
        }
        let front = Front {
            id: id.clone(),
            title: c.title.clone(),
            project: c.project.clone(),
            created: crate::ulid::date_from_ms(crate::event::now_ms()),
            repos: c.repos.clone(),
            path: c.path.clone(),
            stage: c.path.first().cloned().unwrap_or_default(),
            status: "idle".into(),
            path_index: 0,
            depends_on: c.depends_on.clone(),
            parent: c.parent.clone(),
        };
        let body = c.body.unwrap_or_else(|| DEFAULT_BODY.to_string());
        let doc = taskmd::render_new_card(&front, &body)?;
        std::fs::create_dir_all(&abs).with_context(|| format!("create {}", abs.display()))?;
        std::fs::write(abs.join("task.md"), doc)?;
        let store = self.store()?;
        let mut store = store;
        store.append(vec![NewEvent::new(
            card_topic(&rel),
            "card.created",
            serde_json::json!({"id":id,"title":c.title,"project":c.project,"path":c.path,
                "repos":c.repos,"depends_on":c.depends_on,"parent":c.parent}),
        )])?;
        store.append(vec![NewEvent::new(
            project_topic(&project_rel),
            "card.added",
            serde_json::json!({"card":rel,"id":id,"title":c.title}),
        )])?;
        Ok(abs)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn dir_names() {
        let id = "01S5CARD00000000000000AB";
        assert_eq!(card_dir_name("로그인 rate limit", id), "rate-limit-0000ab");
        assert_eq!(card_dir_name("", id), "card-0000ab");
        assert_eq!(card_dir_name("a  b__c", id), "a-b-c-0000ab");
    }
}
