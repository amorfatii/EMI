//! A full LSP session over framed stdio bytes.

use std::fmt::Write as _;
use std::io::Cursor;
use std::path::PathBuf;
use std::process::Command;

use serde_json::{Value, json};

struct Dir(PathBuf);

impl Drop for Dir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

fn frame(messages: &[Value]) -> Vec<u8> {
    let mut out = Vec::new();
    for m in messages {
        let body = m.to_string();
        out.extend(format!("Content-Length: {}\r\n\r\n{body}", body.len()).into_bytes());
    }
    out
}

fn unframe(bytes: &[u8]) -> Vec<Value> {
    let mut input = Cursor::new(bytes.to_vec());
    let mut out = Vec::new();
    while let Ok(Some(m)) = emi_lsp::read_message(&mut input) {
        out.push(m);
    }
    out
}

fn sprawl() -> String {
    let mut body = String::from("def sprawl(x):\n    total = 0\n");
    for i in 0..30 {
        let _ = writeln!(body, "    if x == {i}:\n        total += {i}");
    }
    body + "    return total\n"
}

/// A git repository whose `app.py` at HEAD has only `small`.
fn repo() -> Dir {
    let dir = Dir(std::env::temp_dir().join(format!("emi-lsp-{}", std::process::id())));
    let _ = std::fs::remove_dir_all(&dir.0);
    std::fs::create_dir_all(&dir.0).unwrap_or_default();
    let git = |args: &[&str]| {
        Command::new("git")
            .arg("-C")
            .arg(&dir.0)
            .args([
                "-c",
                "user.name=t",
                "-c",
                "user.email=t@t",
                "-c",
                "commit.gpgsign=false",
            ])
            .args(args)
            .output()
    };
    let _ = git(&["init", "-q", "-b", "main"]);
    std::fs::write(dir.0.join("app.py"), "def small(x):\n    return x\n").unwrap_or_default();
    let _ = git(&["add", "-A"]);
    let _ = git(&["commit", "-q", "-m", "base"]);
    dir
}

/// Open an edited `app.py`, ask for lenses, shut down; the replies.
fn session(dir: &Dir) -> Vec<Value> {
    let root = format!("file://{}", dir.0.display());
    let uri = format!("{root}/app.py");
    let text = format!(
        "def small(x=[]):\n    breakpoint()\n    return x\n\n\ndef quiet():\n    breakpoint()  # emi-ignore[py-breakpoint]: demo\n\n\n{}",
        sprawl()
    );
    let input = frame(&[
        json!({"jsonrpc": "2.0", "id": 1, "method": "initialize", "params": {"rootUri": root, "initializationOptions": {"base": "HEAD"}}}),
        json!({"jsonrpc": "2.0", "method": "initialized", "params": {}}),
        json!({"jsonrpc": "2.0", "method": "textDocument/didOpen", "params": {"textDocument": {"uri": uri, "languageId": "python", "version": 1, "text": text}}}),
        json!({"jsonrpc": "2.0", "id": 2, "method": "textDocument/codeLens", "params": {"textDocument": {"uri": uri}}}),
        json!({"jsonrpc": "2.0", "id": 3, "method": "shutdown"}),
        json!({"jsonrpc": "2.0", "method": "exit"}),
    ]);
    let mut output = Vec::new();
    emi_lsp::serve(Cursor::new(input), &mut output).unwrap_or_default();
    unframe(&output)
}

#[test]
fn diagnostics_and_delta_lenses() {
    let dir = repo();
    let messages = session(&dir);
    assert_eq!(
        messages[0]["result"]["capabilities"]["codeLensProvider"]["resolveProvider"],
        false
    );
    let diags = &messages[1]["params"]["diagnostics"];
    let codes: Vec<&str> = diags
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|d| d["code"].as_str())
        .collect();
    assert!(
        codes.contains(&"py-breakpoint") && codes.contains(&"py-mutable-default"),
        "{codes:?}"
    );
    assert_eq!(
        codes.iter().filter(|c| **c == "py-breakpoint").count(),
        1,
        "the suppressed one is not shown"
    );
    assert!(
        codes.contains(&"erosion"),
        "sprawl crosses the complexity threshold: {codes:?}"
    );
    let mutable = diags
        .as_array()
        .into_iter()
        .flatten()
        .find(|d| d["code"] == "py-mutable-default")
        .cloned()
        .unwrap_or_default();
    assert_eq!(
        mutable["range"]["start"],
        json!({"line": 0, "character": 12})
    );
    let lenses: Vec<String> = messages[2]["result"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|l| l["command"]["title"].as_str().map(str::to_owned))
        .collect();
    assert!(
        lenses
            .iter()
            .any(|t| t.contains("(new)") && t.contains("cc 31")),
        "{lenses:?}"
    );
    assert_eq!(messages[3]["id"], 3);
}
