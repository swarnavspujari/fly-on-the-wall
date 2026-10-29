//! fly-mcp: a stdio MCP server over the Fly on the Wall data dir. External
//! MCP clients (Claude Desktop, etc.) get a CONTEXT layer over the user's
//! meetings — a deterministic briefing (`get_context`), typed extracted
//! items with provenance (`query_items`, `open_items`, `whats_changed`),
//! and the underlying notes/transcripts to verify against. One write tool
//! only: `set_speaker_label`, mirroring what the app UI already allows.
//!
//! Transport: newline-delimited JSON-RPC 2.0 (the MCP stdio transport).
//!
//! Dual-era (spec 2026-07-28 "stateless" + legacy handshake): a request
//! carrying `_meta["io.modelcontextprotocol/protocolVersion"]` is served
//! under the modern per-request-metadata protocol (`server/discover`,
//! `resultType`, caching hints, `-32022` version negotiation); an
//! `initialize` request — or any metadata-less request — is served under
//! the legacy handshake protocol, byte-identical to what older clients
//! already parse. The server holds no session state in either era: every
//! request is answered from storage alone.

pub mod context;

use chrono::{DateTime, Utc};
use fly_context::{Context, SearchPassagesArgs, Visibility};
use fly_core::ItemKind;

use fly_storage::{ItemFilter, SearchFilter, Storage, StorageError};
use serde_json::{json, Value};

pub const SERVER_NAME: &str = "flyonthewall";
pub const SERVER_VERSION: &str = env!("CARGO_PKG_VERSION");
/// The modern (stateless) protocol revision this server implements.
pub const MODERN_PROTOCOL_VERSION: &str = "2026-07-28";
/// Echoed to legacy clients that omit a version in `initialize`.
const DEFAULT_PROTOCOL_VERSION: &str = "2024-11-05";

const META_PROTOCOL_VERSION: &str = "io.modelcontextprotocol/protocolVersion";
const META_CLIENT_CAPABILITIES: &str = "io.modelcontextprotocol/clientCapabilities";
const META_SERVER_INFO: &str = "io.modelcontextprotocol/serverInfo";

/// Tool list + discovery results are static for a given binary; let modern
/// clients cache them for an hour (they contain no user data → public).
const CACHE_TTL_MS: u64 = 3_600_000;

const INSTRUCTIONS: &str = "Fly on the Wall: the user's local meeting notes, transcripts, and a structured context layer extracted from them.\n\nIntended workflow:\n1. START with get_context(query) for any question about a project, customer, or recurring meeting — it returns a deterministic briefing (participants, meeting timeline, decisions, action items, commitments, open questions, key figures) assembled from stored data. Every claim carries its meeting_id and transcript segment ids.\n2. For targeted facts use query_items (filter by type/owner/status/since), open_items (what's still open), or whats_changed (what's new in a series since a date).\n3. Drill into raw material only to VERIFY or quote: search_passages for the transcript passages about a topic (each line carries its segment id), get_transcript / get_transcripts for diarized transcripts, get_note for the user's own notes, search_notes for full-text search.\n4. Speakers appear by label ('You', 'Dana', 'Speaker 1'). If the user tells you who a speaker is, persist it with set_speaker_label — the only write this server allows.\n\nProvenance rule: extracted items are machine-generated. When a claim matters, verify it against the cited segment ids in the transcript before presenting it as fact.";

/// JSON-RPC error: code, message, optional `data`.
type RpcError = (i32, String, Option<Value>);

pub struct Server {
    storage: Storage,
}

impl Server {
    pub fn new(storage: Storage) -> Self {
        Self { storage }
    }

    /// Handle one JSON-RPC message; `None` = notification (no response).
    pub fn handle_message(&self, raw: &str) -> Option<String> {
        dispatch(raw, &|params| self.tools_call(params))
    }
}

/// `tools/call` handler plugged into [`dispatch`].
type ToolsCall<'a> = dyn Fn(&Value) -> Result<Value, RpcError> + 'a;

/// JSON-RPC framing and both protocol eras. Only `tools/call` needs the
/// database, so it is the one pluggable step: [`Server`] answers with real
/// tools, [`LazyServer`] with an "open the app" error while the database
/// can't be opened.
fn dispatch(raw: &str, tools_call: &ToolsCall) -> Option<String> {
    let msg: Value = match serde_json::from_str(raw) {
        Ok(v) => v,
        Err(_) => {
            return Some(error_response(Value::Null, -32700, "parse error", None));
        }
    };
    let id = msg.get("id").cloned();
    let method = msg.get("method").and_then(|m| m.as_str()).unwrap_or("");
    let params = msg.get("params").cloned().unwrap_or(Value::Null);

    // notifications get no response
    let id = id?;

    let modern_version = params
        .get("_meta")
        .and_then(|m| m.get(META_PROTOCOL_VERSION))
        .and_then(|v| v.as_str());

    let result = if method == "initialize" {
        // legacy era: answer the handshake exactly as before
        Ok(initialize(&params))
    } else if let Some(version) = modern_version {
        handle_modern(version, method, &params, tools_call)
    } else if method == "server/discover" {
        // modern-only method without its required per-request metadata
        Err((
            -32602,
            format!(
                "server/discover requires _meta[\"{META_PROTOCOL_VERSION}\"] (spec 2026-07-28)"
            ),
            None,
        ))
    } else {
        // legacy era: metadata-less requests, byte-identical to before
        match method {
            "ping" => Ok(json!({})),
            "tools/list" => Ok(tools_list()),
            "tools/call" => tools_call(&params),
            _ => Err((-32601, format!("method not found: {method}"), None)),
        }
    };

    Some(match result {
        Ok(result) => json!({"jsonrpc": "2.0", "id": id, "result": result}).to_string(),
        Err((code, message, data)) => error_response(id, code, &message, data),
    })
}

/// Modern (2026-07-28) dispatch: validate the per-request `_meta`, then
/// answer with modern result envelopes (`resultType`, serverInfo,
/// caching hints on cacheable results).
fn handle_modern(
    version: &str,
    method: &str,
    params: &Value,
    tools_call: &ToolsCall,
) -> Result<Value, RpcError> {
    if version != MODERN_PROTOCOL_VERSION {
        // UnsupportedProtocolVersionError: the client retries with one of ours
        return Err((
            -32022,
            "Unsupported protocol version".into(),
            Some(json!({
                "supported": [MODERN_PROTOCOL_VERSION],
                "requested": version
            })),
        ));
    }
    if params
        .get("_meta")
        .and_then(|m| m.get(META_CLIENT_CAPABILITIES))
        .is_none()
    {
        return Err((
            -32602,
            format!("missing required _meta field \"{META_CLIENT_CAPABILITIES}\""),
            None,
        ));
    }
    let result = match method {
        "server/discover" => json!({
            "supportedVersions": [MODERN_PROTOCOL_VERSION],
            "capabilities": { "tools": {} },
            "instructions": INSTRUCTIONS,
            "ttlMs": CACHE_TTL_MS,
            "cacheScope": "public"
        }),
        "ping" => json!({}),
        "tools/list" => {
            let mut list = tools_list();
            let obj = list.as_object_mut().expect("tools_list is an object");
            // static per binary and free of user data → cacheable
            obj.insert("ttlMs".into(), json!(CACHE_TTL_MS));
            obj.insert("cacheScope".into(), json!("public"));
            list
        }
        "tools/call" => tools_call(params)?,
        _ => return Err((-32601, format!("method not found: {method}"), None)),
    };
    Ok(modernize(result))
}

fn initialize(params: &Value) -> Value {
    // echo the client's protocol version when given (we speak the basics)
    let version = params
        .get("protocolVersion")
        .and_then(|v| v.as_str())
        .unwrap_or(DEFAULT_PROTOCOL_VERSION);
    json!({
        "protocolVersion": version,
        "capabilities": { "tools": {} },
        "serverInfo": { "name": SERVER_NAME, "version": SERVER_VERSION },
        "instructions": INSTRUCTIONS
    })
}

/// Shown for every tool call while the database can't be opened.
pub const DB_UNAVAILABLE_MESSAGE: &str =
    "Open Fly on the Wall once to finish updating its database, then retry.";

/// The stdio server as `main` runs it: opens the app's database lazily and
/// keeps serving when that fails (the app hasn't been opened since an
/// update, or the database is newer than this binary). The handshake and
/// tool list still work, every tool call returns [`DB_UNAVAILABLE_MESSAGE`]
/// with the reason, and the open is retried on each message so the server
/// recovers without the client restarting it.
pub struct LazyServer {
    data_dir: std::path::PathBuf,
    server: std::cell::RefCell<Option<Server>>,
}

impl LazyServer {
    /// Try the open once up front; failures are reported, not fatal.
    pub fn new(data_dir: std::path::PathBuf) -> Self {
        let lazy = Self {
            data_dir,
            server: std::cell::RefCell::new(None),
        };
        if let Err(e) = lazy.ensure_open() {
            // stderr only: stdout is the protocol channel
            eprintln!("flyonthewall-mcp: database unavailable, serving errors until it opens: {e}");
        }
        lazy
    }

    pub fn is_open(&self) -> bool {
        self.server.borrow().is_some()
    }

    fn ensure_open(&self) -> Result<(), StorageError> {
        if self.is_open() {
            return Ok(());
        }
        // The app owns the schema: never migrate or seed here. Only a first
        // run, before the app ever created a database, uses the full open so
        // Claude Desktop still gets a working (empty) server.
        let storage = match Storage::open_existing(&self.data_dir) {
            Err(StorageError::NotFound(_)) => Storage::open(&self.data_dir)?,
            other => other?,
        };
        *self.server.borrow_mut() = Some(Server::new(storage));
        Ok(())
    }

    pub fn handle_message(&self, raw: &str) -> Option<String> {
        let unavailable = self.ensure_open().err();
        if let Some(server) = self.server.borrow().as_ref() {
            return server.handle_message(raw);
        }
        let reason = unavailable.map(|e| e.to_string()).unwrap_or_default();
        dispatch(raw, &|_params| {
            Ok(json!({
                "content": [{"type": "text", "text": format!("{DB_UNAVAILABLE_MESSAGE} ({reason})")}],
                "isError": true
            }))
        })
    }
}

impl Server {
    fn tools_call(&self, params: &Value) -> Result<Value, RpcError> {
        let name = params.get("name").and_then(|n| n.as_str()).unwrap_or("");
        let args = params.get("arguments").cloned().unwrap_or(json!({}));
        let text = match name {
            "get_context" => self.tool_get_context(&args),
            "whats_changed" => self.tool_whats_changed(&args),
            "open_items" => self.tool_open_items(&args),
            "query_items" => self.tool_query_items(&args),
            "get_meeting_items" => self.tool_get_meeting_items(&args),
            "search_notes" => self.tool_search(&args),
            "search_passages" => self.tool_search_passages(&args),
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
            Ok(text) => Ok(json!({"content": [{"type": "text", "text": cap_result(text)}]})),
            Err(message) => Ok(json!({
                "content": [{"type": "text", "text": message}],
                "isError": true
            })),
        }
    }

    fn tool_search(&self, args: &Value) -> Result<String, String> {
        let query = arg_query(args, "query")?;
        let limit = arg_limit(args, 20);
        // Cursor = "<note offset>:<transcript offset>" (a bare number from
        // older replies applies to both). The two rankings page separately.
        let (note_off, transcript_off) = args
            .get("cursor")
            .and_then(|c| c.as_str())
            .and_then(|c| match c.split_once(':') {
                Some((n, t)) => Some((n.parse().ok()?, t.parse().ok()?)),
                None => c.parse().ok().map(|n| (n, n)),
            })
            .unwrap_or((0usize, 0usize));
        let base = SearchFilter {
            folder_id: arg_str(args, "folder_id"),
            since: arg_time(args, "since")?,
            until: arg_until(args)?,
            limit,
            offset: 0,
            // Keep-local rows are excluded inside SQL: they never fill a page,
            // shift a cursor or make an empty page look different.
            exclude_keep_local: true,
        };
        let search = |offset: usize| {
            self.storage
                .search_split(
                    query,
                    &SearchFilter {
                        offset,
                        ..base.clone()
                    },
                )
                .map_err(|e| e.to_string())
        };
        let (notes, same_offset_transcripts) = search(note_off)?;
        let transcripts = if transcript_off == note_off {
            same_offset_transcripts
        } else {
            search(transcript_off)?.1
        };
        // Notes first, then transcripts, at most `limit` in total; each
        // list's cursor advances only past the rows actually returned.
        let take_notes = notes.len().min(limit);
        let take_transcripts = transcripts.len().min(limit - take_notes);
        let more = notes.len() >= limit
            || transcripts.len() > take_transcripts
            || transcripts.len() >= limit;
        let next = format!(
            "{}:{}",
            note_off + take_notes,
            transcript_off + take_transcripts
        );
        let hits: Vec<_> = notes
            .into_iter()
            .take(take_notes)
            .chain(transcripts.into_iter().take(take_transcripts))
            .collect();
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
        if more {
            out.push_str(&format!(
                "\nMore may exist — pass cursor: \"{next}\" to continue.\n"
            ));
        }
        Ok(out)
    }

    fn tool_list_folders(&self) -> Result<String, String> {
        let folders = self.storage.list_folders().map_err(|e| e.to_string())?;
        if folders.is_empty() {
            return Ok("No folders yet.".into());
        }
        // Keep-local folders (the set already includes their descendants)
        // are omitted entirely — name, id and place in the hierarchy. A
        // visible folder never points at a hidden parent.
        let local = self
            .storage
            .keep_local_folder_ids()
            .map_err(keep_local_lookup_error)?;
        let visible: Vec<_> = folders
            .into_iter()
            .filter(|f| !local.contains(&f.id))
            .collect();
        if visible.is_empty() {
            return Ok("No folders yet.".into());
        }
        let mut out = String::from("Folders:\n");
        for f in visible {
            out.push_str(&format!(
                "- {} (id: {}{})\n",
                f.name,
                f.id,
                f.parent_id
                    .as_deref()
                    .filter(|p| !local.contains(*p))
                    .map(|p| format!(", parent: {p}"))
                    .unwrap_or_default(),
            ));
        }
        Ok(out)
    }

    // ---- keep-local enforcement ----
    //
    // Everything this server returns goes to the MCP client's (cloud) model,
    // so meetings/notes marked "Keep local" — directly or through a
    // local-only folder — must never appear in a result. A meeting counts as
    // keep-local when its note does (own flag, a sibling meeting's flag, or
    // the folder chain). Lookup errors fail closed.

    fn guard_note(&self, note_id: &str) -> Result<(), String> {
        if self.storage.note_keep_local(note_id) {
            return Err(KEEP_LOCAL_NOTE_MESSAGE.into());
        }
        Ok(())
    }

    /// Refuse a by-id meeting lookup when the meeting is keep-local. An
    /// unknown id passes (the tool then reports "not found" as before)
    /// unless orphaned content exists under it — its privacy can't be
    /// determined, so it is refused rather than shared.
    fn guard_meeting(&self, meeting_id: &str) -> Result<(), String> {
        match self.storage.get_meeting(meeting_id) {
            Ok(m) => {
                if self.storage.meeting_keep_local(meeting_id)
                    || self.storage.note_keep_local(&m.note_id)
                {
                    Err(KEEP_LOCAL_MEETING_MESSAGE.into())
                } else {
                    Ok(())
                }
            }
            Err(StorageError::NotFound(_)) => {
                let orphan = !matches!(self.storage.get_transcript(meeting_id), Ok(None))
                    || !matches!(self.storage.has_meeting_items(meeting_id), Ok(false));
                if orphan {
                    Err(format!("meeting {meeting_id} not found"))
                } else {
                    Ok(())
                }
            }
            Err(e) => Err(keep_local_lookup_error(e)),
        }
    }

    fn tool_get_note(&self, args: &Value) -> Result<String, String> {
        let id = args
            .get("note_id")
            .and_then(|q| q.as_str())
            .ok_or("missing required argument: note_id")?;
        self.guard_note(id)?;
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
        self.guard_meeting(meeting_id)?;
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
        let mut skipped: Vec<&str> = Vec::new();
        for id in &ids {
            let part = match self.render_transcript(id, with_ids) {
                Ok(t) => t,
                Err(e) => format!("# meeting {id}\n({e})\n"),
            };
            // Whole transcripts only: stop before the reply outgrows the
            // result cap and say which ids to fetch next.
            if !out.is_empty() && out.len() + part.len() > MAX_RESULT_BYTES - 4_096 {
                skipped.push(id);
                continue;
            }
            out.push_str(&part);
            out.push_str("\n---\n\n");
        }
        if !skipped.is_empty() {
            out.push_str(&format!(
                "[{} transcript(s) omitted to keep this reply under {} bytes — fetch them in another call: {}]\n",
                skipped.len(),
                MAX_RESULT_BYTES,
                skipped.join(", ")
            ));
        }
        Ok(out)
    }

    fn tool_get_meeting(&self, args: &Value) -> Result<String, String> {
        let id = args
            .get("meeting_id")
            .and_then(|q| q.as_str())
            .ok_or("missing required argument: meeting_id")?;
        self.guard_meeting(id)?;
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
        let folder_id = arg_str(args, "folder_id");
        let (since, until) = (arg_time(args, "since")?, arg_until(args)?);
        // Keep-local notes are excluded inside SQL, so they never fill a page
        // and the cursor (the last row returned) is always a shareable note.
        let notes = self
            .storage
            .list_notes_filtered(limit, folder_id.as_deref(), since, until, before, true)
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
            let last = notes.last().expect("non-empty");
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
            out.push_str(&context::render_item_line(
                &i.text,
                i.kind,
                i.owner.as_deref(),
                i.status.as_deref(),
                date,
                &i.meeting_id,
                &i.segment_ids,
            ));
        }
        out
    }

    fn tool_get_meeting_items(&self, args: &Value) -> Result<String, String> {
        let id = args
            .get("meeting_id")
            .and_then(|q| q.as_str())
            .ok_or("missing required argument: meeting_id")?;
        self.guard_meeting(id)?;
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
                exclude_keep_local: true,
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

    // ---- context layer (fly_context::memory, under ShareableOnly) ----
    //
    // Everything here goes to the MCP client's (cloud) model, so the shared
    // memory code runs with Visibility::ShareableOnly: Keep-local meetings
    // never shape a series, a participant list or an item, and a failed
    // Keep-local lookup refuses the whole result.

    fn shared(&self) -> Context<'_> {
        Context::new(&self.storage, Visibility::ShareableOnly)
    }

    fn tool_get_context(&self, args: &Value) -> Result<String, String> {
        let query = arg_query(args, "query")?;
        let since = arg_time(args, "since")?;
        let b = fly_context::memory::mcp::get_context(&self.shared(), query, since)
            .map_err(context_error)?;
        Ok(context::render_briefing(&b, query, since))
    }

    fn tool_whats_changed(&self, args: &Value) -> Result<String, String> {
        let series_key = args
            .get("series")
            .and_then(|q| q.as_str())
            .ok_or("missing required argument: series (a series name from get_context)")?;
        let since = arg_time(args, "since")?
            .ok_or("missing required argument: since (YYYY-MM-DD or RFC 3339)")?;
        let b = fly_context::memory::mcp::whats_changed(&self.shared(), series_key, since)
            .map_err(context_error)?;
        Ok(context::render_whats_changed(&b, series_key, since))
    }

    fn tool_open_items(&self, args: &Value) -> Result<String, String> {
        let scope = arg_str(args, "scope").unwrap_or_else(|| "all".into());
        let o =
            fly_context::memory::mcp::open_items(&self.shared(), &scope).map_err(context_error)?;
        Ok(context::render_open_items(&o))
    }

    /// Passage-level search on fly-context, keyword only (this server has
    /// no embedding model). Transcript lines carry `[seg:ID]`.
    fn tool_search_passages(&self, args: &Value) -> Result<String, String> {
        let query = arg_query(args, "query")?;
        let meeting_ids = match args.get("meeting_ids") {
            None | Some(Value::Null) => None,
            Some(v) => Some(
                v.as_array()
                    .ok_or("meeting_ids must be an array of strings")?
                    .iter()
                    .map(|id| id.as_str().map(String::from))
                    .collect::<Option<Vec<_>>>()
                    .ok_or("meeting_ids must be an array of strings")?,
            ),
        };
        // Dates are checked here so the error names this server's forms.
        let date = |key: &str| -> Result<Option<String>, String> {
            arg_time(args, key)?;
            Ok(arg_str(args, key))
        };
        // (fly-context reads a bare `until` date as the end of that day,
        // like arg_until)
        // Fields added to the args later keep their defaults.
        #[allow(clippy::needless_update)]
        let search = SearchPassagesArgs {
            query: query.to_string(),
            meeting_ids,
            folder_id: arg_str(args, "folder_id"),
            since: date("since")?,
            until: date("until")?,
            // A model's call: held to the schema's 1-20 (in-process
            // callers of fly-context may ask for more).
            limit: args
                .get("limit")
                .and_then(|l| l.as_u64())
                .map(|l| l.min(u64::from(fly_context::tools::MAX_TOOL_LIMIT)) as u32),
            ..Default::default()
        };
        let r = self
            .shared()
            .search_passages(&search, None)
            .map_err(context_error)?;
        let mut out = fly_context::render::passages(&r);
        if !r.passages.is_empty() {
            out.push_str(
                "\nTranscript lines read [seg:ID] Speaker (m:ss): text. Check one in context \
                 with get_transcript(meeting_id, with_segment_ids: true).\n",
            );
        }
        Ok(out)
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
        let label = clean_speaker_label(
            args.get("label")
                .and_then(|q| q.as_str())
                .ok_or("missing required argument: label")?,
        )?;
        let label = label.as_str();
        self.guard_meeting(meeting_id)?;
        // Only rename speakers that exist: the app UI can't invent new keys,
        // so neither can an AI client.
        let transcript = self
            .storage
            .get_transcript(meeting_id)
            .map_err(|e| e.to_string())?
            .ok_or(format!("no transcript for meeting {meeting_id}"))?;
        let known = transcript.speakers.iter().any(|s| s.key == speaker_key)
            || transcript
                .segments
                .iter()
                .any(|s| s.speaker_key == speaker_key);
        if !known {
            return Err(format!(
                "speaker_key \"{speaker_key}\" is not in this transcript. Known keys: {}",
                transcript
                    .speakers
                    .iter()
                    .map(|s| s.key.as_str())
                    .collect::<Vec<_>>()
                    .join(", ")
            ));
        }
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

/// Returned for a by-id lookup of a keep-local meeting. Deliberately says
/// nothing about the meeting (no title, date or attendees).
pub const KEEP_LOCAL_MEETING_MESSAGE: &str =
    "This meeting is marked Keep local and isn't shared with AI tools.";
/// Same, for a note.
pub const KEEP_LOCAL_NOTE_MESSAGE: &str =
    "This note is marked Keep local and isn't shared with AI tools.";
/// A failed keep-local lookup refuses the whole result (fail closed).
fn keep_local_lookup_error(e: StorageError) -> String {
    format!("couldn't check Keep local settings, so nothing was returned: {e}")
}

/// A fly-context error as tool text.
fn context_error(e: fly_context::ContextError) -> String {
    // A failed Keep-local lookup has its own variant and message
    // ("couldn't check Keep local settings, so nothing was returned");
    // other storage errors say what they are.
    e.to_string()
}

/// Longest speaker label `set_speaker_label` accepts, in characters.
pub const MAX_SPEAKER_LABEL_CHARS: usize = 100;

/// Longest free-text query (`search_notes`, `get_context`) accepted.
pub const MAX_QUERY_CHARS: usize = 1_000;

/// Upper bound on one tool result's text. Larger results are cut at a char
/// boundary with a note telling the client how to narrow the request, so a
/// batch of very long transcripts can't produce a multi-megabyte reply.
pub const MAX_RESULT_BYTES: usize = 1_000_000;

/// Normalise an AI-supplied speaker label: control characters, newlines and
/// invisible bidi/zero-width format characters become spaces, whitespace
/// runs collapse, and the result must be non-empty and at most
/// [`MAX_SPEAKER_LABEL_CHARS`].
fn clean_speaker_label(raw: &str) -> Result<String, String> {
    let is_invisible = |c: char| matches!(c, '\u{200B}'..='\u{200F}' | '\u{202A}'..='\u{202E}' | '\u{2066}'..='\u{2069}' | '\u{FEFF}');
    let spaced: String = raw
        .chars()
        .map(|c| {
            if c.is_control() || is_invisible(c) {
                ' '
            } else {
                c
            }
        })
        .collect();
    let label = spaced.split_whitespace().collect::<Vec<_>>().join(" ");
    if label.is_empty() {
        return Err("label must not be empty".into());
    }
    if label.chars().count() > MAX_SPEAKER_LABEL_CHARS {
        return Err(format!(
            "label is longer than {MAX_SPEAKER_LABEL_CHARS} characters"
        ));
    }
    Ok(label)
}

/// Required free-text query argument, length-capped.
fn arg_query<'a>(args: &'a Value, key: &str) -> Result<&'a str, String> {
    let q = args
        .get(key)
        .and_then(|q| q.as_str())
        .ok_or(format!("missing required argument: {key}"))?;
    if q.chars().count() > MAX_QUERY_CHARS {
        return Err(format!("{key} is longer than {MAX_QUERY_CHARS} characters"));
    }
    Ok(q)
}

/// Cut `text` to [`MAX_RESULT_BYTES`] at a char boundary, with a note.
fn cap_result(mut text: String) -> String {
    if text.len() <= MAX_RESULT_BYTES {
        return text;
    }
    let mut cut = MAX_RESULT_BYTES;
    while !text.is_char_boundary(cut) {
        cut -= 1;
    }
    let omitted = text.len() - cut;
    text.truncate(cut);
    text.push_str(&format!(
        "\n\n[result truncated: {omitted} more bytes omitted — request fewer items or one meeting at a time]\n"
    ));
    text
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

/// The `until` argument: RFC 3339, or a bare YYYY-MM-DD meaning through the
/// end of that day (UTC), the same as `search_passages` (fly-context).
fn arg_until(args: &Value) -> Result<Option<DateTime<Utc>>, String> {
    let Some(raw) = args.get("until").and_then(|v| v.as_str()) else {
        return Ok(None);
    };
    if let Ok(d) = chrono::NaiveDate::parse_from_str(raw, "%Y-%m-%d") {
        return Ok(Some(DateTime::from_naive_utc_and_offset(
            d.and_hms_milli_opt(23, 59, 59, 999).unwrap(),
            Utc,
        )));
    }
    arg_time(args, "until")
}

fn error_response(id: Value, code: i32, message: &str, data: Option<Value>) -> String {
    let mut error = json!({"code": code, "message": message});
    if let Some(data) = data {
        error["data"] = data;
    }
    json!({"jsonrpc": "2.0", "id": id, "error": error}).to_string()
}

/// Wrap a result for a modern (2026-07-28) client: mark it complete and
/// self-identify in `_meta` — modern responses carry identity per-message
/// instead of relying on a handshake.
fn modernize(mut result: Value) -> Value {
    let obj = result.as_object_mut().expect("results are objects");
    obj.entry("resultType").or_insert(json!("complete"));
    obj.entry("_meta").or_insert(json!({}))[META_SERVER_INFO] =
        json!({ "name": SERVER_NAME, "version": SERVER_VERSION });
    result
}

fn tools_list() -> Value {
    let time_desc = "RFC 3339, or YYYY-MM-DD (from the start of that day, UTC)";
    let until_desc = "RFC 3339, or YYYY-MM-DD (through the end of that day, UTC)";
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
                    "until": { "type": "string", "description": until_desc },
                    "limit": { "type": "integer", "minimum": 1, "maximum": 100 },
                    "cursor": { "type": "string", "description": "From a previous page's footer" }
                },
                "required": ["query"]
            }
        },
        {
            "name": "search_passages",
            "description": "Passage-level search: the transcript passages (speaker-turn windows of about 160 words) that best match the query, each line carrying its segment id as [seg:ID], plus matching paragraphs of the user's notes. Use it to find what was actually said about a topic and to quote it with ids; get_context gives the structured overview. Keyword ranking (no embeddings).",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "query": { "type": "string", "description": "What to look for, in plain words" },
                    "meeting_ids": { "type": "array", "items": { "type": "string" }, "maxItems": 50, "description": "Only search these meetings" },
                    "folder_id": { "type": "string" },
                    "since": { "type": "string", "description": time_desc },
                    "until": { "type": "string", "description": until_desc },
                    "limit": { "type": "integer", "minimum": 1, "maximum": 20, "description": "Passages to return (default 8)" }
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
                    "until": { "type": "string", "description": until_desc },
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
                "search_passages",
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

    /// The modern per-request `_meta` every 2026-07-28 request must carry.
    fn modern_meta() -> serde_json::Value {
        json!({
            META_PROTOCOL_VERSION: MODERN_PROTOCOL_VERSION,
            META_CLIENT_CAPABILITIES: {},
            "io.modelcontextprotocol/clientInfo": {"name": "t", "version": "0"}
        })
    }

    #[test]
    fn modern_discover_lists_versions_capabilities_and_identity() {
        let (_d, server, _n, _m) = server_with_data();
        let resp = call(
            &server,
            json!({"jsonrpc":"2.0","id":1,"method":"server/discover","params":{"_meta": modern_meta()}}),
        );
        let r = &resp["result"];
        assert_eq!(r["resultType"], "complete", "{resp}");
        assert_eq!(r["supportedVersions"], json!([MODERN_PROTOCOL_VERSION]));
        assert!(r["capabilities"]["tools"].is_object(), "{resp}");
        assert!(r["instructions"].as_str().unwrap().contains("get_context"));
        assert_eq!(r["_meta"][META_SERVER_INFO]["name"], SERVER_NAME);
        assert!(r["ttlMs"].as_u64().unwrap() > 0);
        assert_eq!(r["cacheScope"], "public");
    }

    #[test]
    fn modern_requests_are_stateless_and_self_describing() {
        let (_d, server, note_id, _m) = server_with_data();
        // no discover, no handshake — a lone tools/call must just work
        let hits = call(
            &server,
            json!({"jsonrpc":"2.0","id":2,"method":"tools/call","params":{
                "name":"search_notes","arguments":{"query":"roadmap"},"_meta": modern_meta()}}),
        );
        assert_eq!(hits["result"]["resultType"], "complete", "{hits}");
        assert_eq!(
            hits["result"]["_meta"][META_SERVER_INFO]["version"],
            SERVER_VERSION
        );
        assert!(hits["result"]["content"][0]["text"]
            .as_str()
            .unwrap()
            .contains(&note_id));

        // tools/list is cacheable under the modern era
        let tools = call(
            &server,
            json!({"jsonrpc":"2.0","id":3,"method":"tools/list","params":{"_meta": modern_meta()}}),
        );
        assert_eq!(tools["result"]["resultType"], "complete");
        assert!(tools["result"]["ttlMs"].as_u64().unwrap() > 0);
        assert_eq!(tools["result"]["cacheScope"], "public");
        assert_eq!(tools["result"]["tools"].as_array().unwrap().len(), 14);
    }

    #[test]
    fn modern_version_negotiation_rejects_with_32022() {
        let (_d, server, _n, _m) = server_with_data();
        let resp = call(
            &server,
            json!({"jsonrpc":"2.0","id":4,"method":"tools/list","params":{"_meta":{
                META_PROTOCOL_VERSION: "1900-01-01",
                META_CLIENT_CAPABILITIES: {}}}}),
        );
        assert_eq!(resp["error"]["code"], -32022, "{resp}");
        assert_eq!(
            resp["error"]["data"]["supported"],
            json!([MODERN_PROTOCOL_VERSION])
        );
        assert_eq!(resp["error"]["data"]["requested"], "1900-01-01");
    }

    #[test]
    fn modern_missing_required_meta_is_invalid_params() {
        let (_d, server, _n, _m) = server_with_data();
        // protocolVersion present but clientCapabilities missing → -32602
        let resp = call(
            &server,
            json!({"jsonrpc":"2.0","id":5,"method":"tools/list","params":{"_meta":{
                META_PROTOCOL_VERSION: MODERN_PROTOCOL_VERSION}}}),
        );
        assert_eq!(resp["error"]["code"], -32602, "{resp}");

        // server/discover without any _meta is malformed, not "unknown method"
        let bare = call(
            &server,
            json!({"jsonrpc":"2.0","id":6,"method":"server/discover"}),
        );
        assert_eq!(bare["error"]["code"], -32602, "{bare}");
    }

    #[test]
    fn legacy_era_responses_stay_unchanged_for_old_clients() {
        let (_d, server, _n, _m) = server_with_data();
        // metadata-less requests keep the pre-2026 shape: no resultType,
        // no _meta — old clients parse these byte-for-byte as before
        let tools = call(
            &server,
            json!({"jsonrpc":"2.0","id":7,"method":"tools/list"}),
        );
        assert!(tools["result"].get("resultType").is_none(), "{tools}");
        assert!(tools["result"].get("ttlMs").is_none(), "{tools}");

        let hit = call(
            &server,
            json!({"jsonrpc":"2.0","id":8,"method":"tools/call","params":{"name":"list_folders","arguments":{}}}),
        );
        assert!(hit["result"].get("resultType").is_none(), "{hit}");
        assert!(hit["result"].get("_meta").is_none(), "{hit}");
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
    fn search_passages_returns_citable_lines_and_checks_arguments() {
        let (_d, server, m0, m1) = server_with_series();
        let text = tool_text(
            &server,
            "search_passages",
            json!({"query": "renewal discussion"}),
        );
        assert!(text.starts_with("2 passage(s):"), "{text}");
        assert!(text.contains(&format!("(meeting {m0})")), "{text}");
        assert!(
            text.contains("[seg:s0] Speaker 1 (0:00): renewal discussion"),
            "{text}"
        );
        assert!(text.contains("get_transcript(meeting_id"), "{text}");

        let scoped = tool_text(
            &server,
            "search_passages",
            json!({"query": "renewal", "meeting_ids": [m1], "limit": 1}),
        );
        assert!(scoped.starts_with("1 passage(s):"), "{scoped}");
        assert!(scoped.contains(&m1) && !scoped.contains(&m0), "{scoped}");

        let none = tool_text(&server, "search_passages", json!({"query": "zebra"}));
        assert_eq!(none, "No matching passages found in the user's meetings.\n");
        for bad in [
            json!({}),
            json!({"query": "renewal", "meeting_ids": "m"}),
            json!({"query": "renewal", "meeting_ids": [1]}),
            json!({"query": "renewal", "since": "last week"}),
            json!({"query": "a".repeat(MAX_QUERY_CHARS + 1)}),
        ] {
            tool_error(&server, "search_passages", bad);
        }
    }

    /// Break the Keep-local lookup itself: the context layer and passage
    /// search refuse, and say why, instead of answering unfiltered.
    #[test]
    fn context_layer_and_passages_fail_closed_when_keep_local_cant_be_checked() {
        let f = server_with_keep_local();
        rusqlite::Connection::open(f._dir.path().join("flyonthewall.db"))
            .unwrap()
            .execute_batch("ALTER TABLE folders RENAME COLUMN keep_local TO keep_local_broken")
            .unwrap();
        for (tool, args) in [
            ("get_context", json!({"query": "acme renewal"})),
            (
                "whats_changed",
                json!({"series": "acme renewal", "since": "2020-01-01"}),
            ),
            ("open_items", json!({})),
            ("open_items", json!({"scope": "acme renewal"})),
            ("search_passages", json!({"query": "renewal"})),
        ] {
            let err = tool_error(&f.server, tool, args);
            assert!(
                err.starts_with("couldn't check Keep local settings, so nothing was returned"),
                "{tool}: {err}"
            );
            assert_no_leak(&f, &err);
        }
    }

    /// One transcript that no longer parses: its meeting reads as having no
    /// transcript (as before the move), the series still answers.
    #[test]
    fn one_unreadable_transcript_does_not_fail_get_context() {
        let (d, server, m0, m1) = server_with_series();
        rusqlite::Connection::open(d.path().join("flyonthewall.db"))
            .unwrap()
            .execute(
                "UPDATE transcripts SET json = '{\"speakers\": 1}' WHERE meeting_id = ?1",
                [&m0],
            )
            .unwrap();
        let text = tool_text(&server, "get_context", json!({"query": "acme renewal"}));
        assert!(text.contains("2 meeting(s)"), "{text}");
        assert!(
            text.contains(&format!("(meeting: {m0}, no transcript")),
            "{text}"
        );
        assert!(
            text.contains(&format!("(meeting: {m1}, transcript ✓")),
            "{text}"
        );
        tool_text(&server, "open_items", json!({}));
    }

    /// A bare `until` date includes that whole day on every tool that takes
    /// one.
    #[test]
    fn until_includes_the_whole_day_everywhere() {
        let (_d, server, note_id, meeting_id) = server_with_data();
        let today = Utc::now().format("%Y-%m-%d").to_string();
        let yesterday = (Utc::now() - chrono::Duration::days(1))
            .format("%Y-%m-%d")
            .to_string();
        for (tool, args, want) in [
            (
                "search_notes",
                json!({"query": "roadmap", "until": today}),
                note_id.clone(),
            ),
            ("list_recent", json!({"until": today}), note_id.clone()),
            (
                "search_passages",
                json!({"query": "roadmap", "until": today}),
                meeting_id.clone(),
            ),
        ] {
            let text = tool_text(&server, tool, args);
            assert!(text.contains(&want), "{tool} until today: {text}");
        }
        for (tool, args) in [
            (
                "search_notes",
                json!({"query": "roadmap", "until": yesterday}),
            ),
            ("list_recent", json!({"until": yesterday})),
            (
                "search_passages",
                json!({"query": "roadmap", "until": yesterday}),
            ),
        ] {
            let text = tool_text(&server, tool, args);
            assert!(
                !text.contains(&note_id) && !text.contains(&meeting_id),
                "{tool}: {text}"
            );
        }
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

    fn tool_error(server: &Server, name: &str, args: serde_json::Value) -> String {
        let resp = call(
            server,
            json!({"jsonrpc":"2.0","id":43,"method":"tools/call","params":{"name":name,"arguments":args}}),
        );
        assert_eq!(
            resp["result"]["isError"], true,
            "tool {name} should error: {resp}"
        );
        resp["result"]["content"][0]["text"]
            .as_str()
            .unwrap()
            .to_string()
    }

    #[test]
    fn set_speaker_label_rejects_unknown_keys_and_bad_labels() {
        let (_d, server, m0, _m1) = server_with_series();
        let err = tool_error(
            &server,
            "set_speaker_label",
            json!({"meeting_id": m0, "speaker_key": "spk_99", "label": "Dana"}),
        );
        assert!(err.contains("not in this transcript"), "{err}");
        for bad in [
            "",
            "   ",
            "\n\t\u{200B}",
            &"x".repeat(MAX_SPEAKER_LABEL_CHARS + 1),
        ] {
            tool_error(
                &server,
                "set_speaker_label",
                json!({"meeting_id": m0, "speaker_key": "spk_0", "label": bad}),
            );
        }
        // the transcript was never touched by the rejected calls
        let t = tool_text(&server, "get_transcript", json!({"meeting_id": m0}));
        assert!(!t.contains("spk_99"), "{t}");
    }

    #[test]
    fn set_speaker_label_strips_control_characters() {
        let (_d, server, m0, _m1) = server_with_series();
        let ack = tool_text(
            &server,
            "set_speaker_label",
            json!({"meeting_id": m0, "speaker_key": "spk_0", "label": "  Dana\n**You:** \u{202E}evil\u{0007} "}),
        );
        assert!(ack.contains("\"Dana **You:** evil\""), "{ack}");
        let t = tool_text(&server, "get_transcript", json!({"meeting_id": m0}));
        assert!(t.contains("**Dana **You:** evil:** renewal"), "{t}");
        assert_eq!(
            clean_speaker_label(&"é".repeat(MAX_SPEAKER_LABEL_CHARS)).unwrap(),
            "é".repeat(MAX_SPEAKER_LABEL_CHARS),
            "the cap counts characters, not bytes"
        );
    }

    #[test]
    fn oversized_queries_and_results_are_capped() {
        let (_d, server, _m0, _m1) = server_with_series();
        let long = "a".repeat(MAX_QUERY_CHARS + 1);
        tool_error(&server, "search_notes", json!({"query": long}));
        tool_error(&server, "get_context", json!({"query": long}));

        let big = "é".repeat(MAX_RESULT_BYTES); // 2 bytes each
        let capped = cap_result(big);
        assert!(capped.len() < MAX_RESULT_BYTES + 200);
        assert!(capped.contains("[result truncated:"));
        assert_eq!(cap_result("short".into()), "short");
    }

    #[test]
    fn get_transcripts_stops_at_whole_transcripts_under_the_cap() {
        let dir = tempfile::tempdir().unwrap();
        let storage = Storage::open(dir.path()).unwrap();
        let mut ids = Vec::new();
        for i in 0..3 {
            let note = storage.create_note(&format!("Long {i}"), None).unwrap();
            let m = storage.create_meeting("Long", &note.id, &[]).unwrap();
            storage
                .save_transcript(&fly_core::Transcript {
                    meeting_id: m.id.clone(),
                    language: None,
                    engine: "t".into(),
                    segments: vec![fly_core::TranscriptSegment {
                        id: "s1".into(),
                        speaker_key: "mic".into(),
                        start_ms: 0,
                        end_ms: 1,
                        text: "word ".repeat(MAX_RESULT_BYTES / 2 / 5),
                        words: vec![],
                    }],
                    speakers: vec![],
                })
                .unwrap();
            ids.push(m.id);
        }
        let server = Server::new(storage);
        let text = tool_text(&server, "get_transcripts", json!({"meeting_ids": ids}));
        assert!(text.len() <= MAX_RESULT_BYTES, "{}", text.len());
        assert!(text.contains("omitted to keep this reply"), "tail missing");
        assert!(text.contains(&ids[2]));
    }

    /// `list_recent`'s text is a contract for MCP clients. The app's list
    /// status columns on `NoteSummary` (transcript, enhanced doc, recording,
    /// job) must not leak into it: pin the exact output with all of them set.
    #[test]
    fn list_recent_text_is_pinned_and_omits_app_status() {
        let dir = tempfile::tempdir().unwrap();
        let storage = Storage::open(dir.path()).unwrap();
        let note = storage.create_note("Budget sync", None).unwrap();
        let meeting = storage
            .create_meeting("Budget sync", &note.id, &[])
            .unwrap();
        storage
            .end_meeting(
                &meeting.id,
                &fly_core::RecordingRef {
                    mic_path: Some("recordings/b/recording.mic.wav".into()),
                    system_path: None,
                    mixed_path: None,
                    playback_path: None,
                    duration_ms: 1_000,
                },
            )
            .unwrap();
        storage
            .save_transcript(&fly_core::Transcript {
                meeting_id: meeting.id.clone(),
                language: None,
                engine: "whisper.cpp".into(),
                segments: vec![],
                speakers: vec![],
            })
            .unwrap();
        storage
            .update_note_blocks(&note.id, &[fly_core::NoteBlock::user("Decided")])
            .unwrap();
        storage.enqueue_transcription(&meeting.id).unwrap();
        storage
            .mark_transcription_failed(&meeting.id, 1, "boom")
            .unwrap();
        let row = storage.list_recent_notes(1).unwrap().remove(0);
        assert!(row.has_transcript && row.has_enhanced_doc && row.has_recording);
        assert_eq!(row.job_status.as_deref(), Some("failed"));

        let server = Server::new(storage);
        let text = tool_text(&server, "list_recent", json!({"limit": 10}));
        assert_eq!(
            text,
            format!(
                "Recent notes:
- Budget sync (note_id: {}, updated {}, meeting_id: {})
",
                note.id,
                row.updated_at.to_rfc3339(),
                meeting.id
            )
        );
        for leaked in ["has_", "enhanced", "job", "failed", "recording"] {
            assert!(!text.contains(leaked), "{leaked} leaked: {text}");
        }
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

    /// Three "Acme renewal" meetings in one series: one shared, one with
    /// the meeting's own Keep-local flag, one filed under a sub-folder of a
    /// local-only folder. Everything private carries the word "secretword".
    struct KeepLocalFixture {
        _dir: tempfile::TempDir,
        server: Server,
        visible_note: String,
        visible_meeting: String,
        /// (note_id, meeting_id) — flagged directly, filed in a local folder
        hidden: [(String, String); 2],
    }

    fn server_with_keep_local() -> KeepLocalFixture {
        let dir = tempfile::tempdir().unwrap();
        let storage = Storage::open(dir.path()).unwrap();
        let legal = storage.create_folder("Legal", None).unwrap();
        let cases = storage.create_folder("Cases", Some(&legal.id)).unwrap();
        storage.set_folder_keep_local(&legal.id, true).unwrap();
        let mut made = Vec::new();
        for (i, (title, folder, word)) in [
            ("Acme renewal 2026-06-01", None, "shared"),
            ("Acme renewal 2026-06-08", None, "secretword"),
            (
                "Acme renewal 2026-06-15",
                Some(cases.id.as_str()),
                "secretword",
            ),
        ]
        .into_iter()
        .enumerate()
        {
            let note = storage.create_note(title, folder).unwrap();
            storage
                .update_note_scratchpad(&note.id, &format!("renewal notes {word} body"))
                .unwrap();
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
                        text: format!("renewal discussion {word}"),
                        words: vec![],
                    }],
                    speakers: vec![fly_core::Speaker {
                        key: "spk_0".into(),
                        label: "Speaker 1".into(),
                    }],
                })
                .unwrap();
            let item = |kind: ItemKind, text: String, status: Option<&str>| fly_core::MeetingItem {
                id: fly_core::new_id(),
                meeting_id: meeting.id.clone(),
                kind,
                text,
                owner: Some("Dana".into()),
                status: status.map(String::from),
                speaker_key: Some("spk_0".into()),
                segment_ids: vec![format!("s{i}")],
                created_at: chrono::Utc::now(),
                extracted_by: "mock".into(),
            };
            storage
                .replace_meeting_items(
                    &meeting.id,
                    &[
                        item(ItemKind::Decision, format!("decision {word} {i}"), None),
                        item(
                            ItemKind::ActionItem,
                            format!("action {word} {i}"),
                            Some("open"),
                        ),
                    ],
                )
                .unwrap();
            made.push((note.id, meeting.id));
        }
        storage.set_note_keep_local(&made[1].0, true).unwrap();
        let (visible_note, visible_meeting) = made.remove(0);
        let h1 = made.remove(0);
        let h2 = made.remove(0);
        KeepLocalFixture {
            _dir: dir,
            server: Server::new(storage),
            visible_note,
            visible_meeting,
            hidden: [h1, h2],
        }
    }

    fn assert_no_leak(f: &KeepLocalFixture, text: &str) {
        assert!(
            !text.contains("secretword"),
            "keep-local content leaked: {text}"
        );
        for (note, meeting) in &f.hidden {
            assert!(
                !text.contains(note.as_str()),
                "hidden note id leaked: {text}"
            );
            assert!(
                !text.contains(meeting.as_str()),
                "hidden meeting id leaked: {text}"
            );
        }
    }

    #[test]
    fn keep_local_direct_lookups_are_refused() {
        let f = server_with_keep_local();
        for (note, meeting) in &f.hidden {
            for (tool, args) in [
                ("get_meeting", json!({"meeting_id": meeting})),
                ("get_transcript", json!({"meeting_id": meeting})),
                ("get_meeting_items", json!({"meeting_id": meeting})),
                (
                    "set_speaker_label",
                    json!({"meeting_id": meeting, "speaker_key": "spk_0", "label": "Dana"}),
                ),
            ] {
                let err = tool_error(&f.server, tool, args);
                assert_eq!(err, KEEP_LOCAL_MEETING_MESSAGE, "{tool}");
            }
            let err = tool_error(&f.server, "get_note", json!({"note_id": note}));
            assert_eq!(err, KEEP_LOCAL_NOTE_MESSAGE);
        }
        // the refused write really didn't happen
        let t = f
            .server
            .storage
            .get_transcript(&f.hidden[0].1)
            .unwrap()
            .unwrap();
        assert_eq!(t.speakers[0].label, "Speaker 1");

        // the shared meeting still works through the same tools
        let ok = tool_text(
            &f.server,
            "get_transcript",
            json!({"meeting_id": f.visible_meeting}),
        );
        assert!(ok.contains("renewal discussion shared"), "{ok}");
        tool_text(&f.server, "get_note", json!({"note_id": f.visible_note}));
        tool_text(
            &f.server,
            "get_meeting",
            json!({"meeting_id": f.visible_meeting}),
        );

        // batch: shared transcript rendered, hidden ones refused in place
        let batch = tool_text(
            &f.server,
            "get_transcripts",
            json!({"meeting_ids": [f.visible_meeting, f.hidden[0].1, f.hidden[1].1]}),
        );
        assert!(batch.contains("renewal discussion shared"), "{batch}");
        assert!(!batch.contains("secretword"), "{batch}");
        assert_eq!(
            batch.matches(KEEP_LOCAL_MEETING_MESSAGE).count(),
            2,
            "{batch}"
        );
    }

    #[test]
    fn keep_local_is_excluded_from_briefings_items_search_and_lists() {
        let f = server_with_keep_local();

        let ctx = tool_text(&f.server, "get_context", json!({"query": "acme renewal"}));
        assert!(ctx.contains("1 meeting(s)"), "{ctx}");
        assert!(ctx.contains(&f.visible_meeting), "{ctx}");
        assert!(ctx.contains("decision shared 0"), "{ctx}");
        assert_no_leak(&f, &ctx);
        // full-text fallback can't route to a hidden meeting either
        let fallback = tool_text(&f.server, "get_context", json!({"query": "secretword"}));
        // (the query itself is echoed back, so check for the miss + no ids)
        assert!(fallback.starts_with("No meetings matched"), "{fallback}");
        assert_no_leak(&f, &fallback.replace("\"secretword\"", ""));

        let changed = tool_text(
            &f.server,
            "whats_changed",
            json!({"series": "acme renewal", "since": "2020-01-01"}),
        );
        assert!(changed.contains("1 meeting(s)"), "{changed}");
        assert_no_leak(&f, &changed);

        for scope in [json!({}), json!({"scope": "acme renewal"})] {
            let open = tool_text(&f.server, "open_items", scope);
            assert!(open.contains("action shared 0"), "{open}");
            assert_no_leak(&f, &open);
        }
        for args in [json!({}), json!({"type": "decision"}), json!({"limit": 1})] {
            let items = tool_text(&f.server, "query_items", args);
            assert!(items.contains("shared 0"), "{items}");
            assert_no_leak(&f, &items);
        }

        let miss = tool_text(&f.server, "search_notes", json!({"query": "secretword"}));
        assert!(miss.contains("No matches"), "{miss}");

        // passage search: hidden meetings never surface, even when named
        let passages = tool_text(&f.server, "search_passages", json!({"query": "renewal"}));
        assert!(passages.contains(&f.visible_meeting), "{passages}");
        assert_no_leak(&f, &passages);
        let only_hidden = tool_text(&f.server, "search_passages", json!({"query": "secretword"}));
        let nothing = tool_text(&f.server, "search_passages", json!({"query": "nosuchword"}));
        assert_eq!(only_hidden, nothing);
        let scoped = tool_text(
            &f.server,
            "search_passages",
            json!({"query": "renewal", "meeting_ids": [f.hidden[0].1, f.hidden[1].1]}),
        );
        assert_eq!(scoped, nothing);
        // hidden hits rank alongside the shared one; a 1-hit page still
        // refills to the shared note
        for limit in [1, 20] {
            let hits = tool_text(
                &f.server,
                "search_notes",
                json!({"query": "renewal", "limit": limit}),
            );
            assert!(hits.contains(&f.visible_note), "{hits}");
            assert_no_leak(&f, &hits);
        }

        for limit in [1, 10] {
            let recent = tool_text(&f.server, "list_recent", json!({"limit": limit}));
            assert!(recent.contains(&f.visible_note), "{recent}");
            assert_no_leak(&f, &recent);
        }

        // keep-local folders (and inherited descendants) are not listed at all
        let folders = tool_text(&f.server, "list_folders", json!({}));
        assert!(
            !folders.contains("Legal") && !folders.contains("Cases"),
            "{folders}"
        );
    }

    /// A database the server can't open (here: not a SQLite file — the same
    /// path a too-old or too-new schema takes) must not kill the server:
    /// the handshake and tool list still answer, tool calls explain what to
    /// do, and the next call after the app fixes the database just works.
    #[test]
    fn lazy_server_serves_errors_until_the_database_opens() {
        let dir = tempfile::tempdir().unwrap();
        let db = dir.path().join("flyonthewall.db");
        std::fs::write(&db, b"definitely not sqlite, just garbage bytes here").unwrap();
        let lazy = LazyServer::new(dir.path().to_path_buf());
        assert!(!lazy.is_open());

        let call = |msg: serde_json::Value| -> serde_json::Value {
            serde_json::from_str(&lazy.handle_message(&msg.to_string()).unwrap()).unwrap()
        };
        let init = call(
            json!({"jsonrpc":"2.0","id":1,"method":"initialize","params":{"protocolVersion":"2025-06-18"}}),
        );
        assert_eq!(init["result"]["serverInfo"]["name"], SERVER_NAME);
        let tools = call(json!({"jsonrpc":"2.0","id":2,"method":"tools/list"}));
        assert!(tools["result"]["tools"].as_array().unwrap().len() > 5);
        assert!(lazy
            .handle_message(r#"{"jsonrpc":"2.0","method":"notifications/initialized"}"#)
            .is_none());
        let search = json!({"jsonrpc":"2.0","id":3,"method":"tools/call","params":{"name":"search_notes","arguments":{"query":"x"}}});
        let resp = call(search.clone());
        assert_eq!(resp["result"]["isError"], true, "{resp}");
        let text = resp["result"]["content"][0]["text"].as_str().unwrap();
        assert!(text.starts_with(DB_UNAVAILABLE_MESSAGE), "{text}");

        // the app opens and repairs its database; no client restart needed
        std::fs::remove_file(&db).unwrap();
        drop(Storage::open(dir.path()).unwrap());
        let resp = call(search);
        assert_ne!(resp["result"]["isError"], true, "{resp}");
        assert!(lazy.is_open());
    }

    #[test]
    fn list_folders_never_reveals_a_local_ancestor() {
        let dir = tempfile::tempdir().unwrap();
        let storage = Storage::open(dir.path()).unwrap();
        let work = storage.create_folder("Work", None).unwrap();
        let clients = storage.create_folder("Clients", Some(&work.id)).unwrap();
        let legal = storage.create_folder("Legal", None).unwrap();
        let cases = storage.create_folder("Cases", Some(&legal.id)).unwrap();
        storage.set_folder_keep_local(&legal.id, true).unwrap();
        let server = Server::new(storage);
        let text = tool_text(&server, "list_folders", json!({}));
        assert!(text.contains(&format!(
            "- Clients (id: {}, parent: {})",
            clients.id, work.id
        )));
        assert!(
            text.contains(&format!("- Work (id: {})", work.id)),
            "{text}"
        );
        for leak in ["Legal", "Cases", legal.id.as_str(), cases.id.as_str()] {
            assert!(!text.contains(leak), "{leak} leaked: {text}");
        }
    }

    /// 25 keep-local notes all newer than / ranking with one shared note.
    /// Hidden rows are excluded in SQL, so the shared note comes straight
    /// back, never more than `limit` hits, and no cursor names a hidden row.
    #[test]
    fn hidden_rows_never_shape_pages_or_cursors() {
        let dir = tempfile::tempdir().unwrap();
        let storage = Storage::open(dir.path()).unwrap();
        let shared = storage.create_note("zeta shared", None).unwrap();
        storage
            .update_note_scratchpad(&shared.id, "zeta topic")
            .unwrap();
        let legal = storage.create_folder("Legal", None).unwrap();
        storage.set_folder_keep_local(&legal.id, true).unwrap();
        let mut hidden_ids = Vec::new();
        for i in 0..25 {
            let n = storage
                .create_note(&format!("zeta hidden {i}"), Some(&legal.id))
                .unwrap();
            storage
                .update_note_scratchpad(&n.id, "zeta topic onlyhidden")
                .unwrap();
            hidden_ids.push(n.id);
        }
        let server = Server::new(storage);
        let no_hidden = |text: &str| {
            assert!(!text.contains("hidden"), "leaked: {text}");
            for id in &hidden_ids {
                assert!(!text.contains(id.as_str()), "hidden id leaked: {text}");
            }
        };
        for (tool, args) in [
            ("search_notes", json!({"query": "zeta", "limit": 1})),
            ("list_recent", json!({"limit": 1})),
        ] {
            let text = tool_text(&server, tool, args);
            no_hidden(&text);
            assert!(text.contains(&shared.id), "{tool}: {text}");
            assert_eq!(text.matches("note_id: ").count(), 1, "{tool}: {text}");
        }
        // A query only hidden notes match reads exactly like one nothing
        // matches: no cursor, no count, nothing to probe with.
        let only_hidden = tool_text(&server, "search_notes", json!({"query": "onlyhidden"}));
        let nothing = tool_text(&server, "search_notes", json!({"query": "nosuchword"}));
        assert_eq!(
            only_hidden.replace("onlyhidden", "Q"),
            nothing.replace("nosuchword", "Q")
        );
    }

    /// At most `limit` hits per page, and following the cursor visits every
    /// shareable note and transcript exactly once.
    #[test]
    fn search_pages_are_capped_and_complete() {
        let dir = tempfile::tempdir().unwrap();
        let storage = Storage::open(dir.path()).unwrap();
        let legal = storage.create_folder("Legal", None).unwrap();
        storage.set_folder_keep_local(&legal.id, true).unwrap();
        let mut want = std::collections::BTreeSet::new();
        for i in 0..12 {
            let folder = (i % 2 == 0).then_some(legal.id.as_str());
            let n = storage.create_note(&format!("omega {i}"), folder).unwrap();
            storage.update_note_scratchpad(&n.id, "omega").unwrap();
            let m = storage.create_meeting("omega", &n.id, &[]).unwrap();
            storage
                .save_transcript(&fly_core::Transcript {
                    meeting_id: m.id.clone(),
                    language: None,
                    engine: "t".into(),
                    segments: vec![fly_core::TranscriptSegment {
                        id: "s1".into(),
                        speaker_key: "mic".into(),
                        start_ms: 0,
                        end_ms: 1,
                        text: "omega".into(),
                        words: vec![],
                    }],
                    speakers: vec![],
                })
                .unwrap();
            if folder.is_none() {
                want.insert(format!("note:{}", n.id));
                want.insert(format!("transcript:{}", m.id));
            }
        }
        let server = Server::new(storage);
        let mut seen = std::collections::BTreeSet::new();
        let mut args = json!({"query": "omega", "limit": 4});
        for _ in 0..10 {
            let text = tool_text(&server, "search_notes", args.clone());
            let lines: Vec<&str> = text.lines().filter(|l| l.starts_with("- [")).collect();
            assert!(lines.len() <= 4, "{text}");
            for l in lines {
                let key = if l.starts_with("- [note]") {
                    let id = l
                        .split("note_id: ")
                        .nth(1)
                        .unwrap()
                        .split(')')
                        .next()
                        .unwrap();
                    format!("note:{id}")
                } else {
                    let id = l
                        .split("meeting_id: ")
                        .nth(1)
                        .unwrap()
                        .split(')')
                        .next()
                        .unwrap();
                    format!("transcript:{id}")
                };
                assert!(seen.insert(key.clone()), "repeated {key}: {text}");
            }
            match text.split("pass cursor: \"").nth(1) {
                Some(rest) => args["cursor"] = json!(rest.split('"').next().unwrap()),
                None => break,
            }
        }
        assert_eq!(seen, want);
    }

    #[test]
    fn orphaned_content_of_unknown_privacy_is_not_shared() {
        let f = server_with_keep_local();
        // transcript rows have no FK: simulate one left behind by a deleted
        // meeting — without its meeting row, keep-local can't be determined
        f.server
            .storage
            .save_transcript(&fly_core::Transcript {
                meeting_id: "ghost".into(),
                language: None,
                engine: "t".into(),
                segments: vec![],
                speakers: vec![],
            })
            .unwrap();
        let err = tool_error(&f.server, "get_transcript", json!({"meeting_id": "ghost"}));
        assert!(err.contains("not found"), "{err}");
    }
}
