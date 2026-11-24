//! 배포 판 번호.
//!
//! 릴리즈 빌드는 `JADECODE_VERSION`(예: `0.1.7-alpha`)을 넣어 빌드한다. 없으면 Cargo 판(개발 빌드).

/// 이 빌드의 판(앞의 `v` 없이).
pub const VERSION: &str = match option_env!("JADECODE_VERSION") {
    Some(v) => v,
    None => env!("CARGO_PKG_VERSION"),
};

/// 개발 빌드인가(릴리즈 판이 안 박혔다).
pub fn is_dev_build() -> bool {
    option_env!("JADECODE_VERSION").is_none()
}

fn parts(v: &str) -> (Vec<u64>, String) {
    let v = v.trim().trim_start_matches('v');
    let (num, pre) = match v.split_once('-') {
        Some((n, p)) => (n, p.to_string()),
        None => (v, String::new()),
    };
    let nums = num
        .split('.')
        .map(|p| p.parse::<u64>().unwrap_or(0))
        .collect();
    (nums, pre)
}

/// `latest`가 `current`보다 새 판인가. 숫자는 자리별로 비교하고(0.1.10 > 0.1.9),
/// 숫자가 같으면 접미사 없는 정식판이 접미사(-alpha 등) 판보다 새 것이다.
pub fn is_newer(latest: &str, current: &str) -> bool {
    let (ln, lp) = parts(latest);
    let (cn, cp) = parts(current);
    let n = ln.len().max(cn.len());
    for i in 0..n {
        let (a, b) = (
            ln.get(i).copied().unwrap_or(0),
            cn.get(i).copied().unwrap_or(0),
        );
        if a != b {
            return a > b;
        }
    }
    match (lp.is_empty(), cp.is_empty()) {
        (true, false) => true,
        (false, true) => false,
        _ => lp > cp,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn compares_numerically() {
        assert!(is_newer("v0.1.10-alpha", "0.1.9-alpha"));
        assert!(!is_newer("v0.1.6-alpha", "0.1.6-alpha"));
        assert!(!is_newer("0.1.5-alpha", "0.1.6-alpha"));
        assert!(is_newer("0.2.0", "0.1.99"));
        assert!(is_newer("0.2.0", "0.2.0-alpha"));
        assert!(!is_newer("0.2.0-alpha", "0.2.0"));
        assert!(is_newer("0.2.0-beta", "0.2.0-alpha"));
    }
}
