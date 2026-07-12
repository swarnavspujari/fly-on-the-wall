//! Enhance: prompt construction + response parsing for merging the user's
//! scratchpad with the transcript into provenance-tagged blocks.
//!
//! The LLM is asked for a JSON array of blocks; `user` blocks restate the
//! user's own scratchpad content (rendered as theirs), `ai` blocks carry the
//! transcript segment indices they were derived from — mapped back to real
//! segment ids here, which is what powers zoom-in.

use std::collections::HashMap;

use serde::{Deserialize, Serialize};

use crate::model::{Note, NoteBlock, Template, Transcript, TranscriptSegment};

/// System + user prompt, plus the index→segment-id map for source mapping.
pub struct EnhancePrompt {
    pub system: String,
    pub user: String,
    pub segment_ids: Vec<String>,
}

pub fn build_enhance_prompt(
    note: &Note,
    transcript: Option<&Transcript>,
    template: &Template,
) -> EnhancePrompt {
    let mut segment_ids = Vec::new();
    let transcript_text = match transcript {
        Some(t) => {
            let mut out = String::new();
            for seg in &t.segments {
                let idx = segment_ids.len();
                segment_ids.push(seg.id.clone());
                out.push_str(&format!(
                    "[{idx}] {}: {}\n",
                    t.label_for(&seg.speaker_key),
                    seg.text.trim()
                ));
            }
            out
        }
        None => String::new(),
    };

    let system = format!(
        "{}\n\n\
        You MUST respond with ONLY a JSON array (no prose, no code fences). Each element:\n\
        {{\"type\": \"user\" | \"ai\", \"markdown\": \"...\", \"sources\": [segment numbers]}}\n\
        Rules:\n\
        - \"user\" blocks restate lines from MY NOTES (lightly cleaned up); keep my wording. Use an empty sources array.\n\
        - \"ai\" blocks add structure or content derived from the TRANSCRIPT; cite the segment numbers they came from in sources.\n\
        - Markdown inside blocks may use headings, bullet lists, and bold.\n\
        - Follow this target structure where it fits:\n{}",
        template.system_prompt, template.structure_hint
    );

    let user = if transcript_text.is_empty() {
        format!(
            "MY NOTES (raw scratchpad):\n{}\n\nThere is no transcript. Structure and clean up my notes.",
            note.scratchpad
        )
    } else {
        format!(
            "MY NOTES (raw scratchpad):\n{}\n\nTRANSCRIPT (numbered segments):\n{}",
            note.scratchpad, transcript_text
        )
    };

    EnhancePrompt {
        system,
        user,
        segment_ids,
    }
}

#[derive(Deserialize)]
struct RawBlock {
    #[serde(rename = "type")]
    kind: String,
    markdown: String,
    #[serde(default)]
    sources: Vec<usize>,
}

/// Parse the LLM's block array; tolerate fences/prose around the JSON.
/// Fallback: whole output becomes untraced AI paragraphs (never lose work).
pub fn parse_enhanced_blocks(llm_output: &str, segment_ids: &[String]) -> Vec<NoteBlock> {
    if let Some(blocks) = try_parse_json(llm_output, segment_ids) {
        if !blocks.is_empty() {
            return blocks;
        }
    }
    llm_output
        .split("\n\n")
        .map(str::trim)
        .filter(|p| !p.is_empty())
        .map(|p| NoteBlock::ai(p, vec![]))
        .collect()
}

fn try_parse_json(output: &str, segment_ids: &[String]) -> Option<Vec<NoteBlock>> {
    let start = output.find('[')?;
    let end = output.rfind(']')?;
    if end <= start {
        return None;
    }
    let raw: Vec<RawBlock> = serde_json::from_str(&output[start..=end]).ok()?;
    Some(
        raw.into_iter()
            .filter(|b| !b.markdown.trim().is_empty())
            .map(|b| {
                if b.kind == "user" {
                    NoteBlock::user(b.markdown.trim())
                } else {
                    let sources = b
                        .sources
                        .into_iter()
                        .filter_map(|i| segment_ids.get(i).cloned())
                        .collect();
                    NoteBlock::ai(b.markdown.trim(), sources)
                }
            })
            .collect(),
    )
}

// ---------------------------------------------------------------------------
// Transcript polish (LLM cleanup pass)
//
// A re-runnable pass that cleans each raw segment's TEXT ONLY — fixing
// capitalization/punctuation, dropping disfluencies, correcting obvious
// proper-noun mis-hearings — while preserving segment count, ids, speakers,
// and timestamps EXACTLY. The Enhanced doc cites transcript segment ids for
// provenance, so a changed/dropped id would silently break citations; the map
// below keys cleaned text strictly by id and never touches structure. A guard
// rejects any cleaned segment that lost an implausible share of its words and
// falls back to the raw text, so a lossy or truncated model response can only
// leave text un-cleaned, never drop content.
// ---------------------------------------------------------------------------

/// The verbatim cleanup system prompt (the runtime contract). Its "preserve
/// every word / speaker / timestamp" guarantees are load-bearing — do not
/// weaken them; the mapping + guard below assume the model was asked for a
/// same-ids, same-order response.
pub const CLEANUP_SYSTEM_PROMPT: &str = "\
You clean up raw meeting-transcript segments into a polished, readable transcript — the quality of\n\
a Fathom or Microsoft Teams transcript — WITHOUT losing a single word of substance or any context.\n\
You are not a summarizer. You do not paraphrase, compress, reorder, or invent. You return the same\n\
segments you were given, with the same IDs, speakers, and timestamps, only with the text cleaned.\n\
\n\
Clean each segment's text by:\n\
- Fixing capitalization, punctuation, and sentence boundaries so run-on ASR output reads as clear\n\
  sentences.\n\
- Removing only non-lexical disfluencies and stutters (\"um\", \"uh\", \"uh...\", repeated false starts\n\
  like \"I I want\", \"the the the\") — never remove meaningful words.\n\
- Correcting obviously mis-transcribed proper nouns using in-context evidence (e.g. a speaker named\n\
  \"Swarnav Pujari\" mis-heard as \"Swarnoff\", \"Dubay\", or \"Swarnoff Dubay\" → \"Swarnav Pujari\";\n\
  \"Ann.\" mid-sentence that is clearly a mis-transcribed \"And\" → \"And\"). Only correct when context\n\
  makes the intended word unambiguous; when unsure, leave it.\n\
- Preserving redactions exactly as they appear (e.g. \"******\").\n\
- Keeping every substantive word, number, name, and idea. If a line is genuinely just cross-talk\n\
  acknowledgement (\"Yeah.\", \"Mhm.\", \"Right.\"), keep it — it's context.\n\
\n\
Hard rules:\n\
- Preserve segment count, segment IDs, speaker labels, and timestamps EXACTLY. One input segment →\n\
  one output segment with the same id/speaker/start.\n\
- Never merge or split segments. Never move text between speakers.\n\
- If you cannot clean a segment confidently, return its original text unchanged.\n\
- NEVER return an empty string for a segment. Every input segment maps to a NON-EMPTY output\n\
  segment. If a line is nothing but a short acknowledgement or filler (\"Okay.\", \"Yeah.\", \"Right.\",\n\
  \"Mhm.\", \"Um.\"), keep it verbatim — removing disfluencies never means blanking an entire segment.\n\
- Output ONLY the JSON described, nothing else.";

/// A cleaned segment must retain at least this fraction of the raw content
/// length to be accepted (for segments long enough for the ratio to be
/// meaningful). Disfluency removal trims a little; losing half means
/// substantive content vanished, so we reject the cleaned text and keep the
/// raw segment. This is the no-loss floor — never loosen it to make a lossy
/// result "pass".
const MIN_RETAINED_FRACTION: f64 = 0.5;

/// Below this raw content length the retention ratio is too noisy to trust
/// (cleaning "yeah, um" → "Yeah." is a legitimate big drop), so short segments
/// pass on any non-empty cleaned text; only an emptied segment is rejected.
/// Content length is measured in non-whitespace characters (see `content_len`),
/// NOT whitespace-delimited words: languages without inter-word spaces
/// (Chinese, Japanese, Thai) emit a whole segment as one word, which would
/// make a word-count floor never engage and silently pass lossy cleanings.
const RATIO_MIN_RAW_CHARS: usize = 12;

/// System + user prompt for the cleanup pass, plus the ordered segment ids the
/// response must echo back — the contract `apply_cleanup` maps against.
pub struct CleanupPrompt {
    pub system: String,
    pub user: String,
    pub segment_ids: Vec<String>,
}

/// One segment the guard rejected: its cleaned text lost an implausible share
/// of its content, so the raw text was kept. Surfaced to the UI so a user knows
/// some lines couldn't be safely cleaned (never a silent drop). Lengths are in
/// non-whitespace characters so the measure is meaningful for every language.
#[derive(Debug, Clone, Serialize, PartialEq)]
pub struct PolishFlag {
    pub segment_id: String,
    pub speaker_key: String,
    pub raw_chars: usize,
    pub cleaned_chars: usize,
    pub reason: String,
}

/// Result of mapping a cleaned response back onto the raw transcript. The
/// `transcript` is the cleaned variant — identical to `raw` in count, ids,
/// speakers, timestamps, and words; only segment `text` may differ.
pub struct PolishOutcome {
    pub transcript: Transcript,
    pub segments_cleaned: usize,
    pub segments_kept_raw: usize,
    pub flags: Vec<PolishFlag>,
}

#[derive(Serialize)]
struct PromptSegment<'a> {
    id: &'a str,
    speaker: &'a str,
    start: u64,
    text: &'a str,
}

/// Build the cleanup prompt for a batch of segments. Callers batch long
/// transcripts (one call per chunk) and merge the cleaned text by id; the
/// prompt sends id/speaker/start/text so the model has the context it needs to
/// disambiguate proper nouns without ever being asked to alter structure.
pub fn build_cleanup_prompt(segments: &[TranscriptSegment]) -> CleanupPrompt {
    let prompt_segments: Vec<PromptSegment> = segments
        .iter()
        .map(|s| PromptSegment {
            id: &s.id,
            speaker: &s.speaker_key,
            start: s.start_ms,
            text: s.text.trim(),
        })
        .collect();
    let segments_json =
        serde_json::to_string(&prompt_segments).unwrap_or_else(|_| "[]".to_string());

    let user = format!(
        "Here are the transcript segments as JSON. Return JSON of the form\n\
         {{\"segments\":[{{\"id\": \"...\", \"text\": \"<cleaned text>\"}}...]}} with exactly the same ids, in the same\n\
         order.\n\n\
         <segments>\n{segments_json}\n</segments>\n\n\
         Example (one segment):\n\
         Input:  {{\"id\":\"t7\",\"speaker\":\"mic\",\"start\":16000,\"text\":\"Yeah, it's because of the recording, I I think. Swarnoff, uh, Dubay.\"}}\n\
         Output: {{\"id\":\"t7\",\"text\":\"Yeah, it's because of the recording, I think. Swarnav Pujari.\"}}"
    );

    CleanupPrompt {
        system: CLEANUP_SYSTEM_PROMPT.to_string(),
        user,
        segment_ids: segments.iter().map(|s| s.id.clone()).collect(),
    }
}

/// Group segments into contiguous batches for the cleanup pass, so each
/// provider call's response fits its token cap. A batch is flushed before a
/// segment that would push it past `max_words` or `max_segments`; a single
/// segment larger than `max_words` still forms its own batch (segments are
/// never split — one input segment maps to one output segment). Batches are
/// contiguous and cover every segment exactly once, in order.
pub fn plan_cleanup_batches(
    segments: &[TranscriptSegment],
    max_words: usize,
    max_segments: usize,
) -> Vec<std::ops::Range<usize>> {
    let mut batches = Vec::new();
    let mut start = 0;
    let mut words = 0;
    for (i, seg) in segments.iter().enumerate() {
        let w = word_count(&seg.text);
        let cur_len = i - start;
        if cur_len > 0 && (cur_len >= max_segments || words + w > max_words) {
            batches.push(start..i);
            start = i;
            words = 0;
        }
        words += w;
    }
    if start < segments.len() {
        batches.push(start..segments.len());
    }
    batches
}

#[derive(Deserialize)]
struct CleanupResponse {
    segments: Vec<CleanedSegment>,
}

#[derive(Deserialize)]
struct CleanedSegment {
    id: String,
    text: String,
}

/// Parse a cleanup response into (id, cleaned-text) pairs, tolerating code
/// fences or prose around the JSON object. Returns `None` on unparseable
/// output — the caller then keeps every raw segment (total, lossless fallback).
pub fn parse_cleanup_response(output: &str) -> Option<Vec<(String, String)>> {
    let start = output.find('{')?;
    let end = output.rfind('}')?;
    if end <= start {
        return None;
    }
    let parsed: CleanupResponse = serde_json::from_str(&output[start..=end]).ok()?;
    Some(
        parsed
            .segments
            .into_iter()
            .map(|s| (s.id, s.text))
            .collect(),
    )
}

/// Whitespace-delimited word count. Used only for batch sizing — it is a fine
/// budget proxy there; it is deliberately NOT used by the retention guard,
/// which needs a language-agnostic measure (see `content_len`).
fn word_count(s: &str) -> usize {
    s.split_whitespace().count()
}

/// Content length in non-whitespace characters. Script-agnostic: works for
/// space-delimited languages and for Chinese/Japanese/Thai alike, where a whole
/// segment is one whitespace token.
fn content_len(s: &str) -> usize {
    s.chars().filter(|c| !c.is_whitespace()).count()
}

/// Whether a cleaned segment retained enough of its content to be trusted. A
/// tripped guard means the raw segment is kept, so this only ever *withholds*
/// cleaning — it can never cause loss.
fn is_plausible_cleanup(raw_len: usize, cleaned_len: usize) -> bool {
    if raw_len == 0 {
        return true; // nothing to lose
    }
    if cleaned_len == 0 {
        return false; // the whole segment was emptied — always reject
    }
    if raw_len < RATIO_MIN_RAW_CHARS {
        return true; // too short for the ratio; any non-empty text is fine
    }
    (cleaned_len as f64) >= (raw_len as f64) * MIN_RETAINED_FRACTION
}

/// Map cleaned text (keyed by segment id) back onto the raw transcript.
///
/// The output transcript is `raw` cloned, with each segment's `text` replaced
/// by its cleaned version ONLY when (a) the response supplied that id and
/// (b) the retention guard passes. Everything else — segment count, ids,
/// speaker keys, start/end timestamps, per-word timing, and the speaker list —
/// is copied verbatim, so provenance citations that reference segment ids keep
/// resolving. A missing/renamed id or a guard-failing segment falls back to
/// the raw text (and is flagged), never to a lossy result.
pub fn apply_cleanup(raw: &Transcript, cleaned: &HashMap<String, String>) -> PolishOutcome {
    let mut out = raw.clone();
    let mut flags = Vec::new();
    let mut segments_cleaned = 0;
    let mut segments_kept_raw = 0;

    for seg in &mut out.segments {
        match cleaned.get(&seg.id) {
            Some(new_text) => {
                let trimmed = new_text.trim();
                let raw_chars = content_len(&seg.text);
                let cleaned_chars = content_len(trimmed);
                if is_plausible_cleanup(raw_chars, cleaned_chars) {
                    seg.text = trimmed.to_string();
                    segments_cleaned += 1;
                } else {
                    flags.push(PolishFlag {
                        segment_id: seg.id.clone(),
                        speaker_key: seg.speaker_key.clone(),
                        raw_chars,
                        cleaned_chars,
                        reason: format!(
                            "cleaned content dropped implausibly ({raw_chars} → {cleaned_chars} chars); kept the raw segment"
                        ),
                    });
                    segments_kept_raw += 1;
                }
            }
            // id absent (dropped or renamed by the model) → keep raw text.
            None => segments_kept_raw += 1,
        }
    }

    PolishOutcome {
        transcript: out,
        segments_cleaned,
        segments_kept_raw,
        flags,
    }
}

/// The provenance invariant the cleaned transcript must satisfy: same speaker
/// list and, per segment in order, identical id, speaker key, timestamps, and
/// words. Only `text` is allowed to differ. Used as a persist-time assertion
/// so a future regression fails loudly instead of corrupting citations.
pub fn preserves_provenance(raw: &Transcript, cleaned: &Transcript) -> bool {
    raw.speakers == cleaned.speakers
        && raw.segments.len() == cleaned.segments.len()
        && raw.segments.iter().zip(&cleaned.segments).all(|(a, b)| {
            a.id == b.id
                && a.speaker_key == b.speaker_key
                && a.start_ms == b.start_ms
                && a.end_ms == b.end_ms
                && a.words == b.words
        })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{BlockOrigin, Speaker, TranscriptSegment, Word};
    use chrono::Utc;

    fn note_with_scratchpad(s: &str) -> Note {
        Note {
            id: "n".into(),
            title: "t".into(),
            folder_id: None,
            meeting_id: None,
            scratchpad: s.into(),
            blocks: vec![],
            attachments: vec![],
            created_at: Utc::now(),
            updated_at: Utc::now(),
        }
    }

    fn transcript() -> Transcript {
        Transcript {
            meeting_id: "m".into(),
            language: None,
            engine: "whisper.cpp".into(),
            segments: vec![
                TranscriptSegment {
                    id: "seg-a".into(),
                    speaker_key: "mic".into(),
                    start_ms: 0,
                    end_ms: 1000,
                    text: "we should approve the budget".into(),
                    words: vec![],
                },
                TranscriptSegment {
                    id: "seg-b".into(),
                    speaker_key: "spk_0".into(),
                    start_ms: 1000,
                    end_ms: 2000,
                    text: "agreed, fifty thousand".into(),
                    words: vec![],
                },
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

    #[test]
    fn prompt_numbers_segments_and_keeps_id_map() {
        let tpl = Template {
            id: "t".into(),
            name: "General".into(),
            system_prompt: "sys".into(),
            structure_hint: "## Summary".into(),
            built_in: true,
        };
        let p = build_enhance_prompt(
            &note_with_scratchpad("- budget!"),
            Some(&transcript()),
            &tpl,
        );
        assert!(p.user.contains("[0] You: we should approve the budget"));
        assert!(p.user.contains("[1] Dana: agreed, fifty thousand"));
        assert_eq!(p.segment_ids, vec!["seg-a", "seg-b"]);
        assert!(p.system.contains("## Summary"));
    }

    #[test]
    fn parses_blocks_with_provenance_mapping() {
        // (markdown deliberately avoids `"##` — that sequence would end a
        // raw-string literal)
        let out = r#"Here you go:
[
  {"type": "user", "markdown": "- budget!", "sources": []},
  {"type": "ai", "markdown": "Decisions:\n- Approved $50k", "sources": [1, 99]}
]"#;
        let blocks = parse_enhanced_blocks(out, &["seg-a".into(), "seg-b".into()]);
        assert_eq!(blocks.len(), 2);
        assert_eq!(blocks[0].origin, BlockOrigin::User);
        match &blocks[1].origin {
            BlockOrigin::Ai { source_segment_ids } => {
                // valid index mapped, out-of-range dropped
                assert_eq!(source_segment_ids, &vec!["seg-b".to_string()]);
            }
            _ => panic!("expected ai block"),
        }
    }

    #[test]
    fn malformed_output_falls_back_to_ai_paragraphs() {
        let out = "## Summary\nStuff happened.\n\n## Decisions\n- none";
        let blocks = parse_enhanced_blocks(out, &[]);
        assert_eq!(blocks.len(), 2);
        assert!(blocks
            .iter()
            .all(|b| matches!(b.origin, BlockOrigin::Ai { .. })));
    }

    // -----------------------------------------------------------------------
    // Transcript polish
    // -----------------------------------------------------------------------

    /// Two segments carrying real timestamps + per-word timing, so the
    /// provenance-preservation assertions have something to check.
    fn polish_source() -> Transcript {
        Transcript {
            meeting_id: "m".into(),
            language: Some("en".into()),
            engine: "whisper.cpp".into(),
            segments: vec![
                TranscriptSegment {
                    id: "t1".into(),
                    speaker_key: "mic".into(),
                    start_ms: 16_000,
                    end_ms: 20_000,
                    text: "yeah it's because of the recording I I think Swarnoff uh Dubay".into(),
                    words: vec![Word {
                        text: "recording".into(),
                        start_ms: 16_500,
                        end_ms: 17_000,
                    }],
                },
                TranscriptSegment {
                    id: "t2".into(),
                    speaker_key: "spk_0".into(),
                    start_ms: 20_000,
                    end_ms: 24_000,
                    text: "the the the budget is approved for fifty thousand dollars".into(),
                    words: vec![],
                },
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

    #[test]
    fn cleanup_prompt_is_verbatim_and_lists_segments() {
        let src = polish_source();
        let p = build_cleanup_prompt(&src.segments);
        // system prompt is the runtime contract, used verbatim
        assert_eq!(p.system, CLEANUP_SYSTEM_PROMPT);
        assert!(p.system.contains("WITHOUT losing a single word"));
        // segments serialized with id/speaker/start/text, ids echoed in order
        assert!(p.user.contains(r#""id":"t1""#));
        assert!(p.user.contains(r#""speaker":"mic""#));
        assert!(p.user.contains(r#""start":16000"#));
        assert!(p.user.contains(r#""speaker":"spk_0""#));
        assert_eq!(p.segment_ids, vec!["t1", "t2"]);
    }

    #[test]
    fn apply_cleanup_preserves_ids_speakers_timestamps_words() {
        let src = polish_source();
        let cleaned: HashMap<String, String> = [
            (
                "t1".to_string(),
                "Yeah, it's because of the recording, I think. Swarnav Pujari.".to_string(),
            ),
            (
                "t2".to_string(),
                "The budget is approved for fifty thousand dollars.".to_string(),
            ),
        ]
        .into_iter()
        .collect();

        let outcome = apply_cleanup(&src, &cleaned);
        // structural contract holds exactly
        assert!(preserves_provenance(&src, &outcome.transcript));
        assert_eq!(outcome.transcript.segments.len(), 2);
        // text was actually cleaned
        assert_eq!(
            outcome.transcript.segments[0].text,
            "Yeah, it's because of the recording, I think. Swarnav Pujari."
        );
        // ids / speaker keys / timestamps / words are byte-for-byte the raw ones
        assert_eq!(outcome.transcript.segments[0].id, "t1");
        assert_eq!(outcome.transcript.segments[0].speaker_key, "mic");
        assert_eq!(outcome.transcript.segments[0].start_ms, 16_000);
        assert_eq!(outcome.transcript.segments[0].end_ms, 20_000);
        assert_eq!(outcome.transcript.segments[0].words, src.segments[0].words);
        assert_eq!(outcome.transcript.speakers, src.speakers);
        assert_eq!(outcome.segments_cleaned, 2);
        assert_eq!(outcome.segments_kept_raw, 0);
        assert!(outcome.flags.is_empty());
    }

    #[test]
    fn apply_cleanup_keeps_raw_when_id_missing_or_renamed() {
        let src = polish_source();
        // model returned only t1, and renamed the other to "t2-renamed"
        let cleaned: HashMap<String, String> = [
            (
                "t1".to_string(),
                "Yeah, it's because of the recording, I think. Swarnav Pujari.".to_string(),
            ),
            (
                "t2-renamed".to_string(),
                "totally different text".to_string(),
            ),
        ]
        .into_iter()
        .collect();

        let outcome = apply_cleanup(&src, &cleaned);
        // t2 was neither dropped nor overwritten — it kept its exact raw text
        assert_eq!(outcome.transcript.segments[1].id, "t2");
        assert_eq!(outcome.transcript.segments[1].text, src.segments[1].text);
        assert_eq!(outcome.segments_cleaned, 1);
        assert_eq!(outcome.segments_kept_raw, 1);
        assert!(preserves_provenance(&src, &outcome.transcript));
    }

    #[test]
    fn apply_cleanup_guard_trips_on_lossy_segment() {
        let src = polish_source();
        // t2 has 10 raw words; the model "cleaned" it down to 2 — real content
        // was dropped. The guard must reject it, keep the raw text, and flag it.
        let cleaned: HashMap<String, String> = [
            (
                "t1".to_string(),
                "Yeah, it's because of the recording.".to_string(),
            ),
            ("t2".to_string(), "Budget approved.".to_string()),
        ]
        .into_iter()
        .collect();

        let outcome = apply_cleanup(&src, &cleaned);
        // lossy segment fell back to raw — NOT the two-word summary
        assert_eq!(outcome.transcript.segments[1].text, src.segments[1].text);
        assert_eq!(outcome.segments_kept_raw, 1);
        assert_eq!(outcome.flags.len(), 1);
        assert_eq!(outcome.flags[0].segment_id, "t2");
        // content-char lengths: a big, implausible drop
        assert!(
            outcome.flags[0].cleaned_chars * 2 < outcome.flags[0].raw_chars,
            "flag should record the implausible content drop"
        );
        assert!(preserves_provenance(&src, &outcome.transcript));
    }

    #[test]
    fn apply_cleanup_guard_catches_loss_in_non_space_delimited_languages() {
        // Japanese/Chinese/Thai have no inter-word whitespace, so whisper.cpp
        // emits a whole segment as one token. The retention guard must still
        // catch a lossy cleaning — a word-count metric can't, a content metric
        // can. Raw here says "we discussed the budget, $50k was approved,
        // details next week"; the lossy clean drops the amount + follow-up.
        let src = Transcript {
            meeting_id: "m".into(),
            language: Some("ja".into()),
            engine: "whisper.cpp".into(),
            segments: vec![TranscriptSegment {
                id: "j1".into(),
                speaker_key: "mic".into(),
                start_ms: 0,
                end_ms: 4_000,
                text: "今日は予算について話し合い、五万ドルの支出が承認されました。詳細は来週確認します。"
                    .into(),
                words: vec![],
            }],
            speakers: vec![],
        };
        let cleaned: HashMap<String, String> =
            [("j1".to_string(), "予算が承認されました。".to_string())]
                .into_iter()
                .collect();
        let outcome = apply_cleanup(&src, &cleaned);
        // guard must trip → raw kept, flagged, no words lost
        assert_eq!(
            outcome.segments_kept_raw, 1,
            "lossy CJK clean must be rejected"
        );
        assert_eq!(outcome.flags.len(), 1);
        assert_eq!(outcome.transcript.segments[0].text, src.segments[0].text);
        // a faithful CJK clean (punctuation/spacing only) must still pass
        let faithful: HashMap<String, String> = [(
            "j1".to_string(),
            "今日は予算について話し合い、五万ドルの支出が承認されました。詳細は来週確認します。"
                .to_string(),
        )]
        .into_iter()
        .collect();
        assert_eq!(apply_cleanup(&src, &faithful).segments_cleaned, 1);
    }

    #[test]
    fn apply_cleanup_allows_normal_disfluency_trim() {
        // Dropping "the the the" (a stutter) from a 10-word line is legitimate:
        // 10 → 7 words retains 70%, above the floor, so it must NOT be flagged.
        let src = polish_source();
        let cleaned: HashMap<String, String> = [(
            "t2".to_string(),
            "The budget is approved for fifty thousand dollars.".to_string(),
        )]
        .into_iter()
        .collect();

        let outcome = apply_cleanup(&src, &cleaned);
        assert!(outcome.flags.is_empty());
        assert_eq!(
            outcome.transcript.segments[1].text,
            "The budget is approved for fifty thousand dollars."
        );
    }

    #[test]
    fn apply_cleanup_short_segment_survives_filler_removal() {
        // "Yeah, um." (2 words) → "Yeah." (1 word) is a legit 50% drop but too
        // short for the ratio; only an *emptied* segment is rejected.
        let src = Transcript {
            meeting_id: "m".into(),
            language: None,
            engine: "whisper.cpp".into(),
            segments: vec![TranscriptSegment {
                id: "s".into(),
                speaker_key: "mic".into(),
                start_ms: 0,
                end_ms: 500,
                text: "yeah um".into(),
                words: vec![],
            }],
            speakers: vec![],
        };
        let cleaned: HashMap<String, String> = [("s".to_string(), "Yeah.".to_string())]
            .into_iter()
            .collect();
        let outcome = apply_cleanup(&src, &cleaned);
        assert!(outcome.flags.is_empty());
        assert_eq!(outcome.transcript.segments[0].text, "Yeah.");

        // …but an emptied short segment is still rejected (no silent drop).
        let emptied: HashMap<String, String> =
            [("s".to_string(), "   ".to_string())].into_iter().collect();
        let outcome = apply_cleanup(&src, &emptied);
        assert_eq!(outcome.flags.len(), 1);
        assert_eq!(outcome.transcript.segments[0].text, "yeah um");
    }

    #[test]
    fn preserves_provenance_detects_structural_drift() {
        let src = polish_source();
        // a cleaned variant that renamed a segment id must NOT pass
        let mut tampered = src.clone();
        tampered.segments[0].id = "changed".into();
        assert!(!preserves_provenance(&src, &tampered));
        // one that shifted a timestamp must NOT pass either
        let mut shifted = src.clone();
        shifted.segments[1].start_ms += 1;
        assert!(!preserves_provenance(&src, &shifted));
    }

    #[test]
    fn plan_cleanup_batches_respects_word_and_segment_caps() {
        let seg = |w: usize| TranscriptSegment {
            id: "x".into(),
            speaker_key: "mic".into(),
            start_ms: 0,
            end_ms: 0,
            text: vec!["w"; w].join(" "),
            words: vec![],
        };
        // word cap: 3+3=6 fits, +3=9 > 7 → flush every two
        let segs = vec![seg(3), seg(3), seg(3), seg(3), seg(3)];
        assert_eq!(plan_cleanup_batches(&segs, 7, 10), vec![0..2, 2..4, 4..5]);
        // a single over-budget segment is isolated, never split or dropped
        let big = vec![seg(3), seg(100), seg(3)];
        assert_eq!(plan_cleanup_batches(&big, 7, 10), vec![0..1, 1..2, 2..3]);
        // segment cap independent of words
        let many = vec![seg(1), seg(1), seg(1), seg(1)];
        assert_eq!(plan_cleanup_batches(&many, 1000, 2), vec![0..2, 2..4]);
        // every segment covered exactly once, in order
        let total: usize = plan_cleanup_batches(&segs, 7, 10)
            .iter()
            .map(|r| r.len())
            .sum();
        assert_eq!(total, segs.len());
    }

    #[test]
    fn parse_cleanup_response_tolerates_prose_and_fences() {
        let out = "Sure, here you go:\n```json\n{\"segments\":[{\"id\":\"t1\",\"text\":\"Hello.\"},{\"id\":\"t2\",\"text\":\"World.\"}]}\n```";
        let pairs = parse_cleanup_response(out).unwrap();
        assert_eq!(pairs.len(), 2);
        assert_eq!(pairs[0], ("t1".to_string(), "Hello.".to_string()));
        // unparseable output → None → caller keeps all raw segments
        assert!(parse_cleanup_response("not json at all").is_none());
    }
}
