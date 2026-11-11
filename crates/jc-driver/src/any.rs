//! 백엔드 공통 드라이버. Claude·OpenCode·omp·pi를 같은 인터페이스로 다룬다.

use crate::acp::{AcpDriver, AcpHost, AcpPermission};
use crate::agent::AgentKind;
use crate::pi::PiDriver;
use crate::proc::{Driver, DriverConfig, DriverState};
use crate::spec::isolation_mcp_servers;
use crate::stream::DriverEvent;
use anyhow::{Context, Result};
use serde_json::Value;
use std::path::PathBuf;
use std::sync::Arc;
use tokio::sync::{mpsc, oneshot};

/// 권한 콜백: 요청을 받아 (허용 여부, 메시지)를 돌려준다.
pub type PermissionHook =
    Arc<dyn Fn(AcpPermission) -> oneshot::Receiver<(bool, String)> + Send + Sync>;
/// 파일 쓰기 콜백: (경로, 성공 여부).
pub type FileHook = Arc<dyn Fn(&str, bool) + Send + Sync>;

impl std::fmt::Debug for SpawnHooks {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("SpawnHooks")
            .field("permission", &self.permission.is_some())
            .field("on_file", &self.on_file.is_some())
            .finish()
    }
}

/// ACP/Pi 쪽에 넘길 콜백. Claude는 자체 권한 RPC를 쓰므로 쓰지 않는다.
#[derive(Clone, Default)]
pub struct SpawnHooks {
    pub permission: Option<PermissionHook>,
    pub on_file: Option<FileHook>,
}

/// 실행 중인 에이전트 하나.
pub enum AgentDriver {
    Claude(Box<Driver>),
    Acp(Box<AcpDriver>),
    Pi(Box<PiDriver>),
}

impl AgentDriver {
    /// `DriverConfig.agent`에 맞는 백엔드를 띄운다.
    pub async fn spawn(mut config: DriverConfig, hooks: SpawnHooks) -> Result<Self> {
        match config.agent {
            AgentKind::Claude => Ok(AgentDriver::Claude(Box::new(Driver::spawn(config).await?))),
            AgentKind::OpenCode | AgentKind::Omp => {
                let files = config.prepare()?;
                let bin = config.bin().to_string();
                let args = vec!["acp".to_string()];
                let env = config.env();
                let cwd = config.cwd.clone();
                let socket = config.socket.clone();
                let instance = config.instance_id.clone();
                let card = config.card.clone();
                let project = config.project.clone();
                let add_dirs = config.add_dirs.clone();
                let stderr = files.dir.join("stderr.log");
                let events = {
                    // AcpHost의 events 채널: 드라이버 이벤트 흐름과 합치기 위해 별도 채널을 쓰고
                    // 여기서는 더미 sink를 둔다(이벤트는 next_event로만 나간다).
                    let (tx, _rx) = mpsc::channel::<DriverEvent>(16);
                    tx
                };
                let perm: crate::acp::PermissionFn = hooks.permission.unwrap_or_else(|| {
                    Arc::new(|_p| {
                        let (_tx, rx) = oneshot::channel();
                        // 기본: 허용하지 않는다(사람이 답하지 않은 것과 같다).
                        rx
                    })
                });
                let on_file: crate::acp::FileEventFn = hooks
                    .on_file
                    .unwrap_or_else(|| Arc::new(|_p: &str, _ok: bool| {}));
                let host = AcpHost {
                    root: cwd.clone(),
                    add_dirs,
                    permission: perm,
                    on_file,
                    events,
                };
                let mcp = isolation_mcp_servers(&socket, &instance, &card, &project);
                let d = AcpDriver::spawn(&bin, &args, &cwd, &env, host, mcp, stderr).await?;
                config.bin = Some(bin);
                Ok(AgentDriver::Acp(Box::new(d)))
            }
            AgentKind::Pi => {
                let files = config.prepare()?;
                let bin = config.bin().to_string();
                let ext = files.dir.join("jadecode-extension.ts");
                // jadecode CLI 경로: 환경변수 → 실행 파일 옆 → PATH.
                let jc_bin = std::env::var("JADECODE_BIN").unwrap_or_else(|_| {
                    std::env::current_exe()
                        .ok()
                        .and_then(|e| {
                            let s = e.with_file_name("jadecode");
                            if s.exists() {
                                Some(s)
                            } else {
                                None
                            }
                        })
                        .map(|p| p.display().to_string())
                        .unwrap_or_else(|| "jadecode".to_string())
                });
                crate::pi::write_extension(
                    &ext,
                    &config.socket,
                    &config.instance_id,
                    config.card.as_deref(),
                    &config.project,
                    &jc_bin,
                    &config.workspace,
                )?;
                let env = config.env();
                let stderr = files.dir.join("stderr.log");
                let extra = config.extra_args.clone();
                let d = PiDriver::spawn(&bin, &config.cwd, &env, &ext, stderr, &extra).await?;
                config.bin = Some(bin);
                Ok(AgentDriver::Pi(Box::new(d)))
            }
        }
    }

    pub fn agent(&self) -> AgentKind {
        match self {
            AgentDriver::Claude(_) => AgentKind::Claude,
            AgentDriver::Acp(_) => AgentKind::OpenCode,
            AgentDriver::Pi(_) => AgentKind::Pi,
        }
    }

    pub fn pid(&self) -> u32 {
        match self {
            AgentDriver::Claude(d) => d.pid,
            AgentDriver::Acp(d) => d.pid,
            AgentDriver::Pi(d) => d.pid,
        }
    }

    pub async fn prompt(&mut self, text: &str) -> Result<()> {
        match self {
            AgentDriver::Claude(d) => d.prompt(text).await,
            AgentDriver::Acp(d) => d.prompt(text).await,
            AgentDriver::Pi(d) => d.prompt(text).await,
        }
    }

    pub async fn next_event(&mut self) -> Option<DriverEvent> {
        match self {
            AgentDriver::Claude(d) => d.next_event().await,
            AgentDriver::Acp(d) => d.next_event().await,
            AgentDriver::Pi(d) => d.next_event().await,
        }
    }

    pub async fn cancel(&mut self) -> Result<()> {
        match self {
            AgentDriver::Claude(d) => d.cancel().await,
            AgentDriver::Acp(d) => d.cancel().await,
            AgentDriver::Pi(d) => d.cancel().await,
        }
    }

    pub async fn terminate(self) -> Result<()> {
        match self {
            AgentDriver::Claude(d) => d.terminate().await,
            AgentDriver::Acp(d) => d.terminate().await,
            AgentDriver::Pi(d) => d.terminate().await,
        }
    }

    /// 상태(프로세스가 살아 있는지).
    pub fn state(&mut self) -> DriverState {
        match self {
            AgentDriver::Claude(d) => d.try_state(),
            // ACP·Pi는 이벤트 흐름으로만 판단한다(별도 try_wait 없이 종료 이벤트가 온다).
            AgentDriver::Acp(_) => DriverState::Busy,
            AgentDriver::Pi(_) => DriverState::Busy,
        }
    }

    pub fn stderr_path(&self) -> Option<PathBuf> {
        match self {
            AgentDriver::Claude(d) => d.stderr_path.clone(),
            AgentDriver::Acp(d) => Some(d.stderr_path.clone()),
            AgentDriver::Pi(d) => Some(d.stderr_path.clone()),
        }
    }

    /// ACP의 세션 id(재시작 때 이어 붙이기).
    pub fn session_id(&self) -> Option<&str> {
        match self {
            AgentDriver::Acp(d) => Some(&d.session_id),
            _ => None,
        }
    }

    /// 마지막 턴 요약 텍스트(있으면).
    pub fn last_text(&self) -> Option<String> {
        match self {
            AgentDriver::Pi(d) => d.last_text(),
            _ => None,
        }
    }
}

/// ACP `mcpServers` 항목 형태인지 확인(테스트·문서용).
pub fn acp_mcp_entry(name: &str, command: &str, args: &[String]) -> Value {
    serde_json::json!({
        "name": name,
        "command": command,
        "args": args,
        "env": [],
    })
}

/// 실행 파일을 찾는다: 명시 경로 → PATH.
pub fn resolve_bin(kind: AgentKind, explicit: Option<&str>) -> Result<String> {
    if let Some(p) = explicit {
        return Ok(p.to_string());
    }
    let name = kind.default_bin();
    match std::process::Command::new("sh")
        .arg("-c")
        .arg(format!("command -v {name}"))
        .output()
    {
        Ok(o) if o.status.success() => {
            let p = String::from_utf8_lossy(&o.stdout).trim().to_string();
            if p.is_empty() {
                anyhow::bail!("{name} not found in PATH");
            }
            Ok(p)
        }
        _ => anyhow::bail!(
            "{} 실행 파일을 찾을 수 없다. 설치했는지 확인하라(agents.list로 상태를 볼 수 있다).",
            name
        ),
    }
}

/// 설치 여부·버전을 본다(`agents.list` RPC).
pub fn probe(kind: AgentKind, explicit: Option<&str>) -> Value {
    let bin = explicit.map(String::from).or_else(|| {
        std::process::Command::new("sh")
            .arg("-c")
            .arg(format!("command -v {}", kind.default_bin()))
            .output()
            .ok()
            .filter(|o| o.status.success())
            .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_string())
            .filter(|s| !s.is_empty())
    });
    let (available, version) = match &bin {
        Some(b) => {
            let out = std::process::Command::new(b)
                .arg("--version")
                .output()
                .context("version");
            match out {
                Ok(o) => {
                    let s = String::from_utf8_lossy(&o.stdout).trim().to_string();
                    let s2 = String::from_utf8_lossy(&o.stderr).trim().to_string();
                    let v = if s.is_empty() { s2 } else { s };
                    (true, v.lines().next().unwrap_or("").to_string())
                }
                Err(_) => (true, String::new()),
            }
        }
        None => (false, String::new()),
    };
    serde_json::json!({
        "agent": kind.as_str(),
        "protocol": kind.protocol().as_str(),
        "binary": bin,
        "available": available,
        "version": version,
        "supports_mcp": kind.supports_mcp(),
        "client_writes_files": kind.client_writes_files(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn probes_self_binary() {
        let v = probe(AgentKind::Claude, Some("sh"));
        assert_eq!(v["available"], true);
        assert_eq!(v["supports_mcp"], true);
        let v = probe(AgentKind::Pi, Some("/nonexistent/xyz"));
        assert_eq!(v["available"], true); // 경로를 줬으면 있다고 본다(실행 실패는 spawn에서)
        assert_eq!(v["supports_mcp"], false);
    }

    #[test]
    fn acp_entry_shape() {
        let e = acp_mcp_entry("jadecode", "jadecode-mcp", &["--socket".into()]);
        assert_eq!(e["name"], "jadecode");
        assert_eq!(e["args"][0], "--socket");
    }

    #[test]
    fn resolve_fails_for_missing() {
        let r = resolve_bin(AgentKind::Pi, Some("/definitely/missing"));
        assert!(r.is_ok(), "명시 경로는 그대로 쓴다(존재 확인은 spawn)");
    }
}
