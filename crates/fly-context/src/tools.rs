//! The six read functions, their argument types and JSON schemas.
//!
//! Schemas come in two forms from one definition:
//! * [`ToolDef::input_schema`] — plain JSON Schema: optional arguments are
//!   simply not `required`. Works with every provider, non-strict.
//! * [`ToolDef::strict_input_schema`] — the strict-tools form (OpenAI
//!   strict functions / Anthropic strict tools): every property required,
//!   optional ones nullable, objects closed. No numeric/length keywords,
//!   which strict modes reject; bounds live in descriptions and are
//!   clamped in code.
//!
//! Arguments deserialize from either form (`null` = absent) and reject
//! unknown fields, so a model inventing a parameter gets a clear error.

use std::collections::HashMap;

use chrono::{DateTime, NaiveDate, Utc};
use fly_core::{BlockOrigin, ItemKind, MeetingItem, Transcript};
use fly_storage::{ItemFilter, SearchFilter, SearchHitKind};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

use crate::passages::{self, PassageKey, Via, Window};
use crate::{Context, ContextError, QueryVector, Result};

/// Meetings (and notes) whose windows an unscoped keyword search scores,
/// picked by the IDF mass of the query terms each holds.
const KEYWORD_OWNERS: usize = 40;
/// Vector hits considered before fusion.
const VECTOR_CANDIDATES: usize = 200;
/// Passages per meeting/note in one result.
const PER_OWNER_CAP: usize = 3;
/// Passages kept per keyword-matched meeting before fusion.
const PER_MEETING_KEYWORD: usize = 3;
/// A keyword passage must hold at least this share of the query terms' IDF
/// mass (tuned on the retrieval gold set with the app's embedder): one
/// common word shared with a question is not a match.
const KEYWORD_MIN_COVERAGE: f32 = 0.4;
/// Longest accepted query.
pub const MAX_QUERY_CHARS: usize = 1_000;
/// Most meeting ids one call may scope to.
pub const MAX_MEETING_IDS: usize = 50;
/// Action-item statuses `get_items` accepts.
const STATUSES: &[&str] = &["open", "done"];
/// Every meeting (list_meetings has no offset; corpora are hundreds).
const ALL_ROWS: usize = 1_000_000;

// ---------------------------------------------------------------------------
// Tool definitions
// ---------------------------------------------------------------------------

/// A tool's contract, provider-neutral (the harness maps it onto
/// `fly_llm::ToolSpec`; this crate doesn't depend on fly-llm).
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct ToolDef {
    pub name: &'static str,
    pub description: &'static str,
    /// Plain schema (optional args not required).
    pub input_schema: Value,
    /// Strict-tools schema (all required, optional ones nullable, closed).
    pub strict_input_schema: Value,
}

/// Build both schema forms from `(name, type, description, required)`
/// property specs.
fn def(
    name: &'static str,
    description: &'static str,
    props: Vec<(&'static str, Value, &'static str, bool)>,
) -> ToolDef {
    let mut plain = serde_json::Map::new();
    let mut strict = serde_json::Map::new();
    let mut required = Vec::new();
    let mut all = Vec::new();
    for (prop, ty, desc, req) in props {
        let mut p = ty.clone();
        p["description"] = json!(desc);
        plain.insert(prop.into(), p.clone());
        if !req {
            p = nullable(p);
        }
        strict.insert(prop.into(), p);
        all.push(prop);
        if req {
            required.push(prop);
        }
    }
    ToolDef {
        name,
        description,
        input_schema: json!({
            "type": "object",
            "properties": plain,
            "required": required,
            "additionalProperties": false,
        }),
        strict_input_schema: json!({
            "type": "object",
            "properties": strict,
            "required": all,
            "additionalProperties": false,
        }),
    }
}

/// `{"type": T, …}` → `{"type": [T, "null"], …}`; an `enum` gains `null`
/// too (strict validators check the value against the enum).
fn nullable(mut p: Value) -> Value {
    if let Some(t) = p.get("type").and_then(|t| t.as_str()).map(str::to_string) {
        p["type"] = json!([t, "null"]);
    }
    if let Some(values) = p.get_mut("enum").and_then(|e| e.as_array_mut()) {
        values.push(Value::Null);
    }
    p
}

fn string() -> Value {
    json!({"type": "string"})
}
fn integer() -> Value {
    json!({"type": "integer"})
}
fn boolean() -> Value {
    json!({"type": "boolean"})
}
fn strings() -> Value {
    json!({"type": "array", "items": {"type": "string"}})
}
fn one_of(values: &[&str]) -> Value {
    json!({"type": "string", "enum": values})
}

pub const SEARCH_PASSAGES: &str = "search_passages";
pub const READ_SEGMENTS: &str = "read_segments";
pub const LIST_MEETINGS: &str = "list_meetings";
pub const GET_EVENT: &str = "get_event";
pub const GET_NOTE: &str = "get_note";
pub const GET_ITEMS: &str = "get_items";

/// Every tool, in a stable order.
pub fn tool_defs() -> Vec<ToolDef> {
    vec![
        def(
            SEARCH_PASSAGES,
            "Search the user's meetings for passages relevant to a question. Returns transcript \
             passages whose lines carry [seg:ID] markers (cite those ids), plus matching note \
             paragraphs. Use specific words from the question.",
            vec![
                ("query", string(), "What to look for, in plain words.", true),
                (
                    "meeting_ids",
                    strings(),
                    "Only search these meetings.",
                    false,
                ),
                ("folder_id", string(), "Only search this folder.", false),
                (
                    "since",
                    string(),
                    "Only meetings on/after this date (YYYY-MM-DD).",
                    false,
                ),
                (
                    "until",
                    string(),
                    "Only meetings on/before this date (YYYY-MM-DD).",
                    false,
                ),
                (
                    "limit",
                    integer(),
                    "Passages to return, 1-20 (default 8).",
                    false,
                ),
            ],
        ),
        def(
            READ_SEGMENTS,
            "Read the transcript lines around one segment, to see context before quoting it.",
            vec![
                (
                    "meeting_id",
                    string(),
                    "The meeting the segment belongs to.",
                    true,
                ),
                (
                    "around_segment_id",
                    string(),
                    "The segment id to center on.",
                    true,
                ),
                (
                    "before",
                    integer(),
                    "Lines before it, 0-20 (default 3).",
                    false,
                ),
                (
                    "after",
                    integer(),
                    "Lines after it, 0-20 (default 3).",
                    false,
                ),
                (
                    "prefer_polished",
                    boolean(),
                    "Use the cleaned-up transcript text when one exists (same segment ids).",
                    false,
                ),
            ],
        ),
        def(
            LIST_MEETINGS,
            "List the user's meetings, newest first, optionally filtered by date, folder, \
             title words or attendee.",
            vec![
                ("since", string(), "On/after this date (YYYY-MM-DD).", false),
                (
                    "until",
                    string(),
                    "On/before this date (YYYY-MM-DD).",
                    false,
                ),
                (
                    "folder_id",
                    string(),
                    "Only meetings filed in this folder.",
                    false,
                ),
                (
                    "title_contains",
                    string(),
                    "Words that must appear in the title.",
                    false,
                ),
                (
                    "attendee",
                    string(),
                    "Part of an attendee's name or email.",
                    false,
                ),
                (
                    "limit",
                    integer(),
                    "Meetings to return, 1-50 (default 20).",
                    false,
                ),
            ],
        ),
        def(
            GET_EVENT,
            "Get one meeting's details: title, time, attendees, speakers and how many \
             decisions/action items were extracted.",
            vec![("meeting_id", string(), "The meeting id.", true)],
        ),
        def(
            GET_NOTE,
            "Get the user's own notes for a meeting or note: the scratchpad they typed, the \
             enhanced notes, or both. Pass note_id or meeting_id.",
            vec![
                ("note_id", string(), "The note id.", false),
                (
                    "meeting_id",
                    string(),
                    "A meeting id (its note is returned).",
                    false,
                ),
                (
                    "kind",
                    one_of(&["scratchpad", "enhanced", "both"]),
                    "Which part (default both).",
                    false,
                ),
            ],
        ),
        def(
            GET_ITEMS,
            "List decisions, action items, open questions, commitments and key figures \
             extracted from meetings. Each item cites its transcript segment ids.",
            vec![
                ("meeting_ids", strings(), "Only these meetings.", false),
                (
                    "kind",
                    one_of(&[
                        "decision",
                        "action_item",
                        "question",
                        "commitment",
                        "figure",
                    ]),
                    "Only this kind.",
                    false,
                ),
                ("owner", string(), "Part of the owner's name.", false),
                (
                    "status",
                    one_of(&["open", "done"]),
                    "Action item status.",
                    false,
                ),
                (
                    "since",
                    string(),
                    "From meetings on/after this date (YYYY-MM-DD).",
                    false,
                ),
                (
                    "limit",
                    integer(),
                    "Items to return, 1-200 (default 50).",
                    false,
                ),
            ],
        ),
    ]
}

// ---------------------------------------------------------------------------
// Arguments
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Default, PartialEq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SearchPassagesArgs {
    pub query: String,
    #[serde(default)]
    pub meeting_ids: Option<Vec<String>>,
    #[serde(default)]
    pub folder_id: Option<String>,
    #[serde(default)]
    pub since: Option<String>,
    #[serde(default)]
    pub until: Option<String>,
    #[serde(default)]
    pub limit: Option<u32>,
}

#[derive(Debug, Clone, Default, PartialEq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ReadSegmentsArgs {
    pub meeting_id: String,
    pub around_segment_id: String,
    #[serde(default)]
    pub before: Option<u32>,
    #[serde(default)]
    pub after: Option<u32>,
    #[serde(default)]
    pub prefer_polished: Option<bool>,
}

#[derive(Debug, Clone, Default, PartialEq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ListMeetingsArgs {
    #[serde(default)]
    pub since: Option<String>,
    #[serde(default)]
    pub until: Option<String>,
    #[serde(default)]
    pub folder_id: Option<String>,
    #[serde(default)]
    pub title_contains: Option<String>,
    #[serde(default)]
    pub attendee: Option<String>,
    #[serde(default)]
    pub limit: Option<u32>,
}

#[derive(Debug, Clone, Default, PartialEq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct GetEventArgs {
    pub meeting_id: String,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum NoteKind {
    Scratchpad,
    Enhanced,
    #[default]
    Both,
}

#[derive(Debug, Clone, Default, PartialEq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct GetNoteArgs {
    #[serde(default)]
    pub note_id: Option<String>,
    #[serde(default)]
    pub meeting_id: Option<String>,
    #[serde(default)]
    pub kind: Option<NoteKind>,
}

#[derive(Debug, Clone, Default, PartialEq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct GetItemsArgs {
    #[serde(default)]
    pub meeting_ids: Option<Vec<String>>,
    #[serde(default)]
    pub kind: Option<String>,
    #[serde(default)]
    pub owner: Option<String>,
    #[serde(default)]
    pub status: Option<String>,
    #[serde(default)]
    pub since: Option<String>,
    #[serde(default)]
    pub limit: Option<u32>,
}

/// `YYYY-MM-DD` (start or end of that UTC day) or RFC 3339.
fn parse_when(field: &str, s: &Option<String>, end_of_day: bool) -> Result<Option<DateTime<Utc>>> {
    let Some(s) = s.as_deref().map(str::trim).filter(|s| !s.is_empty()) else {
        return Ok(None);
    };
    if let Ok(t) = DateTime::parse_from_rfc3339(s) {
        return Ok(Some(t.with_timezone(&Utc)));
    }
    let d = NaiveDate::parse_from_str(s, "%Y-%m-%d").map_err(|_| {
        ContextError::InvalidArgs(format!("{field} must be a date like 2026-10-27"))
    })?;
    let t = if end_of_day {
        d.and_hms_milli_opt(23, 59, 59, 999)
    } else {
        d.and_hms_opt(0, 0, 0)
    }
    .expect("valid time of day");
    Ok(Some(t.and_utc()))
}

fn clamp(v: Option<u32>, default: u32, min: u32, max: u32) -> usize {
    v.unwrap_or(default).clamp(min, max) as usize
}

fn non_empty(s: &Option<String>) -> Option<&str> {
    s.as_deref().map(str::trim).filter(|s| !s.is_empty())
}

// ---------------------------------------------------------------------------
// Results
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct SegmentView {
    pub id: String,
    /// Display label (relabelable — cite the id, not the name).
    pub speaker: String,
    pub speaker_key: String,
    pub start_ms: u64,
    pub end_ms: u64,
    pub text: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum PassageSource {
    Transcript,
    Note,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Passage {
    pub source: PassageSource,
    pub meeting_id: Option<String>,
    pub note_id: String,
    pub title: String,
    pub started_at: Option<DateTime<Utc>>,
    pub start_ms: Option<u64>,
    pub end_ms: Option<u64>,
    /// Transcript passages: every line, citable by id. Empty for notes.
    pub segments: Vec<SegmentView>,
    /// Note passages: the paragraph text. Transcript passages: the lines
    /// joined as `Speaker: text`.
    pub text: String,
    /// Fused RRF score (higher = more relevant).
    pub score: f32,
    /// "keyword" and/or "semantic".
    pub matched_by: Vec<&'static str>,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct PassageResults {
    pub passages: Vec<Passage>,
    /// Semantic search ran (a query vector was supplied).
    pub semantic: bool,
    /// Per passage, in the same order: the cosine similarity of the best
    /// semantic hit that landed on it, or `None` when only keywords found
    /// it. (`Passage::score` is the rank-based fused score.)
    pub similarity: Vec<Option<f32>>,
    /// The best cosine similarity of any chunk in scope, whether or not it
    /// reached `similarity_floor`. At or under the floor, nothing in the
    /// library is semantically close to the query. `None` without
    /// semantic search.
    pub top_similarity: Option<f32>,
    /// The cosine a semantic hit needed to count (per embedding model;
    /// `None` = no floor for this model).
    pub similarity_floor: Option<f32>,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct SegmentWindow {
    pub meeting_id: String,
    pub title: String,
    pub polished: bool,
    pub segments: Vec<SegmentView>,
    pub more_before: bool,
    pub more_after: bool,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct MeetingSummary {
    pub meeting_id: String,
    pub note_id: String,
    pub title: String,
    pub started_at: DateTime<Utc>,
    pub ended_at: Option<DateTime<Utc>>,
    pub attendees: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct MeetingList {
    pub meetings: Vec<MeetingSummary>,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct AttendeeView {
    pub name: String,
    pub email: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct SpeakerView {
    pub key: String,
    pub label: String,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct EventInfo {
    pub meeting_id: String,
    pub note_id: String,
    pub title: String,
    pub started_at: DateTime<Utc>,
    pub ended_at: Option<DateTime<Utc>>,
    pub duration_ms: Option<u64>,
    pub attendees: Vec<AttendeeView>,
    /// False for calendar-seeded rosters the user hasn't confirmed.
    pub attendees_confirmed: bool,
    pub speakers: Vec<SpeakerView>,
    pub segment_count: usize,
    pub has_polished_transcript: bool,
    /// Extracted items per kind (`decision`, `action_item`, …).
    pub item_counts: HashMap<String, usize>,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct BlockView {
    pub id: String,
    /// "user" | "ai"
    pub origin: &'static str,
    pub markdown: String,
    /// AI blocks: transcript segments it was written from.
    pub source_segment_ids: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct NoteView {
    pub note_id: String,
    pub title: String,
    pub folder_id: Option<String>,
    pub meeting_id: Option<String>,
    pub scratchpad: Option<String>,
    pub enhanced: Option<Vec<BlockView>>,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct ItemList {
    pub items: Vec<MeetingItem>,
}

/// A tool result in both shapes: JSON (MCP, run logs) and text (model).
#[derive(Debug, Clone, PartialEq)]
pub struct ToolOutput {
    pub json: Value,
    pub text: String,
}

// ---------------------------------------------------------------------------
// Implementations
// ---------------------------------------------------------------------------

/// Per-call cache: meetings + transcripts + windows, loaded once.
struct Loaded {
    meeting: fly_core::Meeting,
    transcript: Transcript,
    windows: Vec<Window>,
}

fn segment_view(t: &Transcript, i: usize) -> SegmentView {
    let s = &t.segments[i];
    SegmentView {
        id: s.id.clone(),
        speaker: t.label_for(&s.speaker_key),
        speaker_key: s.speaker_key.clone(),
        start_ms: s.start_ms,
        end_ms: s.end_ms,
        text: s.text.trim().to_string(),
    }
}

impl Context<'_> {
    fn load(&self, cache: &mut HashMap<String, Option<Loaded>>, meeting_id: &str) -> Result<bool> {
        if !cache.contains_key(meeting_id) {
            let loaded = match self.storage().get_meeting(meeting_id) {
                Ok(meeting) => self.storage().get_transcript(meeting_id)?.map(|t| Loaded {
                    windows: passages::transcript_windows(&t),
                    transcript: t,
                    meeting,
                }),
                Err(fly_storage::StorageError::NotFound(_)) => None,
                Err(e) => return Err(e.into()),
            };
            cache.insert(meeting_id.to_string(), loaded);
        }
        Ok(cache[meeting_id].is_some())
    }

    /// Hybrid passage search: keyword (IDF-weighted term matches, localized
    /// to windows) + semantic (vector hits above the model's similarity
    /// floor, mapped onto windows), fused by RRF with a per-meeting cap.
    pub fn search_passages(
        &self,
        args: &SearchPassagesArgs,
        query_vector: Option<QueryVector<'_>>,
    ) -> Result<PassageResults> {
        let query = args.query.trim();
        if query.is_empty() {
            return Err(ContextError::InvalidArgs("query must not be empty".into()));
        }
        if query.chars().count() > MAX_QUERY_CHARS {
            return Err(ContextError::InvalidArgs(format!(
                "query is longer than {MAX_QUERY_CHARS} characters"
            )));
        }
        let limit = clamp(args.limit, 8, 1, 20);
        let since = parse_when("since", &args.since, false)?;
        let until = parse_when("until", &args.until, true)?;
        let hidden = self.hidden()?;
        let terms = passages::query_terms(query);
        let filter = SearchFilter {
            folder_id: non_empty(&args.folder_id).map(str::to_string),
            since,
            until,
            limit: VECTOR_CANDIDATES,
            offset: 0,
            exclude_keep_local: self.shareable_only(),
        };
        let scope: Option<Vec<String>> = match &args.meeting_ids {
            Some(ids) => {
                check_meeting_ids(ids)?;
                let mut v: Vec<String> = ids.iter().map(|s| s.trim().to_string()).collect();
                v.dedup();
                Some(v)
            }
            None => None,
        };

        let mut cache: HashMap<String, Option<Loaded>> = HashMap::new();
        let mut notes: HashMap<String, fly_core::Note> = HashMap::new();
        let mut keyword_t: Vec<PassageKey> = Vec::new();
        let mut keyword_n: Vec<PassageKey> = Vec::new();

        // Keyword lists. Each window is scored by the IDF mass of the query
        // terms it holds (its speaker labels and the meeting title count:
        // "what did Dana say about the launch" should find Dana's lines in
        // the launch meeting) and kept only above KEYWORD_MIN_COVERAGE.
        if !terms.is_empty() {
            let stats = self.storage().segment_term_stats(&terms, &filter)?;
            let idf = stats.idf();
            // A word the library has never seen counts fully (it weighs
            // the most): a question about "the Berlin lease" isn't
            // answered by a window that only shares "lease".
            let total: f32 = idf.iter().sum();
            let covers = |s: f32| s > 0.0 && total > 0.0 && s / total >= KEYWORD_MIN_COVERAGE;

            let evidence = owner_evidence(&stats, &idf);
            let meeting_order: Vec<String> = match &scope {
                Some(ids) => ids.clone(),
                None => top_owners(&evidence, KEYWORD_OWNERS),
            };
            // (score, own-text score, meeting evidence, started_at, title,
            // window, key): the title counts toward eligibility and across
            // meetings; the window's own words decide between windows of
            // one meeting; remaining ties go to the meeting with more
            // keyword evidence overall, then the newer one. Content only,
            // never the (random) ids, so a search ranks the same way every
            // time.
            type Scored = (
                f32,
                f32,
                (f32, usize),
                DateTime<Utc>,
                String,
                usize,
                PassageKey,
            );
            let mut scored: Vec<Scored> = Vec::new();
            for mid in &meeting_order {
                if !self.load(&mut cache, mid)? {
                    continue;
                }
                let l = cache[mid].as_ref().expect("loaded");
                if hidden.meeting(&l.meeting.id, &l.meeting.note_id) {
                    continue; // defense in depth: the term stats already excluded these
                }
                if scope.is_some()
                    && (since.is_some_and(|s| l.meeting.started_at < s)
                        || until.is_some_and(|u| l.meeting.started_at > u))
                {
                    continue;
                }
                let title = l.meeting.title.as_str();
                let mut best: Vec<(f32, f32, usize)> = l
                    .windows
                    .iter()
                    .enumerate()
                    .map(|(i, w)| {
                        let text = passages::window_text(&l.transcript, w);
                        let own = passages::weighted_match(&terms, &idf, &[(&text, 1.0)]);
                        let texts = [(text.as_str(), 1.0), (title, 1.0)];
                        (passages::weighted_match(&terms, &idf, &texts), own, i)
                    })
                    .filter(|(s, _, _)| covers(*s))
                    .collect();
                best.sort_by(|a, b| {
                    b.0.total_cmp(&a.0)
                        .then(b.1.total_cmp(&a.1))
                        .then(a.2.cmp(&b.2))
                });
                let meeting_evidence = evidence.get(mid).copied().unwrap_or_default();
                for (s, own, i) in best.into_iter().take(PER_MEETING_KEYWORD) {
                    scored.push((
                        s,
                        own,
                        meeting_evidence,
                        l.meeting.started_at,
                        l.meeting.title.clone(),
                        i,
                        PassageKey::Transcript(mid.clone(), i),
                    ));
                }
            }
            scored.sort_by(|a, b| {
                b.0.total_cmp(&a.0)
                    .then(b.1.total_cmp(&a.1))
                    .then(b.2 .0.total_cmp(&a.2 .0))
                    .then(b.2 .1.cmp(&a.2 .1))
                    .then(b.3.cmp(&a.3))
                    .then(a.4.cmp(&b.4))
                    .then(a.5.cmp(&b.5))
            });
            keyword_t.extend(scored.into_iter().map(|(.., k)| k));

            let note_order: Vec<String> = match &scope {
                Some(ids) => ids
                    .iter()
                    .filter_map(|id| self.storage().get_meeting(id).ok().map(|m| m.note_id))
                    .collect(),
                None => {
                    let note_stats = self.storage().note_term_stats(&terms, &filter)?;
                    top_owners(&owner_evidence(&note_stats, &idf), KEYWORD_OWNERS)
                }
            };
            // (score, own-text score, updated_at, title, key)
            let mut scored: Vec<(f32, f32, DateTime<Utc>, String, PassageKey)> = Vec::new();
            for nid in &note_order {
                if hidden.note(nid) {
                    continue;
                }
                let note = match self.storage().get_note(nid) {
                    Ok(n) => n,
                    Err(fly_storage::StorageError::NotFound(_)) => continue,
                    Err(e) => return Err(e.into()),
                };
                let title = note.title.as_str();
                let chunks = fly_storage::embeddings::chunk_note(&note);
                let best = chunks
                    .iter()
                    .enumerate()
                    .map(|(i, c)| {
                        let own = passages::weighted_match(&terms, &idf, &[(&c.text, 1.0)]);
                        let texts = [(c.text.as_str(), 1.0), (title, 1.0)];
                        (passages::weighted_match(&terms, &idf, &texts), own, i)
                    })
                    .filter(|(s, _, _)| covers(*s))
                    .max_by(|a, b| {
                        a.0.total_cmp(&b.0)
                            .then(a.1.total_cmp(&b.1))
                            .then(b.2.cmp(&a.2))
                    });
                if let Some((s, own, i)) = best {
                    scored.push((
                        s,
                        own,
                        note.updated_at,
                        note.title.clone(),
                        PassageKey::Note(nid.clone(), i),
                    ));
                }
                notes.insert(nid.clone(), note);
            }
            scored.sort_by(|a, b| {
                b.0.total_cmp(&a.0)
                    .then(b.1.total_cmp(&a.1))
                    .then(b.2.cmp(&a.2))
                    .then(a.3.cmp(&b.3))
            });
            keyword_n.extend(scored.into_iter().map(|(.., k)| k));
        }

        // Semantic list: hits at or above the model's similarity floor.
        let mut semantic: Vec<PassageKey> = Vec::new();
        let mut similarity: HashMap<PassageKey, f32> = HashMap::new();
        let mut top_similarity: Option<f32> = None;
        let floor = query_vector.and_then(|qv| fly_storage::embeddings::similarity_floor(qv.model));
        if let Some(qv) = query_vector {
            // vector_search filters keep-local (and orphans) inside SQL,
            // before its limit and floor, so hidden chunks can't crowd the
            // pool. A meeting scope isn't expressible there, so a scoped
            // search asks for every hit and applies the scope and the cap
            // itself (truncating first would let out-of-scope chunks crowd
            // in-scope ones out). It applies no floor: an explicit meeting
            // scope is the question's subject, so its best passages count,
            // close or not.
            let found = if scope.is_some() {
                self.storage()
                    .vector_search_floor(qv.vector, qv.model, &filter, ALL_ROWS, None)?
            } else {
                let r = self.storage().vector_search_floor(
                    qv.vector,
                    qv.model,
                    &filter,
                    VECTOR_CANDIDATES,
                    floor,
                )?;
                top_similarity = r.best_score;
                r
            };
            for h in found.hits {
                if semantic.len() >= VECTOR_CANDIDATES {
                    break;
                }
                // Defense in depth over the in-SQL filter: the by-id sets
                // fail closed, and an orphan chunk can't be checked.
                if self.shareable_only() && (h.note_id.is_empty() || hidden.note(&h.note_id)) {
                    continue;
                }
                let key = match h.kind {
                    SearchHitKind::Transcript => {
                        let Some(mid) = h.meeting_id.clone() else {
                            continue;
                        };
                        if scope.as_ref().is_some_and(|s| !s.contains(&mid)) {
                            continue;
                        }
                        if hidden.meeting(&mid, &h.note_id) {
                            continue;
                        }
                        if scope.is_some() {
                            top_similarity =
                                Some(top_similarity.map_or(h.score, |b| b.max(h.score)));
                        }
                        let anchor = match h.segment_ids.first() {
                            Some(id) => id.clone(),
                            None => format!("@{}", h.start_ms.unwrap_or(0)),
                        };
                        match cache.get(&mid) {
                            // Loaded by the keyword pass: map it now, so a
                            // window both lists found fuses into one.
                            Some(Some(l)) => {
                                let Some(i) =
                                    passages::resolve_anchor(&l.transcript, &l.windows, &anchor)
                                else {
                                    continue;
                                };
                                PassageKey::Transcript(mid, i)
                            }
                            Some(None) => continue,
                            None => PassageKey::Chunk(mid, anchor),
                        }
                    }
                    SearchHitKind::Note => {
                        if scope.is_some() {
                            continue;
                        }
                        let nid = h.note_id.clone();
                        if !notes.contains_key(&nid) {
                            match self.storage().get_note(&nid) {
                                Ok(n) => {
                                    notes.insert(nid.clone(), n);
                                }
                                Err(fly_storage::StorageError::NotFound(_)) => continue,
                                Err(e) => return Err(e.into()),
                            }
                        }
                        let chunks = fly_storage::embeddings::chunk_note(&notes[&nid]);
                        let head = flat_prefix(&h.snippet);
                        let Some(i) = chunks.iter().position(|c| flat(&c.text).starts_with(&head))
                        else {
                            continue;
                        };
                        PassageKey::Note(nid, i)
                    }
                };
                let best = similarity.entry(key.clone()).or_insert(h.score);
                *best = best.max(h.score);
                semantic.push(key);
            }
        }

        // Over-fetch: a Chunk key can turn out to be a duplicate of another
        // (two chunks on one window) or unloadable once resolved.
        let fused = passages::fuse(
            &[
                (Via::Keyword, keyword_t),
                (Via::Keyword, keyword_n),
                (Via::Semantic, semantic),
            ],
            PER_OWNER_CAP,
            limit * 2,
        );

        let mut out = Vec::with_capacity(limit);
        let mut sims = Vec::with_capacity(limit);
        let mut seen: std::collections::HashSet<PassageKey> = std::collections::HashSet::new();
        for (key, score, via) in fused {
            if out.len() >= limit {
                break;
            }
            let sim = similarity.get(&key).copied();
            let key = match key {
                PassageKey::Chunk(mid, anchor) => {
                    if !self.load(&mut cache, &mid)? {
                        continue;
                    }
                    let l = cache[&mid].as_ref().expect("loaded");
                    if hidden.meeting(&l.meeting.id, &l.meeting.note_id) {
                        continue;
                    }
                    match passages::resolve_anchor(&l.transcript, &l.windows, &anchor) {
                        Some(i) => PassageKey::Transcript(mid, i),
                        None => continue,
                    }
                }
                k => k,
            };
            if !seen.insert(key.clone()) {
                continue;
            }
            sims.push(sim);
            let matched_by = via
                .iter()
                .map(|v| match v {
                    Via::Keyword => "keyword",
                    Via::Semantic => "semantic",
                })
                .collect();
            match key {
                PassageKey::Transcript(mid, i) => {
                    let l = cache[&mid].as_ref().expect("loaded");
                    let w = &l.windows[i];
                    out.push(Passage {
                        source: PassageSource::Transcript,
                        meeting_id: Some(mid.clone()),
                        note_id: l.meeting.note_id.clone(),
                        title: l.meeting.title.clone(),
                        started_at: Some(l.meeting.started_at),
                        start_ms: Some(w.start_ms),
                        end_ms: Some(w.end_ms),
                        segments: w
                            .segments
                            .iter()
                            .map(|&s| segment_view(&l.transcript, s))
                            .collect(),
                        text: passages::window_text(&l.transcript, w),
                        score,
                        matched_by,
                    });
                }
                PassageKey::Chunk(..) => unreachable!("resolved above"),
                PassageKey::Note(nid, i) => {
                    let note = &notes[&nid];
                    let chunks = fly_storage::embeddings::chunk_note(note);
                    out.push(Passage {
                        source: PassageSource::Note,
                        meeting_id: note.meeting_id.clone(),
                        note_id: nid.clone(),
                        title: note.title.clone(),
                        started_at: None,
                        start_ms: None,
                        end_ms: None,
                        segments: vec![],
                        text: chunks[i].text.clone(),
                        score,
                        matched_by,
                    });
                }
            }
        }
        Ok(PassageResults {
            passages: out,
            semantic: query_vector.is_some(),
            similarity: sims,
            top_similarity,
            // A meeting-scoped search applies no floor (see above).
            similarity_floor: if scope.is_some() { None } else { floor },
        })
    }

    /// The lines around one segment (by id, not time — ids survive
    /// re-diarization and relabeling).
    pub fn read_segments(&self, args: &ReadSegmentsArgs) -> Result<SegmentWindow> {
        let meeting = self.meeting(args.meeting_id.trim())?;
        self.guard_meeting(&meeting)?;
        let raw = self.storage().get_transcript(&meeting.id)?.ok_or_else(|| {
            ContextError::NotFound(format!("transcript for meeting {}", meeting.id))
        })?;
        let polished = if args.prefer_polished.unwrap_or(false) {
            self.storage().get_cleaned_transcript(&meeting.id)?
        } else {
            None
        };
        let is_polished = polished.is_some();
        let t = polished.unwrap_or(raw);
        let target = args.around_segment_id.trim();
        let i = t
            .segments
            .iter()
            .position(|s| s.id == target)
            .ok_or_else(|| {
                ContextError::NotFound(format!(
                    "segment {target} in meeting {} (use an id from a [seg:ID] marker)",
                    meeting.id
                ))
            })?;
        let before = clamp(args.before, 3, 0, 20);
        let after = clamp(args.after, 3, 0, 20);
        let lo = i.saturating_sub(before);
        let hi = (i + after + 1).min(t.segments.len());
        Ok(SegmentWindow {
            meeting_id: meeting.id.clone(),
            title: meeting.title.clone(),
            polished: is_polished,
            segments: (lo..hi)
                .filter(|&j| !t.segments[j].text.trim().is_empty())
                .map(|j| segment_view(&t, j))
                .collect(),
            more_before: lo > 0,
            more_after: hi < t.segments.len(),
        })
    }

    pub fn list_meetings(&self, args: &ListMeetingsArgs) -> Result<MeetingList> {
        let since = parse_when("since", &args.since, false)?;
        let until = parse_when("until", &args.until, true)?;
        let limit = clamp(args.limit, 20, 1, 50);
        let hidden = self.hidden()?;
        let title_words = non_empty(&args.title_contains).map(passages::query_terms);
        let attendee = non_empty(&args.attendee).map(str::to_lowercase);
        let folder = non_empty(&args.folder_id);
        let mut meetings = Vec::new();
        // Dates, folder and keep-local filter inside SQL; title/attendee
        // matching happens here, so every remaining row is read.
        let rows = self
            .storage()
            .list_meetings_filtered(&fly_storage::MeetingFilter {
                since,
                until,
                folder_id: folder.map(str::to_string),
                exclude_keep_local: self.shareable_only(),
                limit: ALL_ROWS,
            })?;
        for m in rows {
            if meetings.len() >= limit {
                break;
            }
            // Defense in depth over the in-SQL filter (fails closed).
            if hidden.meeting(&m.id, &m.note_id) {
                continue;
            }
            if let Some(words) = &title_words {
                let title = m.title.to_lowercase();
                if !words.iter().all(|w| title.contains(w.as_str())) {
                    continue;
                }
            }
            if let Some(a) = &attendee {
                let hit = m.attendees.iter().any(|x| {
                    x.name.to_lowercase().contains(a.as_str())
                        || x.email
                            .as_deref()
                            .is_some_and(|e| e.to_lowercase().contains(a.as_str()))
                });
                if !hit {
                    continue;
                }
            }
            meetings.push(MeetingSummary {
                attendees: m.attendee_names(),
                meeting_id: m.id,
                note_id: m.note_id,
                title: m.title,
                started_at: m.started_at,
                ended_at: m.ended_at,
            });
        }
        Ok(MeetingList { meetings })
    }

    pub fn get_event(&self, args: &GetEventArgs) -> Result<EventInfo> {
        let m = self.meeting(args.meeting_id.trim())?;
        self.guard_meeting(&m)?;
        let t = self.storage().get_transcript(&m.id)?;
        let mut item_counts: HashMap<String, usize> = HashMap::new();
        for item in self.storage().get_meeting_items(&m.id)? {
            *item_counts
                .entry(item.kind.as_str().to_string())
                .or_default() += 1;
        }
        let duration_ms = m
            .recording
            .as_ref()
            .map(|r| r.duration_ms)
            .filter(|d| *d > 0)
            .or_else(|| {
                m.ended_at
                    .map(|e| (e - m.started_at).num_milliseconds().max(0) as u64)
            });
        Ok(EventInfo {
            has_polished_transcript: t.is_some()
                && self.storage().get_cleaned_transcript(&m.id)?.is_some(),
            speakers: t
                .as_ref()
                .map(|t| {
                    t.speakers
                        .iter()
                        .map(|s| SpeakerView {
                            key: s.key.clone(),
                            label: s.label.clone(),
                        })
                        .collect()
                })
                .unwrap_or_default(),
            segment_count: t.as_ref().map(|t| t.segments.len()).unwrap_or(0),
            attendees: m
                .attendees
                .iter()
                .map(|a| AttendeeView {
                    name: a.display_name().to_string(),
                    email: a.email.clone(),
                })
                .collect(),
            attendees_confirmed: m.attendees_confirmed,
            meeting_id: m.id,
            note_id: m.note_id,
            title: m.title,
            started_at: m.started_at,
            ended_at: m.ended_at,
            duration_ms,
            item_counts,
        })
    }

    pub fn get_note(&self, args: &GetNoteArgs) -> Result<NoteView> {
        let note_id = match (non_empty(&args.note_id), non_empty(&args.meeting_id)) {
            (Some(n), None) => n.to_string(),
            (None, Some(mid)) => {
                let m = self.meeting(mid)?;
                self.guard_meeting(&m)?;
                m.note_id
            }
            _ => {
                return Err(ContextError::InvalidArgs(
                    "pass exactly one of note_id or meeting_id".into(),
                ))
            }
        };
        self.guard_note(&note_id)?;
        let note = match self.storage().get_note(&note_id) {
            Ok(n) => n,
            Err(fly_storage::StorageError::NotFound(_)) => {
                return Err(ContextError::NotFound(format!("note {note_id}")))
            }
            Err(e) => return Err(e.into()),
        };
        let kind = args.kind.unwrap_or_default();
        let want_scratch = kind != NoteKind::Enhanced;
        let want_enhanced = kind != NoteKind::Scratchpad;
        Ok(NoteView {
            scratchpad: want_scratch.then(|| note.scratchpad.clone()),
            enhanced: want_enhanced.then(|| {
                note.blocks
                    .iter()
                    .map(|b| BlockView {
                        id: b.id.clone(),
                        origin: match b.origin {
                            BlockOrigin::User => "user",
                            BlockOrigin::Ai { .. } => "ai",
                        },
                        markdown: b.markdown.clone(),
                        source_segment_ids: match &b.origin {
                            BlockOrigin::Ai { source_segment_ids } => source_segment_ids.clone(),
                            BlockOrigin::User => vec![],
                        },
                    })
                    .collect()
            }),
            note_id: note.id,
            title: note.title,
            folder_id: note.folder_id,
            meeting_id: note.meeting_id,
        })
    }

    pub fn get_items(&self, args: &GetItemsArgs) -> Result<ItemList> {
        let kind = match non_empty(&args.kind) {
            Some(k) => Some(ItemKind::parse(k).ok_or_else(|| {
                ContextError::InvalidArgs(format!(
                    "kind must be one of decision, action_item, question, commitment, figure (got {k:?})"
                ))
            })?),
            None => None,
        };
        let status = non_empty(&args.status);
        if let Some(s) = status {
            if !STATUSES.contains(&s) {
                return Err(ContextError::InvalidArgs(format!(
                    "status must be one of open, done (got {s:?})"
                )));
            }
        }
        if let Some(ids) = &args.meeting_ids {
            check_meeting_ids(ids)?;
        }
        let items = self.storage().query_items(&ItemFilter {
            kind,
            owner: non_empty(&args.owner).map(str::to_string),
            status: status.map(str::to_string),
            since: parse_when("since", &args.since, false)?,
            meeting_ids: args.meeting_ids.clone(),
            limit: clamp(args.limit, 50, 1, 200),
            exclude_keep_local: self.shareable_only(),
        })?;
        Ok(ItemList { items })
    }
}

/// The `n` owners (meetings or notes) holding the most IDF mass of the
/// query terms, best first; ties go to more term occurrences, then id.
/// Each owner's (meeting's or note's) keyword evidence: the IDF mass of
/// the query terms it holds anywhere, and how many of its rows hold them.
fn owner_evidence(
    stats: &fly_storage::query::TermStats,
    idf: &[f32],
) -> HashMap<String, (f32, usize)> {
    let mut evidence: HashMap<String, (f32, usize)> = HashMap::new();
    for (term, w) in stats.terms.iter().zip(idf) {
        for (owner, count) in &term.owners {
            let e = evidence.entry(owner.clone()).or_default();
            e.0 += w;
            e.1 += count;
        }
    }
    evidence
}

/// The `n` owners with the most evidence, best first (ties: more term
/// occurrences, then id; the cut only matters past `n` owners).
fn top_owners(evidence: &HashMap<String, (f32, usize)>, n: usize) -> Vec<String> {
    let mut ranked: Vec<(&String, &(f32, usize))> = evidence.iter().collect();
    ranked.sort_by(|a, b| {
        b.1 .0
            .total_cmp(&a.1 .0)
            .then(b.1 .1.cmp(&a.1 .1))
            .then(a.0.cmp(b.0))
    });
    ranked.into_iter().take(n).map(|(o, _)| o.clone()).collect()
}

fn check_meeting_ids(ids: &[String]) -> Result<()> {
    if ids.len() > MAX_MEETING_IDS {
        return Err(ContextError::InvalidArgs(format!(
            "meeting_ids may list at most {MAX_MEETING_IDS} meetings (got {})",
            ids.len()
        )));
    }
    Ok(())
}

/// Whitespace-collapsed text.
fn flat(s: &str) -> String {
    s.split_whitespace().collect::<Vec<_>>().join(" ")
}

/// A vector-hit snippet's comparable head (its trailing " …" removed).
fn flat_prefix(snippet: &str) -> String {
    let f = flat(snippet);
    f.strip_suffix(" …").unwrap_or(&f).to_string()
}

// ---------------------------------------------------------------------------
// Dispatch
// ---------------------------------------------------------------------------

fn parse_args<T: serde::de::DeserializeOwned>(tool: &str, args: &Value) -> Result<T> {
    // Strict-mode callers send `null` for absent optionals; a model with no
    // arguments may send nothing at all.
    let args = if args.is_null() {
        json!({})
    } else {
        args.clone()
    };
    serde_json::from_value(args)
        .map_err(|e| ContextError::InvalidArgs(format!("bad arguments for {tool}: {e}")))
}

fn output<T: Serialize>(value: &T, text: String) -> Result<ToolOutput> {
    Ok(ToolOutput {
        json: serde_json::to_value(value).map_err(|e| ContextError::InvalidArgs(e.to_string()))?,
        text,
    })
}

/// Run a tool by name with JSON arguments (a model's tool call).
pub fn call(
    ctx: &Context<'_>,
    name: &str,
    args: &Value,
    query_vector: Option<QueryVector<'_>>,
) -> Result<ToolOutput> {
    use crate::render;
    match name {
        SEARCH_PASSAGES => {
            let r = ctx.search_passages(&parse_args(name, args)?, query_vector)?;
            output(&r, render::passages(&r))
        }
        READ_SEGMENTS => {
            let r = ctx.read_segments(&parse_args(name, args)?)?;
            output(&r, render::segments(&r))
        }
        LIST_MEETINGS => {
            let r = ctx.list_meetings(&parse_args(name, args)?)?;
            output(&r, render::meetings(&r))
        }
        GET_EVENT => {
            let r = ctx.get_event(&parse_args(name, args)?)?;
            output(&r, render::event(&r))
        }
        GET_NOTE => {
            let r = ctx.get_note(&parse_args(name, args)?)?;
            output(&r, render::note(&r))
        }
        GET_ITEMS => {
            let r = ctx.get_items(&parse_args(name, args)?)?;
            output(&r, render::items(&r))
        }
        other => Err(ContextError::UnknownTool {
            name: other.to_string(),
            available: tool_defs()
                .iter()
                .map(|d| d.name)
                .collect::<Vec<_>>()
                .join(", "),
        }),
    }
}
