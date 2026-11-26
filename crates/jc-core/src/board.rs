//! 칸반: 열은 폴더, 메모는 `.md` 파일.

use crate::workspace::{slugify, Project};
use anyhow::{bail, Context, Result};
use std::path::PathBuf;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum Column {
    Todo,
    Doing,
    Review,
    Done,
}

impl Column {
    pub const ALL: [Column; 4] = [Column::Todo, Column::Doing, Column::Review, Column::Done];

    pub fn dir_name(self) -> &'static str {
        match self {
            Column::Todo => "todo",
            Column::Doing => "doing",
            Column::Review => "review",
            Column::Done => "done",
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            Column::Todo => "할 일",
            Column::Doing => "진행 중",
            Column::Review => "확인 필요",
            Column::Done => "완료",
        }
    }

    pub fn from_dir(s: &str) -> Option<Column> {
        Column::ALL.into_iter().find(|c| c.dir_name() == s)
    }
}

/// 메모 하나. 파일 이름(확장자 뺀 것)이 프로젝트 안에서의 id다.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Memo {
    pub stem: String,
    pub column: Column,
    pub path: PathBuf,
    pub title: String,
    /// 제목 줄을 뺀 본문 첫 부분(타일 미리보기).
    pub preview: String,
    pub text: String,
    pub modified_ms: u64,
}

/// 메모의 제목: 첫 `# ` 제목, 없으면 첫 줄, 그것도 없으면 파일 이름.
pub fn title_of(text: &str, stem: &str) -> String {
    for l in text.lines() {
        let t = l.trim();
        if t.is_empty() {
            continue;
        }
        let t = t.trim_start_matches('#').trim();
        if !t.is_empty() {
            return t.chars().take(120).collect();
        }
    }
    stem.to_string()
}

fn preview_of(text: &str) -> String {
    let mut lines = text.lines().map(str::trim).filter(|l| !l.is_empty());
    lines.next(); // 제목 줄
    let mut out = String::new();
    for l in lines {
        if l.starts_with("```") {
            break;
        }
        let l = l.trim_start_matches(['#', '-', '*', '>', ' ']).trim();
        if l.is_empty() {
            continue;
        }
        if !out.is_empty() {
            out.push(' ');
        }
        out.push_str(l);
        if out.chars().count() > 160 {
            break;
        }
    }
    out.chars().take(160).collect()
}

fn mtime_ms(p: &std::path::Path) -> u64 {
    std::fs::metadata(p)
        .and_then(|m| m.modified())
        .ok()
        .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}

pub fn read_memo(path: PathBuf, column: Column) -> Option<Memo> {
    let stem = path.file_stem()?.to_string_lossy().to_string();
    let text = std::fs::read_to_string(&path).ok()?;
    Some(Memo {
        title: title_of(&text, &stem),
        preview: preview_of(&text),
        modified_ms: mtime_ms(&path),
        stem,
        column,
        path,
        text,
    })
}

/// 프로젝트의 모든 메모. 열 순서, 열 안에서는 최근 수정순.
pub fn scan(p: &Project) -> Vec<Memo> {
    let mut out = vec![];
    for c in Column::ALL {
        let Ok(rd) = std::fs::read_dir(p.column_dir(c)) else {
            continue;
        };
        let mut col: Vec<Memo> = rd
            .flatten()
            .map(|e| e.path())
            .filter(|p| {
                p.extension().is_some_and(|x| x == "md")
                    && !p
                        .file_name()
                        .is_some_and(|n| n.to_string_lossy().starts_with('.'))
            })
            .filter_map(|path| read_memo(path, c))
            .collect();
        col.sort_by(|a, b| b.modified_ms.cmp(&a.modified_ms).then(a.stem.cmp(&b.stem)));
        out.extend(col);
    }
    out
}

pub fn find(p: &Project, stem: &str) -> Option<Memo> {
    Column::ALL.into_iter().find_map(|c| {
        let path = p.column_dir(c).join(format!("{stem}.md"));
        path.is_file().then(|| read_memo(path, c)).flatten()
    })
}

/// 어느 열에도 없는 파일 이름을 고른다.
fn free_stem(p: &Project, base: &str) -> String {
    let base = if base.is_empty() { "memo" } else { base };
    let taken = |s: &str| {
        Column::ALL
            .iter()
            .any(|c| p.column_dir(*c).join(format!("{s}.md")).exists())
    };
    if !taken(base) {
        return base.to_string();
    }
    (2..)
        .map(|n| format!("{base}-{n}"))
        .find(|s| !taken(s))
        .unwrap()
}

/// 새 메모를 쓴다. 본문의 첫 줄이 제목이 아니면 `# 제목`을 붙인다.
pub fn create(p: &Project, title: &str, body: &str, column: Column) -> Result<Memo> {
    let title = title.trim();
    let body = body.trim();
    if title.is_empty() && body.is_empty() {
        bail!("메모가 비어 있다");
    }
    let title = if title.is_empty() {
        title_of(body, "memo")
    } else {
        title.to_string()
    };
    let stem = free_stem(p, &slugify(&title));
    let mut text = String::new();
    let starts_with_title = body
        .lines()
        .next()
        .is_some_and(|l| l.trim_start().starts_with('#'));
    if !starts_with_title {
        text.push_str(&format!("# {title}\n\n"));
    }
    text.push_str(body);
    text.push('\n');
    let path = p.column_dir(column).join(format!("{stem}.md"));
    crate::write_atomic(&path, text.as_bytes())?;
    read_memo(path, column).context("방금 쓴 메모를 읽지 못했다")
}

/// 메모를 다른 열로 옮긴다(파일 이동). 새 위치를 돌려준다.
pub fn move_to(p: &Project, stem: &str, to: Column) -> Result<Memo> {
    let m = find(p, stem).with_context(|| format!("메모를 찾을 수 없다: {stem}"))?;
    if m.column == to {
        return Ok(m);
    }
    let dest = p.column_dir(to).join(format!("{stem}.md"));
    std::fs::create_dir_all(p.column_dir(to))?;
    if dest.exists() {
        bail!("{}에 같은 이름의 메모가 이미 있다", to.label());
    }
    std::fs::rename(&m.path, &dest)?;
    read_memo(dest, to).context("옮긴 메모를 읽지 못했다")
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use crate::workspace::Workspace;

    pub fn tmpdir(tag: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("jc-{tag}-{}", crate::uuid4()));
        std::fs::create_dir_all(&d).unwrap();
        d
    }

    #[test]
    fn titles_and_previews() {
        assert_eq!(title_of("# 로그인 고치기\n\n본문", "x"), "로그인 고치기");
        assert_eq!(title_of("\n\n그냥 첫 줄\n둘째", "x"), "그냥 첫 줄");
        assert_eq!(title_of("", "stem"), "stem");
        assert_eq!(
            preview_of("# T\n\n- 목표: A\n- 조건: B\n```\ncode\n```"),
            "목표: A 조건: B"
        );
    }

    #[test]
    fn create_move_scan() {
        let tmp = tmpdir("board");
        let ws = Workspace::open(&tmp).unwrap();
        let p = ws.create_project("p", "").unwrap();
        let a = create(&p, "Fix login", "사파리에서 버튼이 안 눌린다", Column::Todo).unwrap();
        assert_eq!(a.stem, "fix-login");
        assert!(a.text.starts_with("# Fix login\n\n"));
        let b = create(&p, "Fix login", "# Fix login\n둘째", Column::Todo).unwrap();
        assert_eq!(b.stem, "fix-login-2");
        assert!(!b.text.contains("# Fix login\n\n# Fix"));
        let moved = move_to(&p, "fix-login", Column::Doing).unwrap();
        assert_eq!(moved.column, Column::Doing);
        assert!(!p.column_dir(Column::Todo).join("fix-login.md").exists());
        let all = scan(&p);
        assert_eq!(all.len(), 2);
        assert_eq!(all[0].column, Column::Todo);
        assert_eq!(find(&p, "fix-login").unwrap().column, Column::Doing);
        // 숨김 파일과 md가 아닌 파일은 메모가 아니다.
        std::fs::write(p.column_dir(Column::Todo).join(".x.md"), "x").unwrap();
        std::fs::write(p.column_dir(Column::Todo).join("notes.txt"), "x").unwrap();
        assert_eq!(scan(&p).len(), 2);
        assert!(create(&p, "", "  ", Column::Todo).is_err());
        let _ = std::fs::remove_dir_all(&tmp);
    }
}
