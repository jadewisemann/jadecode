//! jadecode 클라이언트 실행 진입점.
//!
//! ```text
//! jadecode-client [워크스페이스 경로]
//! ```
//! 워크스페이스를 안 주면 현재 폴더에서 위로 올라가며 `jadecode.toml`을 찾는다.
//! 서버는 `jadecode server`로 먼저 띄워 두어야 한다.

fn main() -> anyhow::Result<()> {
    let args: Vec<String> = std::env::args().skip(1).collect();
    if args.iter().any(|a| a == "--help" || a == "-h") {
        println!("usage: jadecode-client [워크스페이스 경로] [--dump]");
        println!("  서버가 떠 있어야 한다: jadecode server");
        println!("  --dump: 창을 열지 않고 보드 스냅숏을 JSON으로 찍는다(시험·스크립트용)");
        return Ok(());
    }
    let dump = args.iter().any(|a| a == "--dump");
    let ws = args
        .iter()
        .find(|a| !a.starts_with("--"))
        .map(std::path::PathBuf::from);
    if dump {
        // 창을 못 여는 환경(CI·헤드리스)에서 데이터 계층만 확인한다.
        let conn = jc_client::Connection::discover(ws.as_deref())?;
        let rt = tokio::runtime::Builder::new_multi_thread().enable_all().build()?;
        let mut board = rt.block_on(conn.open())?;
        rt.block_on(board.refresh())?;
        println!("{}", serde_json::to_string_pretty(board.snapshot())?);
        return Ok(());
    }
    jc_client::ui::run(ws)
}
