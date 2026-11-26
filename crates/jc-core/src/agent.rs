//! Claude Code 한 턴 실행.
//!
//! 턴마다 `claude -p --output-format stream-json --verbose`를 새로 띄우고, 프롬프트는 표준 입력으로 넣는다.
//! 첫 턴은 `--session-id`, 이후 턴은 `--resume`으로 같은 대화를 잇는다. 상주 프로세스가 없으므로
//! 앱이 재시작돼도 다음 턴은 그대로 이어지고, 멈추기는 프로세스를 끝내는 것으로 충분하다.

use anyhow::{Context, Result};
use serde_json::Value;
use std::io::{BufRead, BufReader, Write};
use std::path::PathBuf;
use std::process::{Child, Command, Stdio};
use std::sync::{Arc, Mutex};

#[derive(Debug, Clone, Default)]
pub struct TurnSpec {
    /// 실행 파일(기본 `claude`, `JADECODE_CLAUDE`로 바꿀 수 있다).
    pub bin: String,
    pub cwd: PathBuf,
    pub session_id: String,
    /// 이미 시작한 대화를 잇는가.
    pub resume: bool,
    pub prompt: String,
    pub append_system: String,
    pub permission_mode: String,
    pub allowed_tools: Vec<String>,
    pub disallowed_tools: Vec<String>,
    pub model: String,
}

impl TurnSpec {
    pub fn args(&self) -> Vec<String> {
        let mut a: Vec<String> = vec![
            "-p".into(),
            "--output-format".into(),
            "stream-json".into(),
            "--verbose".into(),
        ];
        if self.resume {
            a.push("--resume".into());
        } else {
            a.push("--session-id".into());
        }
        a.push(self.session_id.clone());
        if !self.append_system.is_empty() {
            a.push("--append-system-prompt".into());
            a.push(self.append_system.clone());
        }
        if !self.permission_mode.is_empty() {
            a.push("--permission-mode".into());
            a.push(self.permission_mode.clone());
        }
        if !self.allowed_tools.is_empty() {
            a.push("--allowedTools".into());
            a.push(self.allowed_tools.join(","));
        }
        if !self.disallowed_tools.is_empty() {
            a.push("--disallowedTools".into());
            a.push(self.disallowed_tools.join(","));
        }
        if !self.model.is_empty() {
            a.push("--model".into());
            a.push(self.model.clone());
        }
        a
    }
}

#[derive(Debug, Clone, PartialEq)]
pub enum Event {
    Text(String),
    Tool(String),
    /// 턴이 끝났다. `ok`가 거짓이면 에이전트가 오류로 끝낸 것이다.
    Finished {
        ok: bool,
        text: String,
        cost_usd: f64,
        duration_ms: u64,
    },
    /// 프로세스를 띄우지 못했거나 결과 없이 끝났다.
    Failed(String),
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

/// 도구 호출에서 사람이 알아볼 한 줄을 뽑는다.
pub fn tool_summary(tool: &str, input: &Value) -> String {
    let detail = [
        "command",
        "file_path",
        "path",
        "pattern",
        "url",
        "query",
        "description",
    ]
    .iter()
    .find_map(|k| input[*k].as_str())
    .unwrap_or("");
    if detail.is_empty() {
        tool.to_string()
    } else {
        format!("{tool} · {}", short(detail, 140))
    }
}

/// stream-json 한 줄을 해석한다. 관심 없는 줄은 빈 목록.
pub fn parse_line(line: &str) -> Vec<Event> {
    let Ok(v) = serde_json::from_str::<Value>(line) else {
        return vec![];
    };
    match v["type"].as_str() {
        Some("assistant") => v["message"]["content"]
            .as_array()
            .into_iter()
            .flatten()
            .filter_map(|b| match b["type"].as_str() {
                Some("text") => b["text"]
                    .as_str()
                    .filter(|t| !t.trim().is_empty())
                    .map(|t| Event::Text(t.trim().to_string())),
                Some("tool_use") => Some(Event::Tool(tool_summary(
                    b["name"].as_str().unwrap_or("tool"),
                    &b["input"],
                ))),
                _ => None,
            })
            .collect(),
        Some("result") => {
            let is_error = v["is_error"].as_bool().unwrap_or(false)
                || v["subtype"].as_str().is_some_and(|s| s != "success");
            let text = v["result"]
                .as_str()
                .map(String::from)
                .or_else(|| {
                    v["errors"].as_array().map(|e| {
                        e.iter()
                            .filter_map(|x| x.as_str())
                            .collect::<Vec<_>>()
                            .join("\n")
                    })
                })
                .unwrap_or_default();
            vec![Event::Finished {
                ok: !is_error,
                text,
                cost_usd: v["total_cost_usd"].as_f64().unwrap_or(0.0),
                duration_ms: v["duration_ms"].as_u64().unwrap_or(0),
            }]
        }
        _ => vec![],
    }
}

/// 실행 중인 턴. `stop`으로 끝낸다.
#[derive(Clone)]
pub struct Running {
    child: Arc<Mutex<Option<Child>>>,
}

impl Running {
    pub fn stop(&self) {
        if let Some(c) = self
            .child
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .as_mut()
        {
            #[cfg(unix)]
            unsafe {
                // 에이전트가 띄운 자식(셸 명령 등)까지 함께 끝낸다.
                libc::kill(-(c.id() as i32), libc::SIGTERM);
            }
            let _ = c.kill();
        }
    }
}

/// 부모 Claude Code 세션의 표시를 자식에게 넘기지 않는다(중첩 세션으로 오인하지 않게).
const DROP_ENV: &[&str] = &[
    "CLAUDECODE",
    "CLAUDE_CODE_ENTRYPOINT",
    "CLAUDE_CODE_SESSION_ID",
    "CLAUDE_CODE_MESSAGING_SOCKET",
];

/// 턴 하나를 띄운다. 이벤트는 `on_event`로 차례로 오고, 마지막은 항상 `Finished`나 `Failed`다.
pub fn spawn(spec: TurnSpec, on_event: impl Fn(Event) + Send + 'static) -> Result<Running> {
    let mut cmd = Command::new(&spec.bin);
    cmd.args(spec.args())
        .current_dir(&spec.cwd)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    for k in DROP_ENV {
        cmd.env_remove(k);
    }
    #[cfg(unix)]
    {
        use std::os::unix::process::CommandExt;
        cmd.process_group(0);
    }
    let mut child = cmd.spawn().with_context(|| {
        format!(
            "{}을(를) 실행하지 못했다. Claude Code가 설치돼 있는가?",
            spec.bin
        )
    })?;
    let mut stdin = child.stdin.take().context("표준 입력 없음")?;
    let stdout = child.stdout.take().context("표준 출력 없음")?;
    let stderr = child.stderr.take().context("표준 오류 없음")?;
    let prompt = spec.prompt.clone();
    std::thread::spawn(move || {
        let _ = stdin.write_all(prompt.as_bytes());
        // 닫아야 claude가 입력 끝을 안다.
    });
    let err_buf = Arc::new(Mutex::new(String::new()));
    {
        let err_buf = err_buf.clone();
        std::thread::spawn(move || {
            for l in BufReader::new(stderr).lines().map_while(|l| l.ok()) {
                let mut b = err_buf.lock().unwrap_or_else(|e| e.into_inner());
                if b.len() < 4000 {
                    b.push_str(&l);
                    b.push('\n');
                }
            }
        });
    }
    let running = Running {
        child: Arc::new(Mutex::new(Some(child))),
    };
    let handle = running.clone();
    std::thread::spawn(move || {
        let mut finished = false;
        for line in BufReader::new(stdout).lines().map_while(|l| l.ok()) {
            for ev in parse_line(&line) {
                if matches!(ev, Event::Finished { .. }) {
                    finished = true;
                }
                on_event(ev);
            }
        }
        let status = handle
            .child
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .take()
            .and_then(|mut c| c.wait().ok());
        if !finished {
            std::thread::sleep(std::time::Duration::from_millis(50));
            let err = err_buf.lock().unwrap_or_else(|e| e.into_inner()).clone();
            let code = status
                .map(|s| match s.code() {
                    Some(c) => format!("종료 코드 {c}"),
                    None => "중단됨".to_string(),
                })
                .unwrap_or_else(|| "알 수 없는 종료".into());
            let msg = if err.trim().is_empty() {
                code
            } else {
                format!("{code}: {}", short(err.trim(), 600))
            };
            on_event(Event::Failed(msg));
        }
    });
    Ok(running)
}

/// Finder·Dock에서 연 macOS 앱은 PATH가 짧아 `claude`·`git`을 못 찾는다.
/// 로그인 셸의 PATH와 흔한 설치 위치를 붙인다. 스레드를 띄우기 전에 한 번 부른다.
pub fn augment_path() {
    let cur = std::env::var("PATH").unwrap_or_default();
    let mut parts: Vec<String> = cur
        .split(':')
        .filter(|s| !s.is_empty())
        .map(String::from)
        .collect();
    let mut extra: Vec<String> = vec![];
    if cfg!(target_os = "macos") {
        if let Some(p) = login_shell_path() {
            extra.extend(p.split(':').filter(|s| !s.is_empty()).map(String::from));
        }
    }
    let home = std::env::var("HOME").unwrap_or_default();
    for d in [
        "/opt/homebrew/bin".to_string(),
        "/usr/local/bin".to_string(),
        format!("{home}/.local/bin"),
        format!("{home}/.claude/local"),
        format!("{home}/.bun/bin"),
        format!("{home}/.npm-global/bin"),
    ] {
        extra.push(d);
    }
    let mut changed = false;
    for e in extra {
        if !parts.contains(&e) && std::path::Path::new(&e).is_dir() {
            parts.push(e);
            changed = true;
        }
    }
    if changed {
        std::env::set_var("PATH", parts.join(":"));
    }
}

fn login_shell_path() -> Option<String> {
    let shell = std::env::var("SHELL").unwrap_or_else(|_| "/bin/zsh".into());
    let mut child = Command::new(shell)
        .args(["-l", "-c", "printf %s \"$PATH\""])
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .ok()?;
    // 셸 설정이 멈추는 경우를 막는다(최대 3초).
    for _ in 0..60 {
        if let Ok(Some(_)) = child.try_wait() {
            let out = child.wait_with_output().ok()?;
            let s = String::from_utf8_lossy(&out.stdout).trim().to_string();
            return (!s.is_empty()).then_some(s);
        }
        std::thread::sleep(std::time::Duration::from_millis(50));
    }
    let _ = child.kill();
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_stream_json() {
        let a = r#"{"type":"assistant","message":{"content":[{"type":"text","text":" 고칠게요 "},{"type":"tool_use","name":"Bash","input":{"command":"cargo test"}}]}}"#;
        assert_eq!(
            parse_line(a),
            vec![
                Event::Text("고칠게요".into()),
                Event::Tool("Bash · cargo test".into())
            ]
        );
        let r = r#"{"type":"result","subtype":"success","is_error":false,"result":"끝","total_cost_usd":0.12,"duration_ms":3400}"#;
        assert_eq!(
            parse_line(r),
            vec![Event::Finished {
                ok: true,
                text: "끝".into(),
                cost_usd: 0.12,
                duration_ms: 3400
            }]
        );
        let e = r#"{"type":"result","subtype":"error_max_turns","is_error":true}"#;
        assert!(matches!(
            &parse_line(e)[0],
            Event::Finished { ok: false, .. }
        ));
        assert!(parse_line(r#"{"type":"system","subtype":"init"}"#).is_empty());
        assert!(parse_line("not json").is_empty());
    }

    #[test]
    fn args_switch_between_new_and_resume() {
        let mut s = TurnSpec {
            bin: "claude".into(),
            session_id: "u".into(),
            allowed_tools: vec!["Read".into(), "Bash(git log:*)".into()],
            ..Default::default()
        };
        let a = s.args();
        assert!(a.windows(2).any(|w| w == ["--session-id", "u"]));
        assert!(a
            .windows(2)
            .any(|w| w == ["--allowedTools", "Read,Bash(git log:*)"]));
        s.resume = true;
        assert!(s.args().windows(2).any(|w| w == ["--resume", "u"]));
    }
}
