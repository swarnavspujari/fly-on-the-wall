//! Spec §11 MCP test: spawn the real flyonthewall-mcp binary, speak MCP over its
//! stdio, and assert tool calls return the expected resources.

use std::io::{BufRead, BufReader, Write};
use std::process::{Command, Stdio};

use serde_json::{json, Value};

#[test]
fn stdio_server_answers_initialize_and_tool_calls() {
    // seed a data dir with one searchable note
    let dir = tempfile::tempdir().unwrap();
    let note_id = {
        let storage = looma_storage::Storage::open(dir.path()).unwrap();
        let note = storage.create_note("MCP smoke", None).unwrap();
        storage
            .update_note_scratchpad(&note.id, "the quarterly zebra migration plan")
            .unwrap();
        note.id
    };

    let exe = env!("CARGO_BIN_EXE_flyonthewall-mcp");
    let mut child = Command::new(exe)
        .arg("--data-dir")
        .arg(dir.path())
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .expect("spawn flyonthewall-mcp");
    let mut stdin = child.stdin.take().unwrap();
    let mut stdout = BufReader::new(child.stdout.take().unwrap());

    let mut send = |v: Value| {
        stdin.write_all(v.to_string().as_bytes()).unwrap();
        stdin.write_all(b"\n").unwrap();
        stdin.flush().unwrap();
    };
    let mut recv = || -> Value {
        let mut line = String::new();
        stdout.read_line(&mut line).unwrap();
        serde_json::from_str(&line).unwrap()
    };

    send(
        json!({"jsonrpc":"2.0","id":1,"method":"initialize","params":{"protocolVersion":"2024-11-05","capabilities":{},"clientInfo":{"name":"test","version":"0"}}}),
    );
    let init = recv();
    assert_eq!(init["result"]["serverInfo"]["name"], "looma");

    send(json!({"jsonrpc":"2.0","method":"notifications/initialized"}));

    send(json!({"jsonrpc":"2.0","id":2,"method":"tools/list"}));
    let tools = recv();
    assert!(tools["result"]["tools"].as_array().unwrap().len() >= 6);

    send(
        json!({"jsonrpc":"2.0","id":3,"method":"tools/call","params":{"name":"search_notes","arguments":{"query":"zebra"}}}),
    );
    let hits = recv();
    let text = hits["result"]["content"][0]["text"].as_str().unwrap();
    assert!(text.contains("MCP smoke"), "search text was: {text}");
    assert!(text.contains(&note_id));

    send(
        json!({"jsonrpc":"2.0","id":4,"method":"tools/call","params":{"name":"get_note","arguments":{"note_id":note_id}}}),
    );
    let note = recv();
    assert!(note["result"]["content"][0]["text"]
        .as_str()
        .unwrap()
        .contains("zebra migration plan"));

    drop(stdin); // EOF → clean shutdown
    let status = child.wait().unwrap();
    assert!(status.success());
}
