//! The request-recording provider every scenario runs through, and the
//! replay files recorded real-model runs are stored as.

use std::collections::VecDeque;
use std::sync::{Arc, Mutex};

use chrono::{DateTime, Utc};
use fly_llm::{
    Capabilities, ChatRequest, ChatResponse, ChatStream, ContentBlock, ConverseRequest,
    LLMProvider, LlmError, ThinkingMode,
};
use futures_util::StreamExt as _;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use sha2::{Digest as _, Sha256};

use super::api::{ProviderErrorKind, Tier};

/// Where a scenario's provider runs.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ProviderKind {
    /// Ollama on this machine: `is_local()` and on device.
    Local,
    /// Ollama at a remote URL: `is_local()` but NOT on device, so
    /// Keep-local content must not reach it (leak-scanned like cloud).
    RemoteLocal,
    /// A cloud provider.
    Cloud,
}

impl ProviderKind {
    pub fn on_device(self) -> bool {
        self == ProviderKind::Local
    }

    pub fn as_str(self) -> &'static str {
        match self {
            ProviderKind::Local => "local",
            ProviderKind::RemoteLocal => "remote_local",
            ProviderKind::Cloud => "cloud",
        }
    }

    /// The provider id and model a scripted mock reports (the app's
    /// defaults), so the harness picks the same prompt profile it would.
    fn scripted_identity(self) -> (&'static str, &'static str) {
        match self {
            ProviderKind::Local | ProviderKind::RemoteLocal => ("ollama", "llama3.1"),
            ProviderKind::Cloud => ("anthropic", fly_llm::anthropic::ANTHROPIC_DEFAULT_MODEL),
        }
    }

    /// What the real provider reports for that model (Ollama as probed for
    /// a tool-capable model; Anthropic's current generation).
    fn scripted_capabilities(self) -> Capabilities {
        match self {
            ProviderKind::Local | ProviderKind::RemoteLocal => Capabilities {
                tools: true,
                tool_choice_none: false,
                tool_choice_required: false,
                tool_choice_required_thinking_off: false,
                strict_tools: false,
                max_strict_tools: 0,
                streaming: true,
                thinking_control: true,
                structured_output: true,
                probed: true,
            },
            ProviderKind::Cloud => Capabilities {
                tools: true,
                tool_choice_none: true,
                tool_choice_required: false,
                tool_choice_required_thinking_off: false,
                strict_tools: true,
                max_strict_tools: 20,
                streaming: true,
                thinking_control: true,
                structured_output: true,
                probed: false,
            },
        }
    }
}

/// One step of a scripted model: a reply, or a provider error.
#[derive(Debug, Clone, PartialEq)]
pub enum ScriptStep {
    Reply(ChatResponse),
    Fail {
        kind: ProviderErrorKind,
        message: String,
    },
}

impl ScriptStep {
    /// A script entry: a `ChatResponse` object, or
    /// `{"error": "<kind>", "message": "..."}`.
    pub fn from_json(v: &Value) -> anyhow::Result<Self> {
        if let Some(kind) = v.get("error") {
            #[derive(Deserialize)]
            #[serde(deny_unknown_fields)]
            struct Fail {
                error: ProviderErrorKind,
                #[serde(default)]
                message: String,
            }
            let _ = kind;
            let f: Fail = serde_json::from_value(v.clone())?;
            return Ok(ScriptStep::Fail {
                kind: f.error,
                message: f.message,
            });
        }
        Ok(ScriptStep::Reply(serde_json::from_value(v.clone())?))
    }
}

/// A provider that records every request. Scripted, it answers from a fixed
/// list of steps; wrapping a real provider (built by the app's
/// `build_provider`, never here), it forwards and keeps the replies.
pub struct RecordingProvider {
    kind: ProviderKind,
    provider_id: &'static str,
    model: String,
    caps: Capabilities,
    script: Mutex<VecDeque<ScriptStep>>,
    inner: Option<Arc<dyn LLMProvider>>,
    seen: Mutex<Vec<ConverseRequest>>,
    recorded: Arc<Mutex<Vec<ChatResponse>>>,
}

impl RecordingProvider {
    /// A scripted model with the app's default identity for `kind`.
    pub fn scripted(steps: Vec<ScriptStep>, kind: ProviderKind) -> Self {
        let (id, model) = kind.scripted_identity();
        Self {
            kind,
            provider_id: id,
            model: model.into(),
            caps: kind.scripted_capabilities(),
            script: Mutex::new(steps.into()),
            inner: None,
            seen: Mutex::new(Vec::new()),
            recorded: Arc::new(Mutex::new(Vec::new())),
        }
    }

    /// A replay: the recorded identity, capabilities and replies.
    pub fn replaying(file: &ReplayFile, steps: Vec<ScriptStep>, kind: ProviderKind) -> Self {
        Self {
            kind,
            provider_id: static_provider_id(&file.provider),
            model: file.model.clone(),
            caps: file.capabilities,
            script: Mutex::new(steps.into()),
            inner: None,
            seen: Mutex::new(Vec::new()),
            recorded: Arc::new(Mutex::new(Vec::new())),
        }
    }

    /// Record mode around a real provider. `on_device` comes from
    /// `build_provider`, as the harness gets it.
    pub fn wrap(inner: Arc<dyn LLMProvider>, on_device: bool) -> Self {
        let kind = match (inner.is_local(), on_device) {
            (true, true) => ProviderKind::Local,
            (true, false) => ProviderKind::RemoteLocal,
            _ => ProviderKind::Cloud,
        };
        Self {
            kind,
            provider_id: inner.id(),
            model: inner.model().to_string(),
            caps: inner.capabilities(),
            script: Mutex::new(VecDeque::new()),
            inner: Some(inner),
            seen: Mutex::new(Vec::new()),
            recorded: Arc::new(Mutex::new(Vec::new())),
        }
    }

    pub fn kind(&self) -> ProviderKind {
        self.kind
    }

    pub fn requests(&self) -> Vec<ConverseRequest> {
        self.seen.lock().expect("lock").clone()
    }

    /// Replies a wrapped provider produced (record mode).
    pub fn recorded(&self) -> Vec<ChatResponse> {
        self.recorded.lock().expect("lock").clone()
    }

    /// A replay file for this run (record mode). `normalize` maps database
    /// ids back to fixture ids (`corpus::Db::normalize_ids`) so the
    /// fingerprint doesn't depend on the database build. Capabilities should
    /// be the probed ones (`provider.probe_capabilities()`).
    pub fn to_replay(
        &self,
        scenario: &str,
        tier: Tier,
        capabilities: Capabilities,
        normalize: &dyn Fn(&str) -> String,
    ) -> ReplayFile {
        let requests = self.requests();
        let first = requests.first();
        ReplayFile {
            format: REPLAY_FORMAT,
            scenario: scenario.into(),
            model: self.model.clone(),
            provider: self.provider_id.into(),
            capabilities,
            profile: ReplayProfile {
                thinking: first.map(|r| r.thinking).unwrap_or_default(),
                effort: first.and_then(|r| r.effort.clone()),
            },
            tier,
            requests_fingerprint: fingerprint(&requests, normalize),
            recorded_at: Utc::now(),
            responses: self.recorded().into_iter().map(strip_thinking).collect(),
        }
    }

    fn next_step(&self) -> fly_llm::Result<ChatResponse> {
        match self.script.lock().expect("lock").pop_front() {
            Some(ScriptStep::Reply(r)) => Ok(r),
            Some(ScriptStep::Fail { kind, message }) => Err(kind.to_error(&message)),
            None => Err(LlmError::Provider("replay script exhausted".into())),
        }
    }
}

fn static_provider_id(id: &str) -> &'static str {
    ["ollama", "openai", "anthropic", "nim"]
        .into_iter()
        .find(|k| *k == id)
        .unwrap_or("eval-replay")
}

#[async_trait::async_trait]
impl LLMProvider for RecordingProvider {
    fn id(&self) -> &'static str {
        self.provider_id
    }

    fn model(&self) -> &str {
        &self.model
    }

    fn is_local(&self) -> bool {
        self.kind != ProviderKind::Cloud
    }

    async fn chat(&self, req: ChatRequest) -> fly_llm::Result<String> {
        Ok(self.converse(req.into()).await?.text())
    }

    async fn test_connection(&self) -> fly_llm::Result<()> {
        Ok(())
    }

    fn capabilities(&self) -> Capabilities {
        self.caps
    }

    async fn probe_capabilities(&self) -> fly_llm::Result<Capabilities> {
        match &self.inner {
            Some(p) => p.probe_capabilities().await,
            None => Ok(self.caps),
        }
    }

    async fn converse(&self, req: ConverseRequest) -> fly_llm::Result<ChatResponse> {
        self.seen.lock().expect("lock").push(req.clone());
        if let Some(inner) = &self.inner {
            let resp = inner.converse(req).await?;
            self.recorded.lock().expect("lock").push(resp.clone());
            return Ok(resp);
        }
        self.next_step()
    }

    async fn chat_stream(&self, req: ConverseRequest) -> fly_llm::Result<ChatStream> {
        self.seen.lock().expect("lock").push(req.clone());
        if let Some(inner) = &self.inner {
            let stream = inner.chat_stream(req).await?;
            let recorded = self.recorded.clone();
            let mut acc = Some(fly_llm::stream::Accumulator::default());
            return Ok(Box::pin(stream.map(move |ev| {
                if let Ok(e) = &ev {
                    let done = matches!(e, fly_llm::StreamEvent::Done { .. });
                    if let Some(a) = acc.as_mut() {
                        a.push(e.clone());
                    }
                    if done {
                        if let Some(Ok(resp)) = acc.take().map(|a| a.finish()) {
                            recorded.lock().expect("lock").push(resp);
                        }
                    }
                }
                ev
            })));
        }
        Ok(fly_llm::stream::replay(self.next_step()?))
    }
}

/// The current replay file format.
pub const REPLAY_FORMAT: u32 = 2;

/// The prompt profile a replay was recorded under.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ReplayProfile {
    pub thinking: ThinkingMode,
    #[serde(default)]
    pub effort: Option<String>,
}

/// A recorded real-model run, replayed in CI
/// (`suites/ask_contract/replays/<model>/<scenario>.json`).
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ReplayFile {
    pub format: u32,
    pub scenario: String,
    /// The resolved model id (`provider.model()`).
    pub model: String,
    /// The provider id (`provider.id()`).
    pub provider: String,
    /// What the provider reported when probed.
    pub capabilities: Capabilities,
    pub profile: ReplayProfile,
    pub tier: Tier,
    /// SHA-256 of the requests the harness sent, database ids normalized.
    /// A replay whose requests no longer match is stale: the model saw
    /// something else, so its replies can't be scored against this run.
    pub requests_fingerprint: String,
    pub recorded_at: DateTime<Utc>,
    /// The replies, thinking removed.
    pub responses: Vec<ChatResponse>,
}

impl ReplayFile {
    /// The quality group a replay scores in: `replay:<model>@<tier>`.
    pub fn group(&self) -> String {
        format!("replay:{}@{}", self.model, self.tier.as_str())
    }
}

/// SHA-256 over the requests, as JSON with database ids normalized.
pub fn fingerprint(requests: &[ConverseRequest], normalize: &dyn Fn(&str) -> String) -> String {
    let mut h = Sha256::new();
    for r in requests {
        let text = serde_json::to_string(r).unwrap_or_default();
        h.update(normalize(&text).as_bytes());
        h.update(b"\n");
    }
    h.finalize().iter().map(|b| format!("{b:02x}")).collect()
}

/// A reply without reasoning: thinking blocks dropped and `<think>` spans
/// removed from text.
pub fn strip_thinking(mut resp: ChatResponse) -> ChatResponse {
    resp.blocks.retain(|b| match b {
        ContentBlock::Opaque { raw, .. } => !raw
            .get("type")
            .and_then(Value::as_str)
            .is_some_and(|t| t.contains("thinking")),
        _ => true,
    });
    for b in &mut resp.blocks {
        if let ContentBlock::Text { text } = b {
            *text = strip_think_tags(text);
        }
    }
    resp
}

fn strip_think_tags(text: &str) -> String {
    let mut out = String::new();
    let mut rest = text;
    while let Some(i) = rest.find("<think>") {
        out.push_str(&rest[..i]);
        match rest[i..].find("</think>") {
            Some(j) => rest = &rest[i + j + "</think>".len()..],
            None => {
                rest = "";
                break;
            }
        }
    }
    out.push_str(rest);
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use fly_llm::{Message, StopReason, Usage};

    #[test]
    fn script_steps_parse_replies_and_errors() {
        let r = ScriptStep::from_json(&serde_json::json!({
            "blocks": [{"type": "text", "text": "hi"}], "stop_reason": "end_turn"
        }))
        .unwrap();
        assert!(matches!(r, ScriptStep::Reply(_)));
        let e = ScriptStep::from_json(&serde_json::json!({"error": "network", "message": "x"}))
            .unwrap();
        assert_eq!(
            e,
            ScriptStep::Fail {
                kind: ProviderErrorKind::Network,
                message: "x".into()
            }
        );
        assert!(ScriptStep::from_json(&serde_json::json!({"error": "bogus"})).is_err());
    }

    #[test]
    fn scripted_errors_and_streams() {
        let p = RecordingProvider::scripted(
            vec![
                ScriptStep::Fail {
                    kind: ProviderErrorKind::Unavailable,
                    message: "down".into(),
                },
                ScriptStep::Reply(ChatResponse::text_reply("ok")),
            ],
            ProviderKind::RemoteLocal,
        );
        assert!(p.is_local());
        assert_eq!(p.kind(), ProviderKind::RemoteLocal);
        let rt = tokio::runtime::Builder::new_current_thread()
            .build()
            .unwrap();
        rt.block_on(async {
            let req = ConverseRequest {
                messages: vec![Message::user("q")],
                ..Default::default()
            };
            assert!(matches!(
                p.converse(req.clone()).await,
                Err(LlmError::Unavailable(_))
            ));
            let s = p.chat_stream(req).await.unwrap();
            let resp = fly_llm::stream::collect(s).await.unwrap();
            assert_eq!(resp.text(), "ok");
        });
        assert_eq!(p.requests().len(), 2);
    }

    #[test]
    fn thinking_is_stripped_and_fingerprints_normalize() {
        let resp = ChatResponse {
            blocks: vec![
                ContentBlock::Opaque {
                    provider: "anthropic".into(),
                    raw: serde_json::json!({"type": "thinking", "thinking": "secret"}),
                },
                ContentBlock::text("<think>hmm</think>Answer."),
            ],
            stop_reason: StopReason::EndTurn,
            usage: Usage::default(),
        };
        let s = strip_thinking(resp);
        assert_eq!(s.blocks.len(), 1);
        assert_eq!(s.text(), "Answer.");
        let a = ConverseRequest {
            messages: vec![Message::user("id-1")],
            ..Default::default()
        };
        let b = ConverseRequest {
            messages: vec![Message::user("id-2")],
            ..Default::default()
        };
        let norm = |t: &str| t.replace("id-1", "X").replace("id-2", "X");
        assert_eq!(
            fingerprint(std::slice::from_ref(&a), &norm),
            fingerprint(&[b], &norm)
        );
        assert_ne!(
            fingerprint(std::slice::from_ref(&a), &norm),
            fingerprint(&[a], &|t| t.into())
        );
    }
}
