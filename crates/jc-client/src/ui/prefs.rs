//! 이 컴퓨터에만 두는 화면 설정(작은 JSON 파일).

use std::path::PathBuf;

fn path() -> PathBuf {
    let home = std::env::var_os("HOME")
        .map(PathBuf::from)
        .unwrap_or_else(std::env::temp_dir);
    let base = if cfg!(target_os = "macos") {
        home.join("Library/Application Support/jadecode")
    } else {
        std::env::var_os("XDG_CONFIG_HOME")
            .map(PathBuf::from)
            .unwrap_or_else(|| home.join(".config"))
            .join("jadecode")
    };
    base.join("prefs.json")
}

fn load() -> serde_json::Value {
    std::fs::read(path())
        .ok()
        .and_then(|b| serde_json::from_slice(&b).ok())
        .unwrap_or_else(|| serde_json::json!({}))
}

/// 리뷰어·리서처 같은 추가 봇을 쓰는가(기본 꺼짐).
pub fn load_extra_bots() -> bool {
    load()["extra_bots"].as_bool().unwrap_or(false)
}

pub fn save_extra_bots(on: bool) {
    let mut v = load();
    v["extra_bots"] = serde_json::Value::Bool(on);
    let p = path();
    if let Some(dir) = p.parent() {
        let _ = std::fs::create_dir_all(dir);
    }
    if let Err(e) = std::fs::write(&p, v.to_string()) {
        eprintln!("prefs 저장 실패: {e}");
    }
}
