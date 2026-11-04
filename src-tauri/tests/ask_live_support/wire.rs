//! A logging pass-through between the app's Ollama provider and Ollama, for
//! the Ask bench (`tests/ask_live.rs`).
//!
//! The provider only reports token counts, so this sits on the wire and
//! keeps what Ollama's final object says about timing (`prompt_eval_*`,
//! `eval_*`, `load_duration`), the first answer byte's time, and what the
//! provider actually sent (`think`, `format`, `tools`, `options.num_ctx`).
//! Nothing is changed in either direction. One request per connection
//! (`Connection: close`), which is how a bench with sequential Asks uses it.

use std::sync::{Arc, Mutex};
use std::time::Instant;

use futures_util::StreamExt;
use serde::Serialize;
use serde_json::{json, Value};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};

/// One `/api/chat` exchange.
#[derive(Debug, Clone, Serialize)]
pub struct Wire {
    /// What the provider sent (a summary, never the prompt text).
    pub sent: Value,
    pub status: u16,
    /// Request written → response headers.
    pub ttfb_ms: u64,
    /// Request written → first answer text or tool call.
    pub first_content_ms: Option<u64>,
    pub total_ms: u64,
    pub content_chars: usize,
    pub thinking_chars: usize,
    pub tool_calls: usize,
    /// The final object's counters (`prompt_eval_count`, `eval_count`,
    /// durations in ns, `done_reason`); null when there was none.
    pub done: Value,
    /// An error body's first 200 characters (non-2xx only).
    pub error: Option<String>,
}

pub struct Proxy {
    addr: std::net::SocketAddr,
    wires: Arc<Mutex<Vec<Wire>>>,
}

impl Proxy {
    /// Listen on a free loopback port, forwarding to `upstream`
    /// (`http://127.0.0.1:11434`).
    pub async fn start(upstream: &str) -> Proxy {
        let listener = TcpListener::bind("127.0.0.1:0").await.expect("bind");
        let addr = listener.local_addr().expect("addr");
        let wires = Arc::new(Mutex::new(Vec::new()));
        let upstream = upstream.trim_end_matches('/').to_string();
        let client = reqwest::Client::new();
        let log = wires.clone();
        tokio::spawn(async move {
            loop {
                let Ok((sock, _)) = listener.accept().await else {
                    return;
                };
                let (client, log, upstream) = (client.clone(), log.clone(), upstream.clone());
                tokio::spawn(async move {
                    let _ = serve(sock, &client, &upstream, &log).await;
                });
            }
        });
        Proxy { addr, wires }
    }

    /// The base URL for `llm.ollama.base_url`.
    pub fn url(&self) -> String {
        format!("http://{}", self.addr)
    }

    /// The exchanges since the last call.
    pub fn take(&self) -> Vec<Wire> {
        std::mem::take(&mut *self.wires.lock().expect("lock"))
    }
}

fn find(hay: &[u8], needle: &[u8]) -> Option<usize> {
    hay.windows(needle.len()).position(|w| w == needle)
}

/// Method, path and body of one request (a `Content-Length` body).
async fn read_request(s: &mut TcpStream) -> Option<(String, String, Vec<u8>)> {
    let mut buf = Vec::new();
    let mut tmp = [0u8; 16 * 1024];
    let end = loop {
        if let Some(p) = find(&buf, b"\r\n\r\n") {
            break p;
        }
        let n = s.read(&mut tmp).await.ok()?;
        if n == 0 {
            return None;
        }
        buf.extend_from_slice(&tmp[..n]);
    };
    let head = String::from_utf8_lossy(&buf[..end]).to_string();
    let mut lines = head.split("\r\n");
    let mut first = lines.next()?.split(' ');
    let (method, path) = (first.next()?.to_string(), first.next()?.to_string());
    let len = lines
        .filter_map(|l| {
            l.to_ascii_lowercase()
                .strip_prefix("content-length:")
                .and_then(|v| v.trim().parse::<usize>().ok())
        })
        .next()
        .unwrap_or(0);
    let mut body = buf[end + 4..].to_vec();
    while body.len() < len {
        let n = s.read(&mut tmp).await.ok()?;
        if n == 0 {
            return None;
        }
        body.extend_from_slice(&tmp[..n]);
    }
    Some((method, path, body))
}

fn summarize_request(body: &[u8]) -> Value {
    let v: Value = serde_json::from_slice(body).unwrap_or(Value::Null);
    let opts = &v["options"];
    let chars: usize = v["messages"]
        .as_array()
        .map(|m| {
            m.iter()
                .map(|x| x["content"].as_str().map_or(0, str::len))
                .sum()
        })
        .unwrap_or(0);
    json!({
        "model": v["model"],
        "stream": v["stream"].as_bool().unwrap_or(false),
        // Presence matters (ollama#10976: omitted `think` turns thinking on).
        "think_sent": v.get("think").is_some(),
        "think": v.get("think").cloned().unwrap_or(Value::Null),
        "format_sent": v.get("format").is_some_and(|f| !f.is_null()),
        "tools": v["tools"].as_array().map_or(0, Vec::len),
        "num_ctx": opts["num_ctx"],
        "num_predict": opts["num_predict"],
        "temperature": opts["temperature"],
        "keep_alive": v["keep_alive"],
        "messages": v["messages"].as_array().map_or(0, Vec::len),
        "prompt_chars": chars,
    })
}

#[derive(Default)]
struct Scan {
    first_content: Option<u64>,
    content_chars: usize,
    thinking_chars: usize,
    tool_calls: usize,
    done: Value,
}

impl Scan {
    fn line(&mut self, line: &str, t0: Instant) {
        let Ok(v) = serde_json::from_str::<Value>(line) else {
            return;
        };
        let msg = &v["message"];
        let content = msg["content"].as_str().unwrap_or("");
        let calls = msg["tool_calls"].as_array().map_or(0, Vec::len);
        self.content_chars += content.len();
        self.thinking_chars += msg["thinking"].as_str().map_or(0, str::len);
        self.tool_calls += calls;
        if self.first_content.is_none() && (!content.is_empty() || calls > 0) {
            self.first_content = Some(t0.elapsed().as_millis() as u64);
        }
        if v["done"].as_bool() == Some(true) {
            self.done = json!({
                "done_reason": v["done_reason"],
                "prompt_eval_count": v["prompt_eval_count"],
                "prompt_eval_duration_ns": v["prompt_eval_duration"],
                "eval_count": v["eval_count"],
                "eval_duration_ns": v["eval_duration"],
                "load_duration_ns": v["load_duration"],
                "total_duration_ns": v["total_duration"],
            });
        }
    }
}

async fn serve(
    mut sock: TcpStream,
    client: &reqwest::Client,
    upstream: &str,
    log: &Mutex<Vec<Wire>>,
) -> Option<()> {
    let (method, path, body) = read_request(&mut sock).await?;
    let is_chat = path.starts_with("/api/chat");
    let sent = if is_chat {
        summarize_request(&body)
    } else {
        Value::Null
    };
    let t0 = Instant::now();
    let m = reqwest::Method::from_bytes(method.as_bytes()).ok()?;
    let mut rb = client.request(m, format!("{upstream}{path}"));
    if !body.is_empty() {
        rb = rb.header("content-type", "application/json").body(body);
    }
    let resp = match rb.send().await {
        Ok(r) => r,
        Err(_) => {
            let _ = sock
                .write_all(
                    b"HTTP/1.1 502 Bad Gateway\r\nconnection: close\r\ncontent-length: 0\r\n\r\n",
                )
                .await;
            return None;
        }
    };
    let status = resp.status();
    let ttfb_ms = t0.elapsed().as_millis() as u64;
    let ctype = resp
        .headers()
        .get("content-type")
        .and_then(|v| v.to_str().ok())
        .unwrap_or("application/json")
        .to_string();
    let head = format!(
        "HTTP/1.1 {} {}\r\ncontent-type: {ctype}\r\ntransfer-encoding: chunked\r\nconnection: close\r\n\r\n",
        status.as_u16(),
        status.canonical_reason().unwrap_or("")
    );
    sock.write_all(head.as_bytes()).await.ok()?;

    let mut scan = Scan::default();
    let mut pending = String::new();
    let mut raw_error = String::new();
    let mut stream = resp.bytes_stream();
    while let Some(chunk) = stream.next().await {
        let Ok(chunk) = chunk else { break };
        if is_chat {
            if status.is_success() {
                pending.push_str(&String::from_utf8_lossy(&chunk));
                while let Some(i) = pending.find('\n') {
                    let line: String = pending.drain(..=i).collect();
                    scan.line(line.trim(), t0);
                }
            } else if raw_error.len() < 200 {
                raw_error.push_str(&String::from_utf8_lossy(&chunk));
            }
        }
        let framed = [format!("{:x}\r\n", chunk.len()).as_bytes(), &chunk, b"\r\n"].concat();
        if sock.write_all(&framed).await.is_err() {
            break;
        }
    }
    let _ = sock.write_all(b"0\r\n\r\n").await;
    let _ = sock.shutdown().await;
    if is_chat {
        if !pending.trim().is_empty() {
            scan.line(pending.trim(), t0);
        }
        log.lock().expect("lock").push(Wire {
            sent,
            status: status.as_u16(),
            ttfb_ms,
            first_content_ms: scan.first_content,
            total_ms: t0.elapsed().as_millis() as u64,
            content_chars: scan.content_chars,
            thinking_chars: scan.thinking_chars,
            tool_calls: scan.tool_calls,
            done: scan.done,
            error: (!status.is_success()).then(|| raw_error.chars().take(200).collect()),
        });
    }
    Some(())
}
