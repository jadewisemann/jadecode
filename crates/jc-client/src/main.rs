//! jadecode 클라이언트 실행 진입점.
//!
//! ```text
//! jadecode-client [워크스페이스 경로]
//! ```
//! 워크스페이스를 안 주면 현재 폴더에서 위로 올라가며 `jadecode.toml`을 찾는다.
//! 그래도 없으면 `~/jc/work`를 만들어 쓴다. 떠 있는 서버가 있으면 거기 붙고,
//! 없으면 앱 안에서 로컬 백엔드를 띄운다(따로 `jadecode server`를 띄울 필요가 없다).

fn main() -> anyhow::Result<()> {
    let args: Vec<String> = std::env::args().skip(1).collect();
    if args.iter().any(|a| a == "--help" || a == "-h") {
        println!("usage: jadecode-client [워크스페이스 경로] [--dump] [--version]");
        println!(
            "  떠 있는 서버가 없으면 앱 안에서 로컬 백엔드를 띄운다 (끄려면 JADECODE_NO_EMBED=1)"
        );
        println!("  --dump: 창을 열지 않고 보드 스냅숏을 JSON으로 찍는다(시험·스크립트용)");
        return Ok(());
    }
    if args.iter().any(|a| a == "--version" || a == "-V") {
        println!("jadecode-client {}", jc_core::version::VERSION);
        return Ok(());
    }
    let dump = args.iter().any(|a| a == "--dump");
    let ws = args
        .iter()
        .find(|a| !a.starts_with("--"))
        .map(std::path::PathBuf::from);
    if dump {
        // 창을 못 여는 환경(CI·헤드리스)에서 데이터 계층만 확인한다.
        let backend = jc_client::ensure_backend(ws.as_deref())?;
        eprintln!(
            "backend: {} ({})",
            backend.label(),
            backend.conn.socket.display()
        );
        let conn = backend.conn;
        let rt = tokio::runtime::Builder::new_multi_thread()
            .enable_all()
            .build()?;
        let mut board = rt.block_on(conn.open())?;
        rt.block_on(board.refresh())?;
        println!("{}", serde_json::to_string_pretty(board.snapshot())?);
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
            "이 바이너리는 UI 기능 없이 빌드됐다. `cargo build -p jc-client --features ui`로 다시 빌드하라. \
             (데이터만 보려면 --dump)"
        )
    }
}
