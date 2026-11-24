//! 로그 파일과 치명적 오류 알림.
//!
//! Finder·Dock에서 연 앱은 표준 오류가 어디에도 남지 않는다. 그래서 터미널이 아니면 표준 오류를
//! 로그 파일로 돌린다(백엔드·에이전트 드라이버의 `eprintln!`까지 함께 남는다). 패닉이나 시작 실패는
//! 창이 없어도 보이도록 시스템 대화상자로 알린다. 알파 테스터가 문제를 보고할 때 이 파일을 붙인다.

use std::io::Write;
use std::path::PathBuf;

const MAX_BYTES: u64 = 5 * 1024 * 1024;

/// 로그 폴더. macOS는 `~/Library/Logs/jadecode`, 그 밖은 `$XDG_STATE_HOME/jadecode`
/// (없으면 `~/.local/state/jadecode`).
pub fn log_dir() -> PathBuf {
    let home = std::env::var_os("HOME")
        .map(PathBuf::from)
        .unwrap_or_else(std::env::temp_dir);
    if cfg!(target_os = "macos") {
        return home.join("Library/Logs/jadecode");
    }
    std::env::var_os("XDG_STATE_HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|| home.join(".local/state"))
        .join("jadecode")
}

pub fn log_path() -> PathBuf {
    log_dir().join("jadecode.log")
}

fn stderr_is_tty() -> bool {
    #[cfg(unix)]
    unsafe {
        libc::isatty(2) == 1
    }
    #[cfg(not(unix))]
    false
}

fn now_secs() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

/// 터미널에서 띄우지 않았거나 `JADECODE_LOG=1`이면 표준 오류를 로그 파일로 돌린다.
/// 돌렸으면 그 경로를 돌려준다.
pub fn init() -> Option<PathBuf> {
    let forced = std::env::var("JADECODE_LOG").ok().as_deref() == Some("1");
    if stderr_is_tty() && !forced {
        return None;
    }
    let dir = log_dir();
    std::fs::create_dir_all(&dir).ok()?;
    let path = log_path();
    if std::fs::metadata(&path)
        .map(|m| m.len() > MAX_BYTES)
        .unwrap_or(false)
    {
        let _ = std::fs::rename(&path, dir.join("jadecode.log.1"));
    }
    let file = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(&path)
        .ok()?;
    #[cfg(unix)]
    {
        use std::os::fd::AsRawFd;
        unsafe {
            libc::dup2(file.as_raw_fd(), 2);
        }
    }
    eprintln!(
        "\n=== jadecode {} 시작 (unix {}) pid {} ===",
        jc_core::version::VERSION,
        now_secs(),
        std::process::id()
    );
    Some(path)
}

/// 패닉을 로그에 남기고, 창이 없어도 보이게 대화상자로 알린다.
pub fn install_panic_hook() {
    let default = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        default(info);
        let _ = std::io::stderr().flush();
        let thread = std::thread::current();
        let name = thread.name().unwrap_or("?");
        // 화면 스레드가 아닌 곳의 패닉은 그 동작만 실패한다. 대화상자는 화면 스레드(main)일 때만 띄운다.
        if name == "main" {
            let msg = info
                .payload()
                .downcast_ref::<&str>()
                .map(|s| s.to_string())
                .or_else(|| info.payload().downcast_ref::<String>().cloned())
                .unwrap_or_else(|| "알 수 없는 오류".into());
            fatal_dialog("jadecode가 예기치 않게 종료됩니다", &msg);
        }
    }));
}

/// 시스템 대화상자로 오류를 알린다(창을 못 연 상태에서도 보이게). 실패해도 조용히 넘어간다.
pub fn fatal_dialog(title: &str, body: &str) {
    let log = log_path();
    let text = format!(
        "{body}\n\n로그: {}\n문제 보고: https://github.com/jadewisemann/jadecode/issues",
        log.display()
    );
    if cfg!(target_os = "macos") {
        let esc = |s: &str| s.replace('\\', "\\\\").replace('"', "\\\"");
        let script = format!(
            "display alert \"{}\" message \"{}\" as critical",
            esc(title),
            esc(&text)
        );
        let _ = std::process::Command::new("osascript")
            .args(["-e", &script])
            .output();
    } else {
        let ok = std::process::Command::new("zenity")
            .args(["--error", "--title", title, "--text", &text])
            .output()
            .map(|o| o.status.success())
            .unwrap_or(false);
        if !ok {
            let _ = std::process::Command::new("notify-send")
                .args([title, &text])
                .output();
        }
    }
}
