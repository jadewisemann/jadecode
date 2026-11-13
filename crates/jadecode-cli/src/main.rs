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
  run [--ws <dir>] [--open]              앱용: 워크스페이스 준비 + 서버를 백그라운드로
                                         (이미 떠 있으면 그대로 쓴다. --open 이면 .app 열기)
  update [--check]                       최신 릴리즈로 제자리 업데이트(서명은 ad-hoc 재적용)
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
                if created {
                    "created"
                } else {
                    "exists (left as is)"
                },
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
                        println!(
                            "{}",
                            ws.root.join(out["path"].as_str().unwrap_or("")).display()
                        );
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
                        println!(
                            "{}",
                            ws.root.join(out["path"].as_str().unwrap_or("")).display()
                        );
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
                    print!(
                        "{}",
                        std::fs::read_to_string(ws.root.join(&c.rel).join("task.md"))?
                    );
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
                        let out =
                            rt().block_on(rpc_call(&sock, "card.advance", json!({"card": c.rel})))?;
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
        // 자체 업데이트: GitHub 최신 릴리즈를 받아 현재 실행 파일들을 갈아 끼운다.
        // 브라우저로 매번 내려받아 Gatekeeper 를 다시 겪지 않게 하는 것이 목적이다.
        Some("update") => {
            let check_only = has_flag(&args, "--check");
            let rt = rt();
            rt.block_on(async move {
                let cur = env!("CARGO_PKG_VERSION");
                let tag = match rt_fetch_latest_tag().await {
                    Ok(t) => t,
                    Err(e) => anyhow::bail!("최신 버전을 확인할 수 없다: {e}"),
                };
                let latest = tag.trim_start_matches('v');
                // 0.1.2-alpha 처럼 -alpha 접미사가 붙는다. 앞 숫자만 비교한다.
                let cur_num = cur.split('-').next().unwrap_or(cur);
                let lat_num = latest.split('-').next().unwrap_or(latest);
                println!("현재: {cur}   최신: {latest}");
                if cur_num >= lat_num {
                    println!("이미 최신이다.");
                    return Ok::<(), anyhow::Error>(());
                }
                if check_only {
                    println!("업데이트 가능: {tag}");
                    return Ok(());
                }
                let plat = platform_tag()?;
                let dir = format!("jadecode-{tag}-{plat}");
                let base = format!("https://github.com/jadewisemann/jadecode/releases/download/{tag}");
                let tmp = std::env::temp_dir().join(format!("jc-update-{}", std::process::id()));
                let _ = std::fs::remove_dir_all(&tmp);
                std::fs::create_dir_all(&tmp)?;
                println!("내려받는 중: {dir}.tar.gz");
                rt_download(&format!("{base}/{dir}.tar.gz"), &tmp.join("a.tgz")).await?;
                // 체크섬이 있으면 검증한다(깨진 파일로 갈아 끼우지 않게).
                let sha_url = format!("{base}/{dir}.tar.gz.sha256");
                if rt_download(&sha_url, &tmp.join("a.sha")).await.is_ok() {
                    let expected = std::fs::read_to_string(tmp.join("a.sha"))?
                        .split_whitespace().next().unwrap_or("").to_string();
                    let actual = jc_core::index::sha_file(&tmp.join("a.tgz"))?;
                    if !expected.is_empty() && !actual.eq_ignore_ascii_case(&expected) {
                        anyhow::bail!("체크섬 불일치: 받은 파일이 손상됐다");
                    }
                    println!("체크섬 OK");
                }
                // 풀기
                let st = std::process::Command::new("tar")
                    .arg("-xzf").arg(tmp.join("a.tgz")).arg("-C").arg(&tmp).status()?;
                if !st.success() { anyhow::bail!("압축 해제 실패"); }
                let src = tmp.join(&dir);
                // 갈아 끼우기: 현재 실행 파일이 있는 폴더에 복사한다.
                let exe = std::env::current_exe()?;
                let exe = std::fs::canonicalize(&exe).unwrap_or(exe);
                let bindir = exe.parent().unwrap_or(std::path::Path::new("/usr/local/bin")).to_path_buf();
                let mut replaced = 0;
                for b in ["jadecode", "jadecode-mcp", "jadecode-client", "jc-bench"] {
                    let s = src.join(b);
                    if !s.exists() { continue; }
                    let d = bindir.join(b);
                    // 실행 중인 자기 자신은 나중에(맥은 실행 중 파일 교체가 가능하다).
                    match std::fs::copy(&s, &d) {
                        Ok(_) => {
                            #[cfg(unix)] {
                                use std::os::unix::fs::PermissionsExt;
                                let _ = std::fs::set_permissions(&d, std::fs::Permissions::from_mode(0o755));
                            }
                            replaced += 1;
                            println!("갱신: {}", d.display());
                        }
                        Err(e) => println!("경고: {} 갱신 실패({e}) — 권한을 확인하라", d.display()),
                    }
                }
                let _ = std::fs::remove_dir_all(&tmp);
                println!("{replaced}개 갱신 완료. 다음 실행부터 새 버전이다.");

                // 맥 앱이 있으면 번들도 교체하고 ad-hoc 서명을 다시 붙인다.
                #[cfg(target_os = "macos")]
                {
                    let app = std::env::var("JADECODE_APP")
                        .unwrap_or_else(|_| "/Applications/jadecode.app".into());
                    let new_app = src.join("jadecode.app");
                    if new_app.is_dir() && std::path::Path::new(&app).is_dir() {
                        let _ = std::process::Command::new("rm").arg("-rf").arg(&app).status();
                        let _ = std::process::Command::new("ditto").arg(&new_app).arg(&app).status();
                        let _ = std::process::Command::new("xattr")
                            .args(["-dr", "com.apple.quarantine"]).arg(&app).status();
                        let _ = std::process::Command::new("codesign")
                            .args(["--force", "--deep", "--timestamp=none", "--sign", "-"]).arg(&app).status();
                        println!("앱 갱신: {app}");
                    }
                }
                Ok(())
            })
        }
        // 앱이 부르는 경로: 워크스페이스 확인 → 서버가 없으면 띄운다 → (선택) 앱 열기.
        // 사용자가 터미널을 열 필요가 없게 하는 것이 목적이다.
        Some("run") => {
            let ws_dir = flag_value(&args, "--ws")
                .map(PathBuf::from)
                .unwrap_or_else(|| {
                    let home = std::env::var("HOME").unwrap_or_else(|_| ".".into());
                    PathBuf::from(home).join("jc/work")
                });
            // 1) 워크스페이스
            if !ws_dir.join("jadecode.toml").exists() {
                std::fs::create_dir_all(&ws_dir)?;
                let name = ws_dir
                    .file_name()
                    .unwrap_or_default()
                    .to_string_lossy()
                    .to_string();
                Workspace::init(&ws_dir, &name)?;
                println!("워크스페이스 생성: {}", ws_dir.display());
            }
            let ws = Workspace::open(&ws_dir)?;
            let sock = default_socket(&ws.root);

            // 2) 서버가 이미 도는가
            let alive = sock.exists()
                && std::os::unix::net::UnixStream::connect(&sock).is_ok();
            if alive {
                println!("서버가 이미 실행 중: {}", sock.display());
            } else {
                // 자기 자신을 다시 불러 서버를 백그라운드로 띄운다(앱이 죽어도 서버는 남는다).
                let exe = std::env::current_exe()?;
                println!("서버 시작: {}", sock.display());
                let mut cmd = std::process::Command::new(&exe);
                cmd.arg("server").arg("--socket").arg(&sock);
                cmd.env("JADECODE_WORKSPACE", &ws.root);
                // 부모의 Claude 세션 변수를 물려주지 않는다(인스턴스 격리와 같은 이유).
                for k in ["CLAUDECODE", "CLAUDE_CODE_SESSION_ID", "CLAUDE_CODE_MESSAGING_SOCKET"] {
                    cmd.env_remove(k);
                }
                cmd.stdin(std::process::Stdio::null())
                    .stdout(std::process::Stdio::null())
                    .stderr(std::process::Stdio::null());
                let child = cmd.spawn()?;
                println!("서버 pid: {}", child.id());
                // 소켓이 생길 때까지 잠깐 기다린다(앱이 바로 붙을 수 있게).
                for _ in 0..150 {
                    if sock.exists() {
                        break;
                    }
                    std::thread::sleep(std::time::Duration::from_millis(20));
                }
            }

            // 3) 앱 열기(맥). --open 이 있거나 JADECODE_OPEN=1 일 때.
            if has_flag(&args, "--open") || std::env::var("JADECODE_OPEN").ok().as_deref() == Some("1") {
                #[cfg(target_os = "macos")]
                {
                    let app = std::env::var("JADECODE_APP")
                        .unwrap_or_else(|_| "/Applications/jadecode.app".into());
                    if std::path::Path::new(&app).is_dir() {
                        let _ = std::process::Command::new("open").arg(&app).status();
                        println!("앱 열기: {app}");
                    } else {
                        println!("앱이 없다: {app} (브라우저에서 설치했으면 Applications 로 옮겨라)");
                    }
                }
                #[cfg(not(target_os = "macos"))]
                {
                    println!("앱 자동 열기는 macOS 에서만 된다: jadecode-client 를 직접 실행하라");
                }
            }
            println!("워크스페이스: {}", ws.root.display());
            Ok(())
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
                    .unwrap_or_else(|| default_socket(&ws.root));
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
                let agent =
                    flag_value(&args, "--agent").context("--agent claude|opencode|omp|pi")?;
                jc_driver::AgentKind::parse(&agent)?;
                let model = flag_value(&args, "--model");
                let dir = ws.root.join("bots").join(name);
                if dir.exists() {
                    bail!("bot exists: {name}");
                }
                std::fs::create_dir_all(dir.join("memory"))?;
                let mut toml =
                    format!("name = \"{name}\"\ntitle = \"{name}\"\nagent = \"{agent}\"\n",);
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
                "{:<10} {:<20} {:<8} {:<8} {:<10} version",
                "agent", "protocol", "ready", "mcp", "client-fs"
            );
            for a in out["agents"].as_array().cloned().unwrap_or_default() {
                println!(
                    "{:<10} {:<20} {:<8} {:<8} {:<10} {}",
                    a["agent"].as_str().unwrap_or(""),
                    a["protocol"].as_str().unwrap_or(""),
                    if a["available"].as_bool().unwrap_or(false) {
                        "yes"
                    } else {
                        "NO"
                    },
                    if a["supports_mcp"].as_bool().unwrap_or(false) {
                        "yes"
                    } else {
                        "no"
                    },
                    if a["client_writes_files"].as_bool().unwrap_or(false) {
                        "client"
                    } else {
                        "agent"
                    },
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
                        if bot == "orchestrator" {
                            "orchestrator".into()
                        } else {
                            "worker".into()
                        }
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
                    let out =
                        rt().block_on(rpc_call(&sock, "instance.stop", json!({"instance": inst})))?;
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
                        if allow {
                            "사람이 승인함".to_string()
                        } else {
                            "사람이 거절함".to_string()
                        }
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
                let (ws, _resp) =
                    tokio_tungstenite::client_async("ws://localhost/rpc", stream).await?;
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
                let (ws, _resp) =
                    tokio_tungstenite::client_async("ws://localhost/rpc", stream).await?;
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
                bail!(
                    "{}: {}",
                    e["code"],
                    e["message"].as_str().unwrap_or("error")
                );
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

/// 기본 소켓 경로. 경로가 유닉스 소켓 한도를 넘으면 짧은 곳에 둔다
/// (macOS `sun_path` 104바이트). 긴 경로에서도 서버가 뜨게 하려는 것이다.
/// 설치된 플랫폼 태그(릴리즈 자산 이름에 쓰는 것과 같아야 한다).
fn platform_tag() -> anyhow::Result<String> {
    let os = if cfg!(target_os = "macos") {
        "macos"
    } else if cfg!(target_os = "linux") {
        "linux"
    } else {
        anyhow::bail!("지원하지 않는 OS");
    };
    let arch = if cfg!(target_arch = "aarch64") {
        "aarch64"
    } else if cfg!(target_arch = "x86_64") {
        "x86_64"
    } else {
        anyhow::bail!("지원하지 않는 CPU");
    };
    Ok(format!("{os}-{arch}"))
}

/// GitHub 최신 릴리즈 태그를 얻는다(`v` 포함).
async fn rt_fetch_latest_tag() -> anyhow::Result<String> {
    let body = rt_get("https://api.github.com/repos/jadewisemann/jadecode/releases").await?;
    let v: serde_json::Value = serde_json::from_str(&body)?;
    let arr = v.as_array().cloned().unwrap_or_default();
    // 초안은 건너뛰고 첫 정식(pre-release 포함) 태그를 쓴다.
    for r in arr {
        if r["draft"].as_bool().unwrap_or(false) {
            continue;
        }
        if let Some(t) = r["tag_name"].as_str() {
            return Ok(t.to_string());
        }
    }
    anyhow::bail!("릴리즈를 찾을 수 없다")
}

async fn rt_get(url: &str) -> anyhow::Result<String> {
    // TLS 를 직접 붙이지 않으려고 curl 을 쓴다(설치 스크립트와 같은 경로).
    let out = std::process::Command::new("curl")
        .args(["-fsSL", "-H", "User-Agent: jadecode-update", url])
        .output()?;
    if !out.status.success() {
        anyhow::bail!("{}", String::from_utf8_lossy(&out.stderr).trim());
    }
    Ok(String::from_utf8_lossy(&out.stdout).to_string())
}

async fn rt_download(url: &str, dest: &std::path::Path) -> anyhow::Result<()> {
    let out = std::process::Command::new("curl")
        .args(["-fsSL", "-o"])
        .arg(dest)
        .arg(url)
        .output()?;
    if !out.status.success() {
        anyhow::bail!("내려받기 실패: {}", String::from_utf8_lossy(&out.stderr).trim());
    }
    Ok(())
}

fn default_socket(root: &std::path::Path) -> PathBuf {
    let inside = root.join(".jadecode/jadecode.sock");
    if inside.as_os_str().len() <= 100 {
        return inside;
    }
    // 워크스페이스 이름으로 짧은 소켓을 만든다(/tmp 안).
    let tag: String = root
        .file_name()
        .map(|s| s.to_string_lossy().to_string())
        .unwrap_or_else(|| "ws".into())
        .chars()
        .map(|c| if c.is_ascii_alphanumeric() { c } else { '-' })
        .collect();
    let tag: String = tag.chars().take(12).collect();
    PathBuf::from(format!("/tmp/jc-{tag}.sock"))
}

fn socket_path(ws: &Workspace, args: &[String]) -> PathBuf {
    flag_value(args, "--socket")
        .map(PathBuf::from)
        .unwrap_or_else(|| default_socket(&ws.root))
}

fn flag_value(args: &[String], name: &str) -> Option<String> {
    args.iter()
        .position(|a| a == name)
        .and_then(|i| args.get(i + 1))
        .cloned()
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
