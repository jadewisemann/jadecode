//! 서버가 없을 때 클라이언트가 같은 프로세스 안에 로컬 백엔드를 띄우는지 본다.

use jc_client::{ensure_backend, BackendKind};

#[test]
fn starts_embedded_backend_when_no_server_then_reuses_it() {
    let ws = jc_core::short_temp_root("jcl");
    jc_core::Workspace::init(&ws, "local").unwrap();

    let b = ensure_backend(Some(&ws)).expect("로컬 백엔드");
    assert_eq!(b.kind, BackendKind::Embedded);
    assert!(b.conn.socket.exists());

    // 보드 스냅숏을 실제로 받아 온다(백엔드 + 프론트엔드 연결 확인).
    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();
    let mut board = rt.block_on(b.conn.open()).expect("연결");
    rt.block_on(board.refresh()).unwrap();
    assert_eq!(board.snapshot().head_seq, 0);

    // 이미 떠 있으면 새로 띄우지 않고 그 백엔드에 붙는다.
    let again = ensure_backend(Some(&ws)).unwrap();
    assert_eq!(again.kind, BackendKind::External);
    assert_eq!(again.conn.socket, b.conn.socket);
}

#[test]
fn rejects_non_workspace_path() {
    let d = jc_core::short_temp_root("jcn");
    assert!(ensure_backend(Some(&d)).is_err());
}
