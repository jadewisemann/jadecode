//! 워크스페이스 — 정본 파일의 뿌리.
//!
//! ```text
//! <workspace>/
//!   jadecode.toml          # 워크스페이스 설정(이름, 기본 에이전트)
//!   events/global.jsonl    # 봇·정책·전역 이벤트 일지
//!   work/<project>/        # 프로젝트: project.toml + events.jsonl
//!     <card>/              # 카드: task.md + events.jsonl + 레포별 워크트리
//!   arxiv/<YYYY-MM>/       # 끝난 카드 문서
//!   log/YYYY-MM-DD.md      # 오케스트레이터 일일 기록
//!   .jadecode/             # 색인·세션·복구 메모(정본 아님)
//! ```

use crate::event::NewEvent;
use crate::index;
use crate::store::{Fsync, Store};
use crate::taskmd::{card_state, CardState};
use crate::topic::project_topic;
use anyhow::{bail, Context, Result};
use std::path::PathBuf;

#[derive(Debug, Clone)]
pub struct Workspace {
    pub root: PathBuf,
}

#[derive(Debug, Clone)]
pub struct CardEntry {
    pub rel: String,
    pub state: CardState,
}

impl Workspace {
    pub fn open(root: impl Into<PathBuf>) -> Result<Self> {
        let root = root.into();
        let abs = if root.is_absolute() {
            root
        } else {
            std::env::current_dir()?.join(root)
        };
        if !abs.join("jadecode.toml").exists() && !abs.join("work").exists() {
            bail!("not a jadecode workspace: {}", abs.display());
        }
        Ok(Self {
            root: std::fs::canonicalize(&abs).unwrap_or(abs),
        })
    }

    /// 워크스페이스를 만든다. 이미 있으면 손대지 않고 알린다.
    pub fn init(root: impl Into<PathBuf>, name: &str) -> Result<(Self, bool)> {
        let root = root.into();
        let abs = if root.is_absolute() {
            root
        } else {
            std::env::current_dir()?.join(root)
        };
        let created = !abs.join("jadecode.toml").exists();
        std::fs::create_dir_all(&abs).with_context(|| format!("create {}", abs.display()))?;
        for d in ["work", "events", "arxiv", "log", ".jadecode"] {
            std::fs::create_dir_all(abs.join(d))?;
        }
        if created {
            std::fs::write(
                abs.join("jadecode.toml"),
                format!(
                    "# jadecode 워크스페이스\nname = \"{}\"\nversion = \"0.1\"\n\n[agents]\ndefault = \"claude\"\n",
                    name.replace('"', "'")
                ),
            )?;
            std::fs::write(
                abs.join("AGENTS.md"),
                "# 워크스페이스 공통 지침\n\njadecode 워크스페이스다. 카드 폴더의 `task.md`가 정본이다.\n",
            )?;
            std::fs::OpenOptions::new()
                .create(true)
                .append(true)
                .open(abs.join("events/global.jsonl"))?;
            // 기본 봇 네 개와 정책(사람이 정하는 값)을 심는다.
            crate::scaffold::install(&abs)?;
        }
        Ok((
            Self {
                root: std::fs::canonicalize(&abs).unwrap_or(abs),
            },
            created,
        ))
    }

    pub fn name(&self) -> String {
        std::fs::read_to_string(self.root.join("jadecode.toml"))
            .ok()
            .and_then(|s| s.parse::<toml::Table>().ok())
            .and_then(|t| t.get("name").and_then(|v| v.as_str().map(String::from)))
            .unwrap_or_else(|| {
                self.root
                    .file_name()
                    .unwrap_or_default()
                    .to_string_lossy()
                    .to_string()
            })
    }

    /// 프로젝트 폴더를 만든다.
    pub fn add_project(&mut self, name: &str, repo: Option<&str>) -> Result<PathBuf> {
        let slug: String = name
            .chars()
            .map(|c| {
                if c.is_ascii_alphanumeric() {
                    c.to_ascii_lowercase()
                } else {
                    '-'
                }
            })
            .collect::<String>()
            .split('-')
            .filter(|s| !s.is_empty())
            .collect::<Vec<_>>()
            .join("-");
        if slug.is_empty() {
            bail!("project name needs ascii letters or digits: {name:?}");
        }
        let rel = format!("work/{slug}");
        let abs = self.root.join(&rel);
        if abs.exists() {
            bail!("project exists: {slug}");
        }
        std::fs::create_dir_all(&abs)?;
        let repos = match repo {
            Some(r) => format!("[[repos]]\nname = \"{r}\"\n"),
            None => String::new(),
        };
        std::fs::write(
            abs.join("project.toml"),
            format!(
                "# jadecode 프로젝트\nname = \"{}\"\nslug = \"{slug}\"\n{repos}",
                name.replace('"', "'")
            ),
        )?;
        let mut store = self.store()?;
        store.append(vec![NewEvent::new(
            project_topic(&rel),
            "project.created",
            serde_json::json!({"name":name,"slug":slug,"repos":repo}),
        )])?;
        Ok(abs)
    }

    /// 카드 목록. 파일이 정본이므로 색인이 아니라 파일을 읽는다.
    pub fn cards(&self) -> Result<Vec<CardEntry>> {
        let layout = index::scan_layout(&self.root)?;
        let mut out = vec![];
        for rel in layout.cards {
            let p = self.root.join(&rel).join("task.md");
            let content = match std::fs::read_to_string(&p) {
                Ok(c) => c,
                Err(_) => continue,
            };
            match card_state(&content) {
                Ok(state) => out.push(CardEntry { rel, state }),
                Err(e) => eprintln!("skip {}: {e}", p.display()),
            }
        }
        out.sort_by(|a, b| a.state.front.id.cmp(&b.state.front.id));
        Ok(out)
    }

    /// 카드 하나. 폴더 이름·상대 경로·ULID(앞부분)로 찾는다.
    pub fn find_card(&self, needle: &str) -> Result<CardEntry> {
        let cards = self.cards()?;
        let needle = needle.trim_end_matches('/');
        let hits: Vec<&CardEntry> = cards
            .iter()
            .filter(|c| {
                let dir = c.rel.rsplit('/').next().unwrap_or("");
                c.rel == needle
                    || dir == needle
                    || c.state.front.id == needle
                    || (!needle.is_empty() && c.state.front.id.starts_with(needle))
            })
            .collect();
        match hits.len() {
            1 => Ok(hits[0].clone()),
            0 => bail!("no card matches {needle:?}"),
            n => bail!("{n} cards match {needle:?}; be more specific"),
        }
    }

    pub fn store(&self) -> Result<Store> {
        Ok(Store::open(&self.root, Fsync::Batch)?.0)
    }

    pub fn store_with(&self, fsync: Fsync) -> Result<Store> {
        Ok(Store::open(&self.root, fsync)?.0)
    }

    pub fn journal_path(&self, rel: &str) -> PathBuf {
        self.root.join(rel)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn init_and_reopen() {
        let d = tempdir();
        let (ws, created) = Workspace::init(&d, "acme").unwrap();
        assert!(created);
        assert!(ws.root.join("jadecode.toml").exists());
        assert!(ws.root.join("work").is_dir());
        assert_eq!(ws.name(), "acme");
        let (ws2, created2) = Workspace::init(&d, "other").unwrap();
        assert!(!created2);
        assert_eq!(ws2.name(), "acme");
        assert!(Workspace::open(&d).is_ok());
        assert!(Workspace::open(d.join("nope")).is_err());
    }

    #[test]
    fn add_project_and_card() {
        let d = tempdir();
        let (mut ws, _) = Workspace::init(&d, "acme").unwrap();
        ws.add_project("Rate Limit", Some("api")).unwrap();
        assert!(ws.root.join("work/rate-limit/project.toml").exists());
        assert!(ws.add_project("rate limit", None).is_err());
        let p = ws
            .add_card(crate::NewCard {
                project: "rate-limit".into(),
                title: "login throttle".into(),
                path: vec!["refine".into(), "implement".into()],
                ..Default::default()
            })
            .unwrap();
        assert!(p.join("task.md").exists());
        let cards = ws.cards().unwrap();
        assert_eq!(cards.len(), 1);
        assert_eq!(cards[0].state.front.stage, "refine");
        assert!(cards[0].rel.starts_with("work/rate-limit/login-throttle-"));
        let found = ws.find_card(&cards[0].state.front.id[..10]).unwrap();
        assert_eq!(found.rel, cards[0].rel);
        // 카드 이벤트는 카드 일지로, 카드 추가 알림은 프로젝트 일지로
        let cj = std::fs::read_to_string(ws.root.join(&cards[0].rel).join("events.jsonl")).unwrap();
        assert!(cj.contains("card.created"), "{cj}");
        let pj = std::fs::read_to_string(ws.root.join("work/rate-limit/events.jsonl")).unwrap();
        assert!(pj.contains("card.added"), "{pj}");
    }

    #[test]
    fn full_check_clean_after_writes() {
        let d = tempdir();
        let (mut ws, _) = Workspace::init(&d, "acme").unwrap();
        ws.add_project("p1", None).unwrap();
        for i in 0..3 {
            ws.add_card(crate::NewCard {
                project: "p1".into(),
                title: format!("card {i}"),
                path: vec!["refine".into()],
                ..Default::default()
            })
            .unwrap();
        }
        let r = crate::store::full_check(&ws.root).unwrap();
        assert!(r.ok, "{r:?}");
        assert_eq!(r.seq_gaps, 0);
        assert_eq!(r.line_mismatch, 0);
    }

    pub fn tempdir() -> PathBuf {
        crate::workspace::short_temp_root("jct")
    }
}

/// 짧은 임시 워크스페이스 경로를 만든다.
///
/// macOS 유닉스 도메인 소켓은 경로가 104바이트를 넘으면 `bind`가 실패한다
/// (`sun_path[104]`, NUL 포함). `std::env::temp_dir()`은 macOS에서
/// `/var/folders/XX/…/T`(약 50자)라 그 아래에 긴 이름을 붙이면 넘친다.
/// 그래서 `/tmp/jc<번호>`처럼 짧게 쓰고, 없으면 임시 폴더로 물러난다.
pub fn short_temp_root(prefix: &str) -> PathBuf {
    use std::sync::atomic::{AtomicU64, Ordering};
    static N: AtomicU64 = AtomicU64::new(0);
    let n = N.fetch_add(1, Ordering::Relaxed);
    // 이름이 겹치면(이전 실행이 남긴 폴더) 안에 든 잠금 때문에 실패한다.
    // ULID를 붙여 겹치지 않게 하고, 그래도 있으면 지우고 새로 만든다.
    let short = format!(
        "/tmp/{prefix}{}-{}-{}",
        std::process::id(),
        n,
        crate::ulid::new_id()[20..].to_lowercase()
    );
    let p = PathBuf::from(&short);
    if p.exists() {
        let _ = std::fs::remove_dir_all(&p);
    }
    if std::fs::create_dir_all(&p).is_ok() {
        return p;
    }
    let fallback = std::env::temp_dir().join(format!(
        "{prefix}{}-{}-{}",
        std::process::id(),
        n,
        crate::ulid::new_id()[20..].to_lowercase()
    ));
    let _ = std::fs::remove_dir_all(&fallback);
    let _ = std::fs::create_dir_all(&fallback);
    fallback
}

/// 유닉스 소켓 경로가 플랫폼 한도 안에 드는지(진단용).
pub fn socket_path_len_ok(p: &std::path::Path) -> bool {
    // macOS 104, 리눅스 108. 양쪽 다 안전한 100을 기준으로 본다.
    p.as_os_str().len() <= 100
}

#[cfg(test)]
mod sock_tests {
    use super::*;

    #[test]
    fn short_temp_root_is_short() {
        let p = short_temp_root("jcx");
        let s = p.to_string_lossy().to_string();
        assert!(s.len() < 40, "{s}");
        assert!(p.is_dir());
        // 두 번 불러도 서로 다르다.
        let q = short_temp_root("jcx");
        assert_ne!(p, q);
    }

    #[test]
    fn socket_len_check() {
        assert!(socket_path_len_ok(std::path::Path::new("/tmp/a.sock")));
        let long = format!("/var/folders/xx/{}/s.sock", "a".repeat(120));
        assert!(!socket_path_len_ok(std::path::Path::new(&long)));
    }
}
