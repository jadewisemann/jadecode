//! 드라이버 프로세스: 띄우기·턴 넣기·이벤트 받기·중단·종료.

use crate::agent::AgentKind;
use crate::spec::{BotSpec, PermissionPolicy};
use crate::stream::{self, DriverEvent};
use anyhow::{bail, Context, Result};
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::process::Stdio;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::process::{Child, ChildStdin, Command};
use tokio::sync::mpsc;

/// 테스트용 "아무것도 안 하는" 명령. macOS에는 `/bin/false`가 없다.
pub fn dud_cmd() -> String {
    if cfg!(target_os = "macos") {
        "/usr/bin/true".to_string()
    } else {
        "/bin/true".to_string()
    }
}

/// 부모 세션의 환경을 물려주지 않는다(S1·S4). 이 목록만 넘긴다.
pub const ENV_WHITELIST: &[&str] = &["HOME", "PATH", "LANG", "USER", "SHELL", "TERM", "TMPDIR"];

/// 백엔드가 인증에 쓰는 환경변수. 이 목록에 있는 것만 자식에게 넘긴다
/// (부모의 Claude Code 세션 변수는 계속 끊는다 — 그건 `CLAUDECODE*`라 여기 없다).
pub const AUTH_ENV: &[&str] = &[
    "ANTHROPIC_API_KEY",
    "OPENAI_API_KEY",
    "DEEPSEEK_API_KEY",
    "GOOGLE_API_KEY",
    "GEMINI_API_KEY",
    "OPENROUTER_API_KEY",
    "XAI_API_KEY",
    "GROQ_API_KEY",
    "CEREBRAS_API_KEY",
    "MOONSHOT_API_KEY",
    "ZAI_API_KEY",
    "AWS_ACCESS_KEY_ID",
    "AWS_SECRET_ACCESS_KEY",
    "AWS_REGION",
];

#[derive(Debug, Clone)]
pub struct DriverConfig {
    /// 카드 폴더 = 에이전트 cwd.
    pub cwd: PathBuf,
    /// 서버가 만든 세션 uuid. 재시작 때도 같은 값.
    pub session_id: String,
    /// 봇 정의 폴더(인스턴스 사본).
    pub bot_dir: PathBuf,
    pub bot: BotSpec,
    pub policy: PermissionPolicy,
    /// jadecode MCP 서버 실행 명령.
    pub mcp_command: String,
    pub mcp_args: Vec<String>,
    pub mcp_env: BTreeMap<String, String>,
    /// `--add-dir`로 더 열 경로(카드의 레포 워크트리).
    pub add_dirs: Vec<PathBuf>,
    /// 기존 세션을 이어 붙인다(`--resume`).
    pub resume: bool,
    /// 실행 파일 경로(비우면 백엔드 기본값·PATH).
    pub bin: Option<String>,
    /// 어느 에이전트로 돌릴지.
    pub agent: AgentKind,
    /// jadecode 서버 소켓(ACP·Pi 도구가 쓴다).
    pub socket: PathBuf,
    pub instance_id: String,
    pub project: String,
    pub card: Option<String>,
    /// 워크스페이스 뿌리(pi 확장이 CLI에 넘긴다).
    pub workspace: PathBuf,
    /// 설정 파일을 둘 곳(인스턴스 폴더). 없으면 임시 폴더를 만든다.
    pub state_dir: Option<PathBuf>,
    pub max_turns: Option<u64>,
    /// 추가 CLI 인자(계약 테스트·실험용).
    pub extra_args: Vec<String>,
}

impl DriverConfig {
    /// `--settings`·`--mcp-config`·지침 파일을 담을 인스턴스 폴더를 준비한다.
    pub fn prepare(&self) -> Result<PreparedFiles> {
        let dir = match &self.state_dir {
            Some(d) => {
                std::fs::create_dir_all(d)?;
                d.clone()
            }
            None => {
                let d = std::env::temp_dir().join(format!("jc-inst-{}", self.session_id));
                std::fs::create_dir_all(&d)?;
                d
            }
        };
        let instructions = self.bot.instructions_text(&self.bot_dir)?;
        let prompt_file = dir.join("system-append.md");
        std::fs::write(&prompt_file, instructions)?;

        let settings = self.policy.to_settings_json(&self.bot.allowed_skills);
        let settings_file = dir.join("settings.json");
        std::fs::write(&settings_file, serde_json::to_string_pretty(&settings)?)?;

        let mcp = crate::spec::mcp_config("jadecode", &self.mcp_command, &self.mcp_args, &self.mcp_env);
        let mcp_file = dir.join("mcp.json");
        std::fs::write(&mcp_file, serde_json::to_string_pretty(&mcp)?)?;

        Ok(PreparedFiles {
            dir,
            settings: settings_file,
            mcp: mcp_file,
            append_prompt: prompt_file,
        })
    }

    /// CLI 인자. 계약 테스트가 이 목록을 본다.
    pub fn bin(&self) -> &str {
        self.bin
            .as_deref()
            .unwrap_or_else(|| self.agent.default_bin())
    }

    pub fn args(&self, f: &PreparedFiles) -> Vec<String> {
        let mut a: Vec<String> = vec![
            "-p".into(),
            "--input-format".into(),
            "stream-json".into(),
            "--output-format".into(),
            "stream-json".into(),
            "--verbose".into(),
            // 사용자 전역 지침·훅·플러그인·커넥터 MCP를 모두 뺀다(S1 H4).
            "--setting-sources".into(),
            "".into(),
            "--settings".into(),
            f.settings.display().to_string(),
            "--mcp-config".into(),
            f.mcp.display().to_string(),
            "--strict-mcp-config".into(),
            "--append-system-prompt-file".into(),
            f.append_prompt.display().to_string(),
        ];
        // --add-dir는 가변 인자라 다른 플래그 앞에 둔다(S9).
        if !self.add_dirs.is_empty() {
            a.push("--add-dir".into());
            for d in &self.add_dirs {
                a.push(d.display().to_string());
            }
        }
        if let Some(m) = &self.bot.model {
            a.push("--model".into());
            a.push(m.clone());
        }
        match &self.policy.default_mode {
            Some(m) => {
                a.push("--permission-mode".into());
                a.push(m.clone());
            }
            None => {
                // 모드를 주지 않으면 `auto`로 뜬다(S4). 기본은 acceptEdits.
                a.push("--permission-mode".into());
                a.push("acceptEdits".into());
            }
        }
        if let Some(skills) = self.bot.skills_dir(&self.bot_dir) {
            a.push("--plugin-dir".into());
            a.push(skills.display().to_string());
        }
        a.push("--permission-prompt-tool".into());
        a.push("mcp__jadecode__permission".into());
        if self.resume {
            a.push("--resume".into());
            a.push(self.session_id.clone());
        } else {
            a.push("--session-id".into());
            a.push(self.session_id.clone());
        }
        if let Some(n) = self.max_turns {
            a.push("--max-turns".into());
            a.push(n.to_string());
        }
        a.extend(self.extra_args.iter().cloned());
        a
    }

    /// 자식 환경: 화이트리스트 + jadecode 변수.
    pub fn env(&self) -> BTreeMap<String, String> {
        let mut env = BTreeMap::new();
        for k in ENV_WHITELIST.iter().chain(AUTH_ENV.iter()) {
            if let Ok(v) = std::env::var(k) {
                env.insert(k.to_string(), v);
            }
        }
        env.entry("LANG".into()).or_insert_with(|| "C.UTF-8".into());
        env.entry("TERM".into()).or_insert_with(|| "dumb".into());
        let mut all = env.clone();
        all.extend(self.mcp_env.clone());
        all
    }
}

pub struct PreparedFiles {
    pub dir: PathBuf,
    pub settings: PathBuf,
    pub mcp: PathBuf,
    pub append_prompt: PathBuf,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DriverState {
    Starting,
    Idle,
    Busy,
    Exited,
}

impl DriverState {
    pub fn as_str(&self) -> &'static str {
        match self {
            DriverState::Starting => "starting",
            DriverState::Idle => "idle",
            DriverState::Busy => "busy",
            DriverState::Exited => "exited",
        }
    }
}

#[derive(Debug, Clone, Default)]
pub struct TurnOutcome {
    pub is_error: bool,
    pub text: Option<String>,
    pub usage: stream::Usage,
    pub tool_calls: Vec<String>,
}

/// 돌고 있는 드라이버 하나. 인스턴스당 프로세스 하나(R-a).
pub struct Driver {
    pub config: DriverConfig,
    pub files: PreparedFiles,
    pub state: DriverState,
    child: Child,
    stdin: Option<ChildStdin>,
    events: mpsc::Receiver<DriverEvent>,
    reader_task: Option<tokio::task::JoinHandle<()>>,
    stderr_task: Option<tokio::task::JoinHandle<()>>,
    pub stderr_path: Option<PathBuf>,
    pub pid: u32,
}

impl Driver {
    /// 띄운다. 첫 `init`을 기다리지 않는다(이벤트 흐름에서 온다).
    pub async fn spawn(config: DriverConfig) -> Result<Self> {
        let files = config.prepare()?;
        let args = config.args(&files);
        let env = config.env();
        if !config.cwd.is_dir() {
            bail!("cwd 없음: {}", config.cwd.display());
        }
        let mut cmd = Command::new(config.bin());
        cmd.args(&args)
            .current_dir(&config.cwd)
            .env_clear()
            .envs(&env)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .kill_on_drop(false);
        // 프로세스 그룹을 따로 둬 자손(도구 자식)까지 치울 수 있게 한다(S1 7).
        unsafe {
            cmd.pre_exec(|| {
                // setpgid(0,0)
                if libc::setpgid(0, 0) != 0 {
                    return Err(std::io::Error::last_os_error());
                }
                Ok(())
            });
        }
        let mut child = cmd
            .spawn()
            .with_context(|| format!("spawn {} {:?}", config.bin(), args))?;
        let pid = child.id().unwrap_or(0);
        let stdin = child.stdin.take();
        let stdout = child.stdout.take().context("no stdout")?;
        let stderr = child.stderr.take();

        let (tx, rx) = mpsc::channel::<DriverEvent>(256);
        let reader_task = tokio::spawn(async move {
            let mut lines = BufReader::new(stdout).lines();
            while let Ok(Some(line)) = lines.next_line().await {
                if line.trim().is_empty() {
                    continue;
                }
                match stream::parse_line(&line) {
                    Some(ev) => {
                        if tx.send(ev).await.is_err() {
                            break;
                        }
                    }
                    None => {
                        let _ = tx
                            .send(DriverEvent::Other(serde_json::json!({"raw": line})))
                            .await;
                    }
                }
            }
        });

        // stderr는 파일로 남긴다(디버그·계약 위반 추적).
        let stderr_path = files.dir.join("stderr.log");
        let sp = stderr_path.clone();
        let stderr_task = stderr.map(|s| {
            tokio::spawn(async move {
                let mut lines = BufReader::new(s).lines();
                let mut f = tokio::fs::File::create(&sp).await.ok();
                while let Ok(Some(l)) = lines.next_line().await {
                    if let Some(f) = f.as_mut() {
                        let _ = f.write_all(l.as_bytes()).await;
                        let _ = f.write_all(b"\n").await;
                    }
                }
            })
        });

        Ok(Self {
            config,
            files,
            state: DriverState::Starting,
            child,
            stdin,
            events: rx,
            reader_task: Some(reader_task),
            stderr_task,
            stderr_path: Some(stderr_path),
            pid,
        })
    }

    /// 사용자 턴을 넣는다. 한가할 때 넣는 것이 기본(급하면 즉시).
    pub async fn prompt(&mut self, text: &str) -> Result<()> {
        let stdin = self.stdin.as_mut().context("stdin 닫힘")?;
        let line = stream::user_turn(text, &self.config.session_id);
        stdin.write_all(line.as_bytes()).await?;
        stdin.write_all(b"\n").await?;
        stdin.flush().await?;
        self.state = DriverState::Busy;
        Ok(())
    }

    /// 현재 턴만 끊는다. 이미 넣은 메시지는 남는다(S1 2).
    pub async fn cancel(&mut self) -> Result<()> {
        let stdin = self.stdin.as_mut().context("stdin 닫힘")?;
        let req = format!("req_{}", crate::stream::interrupt_request_id());
        stdin.write_all(stream::interrupt(&req).as_bytes()).await?;
        stdin.write_all(b"\n").await?;
        stdin.flush().await?;
        Ok(())
    }

    /// 다음 이벤트. 프로세스가 끝나면 `None`.
    pub async fn next_event(&mut self) -> Option<DriverEvent> {
        let ev = self.events.recv().await?;
        match &ev {
            DriverEvent::Init { .. } => self.state = DriverState::Idle,
            DriverEvent::Result { .. } => {
                self.state = if self.child.try_wait().ok().flatten().is_some() {
                    DriverState::Exited
                } else {
                    DriverState::Idle
                };
            }
            DriverEvent::Assistant { .. } => self.state = DriverState::Busy,
            _ => {}
        }
        Some(ev)
    }

    pub fn try_state(&mut self) -> DriverState {
        if self.state != DriverState::Exited {
            if let Ok(Some(_)) = self.child.try_wait() {
                self.state = DriverState::Exited;
            }
        }
        self.state
    }

    /// stdin을 닫아 정상 종료를 유도한다(한가할 때).
    pub fn close_stdin(&mut self) {
        self.stdin.take();
    }

    /// SIGTERM → 5초 → SIGKILL. 프로세스 그룹 전체에 보낸다.
    pub async fn terminate(mut self) -> Result<()> {
        self.close_stdin();
        if let Ok(Some(_)) = self.child.try_wait() {
            self.cleanup();
            return Ok(());
        }
        let pgid = self.pid as i32;
        unsafe {
            libc::kill(-pgid, libc::SIGTERM);
        }
        match tokio::time::timeout(std::time::Duration::from_secs(5), self.child.wait()).await {
            Ok(_) => {}
            Err(_) => {
                unsafe {
                    libc::kill(-pgid, libc::SIGKILL);
                }
                let _ = self.child.wait().await;
            }
        }
        self.cleanup();
        Ok(())
    }

    fn cleanup(&mut self) {
        if let Some(t) = self.reader_task.take() {
            t.abort();
        }
        if let Some(t) = self.stderr_task.take() {
            t.abort();
        }
        self.state = DriverState::Exited;
    }

    /// 남은 이벤트를 다 받아 마지막 `result`를 돌려준다(간단한 일회 실행용).
    pub async fn run_to_result(&mut self) -> Result<TurnOutcome> {
        let mut out = TurnOutcome::default();
        while let Some(ev) = self.next_event().await {
            match ev {
                DriverEvent::Assistant { blocks, .. } => {
                    for b in blocks {
                        if let stream::AssistantBlock::ToolUse { name, .. } = b {
                            out.tool_calls.push(name);
                        }
                    }
                }
                DriverEvent::Result { is_error, text, usage, .. } => {
                    out.is_error = is_error;
                    out.text = text;
                    out.usage = usage;
                    return Ok(out);
                }
                DriverEvent::Init { .. } => {}
                _ => {}
            }
        }
        bail!("프로세스가 result 없이 끝났다")
    }
}

/// 세션 폴더(`.jadecode/instances/<id>/`)를 만든다.
pub fn instance_dir(root: &Path, instance_id: &str) -> PathBuf {
    root.join(".jadecode/instances").join(instance_id)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cfg() -> DriverConfig {
        // 테스트가 병렬로 돌므로 폴더를 매번 따로 만든다.
        let tmp = std::env::temp_dir().join(format!("jc-drv-{}-{}", std::process::id(), jc_core::ulid::new_id()));
        std::fs::create_dir_all(&tmp).unwrap();
        let bot_dir = tmp.join("bot");
        std::fs::create_dir_all(&bot_dir).unwrap();
        std::fs::write(bot_dir.join("bot.toml"), "name=\"w\"\n").unwrap();
        std::fs::write(bot_dir.join("instructions.md"), "you are a worker").unwrap();
        let bot = BotSpec::load(&bot_dir).unwrap();
        DriverConfig {
            cwd: tmp.clone(),
            session_id: "11111111-2222-3333-4444-555555555555".into(),
            bot_dir,
            bot,
            policy: PermissionPolicy::default_policy(),
            mcp_command: dud_cmd(),
            mcp_args: vec![],
            mcp_env: BTreeMap::new(),
            add_dirs: vec![],
            resume: false,
            bin: Some("claude".into()),
            agent: AgentKind::Claude,
            socket: PathBuf::from("/tmp/x.sock"),
            instance_id: "w1".into(),
            project: "p1".into(),
            card: None,
            workspace: PathBuf::from("/tmp/ws"),
            state_dir: Some(tmp.join("state")),
            max_turns: None,
            extra_args: vec![],
        }
    }

    #[test]
    fn args_isolate_user_settings() {
        let c = cfg();
        let f = c.prepare().unwrap();
        let a = c.args(&f);
        let joined = a.join(" ");
        assert!(joined.contains("--setting-sources  --settings"), "{joined}");
        assert!(joined.contains("--strict-mcp-config"));
        assert!(joined.contains("--permission-prompt-tool mcp__jadecode__permission"));
        assert!(joined.contains("--session-id 11111111-2222-3333-4444-555555555555"));
        assert!(!joined.contains("--resume"));
        assert!(f.append_prompt.exists());
        let settings: serde_json::Value =
            serde_json::from_str(&std::fs::read_to_string(&f.settings).unwrap()).unwrap();
        assert!(settings["permissions"]["deny"].as_array().unwrap().len() > 3);
    }

    #[test]
    fn resume_uses_same_session_id() {
        let mut c = cfg();
        c.resume = true;
        let f = c.prepare().unwrap();
        let a = c.args(&f).join(" ");
        assert!(a.contains("--resume 11111111-2222-3333-4444-555555555555"), "{a}");
        assert!(!a.contains("--session-id"));
    }

    #[test]
    fn add_dirs_come_before_other_flags() {
        let mut c = cfg();
        c.add_dirs = vec![PathBuf::from("/tmp/wt1"), PathBuf::from("/tmp/wt2")];
        let f = c.prepare().unwrap();
        let a = c.args(&f);
        let i = a.iter().position(|x| x == "--add-dir").unwrap();
        let m = a.iter().position(|x| x == "--model").unwrap_or(a.len());
        assert!(i < m, "{a:?}");
        assert_eq!(&a[i + 1..i + 3], &["/tmp/wt1", "/tmp/wt2"]);
    }

    #[test]
    fn env_drops_parent_claude_vars() {
        std::env::set_var("CLAUDECODE", "1");
        std::env::set_var("CLAUDE_CODE_SESSION_ID", "x");
        let c = cfg();
        let e = c.env();
        assert!(!e.contains_key("CLAUDECODE"));
        // 인증 변수는 넘어간다(설정돼 있으면)
        if std::env::var("DEEPSEEK_API_KEY").is_ok() {
            assert!(e.contains_key("DEEPSEEK_API_KEY"));
        }
        assert!(!e.contains_key("CLAUDE_CODE_SESSION_ID"));
        assert!(e.contains_key("HOME"));
        assert!(e.contains_key("PATH"));
    }
}
