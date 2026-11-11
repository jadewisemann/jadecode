//! ULID: 26자 Crockford base32 (48비트 시각 + 80비트 난수).
//! 정렬 가능한 카드·인스턴스 id로 쓴다.

use rand::Rng;

const ALPHABET: &[u8; 32] = b"0123456789ABCDEFGHJKMNPQRSTVWXYZ";

pub fn new_id() -> String {
    let ms = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0);
    let mut rng = rand::rng();
    let hi: u16 = rng.random();
    let lo: u64 = rng.random();
    encode(ms, hi, lo)
}

fn encode(ms: u64, hi: u16, lo: u64) -> String {
    // 128비트 = ms(48) << 80 | hi(16) << 64 | lo(64)
    let mut bytes = [0u8; 16];
    bytes[..6].copy_from_slice(&ms.to_be_bytes()[2..]);
    bytes[6..8].copy_from_slice(&hi.to_be_bytes());
    bytes[8..].copy_from_slice(&lo.to_be_bytes());

    // 16바이트 = 128비트 → 26글자(130비트, 앞 2비트는 0)
    let mut out = String::with_capacity(26);
    let mut buf: u128 = 0;
    for b in bytes {
        buf = (buf << 8) | b as u128;
    }
    let mut chars = [0u8; 26];
    for i in (0..26).rev() {
        chars[i] = ALPHABET[(buf & 31) as usize];
        buf >>= 5;
    }
    for c in chars {
        out.push(c as char);
    }
    out
}

/// ULID 모양인지(26자 Crockford, 첫 글자는 0~7).
pub fn is_ulid(s: &str) -> bool {
    s.len() == 26
        && s.bytes().all(|b| ALPHABET.contains(&b.to_ascii_uppercase()))
        && s.as_bytes()[0].to_ascii_uppercase() <= b'7'
}

/// 유닉스 ms → `YYYY-MM-DD`(UTC). 의존성 없이 계산한다.
pub fn date_from_ms(ms: u64) -> String {
    let days = (ms / 86_400_000) as i64;
    // Howard Hinnant의 civil_from_days
    let z = days + 719_468;
    let era = if z >= 0 { z } else { z - 146_096 } / 146_097;
    let doe = (z - era * 146_097) as u64;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let y = yoe as i64 + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = if m <= 2 { y + 1 } else { y };
    format!("{y:04}-{m:02}-{d:02}")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn dates() {
        assert_eq!(date_from_ms(0), "1970-01-01");
        assert_eq!(date_from_ms(1_762_600_000_000), "2025-11-08");
        assert_eq!(date_from_ms(1_797_000_000_000), "2026-12-11");
        assert_eq!(date_from_ms(1_783_620_000_000), "2026-07-09");
    }

    #[test]
    fn shape_and_order() {
        let a = new_id();
        assert!(is_ulid(&a), "{a}");
        let b = new_id();
        assert!(is_ulid(&b), "{b}");
        assert_ne!(a, b);
        // 같은 밀리초 안에서도 증가한다(난수는 뒤쪽 80비트).
        let c = encode(1_762_600_000_000, 0, 0);
        let d = encode(1_762_600_000_001, 0, 0);
        assert!(c < d, "{c} {d}");
    }
}
