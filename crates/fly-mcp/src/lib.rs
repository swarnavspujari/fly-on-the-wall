//! fly-mcp: a stdio MCP server over the Looma data dir. External MCP
//! clients (Claude Desktop, etc.) can search and read notes, folders,
//! meetings, and transcripts — read-only, fully local.
//!
//! Transport: newline-delimited JSON-RPC 2.0 (the MCP stdio transport).

use fly_storage::Storage;
use serde_json::{json, Value};

pub const SERVER_NAME: &str = "looma";
pub const SERVER_VERSION: &str = env!("CARGO_PKG_VERSION");
const DEFAULT_PROTOCOL_VERSION: &str = "2024-11-05";

pub struct Server {
    storage: Storage,
}

impl Server {
    pub fn new(storage: Storage) -> Self {
        Self { storage }
    }

    /// Handle one JSON-RPC message; `None` = notification (no response).
    pub fn handle_message(&self, raw: &str) -> Option<String> {
        let msg: Value = match serde_json::from_str(raw) {
            Ok(v) => v,
            Err(_) => {
                return Some(error_response(Value::Null, -32700, "parse error"));
            }
        };
        let id = msg.get("id").cloned();
        let method = msg.get("method").and_then(|m| m.as_str()).unwrap_or("");
        let params = msg.get("params").cloned().unwrap_or(Value::Null);

        // notifications get no response
        let id = id?;

        let result = match method {
            "initialize" => Ok(self.initialize(&params)),
            "ping" => Ok(json!({})),
            "tools/list" => Ok(tools_list()),
            "tools/call" => self.tools_call(&params),
            _ => Err((-32601, format!("method not found: {method}"))),
        };

        Some(match result {
            Ok(result) => json!({"jsonrpc": "2.0", "id": id, "result": result}).to_string(),
            Err((code, message)) => error_response(id, code, &message),
        })
    }

    fn initialize(&self, params: &Value) -> Value {
        // echo the client's protocol version when given (we speak the basics)
        let version = params
            .get("protocolVersion")
            .and_then(|v| v.as_str())
            .unwrap_or(DEFAULT_PROTOCOL_VERSION);
        json!({
            "protocolVersion": version,
            "capabilities": { "tools": {} },
            "serverInfo": { "name": SERVER_NAME, "version": SERVER_VERSION },
            "instructions": "Looma's local meeting notes: search_notes finds notes and transcript passages; get_note / get_transcript / get_meeting read them; list_folders and list_recent browse."
        })
    }

    fn tools_call(&self, params: &Value) -> Result<Value, (i32, String)> {
        let name = params.get("name").and_then(|n| n.as_str()).unwrap_or("");
        let args = params.get("arguments").cloned().unwrap_or(json!({}));
        let text = match name {
            "search_notes" => self.tool_search(&args),
            "list_folders" => self.tool_list_folders(),
            "get_note" => self.tool_get_note(&args),
            "get_transcript" => self.tool_get_transcript(&args),
            "get_meeting" => self.tool_get_meeting(&args),
            "list_recent" => self.tool_list_recent(&args),
            other => Err(format!("unknown tool: {other}")),
        };
        match text {
            Ok(text) => Ok(json!({"content": [{"type": "text", "text": text}]})),
            Err(message) => Ok(json!({
                "content": [{"type": "text", "text": message}],
                "isError": true
            })),
        }
    }

    fn tool_search(&self, args: &Value) -> Result<String, String> {
        let query = args
            .get("query")
            .and_then(|q| q.as_str())
            .ok_or("missing required argument: query")?;
        let hits = self.storage.search(query, 20).map_err(|e| e.to_string())?;
        if hits.is_empty() {
            return Ok(format!("No matches for \"{query}\"."));
        }
        let mut out = format!("{} matches for \"{query}\":\n", hits.len());
        for h in hits {
            let kind = match h.kind {
                fly_storage::SearchHitKind::Note => "note",
                fly_storage::SearchHitKind::Transcript => "transcript",
            };
            let snippet = h.snippet.replace("[[", "**").replace("]]", "**");
            out.push_str(&format!(
                "- [{kind}] {} (note_id: {}) — {snippet}\n",
                h.title, h.note_id
            ));
        }
        Ok(out)
    }

    fn tool_list_folders(&self) -> Result<String, String> {
        let folders = self.storage.list_folders().map_err(|e| e.to_string())?;
        if folders.is_empty() {
            return Ok("No folders yet.".into());
        }
        let mut out = String::from("Folders:\n");
        for f in folders {
            out.push_str(&format!(
                "- {} (id: {}{})\n",
                f.name,
                f.id,
                f.parent_id
                    .as_deref()
                    .map(|p| format!(", parent: {p}"))
                    .unwrap_or_default()
            ));
        }
        Ok(out)
    }

    fn tool_get_note(&self, args: &Value) -> Result<String, String> {
        let id = args
            .get("note_id")
            .and_then(|q| q.as_str())
            .ok_or("missing required argument: note_id")?;
        let note = self.storage.get_note(id).map_err(|e| e.to_string())?;
        let mut out = note.to_markdown(false);
        out.push_str(&format!(
            "\n---\nnote_id: {}\nupdated: {}\n",
            note.id,
            note.updated_at.to_rfc3339()
        ));
        if let Some(mid) = &note.meeting_id {
            out.push_str(&format!("meeting_id: {mid}\n"));
        }
        if !note.attachments.is_empty() {
            out.push_str("attachments:\n");
            for a in &note.attachments {
                out.push_str(&format!("- {} ({})\n", a.file_name, a.rel_path));
            }
        }
        Ok(out)
    }

    fn tool_get_transcript(&self, args: &Value) -> Result<String, String> {
        let id = args
            .get("meeting_id")
            .and_then(|q| q.as_str())
            .ok_or("missing required argument: meeting_id")?;
        match self.storage.get_transcript(id).map_err(|e| e.to_string())? {
            Some(t) => Ok(t.to_markdown()),
            None => Err(format!("no transcript for meeting {id}")),
        }
    }

    fn tool_get_meeting(&self, args: &Value) -> Result<String, String> {
        let id = args
            .get("meeting_id")
            .and_then(|q| q.as_str())
            .ok_or("missing required argument: meeting_id")?;
        let m = self.storage.get_meeting(id).map_err(|e| e.to_string())?;
        Ok(format!(
            "Meeting: {}\nmeeting_id: {}\nnote_id: {}\nstarted: {}\nended: {}\nattendees: {}\nrecording: {}\ntranscript: {}\n",
            m.title,
            m.id,
            m.note_id,
            m.started_at.to_rfc3339(),
            m.ended_at.map(|e| e.to_rfc3339()).unwrap_or_else(|| "(in progress)".into()),
            if m.attendees.is_empty() { "(none)".into() } else { m.attendees.join(", ") },
            m.recording
                .as_ref()
                .map(|r| format!("{} ms", r.duration_ms))
                .unwrap_or_else(|| "none".into()),
            if self.storage.get_transcript(&m.id).ok().flatten().is_some() { "yes" } else { "no" },
        ))
    }

    fn tool_list_recent(&self, args: &Value) -> Result<String, String> {
        let limit = args
            .get("limit")
            .and_then(|l| l.as_u64())
            .unwrap_or(10)
            .clamp(1, 100) as usize;
        let notes = self
            .storage
            .list_recent_notes(limit)
            .map_err(|e| e.to_string())?;
        if notes.is_empty() {
            return Ok("No notes yet.".into());
        }
        let mut out = String::from("Recent notes:\n");
        for n in notes {
            out.push_str(&format!(
                "- {} (note_id: {}, updated {}{})\n",
                n.title,
                n.id,
                n.updated_at.to_rfc3339(),
                n.meeting_id
                    .as_deref()
                    .map(|m| format!(", meeting_id: {m}"))
                    .unwrap_or_default()
            ));
        }
        Ok(out)
    }
}

fn error_response(id: Value, code: i32, message: &str) -> String {
    json!({"jsonrpc": "2.0", "id": id, "error": {"code": code, "message": message}}).to_string()
}

fn tools_list() -> Value {
    json!({ "tools": [
        {
            "name": "search_notes",
            "description": "Full-text search across Looma note bodies and meeting transcripts. Returns matching notes with snippets and note_ids.",
            "inputSchema": {
                "type": "object",
                "properties": { "query": { "type": "string", "description": "Search terms" } },
                "required": ["query"]
            }
        },
        {
            "name": "list_folders",
            "description": "List all note folders (with ids and parent relationships).",
            "inputSchema": { "type": "object", "properties": {} }
        },
        {
            "name": "get_note",
            "description": "Read one note as markdown (scratchpad or enhanced document), with metadata and attachments.",
            "inputSchema": {
                "type": "object",
                "properties": { "note_id": { "type": "string" } },
                "required": ["note_id"]
            }
        },
        {
            "name": "get_transcript",
            "description": "Read a meeting's diarized transcript as markdown (timestamps + speaker labels).",
            "inputSchema": {
                "type": "object",
                "properties": { "meeting_id": { "type": "string" } },
                "required": ["meeting_id"]
            }
        },
        {
            "name": "get_meeting",
            "description": "Read a meeting's metadata: title, times, attendees, recording, transcript availability.",
            "inputSchema": {
                "type": "object",
                "properties": { "meeting_id": { "type": "string" } },
                "required": ["meeting_id"]
            }
        },
        {
            "name": "list_recent",
            "description": "List the most recently updated notes (default 10, max 100).",
            "inputSchema": {
                "type": "object",
                "properties": { "limit": { "type": "integer", "minimum": 1, "maximum": 100 } }
            }
        }
    ]})
}

#[cfg(test)]
mod tests {
    use super::*;

    fn server_with_data() -> (tempfile::TempDir, Server, String, String) {
        let dir = tempfile::tempdir().unwrap();
        let storage = Storage::open(dir.path()).unwrap();
        let note = storage.create_note("Budget sync", None).unwrap();
        storage
            .update_note_scratchpad(&note.id, "we approved the roadmap budget")
            .unwrap();
        let meeting = storage
            .create_meeting("Budget sync", &note.id, &[])
            .unwrap();
        storage
            .save_transcript(&fly_core::Transcript {
                meeting_id: meeting.id.clone(),
                language: Some("en".into()),
                engine: "whisper.cpp".into(),
                segments: vec![fly_core::TranscriptSegment {
                    id: "s1".into(),
                    speaker_key: "mic".into(),
                    start_ms: 0,
                    end_ms: 1000,
                    text: "the roadmap is approved".into(),
                    words: vec![],
                }],
                speakers: vec![fly_core::Speaker {
                    key: "mic".into(),
                    label: "You".into(),
                }],
            })
            .unwrap();
        let note_id = note.id.clone();
        let meeting_id = meeting.id;
        (dir, Server::new(storage), note_id, meeting_id)
    }

    fn call(server: &Server, msg: serde_json::Value) -> serde_json::Value {
        let resp = server.handle_message(&msg.to_string()).expect("response");
        serde_json::from_str(&resp).unwrap()
    }

    #[test]
    fn initialize_and_list_tools() {
        let (_d, server, _n, _m) = server_with_data();
        let init = call(
            &server,
            json!({"jsonrpc":"2.0","id":1,"method":"initialize","params":{"protocolVersion":"2025-06-18","capabilities":{},"clientInfo":{"name":"t","version":"0"}}}),
        );
        assert_eq!(init["result"]["serverInfo"]["name"], "looma");
        assert_eq!(init["result"]["protocolVersion"], "2025-06-18");

        // notification → no response
        assert!(server
            .handle_message(r#"{"jsonrpc":"2.0","method":"notifications/initialized"}"#)
            .is_none());

        let tools = call(
            &server,
            json!({"jsonrpc":"2.0","id":2,"method":"tools/list"}),
        );
        let names: Vec<&str> = tools["result"]["tools"]
            .as_array()
            .unwrap()
            .iter()
            .map(|t| t["name"].as_str().unwrap())
            .collect();
        assert_eq!(
            names,
            vec![
                "search_notes",
                "list_folders",
                "get_note",
                "get_transcript",
                "get_meeting",
                "list_recent"
            ]
        );
    }

    #[test]
    fn search_and_read_tools_return_content() {
        let (_d, server, note_id, meeting_id) = server_with_data();

        let hits = call(
            &server,
            json!({"jsonrpc":"2.0","id":3,"method":"tools/call","params":{"name":"search_notes","arguments":{"query":"roadmap"}}}),
        );
        let text = hits["result"]["content"][0]["text"].as_str().unwrap();
        assert!(text.contains("Budget sync"));
        assert!(text.contains(&note_id));

        let note = call(
            &server,
            json!({"jsonrpc":"2.0","id":4,"method":"tools/call","params":{"name":"get_note","arguments":{"note_id":note_id}}}),
        );
        assert!(note["result"]["content"][0]["text"]
            .as_str()
            .unwrap()
            .contains("approved the roadmap budget"));

        let transcript = call(
            &server,
            json!({"jsonrpc":"2.0","id":5,"method":"tools/call","params":{"name":"get_transcript","arguments":{"meeting_id":meeting_id}}}),
        );
        assert!(transcript["result"]["content"][0]["text"]
            .as_str()
            .unwrap()
            .contains("**You:** the roadmap is approved"));
    }

    #[test]
    fn unknown_method_and_bad_tool_are_graceful() {
        let (_d, server, _n, _m) = server_with_data();
        let resp = call(&server, json!({"jsonrpc":"2.0","id":9,"method":"nope"}));
        assert_eq!(resp["error"]["code"], -32601);

        let bad = call(
            &server,
            json!({"jsonrpc":"2.0","id":10,"method":"tools/call","params":{"name":"bogus","arguments":{}}}),
        );
        assert_eq!(bad["result"]["isError"], true);
    }
}
