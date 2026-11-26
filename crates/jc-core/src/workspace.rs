//! 워크스페이스와 프로젝트.
//!
//! ```text
//! <ws>/
//!   <project>/
//!     project.toml          name, repo(코드 레포 경로), base(선택: 새 브랜치의 기준)
//!     todo/  doing/  review/  done/   칸반 열. 그 안의 *.md 가 메모
//!   .jadecode/<project>/    실행 상태(세션 id, 대화 기록, 워크트리). 사람이 볼 필요가 없다
//! ```

use crate::board::Column;
use anyhow::{bail, Context, Result};
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

#[derive(Debug, Clone)]
pub struct Workspace {
    pub root: PathBuf,
}

/// `project.toml`에 적히는 값.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct ProjectFile {
    pub name: String,
    /// 코드 레포 폴더. 메모 작업은 여기서 워크트리를 연다.
    #[serde(default)]
    pub repo: String,
    /// 새 브랜치를 만들 기준(비우면 레포의 현재 HEAD).
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub base: String,
    /// 메모 작업 에이전트의 권한 모드. 비우면 `bypassPermissions`(워크트리 안에서 묻지 않고 일한다).
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub permission_mode: String,
    /// 모델(비우면 Claude Code 기본값).
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub model: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Project {
    /// 폴더 이름.
    pub slug: String,
    pub dir: PathBuf,
    pub file: ProjectFile,
}

impl Project {
    pub fn name(&self) -> &str {
        if self.file.name.trim().is_empty() {
            &self.slug
        } else {
            &self.file.name
        }
    }

    /// 코드 레포 경로(`~`를 펼친다). 비어 있으면 없음.
    pub fn repo(&self) -> Option<PathBuf> {
        let r = self.file.repo.trim();
        (!r.is_empty()).then(|| expand_tilde(r))
    }

    pub fn column_dir(&self, c: Column) -> PathBuf {
        self.dir.join(c.dir_name())
    }
}

pub fn expand_tilde(p: &str) -> PathBuf {
    if let Some(rest) = p.strip_prefix("~/") {
        if let Some(h) = std::env::var_os("HOME") {
            return PathBuf::from(h).join(rest);
        }
    }
    PathBuf::from(p)
}

/// 사람이 읽을 경로(`$HOME` → `~`).
pub fn tilde(p: &Path) -> String {
    let s = p.display().to_string();
    match std::env::var("HOME") {
        Ok(h) if !h.is_empty() && s.starts_with(&h) => format!("~{}", &s[h.len()..]),
        _ => s,
    }
}

/// 폴더 이름으로 쓸 수 있게 바꾼다(영문 소문자·숫자·`-`, 한글은 그대로 둔다).
pub fn slugify(s: &str) -> String {
    let mut out = String::new();
    let mut dash = false;
    for ch in s.trim().chars() {
        if ch.is_alphanumeric() {
            for c in ch.to_lowercase() {
                out.push(c);
            }
            dash = false;
        } else if !dash && !out.is_empty() {
            out.push('-');
            dash = true;
        }
    }
    while out.ends_with('-') {
        out.pop();
    }
    out.chars().take(48).collect()
}

impl Workspace {
    /// 인자 → `JADECODE_WORKSPACE` → `~/jc/work` 순서로 고르고, 없으면 만든다.
    pub fn resolve(arg: Option<&Path>) -> Result<Self> {
        let root = match arg {
            Some(p) => p.to_path_buf(),
            None => match std::env::var_os("JADECODE_WORKSPACE") {
                Some(p) if !p.is_empty() => PathBuf::from(p),
                _ => {
                    let home = std::env::var_os("HOME").context("HOME이 없다")?;
                    PathBuf::from(home).join("jc/work")
                }
            },
        };
        Self::open(&root)
    }

    pub fn open(root: &Path) -> Result<Self> {
        std::fs::create_dir_all(root)
            .with_context(|| format!("워크스페이스 폴더를 만들 수 없다: {}", root.display()))?;
        let root = std::fs::canonicalize(root).unwrap_or_else(|_| root.to_path_buf());
        Ok(Self { root })
    }

    /// 실행 상태 폴더(`<ws>/.jadecode/<project>`).
    pub fn state_dir(&self, project: &str) -> PathBuf {
        self.root.join(".jadecode").join(project)
    }

    /// `project.toml`이 있는 하위 폴더를 이름순으로.
    pub fn projects(&self) -> Vec<Project> {
        let mut out = vec![];
        let Ok(rd) = std::fs::read_dir(&self.root) else {
            return out;
        };
        for e in rd.flatten() {
            let dir = e.path();
            let slug = e.file_name().to_string_lossy().to_string();
            if slug.starts_with('.') || !dir.join("project.toml").is_file() {
                continue;
            }
            if let Ok(p) = self.load_project(&slug) {
                out.push(p);
            }
        }
        out.sort_by_key(|p| p.name().to_lowercase());
        out
    }

    pub fn load_project(&self, slug: &str) -> Result<Project> {
        let dir = self.root.join(slug);
        let text = std::fs::read_to_string(dir.join("project.toml"))
            .with_context(|| format!("{slug}/project.toml을 읽을 수 없다"))?;
        let file: ProjectFile =
            toml::from_str(&text).with_context(|| format!("{slug}/project.toml 형식 오류"))?;
        Ok(Project {
            slug: slug.to_string(),
            dir,
            file,
        })
    }

    /// 새 프로젝트를 만든다. 레포가 git 레포가 아니면 거절한다(빈 값은 허용).
    pub fn create_project(&self, name: &str, repo: &str) -> Result<Project> {
        let name = name.trim();
        if name.is_empty() {
            bail!("프로젝트 이름이 비어 있다");
        }
        let slug = slugify(name);
        if slug.is_empty() {
            bail!("프로젝트 이름으로 폴더 이름을 만들 수 없다: {name}");
        }
        let dir = self.root.join(&slug);
        if dir.join("project.toml").exists() {
            bail!("이미 있는 프로젝트다: {slug}");
        }
        let repo = repo.trim();
        if !repo.is_empty() {
            let rp = expand_tilde(repo);
            if !rp.is_dir() {
                bail!("레포 폴더가 없다: {repo}");
            }
            if !crate::worktree::is_git_repo(&rp) {
                bail!("git 레포가 아니다: {repo}");
            }
        }
        let file = ProjectFile {
            name: name.to_string(),
            repo: repo.to_string(),
            ..Default::default()
        };
        std::fs::create_dir_all(&dir)?;
        for c in Column::ALL {
            std::fs::create_dir_all(dir.join(c.dir_name()))?;
        }
        crate::write_atomic(
            &dir.join("project.toml"),
            toml::to_string(&file)?.as_bytes(),
        )?;
        Ok(Project { slug, dir, file })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn slugs() {
        assert_eq!(slugify("My App!"), "my-app");
        assert_eq!(slugify("  결제 영수증  "), "결제-영수증");
        assert_eq!(slugify("a--b"), "a-b");
        assert_eq!(slugify("!!"), "");
    }

    #[test]
    fn create_and_list() {
        let tmp = crate::board::tests::tmpdir("ws");
        let ws = Workspace::open(&tmp).unwrap();
        assert!(ws.projects().is_empty());
        let p = ws.create_project("Demo App", "").unwrap();
        assert_eq!(p.slug, "demo-app");
        assert!(p.dir.join("todo").is_dir());
        let list = ws.projects();
        assert_eq!(list.len(), 1);
        assert_eq!(list[0].name(), "Demo App");
        assert!(ws.create_project("demo app", "").is_err());
        assert!(ws.create_project("x", "/definitely/not/here").is_err());
        let _ = std::fs::remove_dir_all(&tmp);
    }
}
