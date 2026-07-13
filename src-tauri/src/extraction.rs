//! Post-transcription extraction of structured meeting items (decisions,
//! action items, open questions, commitments, key figures) with provenance.
//!
//! Runs in the app — chained after every successful transcription (like the
//! polish pass) plus on-demand backfill for meetings transcribed before the
//! feature existed. Uses the user's selected AI provider via the same
//! plumbing as polish/enhance; the MCP server only ever READS the results
//! from the `meeting_items` table.

use fly_core::prompt_profile::{profile_for, PromptProfile};
use fly_core::{enhance, ItemKind, Meeting, MeetingItem, Transcript};
use fly_llm::{ChatMessage, ChatRequest, LLMProvider, ThinkingMode};
use serde::Deserialize;
use tauri::State;

use crate::llm_commands::{build_provider, ensure_provider_ready};
use crate::state::AppState;

type CmdResult<T> = Result<T, String>;

/// Batch sizing: extraction reads much more than it writes, so batches can be
/// larger than polish batches; output stays small either way.
const EXTRACT_MAX_BATCH_WORDS: usize = 3000;
const EXTRACT_MAX_BATCH_SEGMENTS: usize = 150;
/// Hard cap per meeting — a hallucinating model must not flood the table.
const EXTRACT_MAX_ITEMS: usize = 100;

pub struct ExtractionPrompt {
    pub system: String,
    pub user: String,
}

/// The strict extraction contract (default profile).
const EXTRACT_CONTRACT_FULL: &str = r#"You extract structured facts from a meeting transcript.

Return ONLY a JSON array (no prose, no code fences). Each element:
{
  "kind": "decision" | "action_item" | "question" | "commitment" | "figure",
  "text": "the fact, one sentence, in the speakers' own terms",
  "owner": "who owns it (action items / commitments), else null",
  "status": "action items only: \"open\" or \"done\" as stated in THIS meeting, else null",
  "speaker_key": "the key in parentheses of the line(s) it came from, else null",
  "segment_ids": ["ids of the [bracketed] lines it came from"]
}

Kinds:
- decision: something the participants settled ("we'll go with X").
- action_item: a task someone will do ("A will send B by Friday").
- question: raised but NOT answered in the meeting.
- commitment: a promise to a person/customer/date ("we'll deliver by June").
- figure: a concrete number that matters (money, dates, metrics, counts).

Rules:
- Only facts explicitly present in the transcript — never infer or embellish.
- segment_ids MUST be ids that appear in [brackets] below.
- Skip small talk. An empty array [] is the correct answer for a meeting
  with no such facts."#;

/// Terser contract for models whose profile sets `simplified_contract`: same
/// schema and kinds, shorter rules, an inline example, explicit no-preamble.
const EXTRACT_CONTRACT_SIMPLE: &str = r#"Extract facts from a meeting transcript.

Output ONLY a JSON array. No text before or after it, no code fences.
Each element: {"kind": "decision" | "action_item" | "question" | "commitment" | "figure", "text": "one sentence", "owner": "who or null", "status": "open"/"done" for action items else null, "speaker_key": "key in parentheses or null", "segment_ids": ["ids from [brackets]"]}
Example: [{"kind":"decision","text":"Ship v2 on Friday.","owner":null,"status":null,"speaker_key":"spk_0","segment_ids":["seg004"]}]

kind meanings: decision = settled; action_item = a task someone will do; question = raised but unanswered; commitment = promise to a person/date; figure = a number that matters.
Only facts explicitly in the transcript. segment_ids must be ids that appear in [brackets] below. [] is correct when there are none."#;

/// Build the prompt for one batch of segments. Every line carries the
/// segment id and stable speaker key so the model can cite provenance.
pub fn build_extraction_prompt(
    meeting: &Meeting,
    transcript: &Transcript,
    range: std::ops::Range<usize>,
    profile: &PromptProfile,
) -> ExtractionPrompt {
    let contract = if profile.simplified_contract {
        EXTRACT_CONTRACT_SIMPLE
    } else {
        EXTRACT_CONTRACT_FULL
    };
    let system = profile.apply_preamble(contract.to_string());

    let mut user = format!(
        "Meeting: {}\nDate: {}\nAttendees: {}\n\nTranscript:\n",
        meeting.title,
        meeting.started_at.format("%Y-%m-%d"),
        if meeting.attendees.is_empty() {
            "(not recorded)".to_string()
        } else {
            meeting.attendees.join(", ")
        }
    );
    for seg in &transcript.segments[range] {
        user.push_str(&format!(
            "[{}] {} ({}): {}\n",
            seg.id,
            transcript.label_for(&seg.speaker_key),
            seg.speaker_key,
            seg.text.trim()
        ));
    }
    ExtractionPrompt { system, user }
}

/// One element of the model's JSON array, before validation.
#[derive(Debug, Deserialize)]
pub struct RawExtractedItem {
    pub kind: String,
    pub text: String,
    #[serde(default)]
    pub owner: Option<String>,
    #[serde(default)]
    pub status: Option<String>,
    #[serde(default)]
    pub speaker_key: Option<String>,
    #[serde(default)]
    pub segment_ids: Vec<String>,
}

/// Parse the model output into raw items, tolerating code fences or prose
/// around the JSON array. `None` = unparseable (caller treats the batch as
/// having produced nothing — extraction is enrichment, never load-bearing).
pub fn parse_extraction_response(output: &str) -> Option<Vec<RawExtractedItem>> {
    let start = output.find('[')?;
    let end = output.rfind(']')?;
    if end <= start {
        return None;
    }
    serde_json::from_str(&output[start..=end]).ok()
}

/// Validate raw items against the transcript: known kind, non-empty text,
/// segment ids that actually exist, speaker key that actually exists.
/// Deterministic and total — bad entries are dropped, never errors.
pub fn validate_items(
    raw: Vec<RawExtractedItem>,
    transcript: &Transcript,
    meeting_id: &str,
    extracted_by: &str,
) -> Vec<MeetingItem> {
    let seg_ids: std::collections::HashSet<&str> =
        transcript.segments.iter().map(|s| s.id.as_str()).collect();
    let speaker_keys: std::collections::HashSet<&str> = transcript
        .segments
        .iter()
        .map(|s| s.speaker_key.as_str())
        .collect();
    let mut seen: std::collections::HashSet<(String, String)> = Default::default();
    let mut out = Vec::new();
    for r in raw {
        let Some(kind) = ItemKind::parse(r.kind.trim()) else {
            continue;
        };
        let text = r.text.trim().to_string();
        if text.is_empty() {
            continue;
        }
        if !seen.insert((kind.as_str().to_string(), text.to_lowercase())) {
            continue; // duplicate across batches
        }
        let segment_ids: Vec<String> = r
            .segment_ids
            .into_iter()
            .filter(|id| seg_ids.contains(id.as_str()))
            .collect();
        let status = r
            .status
            .as_deref()
            .map(|s| s.trim().to_lowercase())
            .filter(|s| kind == ItemKind::ActionItem && (s == "open" || s == "done"));
        out.push(MeetingItem {
            id: fly_core::new_id(),
            meeting_id: meeting_id.to_string(),
            kind,
            text,
            owner: r
                .owner
                .map(|o| o.trim().to_string())
                .filter(|o| !o.is_empty()),
            status,
            speaker_key: r.speaker_key.filter(|k| speaker_keys.contains(k.as_str())),
            segment_ids,
            created_at: chrono::Utc::now(),
            extracted_by: extracted_by.to_string(),
        });
        if out.len() >= EXTRACT_MAX_ITEMS {
            break;
        }
    }
    out
}

/// Extract items for one transcript with the given provider. Pure with
/// respect to storage — the caller persists.
pub async fn extract_items(
    provider: &dyn LLMProvider,
    meeting: &Meeting,
    transcript: &Transcript,
) -> Result<Vec<MeetingItem>, String> {
    let extracted_by = provider.id().to_string();
    let profile = profile_for(provider.model());
    let mut raw_all = Vec::new();
    for range in enhance::plan_cleanup_batches(
        &transcript.segments,
        EXTRACT_MAX_BATCH_WORDS,
        EXTRACT_MAX_BATCH_SEGMENTS,
    ) {
        let prompt = build_extraction_prompt(meeting, transcript, range, profile);
        let output = provider
            .chat(ChatRequest {
                messages: vec![
                    ChatMessage::system(prompt.system),
                    ChatMessage::user(prompt.user),
                ],
                // No temperature (claude-sonnet-5 rejects it) and no thinking
                // (reasoning tokens would eat the budget and truncate the
                // JSON) — same contract as the polish pass.
                temperature: None,
                max_tokens: Some(profile.max_tokens.extract.unwrap_or(8192)),
                thinking: ThinkingMode::Disabled,
            })
            .await
            .map_err(|e| e.to_string())?;
        raw_all.extend(parse_extraction_response(&output).unwrap_or_default());
    }
    Ok(validate_items(
        raw_all,
        transcript,
        &meeting.id,
        &extracted_by,
    ))
}

/// Run extraction for one meeting and persist the result. Prefers the
/// polished transcript (same segment ids, cleaner text for the model);
/// falls back to the raw one. Re-runnable: replaces the meeting's items.
pub async fn run_extraction(state: &AppState, meeting_id: &str) -> Result<usize, String> {
    ensure_provider_ready(state).await?;
    let provider = build_provider(state)?;
    let (meeting, transcript) = {
        let storage = state.storage.lock().unwrap();
        let meeting = storage.get_meeting(meeting_id).map_err(|e| e.to_string())?;
        let transcript = storage
            .get_cleaned_transcript(meeting_id)
            .ok()
            .flatten()
            .or(storage
                .get_transcript(meeting_id)
                .map_err(|e| e.to_string())?)
            .ok_or_else(|| "no transcript to extract from yet".to_string())?;
        (meeting, transcript)
    };
    if transcript.segments.is_empty() {
        return Err("transcript has no segments".into());
    }

    let items = extract_items(provider.as_ref(), &meeting, &transcript).await?;
    tracing::info!(
        meeting_id,
        provider = provider.id(),
        items = items.len(),
        "meeting item extraction finished"
    );
    let storage = state.storage.lock().unwrap();
    if items.is_empty() {
        storage
            .mark_extracted(meeting_id, provider.id())
            .map_err(|e| e.to_string())?;
    } else {
        storage
            .replace_meeting_items(meeting_id, &items)
            .map_err(|e| e.to_string())?;
    }
    Ok(items.len())
}

/// Best-effort extraction chained after a successful transcription — must
/// never fail the job (mirrors the polish pass contract).
pub async fn extract_after_transcribe(state: &AppState, meeting_id: &str) {
    match run_extraction(state, meeting_id).await {
        Ok(n) => tracing::info!(meeting_id, items = n, "post-transcription extraction done"),
        Err(e) => tracing::warn!(
            meeting_id,
            error = %e,
            "post-transcription extraction skipped (transcript stands without items)"
        ),
    }
}

/// Re-extract one meeting on demand (also used to backfill a single meeting).
#[tauri::command]
pub async fn extract_meeting_items(
    state: State<'_, AppState>,
    meeting_id: String,
) -> CmdResult<usize> {
    run_extraction(&state, &meeting_id).await
}

#[derive(serde::Serialize)]
pub struct BackfillResult {
    pub processed: usize,
    pub extracted: usize,
    pub failed: usize,
}

/// On-demand backfill: run extraction for every transcribed meeting that has
/// none yet (oldest first). Stops early only on provider-setup failure.
#[tauri::command]
pub async fn backfill_meeting_items(state: State<'_, AppState>) -> CmdResult<BackfillResult> {
    let pending = {
        let storage = state.storage.lock().unwrap();
        storage
            .meetings_missing_items(500)
            .map_err(|e| e.to_string())?
    };
    let mut result = BackfillResult {
        processed: 0,
        extracted: 0,
        failed: 0,
    };
    for meeting_id in pending {
        result.processed += 1;
        match run_extraction(&state, &meeting_id).await {
            Ok(n) => result.extracted += n,
            Err(e) => {
                tracing::warn!(meeting_id, error = %e, "backfill extraction failed");
                result.failed += 1;
            }
        }
    }
    Ok(result)
}

#[cfg(test)]
mod tests {
    use super::*;
    use fly_core::{Speaker, TranscriptSegment};
    use fly_llm::mock::MockLLMProvider;

    fn transcript() -> Transcript {
        let seg = |id: &str, key: &str, text: &str| TranscriptSegment {
            id: id.into(),
            speaker_key: key.into(),
            start_ms: 0,
            end_ms: 1000,
            text: text.into(),
            words: vec![],
        };
        Transcript {
            meeting_id: "m1".into(),
            language: Some("en".into()),
            engine: "whisper.cpp".into(),
            segments: vec![
                seg("s1", "mic", "let's go with the annual plan, that's decided"),
                seg("s2", "spk_0", "I'll send the revised SOW by Friday"),
                seg("s3", "spk_0", "ARR is at 2.4 million"),
            ],
            speakers: vec![
                Speaker {
                    key: "mic".into(),
                    label: "You".into(),
                },
                Speaker {
                    key: "spk_0".into(),
                    label: "Dana".into(),
                },
            ],
        }
    }

    fn meeting() -> Meeting {
        Meeting {
            id: "m1".into(),
            title: "Renewal sync".into(),
            note_id: "n1".into(),
            attendees: vec!["dana@example.com".into()],
            started_at: chrono::Utc::now(),
            ended_at: None,
            recording: None,
        }
    }

    #[test]
    fn prompt_carries_segment_ids_speaker_keys_and_labels() {
        let t = transcript();
        let p = build_extraction_prompt(
            &meeting(),
            &t,
            0..t.segments.len(),
            &fly_core::prompt_profile::DEFAULT_PROFILE,
        );
        assert!(p.user.contains("[s1] You (mic):"));
        assert!(p.user.contains("[s2] Dana (spk_0):"));
        assert!(p.user.contains("Renewal sync"));
        assert!(p.system.contains("segment_ids"));
    }

    #[test]
    fn parse_tolerates_fences_and_prose() {
        let out = "Here you go:\n```json\n[{\"kind\":\"decision\",\"text\":\"annual plan\",\"segment_ids\":[\"s1\"]}]\n```";
        let items = parse_extraction_response(out).unwrap();
        assert_eq!(items.len(), 1);
        assert_eq!(items[0].kind, "decision");
        assert!(parse_extraction_response("no json here").is_none());
    }

    #[test]
    fn validation_drops_unknown_kinds_ids_and_dupes() {
        let t = transcript();
        let raw = vec![
            RawExtractedItem {
                kind: "decision".into(),
                text: "go with the annual plan".into(),
                owner: None,
                status: Some("open".into()), // status only applies to action items
                speaker_key: Some("mic".into()),
                segment_ids: vec!["s1".into(), "bogus".into()],
            },
            RawExtractedItem {
                kind: "decision".into(),
                text: "Go with the annual plan".into(), // dupe (case-insensitive)
                owner: None,
                status: None,
                speaker_key: None,
                segment_ids: vec![],
            },
            RawExtractedItem {
                kind: "vibe".into(), // unknown kind
                text: "good energy".into(),
                owner: None,
                status: None,
                speaker_key: None,
                segment_ids: vec![],
            },
            RawExtractedItem {
                kind: "action_item".into(),
                text: "send the revised SOW".into(),
                owner: Some("Dana".into()),
                status: Some("Open".into()),
                speaker_key: Some("spk_9".into()), // unknown speaker
                segment_ids: vec!["s2".into()],
            },
        ];
        let items = validate_items(raw, &t, "m1", "mock");
        assert_eq!(items.len(), 2);
        assert_eq!(items[0].kind, ItemKind::Decision);
        assert_eq!(items[0].segment_ids, vec!["s1"]); // bogus id dropped
        assert_eq!(items[0].status, None); // status stripped off non-action
        assert_eq!(items[1].status.as_deref(), Some("open")); // normalized
        assert_eq!(items[1].speaker_key, None); // unknown speaker dropped
    }

    #[tokio::test]
    async fn extract_items_end_to_end_with_mock_provider() {
        let canned = r#"[
            {"kind":"decision","text":"go with the annual plan","speaker_key":"mic","segment_ids":["s1"]},
            {"kind":"action_item","text":"send the revised SOW by Friday","owner":"Dana","status":"open","speaker_key":"spk_0","segment_ids":["s2"]},
            {"kind":"figure","text":"ARR is at $2.4M","speaker_key":"spk_0","segment_ids":["s3"]}
        ]"#;
        let provider = MockLLMProvider::with_response(canned);
        let items = extract_items(&provider, &meeting(), &transcript())
            .await
            .unwrap();
        assert_eq!(items.len(), 3);
        assert_eq!(items[0].kind, ItemKind::Decision);
        assert_eq!(items[1].owner.as_deref(), Some("Dana"));
        assert_eq!(items[1].segment_ids, vec!["s2"]);
        assert_eq!(items[2].kind, ItemKind::Figure);
        assert!(items.iter().all(|i| i.extracted_by == "mock"));
    }
}
