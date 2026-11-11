//! 일지(정본) + 색인(투영) 쓰기 경로와 복구. 단일 쓰기 스레드가 이 구조체를 소유한다.
//!
//! 쓰기 순서(아키텍처 ② B2 쓰기 규칙 1):
//!   seq 부여 → 일지 덧붙이기 → fsync → 색인 커밋 → (그 뒤) 구독자 알림·RPC 승인
//! 복구(규칙 2): 색인의 `journals(path, off)`부터 일지 꼬리를 읽어 색인에 넣는다.
//!   찢긴 마지막 줄은 자르고, seq 빈틈 뒤 줄은 미승인으로 보고 `.jadecode/recovery/`로 옮긴다.
//!   색인이 깨졌으면 묻지 않고 다시 만든다.

use crate::event::{now_ms, parse_line_head, NewEvent};
use crate::index::{self, sha, CardRow};
use crate::taskmd::{card_state, with_front, CardState};
use crate::topic::{card_topic, journal_for_topic};
use anyhow::{anyhow, bail, Context, Result};
use rusqlite::{params, Connection};
use std::collections::{BTreeMap, HashMap};
use std::fs::{self, File, OpenOptions};
use std::io::{Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};
use std::time::Instant;

#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Fsync {
    /// 한 줄마다 fsync (테스트·아주 보수적)
    Always,
    /// 한 묶음(append 호출) 끝에 한 번
    Batch,
    /// 여러 묶음을 모아 한 번 fsync(처리량용). `batch_window`/`batch_max`를 따른다.
    BatchGroup,
    /// fsync 없음(벤치)
    Off,
}

impl std::str::FromStr for Fsync {
    type Err = anyhow::Error;
    fn from_str(s: &str) -> Result<Self> {
        Ok(match s {
            "always" => Fsync::Always,
            "batch" => Fsync::Batch,
            "batch-group" => Fsync::BatchGroup,
            "off" => Fsync::Off,
            _ => bail!("fsync: always|batch|batch-group|off"),
        })
    }
}

#[derive(Debug, Default, serde::Serialize)]
pub struct RecoveryStats {
    pub index_discarded: bool,
    pub index_last_seq_before: u64,
    pub journals: usize,
    pub cards: usize,
    pub tail_lines: usize,
    pub torn_lines_truncated: usize,
    pub torn_bytes: u64,
    pub gap_discarded: usize,
    pub journal_shrunk_reindexed: usize,
    pub stale_dup_skipped: usize,
    pub cards_reconciled: usize,
    pub tmp_files_removed: usize,
    pub last_seq_after: u64,
    pub ms_total: u128,
}

/// 일지 한 줄을 파싱한 것.
struct ParsedLine {
    seq: u64,
    topic: String,
    kind: String,
    line: String,
    journal: String,
    offset: u64,
}

pub struct Store {
    pub root: PathBuf,
    pub conn: Connection,
    /// 워크스페이스당 쓰기 프로세스는 하나다(B2 단일 쓰기 스레드). 다른 프로세스가 열면 여기서 막힌다.
    _lock: File,
    next_seq: u64,
    pub fsync: Fsync,
    /// 일지 상대 경로 → 현재 바이트 길이
    jlen: HashMap<String, u64>,
    files: HashMap<String, File>,
    /// 카드 상대 경로 → 서버가 마지막으로 본/쓴 상태
    known: HashMap<String, CardState>,
    pub stats_self_suppressed: u64,
    pub cas_retries: u64,
    /// BatchGroup에서 fsync를 기다리는 파일(경로).
    dirty: Vec<String>,
    /// BatchGroup의 마지막 fsync 시각.
    batch_started: Option<Instant>,
    /// BatchGroup에서 모인 append 횟수.
    pending_group: usize,
    /// BatchGroup: fsync를 미룰 최대 시간.
    pub batch_window: std::time::Duration,
    /// BatchGroup: fsync 전에 모을 최대 append 횟수.
    pub batch_max: usize,
}

impl Store {
    /// 시작 경로 = 복구 경로.
    pub fn open(root: &Path, fsync: Fsync) -> Result<(Store, RecoveryStats, Vec<NewEvent>)> {
        let t0 = Instant::now();
        let mut st = RecoveryStats::default();
        let lock = acquire_writer_lock(root)?;
        let (mut conn, index_last, discarded) = index::open_or_rebuild(root)?;
        st.index_discarded = discarded;
        st.index_last_seq_before = index_last;

        let mut known_off: HashMap<String, u64> = HashMap::new();
        {
            let mut s = conn.prepare("SELECT path, off FROM journals")?;
            for r in s.query_map([], |r| {
                Ok((r.get::<_, String>(0)?, r.get::<_, i64>(1)? as u64))
            })? {
                let (p, o) = r?;
                known_off.insert(p, o);
            }
        }
        let layout = index::scan_layout(root)?;
        st.journals = layout.journals.len();
        st.cards = layout.cards.len();

        // 1) 일지 꼬리 읽기 + 찢긴 줄 자르기
        let mut tails: Vec<ParsedLine> = vec![];
        let mut jlen: HashMap<String, u64> = HashMap::new();
        let mut reindex: Vec<String> = vec![];
        for j in &layout.journals {
            let path = root.join(j);
            let mut f = OpenOptions::new().read(true).write(true).open(&path)?;
            let len = f.metadata()?.len();
            let mut off = *known_off.get(j).unwrap_or(&0);
            if len < off {
                // 일지가 색인이 아는 것보다 짧다 = 바깥에서 줄었다. 그 일지를 처음부터 다시 색인한다.
                st.journal_shrunk_reindexed += 1;
                reindex.push(j.clone());
                off = 0;
            }
            if len == off {
                jlen.insert(j.clone(), len);
                continue;
            }
            f.seek(SeekFrom::Start(off))?;
            let mut buf = Vec::with_capacity((len - off) as usize);
            f.read_to_end(&mut buf)?;
            let complete = buf
                .iter()
                .rposition(|&b| b == b'\n')
                .map(|i| i + 1)
                .unwrap_or(0);
            if complete < buf.len() {
                st.torn_lines_truncated += 1;
                st.torn_bytes += (buf.len() - complete) as u64;
                save_recovery(root, j, &buf[complete..], "torn")?;
                f.set_len(off + complete as u64)?;
                f.sync_data()?;
            }
            let text = std::str::from_utf8(&buf[..complete])?;
            let mut pos = off;
            for line in text.split_terminator('\n') {
                let (seq, topic, kind) = parse_line_head(line)?;
                tails.push(ParsedLine {
                    seq,
                    topic,
                    kind,
                    line: line.to_string(),
                    journal: j.clone(),
                    offset: pos,
                });
                pos += line.len() as u64 + 1;
            }
            jlen.insert(j.clone(), off + complete as u64);
        }
        st.tail_lines = tails.len();
        tails.sort_unstable_by_key(|t| t.seq);

        // 2) seq 이어 붙이기. 색인이 이미 가진 seq(재색인)와 빈틈 뒤(미승인)를 가른다.
        let fresh = index_last == 0 && known_off.is_empty();
        if fresh {
            // 빈 색인을 채울 때는 WAL에 거대한 트랜잭션을 쌓지 않는다. 죽으면 지우고 다시 만들면 된다.
            conn.pragma_update(None, "journal_mode", "OFF")?;
            conn.pragma_update(None, "synchronous", "OFF")?;
        }
        let tx = conn.transaction()?;
        for j in &reindex {
            tx.execute("DELETE FROM events WHERE journal=?1", [j])?;
        }
        let mut have_upto = index_last;
        let mut accepted: Vec<&ParsedLine> = vec![];
        let mut discard_from: BTreeMap<String, u64> = BTreeMap::new();
        let mut gap = false;
        for t in &tails {
            if gap {
                st.gap_discarded += 1;
                discard_from.entry(t.journal.clone()).or_insert(t.offset);
                continue;
            }
            if t.seq <= have_upto {
                if reindex.contains(&t.journal) {
                    accepted.push(t);
                } else {
                    st.stale_dup_skipped += 1;
                }
                continue;
            }
            if t.seq == have_upto + 1 {
                have_upto = t.seq;
                accepted.push(t);
            } else {
                gap = true;
                st.gap_discarded += 1;
                discard_from.entry(t.journal.clone()).or_insert(t.offset);
            }
        }
        for (j, off) in &discard_from {
            // 빈틈 뒤 줄은 색인 커밋 전이라 아직 승인·발송되지 않았다. 따로 남기고 자른다.
            let path = root.join(j);
            let mut f = OpenOptions::new().read(true).write(true).open(&path)?;
            f.seek(SeekFrom::Start(*off))?;
            let mut rest = vec![];
            f.read_to_end(&mut rest)?;
            save_recovery(root, j, &rest, "gap")?;
            f.set_len(*off)?;
            f.sync_data()?;
            jlen.insert(j.clone(), *off);
        }
        {
            let mut ins = tx.prepare(
                "INSERT OR REPLACE INTO events(seq,ts,topic,kind,journal,line) VALUES(?1,?2,?3,?4,?5,?6)",
            )?;
            for t in &accepted {
                let ts: u64 = serde_json::from_str::<serde_json::Value>(&t.line)
                    .ok()
                    .and_then(|v| v.get("ts").and_then(|x| x.as_u64()))
                    .unwrap_or(0);
                ins.execute(params![
                    t.seq as i64,
                    ts as i64,
                    t.topic,
                    t.kind,
                    t.journal,
                    t.line
                ])?;
            }
            let mut up = tx.prepare(
                "INSERT INTO journals(path,off,last_seq) VALUES(?1,?2,?3)
                 ON CONFLICT(path) DO UPDATE SET off=excluded.off, last_seq=MAX(last_seq, excluded.last_seq)",
            )?;
            let mut lastj: HashMap<&str, u64> = HashMap::new();
            for t in &accepted {
                lastj.insert(t.journal.as_str(), t.seq);
            }
            for (j, len) in &jlen {
                up.execute(params![
                    j,
                    *len as i64,
                    *lastj.get(j.as_str()).unwrap_or(&0) as i64
                ])?;
            }
            tx.execute(
                "INSERT OR REPLACE INTO meta(k,v) VALUES('last_seq',?1)",
                [have_upto as i64],
            )?;
        }
        tx.commit()?;
        if fresh {
            conn.pragma_update(None, "journal_mode", "WAL")?;
            conn.pragma_update(None, "synchronous", "NORMAL")?;
        }

        // 3) task.md 투영 맞추기 — 서버가 꺼진 동안의 변경은 파일이 정본이다.
        let mut known = HashMap::new();
        let mut reconcile_events = vec![];
        {
            let mut prev: HashMap<String, CardRow> = HashMap::new();
            for r in index::card_rows(&conn)? {
                prev.insert(r.path.clone(), r);
            }
            let tx = conn.transaction()?;
            {
                let mut up = tx.prepare(
                    "INSERT OR REPLACE INTO cards(path,id,title,project,stage,status,file_hash,id_hash,body_hash)
                     VALUES(?1,?2,?3,?4,?5,?6,?7,?8,?9)",
                )?;
                for c in &layout.cards {
                    let tmp = root.join(c).join(".task.md.jadecode-tmp");
                    if tmp.exists() {
                        let _ = fs::remove_file(&tmp);
                        st.tmp_files_removed += 1;
                    }
                    let content = match fs::read_to_string(root.join(c).join("task.md")) {
                        Ok(x) => x,
                        Err(e) => {
                            eprintln!("skip {c}/task.md: {e}");
                            continue;
                        }
                    };
                    let cs = match card_state(&content) {
                        Ok(x) => x,
                        Err(e) => {
                            eprintln!("skip {c}/task.md: {e}");
                            continue;
                        }
                    };
                    let row = part_row(c, &cs);
                    match prev.get(c) {
                        Some(old) if old.file_hash == row.file_hash => {}
                        other => {
                            up.execute(params![
                                row.path,
                                row.id,
                                row.title,
                                row.project,
                                row.stage,
                                row.status,
                                row.file_hash,
                                row.id_hash,
                                row.body_hash
                            ])?;
                            if let Some(old) = other {
                                st.cards_reconciled += 1;
                                reconcile_events.push(NewEvent::new(
                                    card_topic(c),
                                    "card.reconciled",
                                    serde_json::json!({
                                        "source":"recovery",
                                        "prev_hash":old.file_hash,
                                        "hash":row.file_hash,
                                        "prev":{"stage":old.stage,"status":old.status},
                                        "now":{"stage":row.stage,"status":row.status},
                                    }),
                                ));
                            }
                        }
                    }
                    known.insert(c.clone(), cs);
                }
            }
            tx.commit()?;
        }
        st.last_seq_after = have_upto;
        st.ms_total = t0.elapsed().as_millis();
        Ok((
            Store {
                root: root.to_path_buf(),
                conn,
                _lock: lock,
                next_seq: have_upto + 1,
                fsync,
                jlen,
                files: HashMap::new(),
                known,
                stats_self_suppressed: 0,
                cas_retries: 0,
                dirty: vec![],
                batch_started: None,
                pending_group: 0,
                batch_window: std::time::Duration::from_millis(20),
                batch_max: 256,
            },
            st,
            reconcile_events,
        ))
    }

    pub fn last_seq(&self) -> u64 {
        self.next_seq - 1
    }

    pub fn known_card(&self, rel: &str) -> Option<&CardState> {
        self.known.get(rel)
    }

    /// 모아 둔 fsync를 지금 한다(종료·검사 전).
    pub fn flush_sync(&mut self) -> Result<()> {
        self.sync_touched()
    }

    fn sync_touched(&mut self) -> Result<()> {
        if self.dirty.is_empty() {
            self.batch_started = None;
            self.pending_group = 0;
            return Ok(());
        }
        let paths: Vec<String> = std::mem::take(&mut self.dirty);
        for j in &paths {
            self.file(j)?.sync_data()?;
        }
        self.batch_started = None;
        self.pending_group = 0;
        Ok(())
    }

    fn file(&mut self, j: &str) -> Result<&mut File> {
        if !self.files.contains_key(j) {
            if self.files.len() > 256 {
                self.files.clear();
            }
            let p = self.root.join(j);
            if let Some(d) = p.parent() {
                fs::create_dir_all(d)?;
            }
            let f = OpenOptions::new().create(true).append(true).open(&p)?;
            self.files.insert(j.to_string(), f);
        }
        Ok(self.files.get_mut(j).unwrap())
    }

    /// 묶음 쓰기. 돌려준 seq는 일지에 fsync되고 색인에 커밋된 것이다 = 승인 가능.
    pub fn append(&mut self, evs: Vec<NewEvent>) -> Result<Vec<u64>> {
        if evs.is_empty() {
            return Ok(vec![]);
        }
        let mut out = Vec::with_capacity(evs.len());
        // (seq, ts, topic, kind, journal, line, end_offset)
        let mut rows: Vec<(u64, u64, String, String, String, String, u64)> =
            Vec::with_capacity(evs.len());
        let mut touched: Vec<String> = vec![];
        let ts = now_ms();
        for e in &evs {
            let seq = self.next_seq;
            let j = journal_for_topic(&e.topic)?;
            let mut line = serde_json::to_string(&serde_json::json!({
                "seq": seq, "ts": ts, "topic": e.topic, "kind": e.kind, "body": e.body
            }))?;
            line.push('\n');
            let fsync = self.fsync;
            let f = self.file(&j)?;
            f.write_all(line.as_bytes())?;
            if fsync == Fsync::Always {
                f.sync_data()?;
            }
            let len = self.jlen.entry(j.clone()).or_insert(0);
            *len += line.len() as u64;
            let end = *len;
            line.pop();
            if !touched.iter().any(|x| x == &j) {
                touched.push(j.clone());
            }
            rows.push((seq, ts, e.topic.clone(), e.kind.clone(), j, line, end));
            out.push(seq);
            self.next_seq += 1;
        }
        if self.fsync == Fsync::Batch {
            // 묶음 안에서 건드린 일지만 fsync한다. 카드 1,000개 워크스페이스에서는 묶음마다
            // 서로 다른 일지가 나오므로, 이건 사실상 "이벤트 1건 = fsync 1회"다.
            // 처리량이 필요하면 BatchGroup을 쓴다(묶음을 여러 개 모아 한 번에 fsync).
            for j in &touched {
                self.file(j)?.sync_data()?;
            }
        }
        if self.fsync == Fsync::BatchGroup {
            // 묶음 쓰기를 여러 번 모아 fsync 한 번으로 끝낸다. 승인(seq 응답)은 fsync 뒤에만
            // 나가므로, 모인 사이의 append는 여기서 한꺼번에 확정된다.
            for j in &touched {
                if !self.dirty.contains(j) {
                    self.dirty.push(j.clone());
                }
            }
            self.pending_group += 1;
            if self.batch_started.is_none() {
                self.batch_started = Some(Instant::now());
            }
            let due = self
                .batch_started
                .map(|t| t.elapsed() >= self.batch_window)
                .unwrap_or(true);
            if due || self.pending_group >= self.batch_max {
                self.sync_touched()?;
            }
        }
        // 색인 커밋
        let tx = self.conn.transaction()?;
        {
            let mut ins = tx.prepare_cached(
                "INSERT INTO events(seq,ts,topic,kind,journal,line) VALUES(?1,?2,?3,?4,?5,?6)",
            )?;
            let mut up = tx.prepare_cached(
                "INSERT INTO journals(path,off,last_seq) VALUES(?1,?2,?3)
                 ON CONFLICT(path) DO UPDATE SET off=excluded.off, last_seq=excluded.last_seq",
            )?;
            for (seq, ts, topic, kind, j, line, end) in &rows {
                ins.execute(params![*seq as i64, *ts as i64, topic, kind, j, line])?;
                up.execute(params![j, *end as i64, *seq as i64])?;
            }
            tx.execute(
                "INSERT OR REPLACE INTO meta(k,v) VALUES('last_seq',?1)",
                [*out.last().unwrap() as i64],
            )?;
        }
        tx.commit()?;
        Ok(out)
    }

    /// 서버 소유 칸(앞머리 `jadecode:`) 쓰기. 먼저 바깥 편집이 있었는지 보고 그 사건을 남긴다.
    pub fn set_front(
        &mut self,
        card: &str,
        f: &crate::taskmd::Front,
        kind: &str,
        body: serde_json::Value,
    ) -> Result<Vec<u64>> {
        let path = self.root.join(card).join("task.md");
        let mut seqs = vec![];
        for _attempt in 0..20 {
            seqs.extend(self.check_card(card, "pre-write")?);
            let content = fs::read_to_string(&path)?;
            let based = sha(content.as_bytes());
            let new = with_front(&content, f)?;
            let cs = card_state(&new)?;
            if !write_cas(&path, &based, new.as_bytes())? {
                self.cas_retries += 1;
                continue;
            }
            self.known.insert(card.to_string(), cs.clone());
            seqs.extend(self.append(vec![NewEvent::new(card_topic(card), kind, body)])?);
            self.upsert_card(card, &cs)?;
            return Ok(seqs);
        }
        bail!("set_front: file keeps changing")
    }

    fn upsert_card(&self, card: &str, cs: &CardState) -> Result<()> {
        let row = part_row(card, cs);
        self.conn.execute(
            "INSERT OR REPLACE INTO cards(path,id,title,project,stage,status,file_hash,id_hash,body_hash)
             VALUES(?1,?2,?3,?4,?5,?6,?7,?8,?9)",
            params![
                row.path, row.id, row.title, row.project, row.stage, row.status, row.file_hash,
                row.id_hash, row.body_hash
            ],
        )?;
        Ok(())
    }

    /// 새로 만든 카드를 색인·known에 등록한다(사건은 만들지 않는다 — card.created가 따로 있다).
    pub fn register_card(&mut self, card: &str) -> Result<()> {
        let content = fs::read_to_string(self.root.join(card).join("task.md"))?;
        let cs = card_state(&content)?;
        self.known.insert(card.to_string(), cs.clone());
        self.upsert_card(card, &cs)
    }

    /// 감시가 알린(또는 쓰기 전에 확인하는) `task.md` 변화 처리. 쓰기 스레드에서만 부른다.
    pub fn check_card(&mut self, card: &str, source: &str) -> Result<Vec<u64>> {
        let path = self.root.join(card).join("task.md");
        let content = match fs::read_to_string(&path) {
            Ok(c) => c,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(vec![]), // 이름 바꾸기 도중
            Err(e) => return Err(e.into()),
        };
        let prev = self.known.get(card).cloned();
        let h = sha(content.as_bytes());
        if let Some(p) = &prev {
            if p.file_hash == h {
                if source == "watch" {
                    self.stats_self_suppressed += 1;
                }
                return Ok(vec![]);
            }
        }
        let cs = match card_state(&content) {
            Ok(c) => c,
            Err(e) => {
                // 반쯤 쓴 파일이거나 앞머리가 깨짐. 다음 알림에서 다시 본다. 사건은 남긴다.
                let seqs = self.append(vec![NewEvent::new(
                    card_topic(card),
                    "card.file_invalid",
                    serde_json::json!({"hash":h,"error":e.to_string(),"source":source}),
                )])?;
                let mut k = prev.unwrap_or_default();
                k.file_hash = h;
                self.known.insert(card.to_string(), k);
                return Ok(seqs);
            }
        };
        let mut evs = vec![];
        let jc_changed = prev
            .as_ref()
            .map(|p| p.id_hash != cs.id_hash)
            .unwrap_or(false);
        let body_changed = prev
            .as_ref()
            .map(|p| p.body_hash != cs.body_hash)
            .unwrap_or(true);
        if body_changed {
            evs.push(NewEvent::new(
                card_topic(card),
                "card.file_changed",
                serde_json::json!({"prev_hash":prev.as_ref().map(|p| p.file_hash.clone()),"hash":h,"source":source}),
            ));
        }
        let mut final_cs = cs.clone();
        if jc_changed {
            let p = prev.as_ref().unwrap();
            // 서버 소유 칸을 바깥에서 바꿈 → 거절하고 되돌린다(종료 조건 검사 전).
            let restored = with_front(&content, &p.front)?;
            final_cs = card_state(&restored)?;
            if !write_cas(&path, &h, restored.as_bytes())? {
                // 되돌리는 사이 또 바뀜: 이번 내용의 사건만 남기고 다음 확인에서 다시 본다.
                self.cas_retries += 1;
                let mut k = p.clone();
                k.file_hash = h.clone();
                k.body_hash = cs.body_hash.clone();
                self.known.insert(card.to_string(), k);
                if evs.is_empty() {
                    return Ok(vec![]);
                }
                let seqs = self.append(evs)?;
                self.upsert_card(card, &final_cs)?;
                let more = self.check_card(card, source)?;
                return Ok([seqs, more].concat());
            }
            self.known.insert(card.to_string(), final_cs.clone());
            evs.push(NewEvent::new(
                card_topic(card),
                "card.state_change_rejected",
                serde_json::json!({
                    "attempted":{"stage":cs.stage(),"status":cs.status()},
                    "kept":{"stage":p.stage(),"status":p.status()},
                    "source":source
                }),
            ));
        } else {
            self.known.insert(card.to_string(), final_cs.clone());
        }
        if evs.is_empty() {
            return Ok(vec![]);
        }
        let seqs = self.append(evs)?;
        self.upsert_card(card, &final_cs)?;
        Ok(seqs)
    }
}

/// 워크스페이스 쓰기 잠금. 서버가 살아 있는 동안 CLI가 직접 쓰면 seq가 어긋난다
/// (두 프로세스가 각자 `next_seq`를 들고 색인에 쓰면 `UNIQUE constraint failed: events.seq`).
pub fn acquire_writer_lock(root: &Path) -> Result<File> {
    use std::os::unix::io::AsRawFd;
    let dir = root.join(".jadecode");
    fs::create_dir_all(&dir)?;
    let p = dir.join("writer.lock");
    // 잠금 파일은 비우면 안 된다(다른 프로세스가 잡고 있을 수 있다). truncate 하지 않는다.
    let f = OpenOptions::new()
        .create(true)
        .truncate(false)
        .read(true)
        .write(true)
        .open(&p)?;
    let r = unsafe { libc::flock(f.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) };
    if r != 0 {
        let e = std::io::Error::last_os_error();
        if e.raw_os_error() == Some(libc::EWOULDBLOCK) {
            bail!(
                "workspace is already open for writing by another process ({}). 서버가 돌고 있으면 RPC를 써라: jadecode rpc …",
                p.display()
            );
        }
        return Err(anyhow!("flock: {e}"));
    }
    Ok(f)
}

fn part_row(rel: &str, cs: &CardState) -> CardRow {
    CardRow {
        path: rel.to_string(),
        id: cs.front.id.clone(),
        title: cs.front.title.clone(),
        project: cs.front.project.clone(),
        stage: cs.front.stage.clone(),
        status: cs.front.status.clone(),
        file_hash: cs.file_hash.clone(),
        id_hash: cs.id_hash.clone(),
        body_hash: cs.body_hash.clone(),
    }
}

/// 비교 후 교체: tmp를 다 쓰고 fsync한 뒤, `task.md`가 아직 `based`인지 보고 그때만 맞바꾼다.
/// 맞바꾸기(`RENAME_EXCHANGE`)를 쓰므로 밀려난 내용을 사후에도 확인할 수 있다.
/// 그냥 tmp + rename은 동시 편집에서 에이전트 편집을 잃었다(S5 4).
pub fn write_cas(path: &Path, based: &str, data: &[u8]) -> Result<bool> {
    let tmp = path.with_file_name(".task.md.jadecode-tmp");
    {
        let mut f = File::create(&tmp)?;
        f.write_all(data)?;
        f.sync_data()?;
    }
    let cur = fs::read(path).unwrap_or_default();
    if sha(&cur) != based {
        let _ = fs::remove_file(&tmp);
        return Ok(false);
    }
    if !exchange(&tmp, path)? {
        fs::rename(&tmp, path)?;
        return Ok(true);
    }
    let displaced = fs::read(&tmp)?;
    if sha(&displaced) == based {
        let _ = fs::remove_file(&tmp);
        return Ok(true);
    }
    // 확인과 맞바꾸기 사이에 누가 썼다 → 그 내용을 되돌려 놓고 실패로 알린다.
    let _ = exchange(&tmp, path)?;
    let _ = fs::remove_file(&tmp);
    Ok(false)
}

/// Linux `renameat2(RENAME_EXCHANGE)`, macOS `renamex_np(RENAME_SWAP)`.
/// 지원하지 않으면 `Ok(false)`(호출자가 rename으로 물러난다).
#[cfg(target_os = "linux")]
fn exchange(a: &Path, b: &Path) -> Result<bool> {
    use std::os::unix::ffi::OsStrExt;
    let ca = std::ffi::CString::new(a.as_os_str().as_bytes())?;
    let cb = std::ffi::CString::new(b.as_os_str().as_bytes())?;
    let r = unsafe {
        libc::renameat2(
            libc::AT_FDCWD,
            ca.as_ptr(),
            libc::AT_FDCWD,
            cb.as_ptr(),
            libc::RENAME_EXCHANGE,
        )
    };
    if r == 0 {
        return Ok(true);
    }
    let e = std::io::Error::last_os_error();
    match e.raw_os_error() {
        Some(libc::EINVAL) | Some(libc::ENOSYS) | Some(libc::ENOTSUP) => Ok(false),
        _ => Err(anyhow!("renameat2: {e}")),
    }
}

#[cfg(not(target_os = "linux"))]
fn exchange(_a: &Path, _b: &Path) -> Result<bool> {
    // macOS: renamex_np(a, b, RENAME_SWAP)를 쓸 자리. Linux에서만 확인했다(구현 중 확인).
    Ok(false)
}

fn save_recovery(root: &Path, j: &str, bytes: &[u8], why: &str) -> Result<()> {
    let d = root.join(".jadecode/recovery");
    fs::create_dir_all(&d)?;
    // 복구 기록은 덧붙이기만 한다(open_options: create + append).
    let mut f = OpenOptions::new()
        .create(true)
        .append(true)
        .open(d.join(format!("{why}.log")))?;
    writeln!(f, "# {} {} {} bytes", now_ms(), j, bytes.len())?;
    f.write_all(bytes)?;
    writeln!(f)?;
    Ok(())
}

// ---------- 전수 검사 ----------

#[derive(Debug, Default, serde::Serialize)]
pub struct CheckReport {
    pub journal_events: usize,
    pub index_events: usize,
    pub last_seq: u64,
    pub seq_gaps: usize,
    pub seq_dups: usize,
    pub missing_in_index: usize,
    pub extra_in_index: usize,
    pub line_mismatch: usize,
    pub journal_offset_mismatch: usize,
    pub card_projection_mismatch: usize,
    pub ok: bool,
}

/// 일지를 처음부터 따로 읽어 색인과 줄 단위로 대조한다.
pub fn full_check(root: &Path) -> Result<CheckReport> {
    let mut r = CheckReport::default();
    let conn = index::open_index(root)?;
    let layout = index::scan_layout(root)?;
    let mut jmap: HashMap<u64, (String, String)> = HashMap::new();
    for j in &layout.journals {
        let text = fs::read_to_string(root.join(j))?;
        if !text.is_empty() && !text.ends_with('\n') {
            r.line_mismatch += 1; // 찢긴 줄이 남음
        }
        for line in text.split_terminator('\n') {
            let (seq, _, _) = parse_line_head(line)?;
            if jmap.insert(seq, (j.clone(), line.to_string())).is_some() {
                r.seq_dups += 1;
            }
        }
        let off: Option<i64> = conn
            .query_row("SELECT off FROM journals WHERE path=?1", [j], |x| x.get(0))
            .ok();
        if off != Some(text.len() as i64) {
            r.journal_offset_mismatch += 1;
        }
    }
    r.journal_events = jmap.len();
    let max = jmap.keys().max().copied().unwrap_or(0);
    r.last_seq = max;
    for s in 1..=max {
        if !jmap.contains_key(&s) {
            r.seq_gaps += 1;
        }
    }
    let mut s = conn.prepare("SELECT seq, journal, line FROM events")?;
    for row in s.query_map([], |x| {
        Ok((
            x.get::<_, i64>(0)? as u64,
            x.get::<_, String>(1)?,
            x.get::<_, String>(2)?,
        ))
    })? {
        let (seq, j, line) = row?;
        r.index_events += 1;
        match jmap.get(&seq) {
            None => r.extra_in_index += 1,
            Some((jj, l)) => {
                if *jj != j || *l != line {
                    r.line_mismatch += 1;
                }
            }
        }
    }
    let have: std::collections::HashSet<u64> = {
        let mut st = conn.prepare("SELECT seq FROM events")?;
        let rows: Vec<u64> = st
            .query_map([], |x| x.get::<_, i64>(0))?
            .filter_map(|x| x.ok())
            .map(|x| x as u64)
            .collect();
        rows.into_iter().collect()
    };
    for k in jmap.keys() {
        if !have.contains(k) {
            r.missing_in_index += 1;
        }
    }
    for c in &layout.cards {
        let content = fs::read_to_string(root.join(c).join("task.md"))?;
        let cs = card_state(&content)?;
        let row: Option<(String, String, String)> = conn
            .query_row(
                "SELECT file_hash, stage, status FROM cards WHERE path=?1",
                [c],
                |x| Ok((x.get(0)?, x.get(1)?, x.get(2)?)),
            )
            .ok();
        if row
            != Some((
                cs.file_hash.clone(),
                cs.front.stage.clone(),
                cs.front.status.clone(),
            ))
        {
            r.card_projection_mismatch += 1;
        }
    }
    r.ok = r.seq_gaps == 0
        && r.seq_dups == 0
        && r.missing_in_index == 0
        && r.extra_in_index == 0
        && r.line_mismatch == 0
        && r.journal_offset_mismatch == 0
        && r.card_projection_mismatch == 0;
    Ok(r)
}

pub fn card_topic_of(rel: &str) -> String {
    card_topic(rel)
}

/// 최근 이벤트(색인에서 읽는다). 서버 없이 CLI `tail`이 쓴다.
pub fn read_tail(root: &Path, topics: &[String], limit: u64) -> Result<Vec<serde_json::Value>> {
    let conn = index::open_index_ro(root)?;
    let (cond, args) = crate::topic::topic_sql(topics, 2);
    let last: i64 = conn
        .query_row(
            "SELECT COALESCE((SELECT v FROM meta WHERE k='last_seq'),0)",
            [],
            |r| r.get(0),
        )
        .unwrap_or(0);
    let sql = format!(
        "SELECT line FROM events WHERE seq <= ?1 AND ({cond}) ORDER BY seq DESC LIMIT {limit}"
    );
    let mut st = conn.prepare(&sql)?;
    let mut p: Vec<Box<dyn rusqlite::ToSql>> = vec![Box::new(last)];
    for a in &args {
        p.push(Box::new(a.clone()));
    }
    let pr: Vec<&dyn rusqlite::ToSql> = p.iter().map(|b| b.as_ref()).collect();
    let mut v: Vec<serde_json::Value> = st
        .query_map(pr.as_slice(), |r| r.get::<_, String>(0))?
        .filter_map(|x| x.ok())
        .filter_map(|l| serde_json::from_str(&l).ok())
        .collect();
    v.reverse();
    Ok(v)
}

#[allow(dead_code)]
fn _ctx<T>(r: Result<T>) -> Result<T> {
    r.context("store")
}
