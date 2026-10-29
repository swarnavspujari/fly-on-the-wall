//! fly-context: typed, read-only access to the user's meetings for every
//! AI reader — the Ask harness, Enhance, and the MCP server (spec §4.2).
//!
//! * **No LLM, no keys, no network.** Depends on fly-core + fly-storage
//!   only; semantic search takes a query vector the caller already computed
//!   ([`QueryVector`]) — the MCP server has none and gets keyword search.
//! * **Keep local is enforced here, not by callers.** A [`Context`] is built
//!   with a [`Visibility`]. Under [`Visibility::ShareableOnly`] (cloud model
//!   or external AI tool) keep-local notes and meetings are excluded inside
//!   the storage queries (FTS, vector hits, items, note and meeting
//!   listings), and by a fail-closed set check on top of those and on every
//!   by-id read. A failed keep-local lookup returns an
//!   error, never unfiltered content. Tool results say nothing about
//!   what was excluded — not even a count (see
//!   [`Context::excluded_keep_local_meetings`]).
//! * **Every function is a tool contract:** typed args with a JSON schema
//!   ([`tool_defs`]), a serializable result, and a compact text
//!   rendering for a model ([`render`]) that cites transcript lines as
//!   `[seg:ID]`.
//! * **Memory** ([`memory`]): recurring series, people and the facts
//!   already extracted from meetings (decisions, action items, …), each
//!   with its meeting and segment ids — as tools, as a deterministic
//!   per-question prefetch, and as the MCP server's context layer.
//! * **Fan-out** ([`fanout`]): a broad question split into sub-queries
//!   searched in parallel and merged, within a budget (Ask tier C).
//!
//! The MCP server uses it for its context layer (memory) and passage
//! search; the Ask harness is the other caller. Keyword passages are still
//! localized in memory (see [`passages`]); vector hits map onto windows by
//! the chunk's segment ids (storage schema v3).
//! `crates/fly-storage/SCHEMA_V3.md` records the v3 schema and what is
//! left for fly-context to adopt.

use std::collections::HashSet;

use fly_storage::{Storage, StorageError};

pub mod fanout;
pub mod memory;
pub mod passages;
pub mod render;
pub mod tools;

pub use fanout::{decompose, fan_out, FannedPassage, Fanout, FanoutBudget, OpenPerWorker, Readers};
pub use memory::{
    Briefing, CitedSegment, GetBriefingArgs, MatchedBy, MemoryFact, MemoryPrefetch, OpenItems,
    OpenItemsArgs, TimelineEntry, WhatsChangedArgs,
};
pub use tools::{
    EventInfo, GetEventArgs, GetItemsArgs, GetNoteArgs, ItemList, ListMeetingsArgs, MeetingList,
    MeetingSummary, NoteKind, NoteView, Passage, PassageResults, PassageSource, ReadSegmentsArgs,
    SearchPassagesArgs, SegmentView, SegmentWindow, ToolDef, ToolOutput,
};

/// Every tool a model may call, in a stable order: the six read tools
/// ([`tools::tool_defs`]), then the three memory tools
/// ([`memory::tool_defs`]).
pub fn tool_defs() -> Vec<ToolDef> {
    let mut defs = tools::tool_defs();
    defs.extend(memory::tool_defs());
    defs
}

/// Run any tool from [`tool_defs`] by name with JSON arguments (a model's
/// tool call). `query_vector` is used by `search_passages` only.
pub fn call(
    ctx: &Context<'_>,
    name: &str,
    args: &serde_json::Value,
    query_vector: Option<QueryVector<'_>>,
) -> Result<ToolOutput> {
    if let Some(out) = memory::call(ctx, name, args) {
        return out;
    }
    match tools::call(ctx, name, args, query_vector) {
        Err(ContextError::UnknownTool { name, .. }) => Err(ContextError::UnknownTool {
            name,
            available: tool_defs()
                .iter()
                .map(|d| d.name)
                .collect::<Vec<_>>()
                .join(", "),
        }),
        other => other,
    }
}

/// Who will see the results.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Visibility {
    /// Everything, including Keep-local meetings — only for a model running
    /// on this machine.
    All,
    /// Keep-local notes and meetings never appear — any cloud model, and
    /// every external AI tool (MCP).
    ShareableOnly,
}

/// A precomputed query embedding for semantic search (the caller owns the
/// embedder; this crate never makes network calls).
#[derive(Debug, Clone, Copy)]
pub struct QueryVector<'a> {
    pub vector: &'a [f32],
    /// The embedding model id stored on chunks (e.g. "embeddinggemma:300m").
    pub model: &'a str,
}

/// Shown to a model as a tool error, so messages are short and safe.
#[derive(Debug, thiserror::Error)]
pub enum ContextError {
    #[error("{0}")]
    InvalidArgs(String),
    #[error("{0} not found")]
    NotFound(String),
    /// Refused because the target is Keep local (only under
    /// `ShareableOnly`).
    #[error("{0}")]
    KeepLocal(&'static str),
    #[error("there is no tool named {name:?}; available tools: {available}")]
    UnknownTool { name: String, available: String },
    #[error("couldn't read your meetings: {0}")]
    Storage(#[from] StorageError),
}

pub type Result<T> = std::result::Result<T, ContextError>;

pub const KEEP_LOCAL_MEETING: &str =
    "This meeting is marked Keep local and isn't shared with AI tools.";
pub const KEEP_LOCAL_NOTE: &str = "This note is marked Keep local and isn't shared with AI tools.";

/// Read-only view over storage with a fixed visibility.
pub struct Context<'a> {
    storage: &'a Storage,
    visibility: Visibility,
}

/// The keep-local sets, fetched once per call (empty under `All`).
#[derive(Debug, Default)]
pub(crate) struct Hidden {
    notes: HashSet<String>,
    meetings: HashSet<String>,
}

impl Hidden {
    pub(crate) fn note(&self, note_id: &str) -> bool {
        self.notes.contains(note_id)
    }
    /// A meeting is hidden when it or its note is keep-local.
    pub(crate) fn meeting(&self, meeting_id: &str, note_id: &str) -> bool {
        self.meetings.contains(meeting_id) || self.notes.contains(note_id)
    }
}

impl<'a> Context<'a> {
    pub fn new(storage: &'a Storage, visibility: Visibility) -> Self {
        Self {
            storage,
            visibility,
        }
    }

    pub fn visibility(&self) -> Visibility {
        self.visibility
    }

    pub(crate) fn storage(&self) -> &Storage {
        self.storage
    }

    pub(crate) fn shareable_only(&self) -> bool {
        self.visibility == Visibility::ShareableOnly
    }

    /// Keep-local sets for filtering; errors propagate (fail closed).
    pub(crate) fn hidden(&self) -> Result<Hidden> {
        if !self.shareable_only() {
            return Ok(Hidden::default());
        }
        Ok(Hidden {
            notes: self.storage.keep_local_note_ids()?,
            meetings: self.storage.keep_local_meeting_ids()?,
        })
    }

    /// How many meetings this view can't see — for the USER's Ask trace
    /// ("N kept-local meetings not searched") only. Never put it in
    /// model-facing output or tool results: a count that responds to
    /// filters lets a model or MCP client probe kept-local meetings (e.g.
    /// binary-search their dates). Tool results carry no trace of them.
    /// Loads every meeting per call — call once per answer. Always 0 under
    /// `All`.
    pub fn excluded_keep_local_meetings(&self) -> Result<usize> {
        if !self.shareable_only() {
            return Ok(0);
        }
        let hidden = self.hidden()?;
        Ok(self
            .storage
            .list_meetings(None, usize::MAX >> 1)?
            .iter()
            .filter(|m| hidden.meeting(&m.id, &m.note_id))
            .count())
    }

    /// Refuse a by-id meeting read that this view can't see. Uses the
    /// storage predicates, which answer "keep local" on any lookup error.
    pub(crate) fn guard_meeting(&self, meeting: &fly_core::Meeting) -> Result<()> {
        if self.shareable_only()
            && (self.storage.meeting_keep_local(&meeting.id)
                || self.storage.note_keep_local(&meeting.note_id))
        {
            return Err(ContextError::KeepLocal(KEEP_LOCAL_MEETING));
        }
        Ok(())
    }

    pub(crate) fn guard_note(&self, note_id: &str) -> Result<()> {
        if self.shareable_only() && self.storage.note_keep_local(note_id) {
            return Err(ContextError::KeepLocal(KEEP_LOCAL_NOTE));
        }
        Ok(())
    }

    /// A meeting by id, or `NotFound`.
    pub(crate) fn meeting(&self, meeting_id: &str) -> Result<fly_core::Meeting> {
        match self.storage.get_meeting(meeting_id) {
            Ok(m) => Ok(m),
            Err(StorageError::NotFound(_)) => {
                Err(ContextError::NotFound(format!("meeting {meeting_id}")))
            }
            Err(e) => Err(e.into()),
        }
    }
}
