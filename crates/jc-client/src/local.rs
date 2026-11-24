//! 백엔드 고르기: 떠 있는 서버가 있으면 붙고, 없으면 **앱 안에서 로컬 백엔드를 띄운다**.
//!
//! 사용자가 앱을 열기 전에 터미널에서 `jadecode server`를 따로 띄울 필요가 없게 하는 것이
//! 목적이다. 구조는 그대로 백엔드(`jc-server`) + 프론트엔드(GPUI)이고, 둘이 같은 프로세스에
//! 있을 뿐이다. 로컬 백엔드도 같은 유닉스 소켓을 열기 때문에 CLI·MCP·에이전트 인스턴스가
//! 평소처럼 붙는다.
//!
//! 순서:
//! 1. `JADECODE_SOCKET`이 있고 붙으면 그 서버를 쓴다.
//! 2. 워크스페이스를 정한다: 인자 → `JADECODE_WORKSPACE` → 현재 폴더에서 위로 → `~/jc/work`.
//!    `~/jc/work`가 없으면 만든다.
//! 3. 워크스페이스의 서버(`server.json`의 소켓 또는 기본 소켓)에 붙으면 그 서버를 쓴다.
//! 4. 안 붙으면 백그라운드 스레드에서 `jc_server::serve`를 돌리고, 준비되면 그 소켓을 돌려준다.
//!
//! `JADECODE_NO_EMBED=1`이면 4번을 하지 않고 연결 오류를 낸다(외부 서버만 쓰고 싶을 때).

use crate::board::{find_workspace, Connection};
use anyhow::{anyhow, Context, Result};
use std::path::{Path, PathBuf};
use std::time::Duration;

/// 어떤 백엔드에 붙었는가.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum BackendKind {
    /// 이미 떠 있던 서버(`jadecode server` 등).
    External,
    /// 이 프로세스 안에서 띄운 로컬 백엔드.
    Embedded,
}

#[derive(Debug, Clone)]
pub struct Backend {
    pub kind: BackendKind,
    pub workspace: Option<PathBuf>,
    pub conn: Connection,
}

impl Backend {
    /// 화면에 보일 짧은 설명.
    pub fn label(&self) -> String {
        match self.kind {
            BackendKind::External => "서버 연결".to_string(),
            BackendKind::Embedded => "로컬 백엔드".to_string(),
        }
    }
}

/// 소켓에 실제로 붙는지(파일만 남은 죽은 소켓을 거른다).
pub fn socket_alive(p: &Path) -> bool {
    p.exists() && std::os::unix::net::UnixStream::connect(p).is_ok()
}

/// 기본 워크스페이스 위치: `~/jc/work`.
pub fn default_workspace_dir() -> PathBuf {
    let home = std::env::var("HOME").unwrap_or_else(|_| ".".into());
    PathBuf::from(home).join("jc/work")
}

/// 워크스페이스를 정한다. 아무것도 못 찾으면 `~/jc/work`를 만들어 쓴다.
pub fn resolve_workspace(arg: Option<&Path>) -> Result<PathBuf> {
    if let Some(w) = arg {
        if !w.join("jadecode.toml").exists() {
            anyhow::bail!(
                "jadecode 워크스페이스가 아니다(jadecode.toml 없음): {}",
                w.display()
            );
        }
        return Ok(w.to_path_buf());
    }
    if let Some(w) = std::env::var_os("JADECODE_WORKSPACE").map(PathBuf::from) {
        if w.join("jadecode.toml").exists() {
            return Ok(w);
        }
    }
    // Finder에서 연 앱은 현재 폴더가 `/`라 여기서는 보통 못 찾는다.
    if let Ok(cwd) = std::env::current_dir() {
        if let Ok(w) = find_workspace(&cwd) {
            return Ok(w);
        }
    }
    let w = default_workspace_dir();
    if !w.join("jadecode.toml").exists() {
        std::fs::create_dir_all(&w).with_context(|| format!("만들 수 없다: {}", w.display()))?;
        let name = w
            .file_name()
            .unwrap_or_default()
            .to_string_lossy()
            .to_string();
        jc_core::Workspace::init(&w, &name)?;
        eprintln!("워크스페이스 생성: {}", w.display());
    }
    Ok(w)
}

/// 떠 있는 서버에 붙거나, 없으면 로컬 백엔드를 띄운다.
pub fn ensure_backend(workspace: Option<&Path>) -> Result<Backend> {
    if let Some(p) = std::env::var_os("JADECODE_SOCKET").map(PathBuf::from) {
        if socket_alive(&p) {
            return Ok(Backend {
                kind: BackendKind::External,
                workspace: None,
                conn: Connection { socket: p },
            });
        }
    }
    let ws = resolve_workspace(workspace)?;
    let found = Connection::discover(Some(&ws))?;
    if socket_alive(&found.socket) {
        return Ok(Backend {
            kind: BackendKind::External,
            workspace: Some(ws),
            conn: found,
        });
    }
    let fallback = jc_core::workspace::default_socket(&ws);
    if fallback != found.socket && socket_alive(&fallback) {
        return Ok(Backend {
            kind: BackendKind::External,
            workspace: Some(ws),
            conn: Connection { socket: fallback },
        });
    }
    if std::env::var("JADECODE_NO_EMBED").ok().as_deref() == Some("1") {
        anyhow::bail!(
            "서버에 붙을 수 없다: {} (JADECODE_NO_EMBED=1 이라 로컬 백엔드를 띄우지 않았다)",
            found.socket.display()
        );
    }
    let socket = start_embedded(&ws, fallback)?;
    Ok(Backend {
        kind: BackendKind::Embedded,
        workspace: Some(ws),
        conn: Connection { socket },
    })
}

/// 로컬 백엔드를 백그라운드 스레드에서 띄우고, 소켓이 준비될 때까지 기다린다.
/// 백엔드는 프로세스가 끝날 때 함께 끝난다.
pub fn start_embedded(ws: &Path, socket: PathBuf) -> Result<PathBuf> {
    augment_path();
    let (tx, rx) = std::sync::mpsc::channel::<Result<PathBuf, String>>();
    let root = ws.to_path_buf();
    let sock = socket.clone();
    std::thread::Builder::new()
        .name("jadecode-local-backend".into())
        .spawn(move || {
            let rt = match tokio::runtime::Builder::new_multi_thread()
                .enable_all()
                .thread_name("jadecode-backend")
                .build()
            {
                Ok(r) => r,
                Err(e) => {
                    let _ = tx.send(Err(format!("런타임 오류: {e}")));
                    return;
                }
            };
            let ready = tx.clone();
            let r = rt.block_on(jc_server::serve(
                root,
                jc_server::server::Bind::Unix(sock),
                jc_server::ServerConfig::default(),
                move |b| {
                    let p = match b {
                        jc_server::server::Bound::Unix { path, .. } => path.clone(),
                        _ => PathBuf::new(),
                    };
                    let _ = ready.send(Ok(p));
                },
            ));
            // 준비 전에 끝났으면 오류를 알린다(준비 뒤라면 받는 쪽이 이미 없다).
            let msg = match r {
                Ok(()) => "로컬 백엔드가 끝났다".to_string(),
                Err(e) => format!("{e:#}"),
            };
            eprintln!("jadecode 로컬 백엔드 종료: {msg}");
            let _ = tx.send(Err(msg));
        })?;
    match rx.recv_timeout(Duration::from_secs(30)) {
        Ok(Ok(p)) => {
            let p = if p.as_os_str().is_empty() { socket } else { p };
            eprintln!("jadecode 로컬 백엔드 시작: {}", p.display());
            Ok(p)
        }
        Ok(Err(e)) => Err(anyhow!("로컬 백엔드를 띄우지 못했다: {e}")),
        Err(_) => Err(anyhow!("로컬 백엔드가 30초 안에 준비되지 않았다")),
    }
}

/// Finder·Dock에서 연 macOS 앱은 PATH가 `/usr/bin:/bin:/usr/sbin:/sbin`뿐이라
/// `claude`·`opencode` 같은 에이전트를 못 찾는다. 로그인 셸의 PATH와 흔한 설치 위치를 붙인다.
/// 스레드를 띄우기 전에 부른다.
fn augment_path() {
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
        "/opt/homebrew/bin",
        "/usr/local/bin",
        &format!("{home}/.local/bin"),
        &format!("{home}/.bun/bin"),
        &format!("{home}/.npm-global/bin"),
        &format!("{home}/.cargo/bin"),
    ] {
        extra.push(d.to_string());
    }
    // 앱 번들 안의 jadecode·jadecode-mcp도 PATH로 찾을 수 있게 한다.
    if let Ok(exe) = std::env::current_exe() {
        if let Some(d) = exe.parent() {
            extra.push(d.display().to_string());
        }
    }
    let mut changed = false;
    for e in extra {
        if !parts.contains(&e) && Path::new(&e).is_dir() {
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
    let mut child = std::process::Command::new(shell)
        .args(["-l", "-c", "printf %s \"$PATH\""])
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::null())
        .spawn()
        .ok()?;
    // 셸 설정이 멈추는 경우를 막는다(최대 3초).
    for _ in 0..60 {
        if let Ok(Some(_)) = child.try_wait() {
            let out = child.wait_with_output().ok()?;
            let s = String::from_utf8_lossy(&out.stdout).trim().to_string();
            return (!s.is_empty()).then_some(s);
        }
        std::thread::sleep(Duration::from_millis(50));
    }
    let _ = child.kill();
    None
}
