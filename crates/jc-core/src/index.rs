//! 색인(SQLite) — 정본이 아니라 일지·`task.md`에서 다시 만들 수 있는 투영이다.
//!
//! 지워도 서버가 시작할 때 일지와 `task.md`를 훑어 다시 만든다(아키텍처 ② B2).

use anyhow::{bail, Result};
use rusqlite::Connection;
use sha2::{Digest, Sha256};
use std::collections::HashMap;
use std::fs;
use std::path::{Path, PathBuf};

pub fn sha(b: &[u8]) -> String {
    let d = Sha256::digest(b);
    d.iter().take(12).map(|x| format!("{:02x}", x)).collect()
}

pub const SCHEMA: &str = "
CREATE TABLE IF NOT EXISTS events(
  seq     INTEGER PRIMARY KEY,
  ts      INTEGER NOT NULL,
  topic   TEXT NOT NULL,
  kind    TEXT NOT NULL,
  journal TEXT NOT NULL,
  line    TEXT NOT NULL
);
CREATE INDEX IF NOT EXISTS events_topic ON events(topic, seq);
CREATE TABLE IF NOT EXISTS journals(path TEXT PRIMARY KEY, off INTEGER NOT NULL, last_seq INTEGER NOT NULL);
CREATE TABLE IF NOT EXISTS cards(
  path TEXT PRIMARY KEY, id TEXT, title TEXT, project TEXT,
  stage TEXT, status TEXT, file_hash TEXT, id_hash TEXT, body_hash TEXT
);
CREATE TABLE IF NOT EXISTS meta(k TEXT PRIMARY KEY, v INTEGER);
";

/// 카드 투영 한 줄.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct CardRow {
    pub path: String,
    pub id: String,
    pub title: String,
    pub project: String,
    pub stage: String,
    pub status: String,
    pub file_hash: String,
    pub id_hash: String,
    pub body_hash: String,
}

pub fn index_path(root: &Path) -> PathBuf {
    root.join(".jadecode/index.sqlite")
}

pub fn open_index(root: &Path) -> Result<Connection> {
    fs::create_dir_all(root.join(".jadecode"))?;
    let c = Connection::open(index_path(root))?;
    c.pragma_update(None, "journal_mode", "WAL")?;
    c.pragma_update(None, "synchronous", "NORMAL")?;
    c.busy_timeout(std::time::Duration::from_secs(10))?;
    c.execute_batch(SCHEMA)?;
    Ok(c)
}

pub fn open_index_ro(root: &Path) -> Result<Connection> {
    let c = Connection::open_with_flags(
        index_path(root),
        rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY | rusqlite::OpenFlags::SQLITE_OPEN_NO_MUTEX,
    )?;
    c.busy_timeout(std::time::Duration::from_secs(10))?;
    Ok(c)
}

pub fn remove_index(root: &Path) {
    for s in ["", "-wal", "-shm"] {
        let _ = fs::remove_file(format!("{}{}", index_path(root).display(), s));
    }
}

/// 워크스페이스에 있는 일지·카드 경로(상대 경로).
#[derive(Debug, Clone, Default)]
pub struct Layout {
    pub journals: Vec<String>,
    pub cards: Vec<String>,     // "work/<p>/<c>"
    pub projects: Vec<String>,  // "work/<p>"
}

/// 디렉터리를 훑어 일지·카드를 찾는다. 비재귀(워크트리 안은 보지 않는다).
pub fn scan_layout(root: &Path) -> Result<Layout> {
    let mut l = Layout::default();
    if root.join("events/global.jsonl").exists() {
        l.journals.push("events/global.jsonl".to_string());
    }
    let work = root.join("work");
    if !work.exists() {
        return Ok(l);
    }
    let mut dirs: Vec<PathBuf> = fs::read_dir(&work)?
        .filter_map(|e| e.ok())
        .filter(|e| e.file_type().map(|t| t.is_dir()).unwrap_or(false))
        .map(|e| e.path())
        .collect();
    dirs.sort();
    for p in dirs {
        let pname = p.file_name().unwrap_or_default().to_string_lossy().to_string();
        if pname.starts_with('.') {
            continue;
        }
        let prel = format!("work/{pname}");
        if p.join("project.toml").exists() || p.join("events.jsonl").exists() {
            l.projects.push(prel.clone());
        }
        if p.join("events.jsonl").exists() {
            l.journals.push(format!("{prel}/events.jsonl"));
        }
        if p.join("task.md").exists() {
            // 프로젝트 폴더 자체가 카드인 낡은 모양도 받아 둔다.
            l.cards.push(prel.clone());
        }
        let mut cdirs: Vec<PathBuf> = fs::read_dir(&p)?
            .filter_map(|e| e.ok())
            .filter(|e| e.file_type().map(|t| t.is_dir()).unwrap_or(false))
            .map(|e| e.path())
            .collect();
        cdirs.sort();
        for c in cdirs {
            let cname = c.file_name().unwrap_or_default().to_string_lossy().to_string();
            if cname.starts_with('.') {
                continue;
            }
            let rel = format!("{prel}/{cname}");
            if c.join("events.jsonl").exists() {
                l.journals.push(format!("{rel}/events.jsonl"));
            }
            if c.join("task.md").exists() {
                l.cards.push(rel);
            }
        }
    }
    Ok(l)
}

/// 색인에서 카드 투영을 읽는다.
pub fn card_rows(conn: &Connection) -> Result<Vec<CardRow>> {
    let mut s = conn.prepare(
        "SELECT path,id,title,project,stage,status,file_hash,id_hash,body_hash FROM cards ORDER BY path",
    )?;
    let rows = s
        .query_map([], |r| {
            Ok(CardRow {
                path: r.get(0)?,
                id: r.get(1)?,
                title: r.get(2)?,
                project: r.get(3)?,
                stage: r.get(4)?,
                status: r.get(5)?,
                file_hash: r.get(6)?,
                id_hash: r.get(7)?,
                body_hash: r.get(8)?,
            })
        })?
        .collect::<Result<Vec<_>, _>>()?;
    Ok(rows)
}

/// seq → topic/kind 머리(검사·디버그용).
pub fn seq_heads(conn: &Connection, from: u64, to: u64) -> Result<HashMap<u64, (String, String)>> {
    let mut s = conn.prepare("SELECT seq, topic, kind FROM events WHERE seq BETWEEN ?1 AND ?2")?;
    let mut out = HashMap::new();
    for r in s.query_map([from as i64, to as i64], |r| {
        Ok((r.get::<_, i64>(0)? as u64, r.get::<_, String>(1)?, r.get::<_, String>(2)?))
    })? {
        let (s, t, k) = r?;
        out.insert(s, (t, k));
    }
    Ok(out)
}

/// 색인이 쓸 만한지 보고, 아니면 버린다. 돌려주는 값은 색인이 아는 마지막 seq.
pub fn open_or_rebuild(root: &Path) -> Result<(Connection, u64, bool)> {
    let attempt = || -> Result<(Connection, u64)> {
        let c = open_index(root)?;
        let q: String = c.query_row("PRAGMA quick_check", [], |r| r.get(0))?;
        if q != "ok" {
            bail!("quick_check: {q}");
        }
        let last: i64 = c.query_row(
            "SELECT COALESCE((SELECT v FROM meta WHERE k='last_seq'),0)",
            [],
            |r| r.get(0),
        )?;
        Ok((c, last as u64))
    };
    match attempt() {
        Ok((c, last)) => Ok((c, last, false)),
        Err(e) => {
            eprintln!("index unusable ({e}); rebuilding from journals");
            remove_index(root);
            let (c, last) = attempt()?;
            Ok((c, last, true))
        }
    }
}
