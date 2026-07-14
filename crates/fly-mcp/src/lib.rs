//! fly-mcp: a stdio MCP server over the Fly on the Wall data dir. External
//! MCP clients (Claude Desktop, etc.) get a CONTEXT layer over the user's
//! meetings — a deterministic briefing (`get_context`), typed extracted
//! items with provenance (`query_items`, `open_items`, `whats_changed`),
//! and the underlying notes/transcripts to verify against. One write tool
//! only: `set_speaker_label`, mirroring what the app UI already allows.
//!
//! Transport: newline-delimited JSON-RPC 2.0 (the MCP stdio transport).

pub mod context;

use chrono::{DateTime, Utc};
use fly_core::ItemKind;
use fly_storage::{ItemFilter, SearchFilter, Storage};
use serde_json::{json, Value};

pub const SERVER_NAME: &str = "flyonthewall";
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
            "instructions": "Fly on the Wall: the user's local meeting notes, transcripts, and a structured context layer extracted from them.\n\nIntended workflow:\n1. START with get_context(query) for any question about a project, customer, or recurring meeting — it returns a deterministic briefing (participants, meeting timeline, decisions, action items, commitments, open questions, key figures) assembled from stored data. Every claim carries its meeting_id and transcript segment ids.\n2. For targeted facts use query_items (filter by type/owner/status/since), open_items (what's still open), or whats_changed (what's new in a series since a date).\n3. Drill into raw material only to VERIFY or quote: get_transcript / get_transcripts for diarized transcripts, get_note for the user's own notes, search_notes for full-text search.\n4. Speakers appear by label ('You', 'Dana', 'Speaker 1'). If the user tells you who a speaker is, persist it with set_speaker_label — the only write this server allows.\n\nProvenance rule: extracted items are machine-generated. When a claim matters, verify it against the cited segment ids in the transcript before presenting it as fact."
        })
    }

    fn tools_call(&self, params: &Value) -> Result<Value, (i32, String)> {
        let name = params.get("name").and_then(|n| n.as_str()).unwrap_or("");
        let args = params.get("arguments").cloned().unwrap_or(json!({}));
        let text = match name {
            "get_context" => self.tool_get_context(&args),
            "whats_changed" => self.tool_whats_changed(&args),
            "open_items" => self.tool_open_items(&args),
            "query_items" => self.tool_query_items(&args),
            "get_meeting_items" => self.tool_get_meeting_items(&args),
            "search_notes" => self.tool_search(&args),
            "list_folders" => self.tool_list_folders(),
            "get_note" => self.tool_get_note(&args),
            "get_transcript" => self.tool_get_transcript(&args),
            "get_transcripts" => self.tool_get_transcripts(&args),
            "get_meeting" => self.tool_get_meeting(&args),
            "list_recent" => self.tool_list_recent(&args),
            "set_speaker_label" => self.tool_set_speaker_label(&args),
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
        let limit = arg_limit(args, 20);
        let offset = args
            .get("cursor")
            .and_then(|c| c.as_str())
            .and_then(|c| c.parse::<usize>().ok())
            .unwrap_or(0);
        let hits = self
            .storage
            .search_filtered(
                query,
                &SearchFilter {
                    folder_id: arg_str(args, "folder_id"),
                    since: arg_time(args, "since")?,
                    until: arg_time(args, "until")?,
                    limit,
                    offset,
                },
            )
            .map_err(|e| e.to_string())?;
        if hits.is_empty() {
            return Ok(format!("No matches for \"{query}\"."));
        }
        let n = hits.len();
        let mut out = format!("{n} match(es) for \"{query}\":\n");
        for h in &hits {
            let kind = match h.kind {
                fly_storage::SearchHitKind::Note => "note",
                fly_storage::SearchHitKind::Transcript => "transcript",
            };
            let snippet = h.snippet.replace("[[", "**").replace("]]", "**");
            out.push_str(&format!(
                "- [{kind}] {} (note_id: {}{}) — {snippet}\n",
                h.title,
                h.note_id,
                h.meeting_id
                    .as_deref()
                    .map(|m| format!(", meeting_id: {m}"))
                    .unwrap_or_default()
            ));
        }
        if n >= limit {
            out.push_str(&format!(
                "\nMore may exist — pass cursor: \"{}\" to continue.\n",
                offset + limit
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

    /// One meeting's transcript rendered for an AI client: header with the
    /// speaker legend (label ← stable key) and attendees, then labeled
    /// `[seg_id] [mm:ss] **Label:** text` lines. Prefers the polished
    /// variant when the app has produced one (same segment ids as raw).
    fn render_transcript(&self, meeting_id: &str, with_ids: bool) -> Result<String, String> {
        let raw = self
            .storage
            .get_transcript(meeting_id)
            .map_err(|e| e.to_string())?
            .ok_or(format!("no transcript for meeting {meeting_id}"))?;
        let (t, variant) = match self
            .storage
            .get_cleaned_transcript(meeting_id)
            .ok()
            .flatten()
        {
            Some(cleaned) => (cleaned, "polished"),
            None => (raw, "raw"),
        };
        let meeting = self.storage.get_meeting(meeting_id).ok();
        let mut out = String::new();
        if let Some(m) = &meeting {
            out.push_str(&format!(
                "# {} — {}\n",
                m.title,
                m.started_at.format("%Y-%m-%d")
            ));
            if !m.attendees.is_empty() {
                out.push_str(&format!("Attendees: {}\n", attendees_text(&m.attendees)));
            }
        }
        out.push_str(&format!(
            "meeting_id: {meeting_id} · {variant} transcript\n"
        ));
        if !t.speakers.is_empty() {
            let legend: Vec<String> = t
                .speakers
                .iter()
                .map(|s| format!("{} (key: {})", s.label, s.key))
                .collect();
            out.push_str(&format!(
                "Speakers: {} — rename generic ones with set_speaker_label\n",
                legend.join(", ")
            ));
        }
        out.push('\n');
        for seg in &t.segments {
            let secs = seg.start_ms / 1000;
            if with_ids {
                out.push_str(&format!("[{}] ", seg.id));
            }
            out.push_str(&format!(
                "[{:02}:{:02}] **{}:** {}\n\n",
                secs / 60,
                secs % 60,
                t.label_for(&seg.speaker_key),
                seg.text.trim()
            ));
        }
        Ok(out)
    }

    fn tool_get_transcript(&self, args: &Value) -> Result<String, String> {
        let id = args
            .get("meeting_id")
            .and_then(|q| q.as_str())
            .ok_or("missing required argument: meeting_id")?;
        let with_ids = args
            .get("with_segment_ids")
            .and_then(|v| v.as_bool())
            .unwrap_or(false);
        self.render_transcript(id, with_ids)
    }

    fn tool_get_transcripts(&self, args: &Value) -> Result<String, String> {
        let ids: Vec<String> = args
            .get("meeting_ids")
            .and_then(|v| v.as_array())
            .ok_or("missing required argument: meeting_ids (array of strings)")?
            .iter()
            .filter_map(|v| v.as_str().map(String::from))
            .collect();
        if ids.is_empty() {
            return Err("meeting_ids is empty".into());
        }
        if ids.len() > 20 {
            return Err("at most 20 meeting_ids per call".into());
        }
        let with_ids = args
            .get("with_segment_ids")
            .and_then(|v| v.as_bool())
            .unwrap_or(false);
        let mut out = String::new();
        for id in &ids {
            match self.render_transcript(id, with_ids) {
                Ok(t) => out.push_str(&t),
                Err(e) => out.push_str(&format!("# meeting {id}\n({e})\n")),
            }
            out.push_str("\n---\n\n");
        }
        Ok(out)
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
            if m.attendees.is_empty() { "(none)".into() } else { attendees_text(&m.attendees) },
            m.recording
                .as_ref()
                .map(|r| format!("{} ms", r.duration_ms))
                .unwrap_or_else(|| "none".into()),
            if self.storage.get_transcript(&m.id).ok().flatten().is_some() { "yes" } else { "no" },
        ))
    }

    fn tool_list_recent(&self, args: &Value) -> Result<String, String> {
        let limit = arg_limit(args, 10);
        let before = args
            .get("cursor")
            .and_then(|c| c.as_str())
            .and_then(|c| c.split_once('|'))
            .map(|(ts, id)| (ts.to_string(), id.to_string()));
        let notes = self
            .storage
            .list_notes_filtered(
                limit,
                arg_str(args, "folder_id").as_deref(),
                arg_time(args, "since")?,
                arg_time(args, "until")?,
                before,
            )
            .map_err(|e| e.to_string())?;
        if notes.is_empty() {
            return Ok("No notes match.".into());
        }
        let mut out = String::from("Recent notes:\n");
        for n in &notes {
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
        if notes.len() >= limit {
            let last = notes.last().unwrap();
            out.push_str(&format!(
                "\nMore may exist — pass cursor: \"{}|{}\" to continue.\n",
                last.updated_at.to_rfc3339(),
                last.id
            ));
        }
        Ok(out)
    }

    // ---- structured items ----

    fn render_items(&self, items: &[fly_core::MeetingItem]) -> String {
        let mut out = String::new();
        for i in items {
            let date = self
                .storage
                .get_meeting(&i.meeting_id)
                .map(|m| m.started_at.format("%Y-%m-%d").to_string())
                .unwrap_or_default();
            let mut meta = vec![format!("kind: {}", i.kind.as_str())];
            if let Some(o) = &i.owner {
                meta.push(format!("owner: {o}"));
            }
            if let Some(s) = &i.status {
                meta.push(format!("status: {s}"));
            }
            meta.push(date);
            meta.push(format!("meeting: {}", i.meeting_id));
            if !i.segment_ids.is_empty() {
                meta.push(format!("segments: {}", i.segment_ids.join(",")));
            }
            out.push_str(&format!("- {} ({})\n", i.text, meta.join(" · ")));
        }
        out
    }

    fn tool_get_meeting_items(&self, args: &Value) -> Result<String, String> {
        let id = args
            .get("meeting_id")
            .and_then(|q| q.as_str())
            .ok_or("missing required argument: meeting_id")?;
        let items = self
            .storage
            .get_meeting_items(id)
            .map_err(|e| e.to_string())?;
        if items.is_empty() {
            return Ok(format!(
                "No extracted items for meeting {id}. Either extraction found nothing or it \
                 hasn't run — the transcript itself is available via get_transcript."
            ));
        }
        Ok(format!(
            "{} item(s) for meeting {id}:\n{}",
            items.len(),
            self.render_items(&items)
        ))
    }

    fn tool_query_items(&self, args: &Value) -> Result<String, String> {
        let kind = match arg_str(args, "type") {
            Some(t) => Some(ItemKind::parse(&t).ok_or(
                "type must be one of: decision, action_item, question, commitment, figure",
            )?),
            None => None,
        };
        let items = self
            .storage
            .query_items(&ItemFilter {
                kind,
                owner: arg_str(args, "owner"),
                status: arg_str(args, "status"),
                since: arg_time(args, "since")?,
                meeting_ids: None,
                limit: arg_limit(args, 50),
            })
            .map_err(|e| e.to_string())?;
        if items.is_empty() {
            return Ok(
                "No items match. Items exist only for meetings the app has extracted — \
                       see get_context or get_transcript for raw material."
                    .into(),
            );
        }
        Ok(format!(
            "{} item(s), newest meeting first:\n{}",
            items.len(),
            self.render_items(&items)
        ))
    }

    // ---- context layer ----

    fn all_series(&self) -> Result<Vec<context::Series>, String> {
        let meetings = self
            .storage
            .list_meetings(None, 2000)
            .map_err(|e| e.to_string())?;
        Ok(context::detect_series(meetings))
    }

    fn tool_get_context(&self, args: &Value) -> Result<String, String> {
        let query = args
            .get("query")
            .and_then(|q| q.as_str())
            .ok_or("missing required argument: query")?;
        let since = arg_time(args, "since")?;
        let series = self.all_series()?;
        let mut matched = context::series_matching(&series, query);
        if matched.is_empty() {
            // fall back to full-text: meetings whose transcript/note matches
            let hits = self.storage.search(query, 20).map_err(|e| e.to_string())?;
            let meeting_ids: std::collections::HashSet<String> =
                hits.iter().filter_map(|h| h.meeting_id.clone()).collect();
            matched = series
                .iter()
                .filter(|s| s.meetings.iter().any(|m| meeting_ids.contains(&m.id)))
                .collect();
        }
        context::build_briefing(&self.storage, &matched, since, query)
    }

    fn tool_whats_changed(&self, args: &Value) -> Result<String, String> {
        let series_key = args
            .get("series")
            .and_then(|q| q.as_str())
            .ok_or("missing required argument: series (a series name from get_context)")?;
        let since = arg_time(args, "since")?
            .ok_or("missing required argument: since (YYYY-MM-DD or RFC 3339)")?;
        let series = self.all_series()?;
        let matched = context::series_matching(&series, series_key);
        if matched.is_empty() {
            return Ok(format!(
                "No series matches \"{series_key}\". get_context(query) lists series names."
            ));
        }
        context::build_briefing(&self.storage, &matched, Some(since), series_key)
    }

    fn tool_open_items(&self, args: &Value) -> Result<String, String> {
        let scope = arg_str(args, "scope").unwrap_or_else(|| "all".into());
        let meeting_ids = if scope == "all" {
            None
        } else {
            let series = self.all_series()?;
            let matched = context::series_matching(&series, &scope);
            if matched.is_empty() {
                return Ok(format!(
                    "No series matches \"{scope}\" — pass \"all\" or a series name from get_context."
                ));
            }
            Some(
                matched
                    .iter()
                    .flat_map(|s| s.meetings.iter().map(|m| m.id.clone()))
                    .collect::<Vec<_>>(),
            )
        };
        // open = action items / commitments not marked done in their meeting
        let mut items = self
            .storage
            .query_items(&ItemFilter {
                meeting_ids,
                limit: 200,
                ..Default::default()
            })
            .map_err(|e| e.to_string())?;
        items.retain(|i| {
            matches!(i.kind, ItemKind::ActionItem | ItemKind::Commitment)
                && i.status.as_deref() != Some("done")
        });
        if items.is_empty() {
            return Ok(format!(
                "No open action items or commitments in scope \"{scope}\"."
            ));
        }
        Ok(format!(
            "{} open item(s) in scope \"{scope}\" (newest meeting first; status is as stated \
             in that meeting — verify against later meetings before chasing anyone):\n{}",
            items.len(),
            self.render_items(&items)
        ))
    }

    // ---- the one write ----

    fn tool_set_speaker_label(&self, args: &Value) -> Result<String, String> {
        let meeting_id = args
            .get("meeting_id")
            .and_then(|q| q.as_str())
            .ok_or("missing required argument: meeting_id")?;
        let speaker_key = args
            .get("speaker_key")
            .and_then(|q| q.as_str())
            .ok_or("missing required argument: speaker_key")?;
        let label = args
            .get("label")
            .and_then(|q| q.as_str())
            .map(str::trim)
            .filter(|l| !l.is_empty())
            .ok_or("missing required argument: label")?;
        let t = self
            .storage
            .relabel_speaker(meeting_id, speaker_key, label)
            .map_err(|e| e.to_string())?;
        Ok(format!(
            "Speaker {speaker_key} in meeting {meeting_id} is now \"{label}\". Speakers: {}",
            t.speakers
                .iter()
                .map(|s| format!("{} (key: {})", s.label, s.key))
                .collect::<Vec<_>>()
                .join(", ")
        ))
    }
}

/// Optional string argument, trimmed; empty = absent.
fn arg_str(args: &Value, key: &str) -> Option<String> {
    args.get(key)
        .and_then(|v| v.as_str())
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(String::from)
}

/// Render attendees for text output: "Name <email>" when a rename kept the
/// original calendar address, else just the display name.
fn attendees_text(attendees: &[fly_core::Attendee]) -> String {
    attendees
        .iter()
        .map(|a| match a.email.as_deref() {
            Some(e) if e != a.display_name() => format!("{} <{e}>", a.display_name()),
            _ => a.display_name().to_string(),
        })
        .collect::<Vec<_>>()
        .join(", ")
}

fn arg_limit(args: &Value, default: usize) -> usize {
    args.get("limit")
        .and_then(|l| l.as_u64())
        .unwrap_or(default as u64)
        .clamp(1, 100) as usize
}

/// Optional timestamp argument: RFC 3339 or bare YYYY-MM-DD (midnight UTC).
fn arg_time(args: &Value, key: &str) -> Result<Option<DateTime<Utc>>, String> {
    let Some(raw) = args.get(key).and_then(|v| v.as_str()) else {
        return Ok(None);
    };
    if let Ok(t) = DateTime::parse_from_rfc3339(raw) {
        return Ok(Some(t.with_timezone(&Utc)));
    }
    if let Ok(d) = chrono::NaiveDate::parse_from_str(raw, "%Y-%m-%d") {
        return Ok(Some(DateTime::from_naive_utc_and_offset(
            d.and_hms_opt(0, 0, 0).unwrap(),
            Utc,
        )));
    }
    Err(format!(
        "{key} must be RFC 3339 or YYYY-MM-DD, got \"{raw}\""
    ))
}

fn error_response(id: Value, code: i32, message: &str) -> String {
    json!({"jsonrpc": "2.0", "id": id, "error": {"code": code, "message": message}}).to_string()
}

fn tools_list() -> Value {
    let time_desc = "RFC 3339 or YYYY-MM-DD";
    json!({ "tools": [
        {
            "name": "get_context",
            "description": "START HERE for any question about a project, customer, or recurring meeting. Detects the recurring meeting series matching the query (normalized title + participant overlap, full-text fallback) and returns a deterministic, chronologically threaded briefing: participants, meeting timeline, decisions, action items, commitments, open questions, and key figures — every claim cited with its meeting_id and transcript segment ids. Assembled from stored data only; nothing is generated.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "query": { "type": "string", "description": "Project, customer, meeting series, or topic (e.g. \"acme renewal\")" },
                    "since": { "type": "string", "description": time_desc }
                },
                "required": ["query"]
            }
        },
        {
            "name": "whats_changed",
            "description": "What happened in one meeting series since a date: the same threaded briefing as get_context, restricted to meetings of that series started at/after `since`. Use after get_context has named the series.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "series": { "type": "string", "description": "Series name as reported by get_context" },
                    "since": { "type": "string", "description": time_desc }
                },
                "required": ["series", "since"]
            }
        },
        {
            "name": "open_items",
            "description": "Open action items and commitments (anything not stated as done in its meeting), newest first, with owner and provenance. Scope is \"all\" (default) or a series name from get_context. Status reflects what was said IN each meeting — cross-check recent meetings before treating an old item as still open.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "scope": { "type": "string", "description": "\"all\" or a series name" }
                }
            }
        },
        {
            "name": "query_items",
            "description": "Query the structured items extracted from every transcribed meeting: decisions, action items, open questions, commitments, key figures. Each carries meeting_id + source segment ids + who said it. Filter by type, owner, status (\"open\"/\"done\", action items), and since-date. Prefer get_context for narrative questions; use this for targeted lookups (\"everything Dana owns\", \"all figures since June\").",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "type": { "type": "string", "enum": ["decision", "action_item", "question", "commitment", "figure"] },
                    "owner": { "type": "string", "description": "Substring match on the owner" },
                    "status": { "type": "string", "enum": ["open", "done"] },
                    "since": { "type": "string", "description": time_desc },
                    "limit": { "type": "integer", "minimum": 1, "maximum": 100 }
                }
            }
        },
        {
            "name": "get_meeting_items",
            "description": "All structured items extracted from ONE meeting, with provenance. Empty when extraction hasn't run for it — the transcript is still available via get_transcript.",
            "inputSchema": {
                "type": "object",
                "properties": { "meeting_id": { "type": "string" } },
                "required": ["meeting_id"]
            }
        },
        {
            "name": "search_notes",
            "description": "Full-text search across note bodies and meeting transcripts, with snippets and ids. Supports folder/date filters and cursor pagination. Use to locate material; use get_context to understand it.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "query": { "type": "string", "description": "Search terms" },
                    "folder_id": { "type": "string" },
                    "since": { "type": "string", "description": time_desc },
                    "until": { "type": "string", "description": time_desc },
                    "limit": { "type": "integer", "minimum": 1, "maximum": 100 },
                    "cursor": { "type": "string", "description": "From a previous page's footer" }
                },
                "required": ["query"]
            }
        },
        {
            "name": "list_folders",
            "description": "List all note folders (with ids and parent relationships), for search_notes/list_recent folder filters.",
            "inputSchema": { "type": "object", "properties": {} }
        },
        {
            "name": "get_note",
            "description": "Read one note as markdown (the user's own words + enhanced document), with metadata and attachments. The note is the user's take; the transcript is what was actually said.",
            "inputSchema": {
                "type": "object",
                "properties": { "note_id": { "type": "string" } },
                "required": ["note_id"]
            }
        },
        {
            "name": "get_transcript",
            "description": "One meeting's diarized transcript with speaker legend and attendees (polished variant when available). Use to VERIFY claims from get_context/query_items — pass with_segment_ids: true to see the segment ids those tools cite.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "meeting_id": { "type": "string" },
                    "with_segment_ids": { "type": "boolean", "description": "Prefix each line with its segment id (provenance verification)" }
                },
                "required": ["meeting_id"]
            }
        },
        {
            "name": "get_transcripts",
            "description": "Several meetings' transcripts in one call (max 20) — for reading a whole series after get_context. Same rendering as get_transcript.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "meeting_ids": { "type": "array", "items": { "type": "string" }, "maxItems": 20 },
                    "with_segment_ids": { "type": "boolean" }
                },
                "required": ["meeting_ids"]
            }
        },
        {
            "name": "get_meeting",
            "description": "One meeting's metadata: title, times, attendees, recording, transcript availability.",
            "inputSchema": {
                "type": "object",
                "properties": { "meeting_id": { "type": "string" } },
                "required": ["meeting_id"]
            }
        },
        {
            "name": "list_recent",
            "description": "Recently updated notes (each may carry a meeting_id), newest first, with folder/date filters and cursor pagination.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "limit": { "type": "integer", "minimum": 1, "maximum": 100 },
                    "folder_id": { "type": "string" },
                    "since": { "type": "string", "description": time_desc },
                    "until": { "type": "string", "description": time_desc },
                    "cursor": { "type": "string", "description": "From a previous page's footer" }
                }
            }
        },
        {
            "name": "set_speaker_label",
            "description": "THE ONLY WRITE TOOL. Rename a speaker's display label in one meeting's transcript (e.g. \"Speaker 1\" → \"Dana\"), exactly like the app's own relabel UI. Stable speaker keys never change. Use when the user identifies who a speaker is.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "meeting_id": { "type": "string" },
                    "speaker_key": { "type": "string", "description": "Stable key from the transcript's speaker legend (mic, spk_0, …)" },
                    "label": { "type": "string", "description": "Display name to show" }
                },
                "required": ["meeting_id", "speaker_key", "label"]
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
        assert_eq!(init["result"]["serverInfo"]["name"], "flyonthewall");
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
                "get_context",
                "whats_changed",
                "open_items",
                "query_items",
                "get_meeting_items",
                "search_notes",
                "list_folders",
                "get_note",
                "get_transcript",
                "get_transcripts",
                "get_meeting",
                "list_recent",
                "set_speaker_label"
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

    /// A two-meeting "acme renewal" series with transcripts and extracted
    /// items — the fixture for the context-layer tools.
    fn server_with_series() -> (tempfile::TempDir, Server, String, String) {
        let dir = tempfile::tempdir().unwrap();
        let storage = Storage::open(dir.path()).unwrap();
        let mut meeting_ids = Vec::new();
        for (i, title) in ["Acme renewal 2026-06-01", "Acme renewal 2026-06-15"]
            .iter()
            .enumerate()
        {
            let note = storage.create_note(title, None).unwrap();
            let meeting = storage
                .create_meeting(
                    title,
                    &note.id,
                    &[fly_core::Attendee::from_legacy("dana@acme.com")],
                )
                .unwrap();
            storage
                .save_transcript(&fly_core::Transcript {
                    meeting_id: meeting.id.clone(),
                    language: Some("en".into()),
                    engine: "whisper.cpp".into(),
                    segments: vec![fly_core::TranscriptSegment {
                        id: format!("s{i}"),
                        speaker_key: "spk_0".into(),
                        start_ms: 0,
                        end_ms: 1000,
                        text: "renewal discussion".into(),
                        words: vec![],
                    }],
                    speakers: vec![fly_core::Speaker {
                        key: "spk_0".into(),
                        label: "Speaker 1".into(),
                    }],
                })
                .unwrap();
            meeting_ids.push(meeting.id);
        }
        let item = |meeting_id: &str, kind: ItemKind, text: &str, status: Option<&str>| {
            fly_core::MeetingItem {
                id: fly_core::new_id(),
                meeting_id: meeting_id.into(),
                kind,
                text: text.into(),
                owner: Some("Dana".into()),
                status: status.map(String::from),
                speaker_key: Some("spk_0".into()),
                segment_ids: vec!["s0".into()],
                created_at: chrono::Utc::now(),
                extracted_by: "mock".into(),
            }
        };
        storage
            .replace_meeting_items(
                &meeting_ids[0],
                &[
                    item(
                        &meeting_ids[0],
                        ItemKind::Decision,
                        "renew for 12 months",
                        None,
                    ),
                    item(
                        &meeting_ids[0],
                        ItemKind::ActionItem,
                        "send the revised SOW",
                        Some("open"),
                    ),
                ],
            )
            .unwrap();
        storage
            .replace_meeting_items(
                &meeting_ids[1],
                &[
                    item(
                        &meeting_ids[1],
                        ItemKind::ActionItem,
                        "legal review of data residency",
                        Some("done"),
                    ),
                    item(&meeting_ids[1], ItemKind::Figure, "ACV is $120k", None),
                ],
            )
            .unwrap();
        let (m0, m1) = (meeting_ids.remove(0), meeting_ids.remove(0));
        (dir, Server::new(storage), m0, m1)
    }

    fn tool_text(server: &Server, name: &str, args: serde_json::Value) -> String {
        let resp = call(
            server,
            json!({"jsonrpc":"2.0","id":42,"method":"tools/call","params":{"name":name,"arguments":args}}),
        );
        assert_ne!(
            resp["result"]["isError"], true,
            "tool {name} errored: {resp}"
        );
        resp["result"]["content"][0]["text"]
            .as_str()
            .unwrap()
            .to_string()
    }

    #[test]
    fn get_context_builds_threaded_briefing_with_provenance() {
        let (_d, server, m0, m1) = server_with_series();
        let text = tool_text(&server, "get_context", json!({"query": "acme renewal"}));
        assert!(text.contains("# Context: acme renewal"), "{text}");
        assert!(text.contains("2 meeting(s)"), "{text}");
        assert!(text.contains("dana@acme.com"), "{text}");
        assert!(text.contains("## Timeline"), "{text}");
        assert!(text.contains("## Decisions"), "{text}");
        assert!(text.contains("renew for 12 months"), "{text}");
        // provenance: meeting id + segment ids on the claim lines
        assert!(text.contains(&format!("meeting: {m0}")), "{text}");
        assert!(text.contains(&format!("meeting: {m1}")), "{text}");
        assert!(text.contains("segments: s0"), "{text}");
    }

    #[test]
    fn get_context_falls_back_to_full_text_when_title_does_not_match() {
        let (_d, server, _m0, _m1) = server_with_series();
        // "residency" is not in any title, but IS in an extracted... no — it
        // is in no transcript either; use the transcript text instead.
        let text = tool_text(
            &server,
            "get_context",
            json!({"query": "renewal discussion"}),
        );
        assert!(text.contains("# Context:"), "{text}");
    }

    #[test]
    fn open_items_lists_open_and_hides_done() {
        let (_d, server, _m0, _m1) = server_with_series();
        let text = tool_text(&server, "open_items", json!({}));
        assert!(text.contains("send the revised SOW"), "{text}");
        assert!(!text.contains("legal review of data residency"), "{text}");

        let scoped = tool_text(&server, "open_items", json!({"scope": "acme renewal"}));
        assert!(scoped.contains("send the revised SOW"), "{scoped}");
        let miss = tool_text(
            &server,
            "open_items",
            json!({"scope": "nonexistent series"}),
        );
        assert!(miss.contains("No series matches"), "{miss}");
    }

    #[test]
    fn query_items_filters_by_type_owner_status() {
        let (_d, server, _m0, m1) = server_with_series();
        let figures = tool_text(&server, "query_items", json!({"type": "figure"}));
        assert!(figures.contains("ACV is $120k"), "{figures}");
        assert!(!figures.contains("renew for 12 months"), "{figures}");

        let done = tool_text(&server, "query_items", json!({"status": "done"}));
        assert!(done.contains("legal review"), "{done}");
        assert!(done.contains(&format!("meeting: {m1}")), "{done}");

        let bad = call(
            &server,
            json!({"jsonrpc":"2.0","id":7,"method":"tools/call","params":{"name":"query_items","arguments":{"type":"vibes"}}}),
        );
        assert_eq!(bad["result"]["isError"], true);
    }

    #[test]
    fn get_meeting_items_returns_provenance_or_guidance() {
        let (_d, server, m0, _m1) = server_with_series();
        let text = tool_text(&server, "get_meeting_items", json!({"meeting_id": m0}));
        assert!(text.contains("renew for 12 months"), "{text}");
        assert!(text.contains("owner: Dana"), "{text}");
        assert!(text.contains("segments: s0"), "{text}");

        let none = tool_text(&server, "get_meeting_items", json!({"meeting_id": "nope"}));
        assert!(none.contains("No extracted items"), "{none}");
    }

    #[test]
    fn whats_changed_restricts_to_since() {
        let (_d, server, _m0, _m1) = server_with_series();
        // both fixture meetings were created "now", so a past since includes
        // them and a future since excludes them
        let all = tool_text(
            &server,
            "whats_changed",
            json!({"series": "acme renewal", "since": "2020-01-01"}),
        );
        assert!(all.contains("2 meeting(s)"), "{all}");
        let future = tool_text(
            &server,
            "whats_changed",
            json!({"series": "acme renewal", "since": "2099-01-01"}),
        );
        assert!(future.contains("No meetings matched"), "{future}");
    }

    #[test]
    fn transcripts_render_legend_ids_and_batch() {
        let (_d, server, m0, m1) = server_with_series();
        let one = tool_text(
            &server,
            "get_transcript",
            json!({"meeting_id": m0, "with_segment_ids": true}),
        );
        assert!(one.contains("Speakers: Speaker 1 (key: spk_0)"), "{one}");
        assert!(one.contains("[s0] [00:00] **Speaker 1:**"), "{one}");

        let both = tool_text(
            &server,
            "get_transcripts",
            json!({"meeting_ids": [m0, m1, "missing"]}),
        );
        assert!(both.matches("renewal discussion").count() >= 2, "{both}");
        assert!(both.contains("no transcript for meeting missing"), "{both}");
    }

    #[test]
    fn set_speaker_label_is_the_write_and_sticks() {
        let (_d, server, m0, _m1) = server_with_series();
        let ack = tool_text(
            &server,
            "set_speaker_label",
            json!({"meeting_id": m0, "speaker_key": "spk_0", "label": "Dana"}),
        );
        assert!(ack.contains("Dana (key: spk_0)"), "{ack}");
        let transcript = tool_text(&server, "get_transcript", json!({"meeting_id": m0}));
        assert!(
            transcript.contains("**Dana:** renewal discussion"),
            "{transcript}"
        );
    }

    #[test]
    fn list_recent_paginates_with_cursor() {
        let (_d, server, _m0, _m1) = server_with_series();
        let page1 = tool_text(&server, "list_recent", json!({"limit": 1}));
        assert!(page1.contains("cursor:"), "{page1}");
        let cursor = page1
            .split("cursor: \"")
            .nth(1)
            .unwrap()
            .split('"')
            .next()
            .unwrap()
            .to_string();
        let page2 = tool_text(
            &server,
            "list_recent",
            json!({"limit": 1, "cursor": cursor}),
        );
        // two different notes across the pages
        let title_of = |s: &str| s.lines().nth(1).unwrap_or("").to_string();
        assert_ne!(title_of(&page1), title_of(&page2));
    }
}
