//! 색과 크기. 어두운 테마 하나만 둔다.

pub const BG: u32 = 0x0f1115;
pub const SIDEBAR: u32 = 0x14171d;
pub const PANEL: u32 = 0x181b22;
pub const RAISED: u32 = 0x1f232c;
pub const HOVER: u32 = 0x262b36;
pub const SELECTED: u32 = 0x2a3142;
pub const INPUT: u32 = 0x12151b;
pub const LINE: u32 = 0x2a2f3a;
pub const TEXT: u32 = 0xe7e9ee;
pub const MUTED: u32 = 0x9098a9;
pub const FAINT: u32 = 0x5d6576;
pub const ACCENT: u32 = 0x5b8cff;
pub const GREEN: u32 = 0x4cc38a;
pub const AMBER: u32 = 0xe5a84b;
pub const RED: u32 = 0xe5645c;
pub const VIOLET: u32 = 0xa883f0;
pub const WHITE: u32 = 0xffffff;
/// 사람이 보낸 말풍선.
pub const HUMAN_BUBBLE: u32 = 0x3557b7;

pub const MONO: &str = "Menlo";

pub fn column_color(c: jc_core::Column) -> u32 {
    match c {
        jc_core::Column::Todo => MUTED,
        jc_core::Column::Doing => ACCENT,
        jc_core::Column::Review => AMBER,
        jc_core::Column::Done => GREEN,
    }
}
