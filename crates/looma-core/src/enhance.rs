//! Enhance: prompt construction + response parsing for merging the user's
//! scratchpad with the transcript into provenance-tagged blocks.
//!
//! The LLM is asked for a JSON array of blocks; `user` blocks restate the
//! user's own scratchpad content (rendered as theirs), `ai` blocks carry the
//! transcript segment indices they were derived from — mapped back to real
//! segment ids here, which is what powers zoom-in.

use serde::Deserialize;

use crate::model::{Note, NoteBlock, Template, Transcript};

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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{BlockOrigin, Speaker, TranscriptSegment};
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
}
