//! The durable run log (spec §4.2 step 7, §4.2a #7): one typed record per
//! answer, allowlisted by construction. It holds ids, counts, enums,
//! timings and token counts — no free text: never the question, a passage,
//! the answer, a model name the user typed, or a provider error string
//! (those can contain meeting content). A test serializes a full record
//! and checks every string leaf is a token without spaces.

use serde::{Deserialize, Serialize};

use crate::prefetch::PrefetchStats;
use crate::prompt::RetryReason;
use crate::proposals::RefusalReason;
use crate::router::DateRule;
use crate::scope::{ScopeKind, ScopeRefusal};
use crate::tier::Tier;
use crate::validate::ValidationStats;

pub const RUN_LOG_VERSION: u32 = 1;

/// Where the model runs.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Reach {
    OnDevice,
    /// A local kind of provider (Ollama) at another address.
    LocalRemote,
    Cloud,
}

/// Provider kind, from a fixed list.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ProviderKind {
    Ollama,
    Openai,
    Anthropic,
    Nim,
    Other,
}

impl ProviderKind {
    pub fn from_id(id: &str) -> Self {
        match id {
            "ollama" => Self::Ollama,
            "openai" => Self::Openai,
            "anthropic" => Self::Anthropic,
            "nim" => Self::Nim,
            _ => Self::Other,
        }
    }
}

/// Tool names, from a fixed list (a model-invented name is `unknown`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ToolName {
    SearchPassages,
    ReadSegments,
    ListMeetings,
    GetEvent,
    GetNote,
    GetItems,
    GetBriefing,
    OpenItems,
    WhatsChanged,
    InsertIntoNote,
    AddActionItem,
    Unknown,
}

impl ToolName {
    pub fn from_name(name: &str) -> Self {
        match name {
            "search_passages" => Self::SearchPassages,
            "read_segments" => Self::ReadSegments,
            "list_meetings" => Self::ListMeetings,
            "get_event" => Self::GetEvent,
            "get_note" => Self::GetNote,
            "get_items" => Self::GetItems,
            "get_briefing" => Self::GetBriefing,
            "open_items" => Self::OpenItems,
            "whats_changed" => Self::WhatsChanged,
            "insert_into_note" => Self::InsertIntoNote,
            "add_action_item" => Self::AddActionItem,
            _ => Self::Unknown,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub struct ToolCallLog {
    #[serde(serialize_with = "code::tool")]
    pub tool: ToolName,
    pub ok: bool,
}

/// Why a model turn or the loop stopped.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Stop {
    EndTurn,
    ToolUse,
    MaxTokens,
    CallBudget,
    RoundBudget,
    TokenBudget,
    ToolProtocol,
}

/// How the answer ended.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AnswerStatus {
    /// At least one sentence carries a valid citation.
    Answered,
    /// "Not found in your meetings": the score floor, or the model said so
    /// and cited nothing.
    NotFound,
    /// The model answered but cited nothing it was shown, even after a
    /// retry; shown fully flagged.
    Unverified,
    /// The scope can't be searched (Keep local with an off-device model,
    /// or it doesn't exist).
    Refused,
    /// No usable reply: empty, an error, a broken tool call. Not "not
    /// found".
    ModelFailed,
    Cancelled,
}

/// Why the model failed (typed; the provider's own error text is shown to
/// the user but never logged).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum FailureKind {
    EmptyReply,
    /// The reply stopped at `max_tokens` (or the context limit).
    Truncated,
    NotConfigured,
    Auth,
    Unavailable,
    Network,
    Provider,
    InvalidRequest,
    ToolProtocol,
    BudgetExhausted,
    Storage,
}

impl FailureKind {
    pub fn from_error(e: &fly_llm::LlmError) -> Self {
        use fly_llm::LlmError::*;
        if crate::provider::is_over_budget(e) {
            return Self::BudgetExhausted;
        }
        match e {
            NotConfigured(_) => Self::NotConfigured,
            Auth => Self::Auth,
            Provider(_) => Self::Provider,
            Network(_) => Self::Network,
            Unavailable(_) => Self::Unavailable,
            InvalidRequest(_) => Self::InvalidRequest,
            ToolProtocol(_) => Self::ToolProtocol,
        }
    }
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize)]
pub struct RouterLog {
    #[serde(serialize_with = "code::date_rule")]
    pub date_rule: Option<DateRule>,
    pub people: u32,
    pub folder: bool,
    #[serde(serialize_with = "code::items")]
    pub items_intent: Option<String>,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct FloorLog {
    /// Floor applies to wide scopes only.
    pub applied: bool,
    pub terms: u32,
    pub coverage_pct: u32,
    /// The search's cosine floor was checked too (an unscoped question of
    /// 4+ content words with a measured embedder), on top of the coverage.
    #[serde(default)]
    pub semantic: bool,
    /// Best cosine in scope and the floor it was held to, as percents.
    #[serde(default)]
    pub top_similarity_pct: Option<u32>,
    #[serde(default)]
    pub similarity_floor_pct: Option<u32>,
    pub passed: bool,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Timings {
    pub scope_ms: u64,
    pub embed_ms: u64,
    pub prefetch_ms: u64,
    pub first_token_ms: Option<u64>,
    pub model_ms: u64,
    pub validate_ms: u64,
    pub total_ms: u64,
}

/// Serialize-only: enum values are written as run-log codes (see `code`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct RunLog {
    pub v: u32,
    pub run_id: String,
    pub started_at: String,
    pub tier: Tier,
    #[serde(serialize_with = "code::reach")]
    pub reach: Reach,
    #[serde(serialize_with = "code::provider")]
    pub provider: ProviderKind,
    #[serde(serialize_with = "code::scope")]
    pub scope: ScopeKind,
    pub scope_meetings: u32,
    pub kept_local_excluded: u32,
    #[serde(serialize_with = "code::scope_refusal")]
    pub scope_refusal: Option<ScopeRefusal>,
    pub history_turns: u32,
    pub router: RouterLog,
    pub prefetch: PrefetchStats,
    pub floor: FloorLog,
    pub model_calls: u32,
    pub rounds: u32,
    pub tool_calls: Vec<ToolCallLog>,
    pub stops: Vec<Stop>,
    pub input_tokens_est: u32,
    pub input_tokens: Option<u32>,
    pub output_tokens: Option<u32>,
    pub truncation_suspected: bool,
    #[serde(serialize_with = "code::retries")]
    pub retries: Vec<RetryReason>,
    pub validation: ValidationStats,
    pub cited_segment_ids: Vec<String>,
    pub proposals: u32,
    #[serde(serialize_with = "code::refusals")]
    pub proposal_refusals: Vec<RefusalReason>,
    #[serde(serialize_with = "code::status")]
    pub status: AnswerStatus,
    #[serde(serialize_with = "code::failure")]
    pub failure: Option<FailureKind>,
    pub timings: Timings,
}

impl RunLog {
    pub fn new(
        run_id: String,
        started_at: String,
        tier: Tier,
        reach: Reach,
        provider: ProviderKind,
    ) -> Self {
        Self {
            v: RUN_LOG_VERSION,
            run_id,
            started_at,
            tier,
            reach,
            provider,
            scope: ScopeKind::All,
            scope_meetings: 0,
            kept_local_excluded: 0,
            scope_refusal: None,
            history_turns: 0,
            router: RouterLog::default(),
            prefetch: PrefetchStats::default(),
            floor: FloorLog::default(),
            model_calls: 0,
            rounds: 0,
            tool_calls: Vec::new(),
            stops: Vec::new(),
            input_tokens_est: 0,
            input_tokens: None,
            output_tokens: None,
            truncation_suspected: false,
            retries: Vec::new(),
            validation: ValidationStats::default(),
            cited_segment_ids: Vec::new(),
            proposals: 0,
            proposal_refusals: Vec::new(),
            status: AnswerStatus::ModelFailed,
            failure: None,
            timings: Timings::default(),
        }
    }
}

/// Run-log codes: every enum value is written as `<kind>_<value>`, so no
/// string in a record is ever a bare word (a value like `meeting` or
/// `answered` could otherwise equal a word of the question).
mod code {
    use serde::ser::{Error, SerializeSeq};
    use serde::{Serialize, Serializer};

    fn one<T: Serialize>(prefix: &str, v: &T) -> Result<String, String> {
        match serde_json::to_value(v).map_err(|e| e.to_string())? {
            serde_json::Value::String(s) => Ok(format!("{prefix}_{s}")),
            other => Err(format!("not a unit enum: {other}")),
        }
    }

    fn plain<T: Serialize, S: Serializer>(prefix: &str, v: &T, s: S) -> Result<S::Ok, S::Error> {
        s.serialize_str(&one(prefix, v).map_err(S::Error::custom)?)
    }

    fn opt<T: Serialize, S: Serializer>(
        prefix: &str,
        v: &Option<T>,
        s: S,
    ) -> Result<S::Ok, S::Error> {
        match v {
            Some(x) => plain(prefix, x, s),
            None => s.serialize_none(),
        }
    }

    fn seq<T: Serialize, S: Serializer>(prefix: &str, v: &[T], s: S) -> Result<S::Ok, S::Error> {
        let mut q = s.serialize_seq(Some(v.len()))?;
        for x in v {
            q.serialize_element(&one(prefix, x).map_err(S::Error::custom)?)?;
        }
        q.end()
    }

    pub fn tool<S: Serializer>(v: &super::ToolName, s: S) -> Result<S::Ok, S::Error> {
        plain("tool", v, s)
    }
    pub fn date_rule<S: Serializer>(
        v: &Option<crate::router::DateRule>,
        s: S,
    ) -> Result<S::Ok, S::Error> {
        opt("date", v, s)
    }
    pub fn items<S: Serializer>(v: &Option<String>, s: S) -> Result<S::Ok, S::Error> {
        opt("items", v, s)
    }
    pub fn reach<S: Serializer>(v: &super::Reach, s: S) -> Result<S::Ok, S::Error> {
        plain("reach", v, s)
    }
    pub fn provider<S: Serializer>(v: &super::ProviderKind, s: S) -> Result<S::Ok, S::Error> {
        plain("provider", v, s)
    }
    pub fn scope<S: Serializer>(v: &crate::scope::ScopeKind, s: S) -> Result<S::Ok, S::Error> {
        plain("scope", v, s)
    }
    pub fn scope_refusal<S: Serializer>(
        v: &Option<crate::scope::ScopeRefusal>,
        s: S,
    ) -> Result<S::Ok, S::Error> {
        opt("refused", v, s)
    }
    pub fn retries<S: Serializer>(
        v: &[crate::prompt::RetryReason],
        s: S,
    ) -> Result<S::Ok, S::Error> {
        seq("retry", v, s)
    }
    pub fn refusals<S: Serializer>(
        v: &[crate::proposals::RefusalReason],
        s: S,
    ) -> Result<S::Ok, S::Error> {
        seq("proposal", v, s)
    }
    pub fn status<S: Serializer>(v: &super::AnswerStatus, s: S) -> Result<S::Ok, S::Error> {
        plain("status", v, s)
    }
    pub fn failure<S: Serializer>(v: &Option<super::FailureKind>, s: S) -> Result<S::Ok, S::Error> {
        opt("failure", v, s)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Field names (struct keys), which are fixed by this schema.
    fn is_key(s: &str) -> bool {
        KEYS.with(|k| k.borrow().contains(s))
    }

    thread_local! {
        static KEYS: std::cell::RefCell<std::collections::HashSet<String>> =
            std::cell::RefCell::new(std::collections::HashSet::new());
    }

    fn leaves(v: &serde_json::Value, out: &mut Vec<String>) {
        match v {
            serde_json::Value::String(s) => out.push(s.clone()),
            serde_json::Value::Array(a) => a.iter().for_each(|x| leaves(x, out)),
            serde_json::Value::Object(o) => {
                for (k, x) in o {
                    KEYS.with(|keys| keys.borrow_mut().insert(k.clone()));
                    leaves(x, out);
                }
            }
            _ => {}
        }
    }

    #[test]
    fn a_full_record_holds_no_free_text() {
        let mut log = RunLog::new(
            "6f1c0a3e-1b2c-4d5e-8f90-123456789abc".into(),
            "2026-10-10T17:00:00Z".into(),
            Tier::C,
            Reach::Cloud,
            ProviderKind::Anthropic,
        );
        log.router.date_rule = Some(DateRule::LastWeek);
        log.router.items_intent = Some("action_item".into());
        log.tool_calls.push(ToolCallLog {
            tool: ToolName::Unknown,
            ok: false,
        });
        log.stops.push(Stop::RoundBudget);
        log.retries.push(RetryReason::NoValidCitations);
        log.cited_segment_ids.push("m01-05".into());
        log.proposal_refusals.push(RefusalReason::UnknownOwner);
        log.failure = Some(FailureKind::Provider);
        log.scope_refusal = Some(ScopeRefusal::KeepLocal);
        let mut all = Vec::new();
        leaves(&serde_json::to_value(&log).unwrap(), &mut all);
        for s in all {
            assert!(
                !s.chars().any(char::is_whitespace),
                "free text in the run log: {s:?}"
            );
            assert!(s.len() <= 64, "{s:?}");
            // Values are never a bare word a question could contain.
            let coded = s.len() <= 1
                || s.contains(['_', '-', ':'])
                || s.chars().any(|c| c.is_ascii_digit());
            assert!(coded || is_key(&s), "bare word in the run log: {s:?}");
        }
    }
}
