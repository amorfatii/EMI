//! `emi lsp`: a language server (stdio, JSON-RPC with Content-Length
//! framing). Diagnostics come from EMI's rule catalog (and the workspace's
//! `.emi/rules`) plus erosion hints; code lenses show each notable
//! function's erosion, complexity and size, and its erosion change since a
//! base revision (`initializationOptions.base`, default `HEAD`).

pub mod analysis;

use std::collections::HashMap;
use std::io::{BufRead, Write};
use std::path::{Path, PathBuf};

use emi_rules::engine::Compiled;
use serde_json::{Value, json};

/// Read one framed message; `None` at end of input.
pub fn read_message(input: &mut impl BufRead) -> std::io::Result<Option<Value>> {
    let mut length = None;
    loop {
        let mut line = String::new();
        if input.read_line(&mut line)? == 0 {
            return Ok(None);
        }
        let line = line.trim_end();
        if line.is_empty() {
            break;
        }
        if let Some(v) = line.strip_prefix("Content-Length:") {
            length = v.trim().parse::<usize>().ok();
        }
    }
    let Some(n) = length else {
        return Ok(Some(Value::Null));
    };
    let mut body = vec![0; n];
    input.read_exact(&mut body)?;
    Ok(Some(serde_json::from_slice(&body).unwrap_or(Value::Null)))
}

pub fn write_message(output: &mut impl Write, message: &Value) -> std::io::Result<()> {
    let body = message.to_string();
    write!(output, "Content-Length: {}\r\n\r\n{body}", body.len())?;
    output.flush()
}

fn percent_decode(s: &str) -> String {
    let bytes = s.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'%'
            && let Some(v) = s
                .get(i + 1..i + 3)
                .and_then(|h| u8::from_str_radix(h, 16).ok())
        {
            out.push(v);
            i += 3;
            continue;
        }
        out.push(bytes[i]);
        i += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

pub fn uri_to_path(uri: &str) -> Option<PathBuf> {
    uri.strip_prefix("file://")
        .map(|p| PathBuf::from(percent_decode(p)))
}

/// Server state.
pub struct Server {
    root: PathBuf,
    base: String,
    rules: Vec<Compiled>,
    documents: HashMap<String, String>,
    today: String,
}

fn today() -> String {
    let days = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_secs() / 86_400);
    let z = i64::try_from(days).unwrap_or(0) + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    format!("{:04}-{m:02}-{d:02}", yoe + era * 400 + i64::from(m <= 2))
}

impl Server {
    pub fn new(root: PathBuf, base: String) -> Server {
        let local = root.join(".emi").join("rules");
        let paths: Vec<PathBuf> = if local.is_dir() {
            vec![local]
        } else {
            Vec::new()
        };
        let rules = emi_rules::load(&paths, true)
            .ok()
            .map(|r| {
                r.into_iter()
                    .filter_map(|r| emi_rules::engine::compile(r).ok())
                    .collect()
            })
            .unwrap_or_default();
        Server {
            root,
            base,
            rules,
            documents: HashMap::new(),
            today: today(),
        }
    }

    fn rel(&self, uri: &str) -> String {
        let path = uri_to_path(uri).unwrap_or_default();
        path.strip_prefix(&self.root)
            .map_or_else(|_| path.clone(), Path::to_path_buf)
            .to_string_lossy()
            .replace('\\', "/")
    }

    /// The file's content at the base revision, if tracked there.
    fn base_text(&self, rel: &str) -> Option<String> {
        let out = std::process::Command::new("git")
            .arg("-C")
            .arg(&self.root)
            .args(["show", &format!("{}:./{rel}", self.base)])
            .output()
            .ok()?;
        out.status
            .success()
            .then(|| String::from_utf8_lossy(&out.stdout).into_owned())
    }

    fn publish(&self, uri: &str) -> Value {
        let text = self.documents.get(uri).map_or("", String::as_str);
        let diagnostics = analysis::diagnostics(&self.rules, &self.rel(uri), text, &self.today);
        json!({"jsonrpc": "2.0", "method": "textDocument/publishDiagnostics",
               "params": {"uri": uri, "diagnostics": diagnostics}})
    }

    /// Handle one message; returns the messages to send.
    pub fn handle(&mut self, msg: &Value) -> (Vec<Value>, bool) {
        let method = msg["method"].as_str().unwrap_or("");
        let id = msg.get("id").cloned();
        let reply = |result: Value| json!({"jsonrpc": "2.0", "id": id, "result": result});
        let uri = msg["params"]["textDocument"]["uri"]
            .as_str()
            .unwrap_or("")
            .to_owned();
        match method {
            "initialize" => {
                if let Some(root) = msg["params"]["rootUri"].as_str().and_then(uri_to_path) {
                    let base = msg["params"]["initializationOptions"]["base"]
                        .as_str()
                        .unwrap_or("HEAD")
                        .to_owned();
                    *self = Server::new(root, base);
                }
                let caps = json!({"capabilities": {"textDocumentSync": {"openClose": true, "change": 1, "save": true},
                                                   "codeLensProvider": {"resolveProvider": false}},
                                  "serverInfo": {"name": "emi", "version": env!("CARGO_PKG_VERSION")}});
                (vec![reply(caps)], false)
            }
            "textDocument/didOpen" => {
                self.documents.insert(
                    uri.clone(),
                    msg["params"]["textDocument"]["text"]
                        .as_str()
                        .unwrap_or("")
                        .to_owned(),
                );
                (vec![self.publish(&uri)], false)
            }
            "textDocument/didChange" => {
                if let Some(text) = msg["params"]["contentChanges"]
                    .as_array()
                    .and_then(|c| c.last())
                    .and_then(|c| c["text"].as_str())
                {
                    self.documents.insert(uri.clone(), text.to_owned());
                }
                (vec![self.publish(&uri)], false)
            }
            "textDocument/didSave" => (vec![self.publish(&uri)], false),
            "textDocument/didClose" => {
                self.documents.remove(&uri);
                (
                    vec![
                        json!({"jsonrpc": "2.0", "method": "textDocument/publishDiagnostics", "params": {"uri": uri, "diagnostics": []}}),
                    ],
                    false,
                )
            }
            "textDocument/codeLens" => {
                let rel = self.rel(&uri);
                let text = self.documents.get(&uri).cloned().unwrap_or_default();
                let base = self.base_text(&rel);
                (
                    vec![reply(Value::Array(analysis::lenses(
                        &rel,
                        &text,
                        base.as_deref(),
                    )))],
                    false,
                )
            }
            "shutdown" => (vec![reply(Value::Null)], false),
            "exit" => (Vec::new(), true),
            _ if id.is_some() && !method.is_empty() => (
                vec![
                    json!({"jsonrpc": "2.0", "id": id, "error": {"code": -32601, "message": format!("method not found: {method}")}}),
                ],
                false,
            ),
            _ => (Vec::new(), false),
        }
    }
}

/// Serve until `exit` or end of input.
pub fn serve(mut input: impl BufRead, mut output: impl Write) -> std::io::Result<()> {
    let mut server = Server::new(std::env::current_dir().unwrap_or_default(), "HEAD".into());
    while let Some(msg) = read_message(&mut input)? {
        let (replies, exit) = server.handle(&msg);
        for r in replies {
            write_message(&mut output, &r)?;
        }
        if exit {
            break;
        }
    }
    Ok(())
}
