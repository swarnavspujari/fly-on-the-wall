//! LLM benchmark harness: runs the app's four LLM tasks (Enhance, Polish,
//! Extraction, Ask) over the committed fixture transcripts against a real
//! provider+model, and scores contract compliance, latency, and (for local
//! models) peak Ollama RSS. A second phase judges candidate outputs against
//! a reference model's outputs with an LLM judge. `#[ignore]`d: needs a
//! running provider and (for anthropic / the judge) an API key.
//!
//! It exercises the SHIPPED prompt/parse/guard code paths (fly-core enhance,
//! src-tauri extraction, llm_commands::build_ask_system), not a parallel
//! reimplementation — so its results transfer to the app.
//!
//! Generate phase (one line per provider:model; model may contain ':'):
//!   LLM_BENCH_MODELS="ollama:llama3.1"            # required
//!   LLM_BENCH_OUT=target/llm-bench                # default
//!   LLM_BENCH_TASKS=enhance,polish,extract,ask    # default all
//!   LLM_BENCH_VARIANT=default|simple|fewshot|nopreamble   # profile experiment
//!   ANTHROPIC_API_KEY=sk-ant-...   # anthropic models; falls back to the
//!                                  # app keychain (service com.flyonthewall.app)
//!     cargo test -p fly-app --test llm_bench -- --ignored --nocapture
//!
//! Judge phase (scores every run dir in LLM_BENCH_OUT against the reference):
//!   LLM_BENCH_PHASE=judge
//!   LLM_BENCH_REF=anthropic-claude-sonnet-5-default   # default
//!   LLM_BENCH_JUDGE_MODEL=claude-sonnet-5             # default
//!     cargo test -p fly-app --test llm_bench -- --ignored --nocapture
//!
//! Results land in LLM_BENCH_OUT/<provider>-<model>-<variant>/results.json
//! (raw outputs verbatim, so contract failures can be quoted) and judged.json.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::Arc;
use std::time::Instant;

use fly_core::prompt_profile::{PromptProfile, DEFAULT_PROFILE};
use fly_core::{enhance, Meeting, Note, Template, Transcript};
use fly_llm::{ChatMessage, ChatRequest, LLMProvider, ThinkingMode};
use serde::{Deserialize, Serialize};

// ---------------------------------------------------------------------------
// Fixtures
// ---------------------------------------------------------------------------

#[derive(Deserialize)]
struct Manifest {
    template: Template,
    fixtures: Vec<FixtureSpec>,
    questions: Vec<QuestionSpec>,
}

#[derive(Deserialize)]
struct FixtureSpec {
    id: String,
    file: String,
    meeting_title: String,
    attendees: Vec<String>,
    note_title: String,
    scratchpad: String,
}

#[derive(Deserialize, Clone)]
struct QuestionSpec {
    fixture: String,
    id: String,
    kind: String,
    text: String,
}

struct Fixture {
    spec: FixtureSpec,
    transcript: Transcript,
}

fn bench_dir() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("fixtures/bench")
}

fn load_manifest() -> (Manifest, Vec<Fixture>) {
    let manifest: Manifest = serde_json::from_str(
        &std::fs::read_to_string(bench_dir().join("manifest.json")).expect("manifest.json"),
    )
    .expect("manifest parses");
    let fixtures = manifest
        .fixtures
        .iter()
        .map(|spec| {
            let transcript: Transcript = serde_json::from_str(
                &std::fs::read_to_string(bench_dir().join(&spec.file))
                    .unwrap_or_else(|e| panic!("{}: {e}", spec.file)),
            )
            .unwrap_or_else(|e| panic!("{} parses: {e}", spec.file));
            Fixture {
                spec: FixtureSpec {
                    id: spec.id.clone(),
                    file: spec.file.clone(),
                    meeting_title: spec.meeting_title.clone(),
                    attendees: spec.attendees.clone(),
                    note_title: spec.note_title.clone(),
                    scratchpad: spec.scratchpad.clone(),
                },
                transcript,
            }
        })
        .collect();
    (manifest, fixtures)
}

fn fixture_note(spec: &FixtureSpec, meeting_id: &str) -> Note {
    Note {
        id: format!("bench-note-{}", spec.id),
        title: spec.note_title.clone(),
        folder_id: None,
        meeting_id: Some(meeting_id.to_string()),
        scratchpad: spec.scratchpad.clone(),
        blocks: vec![],
        attachments: vec![],
        created_at: chrono::Utc::now(),
        updated_at: chrono::Utc::now(),
    }
}

fn fixture_meeting(spec: &FixtureSpec, meeting_id: &str) -> Meeting {
    Meeting {
        id: meeting_id.to_string(),
        title: spec.meeting_title.clone(),
        note_id: format!("bench-note-{}", spec.id),
        attendees: spec.attendees.clone(),
        started_at: "2026-07-01T17:00:00Z".parse().unwrap(),
        ended_at: None,
        recording: None,
    }
}

/// Not ignored: keeps the committed fixtures honest on every test run.
#[test]
fn bench_fixtures_parse() {
    let (manifest, fixtures) = load_manifest();
    assert_eq!(fixtures.len(), 3);
    assert_eq!(manifest.questions.len(), 15);
    for f in &fixtures {
        assert!(f.transcript.segments.len() >= 20, "{}", f.spec.id);
        assert!(!f.transcript.speakers.is_empty());
        // Every question's fixture id resolves.
        assert!(manifest.fixtures.iter().any(|m| m.id == f.spec.id));
    }
    for q in &manifest.questions {
        assert!(
            manifest.fixtures.iter().any(|m| m.id == q.fixture),
            "question {} references unknown fixture {}",
            q.id,
            q.fixture
        );
    }
}

// ---------------------------------------------------------------------------
// Providers / profiles
// ---------------------------------------------------------------------------

fn anthropic_key() -> Result<String, String> {
    if let Ok(k) = std::env::var("ANTHROPIC_API_KEY") {
        if !k.trim().is_empty() {
            return Ok(k);
        }
    }
    // Same source the app itself uses.
    use fly_secrets::SecretStore;
    fly_secrets::KeychainSecretStore::new()
        .get(fly_secrets::keys::ANTHROPIC_API_KEY)
        .map_err(|e| e.to_string())?
        .ok_or_else(|| "no ANTHROPIC_API_KEY env var and no key in the app keychain".into())
}

fn build_provider(provider: &str, model: &str) -> Result<Box<dyn LLMProvider>, String> {
    match provider {
        "ollama" => Ok(Box::new(
            fly_llm::openai_compat::OpenAiCompatProvider::ollama(None, model.to_string()),
        )),
        "anthropic" => Ok(Box::new(fly_llm::anthropic::AnthropicProvider::new(
            anthropic_key()?,
            model.to_string(),
        ))),
        other => Err(format!("unsupported provider {other}")),
    }
}

const NOPREAMBLE_PREAMBLE: &str = "Output the requested content only. Do not write any \
introduction, explanation, or closing remarks.";

const FEWSHOT_PREAMBLE: &str = "When a JSON response is requested, reply with the JSON value \
alone. Worked example — request: 'Return ONLY a JSON array of {\"kind\", \"text\"} facts'; a \
valid response is exactly:\n[{\"kind\":\"decision\",\"text\":\"Ship v2 on Friday.\"}]\nand an \
INVALID response is:\nSure! Here are the facts:\n```json\n[...]\n```\nFollow the same rule for \
the schema requested below.";

fn variant_profile(variant: &str) -> PromptProfile {
    match variant {
        "simple" => PromptProfile {
            simplified_contract: true,
            ..DEFAULT_PROFILE
        },
        "fewshot" => PromptProfile {
            system_preamble: Some(FEWSHOT_PREAMBLE),
            ..DEFAULT_PROFILE
        },
        "nopreamble" => PromptProfile {
            system_preamble: Some(NOPREAMBLE_PREAMBLE),
            ..DEFAULT_PROFILE
        },
        _ => DEFAULT_PROFILE,
    }
}

async fn chat_with_retry(provider: &dyn LLMProvider, req: ChatRequest) -> Result<String, String> {
    let mut last = String::new();
    for attempt in 1..=3u64 {
        match provider.chat(req.clone()).await {
            Ok(out) => return Ok(out),
            Err(e) => {
                last = e.to_string();
                eprintln!("    chat error (attempt {attempt}/3): {last}");
                tokio::time::sleep(std::time::Duration::from_secs(5 * attempt)).await;
            }
        }
    }
    Err(last)
}

// ---------------------------------------------------------------------------
// Peak-RSS sampler. Ollama hosts the model in a separate runner process named
// `llama-server` (older versions: `ollama_llama_server`), so the filter must
// cover both the supervisor and the runner — the `ollama` process alone sits
// at ~40 MB while the runner holds the gigabytes.
// ---------------------------------------------------------------------------

struct RssSampler {
    stop: Arc<AtomicBool>,
    peak: Arc<AtomicU64>,
    handle: Option<std::thread::JoinHandle<()>>,
}

impl RssSampler {
    fn start(enabled: bool) -> Self {
        let stop = Arc::new(AtomicBool::new(false));
        let peak = Arc::new(AtomicU64::new(0));
        let handle = if enabled {
            let stop2 = stop.clone();
            let peak2 = peak.clone();
            Some(std::thread::spawn(move || {
                let mut sys = sysinfo::System::new();
                while !stop2.load(Ordering::Relaxed) {
                    sys.refresh_processes(sysinfo::ProcessesToUpdate::All, true);
                    let total: u64 = sys
                        .processes()
                        .values()
                        .filter(|p| {
                            let name = p.name().to_string_lossy().to_lowercase();
                            name.contains("ollama") || name.contains("llama-server")
                        })
                        .map(|p| p.memory())
                        .sum();
                    peak2.fetch_max(total, Ordering::Relaxed);
                    std::thread::sleep(std::time::Duration::from_millis(250));
                }
            }))
        } else {
            None
        };
        Self { stop, peak, handle }
    }

    /// Stop sampling; peak RSS in MB (None when sampling was disabled).
    fn finish(mut self) -> Option<u64> {
        self.stop.store(true, Ordering::Relaxed);
        let handle = self.handle.take()?;
        let _ = handle.join();
        Some(self.peak.load(Ordering::Relaxed) / (1024 * 1024))
    }
}

// ---------------------------------------------------------------------------
// Result records
// ---------------------------------------------------------------------------

#[derive(Serialize, Deserialize, Default)]
struct RunResult {
    provider: String,
    model: String,
    variant: String,
    enhance: Vec<EnhanceRec>,
    polish: Vec<PolishRec>,
    extract: Vec<ExtractRec>,
    ask: Vec<AskRec>,
}

#[derive(Serialize, Deserialize)]
struct EnhanceRec {
    fixture: String,
    secs: f64,
    peak_rss_mb: Option<u64>,
    contract_pass: bool,
    blocks: usize,
    error: Option<String>,
    /// Blocks rendered as markdown (judge input).
    notes_markdown: String,
    raw_output: String,
}

#[derive(Serialize, Deserialize)]
struct PolishRec {
    fixture: String,
    secs: f64,
    peak_rss_mb: Option<u64>,
    batches: usize,
    batches_parsed: usize,
    segments_total: usize,
    segments_cleaned: usize,
    guard_flags: usize,
    provenance_ok: bool,
    pass: bool,
    error: Option<String>,
    /// First unparseable batch output, verbatim (contract-failure evidence).
    sample_failure: Option<String>,
}

#[derive(Serialize, Deserialize)]
struct ExtractRec {
    fixture: String,
    secs: f64,
    peak_rss_mb: Option<u64>,
    parse_ok: bool,
    raw_items: usize,
    valid_items: usize,
    pass: bool,
    error: Option<String>,
    items_summary: Vec<String>,
    raw_output: String,
}

#[derive(Serialize, Deserialize)]
struct AskRec {
    question_id: String,
    fixture: String,
    kind: String,
    secs: f64,
    peak_rss_mb: Option<u64>,
    error: Option<String>,
    answer: String,
}

/// Strict Enhance contract check: the output must contain a parseable JSON
/// array of typed blocks (the shipped parser would otherwise fall back to
/// untraced paragraphs — lossy but silent, so the bench counts it as FAIL).
fn enhance_contract_ok(output: &str) -> (bool, usize) {
    #[derive(Deserialize)]
    struct RawBlockCheck {
        #[serde(rename = "type")]
        _kind: String,
        markdown: String,
        #[serde(default)]
        _sources: Vec<usize>,
    }
    let (Some(start), Some(end)) = (output.find('['), output.rfind(']')) else {
        return (false, 0);
    };
    if end <= start {
        return (false, 0);
    }
    match serde_json::from_str::<Vec<RawBlockCheck>>(&output[start..=end]) {
        Ok(blocks) => {
            let n = blocks
                .iter()
                .filter(|b| !b.markdown.trim().is_empty())
                .count();
            (n >= 1, n)
        }
        Err(_) => (false, 0),
    }
}

// ---------------------------------------------------------------------------
// Generate phase
// ---------------------------------------------------------------------------

const POLISH_MAX_BATCH_WORDS: usize = 1200;
const POLISH_MAX_BATCH_SEGMENTS: usize = 40;
const EXTRACT_MAX_BATCH_WORDS: usize = 3000;
const EXTRACT_MAX_BATCH_SEGMENTS: usize = 150;

fn slug(provider: &str, model: &str, variant: &str) -> String {
    let m = model.replace([':', '/'], "-");
    format!("{provider}-{m}-{variant}")
}

async fn run_generate(
    provider: &dyn LLMProvider,
    profile: &PromptProfile,
    manifest: &Manifest,
    fixtures: &[Fixture],
    tasks: &[String],
    out: &mut RunResult,
) {
    let local = provider.is_local();
    let want = |t: &str| tasks.iter().any(|x| x == t);

    for f in fixtures {
        let meeting_id = format!("bench-{}", f.spec.id);
        let note = fixture_note(&f.spec, &meeting_id);
        let meeting = fixture_meeting(&f.spec, &meeting_id);
        let mut transcript = f.transcript.clone();
        transcript.meeting_id = meeting_id.clone();

        // ------------------------------------------------ Enhance
        if want("enhance") {
            eprintln!("  [{}] enhance…", f.spec.id);
            let prompt = enhance::build_enhance_prompt(
                &note,
                Some(&transcript),
                &manifest.template,
                profile,
            );
            let sampler = RssSampler::start(local);
            let t0 = Instant::now();
            let res = chat_with_retry(
                provider,
                ChatRequest {
                    messages: vec![
                        ChatMessage::system(prompt.system.clone()),
                        ChatMessage::user(prompt.user.clone()),
                    ],
                    temperature: Some(0.2),
                    max_tokens: Some(profile.max_tokens.enhance.unwrap_or(4096)),
                    thinking: ThinkingMode::Default,
                },
            )
            .await;
            let secs = t0.elapsed().as_secs_f64();
            let peak = sampler.finish();
            let rec = match res {
                Ok(output) => {
                    let (pass, _) = enhance_contract_ok(&output);
                    let blocks = enhance::parse_enhanced_blocks(&output, &prompt.segment_ids);
                    let notes_markdown = blocks
                        .iter()
                        .map(|b| b.markdown.clone())
                        .collect::<Vec<_>>()
                        .join("\n\n");
                    EnhanceRec {
                        fixture: f.spec.id.clone(),
                        secs,
                        peak_rss_mb: peak,
                        contract_pass: pass,
                        blocks: blocks.len(),
                        error: None,
                        notes_markdown,
                        raw_output: output,
                    }
                }
                Err(e) => EnhanceRec {
                    fixture: f.spec.id.clone(),
                    secs,
                    peak_rss_mb: peak,
                    contract_pass: false,
                    blocks: 0,
                    error: Some(e),
                    notes_markdown: String::new(),
                    raw_output: String::new(),
                },
            };
            eprintln!(
                "    contract={} blocks={} {:.1}s",
                rec.contract_pass, rec.blocks, rec.secs
            );
            out.enhance.push(rec);
        }

        // ------------------------------------------------ Polish
        if want("polish") {
            eprintln!("  [{}] polish…", f.spec.id);
            let sampler = RssSampler::start(local);
            let t0 = Instant::now();
            let ranges = enhance::plan_cleanup_batches(
                &transcript.segments,
                POLISH_MAX_BATCH_WORDS,
                POLISH_MAX_BATCH_SEGMENTS,
            );
            let batches = ranges.len();
            let mut batches_parsed = 0;
            let mut cleaned_map: HashMap<String, String> = HashMap::new();
            let mut sample_failure = None;
            let mut error = None;
            for range in ranges {
                let prompt = enhance::build_cleanup_prompt(&transcript.segments[range], profile);
                match chat_with_retry(
                    provider,
                    ChatRequest {
                        messages: vec![
                            ChatMessage::system(prompt.system),
                            ChatMessage::user(prompt.user),
                        ],
                        temperature: None,
                        max_tokens: Some(profile.max_tokens.polish.unwrap_or(8192)),
                        thinking: ThinkingMode::Disabled,
                    },
                )
                .await
                {
                    Ok(output) => match enhance::parse_cleanup_response(&output) {
                        Some(pairs) => {
                            batches_parsed += 1;
                            cleaned_map.extend(pairs);
                        }
                        None => {
                            if sample_failure.is_none() {
                                sample_failure = Some(output);
                            }
                        }
                    },
                    Err(e) => error = Some(e),
                }
            }
            let outcome = enhance::apply_cleanup(&transcript, &cleaned_map);
            let provenance_ok = enhance::preserves_provenance(&transcript, &outcome.transcript);
            let rec = PolishRec {
                fixture: f.spec.id.clone(),
                secs: t0.elapsed().as_secs_f64(),
                peak_rss_mb: sampler.finish(),
                batches,
                batches_parsed,
                segments_total: transcript.segments.len(),
                segments_cleaned: outcome.segments_cleaned,
                guard_flags: outcome.flags.len(),
                provenance_ok,
                pass: error.is_none()
                    && batches_parsed == batches
                    && outcome.flags.is_empty()
                    && provenance_ok,
                error,
                sample_failure,
            };
            eprintln!(
                "    pass={} cleaned={}/{} flags={} {:.1}s",
                rec.pass, rec.segments_cleaned, rec.segments_total, rec.guard_flags, rec.secs
            );
            out.polish.push(rec);
        }

        // ------------------------------------------------ Extraction
        if want("extract") {
            eprintln!("  [{}] extract…", f.spec.id);
            let sampler = RssSampler::start(local);
            let t0 = Instant::now();
            let mut raw_all = Vec::new();
            let mut parse_ok = true;
            let mut error = None;
            let mut raw_output = String::new();
            for range in enhance::plan_cleanup_batches(
                &transcript.segments,
                EXTRACT_MAX_BATCH_WORDS,
                EXTRACT_MAX_BATCH_SEGMENTS,
            ) {
                let prompt = fly_app_lib::extraction::build_extraction_prompt(
                    &meeting,
                    &transcript,
                    range,
                    profile,
                );
                match chat_with_retry(
                    provider,
                    ChatRequest {
                        messages: vec![
                            ChatMessage::system(prompt.system),
                            ChatMessage::user(prompt.user),
                        ],
                        temperature: None,
                        max_tokens: Some(profile.max_tokens.extract.unwrap_or(8192)),
                        thinking: ThinkingMode::Disabled,
                    },
                )
                .await
                {
                    Ok(output) => {
                        match fly_app_lib::extraction::parse_extraction_response(&output) {
                            Some(items) => raw_all.extend(items),
                            None => parse_ok = false,
                        }
                        raw_output.push_str(&output);
                        raw_output.push('\n');
                    }
                    Err(e) => error = Some(e),
                }
            }
            let raw_count = raw_all.len();
            let items = fly_app_lib::extraction::validate_items(
                raw_all,
                &transcript,
                &meeting_id,
                provider.id(),
            );
            let items_summary = items
                .iter()
                .map(|i| format!("{}: {}", i.kind.as_str(), i.text))
                .collect::<Vec<_>>();
            let rec = ExtractRec {
                fixture: f.spec.id.clone(),
                secs: t0.elapsed().as_secs_f64(),
                peak_rss_mb: sampler.finish(),
                parse_ok,
                raw_items: raw_count,
                valid_items: items.len(),
                pass: error.is_none()
                    && parse_ok
                    && !items.is_empty()
                    && items.len() * 2 >= raw_count,
                error,
                items_summary,
                raw_output,
            };
            eprintln!(
                "    pass={} items={}/{} {:.1}s",
                rec.pass, rec.valid_items, rec.raw_items, rec.secs
            );
            out.extract.push(rec);
        }

        // ------------------------------------------------ Ask
        if want("ask") {
            let system =
                fly_app_lib::llm_commands::build_ask_system(&note, Some(&transcript), profile);
            for q in manifest.questions.iter().filter(|q| q.fixture == f.spec.id) {
                eprintln!("  [{}] ask {}…", f.spec.id, q.id);
                let sampler = RssSampler::start(local);
                let t0 = Instant::now();
                let res = chat_with_retry(
                    provider,
                    ChatRequest {
                        messages: vec![
                            ChatMessage::system(system.clone()),
                            ChatMessage::user(q.text.clone()),
                        ],
                        temperature: Some(0.3),
                        max_tokens: Some(profile.max_tokens.ask.unwrap_or(2048)),
                        thinking: ThinkingMode::Default,
                    },
                )
                .await;
                let secs = t0.elapsed().as_secs_f64();
                let peak = sampler.finish();
                let (answer, error) = match res {
                    Ok(a) => (a, None),
                    Err(e) => (String::new(), Some(e)),
                };
                eprintln!("    {:.1}s", secs);
                out.ask.push(AskRec {
                    question_id: q.id.clone(),
                    fixture: f.spec.id.clone(),
                    kind: q.kind.clone(),
                    secs,
                    peak_rss_mb: peak,
                    error,
                    answer,
                });
            }
        }
    }
}

// ---------------------------------------------------------------------------
// Judge phase
// ---------------------------------------------------------------------------

#[derive(Serialize, Deserialize)]
struct JudgeScore {
    id: String,
    score: u32,
    why: String,
}

#[derive(Serialize, Deserialize)]
struct Judged {
    model: String,
    reference: String,
    enhance: Vec<JudgeScore>,
    ask: Vec<JudgeScore>,
    mean_enhance: f64,
    mean_ask: f64,
}

const JUDGE_SYSTEM: &str = "You are a strict evaluator of meeting-notes AI output. You compare a \
CANDIDATE output against a REFERENCE output produced by a frontier model for the same input, and \
score the candidate 1-5:\n5 = as good as or better than the reference (correct, complete, useful)\n\
4 = minor gaps or noise, still fully usable\n3 = usable but clearly worse (missing facts, wrong \
emphasis, formatting problems)\n2 = major factual gaps or errors\n1 = wrong, empty, or unusable.\n\
Judge substance over style. Penalize invented facts hard.\n\
Respond with ONLY JSON: {\"score\": <1-5>, \"why\": \"<one line>\"}";

async fn judge_one(
    judge: &dyn LLMProvider,
    task_desc: &str,
    reference: &str,
    candidate: &str,
) -> Result<(u32, String), String> {
    let user = format!(
        "TASK:\n{task_desc}\n\nREFERENCE OUTPUT:\n{reference}\n\nCANDIDATE OUTPUT:\n{candidate}"
    );
    let out = chat_with_retry(
        judge,
        ChatRequest {
            messages: vec![ChatMessage::system(JUDGE_SYSTEM), ChatMessage::user(user)],
            temperature: None,
            max_tokens: Some(300),
            thinking: ThinkingMode::Disabled,
        },
    )
    .await?;
    let start = out.find('{').ok_or("no JSON in judge output")?;
    let end = out.rfind('}').ok_or("no JSON in judge output")?;
    #[derive(Deserialize)]
    struct V {
        score: u32,
        why: String,
    }
    let v: V = serde_json::from_str(&out[start..=end]).map_err(|e| e.to_string())?;
    Ok((v.score.clamp(1, 5), v.why))
}

async fn run_judge(out_dir: &Path, reference_slug: &str, judge_model: &str) {
    let key = anthropic_key().expect("judge needs an Anthropic key (env or app keychain)");
    let judge = fly_llm::anthropic::AnthropicProvider::new(key, judge_model.to_string());
    let reference: RunResult = serde_json::from_str(
        &std::fs::read_to_string(out_dir.join(reference_slug).join("results.json"))
            .expect("reference results.json — run the generate phase for the reference first"),
    )
    .expect("reference results parse");

    let (manifest, _) = load_manifest();
    let q_text: HashMap<&str, &str> = manifest
        .questions
        .iter()
        .map(|q| (q.id.as_str(), q.text.as_str()))
        .collect();

    for entry in std::fs::read_dir(out_dir).expect("out dir") {
        let dir = entry.expect("dir entry").path();
        let slug = dir.file_name().unwrap().to_string_lossy().to_string();
        if !dir.is_dir() || slug == reference_slug {
            continue;
        }
        let Ok(raw) = std::fs::read_to_string(dir.join("results.json")) else {
            continue;
        };
        let candidate: RunResult = serde_json::from_str(&raw).expect("candidate results parse");
        eprintln!("judging {slug} vs {reference_slug}…");

        let mut judged = Judged {
            model: slug.clone(),
            reference: reference_slug.to_string(),
            enhance: vec![],
            ask: vec![],
            mean_enhance: 0.0,
            mean_ask: 0.0,
        };

        for c in &candidate.enhance {
            let Some(r) = reference.enhance.iter().find(|r| r.fixture == c.fixture) else {
                continue;
            };
            if c.notes_markdown.is_empty() {
                judged.enhance.push(JudgeScore {
                    id: c.fixture.clone(),
                    score: 1,
                    why: "no output".into(),
                });
                continue;
            }
            let desc = format!(
                "Turn a meeting transcript + the user's scratchpad into structured meeting notes \
                 (fixture: {}).",
                c.fixture
            );
            match judge_one(&judge, &desc, &r.notes_markdown, &c.notes_markdown).await {
                Ok((score, why)) => judged.enhance.push(JudgeScore {
                    id: c.fixture.clone(),
                    score,
                    why,
                }),
                Err(e) => eprintln!("  judge error on enhance/{}: {e}", c.fixture),
            }
        }

        for c in &candidate.ask {
            let Some(r) = reference
                .ask
                .iter()
                .find(|r| r.question_id == c.question_id)
            else {
                continue;
            };
            if c.answer.is_empty() {
                judged.ask.push(JudgeScore {
                    id: c.question_id.clone(),
                    score: 1,
                    why: "no output".into(),
                });
                continue;
            }
            let desc = format!(
                "Answer a question about a meeting from its transcript.\nQUESTION: {}",
                q_text.get(c.question_id.as_str()).copied().unwrap_or("?")
            );
            match judge_one(&judge, &desc, &r.answer, &c.answer).await {
                Ok((score, why)) => judged.ask.push(JudgeScore {
                    id: c.question_id.clone(),
                    score,
                    why,
                }),
                Err(e) => eprintln!("  judge error on ask/{}: {e}", c.question_id),
            }
        }

        let mean = |v: &[JudgeScore]| {
            if v.is_empty() {
                0.0
            } else {
                v.iter().map(|s| s.score as f64).sum::<f64>() / v.len() as f64
            }
        };
        judged.mean_enhance = mean(&judged.enhance);
        judged.mean_ask = mean(&judged.ask);
        eprintln!(
            "  {slug}: enhance {:.2}/5, ask {:.2}/5",
            judged.mean_enhance, judged.mean_ask
        );
        std::fs::write(
            dir.join("judged.json"),
            serde_json::to_string_pretty(&judged).unwrap(),
        )
        .unwrap();
    }
}

// ---------------------------------------------------------------------------
// Entry point
// ---------------------------------------------------------------------------

#[test]
#[ignore = "LLM benchmark harness; needs a provider (and API key for anthropic) — see file docs"]
fn llm_bench() {
    let out_dir = PathBuf::from(std::env::var("LLM_BENCH_OUT").unwrap_or_else(|_| {
        Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../target/llm-bench")
            .to_string_lossy()
            .into_owned()
    }));
    std::fs::create_dir_all(&out_dir).expect("create out dir");
    let runtime = tokio::runtime::Runtime::new().unwrap();

    if std::env::var("LLM_BENCH_PHASE").as_deref() == Ok("judge") {
        let reference = std::env::var("LLM_BENCH_REF")
            .unwrap_or_else(|_| "anthropic-claude-sonnet-5-default".into());
        let judge_model =
            std::env::var("LLM_BENCH_JUDGE_MODEL").unwrap_or_else(|_| "claude-sonnet-5".into());
        runtime.block_on(run_judge(&out_dir, &reference, &judge_model));
        return;
    }

    let models = match std::env::var("LLM_BENCH_MODELS") {
        Ok(m) if !m.trim().is_empty() => m,
        _ => {
            eprintln!("SKIP: set LLM_BENCH_MODELS=\"ollama:llama3.1,anthropic:claude-sonnet-5\"");
            return;
        }
    };
    let variant = std::env::var("LLM_BENCH_VARIANT").unwrap_or_else(|_| "default".into());
    let profile = variant_profile(&variant);
    let tasks: Vec<String> = std::env::var("LLM_BENCH_TASKS")
        .unwrap_or_else(|_| "enhance,polish,extract,ask".into())
        .split(',')
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
        .collect();

    let (manifest, fixtures) = load_manifest();

    for spec in models.split(',').map(str::trim).filter(|s| !s.is_empty()) {
        let (provider_id, model) = spec
            .split_once(':')
            .expect("model spec must be provider:model");
        let provider = build_provider(provider_id, model).expect("provider");
        let run_slug = slug(provider_id, model, &variant);
        eprintln!("=== {run_slug} (tasks: {}) ===", tasks.join(","));
        let mut result = RunResult {
            provider: provider_id.into(),
            model: model.into(),
            variant: variant.clone(),
            ..Default::default()
        };
        let t0 = Instant::now();
        runtime.block_on(run_generate(
            provider.as_ref(),
            &profile,
            &manifest,
            &fixtures,
            &tasks,
            &mut result,
        ));
        eprintln!(
            "=== {run_slug} done in {:.0}s ===",
            t0.elapsed().as_secs_f64()
        );
        let dir = out_dir.join(&run_slug);
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(
            dir.join("results.json"),
            serde_json::to_string_pretty(&result).unwrap(),
        )
        .unwrap();
    }
}
