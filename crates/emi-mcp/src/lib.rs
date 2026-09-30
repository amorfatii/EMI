//! `emi mcp`: a Model Context Protocol server over stdio (newline-delimited
//! JSON-RPC 2.0). Agents call EMI directly: gate the change they just made
//! (EMI measures the candidate itself — the agent never supplies numbers),
//! explain a file's burden, find hotspots, and check a refactor.

mod tools;

use std::io::{BufRead, Write};

use serde_json::{Value, json};

const PROTOCOL_VERSIONS: [&str; 3] = ["2025-06-18", "2025-03-26", "2024-11-05"];

fn response(id: &Value, result: Result<Value, (i64, String)>) -> Value {
    match result {
        Ok(result) => json!({"jsonrpc": "2.0", "id": id, "result": result}),
        Err((code, message)) => {
            json!({"jsonrpc": "2.0", "id": id, "error": {"code": code, "message": message}})
        }
    }
}

fn initialize(params: &Value) -> Value {
    let requested = params
        .get("protocolVersion")
        .and_then(Value::as_str)
        .unwrap_or("");
    let version = PROTOCOL_VERSIONS
        .iter()
        .find(|v| **v == requested)
        .copied()
        .unwrap_or(PROTOCOL_VERSIONS[0]);
    json!({
        "protocolVersion": version,
        "capabilities": {"tools": {"listChanged": false}},
        "serverInfo": {"name": "emi", "version": env!("CARGO_PKG_VERSION")},
        "instructions": "EMI measures structural maintenance burden (E erosion, V redundancy, C coupling) and gates changes on burden growth it can prove. Call emi_gate after editing code: it measures your working tree against a base ref and returns block/warn/pass with the files responsible. Use emi_explain on those files and emi_hotspots to find where burden concentrates. If a supervised task's tests or checks contradict its objective, call emi_task_escalate instead of editing, skipping or special-casing tests."
    })
}

/// Handle one JSON-RPC message; `None` for notifications.
pub fn handle(message: &Value) -> Option<Value> {
    let id = message.get("id")?;
    let method = message.get("method").and_then(Value::as_str).unwrap_or("");
    let params = message.get("params").cloned().unwrap_or_else(|| json!({}));
    let result = match method {
        "initialize" => Ok(initialize(&params)),
        "ping" => Ok(json!({})),
        "tools/list" => Ok(json!({"tools": tools::list()})),
        "tools/call" => {
            let name = params.get("name").and_then(Value::as_str).unwrap_or("");
            let args = params
                .get("arguments")
                .cloned()
                .unwrap_or_else(|| json!({}));
            match tools::call(name, &args) {
                Some(Ok(value)) => Ok(
                    json!({"content": [{"type": "text", "text": emi_ledger::pyfmt::dumps_pretty(&value)}],
                                         "isError": false}),
                ),
                Some(Err(message)) => {
                    Ok(json!({"content": [{"type": "text", "text": message}], "isError": true}))
                }
                None => Err((-32602, format!("unknown tool: {name}"))),
            }
        }
        _ => Err((-32601, format!("method not found: {method}"))),
    };
    Some(response(id, result))
}

/// Serve until the input closes.
pub fn serve(input: impl BufRead, mut output: impl Write) -> std::io::Result<()> {
    for line in input.lines() {
        let line = line?;
        if line.trim().is_empty() {
            continue;
        }
        let reply = match serde_json::from_str::<Value>(&line) {
            Ok(message) => handle(&message),
            Err(e) => Some(response(
                &Value::Null,
                Err((-32700, format!("parse error: {e}"))),
            )),
        };
        if let Some(reply) = reply {
            writeln!(output, "{reply}")?;
            output.flush()?;
        }
    }
    Ok(())
}
