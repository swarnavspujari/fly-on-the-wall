//! looma-mcp: stdio MCP server exposing Looma notes, folders, and
//! transcripts to external MCP clients (Claude Desktop etc.).
//!
//! The full JSON-RPC/MCP implementation lands in M6. This stub exists so the
//! binary target builds from M0 onward; it announces itself and exits with a
//! non-zero status so no client mistakes it for a working server yet.

fn main() {
    eprintln!("looma-mcp: the MCP server ships in milestone M6; this build is a placeholder.");
    std::process::exit(2);
}
