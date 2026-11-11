//! stdio MCP 전송. JSON-RPC 한 줄씩(stdin/stdout).
//!
//! `tools/call`은 스레드에서 처리한다. `permission` 호출은 사람/정책 답을 기다리며 오래
//! 걸릴 수 있는데, 그동안 다른 요청(취소 등)을 계속 받아야 하기 때문이다.

use anyhow::Result;
use jc_mcp::{Handle, Role, Tools, PROTOCOL_VERSION};
use serde_json::{json, Value};
use std::io::{BufRead, Write};
use std::sync::{Arc, Mutex};

fn arg(args: &[String], name: &str) -> Option<String> {
    args.iter()
        .position(|a| a == name)
        .and_then(|i| args.get(i + 1))
        .cloned()
}

fn usage() -> &'static str {
    "jadecode-mcp --socket <path> [--role worker|orchestrator] [--instance <id>] [--card <work/p/c>] [--project <p>]"
}

fn main() -> Result<()> {
    let args: Vec<String> = std::env::args().collect();
    let Some(socket) = arg(&args, "--socket") else {
        eprintln!("{}", usage());
        std::process::exit(2);
    };
    let handle = Handle {
        role: Role::parse(&arg(&args, "--role").unwrap_or_else(|| "worker".into())),
        instance_id: arg(&args, "--instance").unwrap_or_default(),
        card: arg(&args, "--card"),
        project: arg(&args, "--project").unwrap_or_default(),
        socket: socket.into(),
    };
    let tools = Arc::new(Tools::new(handle));
    let out = Arc::new(Mutex::new(std::io::stdout()));

    fn send(out: &Arc<Mutex<std::io::Stdout>>, v: Value) {
        let mut o = out.lock().unwrap();
        let _ = writeln!(o, "{}", v);
        let _ = o.flush();
    }

    let stdin = std::io::stdin();
    for line in stdin.lock().lines() {
        let line = line?;
        if line.trim().is_empty() {
            continue;
        }
        let msg: Value = match serde_json::from_str(&line) {
            Ok(v) => v,
            Err(e) => {
                send(&out, json!({"jsonrpc":"2.0","id":null,"error":{"code":-32700,"message":e.to_string()}}));
                continue;
            }
        };
        let id = msg.get("id").cloned();
        let method = msg.get("method").and_then(|m| m.as_str()).unwrap_or("");
        let params = msg.get("params").cloned().unwrap_or(json!({}));
        match method {
            "initialize" => {
                let ver = params
                    .get("protocolVersion")
                    .and_then(|v| v.as_str())
                    .unwrap_or(PROTOCOL_VERSION);
                send(
                    &out,
                    json!({"jsonrpc":"2.0","id":id,"result":{
                        "protocolVersion": ver,
                        "capabilities": {"tools": {}},
                        "serverInfo": {"name": "jadecode", "version": env!("CARGO_PKG_VERSION")}
                    }}),
                );
            }
            "notifications/initialized" | "initialized" => {}
            "tools/list" => {
                send(&out, json!({"jsonrpc":"2.0","id":id,"result":{"tools": tools.list()}}));
            }
            "tools/call" => {
                let name = params
                    .get("name")
                    .and_then(|v| v.as_str())
                    .unwrap_or("")
                    .to_string();
                let args = params.get("arguments").cloned().unwrap_or(json!({}));
                let t = tools.clone();
                let o = out.clone();
                // 오래 걸릴 수 있으므로 스레드에서 답한다(동시 요청·취소를 계속 받는다).
                std::thread::spawn(move || {
                    let result = t.call(&name, &args);
                    send(&o, json!({"jsonrpc":"2.0","id":id,"result":result}));
                });
            }
            "notifications/cancelled" => {}
            "ping" => send(&out, json!({"jsonrpc":"2.0","id":id,"result":{}})),
            other => {
                if id.is_some() {
                    send(
                        &out,
                        json!({"jsonrpc":"2.0","id":id,"error":{"code":-32601,"message":format!("no method {other}")}}),
                    );
                }
            }
        }
    }
    Ok(())
}
