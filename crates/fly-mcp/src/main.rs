//! stdio entrypoint: newline-delimited JSON-RPC over stdin/stdout.
//! Usage: flyonthewall-mcp [--data-dir <path>]   (defaults to the app's data dir)

use std::io::{BufRead, Write};

use fly_mcp::Server;

fn main() -> anyhow::Result<()> {
    let mut data_dir = dirs::data_dir()
        .unwrap_or_else(|| std::path::PathBuf::from("."))
        .join("FlyOnTheWall");
    let mut args = std::env::args().skip(1);
    while let Some(arg) = args.next() {
        if arg == "--data-dir" {
            if let Some(dir) = args.next() {
                data_dir = std::path::PathBuf::from(dir);
            }
        }
    }

    let storage = fly_storage::Storage::open(&data_dir)?;
    let server = Server::new(storage);
    // logs must go to stderr — stdout is the protocol channel
    eprintln!(
        "flyonthewall-mcp v{} serving {}",
        fly_mcp::SERVER_VERSION,
        data_dir.display()
    );

    let stdin = std::io::stdin();
    let stdout = std::io::stdout();
    for line in stdin.lock().lines() {
        let line = line?;
        if line.trim().is_empty() {
            continue;
        }
        if let Some(response) = server.handle_message(&line) {
            let mut out = stdout.lock();
            out.write_all(response.as_bytes())?;
            out.write_all(b"\n")?;
            out.flush()?;
        }
    }
    Ok(())
}
