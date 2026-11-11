//! 성능 측정. 재는 것과 못 재는 것을 나눈다.
//!
//! **여기서 재는 것(CPU·디스크 경로)**
//! - 일지 덧붙이기 처리량(단건/묶음, fsync 켜고/끄고)
//! - 색인 재생성(차가운/따뜻한), `task.md` 투영
//! - 전수 검사(`check`) 시간
//! - 구독 팬아웃(구독자 수·지연)
//! - 메모리(RSS 최고치)
//! - 클라이언트 데이터 계층 비용(스냅숏 복제·이벤트 반영)
//!
//! **여기서 못 재는 것**
//! - GPUI 프레임 시간·FPS (이 컨테이너에 GPU가 없다 — `/dev/dri` 없음)
//! - Mac 수치(FSEvents 지연, APFS `F_FULLFSYNC`, `renamex_np`)
//!
//! 사용:
//! ```text
//! jc-bench gen <dir> [--projects N] [--cards N] [--events N]
//! jc-bench append <dir> [--events N] [--batch N] [--fsync batch|off|always]
//! jc-bench rebuild <dir>
//! jc-bench check <dir>
//! jc-bench subscribe <dir> [--subscribers N] [--events N]
//! jc-bench client <dir> [--iterations N]
//! jc-bench all <dir> [--projects N] [--cards N] [--events N]
//! ```
//! 결과는 JSON 한 줄씩 찍는다(스크립트가 모으기 좋게).

use anyhow::{bail, Context, Result};
use jc_core::store::{full_check, Fsync, Store};
use jc_core::{index, NewEvent};
use rand::Rng;
use serde_json::{json, Value};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::time::Instant;

const FILLER: &str = "워커가 구현을 마치고 리뷰 단계로 넘겼다. tests passed 41/41, lint clean. \
다음 행동: 리뷰 봇에게 넘기고 사람 절차를 기다린다. ";

fn arg<T: std::str::FromStr>(args: &[String], name: &str, def: T) -> T {
    args.iter()
        .position(|a| a == name)
        .and_then(|i| args.get(i + 1))
        .and_then(|v| v.parse().ok())
        .unwrap_or(def)
}

/// 프로세스 최고 RSS(KB). 리눅스 전용.
pub fn peak_rss_kb() -> u64 {
    std::fs::read_to_string("/proc/self/status")
        .ok()
        .and_then(|s| {
            s.lines()
                .find(|l| l.starts_with("VmHWM:"))
                .and_then(|l| l.split_whitespace().nth(1)?.parse().ok())
        })
        .unwrap_or(0)
}

fn now_ms() -> u64 {
    jc_core::event::now_ms()
}

fn card_md(i: usize, p: usize) -> String {
    format!(
        "---\njadecode:\n  id: 01S5CARD{i:018}\n  project: p{p}\n  title: 카드 {i}\n  created: 2026-11-11\n  path: [구체화, 구현, 검증]\n  stage: 구현\n  status: idle\n---\n# 카드 {i}\n\n## 목표\n\n카드 {i}의 목표. {FILLER}\n\n## 완료 조건\n\n- 테스트 통과\n\n## 다음 행동\n\n"
    )
}

/// 카드 N개(프로젝트 M개)를 만들고, 일지에 이벤트 K개를 미리 넣는다(색인 없이 파일만).
fn gen(root: &Path, projects: usize, cards: usize, events: usize) -> Result<Value> {
    if root.join("work").exists() && std::fs::read_dir(root.join("work"))?.next().is_some() {
        bail!("{} 에 이미 work/ 가 있다. 새 폴더를 쓰라", root.display());
    }
    let t0 = Instant::now();
    std::fs::create_dir_all(root.join("events"))?;
    std::fs::create_dir_all(root.join("work"))?;
    std::fs::write(
        root.join("jadecode.toml"),
        "name = \"bench\"\nversion = \"0.1\"\n",
    )?;

    let mut rels: Vec<(usize, String)> = vec![];
    for p in 0..projects {
        std::fs::create_dir_all(root.join(format!("work/p{p}")))?;
        std::fs::write(
            root.join(format!("work/p{p}/project.toml")),
            format!("name = \"p{p}\"\nslug = \"p{p}\"\n"),
        )?;
    }
    for i in 0..cards {
        let p = i % projects;
        let rel = format!("work/p{p}/{i:05}-card-{i}");
        std::fs::create_dir_all(root.join(&rel))?;
        std::fs::write(root.join(&rel).join("task.md"), card_md(i, p))?;
        rels.push((p, rel));
    }
    let gen_ms = t0.elapsed().as_millis();

    // 일지: 카드에 90%, 프로젝트 7%, 그 밖 3%
    let t1 = Instant::now();
    let mut files: std::collections::HashMap<String, std::fs::File> = Default::default();
    let mut rng = rand::rng();
    let kinds = [
        "msg.report",
        "msg.ask",
        "instance.output",
        "card.stage_changed",
        "msg.reply",
    ];
    let body: String = FILLER.chars().cycle().take(240).collect();
    for seq in 1..=events as u64 {
        let r: f64 = rng.random();
        let topic = if r < 0.90 && !rels.is_empty() {
            let (_, rel) = &rels[rng.random_range(0..rels.len())];
            jc_core::topic::card_topic(rel)
        } else if r < 0.97 {
            format!("project/p{}", rng.random_range(0..projects))
        } else {
            "global".to_string()
        };
        let kind = kinds[rng.random_range(0..kinds.len())];
        let j = jc_core::journal_for_topic(&topic)?;
        let line = json!({"seq": seq, "ts": now_ms(), "topic": topic, "kind": kind,
            "body": {"text": body, "n": seq}})
        .to_string()
            + "\n";
        let f = files.entry(j.clone()).or_insert_with(|| {
            let p = root.join(&j);
            if let Some(d) = p.parent() {
                let _ = std::fs::create_dir_all(d);
            }
            std::fs::OpenOptions::new()
                .create(true)
                .append(true)
                .open(&p)
                .unwrap()
        });
        f.write_all(line.as_bytes())?;
    }
    for f in files.values_mut() {
        f.sync_data().ok();
    }
    let write_ms = t1.elapsed().as_millis();

    Ok(json!({
        "bench": "gen",
        "projects": projects, "cards": cards, "events": events,
        "gen_ms": gen_ms, "journal_write_ms": write_ms,
        "files": index::scan_layout(root)?.journals.len(),
    }))
}

/// 덧붙이기 처리량. `batch`개 이벤트를 한 번에 넣는다.
fn append(root: &Path, events: usize, batch: usize, fsync: Fsync) -> Result<Value> {
    let t = Instant::now();
    let (mut st, stats, _) = Store::open(root, fsync)?;
    let open_ms = t.elapsed().as_millis();
    let topics: Vec<String> = {
        let l = index::scan_layout(root)?;
        l.cards
            .iter()
            .take(200)
            .map(|c| jc_core::topic::card_topic(c))
            .collect()
    };
    if topics.is_empty() {
        bail!("카드가 없다. gen 먼저");
    }
    let body: String = FILLER.chars().cycle().take(240).collect();
    let mut done = 0usize;
    let t2 = Instant::now();
    while done < events {
        let n = batch.min(events - done);
        let mut evs = Vec::with_capacity(n);
        for k in 0..n {
            evs.push(NewEvent::new(
                topics[(done + k) % topics.len()].clone(),
                "msg.report",
                json!({"text": body, "n": done + k}),
            ));
        }
        st.append(evs)?;
        done += n;
    }
    let wall = t2.elapsed();
    let secs = wall.as_secs_f64().max(1e-9);
    Ok(json!({
        "bench": "append", "events": events, "batch": batch,
        "fsync": format!("{fsync:?}"),
        "wall_ms": wall.as_millis(), "ev_per_sec": (events as f64 / secs).round(),
        "ms_per_event": (wall.as_millis() as f64 / events as f64 * 1000.0).round() / 1000.0,
        "open_recovery_ms": open_ms, "recovery": stats,
        "peak_rss_kb": peak_rss_kb(),
    }))
}

/// 색인 재생성. 차가운(색인 삭제)과 따뜻한(그대로 재시작).
fn rebuild(root: &Path) -> Result<Value> {
    let hot_ms = {
        let t = Instant::now();
        let (_st, stats, _) = Store::open(root, Fsync::Batch)?;
        (t.elapsed().as_millis(), stats)
    };
    index::remove_index(root);
    let t = Instant::now();
    let (_st, stats, _) = Store::open(root, Fsync::Batch)?;
    let cold_ms = t.elapsed().as_millis();
    let size = std::fs::metadata(index::index_path(root))
        .map(|m| m.len())
        .unwrap_or(0);
    let events: i64 = {
        let c = index::open_index_ro(root)?;
        c.query_row("SELECT COUNT(*) FROM events", [], |r| r.get(0))
            .unwrap_or(0)
    };
    Ok(json!({
        "bench": "rebuild",
        "cold_ms": cold_ms, "hot_ms": hot_ms.0,
        "journal_lines": stats.tail_lines, "cards": stats.cards,
        "index_events": events,
        "index_bytes": size,
        "hot_recovery": hot_ms.1,
        "peak_rss_kb": peak_rss_kb(),
    }))
}

fn check(root: &Path) -> Result<Value> {
    let t = Instant::now();
    let r = full_check(root)?;
    Ok(json!({"bench": "check", "ms": t.elapsed().as_millis(), "report": r}))
}

/// 구독 팬아웃: N개 구독을 붙이고 이벤트를 넣어 배달 지연을 잰다.
async fn subscribe(root: &Path, subscribers: usize, events: usize) -> Result<Value> {
    // 서버 프로세스가 없으므로 소켓 대신 색인 읽기 경로를 직접 잰다:
    // 구독 한 명이 from_seq 뒤를 읽는 비용 = 팬아웃 1회 비용.
    let conn = index::open_index_ro(root)?;
    let last: i64 = conn.query_row(
        "SELECT COALESCE((SELECT v FROM meta WHERE k='last_seq'),0)",
        [],
        |r| r.get(0),
    )?;
    let from = (last as u64).saturating_sub(events as u64);
    let t = Instant::now();
    let mut per: Vec<u128> = vec![];
    for _ in 0..subscribers {
        let t1 = Instant::now();
        let mut st = conn
            .prepare_cached("SELECT seq, line FROM events WHERE seq > ?1 ORDER BY seq LIMIT 500")?;
        let _rows: Vec<(i64, String)> = st
            .query_map([from as i64], |r| Ok((r.get(0)?, r.get(1)?)))?
            .filter_map(|x| x.ok())
            .collect();
        per.push(t1.elapsed().as_micros());
    }
    let total = t.elapsed();
    per.sort_unstable();
    Ok(json!({
        "bench": "subscribe", "subscribers": subscribers, "events_read": events,
        "total_ms": total.as_millis(),
        "per_sub_us_p50": per.get(per.len() / 2).copied().unwrap_or(0),
        "per_sub_us_max": per.last().copied().unwrap_or(0),
        "peak_rss_kb": peak_rss_kb(),
    }))
}

/// 클라이언트 데이터 계층 비용: 스냅숏 복제 + 이벤트 반영.
///
/// 화면이 프레임마다 `snap()`을 부르므로, 이 비용이 곧 UI 프레임 예산을 먹는다.
fn client_cost(root: &Path, iterations: usize) -> Result<Value> {
    use jc_client::{Board, BoardSnapshot, Card, Connection};
    let cards = jc_core::index::scan_layout(root)?.cards;
    let mut snap = BoardSnapshot::default();
    for (i, c) in cards.iter().enumerate() {
        snap.cards.push(Card {
            path: c.clone(),
            id: format!("01S5CARD{i:018}"),
            title: format!("카드 {i}"),
            project: c.split('/').nth(1).unwrap_or("p0").to_string(),
            stage: "구현".into(),
            status: "idle".into(),
        });
    }
    snap.projects.push(jc_client::board::Project {
        slug: "p0".into(),
        name: "p0".into(),
        cards: cards.len() as u64,
    });
    for i in 0..500 {
        snap.messages.push(jc_client::board::EventRow {
            seq: i as u64 + 1,
            ts: now_ms(),
            topic: cards
                .get(i % cards.len().max(1))
                .cloned()
                .unwrap_or_else(|| "global".into()),
            kind: "msg.report".into(),
            body: json!({"summary": FILLER.repeat(2), "actor": {"instance": "w-1"}}),
        });
    }
    // 1a) 값 복제(예전 구현: 프레임마다 전체 clone)
    let t = Instant::now();
    let mut sink = 0usize;
    for _ in 0..iterations {
        let s: BoardSnapshot = snap.clone();
        sink += s.cards.len() + s.messages.len();
    }
    let deep_clone_us = t.elapsed().as_micros() as f64 / iterations as f64;

    // 1b) 핸들 복제(지금 구현: Arc 사본)
    let handle = jc_client::SnapshotHandle::new(snap.clone());
    let shared = std::sync::Arc::new(std::sync::Mutex::new(handle));
    let t = Instant::now();
    for _ in 0..iterations {
        let h = shared.lock().map(|s| s.clone()).unwrap_or_default();
        sink += h.cards.len() + h.messages.len();
    }
    let handle_clone_us = t.elapsed().as_micros() as f64 / iterations as f64;

    // 2) 이벤트 반영 비용
    let mut board = Board::new_for_bench();
    let t = Instant::now();
    for i in 0..iterations {
        let ev = jc_client::board::EventRow {
            seq: 1_000_000 + i as u64,
            ts: now_ms(),
            topic: cards
                .get(i % cards.len().max(1))
                .cloned()
                .map(|c| {
                    let s: Vec<&str> = c.split('/').collect();
                    format!("project/{}/card/{}", s[1], s[2])
                })
                .unwrap_or_else(|| "global".into()),
            kind: "card.status_changed".into(),
            body: json!({"from": "idle", "to": "working"}),
        };
        board.apply(&ev);
    }
    let apply_us = t.elapsed().as_micros() as f64 / iterations as f64;
    let _ = sink;
    let _ = Connection {
        socket: PathBuf::from(""),
    };
    Ok(json!({
        "bench": "client",
        "cards": snap.cards.len(), "messages": snap.messages.len(),
        "iterations": iterations,
        "snapshot_deep_clone_us": (deep_clone_us * 100.0).round() / 100.0,
        "snapshot_handle_us": (handle_clone_us * 1000.0).round() / 1000.0,
        "speedup_x": (deep_clone_us / handle_clone_us.max(0.001) * 10.0).round() / 10.0,
        "event_apply_us": (apply_us * 100.0).round() / 100.0,
        "frame_budget_16ms_us": 16000,
        "note": "snapshot_deep_clone_us(예전) vs snapshot_handle_us(지금). 프레임 예산 16000us.",
    }))
}

#[tokio::main(flavor = "current_thread")]
async fn main() -> Result<()> {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let Some(cmd) = args.first().map(|s| s.as_str()) else {
        bail!("usage: jc-bench <gen|append|rebuild|check|subscribe|client|all> <dir> [...]");
    };
    let dir = PathBuf::from(args.get(1).context("dir 필요")?);
    let out = match cmd {
        "gen" => gen(
            &dir,
            arg(&args, "--projects", 12),
            arg(&args, "--cards", 1000),
            arg(&args, "--events", 100_000),
        )?,
        "append" => append(
            &dir,
            arg(&args, "--events", 20_000),
            arg(&args, "--batch", 1),
            match arg(&args, "--fsync", "batch".to_string()).as_str() {
                "off" => Fsync::Off,
                "always" => Fsync::Always,
                "batch-group" => Fsync::BatchGroup,
                _ => Fsync::Batch,
            },
        )?,
        "rebuild" => rebuild(&dir)?,
        "check" => check(&dir)?,
        "subscribe" => {
            subscribe(
                &dir,
                arg(&args, "--subscribers", 8),
                arg(&args, "--events", 500),
            )
            .await?
        }
        "client" => client_cost(&dir, arg(&args, "--iterations", 200))?,
        "all" => {
            let projects = arg(&args, "--projects", 12);
            let cards = arg(&args, "--cards", 1000);
            let events = arg(&args, "--events", 100_000);
            println!("{}", gen(&dir, projects, cards, events)?);
            println!("{}", rebuild(&dir)?);
            for (batch, fsync) in [
                (1, Fsync::Batch),
                (10, Fsync::Batch),
                (100, Fsync::Batch),
                (100, Fsync::Off),
            ] {
                println!(
                    "{}",
                    append(&dir, arg(&args, "--append", 20_000), batch, fsync)?
                );
            }
            println!("{}", subscribe(&dir, 8, 500).await?);
            println!("{}", client_cost(&dir, 200)?);
            println!("{}", check(&dir)?);
            Value::Null
        }
        other => bail!("unknown command {other}"),
    };
    if !out.is_null() {
        println!("{}", out);
    }
    Ok(())
}
