//! jadecode 서버에 붙는 JSON-RPC 클라이언트(WebSocket over Unix 소켓).
//!
//! 호출마다 연결을 하나 열고 닫는다. 값싸고(유닉스 소켓) 상태가 남지 않는다.
//! MCP 도구는 스레드에서 처리되므로 `permission`이 오래 붙들고 있어도 다른 호출은 각자 연결을 쓴다.

use anyhow::{bail, Result};
use futures_util::{SinkExt, StreamExt};
use serde_json::{json, Value};
use std::path::{Path, PathBuf};
use tokio_tungstenite::tungstenite::Message;

/// 한 번의 호출. 내부에서 런타임을 만들고 끝나면 버린다.
pub fn call(socket: &Path, method: &str, params: Value) -> Result<Value> {
    let sock = PathBuf::from(socket);
    let method = method.to_string();
    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()?;
    rt.block_on(async move { call_async(&sock, &method, params).await })
}

/// 이미 런타임 안이면 이걸 쓴다.
pub async fn call_async(socket: &Path, method: &str, params: Value) -> Result<Value> {
    let stream = tokio::net::UnixStream::connect(socket)
        .await
        .map_err(|e| anyhow::anyhow!("서버에 붙을 수 없다({}): {e}", socket.display()))?;
    let (mut ws, _) = tokio_tungstenite::client_async("ws://localhost/jadecode", stream).await?;
    ws.send(Message::text(
        json!({"jsonrpc":"2.0","id":1,"method":method,"params":params}).to_string(),
    ))
    .await?;
    while let Some(msg) = ws.next().await {
        let msg = msg?;
        let Message::Text(t) = msg else { continue };
        let v: Value = serde_json::from_str(&t)?;
        if v.get("id").and_then(|x| x.as_u64()) != Some(1) {
            continue; // 구독 알림 등
        }
        if let Some(e) = v.get("error") {
            bail!(
                "{}: {}",
                e["code"],
                e["message"].as_str().unwrap_or("error")
            );
        }
        let _ = ws.close(None).await;
        return Ok(v.get("result").cloned().unwrap_or(Value::Null));
    }
    bail!("connection closed before response")
}
