//! 저장소 동작: 덧붙이기·복구·재색인·동시 편집·외부 상태 칸 변경 거절.

use jc_core::index::{card_rows, remove_index, scan_layout};
use jc_core::store::{full_check, Fsync, Store};
use jc_core::taskmd::card_state;
use jc_core::{NewCard, Workspace};

fn tmp() -> std::path::PathBuf {
    jc_core::short_temp_root("jcs")
}

fn workspace() -> (Workspace, std::path::PathBuf) {
    let d = tmp();
    let (ws, _) = Workspace::init(&d, "test").unwrap();
    (ws, d)
}

fn add_cards(ws: &mut Workspace, path: &[&str], n: usize) -> Vec<jc_core::workspace::CardEntry> {
    ws.add_project("p1", None).unwrap();
    for i in 0..n {
        ws.add_card(NewCard {
            project: "p1".into(),
            title: format!("card {i}"),
            path: path.iter().map(|s| s.to_string()).collect(),
            ..Default::default()
        })
        .unwrap();
    }
    ws.cards().unwrap()
}

#[test]
fn append_assigns_contiguous_seq_and_writes_journals() {
    let (mut ws, d) = workspace();
    let cards = add_cards(&mut ws, &["refine"], 2);
    let mut store = ws.store_with(Fsync::Always).unwrap();
    let before = store.last_seq();
    let seqs = store
        .append(vec![
            jc_core::NewEvent::new(
                jc_core::topic::card_topic(&cards[0].rel),
                "msg.report",
                serde_json::json!({"text":"done"}),
            ),
            jc_core::NewEvent::new(
                jc_core::topic::project_topic("work/p1"),
                "project.note",
                serde_json::json!({"text":"hi"}),
            ),
        ])
        .unwrap();
    assert_eq!(seqs, vec![before + 1, before + 2]);
    let j = std::fs::read_to_string(d.join(&cards[0].rel).join("events.jsonl")).unwrap();
    assert!(j.contains("msg.report"), "{j}");
    let pj = std::fs::read_to_string(d.join("work/p1/events.jsonl")).unwrap();
    assert!(pj.contains("project.note"), "{pj}");
    // 줄마다 seq가 있다
    for line in j.lines() {
        let v: serde_json::Value = serde_json::from_str(line).unwrap();
        assert!(v["seq"].is_u64());
        assert!(v["ts"].is_u64());
        assert!(v["topic"].is_string());
        assert!(v["kind"].is_string());
    }
    drop(store);
    let r = full_check(&d).unwrap();
    assert!(r.ok, "{r:?}");
}

#[test]
fn reopen_keeps_seq_and_loads_cards() {
    let (mut ws, d) = workspace();
    add_cards(&mut ws, &["refine", "implement"], 1);
    {
        let mut s = ws.store().unwrap();
        s.append(vec![jc_core::NewEvent::new("global", "bot.created", serde_json::json!({}))])
            .unwrap();
        assert_eq!(s.last_seq(), 4); // 프로젝트 생성 1 + 카드 생성 2 + 전역 1
    }
    let (s2, stats, _) = Store::open(&d, Fsync::Batch).unwrap();
    assert_eq!(s2.last_seq(), 4, "{stats:?}");
    assert_eq!(stats.tail_lines, 0, "이미 색인된 줄은 다시 읽지 않는다");
    assert_eq!(stats.cards, 1);
    assert!(!s2.known_card(&ws.cards().unwrap()[0].rel).unwrap().front.id.is_empty());
}

#[test]
fn torn_last_line_is_truncated_and_kept_in_recovery() {
    let (mut ws, d) = workspace();
    let cards = add_cards(&mut ws, &["refine"], 1);
    {
        let mut s = ws.store().unwrap();
        s.append(vec![jc_core::NewEvent::new(
            jc_core::topic::card_topic(&cards[0].rel),
            "msg.report",
            serde_json::json!({"n":1}),
        )])
        .unwrap();
    }
    // 일지 끝에 잘린 줄을 붙인다(전원 손실 흉내).
    let jp = d.join(&cards[0].rel).join("events.jsonl");
    let before = std::fs::read_to_string(&jp).unwrap();
    let mut f = std::fs::OpenOptions::new().append(true).open(&jp).unwrap();
    use std::io::Write;
    f.write_all(br#"{"seq":99,"ts":1,"topic":"x","kind":"y","body":{"h"#)
        .unwrap();
    f.sync_all().unwrap();
    drop(f);

    let (s, stats, _) = Store::open(&d, Fsync::Batch).unwrap();
    assert_eq!(stats.torn_lines_truncated, 1, "{stats:?}");
    assert!(stats.torn_bytes > 0);
    assert_eq!(s.last_seq(), 4, "찢긴 줄은 seq를 먹지 않는다");
    let after = std::fs::read_to_string(&jp).unwrap();
    assert_eq!(after, before, "찢긴 조각은 잘라낸다");
    let rec = std::fs::read_to_string(d.join(".jadecode/recovery/torn.log")).unwrap();
    assert!(rec.contains("\"seq\":99"), "{rec}");
    let r = full_check(&d).unwrap();
    assert!(r.ok, "{r:?}");
}

#[test]
fn seq_gap_after_index_is_discarded_as_unapproved() {
    let (mut ws, d) = workspace();
    let cards = add_cards(&mut ws, &["refine"], 1);
    {
        let mut s = ws.store().unwrap();
        s.append(vec![jc_core::NewEvent::new("global", "a", serde_json::json!({}))])
            .unwrap();
    }
    // 색인에 아직 없는 사이(승인 전)에 seq가 뛴 줄이 들어간 상황을 만든다.
    let jp = d.join("events/global.jsonl");
    let mut f = std::fs::OpenOptions::new().append(true).open(&jp).unwrap();
    use std::io::Write;
    f.write_all(b"{\"seq\":9,\"ts\":1,\"topic\":\"global\",\"kind\":\"ghost\",\"body\":{}}\n")
        .unwrap();
    f.sync_all().unwrap();
    let (s, stats, _) = Store::open(&d, Fsync::Batch).unwrap();
    assert_eq!(stats.gap_discarded, 1, "{stats:?}");
    assert_eq!(s.last_seq(), 4);
    let after = std::fs::read_to_string(&jp).unwrap();
    assert!(!after.contains("ghost"), "빈틈 뒤 줄은 일지에서 잘라낸다");
    let rec = std::fs::read_to_string(d.join(".jadecode/recovery/gap.log")).unwrap();
    assert!(rec.contains("ghost"), "{rec}");
    let _ = cards;
}

#[test]
fn index_can_be_deleted_and_rebuilt_from_journals() {
    let (mut ws, d) = workspace();
    let cards = add_cards(&mut ws, &["refine"], 2);
    {
        let mut s = ws.store().unwrap();
        for i in 0..5 {
            s.append(vec![jc_core::NewEvent::new(
                jc_core::topic::card_topic(&cards[i % 2].rel),
                "msg.note",
                serde_json::json!({"i":i}),
            )])
            .unwrap();
        }
    }
    let before = full_check(&d).unwrap();
    assert!(before.ok, "{before:?}");
    remove_index(&d);
    let (s, stats, _) = Store::open(&d, Fsync::Batch).unwrap();
    assert!(!stats.index_discarded, "색인이 없으면 새로 만든다");
    assert_eq!(stats.tail_lines, before.journal_events as usize);
    assert_eq!(s.last_seq(), before.last_seq);
    let after = full_check(&d).unwrap();
    assert!(after.ok, "{after:?}");
    assert_eq!(after.index_events, before.index_events);
}

#[test]
fn corrupt_index_is_rebuilt_silently() {
    let (mut ws, d) = workspace();
    add_cards(&mut ws, &["refine"], 1);
    {
        let mut s = ws.store().unwrap();
        s.append(vec![jc_core::NewEvent::new("global", "a", serde_json::json!({}))])
            .unwrap();
    }
    // 색인 파일을 쓰레기로 만든다.
    std::fs::write(d.join(".jadecode/index.sqlite"), b"not a sqlite file").unwrap();
    let (s, stats, _) = Store::open(&d, Fsync::Batch).unwrap();
    assert!(stats.index_discarded, "{stats:?}");
    assert_eq!(s.last_seq(), 4);
    let r = full_check(&d).unwrap();
    assert!(r.ok, "{r:?}");
}

#[test]
fn board_projection_matches_cards() {
    let (mut ws, d) = workspace();
    add_cards(&mut ws, &["refine", "implement"], 3);
    let conn = jc_core::index::open_index_ro(&d).unwrap();
    let rows = card_rows(&conn).unwrap();
    assert_eq!(rows.len(), 3);
    for (r, c) in rows.iter().zip(ws.cards().unwrap()) {
        assert_eq!(r.id, c.state.front.id);
        assert_eq!(r.stage, "refine");
        assert_eq!(r.status, "idle");
    }
}

#[test]
fn external_body_edit_is_accepted_and_logged() {
    let (mut ws, d) = workspace();
    let cards = add_cards(&mut ws, &["refine"], 1);
    let rel = cards[0].rel.clone();
    let p = d.join(&rel).join("task.md");
    // 서버가 살아 있는 동안(색인이 현재 파일을 안다) 바깥에서 고친다.
    let mut store = ws.store().unwrap();
    let s = std::fs::read_to_string(&p).unwrap();
    std::fs::write(&p, s.replace("## 목표", "## 목표\n\n사람이 쓴 목표")).unwrap();
    let seqs = store.check_card(&rel, "test").unwrap();
    assert_eq!(seqs.len(), 1, "본문 변경은 card.file_changed 하나");
    assert_eq!(store.last_seq(), seqs[0]);
    let j = std::fs::read_to_string(d.join(&rel).join("events.jsonl")).unwrap();
    assert!(j.contains("card.file_changed"), "{j}");
    // 본문은 그대로 남는다
    let after = std::fs::read_to_string(&p).unwrap();
    assert!(after.contains("사람이 쓴 목표"));
}

#[test]
fn external_status_change_is_rejected_and_reverted() {
    let (mut ws, d) = workspace();
    let cards = add_cards(&mut ws, &["refine", "implement"], 1);
    let rel = cards[0].rel.clone();
    let p = d.join(&rel).join("task.md");
    // 서버가 살아 있는 동안(색인이 현재 파일을 안다) 상태 칸을 바깥에서 바꾼다.
    let mut store = ws.store().unwrap();
    let s = std::fs::read_to_string(&p).unwrap();
    let hacked = s
        .replace("status: idle", "status: done")
        .replace("stage: refine", "stage: implement");
    assert_ne!(hacked, s);
    std::fs::write(&p, &hacked).unwrap();
    store.check_card(&rel, "test").unwrap();
    let now = std::fs::read_to_string(&p).unwrap();
    let cs = card_state(&now).unwrap();
    assert_eq!(cs.front.status, "idle", "상태 칸은 되돌린다");
    assert_eq!(cs.front.stage, "refine", "단계 칸도 되돌린다");
    assert!(!now.contains("status: done"), "{now}");
    let j = std::fs::read_to_string(d.join(&rel).join("events.jsonl")).unwrap();
    assert!(j.contains("card.state_change_rejected"), "{j}");
    assert!(!j.contains("card.file_changed"), "앞머리만 바뀌었으므로 본문 사건은 없다: {j}");
}

#[test]
fn server_front_write_keeps_agent_edits() {
    // 서버 쓰기와 외부 편집이 겹쳐도 잃지 않는다: 서버가 본 내용 뒤에 누가 쓰면 비교 후 교체가 실패하고
    // 다시 읽어 반영한다(CAS 재시도).
    let (mut ws, d) = workspace();
    let cards = add_cards(&mut ws, &["refine", "implement"], 1);
    let rel = cards[0].rel.clone();
    let p = d.join(&rel).join("task.md");
    let mut store = ws.store().unwrap();
    let original = std::fs::read_to_string(&p).unwrap();
    // 외부 편집이 먼저 들어간 상태에서 set_front를 부른다(내부에서 check_card → CAS 순서).
    std::fs::write(&p, original.replace("## 목표", "## 목표\n\n에이전트가 쓴 내용")).unwrap();
    let mut front = cards[0].state.front.clone();
    front.status = "working".into();
    let seqs = store
        .set_front(&rel, &front, "card.status_changed", serde_json::json!({}))
        .unwrap();
    assert!(!seqs.is_empty());
    let now = std::fs::read_to_string(&p).unwrap();
    assert!(now.contains("에이전트가 쓴 내용"), "외부 편집을 잃지 않는다:\n{now}");
    assert!(now.contains("status: working"), "{now}");
    let r = full_check(&d).unwrap();
    assert!(r.ok, "{r:?}");
}

#[test]
fn layout_scans_nested_cards() {
    let (mut ws, _d) = workspace();
    add_cards(&mut ws, &["refine"], 2);
    let l = scan_layout(&ws.root).unwrap();
    assert_eq!(l.cards.len(), 2);
    assert_eq!(l.projects, vec!["work/p1".to_string()]);
    assert!(l.journals.iter().any(|j| j == "events/global.jsonl"));
}

#[test]
fn fsync_off_still_commits_index() {
    let (mut ws, d) = workspace();
    add_cards(&mut ws, &["refine"], 1);
    {
        let mut s = ws.store_with(Fsync::Off).unwrap();
        s.append(vec![jc_core::NewEvent::new("global", "x", serde_json::json!({}))])
            .unwrap();
    }
    let r = full_check(&d).unwrap();
    assert!(r.ok, "{r:?}");
}

#[test]
fn batch_group_still_durable_after_flush() {
    // BatchGroup은 fsync를 미루지만, flush_sync 뒤에는 일지가 디스크에 있어야 한다.
    let (mut ws, d) = workspace();
    let cards = add_cards(&mut ws, &["refine"], 2);
    {
        let mut s = ws.store_with(Fsync::BatchGroup).unwrap();
        for i in 0..50 {
            s.append(vec![jc_core::NewEvent::new(
                jc_core::topic::card_topic(&cards[i % 2].rel),
                "msg.note",
                serde_json::json!({"i": i}),
            )])
            .unwrap();
        }
        s.flush_sync().unwrap();
        // 프로젝트 생성 1 + 카드 생성 2(card.created) + card.added 2 + 노트 50 = 55
        assert_eq!(s.last_seq(), 55);
    }
    // 다시 열어도 전부 남아 있고 이어 붙인다.
    let (s2, stats, _) = Store::open(&d, Fsync::BatchGroup).unwrap();
    assert_eq!(s2.last_seq(), 55, "{stats:?}");
    assert_eq!(stats.tail_lines, 0);
    assert!(full_check(&d).unwrap().ok);
}

#[test]
fn batch_group_keeps_seq_order() {
    let (mut ws, d) = workspace();
    let cards = add_cards(&mut ws, &["refine"], 3);
    let mut s = ws.store_with(Fsync::BatchGroup).unwrap();
    // 여러 카드에 번갈아 넣어도 seq는 전역으로 이어져야 한다.
    let mut all = vec![];
    for i in 0..30 {
        let seqs = s
            .append(vec![jc_core::NewEvent::new(
                jc_core::topic::card_topic(&cards[i % 3].rel),
                "msg.note",
                serde_json::json!({"i": i}),
            )])
            .unwrap();
        all.extend(seqs);
    }
    s.flush_sync().unwrap();
    let mut sorted = all.clone();
    sorted.sort_unstable();
    sorted.dedup();
    assert_eq!(sorted.len(), all.len(), "중복 seq");
    assert_eq!(sorted.first().unwrap() + 1, *sorted.get(1).unwrap());
    drop(s);
    let r = full_check(&d).unwrap();
    assert!(r.ok, "{r:?}");
    assert_eq!(r.seq_gaps, 0);
}
