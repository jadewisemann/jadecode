//! jadecode 명령줄. 보드는 파일이라 앱 없이도 보고 고칠 수 있다.

use anyhow::{bail, Context, Result};
use jc_core::board::{self, Column};
use jc_core::workspace::{tilde, Workspace};
use std::io::Read;
use std::path::PathBuf;

fn usage() -> String {
    "jadecode — 프로젝트별 봇 대화 + 칸반 메모(md) + Claude Code

usage:
  jadecode ls [프로젝트]                      프로젝트 목록, 또는 그 프로젝트의 보드
  jadecode new <이름> [--repo <레포 폴더>]     프로젝트 만들기
  jadecode memo <프로젝트> <제목> [--doing]    메모 쓰기(본문은 표준 입력). --doing이면 앱이 바로 시작한다
  jadecode mv <프로젝트> <메모> <todo|doing|review|done>
  jadecode update [--check]                   최신 릴리즈로 업데이트
  jadecode version

공통: --ws <워크스페이스>  (기본: $JADECODE_WORKSPACE, 없으면 ~/jc/work)"
        .to_string()
}

fn flag_value(args: &[String], name: &str) -> Option<String> {
    args.iter()
        .position(|a| a == name)
        .and_then(|i| args.get(i + 1))
        .cloned()
}

/// 플래그와 그 값을 뺀 위치 인자.
fn positional(args: &[String]) -> Vec<String> {
    let mut out = vec![];
    let mut skip = false;
    for a in args {
        if skip {
            skip = false;
            continue;
        }
        if a == "--ws" || a == "--repo" {
            skip = true;
            continue;
        }
        if a.starts_with("--") {
            continue;
        }
        out.push(a.clone());
    }
    out
}

fn main() {
    if let Err(e) = run() {
        eprintln!("오류: {e:#}");
        std::process::exit(1);
    }
}

fn run() -> Result<()> {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let pos = positional(&args);
    let ws = || Workspace::resolve(flag_value(&args, "--ws").map(PathBuf::from).as_deref());
    match pos.first().map(String::as_str) {
        None | Some("help") => println!("{}", usage()),
        Some("version") => println!("jadecode {}", jc_core::version::VERSION),
        Some("ls") => {
            let ws = ws()?;
            match pos.get(1) {
                None => {
                    println!("워크스페이스: {}", tilde(&ws.root));
                    for p in ws.projects() {
                        let memos = board::scan(&p);
                        let n = |c| memos.iter().filter(|m| m.column == c).count();
                        println!(
                            "  {:<20} 할 일 {} · 진행 중 {} · 확인 필요 {} · 완료 {}   {}",
                            p.slug,
                            n(Column::Todo),
                            n(Column::Doing),
                            n(Column::Review),
                            n(Column::Done),
                            p.file.repo
                        );
                    }
                }
                Some(slug) => {
                    let p = ws.load_project(slug)?;
                    let memos = board::scan(&p);
                    for c in Column::ALL {
                        println!("{} ({}/)", c.label(), c.dir_name());
                        for m in memos.iter().filter(|m| m.column == c) {
                            println!("  {:<28} {}", m.stem, m.title);
                        }
                    }
                }
            }
        }
        Some("new") => {
            let name = pos.get(1).context("프로젝트 이름이 필요하다")?;
            let repo = flag_value(&args, "--repo").unwrap_or_default();
            let p = ws()?.create_project(name, &repo)?;
            println!("만들었다: {}", tilde(&p.dir));
        }
        Some("memo") => {
            let (Some(slug), Some(title)) = (pos.get(1), pos.get(2)) else {
                bail!("usage: jadecode memo <프로젝트> <제목> [--doing]")
            };
            let p = ws()?.load_project(slug)?;
            let mut body = String::new();
            if !atty_stdin() {
                std::io::stdin().read_to_string(&mut body)?;
            }
            let col = if args.iter().any(|a| a == "--doing") {
                Column::Doing
            } else {
                Column::Todo
            };
            let m = board::create(&p, title, &body, col)?;
            println!("{}", tilde(&m.path));
        }
        Some("mv") => {
            let (Some(slug), Some(stem), Some(col)) = (pos.get(1), pos.get(2), pos.get(3)) else {
                bail!("usage: jadecode mv <프로젝트> <메모> <todo|doing|review|done>")
            };
            let col = Column::from_dir(col).context("열은 todo, doing, review, done 중 하나다")?;
            let p = ws()?.load_project(slug)?;
            let m = board::move_to(&p, stem, col)?;
            println!("{}", tilde(&m.path));
        }
        Some("update") => {
            let cur = jc_core::version::VERSION;
            let tag = jc_core::update::latest_tag().context("최신 버전을 확인할 수 없다")?;
            println!("현재: {cur}   최신: {}", tag.trim_start_matches('v'));
            if !jc_core::version::is_newer(tag.trim_start_matches('v'), cur) {
                println!("이미 최신이다.");
                return Ok(());
            }
            if args.iter().any(|a| a == "--check") {
                println!("업데이트 가능: {tag}");
                return Ok(());
            }
            let exe = std::env::current_exe()?;
            let exe = std::fs::canonicalize(&exe).unwrap_or(exe);
            let bindir = exe.parent().context("실행 파일 폴더를 알 수 없다")?;
            jc_core::update::install(&tag, bindir, &|s| println!("{s}"))?;
            println!("끝. 다음 실행부터 새 버전이다.");
        }
        Some(other) => bail!("모르는 명령: {other}\n\n{}", usage()),
    }
    Ok(())
}

fn atty_stdin() -> bool {
    unsafe { libc_isatty(0) == 1 }
}

extern "C" {
    #[link_name = "isatty"]
    fn libc_isatty(fd: i32) -> i32;
}
