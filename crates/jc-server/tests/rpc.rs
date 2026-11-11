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
    shutdown: tokio::sync::oneshot::Sender<()>,
}

async fn setup(path: &[&str]) -> (Harness, Vec<jc_core::workspace::CardEntry>) {
    let dir = std::env::temp_dir().join(format!(
        "jc-srv-{}-{}",
        std::process::id(),
        jc_core::ulid::new_id()
    ));
    std::fs::create_dir_all(&dir).unwrap();
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
    let socket = dir.join(".jadecode/test.sock");
    let (tx, rx) = tokio::sync::oneshot::channel::<()>();
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
    let mut ready_rx = ready.1;
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
    let _ = rx;
    (
        Harness {
            dir,
            ws,
            socket,
            client,
            shutdown: tx,
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
    assert!(r["result"]["workspace"].as_str().unwrap().contains("jc-srv-"));
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
    let r = call(&mut ws, 1, "card.set_status", json!({"card": id, "status": "working"})).await;
    assert!(r["result"]["seqs"].as_array().unwrap().len() >= 1, "{r}");
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
    let bad = call(&mut ws, 3, "card.set_status", json!({"card": id, "status": "nope"})).await;
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
