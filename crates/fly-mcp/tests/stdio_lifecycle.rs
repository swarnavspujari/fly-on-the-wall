//! Lifecycle contract: the server must die with its client. Claude Desktop
//! spawns the binary directly and the Windows installer can't overwrite a
//! running exe, so a server that lingers after stdin closes blocks every
//! app update. Spawns the REAL binary, speaks one initialize round-trip,
//! closes stdin, and requires a prompt clean exit.

use std::io::{BufRead, BufReader, Write};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

#[test]
fn server_exits_promptly_on_stdin_eof() {
    let dir = tempfile::tempdir().unwrap();
    let mut child = Command::new(env!("CARGO_BIN_EXE_flyonthewall-mcp"))
        .args(["--data-dir", dir.path().to_str().unwrap()])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .expect("spawn server binary");

    let mut stdin = child.stdin.take().unwrap();
    let mut stdout = BufReader::new(child.stdout.take().unwrap());

    stdin
        .write_all(
            br#"{"jsonrpc":"2.0","id":1,"method":"initialize","params":{"protocolVersion":"2024-11-05","capabilities":{},"clientInfo":{"name":"t","version":"0"}}}"#,
        )
        .unwrap();
    stdin.write_all(b"\n").unwrap();
    stdin.flush().unwrap();

    let mut line = String::new();
    stdout.read_line(&mut line).unwrap();
    assert!(
        line.contains("\"serverInfo\""),
        "bad initialize reply: {line}"
    );

    // client goes away
    drop(stdin);

    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        match child.try_wait().unwrap() {
            Some(status) => {
                assert!(status.success(), "server exited non-zero: {status}");
                break;
            }
            None if Instant::now() > deadline => {
                let _ = child.kill();
                panic!("server still running 5s after stdin EOF — it would block installs");
            }
            None => std::thread::sleep(Duration::from_millis(50)),
        }
    }
}
