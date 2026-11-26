//! 자체 업데이트: GitHub 최신 릴리즈를 받아 실행 파일(과 macOS 앱 번들)을 갈아 끼운다.
//! 브라우저로 내려받지 않으므로 Gatekeeper의 격리 표시가 붙지 않는다. TLS는 `curl`에 맡긴다.

use anyhow::{bail, Context, Result};
use std::path::Path;
use std::process::Command;

const REPO: &str = "jadewisemann/jadecode";
/// 릴리즈에 들어 있는 실행 파일.
pub const BINARIES: &[&str] = &["jadecode", "jadecode-client"];

fn curl_get(url: &str) -> Result<String> {
    let out = Command::new("curl")
        .args(["-fsSL", "-H", "User-Agent: jadecode-update", url])
        .output()
        .context("curl을 실행하지 못했다")?;
    if !out.status.success() {
        bail!("{}", String::from_utf8_lossy(&out.stderr).trim());
    }
    Ok(String::from_utf8_lossy(&out.stdout).to_string())
}

fn curl_download(url: &str, dest: &Path) -> Result<()> {
    let out = Command::new("curl")
        .args(["-fsSL", "-o"])
        .arg(dest)
        .arg(url)
        .output()
        .context("curl을 실행하지 못했다")?;
    if !out.status.success() {
        bail!(
            "내려받기 실패: {}",
            String::from_utf8_lossy(&out.stderr).trim()
        );
    }
    Ok(())
}

/// 최신 릴리즈 태그(`v` 포함). 초안은 건너뛴다.
pub fn latest_tag() -> Result<String> {
    let body = curl_get(&format!("https://api.github.com/repos/{REPO}/releases"))?;
    let v: serde_json::Value = serde_json::from_str(&body)?;
    for r in v.as_array().cloned().unwrap_or_default() {
        if r["draft"].as_bool().unwrap_or(false) {
            continue;
        }
        if let Some(t) = r["tag_name"].as_str() {
            return Ok(t.to_string());
        }
    }
    bail!("릴리즈를 찾을 수 없다")
}

/// 새 판이 있으면 그 태그.
pub fn check() -> Result<Option<String>> {
    let tag = latest_tag()?;
    Ok(
        crate::version::is_newer(tag.trim_start_matches('v'), crate::version::VERSION)
            .then_some(tag),
    )
}

pub fn platform_tag() -> Result<String> {
    let os = if cfg!(target_os = "macos") {
        "macos"
    } else if cfg!(target_os = "linux") {
        "linux"
    } else {
        bail!("지원하지 않는 OS");
    };
    let arch = if cfg!(target_arch = "aarch64") {
        "aarch64"
    } else if cfg!(target_arch = "x86_64") {
        "x86_64"
    } else {
        bail!("지원하지 않는 CPU");
    };
    Ok(format!("{os}-{arch}"))
}

pub fn sha256_file(p: &Path) -> Result<String> {
    use sha2::{Digest, Sha256};
    let data = std::fs::read(p)?;
    Ok(Sha256::digest(&data)
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect())
}

/// 태그의 릴리즈를 받아 `bindir`의 실행 파일을 갈아 끼운다. 진행 상황은 `say`로 알린다.
pub fn install(tag: &str, bindir: &Path, say: &dyn Fn(&str)) -> Result<()> {
    let plat = platform_tag()?;
    let dir = format!("jadecode-{tag}-{plat}");
    let base = format!("https://github.com/{REPO}/releases/download/{tag}");
    let tmp = std::env::temp_dir().join(format!("jc-update-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&tmp);
    std::fs::create_dir_all(&tmp)?;
    say(&format!("내려받는 중: {dir}.tar.gz"));
    curl_download(&format!("{base}/{dir}.tar.gz"), &tmp.join("a.tgz"))?;
    if curl_download(&format!("{base}/{dir}.tar.gz.sha256"), &tmp.join("a.sha")).is_ok() {
        let expected = std::fs::read_to_string(tmp.join("a.sha"))?
            .split_whitespace()
            .next()
            .unwrap_or("")
            .to_string();
        let actual = sha256_file(&tmp.join("a.tgz"))?;
        if !expected.is_empty() && !actual.eq_ignore_ascii_case(&expected) {
            bail!("체크섬 불일치: 받은 파일이 손상됐다");
        }
        say("체크섬 확인");
    }
    let st = Command::new("tar")
        .arg("-xzf")
        .arg(tmp.join("a.tgz"))
        .arg("-C")
        .arg(&tmp)
        .status()?;
    if !st.success() {
        bail!("압축 해제 실패");
    }
    let src = tmp.join(&dir);
    for b in BINARIES {
        let s = src.join(b);
        if !s.exists() {
            continue;
        }
        let d = bindir.join(b);
        // 실행 중인 파일은 덮어쓰지 말고 새 파일로 바꿔 넣는다(실행 중인 프로세스는 옛 파일을 계속 쓴다).
        let staged = bindir.join(format!(".{b}.new"));
        match std::fs::copy(&s, &staged).and_then(|_| std::fs::rename(&staged, &d)) {
            Ok(_) => {
                #[cfg(unix)]
                {
                    use std::os::unix::fs::PermissionsExt;
                    let _ = std::fs::set_permissions(&d, std::fs::Permissions::from_mode(0o755));
                }
                say(&format!("갱신: {}", d.display()));
            }
            Err(e) => say(&format!("경고: {} 갱신 실패({e})", d.display())),
        }
    }
    #[cfg(target_os = "macos")]
    {
        let app =
            std::env::var("JADECODE_APP").unwrap_or_else(|_| "/Applications/jadecode.app".into());
        let new_app = src.join("jadecode.app");
        if new_app.is_dir() && Path::new(&app).is_dir() {
            let _ = Command::new("rm").arg("-rf").arg(&app).status();
            let _ = Command::new("ditto").arg(&new_app).arg(&app).status();
            let _ = Command::new("xattr")
                .args(["-dr", "com.apple.quarantine"])
                .arg(&app)
                .status();
            let _ = Command::new("codesign")
                .args(["--force", "--deep", "--timestamp=none", "--sign", "-"])
                .arg(&app)
                .status();
            say(&format!("앱 갱신: {app}"));
        }
    }
    let _ = std::fs::remove_dir_all(&tmp);
    Ok(())
}
