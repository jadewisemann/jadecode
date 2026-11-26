//! 메모 작업용 git 워크트리.
//!
//! 메모마다 브랜치 `jc/<메모 이름>`과 워크트리 `<ws>/.jadecode/<project>/wt/<메모 이름>`을 연다.
//! 사람의 작업 폴더(원본 레포)는 건드리지 않는다.

use anyhow::{bail, Context, Result};
use std::path::{Path, PathBuf};
use std::process::Command;

pub fn is_git_repo(p: &Path) -> bool {
    p.join(".git").exists()
}

pub fn git(dir: &Path, args: &[&str]) -> Result<String> {
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

pub fn branch_for(stem: &str) -> String {
    format!("jc/{stem}")
}

/// 워크트리를 연다(이미 있으면 그대로 쓴다). 브랜치가 이미 있으면 그 브랜치를 붙인다.
pub fn ensure(repo: &Path, wt: &Path, branch: &str, base: &str) -> Result<PathBuf> {
    if is_git_repo(wt) {
        return Ok(wt.to_path_buf());
    }
    if !is_git_repo(repo) {
        bail!("git 레포가 아니다: {}", repo.display());
    }
    if wt.exists()
        && std::fs::read_dir(wt)
            .map(|mut d| d.next().is_some())
            .unwrap_or(false)
    {
        bail!("{}가 이미 있고 비어 있지 않다", wt.display());
    }
    if let Some(parent) = wt.parent() {
        std::fs::create_dir_all(parent)?;
    }
    // 지워진 워크트리의 흔적이 남아 있으면 add가 거절하므로 먼저 정리한다.
    let _ = git(repo, &["worktree", "prune"]);
    let wt_s = wt.display().to_string();
    let exists = git(
        repo,
        &[
            "rev-parse",
            "--verify",
            "--quiet",
            &format!("refs/heads/{branch}"),
        ],
    )
    .is_ok();
    if exists {
        git(repo, &["worktree", "add", &wt_s, branch])?;
    } else if base.trim().is_empty() {
        git(repo, &["worktree", "add", "-b", branch, &wt_s])?;
    } else {
        git(repo, &["worktree", "add", "-b", branch, &wt_s, base.trim()])?;
    }
    Ok(wt.to_path_buf())
}

/// 커밋하지 않은 변경이 있는가.
pub fn is_dirty(wt: &Path) -> bool {
    git(wt, &["status", "--porcelain"])
        .map(|s| !s.is_empty())
        .unwrap_or(false)
}

/// 기준 대비 바뀐 파일 수·추가·삭제 줄 수(커밋한 것과 안 한 것 모두).
pub fn diff_stat(wt: &Path, base: &str) -> Option<(usize, usize, usize)> {
    if !is_git_repo(wt) {
        return None;
    }
    let base = if base.trim().is_empty() {
        // 브랜치가 갈라진 지점. 실패하면 HEAD.
        git(wt, &["merge-base", "HEAD", "@{upstream}"])
            .or_else(|_| default_base(wt))
            .unwrap_or_else(|_| "HEAD".into())
    } else {
        base.trim().to_string()
    };
    let out = git(wt, &["diff", "--numstat", &base]).ok()?;
    let mut files = 0;
    let mut add = 0;
    let mut del = 0;
    for l in out.lines() {
        let mut it = l.split('\t');
        let a = it.next().unwrap_or("0").parse::<usize>().unwrap_or(0);
        let d = it.next().unwrap_or("0").parse::<usize>().unwrap_or(0);
        files += 1;
        add += a;
        del += d;
    }
    // 아직 git이 모르는 새 파일.
    if let Ok(u) = git(wt, &["ls-files", "--others", "--exclude-standard"]) {
        files += u.lines().filter(|l| !l.is_empty()).count();
    }
    Some((files, add, del))
}

/// 레포의 기본 브랜치(main/master)와 HEAD가 갈라진 지점.
fn default_base(wt: &Path) -> Result<String> {
    for b in ["main", "master"] {
        if let Ok(mb) = git(wt, &["merge-base", "HEAD", b]) {
            return Ok(mb);
        }
    }
    bail!("기본 브랜치를 찾지 못했다")
}

/// 워크트리를 지운다. 변경이 남아 있으면 지우지 않고 `false`. 브랜치는 남긴다.
pub fn remove_if_clean(repo: &Path, wt: &Path) -> Result<bool> {
    if !wt.exists() {
        return Ok(true);
    }
    if is_dirty(wt) {
        return Ok(false);
    }
    git(repo, &["worktree", "remove", &wt.display().to_string()])?;
    Ok(true)
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;

    pub fn sh(dir: &Path, args: &[&str]) {
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

    pub fn repo(dir: &Path) {
        std::fs::create_dir_all(dir).unwrap();
        sh(dir, &["init", "-q", "-b", "main"]);
        std::fs::write(dir.join("a.txt"), "a\n").unwrap();
        sh(dir, &["add", "."]);
        sh(dir, &["commit", "-qm", "init"]);
    }

    #[test]
    fn worktree_lifecycle() {
        let tmp = crate::board::tests::tmpdir("wt");
        let src = tmp.join("src");
        repo(&src);
        let wt = tmp.join("state/wt/fix");
        ensure(&src, &wt, "jc/fix", "").unwrap();
        assert!(wt.join("a.txt").exists());
        assert_eq!(
            git(&wt, &["rev-parse", "--abbrev-ref", "HEAD"]).unwrap(),
            "jc/fix"
        );
        ensure(&src, &wt, "jc/fix", "").unwrap();
        std::fs::write(wt.join("a.txt"), "a\nb\n").unwrap();
        std::fs::write(wt.join("new.txt"), "n\n").unwrap();
        assert_eq!(diff_stat(&wt, "").unwrap(), (2, 1, 0));
        assert!(!remove_if_clean(&src, &wt).unwrap());
        sh(&wt, &["add", "."]);
        sh(&wt, &["commit", "-qm", "work"]);
        assert_eq!(diff_stat(&wt, "").unwrap().0, 2);
        assert!(remove_if_clean(&src, &wt).unwrap());
        assert!(!wt.exists());
        // 브랜치는 남고, 다시 열면 그 브랜치에 붙는다.
        ensure(&src, &wt, "jc/fix", "").unwrap();
        assert!(wt.join("new.txt").exists());
        let _ = std::fs::remove_dir_all(&tmp);
    }
}
