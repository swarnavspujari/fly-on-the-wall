//! The Ask harness contract: what the eval hands the harness
//! ([`AskRequest`]) and what it expects back ([`AskAnswer`]). The harness
//! implements [`AskHarness`]; nothing else here is for it to implement.
//!
//! Rules the scorer checks, whatever the harness reports about itself:
//! * Keep-local visibility comes from [`AskRequest::on_device`] only
//!   (`Visibility::All` iff on device). `provider.is_local()` is not enough:
//!   an Ollama at a remote URL is local in kind but sends data off the
//!   device. The app sets `on_device` from `build_provider`.
//! * Retrieved segments reach the model as `[seg:ID]` markers in system,
//!   user or tool-result content (fly-context's rendering). Segment ids are
//!   the database's ids (UUIDs in production and in the eval database).
//! * The harness never constructs a provider (`build_provider` is the only
//!   constructor), never writes to storage or settings, and stays inside
//!   [`AskRequest::budget`].

use chrono::{DateTime, Utc};
use fly_llm::{ConverseRequest, LLMProvider, LlmError};
use fly_storage::Storage;
use serde::{Deserialize, Serialize};
use serde_json::Value;

/// Model-capability tier (spec §4.2): S makes no model-chosen tool calls,
/// M ≤ 3 calls in 2 tool rounds, C ≤ 8 calls in 5 rounds.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub enum Tier {
    S,
    M,
    C,
}

impl Tier {
    pub const ALL: [Tier; 3] = [Tier::S, Tier::M, Tier::C];

    pub fn as_str(self) -> &'static str {
        match self {
            Tier::S => "S",
            Tier::M => "M",
            Tier::C => "C",
        }
    }
}

/// The budget for one Ask at its tier (`thresholds.toml`,
/// `[ask_contract.tiers.<T>.budget]`). The harness must stay inside it; the
/// scorer counts what it actually sent (`tier_budget_violations`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AskBudget {
    /// Model calls (requests sent), retries included.
    pub model_calls: u32,
    /// Tool rounds: model replies that asked for a tool. 0 means no tools
    /// offered and no tool results sent (tier S).
    pub tool_rounds: u32,
    /// Estimated input tokens summed over every request ([`estimate_tokens`]);
    /// `None` = no cap beyond `request_tokens` per call.
    #[serde(default)]
    pub input_tokens: Option<u32>,
    /// Estimated tokens of the largest single request.
    pub request_tokens: u32,
}

/// What the question is about.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub enum AskScope {
    All,
    /// A meeting id (Ask from inside a note).
    Meeting(String),
    /// A folder id (subfolders included).
    Folder(String),
}

/// An earlier turn of the same conversation (multi-turn Ask).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct AskTurn {
    pub question: String,
    /// The answer as it was displayed; `[seg:ID]` markers may stay in it.
    pub answer: String,
    /// The segment ids that answer cited.
    #[serde(default)]
    pub citations: Vec<String>,
    /// That turn ran on this device, so its answer may hold Keep-local
    /// content that must not reach a provider that isn't on device.
    pub on_device: bool,
}

/// Embeds a search query the way the app does (the vector half of
/// fly-context's hybrid search). The eval passes one that matches the
/// vectors stored in its database.
#[async_trait::async_trait(?Send)]
pub trait QueryEmbedder {
    /// The model id stored on chunks (`fly_context::QueryVector::model`).
    fn model(&self) -> &str;
    /// Embed a query; applies the app's query prompt itself
    /// (`fly_core::embed_prompt::query`), so pass the raw query.
    async fn embed_query(&self, query: &str) -> std::result::Result<Vec<f32>, String>;
}

/// One Ask, as the eval hands it to the harness.
pub struct AskRequest<'a> {
    /// The fixture database (fresh, or restored, per scenario).
    pub storage: &'a Storage,
    /// A recording mock (see `RecordingProvider`).
    pub provider: &'a dyn LLMProvider,
    /// True only when the provider runs on this machine (`build_provider`'s
    /// `on_device`). The only source of Keep-local visibility.
    pub on_device: bool,
    pub embedder: &'a dyn QueryEmbedder,
    pub question: &'a str,
    /// Earlier turns, oldest first (empty for a first question).
    pub history: &'a [AskTurn],
    pub scope: AskScope,
    /// Forced tier (the app measures it; the eval pins it per scenario).
    pub tier: Tier,
    pub budget: AskBudget,
    /// The clock for relative dates ("last week").
    pub now: DateTime<Utc>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SentenceStatus {
    /// Shown as sourced: every citation valid, claims checked.
    Sourced,
    /// Styled unsourced (no valid citation).
    Unsourced,
    /// Cited but flagged (e.g. failed the claim check).
    Flagged,
}

/// One sentence as the user would see it.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AnswerSentence {
    /// Displayed text (Markdown). `[seg:ID]` markers may stay in it.
    pub text: String,
    /// The segment ids this sentence is shown citing.
    pub citations: Vec<String>,
    pub status: SentenceStatus,
}

/// A source as listed under the answer (spec §4.2a #10: sources render
/// before the answer).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SourceView {
    pub meeting_id: String,
    pub segment_ids: Vec<String>,
    /// The displayed label (title, time, snippet).
    pub label: String,
}

/// A proposal (spec §4.2 step 6, §4.2a #5): rendered as a button with a
/// preview, never executed by the harness.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ProposalView {
    pub tool: String,
    /// What the button's preview shows (Markdown).
    pub preview: String,
    pub executed: bool,
}

/// Why an Ask produced no usable answer, distinct from "not found": an
/// empty local reply is a model failure, not a correct refusal.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", tag = "failure", content = "kind")]
pub enum ModelFailure {
    /// No text and no tool call (also after a tool result).
    EmptyReply,
    /// The reply stopped at `max_tokens` or the context limit.
    Truncated,
    /// The provider kept failing after the harness's bounded retries.
    Provider(ProviderErrorKind),
    /// The tier budget ran out before an answer.
    BudgetExhausted,
    /// The model broke the tool protocol and the repair failed.
    ToolProtocol,
}

/// A provider error, typed (spec §4.2a #7: never the raw error text).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ProviderErrorKind {
    NotConfigured,
    Auth,
    Provider,
    Network,
    Unavailable,
    InvalidRequest,
}

impl ProviderErrorKind {
    /// The kind of a provider error; `None` for a tool-protocol failure.
    pub fn of(e: &LlmError) -> Option<Self> {
        Some(match e {
            LlmError::NotConfigured(_) => Self::NotConfigured,
            LlmError::Auth => Self::Auth,
            LlmError::Provider(_) => Self::Provider,
            LlmError::Network(_) => Self::Network,
            LlmError::Unavailable(_) => Self::Unavailable,
            LlmError::InvalidRequest(_) => Self::InvalidRequest,
            LlmError::ToolProtocol(_) => return None,
        })
    }

    /// An error of this kind, as a scripted provider returns it.
    pub fn to_error(self, message: &str) -> LlmError {
        let m = message.to_string();
        match self {
            Self::NotConfigured => LlmError::NotConfigured(m),
            Self::Auth => LlmError::Auth,
            Self::Provider => LlmError::Provider(m),
            Self::Network => LlmError::Network(m),
            Self::Unavailable => LlmError::Unavailable(m),
            Self::InvalidRequest => LlmError::InvalidRequest(m),
        }
    }
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", tag = "outcome", content = "detail")]
pub enum AskOutcome {
    /// An answer (its sentences say what's sourced).
    #[default]
    Answered,
    /// "Not found in your meetings", with zero citations.
    NotFound,
    /// No usable answer because the model or provider failed.
    ModelFailed(ModelFailure),
}

/// Wall-clock timings the harness measured (report-only; mocks answer
/// instantly, so these matter in local_bench with a real provider).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct AskTiming {
    /// Ask start to the first answer token shown.
    pub first_token_ms: Option<u64>,
    /// Ask start to the final answer.
    pub total_ms: u64,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct AskAnswer {
    pub outcome: AskOutcome,
    pub sentences: Vec<AnswerSentence>,
    /// The sources list shown with the answer.
    pub sources: Vec<SourceView>,
    /// The trace lines shown while answering ("Searched your meetings", ...).
    pub trace: Vec<String>,
    pub proposals: Vec<ProposalView>,
    /// Every intermediate rendering of the answer shown while streaming,
    /// in order (empty when the harness doesn't stream).
    pub display_frames: Vec<String>,
    pub timing: AskTiming,
    /// The durable run log for this answer (spec §4.2a #7): typed values
    /// only, no free text.
    pub run_log: Value,
}

impl AskAnswer {
    pub fn not_found(&self) -> bool {
        self.outcome == AskOutcome::NotFound
    }
}

/// The contract.
#[async_trait::async_trait(?Send)]
pub trait AskHarness {
    fn name(&self) -> &str;
    /// `Err` is a harness bug (the scenario counts as errored); model and
    /// provider failures are an `Ok` answer with [`AskOutcome::ModelFailed`].
    async fn ask(&self, req: AskRequest<'_>) -> std::result::Result<AskAnswer, String>;
}

/// The eval's token estimate: characters / 4, rounded up. Deterministic and
/// model-free; it's what the budgets and size gates are measured in.
pub fn estimate_tokens(text: &str) -> u32 {
    (text.chars().count() as u32).div_ceil(4)
}

/// Estimated input tokens of one request: every message's text, tool
/// calls and results, plus the tool definitions offered.
pub fn request_tokens(req: &ConverseRequest) -> u32 {
    use fly_llm::ContentBlock;
    let mut n = 0;
    for m in &req.messages {
        for b in &m.content {
            n += match b {
                ContentBlock::Text { text } => estimate_tokens(text),
                ContentBlock::ToolUse { name, input, .. } => {
                    estimate_tokens(name) + estimate_tokens(&input.to_string())
                }
                ContentBlock::ToolResult { content, .. } => estimate_tokens(content),
                ContentBlock::Opaque { .. } => 0,
            };
        }
    }
    for t in &req.tools {
        n += estimate_tokens(&t.name)
            + estimate_tokens(&t.description)
            + estimate_tokens(&t.input_schema.to_string());
    }
    n
}
