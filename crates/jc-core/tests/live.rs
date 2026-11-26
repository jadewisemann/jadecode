//! 진짜 Claude Code로 끝까지 돌려 본다(과금·로그인 필요). `cargo test -p jc-core --test live -- --ignored`
//! root로 돌릴 때는 `IS_SANDBOX=1`이 필요하다(bypassPermissions 제한).

use jc_core::{Column, Engine, Key, Workspace};
use std::process::Command;
use std::time::{Duration, Instant};

fn git(dir: &std::path::Path, args: &[&str]) -> String {
    let out = Command::new("git")
        .arg("-C")
        .arg(dir)
        .args(args)
        .env("GIT_AUTHOR_NAME", "t")
        .env("GIT_AUTHOR_EMAIL", "t@example.com")
        .env("GIT_COMMITTER_NAME", "t")
        .env("GIT_COMMITTER_EMAIL", "t@example.com")
        .output()
        .unwrap();
    assert!(out.status.success(), "git {args:?}");
    String::from_utf8_lossy(&out.stdout).trim().to_string()
}

fn wait(e: &mut Engine, k: &Key, secs: u64) {
    let t = Instant::now();
    while t.elapsed() < Duration::from_secs(secs) {
        e.tick();
        if !e.is_running(k) {
            return;
        }
        std::thread::sleep(Duration::from_millis(200));
    }
    panic!("{secs}초 안에 끝나지 않았다");
}

#[test]
#[ignore]
fn bot_writes_memo_and_agent_does_it() {
    let tmp = std::env::temp_dir().join(format!("jc-live-{}", jc_core::uuid4()));
    let repo = tmp.join("repo");
    std::fs::create_dir_all(&repo).unwrap();
    git(&repo, &["init", "-q", "-b", "main"]);
    std::fs::write(repo.join("README.md"), "# demo\n").unwrap();
    git(&repo, &["add", "."]);
    git(&repo, &["commit", "-qm", "init"]);

    let mut e = Engine::new(Workspace::open(&tmp.join("ws")).unwrap());
    e.create_project("demo", &repo.display().to_string())
        .unwrap();

    // 1) 봇에게 말하면 보드에 메모를 쓴다.
    e.send_bot(
        "demo",
        "hello.txt 파일을 만들고 안에 hi 라고 적는 일을 바로 시작하는 메모로 만들어 줘. 파일 이름은 hello.md로.",
    )
    .unwrap();
    let bot = Key::Bot("demo".into());
    wait(&mut e, &bot, 240);
    for x in e.log(&bot) {
        eprintln!("bot {:?}: {}", x.kind, x.text);
    }
    e.scan();
    let memo = e
        .memos("demo")
        .iter()
        .find(|m| m.stem == "hello")
        .cloned()
        .expect("봇이 hello.md를 쓰지 않았다");
    eprintln!("memo in {:?}:\n{}", memo.column, memo.text);
    if memo.column == Column::Todo {
        e.move_memo("demo", "hello", Column::Doing).unwrap();
    }
    e.tick();

    // 2) 진행 중 메모는 워크트리에서 에이전트가 처리하고 확인 필요로 간다.
    let k = Key::Memo("demo".into(), "hello".into());
    let t = Instant::now();
    while !e.is_running(&k) && t.elapsed() < Duration::from_secs(5) {
        e.tick();
        std::thread::sleep(Duration::from_millis(100));
    }
    wait(&mut e, &k, 400);
    for x in e.log(&k) {
        eprintln!("memo {:?}: {}", x.kind, x.text);
    }
    assert_eq!(e.memo("demo", "hello").unwrap().column, Column::Review);
    let wt = e.worktree_path("demo", "hello");
    assert_eq!(
        std::fs::read_to_string(wt.join("hello.txt"))
            .unwrap()
            .trim(),
        "hi"
    );
    assert_eq!(git(&wt, &["rev-parse", "--abbrev-ref", "HEAD"]), "jc/hello");
    // 원본 레포의 작업 폴더는 그대로다.
    assert!(!repo.join("hello.txt").exists());
    eprintln!("log: {}", git(&wt, &["log", "--oneline"]));
    let _ = std::fs::remove_dir_all(&tmp);
}
