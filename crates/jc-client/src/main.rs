//! jadecode 앱 실행 진입점.
//!
//! ```text
//! jadecode-client [워크스페이스 경로] [--dump] [--version]
//! ```
//! 워크스페이스를 안 주면 `$JADECODE_WORKSPACE`, 그것도 없으면 `~/jc/work`를 만들어 쓴다.

fn main() -> anyhow::Result<()> {
    let args: Vec<String> = std::env::args().skip(1).collect();
    if args.iter().any(|a| a == "--help" || a == "-h") {
        println!("usage: jadecode-client [워크스페이스 경로] [--dump] [--version]");
        println!("  --dump: 창을 열지 않고 보드를 찍는다(시험·스크립트용)");
        return Ok(());
    }
    if args.iter().any(|a| a == "--version" || a == "-V") {
        println!("jadecode-client {}", jc_core::version::VERSION);
        return Ok(());
    }
    let ws = args
        .iter()
        .find(|a| !a.starts_with("--"))
        .map(std::path::PathBuf::from);
    if args.iter().any(|a| a == "--dump") {
        let ws = jc_core::Workspace::resolve(ws.as_deref())?;
        println!("{}", ws.root.display());
        for p in ws.projects() {
            println!("{} ({})", p.name(), p.slug);
            for m in jc_core::board::scan(&p) {
                println!("  [{}] {} — {}", m.column.dir_name(), m.stem, m.title);
            }
        }
        return Ok(());
    }
    #[cfg(feature = "ui")]
    {
        jc_client::logging::init();
        jc_client::logging::install_panic_hook();
        let r = jc_client::ui::run(ws);
        if let Err(e) = &r {
            eprintln!("시작 실패: {e:#}");
            jc_client::logging::fatal_dialog("jadecode를 시작하지 못했습니다", &format!("{e:#}"));
        }
        r
    }
    #[cfg(not(feature = "ui"))]
    {
        let _ = ws;
        anyhow::bail!(
            "이 바이너리는 화면 없이 빌드됐다. `cargo build -p jc-client --features ui`로 다시 빌드하라. (보드만 보려면 --dump)"
        )
    }
}
