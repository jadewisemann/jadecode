//! 인스턴스·권한·봇 RPC의 서버 쪽 계약(실제 Claude Code는 안 띄운다).

use futures_util::{SinkExt, StreamExt};
use jc_core::workspace::Workspace;
use jc_core::NewCard;
use jc_server::{serve, server::Bind, ServerConfig};
use serde_json::{json, Value};
use std::path::PathBuf;
use std::time::Duration;
use tokio_tungstenite::tungstenite::Message;

async fn setup() -> (
    PathBuf,
    PathBuf,
    tokio::task::JoinHandle<()>,
    String,
) {
    // macOS 유닉스 소켓 경로 한도(104) 때문에 짧은 경로를 쓴다.
    let dir = jc_core::short_temp_root("jci");
    let (mut ws, _) = Workspace::init(&dir, "test").unwrap();
    ws.add_project("p1", Some("api")).unwrap();
    let card = ws
        .add_card(NewCard {
            project: "p1".into(),
            title: "probe".into(),
            path: vec!["implement".into(), "verify".into()],
            ..Default::default()
        })
        .unwrap();
    let card_dir = card.file_name().unwrap().to_string_lossy().to_string();
    let socket = dir.join(".jadecode/s.sock");
    let root = dir.clone();
    let sock = socket.clone();
    let cfg = ServerConfig {
        watch: true,
        debounce: Duration::from_millis(20),
        mcp_socket: socket.clone(),
        // 실제 claude를 띄우지 않도록 없는 바이너리를 준다(인스턴스 시작은 실패해야 한다).
        claude_bin: Some("/bin/false".into()),
        ..Default::default()
    };
    let t = tokio::spawn(async move {
        let _ = serve(root, Bind::Unix(sock), cfg, |_| {}).await;
    });
    for _ in 0..300 {
        if socket.exists() {
            break;
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    (dir, socket, t, card_dir)
}

async fn connect(
    socket: &std::path::Path,
) -> tokio_tungstenite::WebSocketStream<tokio::net::UnixStream> {
    for _ in 0..300 {
        if let Ok(s) = tokio::net::UnixStream::connect(socket).await {
            if let Ok((ws, _)) = tokio_tungstenite::client_async("ws://localhost/", s).await {
                return ws;
            }
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    panic!("connect");
}

async fn call(
    ws: &mut tokio_tungstenite::WebSocketStream<tokio::net::UnixStream>,
    id: u64,
    method: &str,
    params: Value,
) -> Value {
    ws.send(Message::text(
        json!({"jsonrpc":"2.0","id":id,"method":method,"params":params}).to_string(),
    ))
    .await
    .unwrap();
    loop {
        let msg = tokio::time::timeout(Duration::from_secs(20), ws.next())
            .await
            .expect("timeout")
            .expect("closed")
            .unwrap();
        if let Message::Text(t) = msg {
            let v: Value = serde_json::from_str(&t).unwrap();
            if v.get("id").and_then(|x| x.as_u64()) == Some(id) {
                return v;
            }
        }
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn bots_list_shows_four_default_bots() {
    let (_d, socket, t, card) = setup().await;
    let mut ws = connect(&socket).await;
    let r = call(&mut ws, 1, "bots.list", json!({})).await;
    let names: Vec<String> = r["result"]["bots"]
        .as_array()
        .unwrap()
        .iter()
        .map(|b| b["name"].as_str().unwrap().to_string())
        .collect();
    for n in ["orchestrator", "worker", "reviewer", "researcher"] {
        assert!(names.contains(&n.to_string()), "{names:?}");
    }
    t.abort();
}

#[tokio::test(flavor = "multi_thread")]
async fn policy_get_exposes_human_values() {
    let (_d, socket, t, card) = setup().await;
    let mut ws = connect(&socket).await;
    let r = call(&mut ws, 1, "policy.get", json!({})).await;
    assert_eq!(r["result"]["policy"]["permission_timeout_min"], 25);
    assert_eq!(r["result"]["defaults"]["permission_timeout_min"], 25);
    let procs = r["result"]["defaults"]["human_procedures"].as_array().unwrap();
    assert!(procs.iter().any(|p| p == "merge_protected_branch"));
    t.abort();
}

#[tokio::test(flavor = "multi_thread")]
async fn card_add_through_rpc_is_indexed() {
    let (_d, socket, t, card) = setup().await;
    let mut ws = connect(&socket).await;
    let r = call(
        &mut ws,
        1,
        "card.add",
        json!({"project":"p1","title":"via rpc","path":["implement","verify"]}),
    )
    .await;
    let path = r["result"]["path"].as_str().unwrap().to_string();
    assert!(path.starts_with("work/p1/via-rpc-"), "{path}");
    // 색인에 바로 보여야 한다(감시를 기다리지 않는다).
    let l = call(&mut ws, 2, "cards.list", json!({"project":"p1"})).await;
    let titles: Vec<String> = l["result"]["cards"]
        .as_array()
        .unwrap()
        .iter()
        .map(|c| c["title"].as_str().unwrap().to_string())
        .collect();
    assert!(titles.contains(&"via rpc".to_string()), "{titles:?}");
    // 일지에도 사건이 남는다
    let tail = call(&mut ws, 3, "events.tail", json!({"topics":["project/p1/card"],"limit":20})).await;
    let kinds: Vec<String> = tail["result"]["events"]
        .as_array()
        .unwrap()
        .iter()
        .map(|e| e["kind"].as_str().unwrap().to_string())
        .collect();
    assert!(kinds.contains(&"card.created".to_string()), "{kinds:?}");
    t.abort();
}

#[tokio::test(flavor = "multi_thread")]
async fn project_add_is_idempotent_guard() {
    let (_d, socket, t, card) = setup().await;
    let mut ws = connect(&socket).await;
    let r = call(&mut ws, 1, "project.add", json!({"name":"New Proj"})).await;
    assert_eq!(r["result"]["slug"], "new-proj");
    let again = call(&mut ws, 2, "project.add", json!({"name":"New Proj"})).await;
    assert_eq!(again["error"]["code"], 4000);
    t.abort();
}

#[tokio::test(flavor = "multi_thread")]
async fn instances_list_empty_and_start_reports_error() {
    let (_d, socket, t, card) = setup().await;
    let mut ws = connect(&socket).await;
    let l = call(&mut ws, 1, "instances.list", json!({})).await;
    assert_eq!(l["result"]["instances"].as_array().unwrap().len(), 0);
    // 없는 봇 → 오류
    let bad = call(
        &mut ws,
        2,
        "instance.start",
        json!({"role":"worker","project":"p1","bot":"nope","card":card}),
    )
    .await;
    assert!(bad.get("error").is_some(), "{bad}");
    // 없는 카드 → 오류
    let bad2 = call(
        &mut ws,
        3,
        "instance.start",
        json!({"role":"worker","project":"p1","bot":"worker","card":"zzz"}),
    )
    .await;
    assert_eq!(bad2["error"]["code"], -32602);
    t.abort();
}

#[tokio::test(flavor = "multi_thread")]
async fn permission_request_denies_when_no_instance() {
    let (_d, socket, t, card) = setup().await;
    let mut ws = connect(&socket).await;
    let r = call(
        &mut ws,
        1,
        "permission.request",
        json!({"instance":"nope","tool_name":"Bash","input":{"command":"ls"},"tool_use_id":"t"}),
    )
    .await;
    // 인스턴스를 모르면 deny 계약으로 답한다(도구 오류가 아니라 거부).
    assert_eq!(r["result"]["behavior"], "deny", "{r}");
    t.abort();
}

#[tokio::test(flavor = "multi_thread")]
async fn permission_resolve_unknown_seq_is_refused() {
    let (_d, socket, t, card) = setup().await;
    let mut ws = connect(&socket).await;
    let r = call(
        &mut ws,
        1,
        "permission.resolve",
        json!({"request_seq":9999,"allow":true,"message":"x"}),
    )
    .await;
    assert_eq!(r["error"]["code"], 4000);
    t.abort();
}

#[tokio::test(flavor = "multi_thread")]
async fn instance_start_with_dead_claude_fails_cleanly() {
    // claude 바이너리가 /bin/false면 spawn은 되지만 init이 오지 않는다(정상: 이벤트로 남는다).
    let (_d, socket, t, card) = setup().await;
    let mut ws = connect(&socket).await;
    let r = call(
        &mut ws,
        1,
        "instance.start",
        json!({"role":"worker","project":"p1","bot":"worker","card":card}),
    )
    .await;
    // 시작 자체는 받아들인다(프로세스는 떴다). 실패는 이벤트로 드러난다.
    assert!(r.get("result").is_some(), "{r}");
    tokio::time::sleep(Duration::from_millis(500)).await;
    let tail = call(&mut ws, 2, "events.tail", json!({"topics":["project/p1/card"],"limit":30})).await;
    let kinds: Vec<String> = tail["result"]["events"]
        .as_array()
        .unwrap()
        .iter()
        .map(|e| e["kind"].as_str().unwrap().to_string())
        .collect();
    assert!(
        kinds.iter().any(|k| k == "instance.started"),
        "{kinds:?}"
    );
    t.abort();
}

#[tokio::test(flavor = "multi_thread")]
async fn many_workspaces_do_not_collide() {
    // 병렬 테스트가 서로 다른 워크스페이스를 써야 한다(잠금 충돌 방지).
    let a = jc_core::short_temp_root("jcq");
    let b = jc_core::short_temp_root("jcq");
    assert_ne!(a, b);
    let (mut wa, _) = Workspace::init(&a, "a").unwrap();
    let (mut wb, _) = Workspace::init(&b, "b").unwrap();
    // 둘 다 동시에 쓸 수 있어야 한다.
    let mut sa = wa.store().unwrap();
    let mut sb = wb.store().unwrap();
    sa.append(vec![jc_core::NewEvent::new("global", "x", serde_json::json!({}))]).unwrap();
    sb.append(vec![jc_core::NewEvent::new("global", "y", serde_json::json!({}))]).unwrap();
}
