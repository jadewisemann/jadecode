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
  rpc <method> [json]                    서버에 JSON-RPC 한 번
  rpc-stream                             stdin의 JSON-RPC 줄을 한 연결로 보낸다
  bot list                               봇 목록
  bot add <name> --agent <a> [--model <m>]
                                         봇 추가(claude|opencode|omp|pi)
  agents                                 지원 에이전트와 설치 여부
  instance list                          인스턴스 목록
  instance start <bot> --project <p>     인스턴스 띄우기
       [--card <card>] [--role worker|orchestrator] [--model <m>]
  instance send <instance> <text>        인스턴스에 메시지
  instance stop <instance>               인스턴스 멈춤
  permission list                        사람 답을 기다리는 권한 요청
  permission allow <request_seq> [msg]   승인
  permission deny <request_seq> [msg]    거절
  policy                                 사람이 정한 값 보기
  top                                    보드(카드 목록)
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
            let mut ws = open_ws()?;
            match args.get(1).map(|s| s.as_str()) {
                Some("add") => {
                    let name = args.get(2).context("project add <name>")?;
                    let repo = flag_value(&args, "--repo");
                    if let Some(sock) = server_up(&ws, &args) {
                        let out = rt().block_on(rpc_call(
                            &sock,
                            "project.add",
                            json!({"name": name, "repo": repo}),
                        ))?;
                        println!("{}", ws.root.join(out["path"].as_str().unwrap_or("")).display());
                        return Ok(());
                    }
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
                    if let Some(sock) = server_up(&ws, &args) {
                        let out = rt().block_on(rpc_call(
                            &sock,
                            "card.add",
                            json!({"project":project,"title":title,"path":path,"repos":repos,
                                   "depends_on":depends_on,"parent":parent}),
                        ))?;
                        println!("{}", ws.root.join(out["path"].as_str().unwrap_or("")).display());
                        return Ok(());
                    }
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
                    if let Some(sock) = server_up(&ws, &args) {
                        let out = rt().block_on(rpc_call(
                            &sock,
                            "card.set_status",
                            json!({"card": c.rel, "status": status}),
                        ))?;
                        println!("{}\t{}", c.rel, out["seqs"][0].as_u64().unwrap_or(0));
                        return Ok(());
                    }
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
                    if let Some(sock) = server_up(&ws, &args) {
                        let out = rt().block_on(rpc_call(
                            &sock,
                            "card.advance",
                            json!({"card": c.rel}),
                        ))?;
                        println!(
                            "{}\t{}\t{}",
                            c.rel,
                            out["stage"].as_str().unwrap_or(""),
                            out["seqs"][0].as_u64().unwrap_or(0)
                        );
                        return Ok(());
                    }
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
        Some("bot") => {
            let ws = open_ws()?;
            let sock = socket_path(&ws, &args);
            if args.get(1).map(|s| s.as_str()) == Some("add") {
                let name = args.get(2).context("bot add <name> --agent <a>")?;
                let agent = flag_value(&args, "--agent").context("--agent claude|opencode|omp|pi")?;
                jc_driver::AgentKind::parse(&agent)?;
                let model = flag_value(&args, "--model");
                let dir = ws.root.join("bots").join(name);
                if dir.exists() {
                    bail!("bot exists: {name}");
                }
                std::fs::create_dir_all(dir.join("memory"))?;
                let mut toml = format!(
                    "name = \"{name}\"\ntitle = \"{name}\"\nagent = \"{agent}\"\n",
                );
                if let Some(m) = &model {
                    toml.push_str(&format!("model = \"{m}\"\n"));
                }
                toml.push_str("instructions = \"instructions.md\"\npolicy = \"default\"\nallowed_skills = []\n");
                std::fs::write(dir.join("bot.toml"), toml)?;
                std::fs::write(
                    dir.join("instructions.md"),
                    format!("# {name}\n\n너는 jadecode의 {name} 봇이다.\n\n- 카드 폴더의 task.md를 읽고 단계 목표를 수행한다.\n- 끝나면 report(status=done|progress|blocked, summary)를 부른다.\n- 사람 승인이 필요한 일은 propose로 올린다.\n"),
                )?;
                println!("{}", dir.display());
                return Ok(());
            }
            let out = rt().block_on(rpc_call(&sock, "bots.list", json!({})))?;
            for b in out["bots"].as_array().cloned().unwrap_or_default() {
                println!(
                    "{}\t{}\t{}\t{}\t{}\t{}",
                    b["name"].as_str().unwrap_or(""),
                    b["agent"].as_str().unwrap_or(""),
                    b["title"].as_str().unwrap_or(""),
                    b["model"].as_str().unwrap_or("-"),
                    b["policy"].as_str().unwrap_or(""),
                    b["error"].as_str().unwrap_or(""),
                );
            }
            Ok(())
        }
        Some("agents") => {
            let ws = open_ws()?;
            let sock = socket_path(&ws, &args);
            let out = rt().block_on(rpc_call(&sock, "agents.list", json!({})))?;
            println!(
                "{:<10} {:<20} {:<8} {:<8} {:<10} {}",
                "agent", "protocol", "ready", "mcp", "client-fs", "version"
            );
            for a in out["agents"].as_array().cloned().unwrap_or_default() {
                println!(
                    "{:<10} {:<20} {:<8} {:<8} {:<10} {}",
                    a["agent"].as_str().unwrap_or(""),
                    a["protocol"].as_str().unwrap_or(""),
                    if a["available"].as_bool().unwrap_or(false) { "yes" } else { "NO" },
                    if a["supports_mcp"].as_bool().unwrap_or(false) { "yes" } else { "no" },
                    if a["client_writes_files"].as_bool().unwrap_or(false) { "client" } else { "agent" },
                    a["version"].as_str().unwrap_or(""),
                );
            }
            Ok(())
        }
        Some("policy") => {
            let ws = open_ws()?;
            let sock = socket_path(&ws, &args);
            let out = rt().block_on(rpc_call(&sock, "policy.get", json!({})))?;
            println!("{}", serde_json::to_string_pretty(&out)?);
            Ok(())
        }
        Some("top") => {
            let ws = open_ws()?;
            let sock = socket_path(&ws, &args);
            let out = rt().block_on(rpc_call(&sock, "cards.list", json!({})))?;
            let cards = out["cards"].as_array().cloned().unwrap_or_default();
            // 단계별로 묶어 보드처럼 보여 준다.
            let mut by_stage: std::collections::BTreeMap<String, Vec<Value>> = Default::default();
            for c in cards {
                by_stage
                    .entry(c["stage"].as_str().unwrap_or("-").to_string())
                    .or_default()
                    .push(c);
            }
            for (stage, cs) in by_stage {
                println!("[{stage}]");
                for c in cs {
                    println!(
                        "  {}  {}  {} ({})",
                        c["status"].as_str().unwrap_or(""),
                        c["title"].as_str().unwrap_or(""),
                        c["id"].as_str().unwrap_or(""),
                        c["project"].as_str().unwrap_or("")
                    );
                }
            }
            Ok(())
        }
        Some("instance") => {
            let ws = open_ws()?;
            let sock = socket_path(&ws, &args);
            match args.get(1).map(|s| s.as_str()) {
                Some("list") => {
                    let out = rt().block_on(rpc_call(&sock, "instances.list", json!({})))?;
                    for i in out["instances"].as_array().cloned().unwrap_or_default() {
                        println!(
                            "{}\t{}\t{}\t{}\t{}\tturns={}\tpid={}",
                            i["id"].as_str().unwrap_or(""),
                            i["role"].as_str().unwrap_or(""),
                            i["state"].as_str().unwrap_or(""),
                            i["bot"].as_str().unwrap_or(""),
                            i["card"].as_str().or(i["project"].as_str()).unwrap_or(""),
                            i["turns"].as_u64().unwrap_or(0),
                            i["pid"].as_u64().unwrap_or(0),
                        );
                    }
                    Ok(())
                }
                Some("start") => {
                    let bot = args.get(2).context("instance start <bot> --project <p>")?;
                    let project = flag_value(&args, "--project").context("--project <p>")?;
                    let role = flag_value(&args, "--role").unwrap_or_else(|| {
                        if bot == "orchestrator" { "orchestrator".into() } else { "worker".into() }
                    });
                    let mut params = json!({"role": role, "project": project, "bot": bot});
                    if let Some(c) = flag_value(&args, "--card") {
                        params["card"] = json!(c);
                    }
                    if let Some(m) = flag_value(&args, "--model") {
                        params["model"] = json!(m);
                    }
                    if let Some(i) = flag_value(&args, "--id") {
                        params["id"] = json!(i);
                    }
                    if let Some(a) = flag_value(&args, "--agent") {
                        params["agent"] = json!(a);
                    }
                    let out = rt().block_on(rpc_call(&sock, "instance.start", params))?;
                    println!("{}", serde_json::to_string_pretty(&out)?);
                    Ok(())
                }
                Some("send") => {
                    let inst = args.get(2).context("instance send <instance> <text>")?;
                    let text = args[3..].join(" ");
                    if text.trim().is_empty() {
                        bail!("text required");
                    }
                    let out = rt().block_on(rpc_call(
                        &sock,
                        "instance.send",
                        json!({"instance": inst, "text": text}),
                    ))?;
                    println!("{}", out);
                    Ok(())
                }
                Some("stop") => {
                    let inst = args.get(2).context("instance stop <instance>")?;
                    let out = rt().block_on(rpc_call(
                        &sock,
                        "instance.stop",
                        json!({"instance": inst}),
                    ))?;
                    println!("{}", out);
                    Ok(())
                }
                _ => bail!(usage()),
            }
        }
        Some("permission") => {
            let ws = open_ws()?;
            let sock = socket_path(&ws, &args);
            match args.get(1).map(|s| s.as_str()) {
                Some("list") => {
                    // 대기 중인 요청은 일지에서 `permission.requested` 뒤 `permission.resolved`가 없는 것.
                    let evs = jc_core::store::read_tail(&ws.root, &[], 500)?;
                    let mut open: std::collections::BTreeMap<u64, Value> = Default::default();
                    for e in evs {
                        let kind = e["kind"].as_str().unwrap_or("");
                        let seq = e["seq"].as_u64().unwrap_or(0);
                        if kind == "permission.requested" {
                            open.insert(seq, e);
                        } else if kind == "permission.resolved" {
                            if let Some(s) = e["body"]["request_seq"].as_u64() {
                                open.remove(&s);
                            }
                        }
                    }
                    if open.is_empty() {
                        println!("(대기 중인 권한 요청 없음)");
                    }
                    for (seq, e) in open {
                        println!(
                            "{}\t{}\t{}\t{}",
                            seq,
                            e["body"]["instance"].as_str().unwrap_or(""),
                            e["body"]["tool"].as_str().unwrap_or(""),
                            serde_json::to_string(&e["body"]["input"]).unwrap_or_default()
                        );
                    }
                    Ok(())
                }
                Some("allow") | Some("deny") => {
                    let allow = args[1] == "allow";
                    let seq: u64 = args
                        .get(2)
                        .context("permission allow|deny <request_seq> [msg]")?
                        .parse()?;
                    let msg = args[3..].join(" ");
                    let msg = if msg.trim().is_empty() {
                        if allow { "사람이 승인함".to_string() } else { "사람이 거절함".to_string() }
                    } else {
                        msg
                    };
                    let out = rt().block_on(rpc_call(
                        &sock,
                        "permission.resolve",
                        json!({"request_seq": seq, "allow": allow, "message": msg}),
                    ))?;
                    println!("{out}");
                    Ok(())
                }
                _ => bail!(usage()),
            }
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

async fn rpc_call(socket: &PathBuf, method: &str, params: Value) -> Result<Value> {
    let stream = tokio::net::UnixStream::connect(socket)
        .await
        .with_context(|| format!("서버에 붙을 수 없다: {}", socket.display()))?;
    let (ws, _resp) = tokio_tungstenite::client_async("ws://localhost/rpc", stream).await?;
    let (mut w, mut r) = ws.split();
    w.send(Message::text(
        json!({"jsonrpc":"2.0","id":1,"method":method,"params":params}).to_string(),
    ))
    .await?;
    while let Some(m) = r.next().await {
        let m = m?;
        if let Message::Text(t) = m {
            let v: Value = serde_json::from_str(&t)?;
            if let Some(e) = v.get("error") {
                bail!("{}: {}", e["code"], e["message"].as_str().unwrap_or("error"));
            }
            return Ok(v.get("result").cloned().unwrap_or(Value::Null));
        }
    }
    bail!("no response")
}

/// 서버가 살아 있으면 RPC를 쓴다(쓰기 프로세스는 하나여야 한다).
fn server_up(ws: &Workspace, args: &[String]) -> Option<PathBuf> {
    let p = socket_path(ws, args);
    if p.exists() {
        Some(p)
    } else {
        None
    }
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
