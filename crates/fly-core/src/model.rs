//! Domain types shared by every Fly on the Wall crate.

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

// ---------------------------------------------------------------------------
// Folders & notes
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct Folder {
    pub id: String,
    pub name: String,
    /// `None` = top level. Folders nest arbitrarily deep.
    pub parent_id: Option<String>,
    pub created_at: DateTime<Utc>,
}

/// Where a note block came from. This is the provenance model: user text and
/// AI text are never mixed inside one block, so rendering can color them and
/// editing an AI block reclaims it as the user's own words.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum BlockOrigin {
    User,
    Ai {
        /// Transcript segment ids this block was derived from — powers
        /// "zoom in": click an AI line, see the exact source segments.
        source_segment_ids: Vec<String>,
    },
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct NoteBlock {
    pub id: String,
    pub origin: BlockOrigin,
    pub markdown: String,
}

impl NoteBlock {
    pub fn user(markdown: impl Into<String>) -> Self {
        Self {
            id: crate::new_id(),
            origin: BlockOrigin::User,
            markdown: markdown.into(),
        }
    }

    pub fn ai(markdown: impl Into<String>, source_segment_ids: Vec<String>) -> Self {
        Self {
            id: crate::new_id(),
            origin: BlockOrigin::Ai { source_segment_ids },
            markdown: markdown.into(),
        }
    }

    /// A user edit to an AI block reclaims it: it becomes the user's text and
    /// drops its transcript sourcing. Editing a user block is a no-op on origin.
    pub fn apply_edit(&mut self, new_markdown: impl Into<String>) {
        let new_markdown = new_markdown.into();
        if self.markdown != new_markdown {
            self.markdown = new_markdown;
            self.origin = BlockOrigin::User;
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct Attachment {
    pub id: String,
    /// Original file name, for display.
    pub file_name: String,
    /// Path relative to the data dir — notes stay portable if the dir moves.
    pub rel_path: String,
    pub mime: Option<String>,
    pub added_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct Note {
    pub id: String,
    pub title: String,
    pub folder_id: Option<String>,
    pub meeting_id: Option<String>,
    /// The user's raw in-meeting notes (always user-origin, freely edited).
    pub scratchpad: String,
    /// The enhanced document: ordered blocks with provenance. Empty until
    /// the first Enhance run (M4).
    pub blocks: Vec<NoteBlock>,
    pub attachments: Vec<Attachment>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

impl Note {
    /// Flatten to plain markdown. Provenance colors are a rendering concern;
    /// exports optionally keep sourcing as footnote-style comments. The
    /// enhanced document supersedes the scratchpad when it exists.
    pub fn to_markdown(&self, include_sources: bool) -> String {
        let mut out = format!("# {}\n", self.title);
        if self.blocks.is_empty() {
            if !self.scratchpad.is_empty() {
                out.push('\n');
                out.push_str(&self.scratchpad);
                out.push('\n');
            }
            return out;
        }
        for block in &self.blocks {
            out.push('\n');
            out.push_str(&block.markdown);
            out.push('\n');
            if include_sources {
                if let BlockOrigin::Ai { source_segment_ids } = &block.origin {
                    if !source_segment_ids.is_empty() {
                        out.push_str(&format!(
                            "<!-- sources: {} -->\n",
                            source_segment_ids.join(", ")
                        ));
                    }
                }
            }
        }
        out
    }
}

// ---------------------------------------------------------------------------
// Meetings & recordings
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct RecordingRef {
    /// Microphone channel (you), WAV.
    pub mic_path: Option<String>,
    /// System-loopback channel (them), WAV.
    pub system_path: Option<String>,
    /// Mixed track (16 kHz mono ASR downmix), used by single-file pipelines
    /// and as the playback fallback for recordings made before `playback_path`.
    pub mixed_path: Option<String>,
    /// Full-quality playback mix (mic + system at native rate). Absent on
    /// recordings made before it existed — players must fall back.
    #[serde(default)]
    pub playback_path: Option<String>,
    pub duration_ms: u64,
}

/// One meeting participant besides the user: a display name plus the
/// calendar email it came from (kept through renames, so "priya@acme.com"
/// renamed to "Priya Kapoor" still matches the next event's roster).
///
/// Serialized as `{ "name": …, "email": … }`; deserialization also accepts
/// the legacy plain-string form (pre-feature rows stored `Vec<String>` of
/// calendar emails), mapping a string to name = the string, email = the
/// string when it looks like an address.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(from = "AttendeeCompat")]
pub struct Attendee {
    pub name: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub email: Option<String>,
}

#[derive(Deserialize)]
#[serde(untagged)]
enum AttendeeCompat {
    Full {
        name: String,
        #[serde(default)]
        email: Option<String>,
    },
    Legacy(String),
}

impl From<AttendeeCompat> for Attendee {
    fn from(c: AttendeeCompat) -> Self {
        match c {
            AttendeeCompat::Full { name, email } => Attendee { name, email },
            AttendeeCompat::Legacy(s) => Attendee::from_legacy(&s),
        }
    }
}

impl Attendee {
    /// Build from a legacy string (calendar flow stores raw emails).
    pub fn from_legacy(s: &str) -> Self {
        let s = s.trim();
        Attendee {
            name: s.to_string(),
            email: s.contains('@').then(|| s.to_string()),
        }
    }

    /// What to show for this attendee (name, falling back to email).
    pub fn display_name(&self) -> &str {
        let name = self.name.trim();
        if name.is_empty() {
            self.email.as_deref().unwrap_or("")
        } else {
            name
        }
    }

    /// Identity for cross-meeting matching (series detection): the stable
    /// email when present — a rename must not break the match — else the
    /// name. Lowercased.
    pub fn match_key(&self) -> String {
        self.email
            .as_deref()
            .filter(|e| !e.trim().is_empty())
            .unwrap_or(&self.name)
            .trim()
            .to_lowercase()
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct Meeting {
    pub id: String,
    pub title: String,
    pub note_id: String,
    pub attendees: Vec<Attendee>,
    /// True once the user explicitly confirmed the attendee list in the
    /// attendee editor. Calendar-seeded lists stay unconfirmed: rosters are
    /// unreliable speaker-count proxies (organizer omitted, resources,
    /// declines — see the E2 findings), so only a confirmed list may drive
    /// `DiarizeOptions::num_speakers`.
    #[serde(default)]
    pub attendees_confirmed: bool,
    pub started_at: DateTime<Utc>,
    pub ended_at: Option<DateTime<Utc>>,
    pub recording: Option<RecordingRef>,
}

impl Meeting {
    /// Attendee display names joined for prompts and text output.
    pub fn attendee_names(&self) -> Vec<String> {
        self.attendees
            .iter()
            .map(|a| a.display_name().to_string())
            .filter(|n| !n.is_empty())
            .collect()
    }
}

// ---------------------------------------------------------------------------
// Transcript
// ---------------------------------------------------------------------------

/// One recognized word with timing. The atom the aligner works on.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct Word {
    pub text: String,
    pub start_ms: u64,
    pub end_ms: u64,
}

/// A contiguous run of one speaker's words.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct TranscriptSegment {
    pub id: String,
    /// Stable machine key (`mic`, `spk_0`, `spk_1`, …). Never changes.
    pub speaker_key: String,
    pub start_ms: u64,
    pub end_ms: u64,
    pub text: String,
    pub words: Vec<Word>,
}

/// Relabelable display name for a speaker key.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct Speaker {
    pub key: String,
    pub label: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct Transcript {
    pub meeting_id: String,
    pub language: Option<String>,
    /// Which ASR engine produced this ("whisper.cpp", "parakeet", "groq").
    pub engine: String,
    pub segments: Vec<TranscriptSegment>,
    pub speakers: Vec<Speaker>,
}

impl Transcript {
    pub fn label_for(&self, key: &str) -> String {
        self.speakers
            .iter()
            .find(|s| s.key == key)
            .map(|s| s.label.clone())
            .unwrap_or_else(|| key.to_string())
    }

    /// Render as markdown with `[mm:ss] **Label:** text` lines.
    pub fn to_markdown(&self) -> String {
        let mut out = String::new();
        for seg in &self.segments {
            let secs = seg.start_ms / 1000;
            out.push_str(&format!(
                "[{:02}:{:02}] **{}:** {}\n\n",
                secs / 60,
                secs % 60,
                self.label_for(&seg.speaker_key),
                seg.text.trim()
            ));
        }
        out
    }
}

/// Raw diarization output: who spoke when, no words.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct SpeakerTurn {
    pub speaker_key: String,
    pub start_ms: u64,
    pub end_ms: u64,
}

// ---------------------------------------------------------------------------
// Extracted meeting items (structured context layer)
// ---------------------------------------------------------------------------

/// What kind of structured fact a [`MeetingItem`] is.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum ItemKind {
    Decision,
    ActionItem,
    Question,
    Commitment,
    Figure,
}

impl ItemKind {
    pub const ALL: [ItemKind; 5] = [
        ItemKind::Decision,
        ItemKind::ActionItem,
        ItemKind::Question,
        ItemKind::Commitment,
        ItemKind::Figure,
    ];

    /// Stable string form ("decision", "action_item", …) — the DB `kind`
    /// column and the MCP tool argument both use it.
    pub fn as_str(&self) -> &'static str {
        match self {
            ItemKind::Decision => "decision",
            ItemKind::ActionItem => "action_item",
            ItemKind::Question => "question",
            ItemKind::Commitment => "commitment",
            ItemKind::Figure => "figure",
        }
    }

    pub fn parse(s: &str) -> Option<ItemKind> {
        Self::ALL.iter().copied().find(|k| k.as_str() == s)
    }
}

/// One typed fact extracted from a meeting transcript (a decision, an action
/// item, an open question, a commitment, a key figure), carrying provenance:
/// the meeting, the transcript segments it came from, and who said it.
/// Extraction runs in the app (LLM); readers (the MCP server) only query.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct MeetingItem {
    pub id: String,
    pub meeting_id: String,
    pub kind: ItemKind,
    /// The fact itself, one sentence, in the speakers' own terms.
    pub text: String,
    /// Action items / commitments: who owns it (speaker label or name).
    pub owner: Option<String>,
    /// Action items: "open" | "done" as stated IN that meeting.
    pub status: Option<String>,
    /// Stable speaker key of who said it (`mic`, `spk_0`, …).
    pub speaker_key: Option<String>,
    /// Transcript segment ids this item was extracted from.
    pub segment_ids: Vec<String>,
    pub created_at: DateTime<Utc>,
    /// Provider id + model that produced the extraction (auditability).
    pub extracted_by: String,
}

// ---------------------------------------------------------------------------
// Templates
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct Template {
    pub id: String,
    pub name: String,
    pub system_prompt: String,
    pub structure_hint: String,
    pub built_in: bool,
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Legacy rows store `["email", …]`; the struct form is `[{name, email}]`.
    /// Both must deserialize — a parse failure would silently empty the list
    /// (see fly-storage row_to_meeting).
    #[test]
    fn attendees_deserialize_from_legacy_and_struct_forms() {
        let legacy: Vec<Attendee> =
            serde_json::from_str(r#"["dana@example.com", "Room 4A"]"#).unwrap();
        assert_eq!(legacy[0].name, "dana@example.com");
        assert_eq!(legacy[0].email.as_deref(), Some("dana@example.com"));
        assert_eq!(legacy[1].name, "Room 4A");
        assert_eq!(legacy[1].email, None);

        let full: Vec<Attendee> = serde_json::from_str(
            r#"[{"name":"Priya Kapoor","email":"priya@acme.com"},{"name":"Taylor"}]"#,
        )
        .unwrap();
        assert_eq!(full[0].display_name(), "Priya Kapoor");
        assert_eq!(full[0].match_key(), "priya@acme.com");
        assert_eq!(full[1].email, None);
        assert_eq!(full[1].match_key(), "taylor");

        // round-trip: emailless attendees serialize without the field
        let json = serde_json::to_string(&full).unwrap();
        assert!(json.contains(r#""email":"priya@acme.com""#));
        assert!(!json.contains(r#""email":null"#));
        let back: Vec<Attendee> = serde_json::from_str(&json).unwrap();
        assert_eq!(back, full);
    }

    #[test]
    fn meeting_without_confirmed_flag_still_parses() {
        let json = r#"{
            "id":"m1","title":"t","note_id":"n1",
            "attendees":["a@x.com"],
            "started_at":"2026-07-01T10:00:00Z","ended_at":null,"recording":null
        }"#;
        let m: Meeting = serde_json::from_str(json).unwrap();
        assert!(!m.attendees_confirmed);
        assert_eq!(m.attendee_names(), vec!["a@x.com"]);
    }

    #[test]
    fn editing_an_ai_block_reclaims_it_as_user() {
        let mut b = NoteBlock::ai("summary line", vec!["seg-1".into()]);
        assert!(matches!(b.origin, BlockOrigin::Ai { .. }));
        b.apply_edit("my corrected line");
        assert_eq!(b.origin, BlockOrigin::User);
        assert_eq!(b.markdown, "my corrected line");
    }

    #[test]
    fn identical_edit_does_not_reclaim() {
        let mut b = NoteBlock::ai("same text", vec!["seg-1".into()]);
        b.apply_edit("same text");
        assert!(matches!(b.origin, BlockOrigin::Ai { .. }));
    }

    #[test]
    fn user_block_edit_stays_user() {
        let mut b = NoteBlock::user("hello");
        b.apply_edit("hello world");
        assert_eq!(b.origin, BlockOrigin::User);
    }

    #[test]
    fn note_markdown_includes_sources_when_asked() {
        let note = Note {
            id: "n1".into(),
            title: "Standup".into(),
            folder_id: None,
            meeting_id: None,
            scratchpad: "raw jotted line".into(),
            blocks: vec![
                NoteBlock::user("my scratch line"),
                NoteBlock::ai("ai summary", vec!["seg-9".into()]),
            ],
            attachments: vec![],
            created_at: Utc::now(),
            updated_at: Utc::now(),
        };
        let md = note.to_markdown(true);
        assert!(md.contains("# Standup"));
        assert!(md.contains("<!-- sources: seg-9 -->"));
        let plain = note.to_markdown(false);
        assert!(!plain.contains("sources:"));
    }

    #[test]
    fn unenhanced_note_exports_scratchpad() {
        let note = Note {
            id: "n2".into(),
            title: "Quick thoughts".into(),
            folder_id: None,
            meeting_id: None,
            scratchpad: "- talk to sam\n- ship it".into(),
            blocks: vec![],
            attachments: vec![],
            created_at: Utc::now(),
            updated_at: Utc::now(),
        };
        let md = note.to_markdown(false);
        assert!(md.contains("# Quick thoughts"));
        assert!(md.contains("- talk to sam"));
    }
}
