//! 카드의 레포 워크트리.
//!
//! 프로젝트는 `project.toml`의 `[[repos]]`에 레포를 적는다(`name`, 선택 `path`).
//! 원본 위치는 `path`(사람 컴퓨터의 레포 폴더)이고, 없으면 `<워크스페이스>/repository/<name>`이다.
//! 카드에서 워커를 시작할 때 카드 폴더 아래 `<카드>/<name>`에 `git worktree`를 연다.
//! 브랜치는 카드가 정한 것(`repos[].branch`)이고, 비어 있으면 `jc/<카드 폴더 이름>`이다.

use anyhow::{bail, Context, Result};
use std::path::{Path, PathBuf};
use std::process::Command;

/// 프로젝트에 적힌 레포 하나.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProjectRepo {
    pub name: String,
    pub path: Option<PathBuf>,
}

/// `work/<project>/project.toml`의 레포 목록.
pub fn project_repos(root: &Path, project: &str) -> Vec<ProjectRepo> {
    let p = root.join("work").join(project).join("project.toml");
    let Ok(text) = std::fs::read_to_string(p) else {
        return vec![];
    };
    let Ok(t) = text.parse::<toml::Table>() else {
        return vec![];
    };
    t.get("repos")
        .and_then(|v| v.as_array())
        .into_iter()
        .flatten()
        .filter_map(|r| {
            let name = r.get("name")?.as_str()?.to_string();
            let path = r
                .get("path")
                .and_then(|v| v.as_str())
                .filter(|s| !s.is_empty())
                .map(PathBuf::from);
            Some(ProjectRepo { name, path })
        })
        .collect()
}

/// git 레포 폴더인가(일반 레포·워크트리 모두).
pub fn is_git_repo(p: &Path) -> bool {
    p.join(".git").exists()
}

/// 레포 원본 위치.
pub fn source_of(root: &Path, project: &str, repo: &str) -> Option<PathBuf> {
    let listed = project_repos(root, project)
        .into_iter()
        .find(|r| r.name == repo)
        .and_then(|r| r.path);
    listed
        .into_iter()
        .chain([root.join("repository").join(repo)])
        .find(|p| is_git_repo(p))
}

fn git(dir: &Path, args: &[&str]) -> Result<String> {
    let out = Command::new("git")
        .arg("-C")
        .arg(dir)
        .args(args)
        .output()
        .context("git을 실행하지 못했다(설치돼 있는가?)")?;
    if !out.status.success() {
        bail!(
            "git {}: {}",
            args.join(" "),
            String::from_utf8_lossy(&out.stderr).trim()
        );
    }
    Ok(String::from_utf8_lossy(&out.stdout).trim().to_string())
}

/// 카드 브랜치 이름(비어 있으면 카드 폴더 이름에서 만든다).
pub fn branch_for(card_rel: &str, branch: &str) -> String {
    if !branch.trim().is_empty() {
        return branch.trim().to_string();
    }
    let leaf = card_rel.rsplit('/').next().unwrap_or(card_rel);
    format!("jc/{leaf}")
}

/// 레포 하나의 워크트리를 연다(이미 있으면 그대로 둔다). 워크트리 경로를 돌려준다.
pub fn ensure_one(
    root: &Path,
    project: &str,
    card_rel: &str,
    repo: &str,
    branch: &str,
    base: &str,
) -> Result<PathBuf> {
    let wt = root.join(card_rel).join(repo);
    if is_git_repo(&wt) {
        return Ok(wt);
    }
    let Some(src) = source_of(root, project, repo) else {
        bail!(
            "레포 {repo}의 원본을 찾지 못했다(project.toml의 path나 repository/{repo}를 확인하라)"
        );
    };
    if wt.exists()
        && std::fs::read_dir(&wt)
            .map(|mut d| d.next().is_some())
            .unwrap_or(false)
    {
        bail!("{}가 이미 있고 비어 있지 않다", wt.display());
    }
    let branch = branch_for(card_rel, branch);
    let wt_s = wt.display().to_string();
    let exists = git(
        &src,
        &[
            "rev-parse",
            "--verify",
            "--quiet",
            &format!("refs/heads/{branch}"),
        ],
    )
    .is_ok();
    if exists {
        git(&src, &["worktree", "add", &wt_s, &branch])?;
    } else if base.trim().is_empty() {
        git(&src, &["worktree", "add", "-b", &branch, &wt_s])?;
    } else {
        git(
            &src,
            &["worktree", "add", "-b", &branch, &wt_s, base.trim()],
        )?;
    }
    Ok(wt)
}

/// 카드의 모든 레포 워크트리를 연다. 실패한 레포는 이유를 돌려준다(카드 폴더만으로도 일은 시작한다).
pub fn ensure_card(root: &Path, card_rel: &str) -> Vec<(String, Result<PathBuf, String>)> {
    let Ok(content) = std::fs::read_to_string(root.join(card_rel).join("task.md")) else {
        return vec![];
    };
    let Ok(cs) = jc_core::taskmd::card_state(&content) else {
        return vec![];
    };
    cs.front
        .repos
        .iter()
        .map(|r| {
            let res = ensure_one(
                root,
                &cs.front.project,
                card_rel,
                &r.repo,
                &r.branch,
                &r.base,
            )
            .map_err(|e| format!("{e:#}"));
            (r.repo.clone(), res)
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sh(dir: &Path, args: &[&str]) {
        let st = Command::new("git")
            .arg("-C")
            .arg(dir)
            .args(args)
            .env("GIT_AUTHOR_NAME", "t")
            .env("GIT_AUTHOR_EMAIL", "t@example.com")
            .env("GIT_COMMITTER_NAME", "t")
            .env("GIT_COMMITTER_EMAIL", "t@example.com")
            .status()
            .unwrap();
        assert!(st.success(), "git {args:?}");
    }

    #[test]
    fn opens_worktree_on_card_branch() {
        let tmp = std::env::temp_dir().join(format!("jc-wt-{}", jc_core::ulid::uuid4()));
        let src = tmp.join("src-repo");
        std::fs::create_dir_all(&src).unwrap();
        sh(&src, &["init", "-q", "-b", "main"]);
        std::fs::write(src.join("a.txt"), "a").unwrap();
        sh(&src, &["add", "."]);
        sh(&src, &["commit", "-qm", "init"]);

        let root = tmp.join("ws");
        std::fs::create_dir_all(root.join("work/p/c1")).unwrap();
        std::fs::write(
            root.join("work/p/project.toml"),
            format!(
                "name = \"p\"\n[[repos]]\nname = \"app\"\npath = {}\n",
                toml::Value::String(src.display().to_string())
            ),
        )
        .unwrap();
        assert_eq!(project_repos(&root, "p").len(), 1);
        let wt = ensure_one(&root, "p", "work/p/c1", "app", "", "").unwrap();
        assert!(wt.join("a.txt").exists());
        let head = git(&wt, &["rev-parse", "--abbrev-ref", "HEAD"]).unwrap();
        assert_eq!(head, "jc/c1");
        // 두 번째는 그대로 둔다.
        ensure_one(&root, "p", "work/p/c1", "app", "", "").unwrap();
        let _ = std::fs::remove_dir_all(&tmp);
    }
}
