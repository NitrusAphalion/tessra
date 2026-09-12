//! The MCP server end to end: `tessra init` in a fresh directory, then
//! `tessra mcp` over piped stdio answering `initialize`, `tools/list`, and a
//! `tools/call` of `tessra_status`. Both processes get `TESSRA_DATA_DIR`
//! inside the tempdir, so the store, keys, and workspaces vanish with it.

use std::io::{BufRead, BufReader, Write};
use std::path::Path;
use std::process::{Child, Command, Stdio};
use std::sync::mpsc;
use std::time::{Duration, Instant};

use serde_json::{json, Value};

const TESSRA: &str = env!("CARGO_BIN_EXE_tessra");

/// How long one reply may take. A debug build opens the store, indexes, and
/// signs a session op before the first answer.
const REPLY_TIMEOUT: Duration = Duration::from_secs(60);

/// How long the server may take to exit once its stdin closes.
const EXIT_TIMEOUT: Duration = Duration::from_secs(10);

const VERBS: [&str; 13] = [
    "status",
    "context",
    "query",
    "workspace",
    "edit",
    "snapshot",
    "claim",
    "remember",
    "verify",
    "try",
    "promote",
    "revert",
    "undo",
];

#[test]
fn mcp_lists_the_thirteen_tools_and_answers_status() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().join("repo");
    let data = dir.path().join("data");
    std::fs::create_dir_all(&root).unwrap();

    let out = Command::new(TESSRA)
        .args(["init", "--no-git"])
        .current_dir(&root)
        .env("TESSRA_DATA_DIR", &data)
        .output()
        .expect("run tessra init");
    let init: Value = serde_json::from_slice(&out.stdout).unwrap_or_else(|e| {
        panic!(
            "init printed no JSON ({e}): stdout {:?}, stderr {:?}",
            String::from_utf8_lossy(&out.stdout),
            String::from_utf8_lossy(&out.stderr)
        )
    });
    assert_eq!(init["ok"], json!(true), "{init}");
    for key in ["store", "keys"] {
        let path = Path::new(init["result"][key].as_str().expect(key));
        assert!(
            path.starts_with(&data),
            "{key} {} is not under TESSRA_DATA_DIR {}",
            path.display(),
            data.display()
        );
    }

    let mut child = Command::new(TESSRA)
        .args(["--no-daemon", "--agent", "bot", "mcp"])
        .current_dir(&root)
        .env("TESSRA_DATA_DIR", &data)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::inherit())
        .spawn()
        .expect("spawn tessra mcp");
    let mut stdin = child.stdin.take().expect("child stdin");
    let stdout = child.stdout.take().expect("child stdout");
    // Replies arrive through a thread, so a server that never answers fails
    // the test instead of hanging it.
    let (tx, rx) = mpsc::channel::<String>();
    std::thread::spawn(move || {
        for line in BufReader::new(stdout).lines() {
            let Ok(line) = line else { break };
            if tx.send(line).is_err() {
                break;
            }
        }
    });
    let mut call = |id: u64, method: &str, params: Value| -> Value {
        let request = json!({ "jsonrpc": "2.0", "id": id, "method": method, "params": params });
        writeln!(stdin, "{request}").expect("write request");
        stdin.flush().expect("flush request");
        let line = rx
            .recv_timeout(REPLY_TIMEOUT)
            .unwrap_or_else(|_| panic!("no reply to {method} within {REPLY_TIMEOUT:?}"));
        let reply: Value =
            serde_json::from_str(&line).unwrap_or_else(|e| panic!("{method}: {e}: {line}"));
        assert_eq!(reply["jsonrpc"], json!("2.0"), "{reply}");
        assert_eq!(reply["id"], json!(id), "{reply}");
        assert!(reply.get("error").is_none(), "{method} failed: {reply}");
        reply["result"].clone()
    };

    let hello = call(
        1,
        "initialize",
        json!({
            "protocolVersion": "2024-11-05",
            "capabilities": {},
            "clientInfo": { "name": "tessra-test", "version": "0" }
        }),
    );
    assert_eq!(hello["protocolVersion"], json!("2024-11-05"), "{hello}");
    assert_eq!(hello["serverInfo"]["name"], json!("tessra"), "{hello}");
    assert!(hello["capabilities"]["tools"].is_object(), "{hello}");

    let list = call(2, "tools/list", json!({}));
    let tools = list["tools"].as_array().expect("tools array");
    let names: Vec<&str> = tools.iter().filter_map(|t| t["name"].as_str()).collect();
    assert_eq!(names.len(), 13, "{names:?}");
    for verb in VERBS {
        let name = format!("tessra_{verb}");
        assert!(
            names.contains(&name.as_str()),
            "{name} missing from {names:?}"
        );
    }
    for tool in tools {
        assert!(
            tool["description"].as_str().is_some_and(|d| !d.is_empty()),
            "{tool}"
        );
        assert_eq!(tool["inputSchema"]["type"], json!("object"), "{tool}");
    }

    let status = call(
        3,
        "tools/call",
        json!({ "name": "tessra_status", "arguments": {} }),
    );
    assert_eq!(status["isError"], json!(false), "{status}");
    let content = &status["content"][0];
    assert_eq!(content["type"], json!("text"), "{status}");
    let text = content["text"].as_str().expect("text content");
    let body: Value = serde_json::from_str(text)
        .unwrap_or_else(|e| panic!("status text is not JSON: {e}: {text}"));
    assert_eq!(body["ok"], json!(true), "{body}");
    assert!(body["state"].is_object(), "no state object: {body}");

    // EOF on stdin ends the server's loop.
    drop(stdin);
    assert_eq!(
        wait_for_exit(&mut child, EXIT_TIMEOUT),
        Some(0),
        "tessra mcp did not exit cleanly"
    );
}

/// The child's exit code, waiting up to `limit` for it; `None` when it had
/// to be killed instead.
fn wait_for_exit(child: &mut Child, limit: Duration) -> Option<i32> {
    let start = Instant::now();
    loop {
        if let Some(status) = child.try_wait().expect("try_wait") {
            return status.code();
        }
        if start.elapsed() > limit {
            let _ = child.kill();
            let _ = child.wait();
            return None;
        }
        std::thread::sleep(Duration::from_millis(50));
    }
}
