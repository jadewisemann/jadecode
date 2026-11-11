//! `task.md` 앞머리(`---` YAML)와 서버 소유 칸.
//!
//! 본문은 누구나 고친다. 앞머리의 `jadecode:` 묶음만 서버가 쓴다(아키텍처 ② B2 쓰기 규칙).

use anyhow::{anyhow, bail, Result};
use serde::{Deserialize, Serialize};

pub const FRONT_KEY: &str = "jadecode";

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Default, Eq)]
pub struct RepoRef {
    pub repo: String,
    pub branch: String,
    #[serde(default)]
    pub base: String,
}

/// `jadecode:` 앞머리 묶음 — 서버가 쓰는 칸.
#[derive(Debug, Clone, Serialize, Deserialize, Default, PartialEq, Eq)]
pub struct Front {
    pub id: String,
    #[serde(default)]
    pub title: String,
    #[serde(default)]
    pub project: String,
    #[serde(default)]
    pub created: String,
    #[serde(default)]
    pub repos: Vec<RepoRef>,
    /// 카드가 거칠 단계(프로젝트 단계 목록의 부분집합).
    #[serde(default)]
    pub path: Vec<String>,
    #[serde(default)]
    pub stage: String,
    #[serde(default)]
    pub status: String,
    #[serde(default)]
    pub path_index: usize,
    #[serde(default)]
    pub depends_on: Vec<String>,
    #[serde(default)]
    pub parent: Option<String>,
}

impl Front {
    pub fn stage_list(&self) -> Vec<String> {
        self.path.clone()
    }

    pub fn next_stage(&self) -> Option<&str> {
        self.path.get(self.path_index + 1).map(|s| s.as_str())
    }
}

pub struct TaskDoc {
    pub front: serde_yaml::Mapping,
    pub body: String,
}

pub fn split_task(content: &str) -> Result<TaskDoc> {
    let rest = content
        .strip_prefix("---\n")
        .ok_or_else(|| anyhow!("no frontmatter"))?;
    let end = rest
        .find("\n---\n")
        .ok_or_else(|| anyhow!("frontmatter not closed"))?;
    let yaml = &rest[..end + 1];
    let body = rest[end + 5..].to_string();
    let v: serde_yaml::Value = serde_yaml::from_str(yaml)?;
    let front = match v {
        serde_yaml::Value::Mapping(m) => m,
        _ => bail!("frontmatter not a mapping"),
    };
    Ok(TaskDoc { front, body })
}

pub fn join_task(front: &serde_yaml::Mapping, body: &str) -> Result<String> {
    let y = serde_yaml::to_string(front)?;
    Ok(format!("---\n{y}---\n{body}"))
}

/// `task.md` 전체에서 뽑은 상태. 해시 세 개로 "누가 무엇을 바꿨나"를 가른다.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct CardState {
    pub file_hash: String,
    pub id_hash: String,   // 서버 소유 묶음
    pub body_hash: String, // 그 밖의 앞머리 + 본문
    pub front: Front,
    pub raw_front: serde_yaml::Value,
}

impl CardState {
    pub fn id(&self) -> &str {
        &self.front.id
    }
    pub fn stage(&self) -> &str {
        &self.front.stage
    }
    pub fn status(&self) -> &str {
        &self.front.status
    }
    pub fn title(&self) -> &str {
        &self.front.title
    }
}

pub fn card_state(content: &str) -> Result<CardState> {
    let doc = split_task(content)?;
    let key = serde_yaml::Value::String(FRONT_KEY.into());
    let jc = doc
        .front
        .get(&key)
        .cloned()
        .unwrap_or(serde_yaml::Value::Null);
    let mut rest = doc.front.clone();
    rest.remove(&key);
    let jc_s = serde_yaml::to_string(&jc)?;
    let rest_s = serde_yaml::to_string(&rest)?;
    let front: Front = if jc.is_null() {
        Front::default()
    } else {
        serde_yaml::from_value(jc.clone()).unwrap_or_default()
    };
    Ok(CardState {
        file_hash: crate::index::sha(content.as_bytes()),
        id_hash: crate::index::sha(jc_s.as_bytes()),
        body_hash: crate::index::sha(format!("{rest_s}\u{0}{}", doc.body).as_bytes()),
        front,
        raw_front: jc,
    })
}

/// 서버가 쓰는 칸만 바꾸고 문서를 다시 만든다. 본문·다른 앞머리는 그대로 둔다.
pub fn with_front(content: &str, f: &Front) -> Result<String> {
    let mut doc = split_task(content)?;
    let key = serde_yaml::Value::String(FRONT_KEY.into());
    doc.front.insert(key, front_mapping(f)?);
    join_task(&doc.front, &doc.body)
}

/// `Front` → YAML 값. 빈 값(Null)은 넣지 않는다.
fn front_mapping(f: &Front) -> Result<serde_yaml::Value> {
    let mut v = serde_yaml::to_value(f)?;
    if let serde_yaml::Value::Mapping(m) = &mut v {
        m.retain(|_, val| !val.is_null());
    }
    Ok(v)
}

/// 새 카드 문서.
pub fn render_new_card(f: &Front, body: &str) -> Result<String> {
    let mut m = serde_yaml::Mapping::new();
    m.insert(
        serde_yaml::Value::String(FRONT_KEY.into()),
        front_mapping(f)?,
    );
    join_task(&m, body)
}

pub const DEFAULT_BODY: &str =
    "\n## 목표\n\n## 범위\n\n## 완료 조건\n\n## 결정 사항\n\n## 다음 행동\n";

#[cfg(test)]
mod tests {
    use super::*;

    fn sample() -> String {
        render_new_card(
            &Front {
                id: "01S5CARD0000000000000001".into(),
                title: "로그인 rate limit".into(),
                project: "p1".into(),
                created: "2026-11-11".into(),
                repos: vec![RepoRef {
                    repo: "api".into(),
                    branch: "feat/rate-limit".into(),
                    base: "main".into(),
                }],
                path: vec!["구체화".into(), "구현".into()],
                stage: "구체화".into(),
                status: "idle".into(),
                ..Default::default()
            },
            DEFAULT_BODY,
        )
        .unwrap()
    }

    #[test]
    fn roundtrip() {
        let s = sample();
        let cs = card_state(&s).unwrap();
        assert_eq!(cs.id(), "01S5CARD0000000000000001");
        assert_eq!(cs.stage(), "구체화");
        assert_eq!(cs.front.repos.len(), 1);
        assert_eq!(cs.front.next_stage(), Some("구현"));
        // 다시 써도 안정적
        let s2 = with_front(&s, &cs.front).unwrap();
        assert_eq!(card_state(&s2).unwrap().file_hash, cs.file_hash);
    }

    #[test]
    fn body_edit_changes_only_body_hash() {
        let s = sample();
        let a = card_state(&s).unwrap();
        let edited = s.replace("## 목표", "## 목표 (사람이 고침)");
        let b = card_state(&edited).unwrap();
        assert_eq!(a.id_hash, b.id_hash);
        assert_ne!(a.body_hash, b.body_hash);
        assert_ne!(a.file_hash, b.file_hash);
    }

    #[test]
    fn front_edit_changes_id_hash() {
        let s = sample();
        let a = card_state(&s).unwrap();
        let mut f = a.front.clone();
        f.status = "working".into();
        let b = card_state(&with_front(&s, &f).unwrap()).unwrap();
        assert_ne!(a.id_hash, b.id_hash);
        assert_eq!(a.body_hash, b.body_hash);
    }

    #[test]
    fn foreign_frontmatter_is_preserved() {
        let s = sample().replace("jadecode:", "owner: someone\njadecode:");
        let a = card_state(&s).unwrap();
        let mut f = a.front.clone();
        f.status = "done".into();
        let out = with_front(&s, &f).unwrap();
        assert!(out.contains("owner: someone"), "{out}");
    }

    #[test]
    fn invalid_doc_is_error() {
        assert!(card_state("no frontmatter").is_err());
        assert!(card_state("---\nfoo: [\n---\n").is_err());
    }
}
