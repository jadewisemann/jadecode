//! jadecode CLI — 서버 없이도 동작하는 최소 명령.
//!
//! ```text
//! jadecode init <dir> [--name <n>]
//! jadecode project add <name> [--repo <r>]
//! jadecode card add <title> --project <p> [--path a,b,c] [--repo r:b] [--after <id>]...
//! jadecode card list [--project <p>]
//! jadecode card show <card>
//! jadecode card status <card> <status>
//! jadecode card next <card>
//! jadecode server [--socket <path>] [--port <p>] [--fsync batch] [--no-watch]
//! jadecode tail [--topics a,b] [--limit 20]
//! jadecode check
//! ```

use anyhow::{bail, Context, Result};
use futures_util::{SinkExt, StreamExt};
use jc_core::store::Fsync;
use jc_core::taskmd::RepoRef;
use jc_core::workspace::Workspace;
use jc_core::NewCard;
use serde_json::{json, Value};
use std::path::PathBuf;
use tokio_tungstenite::tungstenite::Message;

fn usage() -> &'static str {
    "jadecode <command>

  init <dir> [--name <n>]                워크스페이스 만들기
  project add <name> [--repo <r>]        프로젝트 만들기
  card add <title> --project <p>         카드 만들기
       [--path a,b,c] [--repo r=branch] [--after <id>] [--parent <id>]
  card list [--project <p>]              카드 목록
  card show <card>                       카드 하나(파일 그대로)
  card status <card> <status>            상태 칸 쓰기
  card next <card>                       다음 단계로
  server [--socket <p>] [--port <p>]     서버 띄우기(기본 .jadecode/jadecode.sock)
  tail [--topics a,b] [--limit n]        최근 이벤트
  check                                  일지·색인 전수 검사
"
}

fn main() -> Result<()> {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let rt = || {
        tokio::runtime::Builder::new_multi_thread()
            .enable_all()
            .build()
            .unwrap()
    };
    match args.first().map(|s| s.as_str()) {
        Some("init") => {
            let dir = args.get(1).context("init <dir>")?;
            let name = flag_value(&args, "--name").unwrap_or_else(|| {
                PathBuf::from(dir)
                    .file_name()
                    .unwrap_or_default()
                    .to_string_lossy()
                    .to_string()
            });
            let (ws, created) = Workspace::init(dir, &name)?;
            println!(
                "{} {}",
                if created { "created" } else { "exists (left as is)" },
                ws.root.display()
            );
            Ok(())
        }
        Some("project") => {
            let ws = open_ws()?;
            let mut ws = ws;
            match args.get(1).map(|s| s.as_str()) {
                Some("add") => {
                    let name = args.get(2).context("project add <name>")?;
                    let repo = flag_value(&args, "--repo");
                    let p = ws.add_project(name, repo.as_deref())?;
                    println!("{}", p.display());
                    Ok(())
                }
                _ => bail!(usage()),
            }
        }
        Some("card") => {
            let mut ws = open_ws()?;
            match args.get(1).map(|s| s.as_str()) {
                Some("add") => {
                    let title = args.get(2).context("card add <title>")?;
                    let project = flag_value(&args, "--project").context("--project <p>")?;
                    let path: Vec<String> = flag_value(&args, "--path")
                        .map(|s| s.split(',').map(|x| x.trim().to_string()).collect())
                        .unwrap_or_else(default_path);
                    let repos: Vec<RepoRef> = all_values(&args, "--repo")
                        .into_iter()
                        .map(|s| {
                            let (repo, branch) = s.split_once('=').unwrap_or((s.as_str(), ""));
                            RepoRef {
                                repo: repo.to_string(),
                                branch: branch.to_string(),
                                base: flag_value(&args, "--base").unwrap_or_else(|| "main".into()),
                            }
                        })
                        .collect();
                    let depends_on = all_values(&args, "--after");
                    let parent = flag_value(&args, "--parent");
                    let p = ws.add_card(NewCard {
                        project,
                        title: title.clone(),
                        path,
                        repos,
                        depends_on,
                        parent,
                        body: None,
                    })?;
                    println!("{}", p.display());
                    Ok(())
                }
                Some("list") => {
                    let project = flag_value(&args, "--project");
                    for c in ws.cards()? {
                        if let Some(p) = &project {
                            if !c.rel.starts_with(&format!("work/{p}/")) {
                                continue;
                            }
                        }
                        println!(
                            "{}\t{}\t{}\t{}\t{}",
                            c.state.front.id,
                            c.state.front.stage,
                            c.state.front.status,
                            c.rel,
                            c.state.front.title
                        );
                    }
                    Ok(())
                }
                Some("show") => {
                    let needle = args.get(2).context("card show <card>")?;
                    let c = ws.find_card(needle)?;
                    print!("{}", std::fs::read_to_string(ws.root.join(&c.rel).join("task.md"))?);
                    Ok(())
                }
                Some("status") => {
                    let needle = args.get(2).context("card status <card> <status>")?;
                    let status = args.get(3).context("card status <card> <status>")?;
                    let c = ws.find_card(needle)?;
                    let mut store = ws.store()?;
                    let mut front = c.state.front.clone();
                    front.status = status.clone();
                    let seqs = store.set_front(
                        &c.rel,
                        &front,
                        "card.status_changed",
                        json!({"from": c.state.front.status, "to": status, "source": "cli"}),
                    )?;
                    println!("{}\t{}", c.rel, seqs.last().copied().unwrap_or(0));
                    Ok(())
                }
                Some("next") => {
                    let needle = args.get(2).context("card next <card>")?;
                    let c = ws.find_card(needle)?;
                    let Some(next) = c.state.front.next_stage() else {
                        bail!("card at end of path ({})", c.state.front.stage);
                    };
                    let from = c.state.front.stage.clone();
                    let mut front = c.state.front.clone();
                    front.path_index += 1;
                    front.stage = next.to_string();
                    front.status = "idle".into();
                    let mut store = ws.store()?;
                    let seqs = store.set_front(
                        &c.rel,
                        &front,
                        "card.stage_changed",
                        json!({"from": from, "to": next, "source": "cli"}),
                    )?;
                    println!("{}\t{}\t{}", c.rel, next, seqs.last().copied().unwrap_or(0));
                    Ok(())
                }
                _ => bail!(usage()),
            }
        }
        Some("server") => {
            let ws = open_ws()?;
            let fsync = flag_value(&args, "--fsync")
                .map(|s| s.parse::<Fsync>())
                .transpose()?
                .unwrap_or(Fsync::Batch);
            let config = jc_server::ServerConfig {
                fsync,
                watch: !has_flag(&args, "--no-watch"),
                ..Default::default()
            };
            if let Some(port) = flag_value(&args, "--port") {
                let port: u16 = port.parse()?;
                rt().block_on(jc_server::serve(
                    ws.root.clone(),
                    jc_server::server::Bind::Tcp("127.0.0.1".into(), port),
                    config,
                    |b| println!("{}", b.endpoint()),
                ))
            } else {
                let sock = flag_value(&args, "--socket")
                    .map(PathBuf::from)
                    .unwrap_or_else(|| ws.root.join(".jadecode/jadecode.sock"));
                rt().block_on(jc_server::serve(
                    ws.root.clone(),
                    jc_server::server::Bind::Unix(sock),
                    config,
                    |b| println!("{}", b.endpoint()),
                ))
            }
        }
        Some("tail") => {
            let ws = open_ws()?;
            let topics: Vec<String> = flag_value(&args, "--topics")
                .map(|s| s.split(',').map(|x| x.trim().to_string()).collect())
                .unwrap_or_default();
            let limit: u64 = flag_value(&args, "--limit")
                .and_then(|s| s.parse().ok())
                .unwrap_or(20);
            let evs = jc_core::store::read_tail(&ws.root, &topics, limit)?;
            for e in evs {
                println!("{}", serde_json::to_string(&e)?);
            }
            Ok(())
        }
        Some("check") => {
            let ws = open_ws()?;
            let r = jc_core::store::full_check(&ws.root)?;
            println!("{}", serde_json::to_string_pretty(&r)?);
            if !r.ok {
                std::process::exit(1);
            }
            Ok(())
        }
        Some("rpc") => {
            let ws = open_ws()?;
            let method = args.get(1).context("rpc <method> [json]")?;
            let params: Value = args
                .get(2)
                .map(|s| serde_json::from_str(s))
                .transpose()?
                .unwrap_or(json!({}));
            let sock = socket_path(&ws, &args);
            rt().block_on(async move {
                let stream = tokio::net::UnixStream::connect(&sock).await?;
                let (ws, _resp) = tokio_tungstenite::client_async("ws://localhost/rpc", stream).await?;
                let (mut w, mut r) = ws.split();
                w.send(Message::text(
                    json!({"jsonrpc":"2.0","id":1,"method":method,"params":params}).to_string(),
                ))
                .await?;
                while let Some(m) = r.next().await {
                    let m = m?;
                    if let Message::Text(t) = m {
                        println!("{t}");
                        return Ok(());
                    }
                }
                bail!("no response")
            })
        }
        // stdin의 JSON-RPC 줄을 한 연결로 보내고 응답을 그대로 찍는다(스크립트·부하 시험용).
        Some("rpc-stream") => {
            let ws = open_ws()?;
            let sock = socket_path(&ws, &args);
            let mut lines: Vec<String> = vec![];
            {
                use std::io::BufRead;
                for line in std::io::stdin().lock().lines() {
                    let line = line?;
                    if !line.trim().is_empty() {
                        lines.push(line);
                    }
                }
            }
            let wait_all = !has_flag(&args, "--no-wait");
            rt().block_on(async move {
                let stream = tokio::net::UnixStream::connect(&sock).await?;
                let (ws, _resp) = tokio_tungstenite::client_async("ws://localhost/rpc", stream).await?;
                let (mut w, mut r) = ws.split();
                let n = lines.len();
                let reader = tokio::spawn(async move {
                    let mut got = 0usize;
                    while let Some(Ok(m)) = r.next().await {
                        if let Message::Text(t) = m {
                            println!("{t}");
                            let is_resp = serde_json::from_str::<Value>(&t)
                                .map(|v| v.get("id").is_some())
                                .unwrap_or(false);
                            if is_resp {
                                got += 1;
                                if got >= n {
                                    break;
                                }
                            }
                        }
                    }
                    got
                });
                for l in &lines {
                    w.send(Message::text(l.clone())).await?;
                    tokio::task::yield_now().await;
                }
                if wait_all {
                    let _ = reader.await;
                }
                Ok::<(), anyhow::Error>(())
            })
        }
        _ => bail!(usage()),
    }
}

fn default_path() -> Vec<String> {
    ["구체화", "구현", "검증", "PR", "보고"]
        .iter()
        .map(|s| s.to_string())
        .collect()
}

fn open_ws() -> Result<Workspace> {
    let dir = std::env::var("JADECODE_WORKSPACE")
        .map(PathBuf::from)
        .unwrap_or(std::env::current_dir()?);
    // 위로 올라가며 워크스페이스를 찾는다.
    let mut d = dir.clone();
    loop {
        if d.join("jadecode.toml").exists() {
            return Workspace::open(&d);
        }
        if !d.pop() {
            return Workspace::open(&dir);
        }
    }
}

fn has_flag(args: &[String], name: &str) -> bool {
    args.iter().any(|a| a == name)
}

fn socket_path(ws: &Workspace, args: &[String]) -> PathBuf {
    flag_value(args, "--socket")
        .map(PathBuf::from)
        .unwrap_or_else(|| ws.root.join(".jadecode/jadecode.sock"))
}

fn flag_value(args: &[String], name: &str) -> Option<String> {
    args.iter().position(|a| a == name).and_then(|i| args.get(i + 1)).cloned()
}

fn all_values(args: &[String], name: &str) -> Vec<String> {
    let mut out = vec![];
    for (i, a) in args.iter().enumerate() {
        if a == name {
            if let Some(v) = args.get(i + 1) {
                out.push(v.clone());
            }
        }
    }
    out
}
