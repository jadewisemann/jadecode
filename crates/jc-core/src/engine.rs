//! 앱의 상태와 동작. 화면(GPUI)과 떨어져 있어서 그대로 시험할 수 있다.
//!
//! - 보드는 파일에서 읽는다. 사람이 Finder나 편집기로 메모를 고치거나 옮겨도 다음 훑기에 반영된다.
//! - 메모가 `doing/`에 들어왔는데 아직 시작한 적이 없으면 워크트리를 열고 첫 턴을 띄운다.
//! - 메모 작업의 턴이 끝나면 메모를 `review/`로 옮기고, 그 소식을 봇의 다음 턴 앞에 붙인다.
//! - 실행은 백그라운드 스레드에서 하고, 이벤트는 채널로 받아 `tick`에서 반영한다.

use crate::agent::{self, Event, Running, TurnSpec};
use crate::board::{self, Column, Memo};
use crate::log::{self, Entry, Kind};
use crate::workspace::{Project, Workspace};
use crate::worktree;
use anyhow::{bail, Context, Result};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{channel, Receiver, Sender};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

/// 대화 하나: 프로젝트 봇, 또는 메모 작업.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum Key {
    Bot(String),
    Memo(String, String),
}

impl Key {
    pub fn project(&self) -> &str {
        match self {
            Key::Bot(p) | Key::Memo(p, _) => p,
        }
    }
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
struct Session {
    session_id: String,
}

/// 앱이 사람에게 알릴 일(데스크톱 알림).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Notice {
    pub title: String,
    pub body: String,
}

struct Run {
    handle: Arc<Mutex<Option<Running>>>,
    cancelled: Arc<AtomicBool>,
}

impl Run {
    fn stop(&self) {
        self.cancelled.store(true, Ordering::SeqCst);
        if let Some(r) = self
            .handle
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .as_ref()
        {
            r.stop();
        }
    }
}

pub const BOT_TOOLS: &[&str] = &[
    "Read",
    "Glob",
    "Grep",
    "LS",
    "WebSearch",
    "WebFetch",
    "TodoWrite",
    "Bash(ls:*)",
    "Bash(cat:*)",
    "Bash(mv:*)",
    "Bash(mkdir:*)",
    "Bash(git log:*)",
    "Bash(git status:*)",
    "Bash(git diff:*)",
    "Bash(git show:*)",
    "Bash(git branch:*)",
];

fn bot_instructions(p: &Project) -> String {
    let repo = p
        .repo()
        .map(|r| r.display().to_string())
        .unwrap_or_else(|| "(설정되지 않음)".into());
    format!(
        r#"# jadecode 프로젝트 봇

너는 프로젝트 「{name}」을 맡은 봇이다. 사람과 대화하며 할 일을 정리하고, 코드 작업은 직접 하지 않고 칸반 메모로 위임한다.

## 보드
- 지금 폴더가 이 프로젝트의 보드다. `todo/` `doing/` `review/` `done/` 폴더가 칸반 열이고, 그 안의 `.md` 파일 하나가 작업 하나다.
- 메모 파일의 내용이 그 작업을 맡는 코딩 에이전트가 받는 맥락의 전부다. 앞머리나 메타데이터는 쓰지 않는다.
- 위임할 때는 `todo/<짧은-영문-이름>.md`를 쓴다. 첫 줄은 `# 제목`이고, 목표·완료 조건·관련 파일·주의점만 짧게 적는다.
- 사람이 바로 시작하라고 하면 `doing/`에 쓰거나 `mv todo/x.md doing/`으로 옮긴다. `doing/`에 들어간 메모는 앱이 브랜치 `jc/<이름>`과 워크트리를 만들고 코딩 에이전트에게 맡긴다.
- `review/`는 에이전트가 한 차례 마치고 사람 확인을 기다리는 메모, `done/`은 끝난 메모다. 끝난 일을 정리할 때만 옮긴다.
- 메시지 앞의 [보드 소식]은 앱이 붙인 것이다. 사람에게 필요한 것만 짧게 전한다.

## 코드 레포
- {repo}
- 레포는 읽기만 한다. 보드 폴더 밖의 파일은 고치지 않는다.
"#,
        name = p.name(),
        repo = repo
    )
}

fn worker_instructions(branch: Option<&str>) -> String {
    let where_ = match branch {
        Some(b) => format!("지금 폴더는 브랜치 `{b}`의 전용 git 워크트리다. 사람의 작업 폴더가 아니므로 마음껏 고쳐도 된다."),
        None => "지금 폴더가 작업 폴더다.".to_string(),
    };
    format!(
        r#"# jadecode 메모 작업

첫 메시지는 칸반 메모 파일의 내용이고, 그것이 이 작업의 맥락이다.
- {where_}
- 작업을 마치면 변경을 커밋하고, 무엇을 했는지와 사람이 확인할 점을 짧게 정리해 답한다.
- 사람이 따로 말하지 않으면 기본 브랜치에 병합하거나 push하지 않는다.
- 막히면 추측으로 진행하지 말고 무엇이 필요한지 묻고 턴을 끝낸다.
"#
    )
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

pub fn turn_summary(ok: bool, duration_ms: u64, cost_usd: f64) -> String {
    let secs = duration_ms / 1000;
    let t = if secs >= 60 {
        format!("{}분 {}초", secs / 60, secs % 60)
    } else {
        format!("{secs}초")
    };
    let head = if ok { "턴 끝" } else { "오류로 끝" };
    if cost_usd > 0.0 {
        format!("{head} · {t} · ${cost_usd:.2}")
    } else {
        format!("{head} · {t}")
    }
}

pub struct Engine {
    pub ws: Workspace,
    pub claude_bin: String,
    projects: Vec<Project>,
    memos: HashMap<String, Vec<Memo>>,
    runs: HashMap<Key, Run>,
    tx: Sender<(Key, Event)>,
    rx: Receiver<(Key, Event)>,
    logs: HashMap<Key, Vec<Entry>>,
    diffs: HashMap<Key, Option<(usize, usize, usize)>>,
    notices: Vec<Notice>,
    last_scan: Instant,
    /// 화면이 다시 그릴지 정한다. 무언가 바뀔 때마다 오른다.
    pub version: u64,
}

impl Engine {
    pub fn new(ws: Workspace) -> Self {
        let (tx, rx) = channel();
        let mut e = Self {
            ws,
            claude_bin: std::env::var("JADECODE_CLAUDE")
                .ok()
                .filter(|s| !s.is_empty())
                .unwrap_or_else(|| "claude".into()),
            projects: vec![],
            memos: HashMap::new(),
            runs: HashMap::new(),
            tx,
            rx,
            logs: HashMap::new(),
            diffs: HashMap::new(),
            notices: vec![],
            last_scan: Instant::now(),
            version: 1,
        };
        e.scan();
        e.recover_interrupted();
        e.autostart();
        e
    }

    // ---------- 경로 ----------

    fn session_path(&self, k: &Key) -> PathBuf {
        let d = self.ws.state_dir(k.project());
        match k {
            Key::Bot(_) => d.join("bot/session.json"),
            Key::Memo(_, s) => d.join(format!("memos/{s}.json")),
        }
    }

    pub fn log_path(&self, k: &Key) -> PathBuf {
        let d = self.ws.state_dir(k.project());
        match k {
            Key::Bot(_) => d.join("bot/log.jsonl"),
            Key::Memo(_, s) => d.join(format!("memos/{s}.jsonl")),
        }
    }

    fn notes_path(&self, project: &str) -> PathBuf {
        self.ws.state_dir(project).join("bot/notes.json")
    }

    pub fn worktree_path(&self, project: &str, stem: &str) -> PathBuf {
        self.ws.state_dir(project).join("wt").join(stem)
    }

    fn session(&self, k: &Key) -> Option<Session> {
        let t = std::fs::read_to_string(self.session_path(k)).ok()?;
        serde_json::from_str::<Session>(&t)
            .ok()
            .filter(|s| !s.session_id.is_empty())
    }

    fn save_session(&self, k: &Key, s: &Session) -> Result<()> {
        crate::write_atomic(&self.session_path(k), serde_json::to_string(s)?.as_bytes())?;
        Ok(())
    }

    pub fn has_session(&self, k: &Key) -> bool {
        self.session(k).is_some()
    }

    // ---------- 읽기 ----------

    pub fn projects(&self) -> &[Project] {
        &self.projects
    }

    pub fn project(&self, slug: &str) -> Option<&Project> {
        self.projects.iter().find(|p| p.slug == slug)
    }

    pub fn memos(&self, slug: &str) -> &[Memo] {
        self.memos.get(slug).map(|v| v.as_slice()).unwrap_or(&[])
    }

    pub fn memo(&self, slug: &str, stem: &str) -> Option<&Memo> {
        self.memos(slug).iter().find(|m| m.stem == stem)
    }

    pub fn is_running(&self, k: &Key) -> bool {
        self.runs.contains_key(k)
    }

    /// 대화 기록(처음 부를 때 파일에서 읽고, 이후에는 메모리에서 이어 붙인다).
    pub fn log(&mut self, k: &Key) -> &[Entry] {
        if !self.logs.contains_key(k) {
            let v = log::read(&self.log_path(k));
            self.logs.insert(k.clone(), v);
        }
        self.logs.get(k).map(|v| v.as_slice()).unwrap_or(&[])
    }

    /// 메모 작업의 마지막 에이전트 말(타일 미리보기).
    pub fn last_reply(&mut self, k: &Key) -> Option<String> {
        self.log(k)
            .iter()
            .rev()
            .find(|e| matches!(e.kind, Kind::Assistant | Kind::Error))
            .map(|e| short(&e.text, 140))
    }

    pub fn branch(&self, project: &str, stem: &str) -> Option<String> {
        let p = self.project(project)?;
        let repo = p.repo()?;
        worktree::is_git_repo(&repo).then(|| worktree::branch_for(stem))
    }

    /// 메모 작업이 일하는 폴더(아직 없을 수도 있다).
    pub fn work_dir(&self, project: &str, stem: &str) -> Option<PathBuf> {
        let p = self.project(project)?;
        let repo = p.repo()?;
        if worktree::is_git_repo(&repo) {
            Some(self.worktree_path(project, stem))
        } else {
            Some(repo)
        }
    }

    pub fn diff(&self, k: &Key) -> Option<(usize, usize, usize)> {
        self.diffs.get(k).copied().flatten()
    }

    /// 워크트리의 변경 규모를 다시 잰다(git을 부르므로 매 프레임 부르지 않는다).
    pub fn refresh_diff(&mut self, k: &Key) {
        let Key::Memo(p, s) = k else { return };
        let base = self
            .project(p)
            .map(|p| p.file.base.clone())
            .unwrap_or_default();
        let d = self
            .work_dir(p, s)
            .filter(|d| d.exists() && *d == self.worktree_path(p, s))
            .and_then(|d| worktree::diff_stat(&d, &base));
        if self.diffs.get(k) != Some(&d) {
            self.diffs.insert(k.clone(), d);
            self.version += 1;
        }
    }

    pub fn take_notices(&mut self) -> Vec<Notice> {
        std::mem::take(&mut self.notices)
    }

    // ---------- 훑기 ----------

    /// 파일에서 프로젝트·메모를 다시 읽는다. 바뀌었으면 `version`을 올린다.
    pub fn scan(&mut self) {
        self.last_scan = Instant::now();
        let projects = self.ws.projects();
        let mut memos = HashMap::new();
        for p in &projects {
            memos.insert(p.slug.clone(), board::scan(p));
        }
        if projects != self.projects || memos != self.memos {
            self.projects = projects;
            self.memos = memos;
            self.version += 1;
        }
    }

    /// 앱이 꺼진 사이 끊긴 작업: `doing/`에 있고 시작한 적은 있는데 지금 돌지 않는 메모.
    fn recover_interrupted(&mut self) {
        let mut stuck = vec![];
        for p in &self.projects {
            for m in self.memos(&p.slug) {
                let k = Key::Memo(p.slug.clone(), m.stem.clone());
                if m.column == Column::Doing && self.has_session(&k) && !self.is_running(&k) {
                    stuck.push(k);
                }
            }
        }
        for k in stuck {
            self.push_log(
                &k,
                Entry::new(
                    Kind::Note,
                    "앱이 다시 시작되어 작업이 멈췄습니다. 말을 걸면 이어서 합니다.",
                ),
            );
            if let Key::Memo(p, s) = &k {
                self.move_quiet(p, s, Column::Review);
            }
        }
    }

    /// `doing/`에 새로 들어온 메모를 시작한다.
    fn autostart(&mut self) {
        let mut todo = vec![];
        for p in &self.projects {
            for m in self.memos(&p.slug) {
                let k = Key::Memo(p.slug.clone(), m.stem.clone());
                if m.column == Column::Doing && !self.is_running(&k) && !self.has_session(&k) {
                    todo.push(k);
                }
            }
        }
        for k in todo {
            if let Err(e) = self.run_memo(&k, None) {
                self.fail_memo(&k, &format!("{e:#}"));
            }
        }
    }

    /// 주기 작업. 화면이 100ms쯤마다 부른다. 바뀐 것이 있으면 참.
    pub fn tick(&mut self) -> bool {
        let before = self.version;
        while let Ok((k, ev)) = self.rx.try_recv() {
            self.on_event(k, ev);
        }
        if self.last_scan.elapsed() >= Duration::from_millis(800) {
            self.scan();
            self.autostart();
        }
        self.version != before
    }

    fn push_log(&mut self, k: &Key, e: Entry) {
        let _ = log::append(&self.log_path(k), &e);
        if let Some(v) = self.logs.get_mut(k) {
            v.push(e);
        }
        self.version += 1;
    }

    fn on_event(&mut self, k: Key, ev: Event) {
        match ev {
            Event::Text(t) => self.push_log(&k, Entry::new(Kind::Assistant, t)),
            Event::Tool(t) => self.push_log(&k, Entry::new(Kind::Tool, t)),
            Event::Finished {
                ok,
                text,
                cost_usd,
                duration_ms,
            } => {
                if !ok && !text.trim().is_empty() {
                    self.push_log(&k, Entry::new(Kind::Error, text));
                }
                self.push_log(
                    &k,
                    Entry::new(Kind::Turn, turn_summary(ok, duration_ms, cost_usd)),
                );
                self.turn_done(&k);
            }
            Event::Failed(msg) => {
                let cancelled = self
                    .runs
                    .get(&k)
                    .is_some_and(|r| r.cancelled.load(Ordering::SeqCst));
                if cancelled {
                    self.push_log(&k, Entry::new(Kind::Note, "멈췄습니다."));
                } else {
                    self.push_log(&k, Entry::new(Kind::Error, msg));
                }
                self.turn_done(&k);
            }
        }
    }

    /// 턴이 끝났다: 메모는 `review/`로, 봇에게 소식을 남긴다.
    fn turn_done(&mut self, k: &Key) {
        self.runs.remove(k);
        self.version += 1;
        let Key::Memo(p, s) = k else { return };
        let title = self
            .memo(p, s)
            .map(|m| m.title.clone())
            .unwrap_or_else(|| s.clone());
        let in_doing = self.memo(p, s).is_some_and(|m| m.column == Column::Doing);
        if in_doing {
            self.move_quiet(p, s, Column::Review);
        }
        self.refresh_diff(k);
        let last = self.last_reply(k).unwrap_or_default();
        self.add_note(
            p,
            format!(
                "「{title}」(review/{s}.md) 에이전트가 한 차례 마쳤습니다: {}",
                short(&last, 200)
            ),
        );
        let pname = self
            .project(p)
            .map(|x| x.name().to_string())
            .unwrap_or_default();
        self.notices.push(Notice {
            title: format!("{pname} · 확인 필요"),
            body: title,
        });
    }

    fn fail_memo(&mut self, k: &Key, msg: &str) {
        self.push_log(k, Entry::new(Kind::Error, msg));
        if let Key::Memo(p, s) = k {
            self.move_quiet(p, s, Column::Review);
        }
    }

    fn move_quiet(&mut self, project: &str, stem: &str, to: Column) {
        if let Some(p) = self.project(project).cloned() {
            if board::move_to(&p, stem, to).is_ok() {
                self.scan();
            }
        }
    }

    // ---------- 봇 소식 ----------

    fn notes(&self, project: &str) -> Vec<String> {
        std::fs::read_to_string(self.notes_path(project))
            .ok()
            .and_then(|t| serde_json::from_str(&t).ok())
            .unwrap_or_default()
    }

    fn add_note(&mut self, project: &str, note: String) {
        let mut v = self.notes(project);
        v.push(note.clone());
        // 오래 쌓이면 최근 것만 남긴다.
        if v.len() > 20 {
            v.drain(..v.len() - 20);
        }
        if let Ok(s) = serde_json::to_string(&v) {
            let _ = crate::write_atomic(&self.notes_path(project), s.as_bytes());
        }
        self.push_log(&Key::Bot(project.to_string()), Entry::new(Kind::Note, note));
    }

    // ---------- 동작 ----------

    pub fn create_project(&mut self, name: &str, repo: &str) -> Result<Project> {
        let p = self.ws.create_project(name, repo)?;
        self.scan();
        Ok(p)
    }

    pub fn create_memo(
        &mut self,
        project: &str,
        title: &str,
        body: &str,
        col: Column,
    ) -> Result<Memo> {
        let p = self.project(project).cloned().context("프로젝트가 없다")?;
        let m = board::create(&p, title, body, col)?;
        self.scan();
        if col == Column::Doing {
            self.autostart();
        }
        Ok(m)
    }

    /// 메모를 다른 열로 옮긴다. `doing/`으로 가면 시작하고, 다른 열로 가면 돌던 작업을 멈춘다.
    /// `done/`으로 가면 변경이 없는 워크트리를 정리한다(브랜치는 남는다).
    pub fn move_memo(&mut self, project: &str, stem: &str, to: Column) -> Result<()> {
        let p = self.project(project).cloned().context("프로젝트가 없다")?;
        let k = Key::Memo(project.to_string(), stem.to_string());
        if to != Column::Doing {
            if let Some(r) = self.runs.get(&k) {
                r.stop();
            }
        }
        board::move_to(&p, stem, to)?;
        self.scan();
        if to == Column::Done {
            if let Some(repo) = p.repo().filter(|r| worktree::is_git_repo(r)) {
                let wt = self.worktree_path(project, stem);
                match worktree::remove_if_clean(&repo, &wt) {
                    Ok(true) if !wt.exists() => {}
                    Ok(false) => self.push_log(
                        &k,
                        Entry::new(
                            Kind::Note,
                            "커밋하지 않은 변경이 있어 워크트리를 남겨 두었습니다.",
                        ),
                    ),
                    Err(e) => self.push_log(
                        &k,
                        Entry::new(Kind::Note, format!("워크트리를 정리하지 못했습니다: {e:#}")),
                    ),
                    _ => {}
                }
            }
        }
        if to == Column::Doing && !self.is_running(&k) && !self.has_session(&k) {
            self.run_memo(&k, None)?;
        }
        Ok(())
    }

    pub fn stop(&mut self, k: &Key) {
        if let Some(r) = self.runs.get(k) {
            r.stop();
        }
    }

    pub fn stop_all(&mut self) {
        for r in self.runs.values() {
            r.stop();
        }
    }

    /// 봇에게 말한다. 쌓인 보드 소식을 앞에 붙인다.
    pub fn send_bot(&mut self, project: &str, text: &str) -> Result<()> {
        let text = text.trim();
        if text.is_empty() {
            return Ok(());
        }
        let k = Key::Bot(project.to_string());
        if self.is_running(&k) {
            bail!("봇이 아직 답하는 중입니다");
        }
        let p = self.project(project).cloned().context("프로젝트가 없다")?;
        let notes = self.notes(project);
        let mut prompt = String::new();
        if !notes.is_empty() {
            prompt.push_str("[보드 소식]\n");
            for n in &notes {
                prompt.push_str(&format!("- {n}\n"));
            }
            prompt.push('\n');
        }
        prompt.push_str(text);
        let (session, resume) = match self.session(&k) {
            Some(s) => (s, true),
            None => {
                let s = Session {
                    session_id: crate::uuid4(),
                };
                self.save_session(&k, &s)?;
                (s, false)
            }
        };
        let spec = TurnSpec {
            bin: self.claude_bin.clone(),
            cwd: p.dir.clone(),
            session_id: session.session_id,
            resume,
            prompt,
            append_system: bot_instructions(&p),
            permission_mode: "acceptEdits".into(),
            allowed_tools: BOT_TOOLS.iter().map(|s| s.to_string()).collect(),
            disallowed_tools: vec![],
            model: p.file.model.clone(),
        };
        self.push_log(&k, Entry::new(Kind::User, text));
        let _ = std::fs::remove_file(self.notes_path(project));
        self.launch(k, spec, None);
        Ok(())
    }

    /// 메모 작업에 말한다. 메모를 `doing/`으로 옮기고 턴을 띄운다.
    pub fn send_memo(&mut self, project: &str, stem: &str, text: &str) -> Result<()> {
        let text = text.trim();
        if text.is_empty() {
            return Ok(());
        }
        let k = Key::Memo(project.to_string(), stem.to_string());
        if self.is_running(&k) {
            bail!("에이전트가 아직 일하는 중입니다");
        }
        let p = self.project(project).cloned().context("프로젝트가 없다")?;
        if self
            .memo(project, stem)
            .is_some_and(|m| m.column != Column::Doing)
        {
            board::move_to(&p, stem, Column::Doing)?;
            self.scan();
        }
        self.run_memo(&k, Some(text))
    }

    /// 메모 작업의 턴을 띄운다. 첫 턴이면 메모 내용이 프롬프트다.
    fn run_memo(&mut self, k: &Key, user_text: Option<&str>) -> Result<()> {
        let Key::Memo(project, stem) = k else {
            bail!("메모가 아니다")
        };
        let p = self.project(project).cloned().context("프로젝트가 없다")?;
        let memo = self
            .memo(project, stem)
            .cloned()
            .context("메모를 찾을 수 없다")?;
        let Some(repo) = p.repo() else {
            bail!(
                "프로젝트에 코드 레포가 없습니다. {}/project.toml의 repo를 채워 주세요",
                p.slug
            )
        };
        if !repo.is_dir() {
            bail!("코드 레포 폴더가 없습니다: {}", repo.display());
        }
        let git = worktree::is_git_repo(&repo);
        let branch = git.then(|| worktree::branch_for(stem));
        let cwd = if git {
            self.worktree_path(project, stem)
        } else {
            repo.clone()
        };
        let (session, resume) = match self.session(k) {
            Some(s) => (s, true),
            None => {
                let s = Session {
                    session_id: crate::uuid4(),
                };
                self.save_session(k, &s)?;
                (s, false)
            }
        };
        let prompt = if resume {
            user_text.unwrap_or("이어서 진행해 주세요.").to_string()
        } else {
            match user_text {
                Some(t) => format!("{}\n\n---\n\n{t}", memo.text.trim()),
                None => memo.text.trim().to_string(),
            }
        };
        let spec = TurnSpec {
            bin: self.claude_bin.clone(),
            cwd: cwd.clone(),
            session_id: session.session_id,
            resume,
            prompt,
            append_system: worker_instructions(branch.as_deref()),
            permission_mode: if p.file.permission_mode.is_empty() {
                "bypassPermissions".into()
            } else {
                p.file.permission_mode.clone()
            },
            allowed_tools: vec![],
            disallowed_tools: vec![],
            model: p.file.model.clone(),
        };
        match user_text {
            Some(t) => self.push_log(k, Entry::new(Kind::User, t)),
            None => {
                let where_ = branch
                    .as_deref()
                    .map(|b| format!("브랜치 {b}에서"))
                    .unwrap_or_else(|| "레포 폴더에서".into());
                self.push_log(
                    k,
                    Entry::new(
                        Kind::Note,
                        format!("메모를 맥락으로 {where_} 작업을 시작합니다."),
                    ),
                )
            }
        }
        let prep = branch.map(|b| (repo, cwd, b, p.file.base.clone()));
        self.launch(k.clone(), spec, prep);
        Ok(())
    }

    /// 백그라운드에서 (필요하면 워크트리를 열고) 턴을 띄운다.
    fn launch(
        &mut self,
        k: Key,
        spec: TurnSpec,
        worktree: Option<(PathBuf, PathBuf, String, String)>,
    ) {
        let run = Run {
            handle: Arc::new(Mutex::new(None)),
            cancelled: Arc::new(AtomicBool::new(false)),
        };
        let handle = run.handle.clone();
        let cancelled = run.cancelled.clone();
        self.runs.insert(k.clone(), run);
        self.version += 1;
        let tx = self.tx.clone();
        std::thread::spawn(move || {
            if let Some((repo, wt, branch, base)) = worktree {
                if let Err(e) = worktree::ensure(&repo, &wt, &branch, &base) {
                    let _ = tx.send((
                        k,
                        Event::Failed(format!("워크트리를 열지 못했습니다: {e:#}")),
                    ));
                    return;
                }
            }
            if cancelled.load(Ordering::SeqCst) {
                let _ = tx.send((k, Event::Failed("멈췄습니다".into())));
                return;
            }
            let key = k.clone();
            let etx = tx.clone();
            match agent::spawn(spec, move |ev| {
                let _ = etx.send((key.clone(), ev));
            }) {
                Ok(r) => {
                    let mut h = handle.lock().unwrap_or_else(|e| e.into_inner());
                    if cancelled.load(Ordering::SeqCst) {
                        r.stop();
                    }
                    *h = Some(r);
                }
                Err(e) => {
                    let _ = tx.send((k, Event::Failed(format!("{e:#}"))));
                }
            }
        });
    }
}

impl Drop for Engine {
    fn drop(&mut self) {
        self.stop_all();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::board::tests::tmpdir;

    /// 진짜 claude 대신 쓰는 가짜: 받은 프롬프트를 파일에 남기고 stream-json 두 줄을 낸다.
    fn fake_claude(dir: &std::path::Path) -> String {
        let p = dir.join("fake-claude.sh");
        std::fs::write(
            &p,
            r#"#!/bin/sh
cat >> "$FAKE_LOG"
printf '\n=== %s\n' "$*" >> "$FAKE_LOG"
pwd >> "$FAKE_LOG"
echo '{"type":"system","subtype":"init"}'
echo '{"type":"assistant","message":{"content":[{"type":"text","text":"고쳤습니다"},{"type":"tool_use","name":"Edit","input":{"file_path":"a.txt"}}]}}'
echo '{"type":"result","subtype":"success","is_error":false,"result":"고쳤습니다","total_cost_usd":0.01,"duration_ms":1200}'
"#,
        )
        .unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&p, std::fs::Permissions::from_mode(0o755)).unwrap();
        }
        p.display().to_string()
    }

    fn wait_idle(e: &mut Engine, k: &Key) {
        for _ in 0..200 {
            e.tick();
            if !e.is_running(k) {
                return;
            }
            std::thread::sleep(Duration::from_millis(25));
        }
        panic!("턴이 끝나지 않았다");
    }

    #[test]
    fn memo_runs_in_worktree_then_waits_for_review_and_bot_hears_about_it() {
        let tmp = tmpdir("engine");
        let repo = tmp.join("repo");
        crate::worktree::tests::repo(&repo);
        let fake_log = tmp.join("fake.log");
        std::env::set_var("FAKE_LOG", &fake_log);
        let ws = Workspace::open(&tmp.join("ws")).unwrap();
        let mut e = Engine::new(ws);
        e.claude_bin = fake_claude(&tmp);
        e.create_project("demo", &repo.display().to_string())
            .unwrap();

        // 할 일에 쓴 메모는 저절로 시작하지 않는다.
        e.create_memo("demo", "Fix login", "사파리 버튼", Column::Todo)
            .unwrap();
        e.tick();
        let k = Key::Memo("demo".into(), "fix-login".into());
        assert!(!e.is_running(&k));

        // 진행 중으로 옮기면 워크트리를 열고 메모 내용으로 첫 턴을 띄운다.
        e.move_memo("demo", "fix-login", Column::Doing).unwrap();
        assert!(e.is_running(&k));
        wait_idle(&mut e, &k);
        let wt = e.worktree_path("demo", "fix-login");
        assert!(wt.join("a.txt").exists());
        let logged = std::fs::read_to_string(&fake_log).unwrap();
        assert!(logged.contains("# Fix login"));
        assert!(logged.contains("--session-id"));
        assert!(logged.contains("bypassPermissions"));
        assert!(logged.contains(&wt.display().to_string()) || logged.contains("fix-login"));

        // 끝나면 확인 필요로 가고, 기록이 남는다.
        assert_eq!(e.memo("demo", "fix-login").unwrap().column, Column::Review);
        let kinds: Vec<Kind> = e.log(&k).iter().map(|x| x.kind).collect();
        assert_eq!(
            kinds,
            vec![Kind::Note, Kind::Assistant, Kind::Tool, Kind::Turn]
        );
        assert_eq!(e.last_reply(&k).as_deref(), Some("고쳤습니다"));
        assert_eq!(e.take_notices().len(), 1);

        // 메모에 말을 걸면 진행 중으로 돌아가 같은 세션을 잇는다.
        e.send_memo("demo", "fix-login", "테스트도 추가해 주세요")
            .unwrap();
        assert_eq!(e.memo("demo", "fix-login").unwrap().column, Column::Doing);
        wait_idle(&mut e, &k);
        let logged = std::fs::read_to_string(&fake_log).unwrap();
        assert!(logged.contains("--resume"));
        assert!(logged.contains("테스트도 추가해 주세요"));

        // 봇의 다음 턴 앞에 보드 소식이 붙고, 보낸 뒤에는 비워진다.
        let bot = Key::Bot("demo".into());
        e.send_bot("demo", "상태 어때?").unwrap();
        wait_idle(&mut e, &bot);
        let logged = std::fs::read_to_string(&fake_log).unwrap();
        assert!(logged.contains("[보드 소식]\n- 「Fix login」"));
        assert!(logged.contains("acceptEdits"));
        std::fs::write(&fake_log, "").unwrap();
        e.send_bot("demo", "고마워").unwrap();
        wait_idle(&mut e, &bot);
        let logged = std::fs::read_to_string(&fake_log).unwrap();
        assert!(!logged.contains("[보드 소식]\n-"));
        assert!(logged.contains("--resume"));

        // 완료로 옮기면 깨끗한 워크트리를 정리한다(가짜는 아무것도 안 바꿨다).
        e.move_memo("demo", "fix-login", Column::Done).unwrap();
        assert!(!wt.exists());
        let _ = std::fs::remove_dir_all(&tmp);
    }

    #[test]
    fn memo_written_into_doing_from_outside_starts_and_missing_claude_fails_cleanly() {
        let tmp = tmpdir("engine2");
        let repo = tmp.join("repo");
        crate::worktree::tests::repo(&repo);
        let ws = Workspace::open(&tmp.join("ws")).unwrap();
        let mut e = Engine::new(ws);
        e.claude_bin = tmp.join("no-such-claude").display().to_string();
        let p = e
            .create_project("demo", &repo.display().to_string())
            .unwrap();
        // 봇이 파일을 직접 쓴 것처럼.
        std::fs::write(p.dir.join("doing/add-tests.md"), "# 테스트 추가\n").unwrap();
        e.last_scan = Instant::now() - Duration::from_secs(5);
        e.tick();
        let k = Key::Memo("demo".into(), "add-tests".into());
        assert!(e.is_running(&k));
        wait_idle(&mut e, &k);
        assert_eq!(e.memo("demo", "add-tests").unwrap().column, Column::Review);
        assert!(e.log(&k).iter().any(|x| x.kind == Kind::Error));
        // 이미 한 번 시작했으므로 다시 진행 중으로 옮겨도 저절로 다시 돌지 않는다(말을 걸어야 한다).
        e.move_memo("demo", "add-tests", Column::Doing).unwrap();
        assert!(!e.is_running(&k));
        let _ = std::fs::remove_dir_all(&tmp);
    }

    #[test]
    fn memo_without_repo_reports_why() {
        let tmp = tmpdir("engine3");
        let ws = Workspace::open(&tmp).unwrap();
        let mut e = Engine::new(ws);
        e.create_project("norepo", "").unwrap();
        e.create_memo("norepo", "x", "y", Column::Doing).unwrap();
        let k = Key::Memo("norepo".into(), "x".into());
        assert!(!e.is_running(&k));
        assert_eq!(e.memo("norepo", "x").unwrap().column, Column::Review);
        assert!(e.log(&k)[0].text.contains("코드 레포가 없습니다"));
        let _ = std::fs::remove_dir_all(&tmp);
    }

    #[test]
    fn restart_moves_interrupted_work_to_review() {
        let tmp = tmpdir("engine4");
        let ws = Workspace::open(&tmp).unwrap();
        let p = ws.create_project("demo", "").unwrap();
        std::fs::write(p.dir.join("doing/half.md"), "# 반쯤\n").unwrap();
        let s = ws.state_dir("demo").join("memos/half.json");
        crate::write_atomic(&s, br#"{"session_id":"abc"}"#).unwrap();
        let mut e = Engine::new(ws);
        assert_eq!(e.memo("demo", "half").unwrap().column, Column::Review);
        let k = Key::Memo("demo".into(), "half".into());
        assert_eq!(e.log(&k)[0].kind, Kind::Note);
        let _ = std::fs::remove_dir_all(&tmp);
    }
}
