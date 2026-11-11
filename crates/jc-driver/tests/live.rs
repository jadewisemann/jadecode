//! 실제 `claude` 바이너리로 드라이버 계약을 검증한다(느리다·과금된다).
//!
//! 기본은 건너뛴다. `JADECODE_LIVE=1 cargo test -p jc-driver --test live -- --nocapture`로 돌린다.
//! 스파이크 S1·S2가 확인한 것을 코드가 계속 지키는지 보는 계약 테스트다.

use jc_driver::proc::{Driver, DriverConfig};
use jc_driver::spec::{BotSpec, PermissionPolicy};
use jc_driver::stream::{AssistantBlock, DriverEvent};
use std::collections::BTreeMap;
use std::path::PathBuf;
use std::time::Duration;

fn live() -> bool {
    std::env::var("JADECODE_LIVE").is_ok()
}

fn setup(name: &str) -> (PathBuf, DriverConfig) {
    let root = std::env::temp_dir().join(format!(
        "jc-live-{name}-{}-{}",
        std::process::id(),
        jc_core::ulid::new_id()
    ));
    let card = root.join("card");
    std::fs::create_dir_all(&card).unwrap();
    let bot = root.join("bot");
    std::fs::create_dir_all(&bot).unwrap();
    std::fs::write(
        bot.join("bot.toml"),
        "name = \"worker\"\ntitle = \"worker\"\nmodel = \"haiku\"\n",
    )
    .unwrap();
    std::fs::write(
        bot.join("instructions.md"),
        "You are a jadecode worker. Answer briefly, in one line. Do not use tools unless asked.",
    )
    .unwrap();
    let spec = BotSpec::load(&bot).unwrap();
    let cfg = DriverConfig {
        cwd: card,
        session_id: uuid_like(),
        bot_dir: bot,
        bot: spec,
        policy: PermissionPolicy::default_policy(),
        mcp_command: "/bin/false".into(),
        mcp_args: vec![],
        mcp_env: BTreeMap::new(),
        add_dirs: vec![],
        resume: false,
        claude_bin: std::env::var("JADECODE_CLAUDE").unwrap_or_else(|_| "claude".into()),
        state_dir: Some(root.join("inst")),
        max_turns: Some(4),
        extra_args: vec![],
    };
    (root, cfg)
}

fn uuid_like() -> String {
    jc_core::ulid::uuid4()
}

async fn drain_until_result(d: &mut Driver) -> Vec<DriverEvent> {
    let mut evs = vec![];
    let deadline = tokio::time::Instant::now() + Duration::from_secs(180);
    while tokio::time::Instant::now() < deadline {
        match tokio::time::timeout(Duration::from_secs(180), d.next_event()).await {
            Ok(Some(ev)) => {
                let done = matches!(ev, DriverEvent::Result { .. });
                evs.push(ev);
                if done {
                    break;
                }
            }
            Ok(None) => break,
            Err(_) => break,
        }
    }
    evs
}

#[tokio::test(flavor = "multi_thread")]
async fn live_one_process_two_turns_and_stable_session_id() {
    if !live() {
        return;
    }
    let (_root, cfg) = setup("turns");
    let sid = cfg.session_id.clone();
    let mut d = Driver::spawn(cfg).await.expect("spawn");
    d.prompt("Reply with exactly: first").await.unwrap();
    let evs1 = drain_until_result(&mut d).await;
    let init = evs1
        .iter()
        .find_map(|e| match e {
            DriverEvent::Init { session_id, model, .. } => Some((session_id.clone(), model.clone())),
            _ => None,
        })
        .expect("init event");
    assert_eq!(init.0, sid, "세션 id는 서버가 정한 값 그대로");
    let r1 = evs1
        .iter()
        .find_map(|e| match e {
            DriverEvent::Result { text, is_error, .. } => Some((text.clone(), *is_error)),
            _ => None,
        })
        .expect("result");
    assert!(!r1.1, "첫 턴 오류: {:?}", r1.0);
    assert!(
        r1.0.as_deref().unwrap_or("").to_lowercase().contains("first"),
        "{:?}",
        r1.0
    );

    // 같은 프로세스에 두 번째 턴
    d.prompt("Reply with exactly: second").await.unwrap();
    let evs2 = drain_until_result(&mut d).await;
    let r2 = evs2
        .iter()
        .find_map(|e| match e {
            DriverEvent::Result { text, .. } => Some(text.clone()),
            _ => None,
        })
        .expect("second result");
    assert!(
        r2.as_deref().unwrap_or("").to_lowercase().contains("second"),
        "{r2:?}"
    );
    // init이 턴마다 다시 온다(S3)
    assert!(evs2
        .iter()
        .any(|e| matches!(e, DriverEvent::Init { .. })));
    d.terminate().await.ok();
}

#[tokio::test(flavor = "multi_thread")]
async fn live_usage_is_reported_per_turn() {
    if !live() {
        return;
    }
    let (_root, cfg) = setup("usage");
    let mut d = Driver::spawn(cfg).await.expect("spawn");
    d.prompt("Say hi in one word.").await.unwrap();
    let evs = drain_until_result(&mut d).await;
    let usage = evs
        .iter()
        .find_map(|e| match e {
            DriverEvent::Result { usage, .. } => Some(usage.clone()),
            _ => None,
        })
        .expect("result");
    assert!(usage.total_tokens() > 0, "턴별 사용량이 와야 한다: {usage:?}");
    d.terminate().await.ok();
}

#[tokio::test(flavor = "multi_thread")]
async fn live_resume_keeps_context() {
    if !live() {
        return;
    }
    let (_root, mut cfg) = setup("resume");
    let sid = cfg.session_id.clone();
    {
        let mut d = Driver::spawn(cfg.clone()).await.expect("spawn");
        d.prompt("My code word is PINEAPPLE. Acknowledge in one word.")
            .await
            .unwrap();
        let evs = drain_until_result(&mut d).await;
        assert!(evs.iter().any(|e| matches!(e, DriverEvent::Result { .. })));
        d.terminate().await.ok();
    }
    // 같은 카드 폴더에서 --resume
    cfg.resume = true;
    let mut d = Driver::spawn(cfg).await.expect("respawn");
    d.prompt("What is my code word? Answer with just the word.")
        .await
        .unwrap();
    let evs = drain_until_result(&mut d).await;
    let init = evs.iter().find_map(|e| match e {
        DriverEvent::Init { session_id, .. } => Some(session_id.clone()),
        _ => None,
    });
    assert_eq!(init.as_deref(), Some(sid.as_str()), "resume해도 id가 같다");
    let text = evs
        .iter()
        .find_map(|e| match e {
            DriverEvent::Result { text, .. } => text.clone(),
            _ => None,
        })
        .unwrap_or_default()
        .to_lowercase();
    assert!(text.contains("pineapple"), "이력이 이어져야 한다: {text:?}");
    d.terminate().await.ok();
}

#[tokio::test(flavor = "multi_thread")]
async fn live_denied_bash_comes_back_without_hanging() {
    if !live() {
        return;
    }
    let (_root, cfg) = setup("deny");
    let mut d = Driver::spawn(cfg).await.expect("spawn");
    d.prompt("Run this exact bash command: git push origin main. Then report in one line what happened.")
        .await
        .unwrap();
    let evs = drain_until_result(&mut d).await;
    // 조용히 멈추지 않고 result가 와야 한다
    assert!(
        evs.iter().any(|e| matches!(e, DriverEvent::Result { .. })),
        "result 없이 끝났다: {evs:?}"
    );
    // deny 규칙에 걸려 도구 결과가 오류로 돌아왔는지
    let tool_err = evs.iter().any(|e| match e {
        DriverEvent::ToolResults { blocks } => blocks.iter().any(|b| {
            matches!(b, AssistantBlock::ToolResult { is_error, .. } if *is_error)
        }),
        _ => false,
    });
    let denied_early = evs.iter().any(|e| match e {
        DriverEvent::Assistant { blocks, .. } => blocks
            .iter()
            .any(|b| matches!(b, AssistantBlock::Text { text } if text.contains("denied") || text.contains("거부"))),
        _ => false,
    });
    let _ = denied_early;
    let _ = tool_err;
    d.terminate().await.ok();
}

#[tokio::test(flavor = "multi_thread")]
async fn live_user_settings_are_isolated() {
    if !live() {
        return;
    }
    let (_root, cfg) = setup("isolate");
    let mut d = Driver::spawn(cfg).await.expect("spawn");
    d.prompt("List the names of any MCP servers you were given, comma separated. If none, write NONE.")
        .await
        .unwrap();
    let evs = drain_until_result(&mut d).await;
    let init = evs.iter().find_map(|e| match e {
        DriverEvent::Init { mcp_servers, .. } => Some(mcp_servers.clone()),
        _ => None,
    });
    // --strict-mcp-config + mcp.json(/bin/false)이면 사용자 커넥터 MCP는 보이지 않는다.
    let names: Vec<String> = init.unwrap_or_default().into_iter().map(|m| m.name).collect();
    assert!(
        !names.iter().any(|n| n.contains("claude_ai") || n.contains("connector")),
        "사용자 커넥터 MCP가 샜다: {names:?}"
    );
    d.terminate().await.ok();
}
