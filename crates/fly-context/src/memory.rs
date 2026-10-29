//! Memory: what the user's meetings already establish, offered to AI
//! readers as cited facts.
//!
//! "Memory" here is not free-form model memory. It is the structured
//! knowledge the app has already extracted (`meeting_items`) and the shape
//! of the user's calendar, assembled deterministically at query time:
//!
//! * **recurring series** — meetings grouped by normalized title and
//!   attendee overlap ([`detect_series`]);
//! * **people** — attendees, speaker labels (so a name set with
//!   `set_speaker_label` flows in) and item owners, and who owns or said
//!   which fact;
//! * **facts** — decisions, action items and their status, commitments,
//!   open questions and key figures, each carrying its meeting id and the
//!   transcript segment ids it was extracted from;
//! * **what changed** — the same facts restricted to a series' newest
//!   meetings.
//!
//! No model runs here. Every fact keeps its provenance, and a
//! [`Briefing`] carries the cited segments themselves ([`CitedSegment`]) so
//! a caller can show and verify them like any retrieved passage.
//!
//! **Visibility** works exactly as for passages: under
//! [`Visibility::ShareableOnly`](crate::Visibility) Keep-local meetings are
//! excluded inside the storage queries *and* by the fail-closed in-memory
//! sets, so they never shape a series, a participant list, a person match
//! or a fact. A failed Keep-local lookup is an error, never unfiltered
//! output.
//!
//! The MCP server (`get_context`, `whats_changed`, `open_items`) and Ask
//! (`get_briefing`, `open_items`, `whats_changed`, [`Context::memory_prefetch`])
//! both run this code; each renders the result its own way.

use std::collections::{BTreeMap, HashMap, HashSet};

use chrono::{DateTime, Utc};
use fly_core::{ItemKind, Meeting, MeetingItem, Transcript};
use fly_storage::{ItemFilter, MeetingFilter, SearchFilter};
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::tools::{
    clamp, def, integer, non_empty, output, parse_args, parse_when, segment_view, string,
    SegmentView, ToolDef, ToolOutput, MAX_QUERY_CHARS,
};
use crate::{Context, ContextError, Hidden, Result};

pub const GET_BRIEFING: &str = "get_briefing";
pub const OPEN_ITEMS: &str = "open_items";
pub const WHATS_CHANGED: &str = "whats_changed";

/// Every meeting row (meeting rows are small; corpora are thousands).
const ALL_MEETINGS: usize = 1_000_000;
/// Items one briefing reads.
const BRIEFING_ITEMS: usize = 500;
/// Items `open_items` scans before keeping the open ones (MCP parity).
const OPEN_SCAN_MCP: usize = 200;
/// Items `open_items` scans for Ask (an owner filter needs a deeper scan).
const OPEN_SCAN: usize = 1_000;
/// Full-text hits that route a query to series.
const FULL_TEXT_HITS: usize = 20;
/// Newest meetings a person briefing reads (each needs its transcript).
const PERSON_MEETINGS: usize = 30;
/// Most series one question-level prefetch covers.
const PREFETCH_SERIES: usize = 3;
/// Facts a prefetch or a `get_briefing` call keeps (the newest).
pub const DEFAULT_MAX_FACTS: usize = 40;
/// Meetings a prefetch or tool briefing keeps on its timeline (the newest).
pub const DEFAULT_MAX_MEETINGS: usize = 12;

// ---------------------------------------------------------------------------
// Recurring series (moved from fly-mcp's context layer, unchanged)
// ---------------------------------------------------------------------------

/// Normalize a meeting title into its series key: lowercase, digits and
/// punctuation stripped (dates, "#12", "(3/4)" all collapse), whitespace
/// folded. "Weekly Sync 2026-07-01" and "weekly sync #2" → "weekly sync".
pub fn normalize_title(title: &str) -> String {
    let mut out = String::with_capacity(title.len());
    for c in title.to_lowercase().chars() {
        if c.is_alphabetic() {
            out.push(c);
        } else if c.is_whitespace() || c.is_numeric() || c.is_ascii_punctuation() {
            out.push(' ');
        }
    }
    out.split_whitespace().collect::<Vec<_>>().join(" ")
}

/// Attendee-overlap (Jaccard) between two meetings; 1.0 when either side has
/// no attendee data (manual recordings) so the title match alone decides.
fn participant_overlap(a: &Meeting, b: &Meeting) -> f64 {
    if a.attendees.is_empty() || b.attendees.is_empty() {
        return 1.0;
    }
    // Match on the stable identity (email when present) so renaming an
    // attendee in the editor doesn't break series detection.
    let sa: HashSet<String> = a.attendees.iter().map(|s| s.match_key()).collect();
    let sb: HashSet<String> = b.attendees.iter().map(|s| s.match_key()).collect();
    let inter = sa.intersection(&sb).count() as f64;
    let union = sa.union(&sb).count() as f64;
    if union == 0.0 {
        1.0
    } else {
        inter / union
    }
}

/// A detected recurring series: same normalized title AND sufficient
/// participant overlap with the rest of the group.
#[derive(Debug, Clone)]
pub struct Series {
    pub key: String,
    /// Chronological (oldest → newest).
    pub meetings: Vec<Meeting>,
}

/// Group meetings into series. Deterministic: meetings sort chronologically,
/// grouping is by normalized title, then a meeting only joins the group when
/// it overlaps (≥ 0.34 Jaccard) with the group's first member — enough that
/// "Weekly sync" with a disjoint set of people forms its own thread.
///
/// Callers pass only meetings the reader may see: a hidden meeting's title
/// would otherwise leak through series keys and timelines.
pub fn detect_series(mut meetings: Vec<Meeting>) -> Vec<Series> {
    meetings.sort_by_key(|m| m.started_at);
    let mut groups: BTreeMap<String, Vec<Vec<Meeting>>> = BTreeMap::new();
    for m in meetings {
        let key = normalize_title(&m.title);
        let buckets = groups.entry(key).or_default();
        match buckets
            .iter_mut()
            .find(|b| participant_overlap(&b[0], &m) >= 0.34)
        {
            Some(bucket) => bucket.push(m),
            None => buckets.push(vec![m]),
        }
    }
    let mut out = Vec::new();
    for (key, buckets) in groups {
        for (i, meetings) in buckets.into_iter().enumerate() {
            let key = if i == 0 {
                key.clone()
            } else {
                format!("{key} ({})", i + 1)
            };
            out.push(Series { key, meetings });
        }
    }
    out
}

/// Series whose key contains every query token. Routes `get_context`,
/// `whats_changed` and `open_items` scopes.
pub fn series_matching<'a>(series: &'a [Series], query: &str) -> Vec<&'a Series> {
    let q = normalize_title(query);
    let tokens: Vec<&str> = q.split_whitespace().collect();
    if tokens.is_empty() {
        return Vec::new();
    }
    series
        .iter()
        .filter(|s| tokens.iter().all(|t| s.key.contains(t)))
        .collect()
}

// ---------------------------------------------------------------------------
// Results
// ---------------------------------------------------------------------------

/// How a briefing found its meetings.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum MatchedBy {
    /// One or more recurring series named by the query.
    Series,
    /// A person named by the query.
    Person,
    /// Series holding meetings whose transcript or notes match the query.
    FullText,
    /// Nothing matched.
    None,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct TimelineEntry {
    pub meeting_id: String,
    pub note_id: String,
    pub title: String,
    pub started_at: DateTime<Utc>,
    pub has_transcript: bool,
    /// Every extracted item of the meeting (not only the facts shown).
    pub item_count: usize,
}

/// One extracted item, with its provenance.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct MemoryFact {
    pub item_id: String,
    pub kind: ItemKind,
    pub text: String,
    pub owner: Option<String>,
    /// As stated in that meeting ("open" / "done"); a later meeting may
    /// have changed it.
    pub status: Option<String>,
    /// The current display label of the speaker it was extracted from
    /// (relabels flow in). `None` without a transcript or speaker.
    pub said_by: Option<String>,
    pub meeting_id: String,
    pub meeting_title: String,
    pub started_at: DateTime<Utc>,
    /// Transcript segments it was extracted from, as stored.
    pub segment_ids: Vec<String>,
}

/// A transcript segment a fact cites, resolved from the current
/// transcript (current speaker label, raw text).
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct CitedSegment {
    pub meeting_id: String,
    pub meeting_title: String,
    pub started_at: DateTime<Utc>,
    pub segment: SegmentView,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Briefing {
    /// What was asked for (a query, series name or question).
    pub query: String,
    pub matched_by: MatchedBy,
    /// Series keys covered (empty for a person briefing or no match).
    pub series: Vec<String>,
    /// Facts are limited to what this person owns or said.
    pub person: Option<String>,
    pub since: Option<DateTime<Utc>>,
    pub until: Option<DateTime<Utc>>,
    /// Attendees, then named speakers, first seen first.
    pub participants: Vec<String>,
    /// Oldest → newest.
    pub meetings: Vec<TimelineEntry>,
    /// Oldest → newest meeting; extraction order within a meeting.
    pub facts: Vec<MemoryFact>,
    /// Every segment a fact cites that still exists in its transcript.
    pub evidence: Vec<CitedSegment>,
    /// Older facts dropped by the fact cap.
    pub facts_omitted: usize,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct OpenItems {
    /// "all" or the series name asked for.
    pub scope: String,
    /// False when `scope` named no series (and nothing was read).
    pub scope_found: bool,
    pub owner: Option<String>,
    /// Newest meeting first.
    pub facts: Vec<MemoryFact>,
    pub evidence: Vec<CitedSegment>,
}

impl Briefing {
    /// Keep only meetings `keep` allows (e.g. an Ask scope): timeline,
    /// facts and evidence alike.
    pub fn retain_meetings(&mut self, keep: impl Fn(&str) -> bool) {
        self.meetings.retain(|m| keep(&m.meeting_id));
        self.facts.retain(|f| keep(&f.meeting_id));
        self.evidence.retain(|e| keep(&e.meeting_id));
    }

    /// The cited segment for `(meeting_id, segment_id)`, if it resolved.
    pub fn cited(&self, meeting_id: &str, segment_id: &str) -> Option<&CitedSegment> {
        self.evidence
            .iter()
            .find(|e| e.meeting_id == meeting_id && e.segment.id == segment_id)
    }
}

impl OpenItems {
    pub fn retain_meetings(&mut self, keep: impl Fn(&str) -> bool) {
        self.facts.retain(|f| keep(&f.meeting_id));
        self.evidence.retain(|e| keep(&e.meeting_id));
    }

    pub fn cited(&self, meeting_id: &str, segment_id: &str) -> Option<&CitedSegment> {
        self.evidence
            .iter()
            .find(|e| e.meeting_id == meeting_id && e.segment.id == segment_id)
    }
}

// ---------------------------------------------------------------------------
// Arguments
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Default, PartialEq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct GetBriefingArgs {
    /// A series, project, customer, topic or person's name.
    pub query: String,
    #[serde(default)]
    pub person: Option<String>,
    #[serde(default)]
    pub since: Option<String>,
}

#[derive(Debug, Clone, Default, PartialEq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct OpenItemsArgs {
    /// "all" (default) or a series name.
    #[serde(default)]
    pub scope: Option<String>,
    #[serde(default)]
    pub owner: Option<String>,
    #[serde(default)]
    pub limit: Option<u32>,
}

#[derive(Debug, Clone, Default, PartialEq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct WhatsChangedArgs {
    pub series: String,
    /// Default: the series' newest meeting that has extracted items.
    #[serde(default)]
    pub since: Option<String>,
}

/// Options for [`Context::memory_prefetch`].
#[derive(Debug, Clone, Default, PartialEq)]
pub struct MemoryPrefetch {
    /// People the caller already validated against the user's meetings
    /// (e.g. the Ask router's). Empty: detect from the question.
    pub people: Vec<String>,
    pub since: Option<DateTime<Utc>>,
    pub until: Option<DateTime<Utc>>,
    /// Newest facts kept; 0 = [`DEFAULT_MAX_FACTS`].
    pub max_facts: usize,
    /// Newest meetings kept on a series timeline; 0 =
    /// [`DEFAULT_MAX_MEETINGS`].
    pub max_meetings: usize,
}

/// How a briefing is assembled (internal knobs; the MCP keeps its
/// historical behaviour, Ask gets caps and person routing).
#[derive(Debug, Clone, Copy, Default)]
pub(crate) struct BriefingOptions {
    /// Route a query that names a known person to a person briefing.
    pub resolve_person: bool,
    pub max_facts: Option<usize>,
    pub max_meetings: Option<usize>,
}

// ---------------------------------------------------------------------------
// People
// ---------------------------------------------------------------------------

/// Words of a name: an email's local part counts as the name
/// ("dana.lee@example.com" → ["dana", "lee"]).
fn name_tokens(s: &str) -> Vec<String> {
    let s = s.trim();
    let base = match s.split_once('@') {
        Some((local, _)) if !local.is_empty() => local,
        _ => s,
    };
    base.split(|c: char| !c.is_alphanumeric())
        .filter(|t| !t.is_empty())
        .map(str::to_lowercase)
        .collect()
}

/// Speaker labels and owners that name nobody in particular.
fn generic_name(tokens: &[String]) -> bool {
    if tokens
        .first()
        .is_some_and(|t| STOPWORDS.contains(&t.as_str()) && !NAME_WORDS.contains(&t.as_str()))
    {
        return true; // "We", "The team", "All hands"
    }
    match tokens {
        [] => true,
        [one] => {
            one.chars().count() < 2
                || matches!(
                    one.as_str(),
                    "you"
                        | "unknown"
                        | "guest"
                        | "other"
                        | "others"
                        | "everyone"
                        | "team"
                        | "all"
                        | "we"
                        | "us"
                        | "none"
                        | "tbd"
                        | "n"
                        | "na"
                )
        }
        [first, ..] => matches!(first.as_str(), "speaker" | "spk" | "unknown"),
    }
}

/// A person a briefing is about.
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct PersonKey {
    /// Display form ("Dana Lee").
    pub name: String,
    tokens: Vec<String>,
}

impl PersonKey {
    pub(crate) fn new(name: &str) -> Option<Self> {
        let tokens = name_tokens(name);
        (!tokens.is_empty()).then(|| PersonKey {
            name: name.trim().to_string(),
            tokens,
        })
    }

    /// The user themself (the mic speaker's default label).
    fn you() -> Self {
        PersonKey {
            name: "You".into(),
            tokens: vec!["you".into()],
        }
    }

    /// Same name, or the same first name where one side is only a first
    /// name ("Dana" ~ "Dana Lee" ~ "dana@…", but "Dana Lee" ≁ "Dana Kim").
    pub(crate) fn matches(&self, other: &str) -> bool {
        let o = name_tokens(other);
        if o.is_empty() {
            return false;
        }
        o == self.tokens || (o[0] == self.tokens[0] && (o.len() == 1 || self.tokens.len() == 1))
    }
}

/// English words that are also first names: matched as a name only when
/// the question capitalizes them.
const NAME_WORDS: &[&str] = &[
    "will", "may", "mark", "bill", "april", "june", "august", "frank", "grant", "rich", "joy",
    "hope", "faith", "art", "sue", "pat", "ray", "dawn", "rose", "jack", "max", "chase", "page",
    "lane", "drew", "gene", "earl", "guy", "jay", "rob", "sky", "summer", "victor", "sunny",
    "brook", "glen", "dean", "miles", "wade", "reed", "young", "long", "white", "black", "brown",
    "green", "king", "price", "case", "bob", "don", "nick", "pete", "harry", "penny", "holly",
];

/// Question words that never start a person or project match.
const STOPWORDS: &[&str] = &[
    "a",
    "about",
    "after",
    "again",
    "all",
    "also",
    "am",
    "an",
    "and",
    "any",
    "are",
    "as",
    "at",
    "be",
    "been",
    "before",
    "between",
    "but",
    "by",
    "can",
    "change",
    "changed",
    "changes",
    "commit",
    "committed",
    "commitments",
    "could",
    "decide",
    "decided",
    "decision",
    "decisions",
    "did",
    "do",
    "does",
    "doing",
    "done",
    "down",
    "each",
    "for",
    "from",
    "get",
    "give",
    "going",
    "got",
    "had",
    "has",
    "have",
    "he",
    "her",
    "here",
    "him",
    "his",
    "how",
    "i",
    "if",
    "in",
    "into",
    "is",
    "it",
    "its",
    "just",
    "last",
    "latest",
    "let",
    "me",
    "meeting",
    "meetings",
    "mine",
    "more",
    "most",
    "my",
    "myself",
    "need",
    "new",
    "next",
    "no",
    "not",
    "now",
    "of",
    "on",
    "one",
    "open",
    "or",
    "our",
    "out",
    "over",
    "owe",
    "owes",
    "own",
    "owns",
    "promised",
    "said",
    "say",
    "says",
    "she",
    "should",
    "since",
    "so",
    "some",
    "status",
    "still",
    "tell",
    "than",
    "that",
    "the",
    "their",
    "them",
    "then",
    "there",
    "these",
    "they",
    "this",
    "those",
    "time",
    "to",
    "told",
    "up",
    "update",
    "updates",
    "us",
    "was",
    "we",
    "were",
    "what",
    "whats",
    "when",
    "where",
    "which",
    "who",
    "whom",
    "why",
    "will",
    "with",
    "would",
    "yet",
    "you",
    "your",
    "items",
    "item",
    "action",
    "actions",
    "agreed",
    "agree",
    "summary",
    "summarize",
    "recap",
    "week",
    "weeks",
    "month",
    "today",
    "yesterday",
    "happened",
    "happen",
    "discuss",
    "discussed",
    "talk",
    "talked",
    "anything",
    "everything",
    "things",
    "thing",
    "please",
];

/// Title words too common to identify a project on their own.
const GENERIC_TITLE_WORDS: &[&str] = &[
    "weekly",
    "daily",
    "monthly",
    "quarterly",
    "biweekly",
    "sync",
    "meeting",
    "call",
    "calls",
    "standup",
    "stand",
    "review",
    "team",
    "planning",
    "plan",
    "chat",
    "catch",
    "check",
    "update",
    "updates",
    "kickoff",
    "kick",
    "retro",
    "retrospective",
    "session",
    "discussion",
    "notes",
    "internal",
    "external",
    "intro",
    "follow",
    "followup",
    "with",
    "and",
    "the",
    "for",
    "on",
    "of",
    "to",
    "vs",
    "x",
    "one",
    "all",
    "hands",
    "office",
    "hours",
    "touch",
    "base",
    "huddle",
    "demo",
    "workshop",
    "interview",
    "lunch",
    "coffee",
];

/// "What changed" phrasing in a question.
const CHANGE_WORDS: &[&str] = &[
    "changed",
    "change",
    "changes",
    "new",
    "since",
    "latest",
    "update",
    "updates",
    "different",
    "progress",
];

const FIRST_PERSON: &[&str] = &["i", "me", "my", "mine", "myself"];

/// A question as words: lowercase tokens plus whether each was written
/// capitalized.
struct Words {
    lower: Vec<String>,
    capitalized: Vec<bool>,
}

impl Words {
    fn new(text: &str) -> Self {
        let raw: Vec<&str> = text
            .split(|c: char| !c.is_alphanumeric())
            .filter(|w| !w.is_empty())
            .collect();
        Words {
            lower: raw.iter().map(|w| w.to_lowercase()).collect(),
            capitalized: raw
                .iter()
                .map(|w| w.chars().next().is_some_and(char::is_uppercase))
                .collect(),
        }
    }

    fn has(&self, word: &str) -> bool {
        self.lower.iter().any(|w| w == word)
    }

    /// `tokens` as a contiguous run.
    fn has_run(&self, tokens: &[String]) -> bool {
        !tokens.is_empty() && self.lower.windows(tokens.len()).any(|w| w == tokens)
    }

    /// `token` appears as a name. A name that is also a common word
    /// ("Will", "May") counts only when capitalized mid-sentence.
    fn has_name(&self, token: &str) -> bool {
        self.lower.iter().enumerate().any(|(i, w)| {
            w == token && (!NAME_WORDS.contains(&token) || (i > 0 && self.capitalized[i]))
        })
    }

    /// Matches `key_token` exactly, or as a plural/prefix for longer words
    /// ("renewals" ~ "renewal").
    fn has_like(&self, key_token: &str) -> bool {
        self.lower.iter().any(|w| {
            w == key_token
                || (key_token.chars().count() >= 4
                    && (w.starts_with(key_token) || key_token.starts_with(w.as_str()))
                    && w.chars().count() >= 4)
        })
    }
}

/// Names the reader may see: attendees and item owners of visible meetings.
fn known_names(meetings: &[Meeting], owners: &[String]) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    let mut push = |n: &str| {
        let n = n.trim();
        if !n.is_empty() && !out.iter().any(|o| o.eq_ignore_ascii_case(n)) {
            out.push(n.to_string());
        }
    };
    for m in meetings {
        for a in &m.attendees {
            push(&a.name);
            if let Some(e) = &a.email {
                push(e);
            }
        }
    }
    for o in owners {
        push(o);
    }
    out
}

/// The person `text` names, validated against `known`: a full name in the
/// text wins; otherwise a first name that maps to one person. `None` when
/// nobody (or more than one person) is named.
fn find_person(text: &str, known: &[String]) -> Option<PersonKey> {
    let words = Words::new(text);
    let candidates: Vec<(String, Vec<String>)> = known
        .iter()
        .map(|n| (n.clone(), name_tokens(n)))
        .filter(|(_, t)| !generic_name(t))
        .collect();
    // Full names (2+ words) written out in the text.
    let mut full: Vec<&(String, Vec<String>)> = candidates
        .iter()
        .filter(|(_, t)| t.len() >= 2 && words.has_run(t))
        .collect();
    if !full.is_empty() {
        full.sort_by_key(|(n, t)| (std::cmp::Reverse(t.len()), n.contains('@'), n.clone()));
        let (name, _) = full[0];
        return PersonKey::new(&display_name(name, &candidates));
    }
    // First names: every candidate sharing the matched first word must be
    // the same person (one multi-word name at most).
    let mut firsts: Vec<&String> = candidates
        .iter()
        .map(|(_, t)| &t[0])
        .filter(|f| f.chars().count() >= 2 && words.has_name(f))
        .collect();
    firsts.sort();
    firsts.dedup();
    if firsts.len() != 1 {
        return None;
    }
    let first = firsts[0];
    let mut longs: Vec<&Vec<String>> = candidates
        .iter()
        .map(|(_, t)| t)
        .filter(|t| &t[0] == first && t.len() >= 2)
        .collect();
    longs.sort();
    longs.dedup();
    if longs.len() > 1 {
        return None; // "Dana" could be Dana Lee or Dana Kim
    }
    let name = candidates
        .iter()
        .filter(|(_, t)| &t[0] == first)
        .map(|(n, _)| n.clone())
        .next()?;
    PersonKey::new(&display_name(&name, &candidates))
}

/// The nicest display for a person: a written name over an email.
fn display_name(name: &str, candidates: &[(String, Vec<String>)]) -> String {
    if !name.contains('@') {
        return name.to_string();
    }
    let toks = name_tokens(name);
    candidates
        .iter()
        .filter(|(n, t)| !n.contains('@') && (t == &toks || t[0] == toks[0]))
        .map(|(n, _)| n.clone())
        .next()
        .unwrap_or_else(|| name.to_string())
}

// ---------------------------------------------------------------------------
// Assembly
// ---------------------------------------------------------------------------

/// One memory read: the Keep-local sets (fetched once, fail closed) and a
/// transcript cache.
struct Reader<'c, 'a> {
    ctx: &'c Context<'a>,
    hidden: Hidden,
    transcripts: HashMap<String, Option<Transcript>>,
}

/// What [`Reader::assemble`] produces facts for.
struct Plan {
    query: String,
    matched_by: MatchedBy,
    series: Vec<String>,
    meetings: Vec<Meeting>,
    since: Option<DateTime<Utc>>,
    until: Option<DateTime<Utc>>,
    person: Option<PersonKey>,
    max_facts: Option<usize>,
    max_meetings: Option<usize>,
}

impl<'c, 'a> Reader<'c, 'a> {
    fn new(ctx: &'c Context<'a>) -> Result<Self> {
        Ok(Reader {
            ctx,
            hidden: ctx.hidden()?,
            transcripts: HashMap::new(),
        })
    }

    fn transcript(&mut self, meeting_id: &str) -> Result<Option<&Transcript>> {
        if !self.transcripts.contains_key(meeting_id) {
            let t = self.ctx.storage().get_transcript(meeting_id)?;
            self.transcripts.insert(meeting_id.to_string(), t);
        }
        Ok(self.transcripts[meeting_id].as_ref())
    }

    /// Every meeting this view may see, newest first. Keep local is
    /// filtered in SQL and again by the in-memory sets.
    fn visible_meetings(&self) -> Result<Vec<Meeting>> {
        let rows = self.ctx.storage().list_meetings_filtered(&MeetingFilter {
            exclude_keep_local: self.ctx.shareable_only(),
            limit: ALL_MEETINGS,
            ..Default::default()
        })?;
        Ok(rows
            .into_iter()
            .filter(|m| !self.hidden.meeting(&m.id, &m.note_id))
            .collect())
    }

    /// Items, Keep local filtered in SQL, and kept only when their meeting
    /// is in `visible` (so an item of a hidden or unknown meeting can't
    /// slip through).
    fn items(&self, filter: ItemFilter, visible: &HashSet<&str>) -> Result<Vec<MeetingItem>> {
        if filter
            .meeting_ids
            .as_ref()
            .is_some_and(|ids| ids.is_empty())
        {
            return Ok(Vec::new());
        }
        let items = self.ctx.storage().query_items(&ItemFilter {
            exclude_keep_local: self.ctx.shareable_only(),
            ..filter
        })?;
        Ok(items
            .into_iter()
            .filter(|i| visible.contains(i.meeting_id.as_str()))
            .collect())
    }

    fn fact(&mut self, item: &MeetingItem, meeting: &Meeting) -> Result<MemoryFact> {
        let said_by = match &item.speaker_key {
            Some(key) => self.transcript(&item.meeting_id)?.map(|t| t.label_for(key)),
            None => None,
        };
        Ok(MemoryFact {
            item_id: item.id.clone(),
            kind: item.kind,
            text: item.text.clone(),
            owner: item.owner.clone(),
            status: item.status.clone(),
            said_by,
            meeting_id: item.meeting_id.clone(),
            meeting_title: meeting.title.clone(),
            started_at: meeting.started_at,
            segment_ids: item.segment_ids.clone(),
        })
    }

    /// The segments `facts` cite that still exist in their transcripts.
    fn evidence(&mut self, facts: &[MemoryFact]) -> Result<Vec<CitedSegment>> {
        let mut out: Vec<CitedSegment> = Vec::new();
        let mut seen: HashSet<(String, String)> = HashSet::new();
        for f in facts {
            if f.segment_ids.is_empty() {
                continue;
            }
            let Some(t) = self.transcript(&f.meeting_id)? else {
                continue;
            };
            for id in &f.segment_ids {
                if !seen.insert((f.meeting_id.clone(), id.clone())) {
                    continue;
                }
                if let Some(i) = t.segments.iter().position(|s| &s.id == id) {
                    out.push(CitedSegment {
                        meeting_id: f.meeting_id.clone(),
                        meeting_title: f.meeting_title.clone(),
                        started_at: f.started_at,
                        segment: segment_view(t, i),
                    });
                }
            }
        }
        Ok(out)
    }

    /// Participants, timeline, facts and evidence for `plan.meetings`.
    fn assemble(&mut self, plan: Plan) -> Result<Briefing> {
        let Plan {
            query,
            matched_by,
            series,
            mut meetings,
            since,
            until,
            person,
            max_facts,
            max_meetings,
        } = plan;
        // Defense in depth: callers pass visible meetings only.
        meetings.retain(|m| !self.hidden.meeting(&m.id, &m.note_id));
        meetings.retain(|m| since.is_none_or(|s| m.started_at >= s));
        meetings.retain(|m| until.is_none_or(|u| m.started_at <= u));
        meetings.sort_by_key(|m| m.started_at);
        if let Some(n) = max_meetings {
            let drop = meetings.len().saturating_sub(n.max(1));
            meetings.drain(..drop);
        }
        let mut b = Briefing {
            query,
            matched_by: if meetings.is_empty() && matched_by != MatchedBy::Person {
                MatchedBy::None
            } else {
                matched_by
            },
            series,
            person: person.as_ref().map(|p| p.name.clone()),
            since,
            until,
            participants: Vec::new(),
            meetings: Vec::new(),
            facts: Vec::new(),
            evidence: Vec::new(),
            facts_omitted: 0,
        };
        if meetings.is_empty() {
            return Ok(b);
        }

        // Participants: attendee union, then named speakers.
        for m in &meetings {
            for a in &m.attendees {
                let name = a.display_name();
                if !name.is_empty() && !b.participants.iter().any(|p| p.eq_ignore_ascii_case(name))
                {
                    b.participants.push(name.to_string());
                }
            }
            if let Some(t) = self.transcript(&m.id)? {
                for s in &t.speakers {
                    if !s.label.starts_with("Speaker ")
                        && !b
                            .participants
                            .iter()
                            .any(|p| p.eq_ignore_ascii_case(&s.label))
                    {
                        b.participants.push(s.label.clone());
                    }
                }
            }
        }

        // Timeline.
        for m in &meetings {
            let has_transcript = self.transcript(&m.id)?.is_some();
            let item_count = self.ctx.storage().get_meeting_items(&m.id)?.len();
            b.meetings.push(TimelineEntry {
                meeting_id: m.id.clone(),
                note_id: m.note_id.clone(),
                title: m.title.clone(),
                started_at: m.started_at,
                has_transcript,
                item_count,
            });
        }

        // Facts, chronological by meeting (stable: extraction order within).
        let by_id: HashMap<&str, &Meeting> = meetings.iter().map(|m| (m.id.as_str(), m)).collect();
        let visible: HashSet<&str> = by_id.keys().copied().collect();
        let mut items = self.items(
            ItemFilter {
                meeting_ids: Some(meetings.iter().map(|m| m.id.clone()).collect()),
                limit: BRIEFING_ITEMS,
                ..Default::default()
            },
            &visible,
        )?;
        items.sort_by_key(|i| by_id.get(i.meeting_id.as_str()).map(|m| m.started_at));
        let mut facts = Vec::with_capacity(items.len());
        for item in &items {
            let m = by_id[item.meeting_id.as_str()];
            facts.push(self.fact(item, m)?);
        }
        if let Some(p) = &person {
            facts.retain(|f| {
                f.owner.as_deref().is_some_and(|o| p.matches(o))
                    || f.said_by.as_deref().is_some_and(|s| p.matches(s))
            });
        }
        if let Some(n) = max_facts {
            let drop = facts.len().saturating_sub(n);
            facts.drain(..drop);
            b.facts_omitted = drop;
        }
        b.evidence = self.evidence(&facts)?;
        b.facts = facts;
        Ok(b)
    }

    /// Meetings where `person` attended or owns an item, newest first.
    fn person_meetings(&self, person: &PersonKey, visible: &[Meeting]) -> Result<Vec<Meeting>> {
        let ids: HashSet<&str> = visible.iter().map(|m| m.id.as_str()).collect();
        let owned: HashSet<String> = self
            .items(
                ItemFilter {
                    owner: Some(person.tokens[0].clone()),
                    limit: OPEN_SCAN,
                    ..Default::default()
                },
                &ids,
            )?
            .into_iter()
            .filter(|i| i.owner.as_deref().is_some_and(|o| person.matches(o)))
            .map(|i| i.meeting_id)
            .collect();
        Ok(visible
            .iter()
            .filter(|m| {
                owned.contains(&m.id)
                    || m.attendees.iter().any(|a| {
                        person.matches(&a.name)
                            || a.email.as_deref().is_some_and(|e| person.matches(e))
                    })
            })
            .cloned()
            .collect())
    }

    /// Item owners of visible meetings (for person matching).
    fn owners(&self, visible: &[Meeting]) -> Result<Vec<String>> {
        let ids: HashSet<&str> = visible.iter().map(|m| m.id.as_str()).collect();
        let mut owners: Vec<String> = self
            .items(
                ItemFilter {
                    limit: OPEN_SCAN,
                    ..Default::default()
                },
                &ids,
            )?
            .into_iter()
            .filter_map(|i| i.owner)
            .collect();
        owners.sort();
        owners.dedup();
        Ok(owners)
    }

    fn person_briefing(
        &mut self,
        query: &str,
        person: PersonKey,
        visible: &[Meeting],
        since: Option<DateTime<Utc>>,
        until: Option<DateTime<Utc>>,
        opts: BriefingOptions,
    ) -> Result<Briefing> {
        let mut meetings = self.person_meetings(&person, visible)?;
        meetings.retain(|m| since.is_none_or(|s| m.started_at >= s));
        meetings.retain(|m| until.is_none_or(|u| m.started_at <= u));
        // visible is newest first; read at most PERSON_MEETINGS transcripts
        meetings.truncate(PERSON_MEETINGS);
        self.assemble(Plan {
            query: query.to_string(),
            matched_by: MatchedBy::Person,
            series: Vec::new(),
            meetings,
            since,
            until,
            person: Some(person),
            max_facts: opts.max_facts,
            max_meetings: opts.max_meetings,
        })
    }
}

/// The start of the newest meeting in `meetings` that has extracted items
/// (else the newest meeting): "since last time".
fn latest_start(ctx: &Context<'_>, meetings: &[Meeting]) -> Result<Option<DateTime<Utc>>> {
    let mut sorted: Vec<&Meeting> = meetings.iter().collect();
    sorted.sort_by_key(|m| std::cmp::Reverse(m.started_at));
    for m in &sorted {
        if !ctx.storage().get_meeting_items(&m.id)?.is_empty() {
            return Ok(Some(m.started_at));
        }
    }
    Ok(sorted.first().map(|m| m.started_at))
}

fn series_meetings(matched: &[&Series]) -> Vec<Meeting> {
    matched
        .iter()
        .flat_map(|s| s.meetings.iter().cloned())
        .collect()
}

fn check_query(field: &str, q: &str) -> Result<()> {
    if q.trim().is_empty() {
        return Err(ContextError::InvalidArgs(format!(
            "{field} must not be empty"
        )));
    }
    if q.chars().count() > MAX_QUERY_CHARS {
        return Err(ContextError::InvalidArgs(format!(
            "{field} is longer than {MAX_QUERY_CHARS} characters"
        )));
    }
    Ok(())
}

impl Context<'_> {
    /// The briefing for a series, project, topic or person (the MCP's
    /// `get_context`, Ask's `get_briefing`). Routing: series whose name
    /// holds every query word; else (Ask only) a known person the query
    /// names; else series holding meetings whose transcript or notes match
    /// the query.
    pub(crate) fn briefing_with(
        &self,
        query: &str,
        person: Option<&str>,
        since: Option<DateTime<Utc>>,
        opts: BriefingOptions,
    ) -> Result<Briefing> {
        let mut r = Reader::new(self)?;
        let visible = r.visible_meetings()?;
        let series = detect_series(visible.clone());
        let person =
            match person.and_then(non_empty_str) {
                Some(p) => Some(PersonKey::new(p).ok_or_else(|| {
                    ContextError::InvalidArgs("person must contain a name".into())
                })?),
                None => None,
            };
        let mut matched = series_matching(&series, query);
        let mut matched_by = MatchedBy::Series;
        if matched.is_empty() && opts.resolve_person && person.is_none() {
            let owners = r.owners(&visible)?;
            if let Some(p) = find_person(query, &known_names(&visible, &owners)) {
                return r.person_briefing(query, p, &visible, since, None, opts);
            }
        }
        if matched.is_empty() {
            matched_by = MatchedBy::FullText;
            let hits = self.storage().search_filtered(
                query,
                &SearchFilter {
                    limit: FULL_TEXT_HITS,
                    exclude_keep_local: self.shareable_only(),
                    ..Default::default()
                },
            )?;
            let ids: HashSet<String> = hits
                .into_iter()
                .filter(|h| !r.hidden.note(&h.note_id))
                .filter_map(|h| h.meeting_id)
                .collect();
            matched = series
                .iter()
                .filter(|s| s.meetings.iter().any(|m| ids.contains(&m.id)))
                .collect();
        }
        r.assemble(Plan {
            query: query.to_string(),
            matched_by,
            series: matched.iter().map(|s| s.key.clone()).collect(),
            meetings: series_meetings(&matched),
            since,
            until: None,
            person,
            max_facts: opts.max_facts,
            max_meetings: opts.max_meetings,
        })
    }

    /// One series since a date (default for Ask: since its newest meeting
    /// with extracted items). `series` empty in the result = no series
    /// matched.
    pub(crate) fn whats_changed_with(
        &self,
        series_name: &str,
        since: Option<DateTime<Utc>>,
        opts: BriefingOptions,
    ) -> Result<Briefing> {
        let mut r = Reader::new(self)?;
        let visible = r.visible_meetings()?;
        let series = detect_series(visible);
        let matched = series_matching(&series, series_name);
        let meetings = series_meetings(&matched);
        let since = match since {
            Some(s) => Some(s),
            None => latest_start(self, &meetings)?,
        };
        r.assemble(Plan {
            query: series_name.to_string(),
            matched_by: MatchedBy::Series,
            series: matched.iter().map(|s| s.key.clone()).collect(),
            meetings,
            since,
            until: None,
            person: None,
            max_facts: opts.max_facts,
            max_meetings: opts.max_meetings,
        })
    }

    /// Open action items and commitments (not stated as done), newest
    /// meeting first. `evidence` resolves cited segments (needs each
    /// meeting's transcript; the MCP skips it).
    pub(crate) fn open_items_with(
        &self,
        scope: &str,
        owner: Option<&str>,
        limit: Option<usize>,
        evidence: bool,
    ) -> Result<OpenItems> {
        let mut r = Reader::new(self)?;
        let visible = r.visible_meetings()?;
        let by_id: HashMap<&str, &Meeting> = visible.iter().map(|m| (m.id.as_str(), m)).collect();
        let owner = match owner.and_then(non_empty_str) {
            Some(o) => Some(
                PersonKey::new(o)
                    .ok_or_else(|| ContextError::InvalidArgs("owner must contain a name".into()))?,
            ),
            None => None,
        };
        let mut out = OpenItems {
            scope: scope.to_string(),
            scope_found: true,
            owner: owner.as_ref().map(|o| o.name.clone()),
            facts: Vec::new(),
            evidence: Vec::new(),
        };
        let meeting_ids = if scope == "all" {
            None
        } else {
            let series = detect_series(visible.clone());
            let matched = series_matching(&series, scope);
            if matched.is_empty() {
                out.scope_found = false;
                return Ok(out);
            }
            Some(
                matched
                    .iter()
                    .flat_map(|s| s.meetings.iter().map(|m| m.id.clone()))
                    .collect::<Vec<_>>(),
            )
        };
        let ids: HashSet<&str> = by_id.keys().copied().collect();
        let mut items = r.items(
            ItemFilter {
                meeting_ids,
                // with an owner filter, scan deeper before filtering
                owner: owner.as_ref().map(|o| o.tokens[0].clone()),
                limit: if evidence { OPEN_SCAN } else { OPEN_SCAN_MCP },
                ..Default::default()
            },
            &ids,
        )?;
        items.retain(|i| {
            matches!(i.kind, ItemKind::ActionItem | ItemKind::Commitment)
                && i.status.as_deref() != Some("done")
        });
        if let Some(o) = &owner {
            items.retain(|i| i.owner.as_deref().is_some_and(|x| o.matches(x)));
        }
        if let Some(n) = limit {
            items.truncate(n);
        }
        for item in &items {
            let m = by_id[item.meeting_id.as_str()];
            let fact = if evidence {
                r.fact(item, m)?
            } else {
                MemoryFact {
                    item_id: item.id.clone(),
                    kind: item.kind,
                    text: item.text.clone(),
                    owner: item.owner.clone(),
                    status: item.status.clone(),
                    said_by: None,
                    meeting_id: item.meeting_id.clone(),
                    meeting_title: m.title.clone(),
                    started_at: m.started_at,
                    segment_ids: item.segment_ids.clone(),
                }
            };
            out.facts.push(fact);
        }
        if evidence {
            out.evidence = r.evidence(&out.facts)?;
        }
        Ok(out)
    }

    // ---- the Ask-facing API ------------------------------------------------

    /// `get_briefing`: a series, project, topic or person, capped to the
    /// newest [`DEFAULT_MAX_MEETINGS`] meetings and [`DEFAULT_MAX_FACTS`]
    /// facts.
    pub fn get_briefing(&self, args: &GetBriefingArgs) -> Result<Briefing> {
        check_query("query", &args.query)?;
        let since = parse_when("since", &args.since, false)?;
        self.briefing_with(
            args.query.trim(),
            args.person.as_deref(),
            since,
            BriefingOptions {
                resolve_person: true,
                max_facts: Some(DEFAULT_MAX_FACTS),
                max_meetings: Some(DEFAULT_MAX_MEETINGS),
            },
        )
    }

    /// `open_items`: open action items and commitments, newest first.
    pub fn open_items(&self, args: &OpenItemsArgs) -> Result<OpenItems> {
        let scope = non_empty(&args.scope).unwrap_or("all");
        if scope.chars().count() > MAX_QUERY_CHARS {
            return Err(ContextError::InvalidArgs(format!(
                "scope is longer than {MAX_QUERY_CHARS} characters"
            )));
        }
        let limit = clamp(args.limit, 30, 1, 100);
        self.open_items_with(scope, args.owner.as_deref(), Some(limit), true)
    }

    /// `whats_changed`: one series since a date (default: its newest
    /// meeting with extracted items).
    pub fn whats_changed(&self, args: &WhatsChangedArgs) -> Result<Briefing> {
        check_query("series", &args.series)?;
        let since = parse_when("since", &args.since, false)?;
        self.whats_changed_with(
            args.series.trim(),
            since,
            BriefingOptions {
                resolve_person: false,
                max_facts: Some(DEFAULT_MAX_FACTS),
                max_meetings: Some(DEFAULT_MAX_MEETINGS),
            },
        )
    }

    /// Deterministic memory for one question, with no model tool call
    /// (Ask tier S). `Some` only when the question names a known person
    /// (attendee or item owner; "I"/"my" = the user's own "You") or a
    /// recurring series / project from the user's meeting titles — all
    /// validated against meetings this view may see. "What changed …"
    /// about a series narrows to its newest meeting with items, unless
    /// `opts.since` is set.
    pub fn memory_prefetch(
        &self,
        question: &str,
        opts: &MemoryPrefetch,
    ) -> Result<Option<Briefing>> {
        if question.trim().is_empty() || question.chars().count() > MAX_QUERY_CHARS {
            return Ok(None);
        }
        let mut r = Reader::new(self)?;
        let visible = r.visible_meetings()?;
        if visible.is_empty() {
            return Ok(None);
        }
        let words = Words::new(question);
        let owners = r.owners(&visible)?;
        let known = known_names(&visible, &owners);
        let person = match opts.people.iter().find_map(|p| PersonKey::new(p)) {
            Some(p) => Some(p),
            None => find_person(question, &known).or_else(|| {
                // "my action items": the user's own, when "You" owns or
                // says anything.
                FIRST_PERSON
                    .iter()
                    .any(|w| words.has(w))
                    .then(PersonKey::you)
                    .filter(|_| owners.iter().any(|o| o.eq_ignore_ascii_case("you")))
            }),
        };
        let person_tokens: HashSet<String> = person
            .as_ref()
            .map(|p| p.tokens.iter().cloned().collect())
            .unwrap_or_default();

        let series = detect_series(visible.clone());
        let matched = series_in_question(&series, &words, &person_tokens);
        let max_facts = Some(if opts.max_facts == 0 {
            DEFAULT_MAX_FACTS
        } else {
            opts.max_facts
        });
        let max_meetings = Some(if opts.max_meetings == 0 {
            DEFAULT_MAX_MEETINGS
        } else {
            opts.max_meetings
        });
        let b = if matched.is_empty() {
            let Some(p) = person else {
                return Ok(None);
            };
            r.person_briefing(
                question,
                p,
                &visible,
                opts.since,
                opts.until,
                BriefingOptions {
                    resolve_person: true,
                    max_facts,
                    max_meetings,
                },
            )?
        } else {
            let meetings = series_meetings(&matched);
            let since = match opts.since {
                Some(s) => Some(s),
                None if CHANGE_WORDS.iter().any(|w| words.has(w)) => latest_start(self, &meetings)?,
                None => None,
            };
            r.assemble(Plan {
                query: question.to_string(),
                matched_by: MatchedBy::Series,
                series: matched.iter().map(|s| s.key.clone()).collect(),
                meetings,
                since,
                until: opts.until,
                person,
                max_facts,
                max_meetings,
            })?
        };
        Ok((!b.meetings.is_empty()).then_some(b))
    }
}

fn non_empty_str(s: &str) -> Option<&str> {
    let s = s.trim();
    (!s.is_empty()).then_some(s)
}

/// Series a question names: every word of a series name (one that isn't
/// all generic words or the person's name), else a distinctive question
/// word found in series names (a project: "launch" → "launch weekly",
/// "launch retro"). Newest series first, at most [`PREFETCH_SERIES`].
fn series_in_question<'s>(
    series: &'s [Series],
    words: &Words,
    person_tokens: &HashSet<String>,
) -> Vec<&'s Series> {
    let distinctive = |t: &str| {
        !GENERIC_TITLE_WORDS.contains(&t) && !STOPWORDS.contains(&t) && !person_tokens.contains(t)
    };
    let mut full: Vec<&Series> = series
        .iter()
        .filter(|s| {
            // keys like "weekly sync (2)" carry a bucket suffix
            let toks: Vec<String> = s
                .key
                .split_whitespace()
                .filter(|t| !t.starts_with('(') && !t.ends_with(')'))
                .map(str::to_string)
                .collect();
            if toks.is_empty() {
                false
            } else if toks.iter().any(|t| distinctive(t)) {
                toks.iter().all(|t| words.has_like(t))
            } else {
                // an all-generic name ("weekly sync") only as written
                toks.len() >= 2 && words.has_run(&toks)
            }
        })
        .collect();
    if full.is_empty() {
        let project: Vec<&String> = words
            .lower
            .iter()
            .filter(|w| w.chars().count() >= 4 && distinctive(w))
            .collect();
        full = series
            .iter()
            .filter(|s| {
                s.key.split_whitespace().any(|k| {
                    distinctive(k)
                        && project.iter().any(|w| {
                            *w == k
                                || (k.chars().count() >= 4
                                    && (w.starts_with(k) || k.starts_with(w.as_str())))
                        })
                })
            })
            .collect();
    }
    full.sort_by_key(|s| std::cmp::Reverse(s.meetings.last().map(|m| m.started_at)));
    full.truncate(PREFETCH_SERIES);
    full
}

// ---------------------------------------------------------------------------
// Tools
// ---------------------------------------------------------------------------

/// The memory tools, in a stable order (appended to the read tools by
/// [`crate::tool_defs`]).
pub fn tool_defs() -> Vec<ToolDef> {
    vec![
        def(
            GET_BRIEFING,
            "What the user's meetings already establish about a recurring meeting, project, \
             customer or person: who takes part, the meeting timeline, decisions, action items \
             with their status, commitments, open questions and key figures. Every fact ends \
             with [seg:ID] markers for the transcript lines it came from (cite those). Use it \
             for questions like \"what did Dana commit to\" or \"where are we on the launch\".",
            vec![
                (
                    "query",
                    string(),
                    "A recurring meeting, project, customer, topic or person's name.",
                    true,
                ),
                (
                    "person",
                    string(),
                    "Only facts this person owns or said.",
                    false,
                ),
                (
                    "since",
                    string(),
                    "Only meetings on/after this date (YYYY-MM-DD).",
                    false,
                ),
            ],
        ),
        def(
            OPEN_ITEMS,
            "Action items and commitments not stated as done, newest meeting first, with owner, \
             status and [seg:ID] markers. Status is as said in that meeting; a later meeting may \
             have closed it.",
            vec![
                (
                    "scope",
                    string(),
                    "\"all\" (default) or a recurring meeting's name.",
                    false,
                ),
                ("owner", string(), "Only items this person owns.", false),
                (
                    "limit",
                    integer(),
                    "Items to return, 1-100 (default 30).",
                    false,
                ),
            ],
        ),
        def(
            WHATS_CHANGED,
            "What is new in one recurring meeting: its meetings since a date (default: its \
             latest meeting) with their decisions, action items and other facts, each with \
             [seg:ID] markers.",
            vec![
                (
                    "series",
                    string(),
                    "The recurring meeting's name (as get_briefing reports it).",
                    true,
                ),
                (
                    "since",
                    string(),
                    "From meetings on/after this date (YYYY-MM-DD); default: the latest meeting.",
                    false,
                ),
            ],
        ),
    ]
}

/// Run a memory tool; `None` when `name` isn't one.
pub(crate) fn call(ctx: &Context<'_>, name: &str, args: &Value) -> Option<Result<ToolOutput>> {
    use crate::render;
    let run = || -> Result<ToolOutput> {
        match name {
            GET_BRIEFING => {
                let b = ctx.get_briefing(&parse_args(name, args)?)?;
                output(&b, render::briefing(&b))
            }
            OPEN_ITEMS => {
                let o = ctx.open_items(&parse_args(name, args)?)?;
                output(&o, render::open_items(&o))
            }
            WHATS_CHANGED => {
                let b = ctx.whats_changed(&parse_args(name, args)?)?;
                output(&b, render::briefing(&b))
            }
            _ => unreachable!("checked below"),
        }
    };
    matches!(name, GET_BRIEFING | OPEN_ITEMS | WHATS_CHANGED).then(run)
}

// ---------------------------------------------------------------------------
// MCP entry points (historical behaviour, kept byte-stable by fly-mcp's
// golden tests)
// ---------------------------------------------------------------------------

/// Entry points the MCP server uses. They keep its historical routing (no
/// person routing, no caps) so its replies don't change; visibility is
/// whatever the [`Context`] was built with (the MCP uses `ShareableOnly`).
pub mod mcp {
    use super::*;

    /// `get_context(query, since)`.
    pub fn get_context(
        ctx: &Context<'_>,
        query: &str,
        since: Option<DateTime<Utc>>,
    ) -> Result<Briefing> {
        ctx.briefing_with(query, None, since, BriefingOptions::default())
    }

    /// `whats_changed(series, since)`.
    pub fn whats_changed(
        ctx: &Context<'_>,
        series: &str,
        since: DateTime<Utc>,
    ) -> Result<Briefing> {
        ctx.whats_changed_with(series, Some(since), BriefingOptions::default())
    }

    /// `open_items(scope)` (no evidence: the MCP prints ids only).
    pub fn open_items(ctx: &Context<'_>, scope: &str) -> Result<OpenItems> {
        ctx.open_items_with(scope, None, None, false)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn meeting(title: &str, attendees: &[&str], days_ago: i64) -> Meeting {
        Meeting {
            id: format!("m-{title}-{days_ago}"),
            title: title.into(),
            note_id: "n".into(),
            attendees: attendees
                .iter()
                .map(|s| fly_core::Attendee::from_legacy(s))
                .collect(),
            attendees_confirmed: false,
            started_at: Utc::now() - chrono::Duration::days(days_ago),
            ended_at: None,
            recording: None,
        }
    }

    #[test]
    fn titles_normalize_dates_numbers_and_case_away() {
        assert_eq!(normalize_title("Weekly Sync 2026-07-01"), "weekly sync");
        assert_eq!(normalize_title("weekly sync #2"), "weekly sync");
        assert_eq!(
            normalize_title("Acme <> Us: Renewal (3/4)"),
            "acme us renewal"
        );
    }

    #[test]
    fn series_groups_by_title_and_participant_overlap() {
        let series = detect_series(vec![
            meeting("Weekly Sync 2026-06-01", &["a@x.com", "b@x.com"], 30),
            meeting("Weekly Sync 2026-06-08", &["a@x.com", "b@x.com"], 23),
            // same title, disjoint people → its own thread
            meeting("Weekly Sync", &["p@y.com", "q@y.com"], 20),
            meeting("Board prep", &["a@x.com"], 10),
        ]);
        let keys: Vec<&str> = series.iter().map(|s| s.key.as_str()).collect();
        assert_eq!(keys, vec!["board prep", "weekly sync", "weekly sync (2)"]);
        let weekly = series.iter().find(|s| s.key == "weekly sync").unwrap();
        assert_eq!(weekly.meetings.len(), 2);
        // chronological inside the series
        assert!(weekly.meetings[0].started_at < weekly.meetings[1].started_at);
    }

    #[test]
    fn empty_attendees_join_on_title_alone() {
        let series = detect_series(vec![
            meeting("1:1 Dana", &[], 14),
            meeting("1:1 Dana", &["dana@x.com"], 7),
        ]);
        assert_eq!(series.len(), 1);
        assert_eq!(series[0].meetings.len(), 2);
    }

    #[test]
    fn matching_requires_all_query_tokens() {
        let series = detect_series(vec![
            meeting("Acme renewal sync", &[], 7),
            meeting("Beta kickoff", &[], 6),
        ]);
        let hits = series_matching(&series, "acme renewal");
        assert_eq!(hits.len(), 1);
        assert_eq!(hits[0].key, "acme renewal sync");
        assert!(series_matching(&series, "gamma").is_empty());
    }

    #[test]
    fn person_keys_match_first_names_and_emails_but_not_other_surnames() {
        let dana = PersonKey::new("Dana Lee").unwrap();
        assert!(dana.matches("Dana"));
        assert!(dana.matches("dana lee"));
        assert!(dana.matches("dana.lee@example.com"));
        assert!(dana.matches("dana@example.com"));
        assert!(!dana.matches("Dana Kim"));
        assert!(!dana.matches("Sam"));
        assert!(!dana.matches(""));
        let short = PersonKey::new("Dana").unwrap();
        assert!(short.matches("Dana Kim") && short.matches("Dana Lee"));
    }

    fn known(names: &[&str]) -> Vec<String> {
        names.iter().map(|s| s.to_string()).collect()
    }

    #[test]
    fn people_are_found_only_when_named_and_known() {
        let k = known(&[
            "Dana Lee",
            "dana.lee@example.com",
            "Sam",
            "Will Ortiz",
            "Speaker 2",
            "You",
        ]);
        assert_eq!(
            find_person("What did Dana commit to?", &k).unwrap().name,
            "Dana Lee"
        );
        assert_eq!(
            find_person("what did dana commit to", &k).unwrap().name,
            "Dana Lee"
        );
        assert_eq!(
            find_person("Anything for Dana Lee?", &k).unwrap().name,
            "Dana Lee"
        );
        assert_eq!(find_person("open items for sam", &k).unwrap().name, "Sam");
        // an unknown name is never invented
        assert!(find_person("What did Priya decide?", &k).is_none());
        // generic labels are nobody
        assert!(find_person("what did speaker 2 say", &k).is_none());
        assert!(find_person("what did you hear", &k).is_none());
        // "will" is a word unless written as a name
        assert!(find_person("What will happen next?", &k).is_none());
        assert!(find_person("what will happen next", &k).is_none());
        assert!(find_person("Will we ship?", &k).is_none());
        assert_eq!(
            find_person("What did Will say?", &k).unwrap().name,
            "Will Ortiz"
        );
        assert_eq!(
            find_person("will ortiz's tasks", &k).unwrap().name,
            "Will Ortiz"
        );
        // owners that name nobody
        assert!(find_person("what did the team agree", &known(&["The team", "We"])).is_none());
        // two different Danas: a bare first name is ambiguous
        let two = known(&["Dana Lee", "Dana Kim"]);
        assert!(find_person("What did Dana say?", &two).is_none());
        assert_eq!(
            find_person("What did Dana Kim say?", &two).unwrap().name,
            "Dana Kim"
        );
    }

    #[test]
    fn series_are_found_by_full_name_or_project_word() {
        let series = detect_series(vec![
            meeting("Launch weekly", &[], 20),
            meeting("Launch weekly", &[], 13),
            meeting("Launch retro", &[], 5),
            meeting("Weekly sync", &[], 4),
            meeting("1:1 Dana", &[], 3),
        ]);
        let none = HashSet::new();
        let keys = |q: &str, p: &HashSet<String>| -> Vec<String> {
            series_in_question(&series, &Words::new(q), p)
                .iter()
                .map(|s| s.key.clone())
                .collect()
        };
        assert_eq!(
            keys("What changed in the launch weekly?", &none),
            vec!["launch weekly"]
        );
        assert_eq!(
            keys("Where are we on the launch?", &none),
            vec!["launch retro", "launch weekly"]
        );
        // an all-generic name only when written out
        assert_eq!(
            keys("what happened in the weekly sync", &none),
            vec!["weekly sync"]
        );
        assert!(keys("what did we decide this weekly cycle", &none).is_empty());
        assert!(keys("what did we decide", &none).is_empty());
        // a 1:1 named after the person isn't a project for a person question
        let dana: HashSet<String> = ["dana".to_string()].into();
        assert!(keys("what did Dana commit to", &dana).is_empty());
        assert_eq!(keys("what did Dana commit to", &none), vec!["dana"]);
    }
}
