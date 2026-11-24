//! 서버 통합 테스트: 실제 유닉스 소켓 + WebSocket 위에서 JSON-RPC를 부른다.

use futures_util::{SinkExt, StreamExt};
use jc_core::workspace::Workspace;
use jc_core::NewCard;
use jc_server::{serve, server::Bind, ServerConfig};
use serde_json::{json, Value};
use std::path::PathBuf;
use std::time::Duration;
use tokio_tungstenite::tungstenite::Message;

struct Harness {
    dir: PathBuf,
    ws: Workspace,
    socket: PathBuf,
    client: tokio::task::JoinHandle<()>,
}

async fn setup(path: &[&str]) -> (Harness, Vec<jc_core::workspace::CardEntry>) {
    // macOS 유닉스 소켓 경로 한도(104) 때문에 짧은 경로를 쓴다.
    let dir = jc_core::short_temp_root("jcr");
    let (mut ws, _) = Workspace::init(&dir, "test").unwrap();
    ws.add_project("p1", None).unwrap();
    let mut cards = vec![];
    for i in 0..2 {
        ws.add_card(NewCard {
            project: "p1".into(),
            title: format!("card {i}"),
            path: path.iter().map(|s| s.to_string()).collect(),
            ..Default::default()
        })
        .unwrap();
    }
    cards.extend(ws.cards().unwrap());
    let socket = dir.join(".jadecode/s.sock");
    let (_tx, rx) = tokio::sync::oneshot::channel::<()>();
    let ready = tokio::sync::oneshot::channel::<()>();
    let ready_tx = ready.0;
    let root = dir.clone();
    let sock = socket.clone();
    let cfg = ServerConfig {
        fsync: jc_core::store::Fsync::Batch,
        watch: true,
        debounce: Duration::from_millis(20),
        mcp_socket: socket.clone(),
        ..Default::default()
    };
    let client = tokio::spawn(async move {
        let mut ready_tx = Some(ready_tx);
        let r = serve(root, Bind::Unix(sock), cfg, move |_b| {
            if let Some(tx) = ready_tx.take() {
                let _ = tx.send(());
            }
        })
        .await;
        let _ = r;
    });
    // 준비를 기다린다(accept 루프 진입은 리스너 바인딩 뒤).
    let ready_rx = ready.1;
    let _ = tokio::time::timeout(Duration::from_secs(10), async {
        let _ = ready_rx.await;
    })
    .await;
    // 소켓 파일이 보이면 연결 시도 가능
    for _ in 0..100 {
        if socket.exists() {
            break;
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    drop(rx);
    (
        Harness {
            dir,
            ws,
            socket,
            client,
        },
        cards,
    )
}

type Conn = tokio_tungstenite::WebSocketStream<tokio::net::UnixStream>;

async fn connect(socket: &std::path::Path) -> Conn {
    for _ in 0..200 {
        if let Ok(s) = tokio::net::UnixStream::connect(socket).await {
            if let Ok((ws, _)) = tokio_tungstenite::client_async("ws://localhost/", s).await {
                return ws;
            }
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    panic!("cannot connect to {}", socket.display());
}

async fn call(ws: &mut Conn, id: u64, method: &str, params: Value) -> Value {
    ws.send(Message::text(
        json!({"jsonrpc":"2.0","id":id,"method":method,"params":params}).to_string(),
    ))
    .await
    .unwrap();
    loop {
        let msg = tokio::time::timeout(Duration::from_secs(15), ws.next())
            .await
            .expect("timeout")
            .expect("closed")
            .unwrap();
        if let Message::Text(t) = msg {
            let v: Value = serde_json::from_str(&t).unwrap();
            if v.get("id").and_then(|x| x.as_u64()) == Some(id) {
                return v;
            }
            // 구독 알림이 먼저 올 수 있다
        }
    }
}

async fn next_notification(ws: &mut Conn) -> Value {
    loop {
        let msg = tokio::time::timeout(Duration::from_secs(15), ws.next())
            .await
            .expect("timeout")
            .expect("closed")
            .unwrap();
        if let Message::Text(t) = msg {
            let v: Value = serde_json::from_str(&t).unwrap();
            if v.get("method").and_then(|m| m.as_str()) == Some("events.event") {
                return v;
            }
        }
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn info_lists_methods() {
    let (h, _cards) = setup(&["refine", "implement"]).await;
    let mut ws = connect(&h.socket).await;
    let r = call(&mut ws, 1, "server.info", json!({})).await;
    let names = r["result"]["methods"].as_array().unwrap();
    assert!(names.iter().any(|n| n == "events.subscribe"));
    assert_eq!(r["result"]["name"], "jadecode");
    // 짧은 임시 경로를 쓰므로 이름 규칙 대신 "그 워크스페이스"인지만 본다.
    assert_eq!(
        r["result"]["workspace"].as_str().unwrap(),
        h.ws.root.display().to_string()
    );
    h.client.abort();
}

#[tokio::test(flavor = "multi_thread")]
async fn cards_list_and_get() {
    let (h, cards) = setup(&["refine", "implement"]).await;
    let mut ws = connect(&h.socket).await;
    let r = call(&mut ws, 1, "cards.list", json!({})).await;
    let list = r["result"]["cards"].as_array().unwrap();
    assert_eq!(list.len(), 2);
    assert_eq!(list[0]["stage"], "refine");
    assert_eq!(list[0]["status"], "idle");
    assert_eq!(list[0]["project"], "p1");

    let id = &cards[0].state.front.id;
    let g = call(&mut ws, 2, "card.get", json!({"card": id})).await;
    assert_eq!(g["result"]["path"], cards[0].rel);
    assert_eq!(g["result"]["stage"], "refine");
    assert!(g["result"]["markdown"]
        .as_str()
        .unwrap()
        .contains("## 목표"));

    let miss = call(&mut ws, 3, "card.get", json!({"card": "nope"})).await;
    assert_eq!(miss["error"]["code"], -32602);
    h.client.abort();
}

#[tokio::test(flavor = "multi_thread")]
async fn set_status_writes_file_and_emits_event() {
    let (h, cards) = setup(&["refine", "implement"]).await;
    let mut ws = connect(&h.socket).await;
    let id = &cards[0].state.front.id;
    let r = call(
        &mut ws,
        1,
        "card.set_status",
        json!({"card": id, "status": "working"}),
    )
    .await;
    assert!(!r["result"]["seqs"].as_array().unwrap().is_empty(), "{r}");
    // 파일이 정본
    let md = std::fs::read_to_string(h.ws.root.join(&cards[0].rel).join("task.md")).unwrap();
    assert!(md.contains("status: working"), "{md}");
    // 다시 목록을 보면 반영돼 있다
    let list = call(&mut ws, 2, "cards.list", json!({})).await;
    let row = list["result"]["cards"]
        .as_array()
        .unwrap()
        .iter()
        .find(|c| c["id"] == *id)
        .unwrap();
    assert_eq!(row["status"], "working");
    // 잘못된 상태는 거절
    let bad = call(
        &mut ws,
        3,
        "card.set_status",
        json!({"card": id, "status": "nope"}),
    )
    .await;
    assert_eq!(bad["error"]["code"], -32602);
    h.client.abort();
}

#[tokio::test(flavor = "multi_thread")]
async fn advance_moves_along_path_then_refuses() {
    let (h, cards) = setup(&["refine", "implement"]).await;
    let mut ws = connect(&h.socket).await;
    let id = &cards[0].state.front.id;
    let a = call(&mut ws, 1, "card.advance", json!({"card": id})).await;
    assert_eq!(a["result"]["stage"], "implement", "{a}");
    let md = std::fs::read_to_string(h.ws.root.join(&cards[0].rel).join("task.md")).unwrap();
    assert!(md.contains("stage: implement"), "{md}");
    let b = call(&mut ws, 2, "card.advance", json!({"card": id})).await;
    assert_eq!(b["error"]["code"], 4000, "{b}");
    assert_eq!(b["error"]["data"]["reason"], "path_end");
    // 칸반 끌어 놓기: 경로 안의 단계로 바로 옮긴다. 경로 밖은 거절한다.
    let c = call(
        &mut ws,
        3,
        "card.advance",
        json!({"card": id, "to": "refine"}),
    )
    .await;
    assert_eq!(c["result"]["stage"], "refine", "{c}");
    let d = call(
        &mut ws,
        4,
        "card.advance",
        json!({"card": id, "to": "verify"}),
    )
    .await;
    assert_eq!(d["error"]["data"]["reason"], "not_on_path", "{d}");
    let e = call(&mut ws, 5, "card.advance", json!({"card": id})).await;
    assert_eq!(e["result"]["stage"], "implement", "{e}");
    h.client.abort();
}

#[tokio::test(flavor = "multi_thread")]
async fn subscribe_receives_events_after_from_seq() {
    let (h, cards) = setup(&["refine"]).await;
    let mut ws = connect(&h.socket).await;
    let head = call(&mut ws, 1, "events.head", json!({})).await["result"]["seq"]
        .as_u64()
        .unwrap();
    let sub = call(
        &mut ws,
        2,
        "events.subscribe",
        json!({"topics":["project/p1/card"], "from_seq": head}),
    )
    .await;
    assert_eq!(sub["result"]["head_seq"], head);
    let id = cards[0].state.front.id.clone();
    let seq = call(
        &mut ws,
        3,
        "card.append_event",
        json!({"card": id, "kind": "msg.report", "body": {"text": "done"}}),
    )
    .await;
    let seq = seq["result"]["seqs"][0].as_u64().unwrap();
    let n = next_notification(&mut ws).await;
    assert_eq!(n["params"]["event"]["seq"], seq);
    assert_eq!(n["params"]["event"]["kind"], "msg.report");
    assert_eq!(n["params"]["event"]["body"]["text"], "done");
    h.client.abort();
}

#[tokio::test(flavor = "multi_thread")]
async fn subscription_filter_is_segment_wise() {
    let dir_cards = setup(&["refine"]).await;
    let (h, cards) = dir_cards;
    let mut ws = connect(&h.socket).await;
    let head = call(&mut ws, 1, "events.head", json!({})).await["result"]["seq"]
        .as_u64()
        .unwrap();
    // project/p2 는 p1 카드 이벤트를 받지 않는다.
    call(
        &mut ws,
        2,
        "events.subscribe",
        json!({"topics":["project/p2"], "from_seq": head}),
    )
    .await;
    let id = cards[0].state.front.id.clone();
    call(
        &mut ws,
        3,
        "card.append_event",
        json!({"card": id, "kind":"msg.note","body":{}}),
    )
    .await;
    // 0.7초 안에 알림이 오면 안 된다
    let got = tokio::time::timeout(Duration::from_millis(700), next_notification(&mut ws)).await;
    assert!(got.is_err(), "p2 구독이 p1 이벤트를 받았다");
    h.client.abort();
}

#[tokio::test(flavor = "multi_thread")]
async fn tail_returns_recent_events_newest_last() {
    let (h, cards) = setup(&["refine"]).await;
    let mut ws = connect(&h.socket).await;
    let id = cards[0].state.front.id.clone();
    for i in 0..3 {
        call(
            &mut ws,
            10 + i,
            "card.append_event",
            json!({"card": id, "kind":"msg.note","body":{"i":i}}),
        )
        .await;
    }
    let t = call(&mut ws, 20, "events.tail", json!({"limit": 3})).await;
    let evs = t["result"]["events"].as_array().unwrap();
    assert_eq!(evs.len(), 3);
    assert_eq!(evs[2]["body"]["i"], 2, "최신이 마지막");
    let t2 = call(
        &mut ws,
        21,
        "events.tail",
        json!({"topics":["project/p1/card"], "limit": 100}),
    )
    .await;
    let n = t2["result"]["events"].as_array().unwrap().len();
    assert!(n >= 3, "{n}");
    h.client.abort();
}

#[tokio::test(flavor = "multi_thread")]
async fn external_edit_is_seen_by_watcher() {
    let (h, cards) = setup(&["refine"]).await;
    let mut ws = connect(&h.socket).await;
    // 감시가 붙었는지 확인: head를 읽고, 파일을 고친 뒤 새 이벤트가 생기길 기다린다.
    let head = call(&mut ws, 1, "events.head", json!({})).await["result"]["seq"]
        .as_u64()
        .unwrap();
    call(&mut ws, 2, "events.subscribe", json!({"from_seq": head})).await;
    let p = h.ws.root.join(&cards[0].rel).join("task.md");
    let md = std::fs::read_to_string(&p).unwrap();
    std::fs::write(&p, md.replace("## 목표", "## 목표\n\n사람이 쓴 목표")).unwrap();
    let n = next_notification(&mut ws).await;
    assert_eq!(n["params"]["event"]["kind"], "card.file_changed", "{n}");
    // 본문은 남아 있다
    let after = std::fs::read_to_string(&p).unwrap();
    assert!(after.contains("사람이 쓴 목표"));
    // 색인 투영도 맞다
    let list = call(&mut ws, 3, "cards.list", json!({})).await;
    assert_eq!(list["result"]["cards"].as_array().unwrap().len(), 2);
    let _ = h.dir;
    h.client.abort();
}

#[tokio::test(flavor = "multi_thread")]
async fn projects_list_counts_cards() {
    let (h, _cards) = setup(&["refine"]).await;
    let mut ws = connect(&h.socket).await;
    let r = call(&mut ws, 1, "projects.list", json!({})).await;
    let p = &r["result"]["projects"][0];
    assert_eq!(p["slug"], "p1");
    assert_eq!(p["cards"], 2);
    assert_eq!(p["topic"], "project/p1");
    h.client.abort();
}

#[tokio::test(flavor = "multi_thread")]
async fn unknown_method_and_bad_params() {
    let (h, _cards) = setup(&["refine"]).await;
    let mut ws = connect(&h.socket).await;
    let r = call(&mut ws, 1, "nope.method", json!({})).await;
    assert_eq!(r["error"]["code"], -32601);
    let r = call(&mut ws, 2, "card.set_status", json!({})).await;
    assert_eq!(r["error"]["code"], -32602);
    let r = call(
        &mut ws,
        3,
        "card.append_event",
        json!({"card": "x", "kind": "wrong.prefix"}),
    )
    .await;
    assert_eq!(r["error"]["code"], -32602);
    h.client.abort();
}

#[tokio::test(flavor = "multi_thread")]
async fn long_socket_path_fails_with_clear_message() {
    // macOS 유닉스 소켓 한도(104)를 넘는 경로는 "알 수 없는 오류"가 아니라
    // 원인이 담긴 오류로 끝나야 한다(사용자가 --socket 을 고칠 수 있게).
    // 워크스페이스만 새로 만들고 서버는 띄우지 않는다(잠금 충돌 방지).
    let dir = jc_core::short_temp_root("jcl");
    let (ws, _) = jc_core::Workspace::init(&dir, "t").unwrap();
    let workspace = ws.root.clone();
    let long = workspace.join("a".repeat(120)).join("x.sock");
    let cfg = ServerConfig {
        watch: false,
        mcp_socket: long.clone(),
        ..Default::default()
    };
    let res = jc_server::serve(workspace, Bind::Unix(long.clone()), cfg, |_| {}).await;
    let err = res.expect_err("긴 경로는 실패해야 한다");
    let msg = format!("{err:#}");
    assert!(msg.contains("너무 길다"), "{msg}");
}

/// 외부 사이클 전체: 커넥터(MCP) 동기화 → 수집 → 분류(카드) → 카드 끝 → 회신 대기 → 답장 → 닫힘.
#[tokio::test(flavor = "multi_thread")]
async fn intake_cycle_through_mcp_connector() {
    if std::process::Command::new("python3")
        .arg("-V")
        .output()
        .is_err()
    {
        return;
    }
    let (h, _cards) = setup(&["refine", "implement"]).await;
    let mut ws = connect(&h.socket).await;

    // Slack 서버 흉내: 채널 기록 + 스레드 답장(받은 답장을 파일에 남긴다).
    let fake = h.dir.join("fake_slack.py");
    let replies = h.dir.join("replies.txt");
    std::fs::write(
        &fake,
        format!(
            r#"import sys, json
for line in sys.stdin:
    m = json.loads(line)
    if "id" not in m:
        continue
    meth = m["method"]
    if meth == "initialize":
        r = {{"protocolVersion": "2025-06-18", "capabilities": {{}}}}
    elif meth == "tools/list":
        r = {{"tools": [{{"name": "slack_get_channel_history"}}, {{"name": "slack_reply_to_thread"}}]}}
    elif m["params"]["name"] == "slack_get_channel_history":
        msgs = [{{"ts": "1.1", "user": "U1", "text": "결제 실패 고쳐 주세요\n상세"}}, {{"ts": "1.2", "bot_id": "B", "text": "봇"}}]
        r = {{"content": [{{"type": "text", "text": json.dumps({{"ok": True, "messages": msgs}})}}]}}
    else:
        a = m["params"]["arguments"]
        open({replies:?}, "a").write(a["thread_ts"] + "|" + a["text"] + "\n")
        r = {{"content": [{{"type": "text", "text": "{{}}"}}]}}
    print(json.dumps({{"jsonrpc": "2.0", "id": m["id"], "result": r}}), flush=True)
"#,
            replies = replies.display().to_string()
        ),
    )
    .unwrap();
    let r = call(
        &mut ws,
        1,
        "connectors.save",
        json!({"name":"slack","project":"p1","preset":"slack",
               "fields":{"token":"xoxb-test-token","team_id":"T1","channel_id":"C1"}}),
    )
    .await;
    assert!(r.get("error").is_none(), "{r}");
    // 프리셋의 npx 대신 가짜 서버를 쓰도록 설정 파일을 고친다.
    let cfg = h.dir.join(".jadecode/connectors.toml");
    let text = std::fs::read_to_string(&cfg).unwrap();
    let text = text
        .replace("command = \"npx\"", "command = \"python3\"")
        .replace(
            "args = [\"-y\", \"@modelcontextprotocol/server-slack\"]",
            &format!("args = [{:?}]", fake.display().to_string()),
        );
    std::fs::write(&cfg, text).unwrap();

    let r = call(&mut ws, 2, "connectors.list", json!({})).await;
    assert_eq!(
        r["result"]["connectors"][0]["env"]["SLACK_BOT_TOKEN"],
        "xoxb…"
    );

    let r = call(&mut ws, 3, "connectors.sync", json!({"name":"slack"})).await;
    assert_eq!(r["result"]["results"][0]["added"], 1, "{r}");
    // 두 번 동기화해도 같은 메시지는 다시 들어오지 않는다.
    let r = call(&mut ws, 4, "connectors.sync", json!({"name":"slack"})).await;
    assert_eq!(r["result"]["results"][0]["added"], 0, "{r}");

    let r = call(&mut ws, 5, "intake.list", json!({"project":"p1"})).await;
    let items = r["result"]["items"].as_array().unwrap().clone();
    assert_eq!(items.len(), 1);
    assert_eq!(items[0]["title"], "결제 실패 고쳐 주세요");
    assert_eq!(items[0]["state"], "new");
    let id = items[0]["id"].as_str().unwrap().to_string();

    let r = call(
        &mut ws,
        6,
        "intake.triage",
        json!({"id": id, "decision":"card", "title":"결제 실패 수정",
               "brief":"결제 API가 실패할 때 재시도한다.", "done_when":"테스트 통과"}),
    )
    .await;
    let card = r["result"]["card"].as_str().unwrap().to_string();
    let md = std::fs::read_to_string(h.dir.join(&card).join("task.md")).unwrap();
    assert!(md.contains("결제 API가 실패할 때 재시도한다."));
    assert!(md.contains("slack.com/archives/C1/p11"));

    let r = call(
        &mut ws,
        7,
        "card.set_status",
        json!({"card": card, "status":"done"}),
    )
    .await;
    assert!(r.get("error").is_none(), "{r}");
    let r = call(&mut ws, 8, "intake.list", json!({"project":"p1"})).await;
    assert_eq!(r["result"]["items"][0]["state"], "reply_pending");

    let r = call(
        &mut ws,
        9,
        "intake.reply",
        json!({"id": id, "text":"고쳤습니다"}),
    )
    .await;
    assert_eq!(r["result"]["sent"], true, "{r}");
    assert_eq!(
        std::fs::read_to_string(&replies).unwrap(),
        "1.1|고쳤습니다\n"
    );
    let r = call(&mut ws, 10, "intake.list", json!({"project":"p1"})).await;
    assert_eq!(r["result"]["items"][0]["state"], "closed");

    // 직접 넣은 메모는 보관할 수 있다.
    let r = call(
        &mut ws,
        11,
        "intake.add",
        json!({"project":"p1","title":"메모"}),
    )
    .await;
    let mid = r["result"]["id"].as_str().unwrap().to_string();
    let r = call(
        &mut ws,
        12,
        "intake.triage",
        json!({"id": mid, "decision":"archive"}),
    )
    .await;
    assert!(r.get("error").is_none(), "{r}");
    h.client.abort();
    let _ = std::fs::remove_dir_all(&h.dir);
}

#[tokio::test(flavor = "multi_thread")]
async fn project_with_repo_path_gives_cards_a_worktree() {
    let (h, _cards) = setup(&["implement"]).await;
    let mut ws = connect(&h.socket).await;
    // 사람 컴퓨터의 레포를 흉내 낸다.
    let src = h.ws.root.join("src-app");
    std::fs::create_dir_all(&src).unwrap();
    let git = |args: &[&str]| {
        let st = std::process::Command::new("git")
            .arg("-C")
            .arg(&src)
            .args(args)
            .env("GIT_AUTHOR_NAME", "t")
            .env("GIT_AUTHOR_EMAIL", "t@example.com")
            .env("GIT_COMMITTER_NAME", "t")
            .env("GIT_COMMITTER_EMAIL", "t@example.com")
            .status()
            .unwrap();
        assert!(st.success());
    };
    git(&["init", "-q", "-b", "main"]);
    std::fs::write(src.join("README"), "x").unwrap();
    git(&["add", "."]);
    git(&["commit", "-qm", "init"]);

    let bad = call(
        &mut ws,
        1,
        "project.add",
        json!({"name": "nope", "repo_path": h.ws.root.join("missing").display().to_string()}),
    )
    .await;
    assert!(bad["error"].is_object(), "{bad}");

    let p = call(
        &mut ws,
        2,
        "project.add",
        json!({"name": "web", "repo_path": src.display().to_string()}),
    )
    .await;
    assert_eq!(p["result"]["slug"], "web", "{p}");
    let toml = std::fs::read_to_string(h.ws.root.join("work/web/project.toml")).unwrap();
    assert!(toml.contains("name = \"src-app\""), "{toml}");

    let c = call(
        &mut ws,
        3,
        "card.add",
        json!({"project": "web", "title": "fix login"}),
    )
    .await;
    let rel = c["result"]["path"].as_str().unwrap().to_string();
    let md = std::fs::read_to_string(h.ws.root.join(&rel).join("task.md")).unwrap();
    assert!(md.contains("repo: src-app"), "{md}");

    let opened = jc_server::worktree::ensure_card(&h.ws.root, &rel);
    assert_eq!(opened.len(), 1);
    let wt = opened[0].1.clone().unwrap();
    assert!(wt.join("README").exists());
    h.client.abort();
}
