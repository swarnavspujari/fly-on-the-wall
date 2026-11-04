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

/// Why an AI block that cites transcript lines is not shown as sourced:
/// something it states isn't in the lines it cites (the Enhance checker,
/// fly-ask `enhance`). The first six mirror fly-ask's answer flags, so the
/// Enhanced view and the Ask sheet say the same thing about the same failure.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum BlockFlagKind {
    /// States a number, date or quote and cites nothing it could be read in.
    UncitedClaim,
    /// Cited a line the model was never shown.
    InvalidCitation,
    Number,
    Date,
    Name,
    Quote,
    /// Claimed to restate the user's own notes but isn't in them.
    NotFromNotes,
    /// Cites a line that reads as aimed at an AI assistant (the injected
    /// line is never a source), or states what only such a line says.
    AimedAtAi,
    /// Rests only on a line phrased as an order ("Pay the invoice by
    /// Friday"): what colleagues say to each other, and what an injected
    /// line can look like. Kept with its source, to check.
    Instruction,
    /// Says the opposite of the lines it cites (approved / not approving).
    /// The detail is the word the block uses.
    Reversed,
}

/// How much of the meeting an Enhance run read, saved on the blocks it
/// wrote when it was less than all of it (a long meeting that didn't fit
/// the model's context), or when the AI's reply was cut off at its length
/// limit, so a partial note never looks complete.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
pub struct PartialRead {
    /// Transcript lines the model was given, and lines in the meeting.
    /// Equal when it read the whole meeting (and the reply was cut off).
    pub shown: u32,
    pub total: u32,
    /// The AI's reply stopped at its length limit, so later points may be
    /// missing. Absent on disk when false, and in notes saved before this
    /// existed.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub cut_off: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct BlockFlag {
    pub kind: BlockFlagKind,
    /// The number, date, name or quote that failed. Answer text for the UI;
    /// never logged.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub detail: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct NoteBlock {
    pub id: String,
    pub origin: BlockOrigin,
    pub markdown: String,
    /// Checks this AI block failed (empty = none, and always empty for the
    /// user's own text). Absent in notes saved before the check existed.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub flags: Vec<BlockFlag>,
    /// Set on the AI blocks of an Enhance run that read only part of the
    /// meeting, or whose reply was cut off at its length limit. Absent for a
    /// whole, finished run, for the user's own text, and in notes saved
    /// before this existed.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub partial: Option<PartialRead>,
}

impl NoteBlock {
    pub fn user(markdown: impl Into<String>) -> Self {
        Self {
            id: crate::new_id(),
            origin: BlockOrigin::User,
            markdown: markdown.into(),
            flags: Vec::new(),
            partial: None,
        }
    }

    pub fn ai(markdown: impl Into<String>, source_segment_ids: Vec<String>) -> Self {
        Self {
            id: crate::new_id(),
            origin: BlockOrigin::Ai { source_segment_ids },
            markdown: markdown.into(),
            flags: Vec::new(),
            partial: None,
        }
    }

    /// An AI block that failed a check (see [`BlockFlag`]).
    pub fn ai_flagged(
        markdown: impl Into<String>,
        source_segment_ids: Vec<String>,
        flags: Vec<BlockFlag>,
    ) -> Self {
        Self {
            flags,
            ..Self::ai(markdown, source_segment_ids)
        }
    }

    /// A user edit to an AI block reclaims it: it becomes the user's text and
    /// drops its transcript sourcing. Editing a user block is a no-op on origin.
    pub fn apply_edit(&mut self, new_markdown: impl Into<String>) {
        let new_markdown = new_markdown.into();
        if self.markdown != new_markdown {
            self.markdown = new_markdown;
            self.reclaim();
        }
    }

    /// The user keeps this block as their own words without changing it
    /// ("Keep" on an AI line that has no source): the same reclaim an edit
    /// does, so its transcript sourcing is dropped and the text is untouched.
    pub fn reclaim(&mut self) {
        self.origin = BlockOrigin::User;
        // Flags and a partial read describe AI text; once it is the user's
        // they have nothing to say.
        self.flags.clear();
        self.partial = None;
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
                        // A flagged block's sources don't back what it says:
                        // an export must not present them as if they did.
                        let label = if block.flags.is_empty() {
                            "sources"
                        } else {
                            "check: not backed by its sources"
                        };
                        out.push_str(&format!(
                            "<!-- {label}: {} -->\n",
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
    /// Which ASR engine produced this: "whisper.cpp", "whisper.cpp-vulkan",
    /// "groq", … — and for engines that record their weights,
    /// "parakeet[-vulkan]:<model file stem>".
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

    /// Render as WebVTT: one cue per segment (cue id = segment id) with a
    /// `<v Label>` voice tag. Text is escaped for the cue payload. A cue
    /// whose end does not follow its start is nudged 1 ms so the file
    /// stays valid for strict parsers.
    pub fn to_vtt(&self) -> String {
        let mut out = String::from(
            "WEBVTT

",
        );
        for seg in &self.segments {
            let end_ms = seg.end_ms.max(seg.start_ms + 1);
            out.push_str(&format!(
                "{}
{} --> {}
<v {}>{}

",
                seg.id,
                vtt_timestamp(seg.start_ms),
                vtt_timestamp(end_ms),
                self.label_for(&seg.speaker_key),
                vtt_escape(seg.text.trim())
            ));
        }
        out
    }
}

/// `HH:MM:SS.mmm` — WebVTT's long timestamp form (hours are optional in the
/// spec but every parser accepts them, and meetings do run past an hour).
fn vtt_timestamp(ms: u64) -> String {
    format!(
        "{:02}:{:02}:{:02}.{:03}",
        ms / 3_600_000,
        (ms / 60_000) % 60,
        (ms / 1000) % 60,
        ms % 1000
    )
}

/// Escape the three characters WebVTT reserves inside cue text.
fn vtt_escape(text: &str) -> String {
    text.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
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

    fn vtt_fixture() -> Transcript {
        Transcript {
            meeting_id: "m1".into(),
            language: None,
            engine: "whisper.cpp".into(),
            segments: vec![
                TranscriptSegment {
                    id: "seg-1".into(),
                    speaker_key: "mic".into(),
                    start_ms: 1500,
                    end_ms: 62_250,
                    text: " Budget <b>&</b> scope ".into(),
                    words: vec![],
                },
                TranscriptSegment {
                    id: "seg-2".into(),
                    speaker_key: "spk_0".into(),
                    start_ms: 3_600_000,
                    end_ms: 3_600_000,
                    text: "ok".into(),
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
    fn to_vtt_renders_cues_with_voice_tags_and_escapes() {
        let vtt = vtt_fixture().to_vtt();
        assert!(
            vtt.starts_with(
                "WEBVTT

"
            ),
            "{vtt}"
        );
        assert!(
            vtt.contains(
                "seg-1
00:00:01.500 --> 00:01:02.250
<v You>Budget &lt;b&gt;&amp;&lt;/b&gt; scope

"
            ),
            "{vtt}"
        );
        // a zero-length cue is invalid WebVTT: end is nudged 1 ms past start
        assert!(
            vtt.contains(
                "seg-2
01:00:00.000 --> 01:00:00.001
<v Dana>ok
"
            ),
            "{vtt}"
        );
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
    fn reclaim_keeps_text_and_makes_it_user() {
        let mut b = NoteBlock::ai("launch likely moves to May", vec![]);
        b.reclaim();
        assert_eq!(b.origin, BlockOrigin::User);
        assert_eq!(b.markdown, "launch likely moves to May");
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

    fn flag(kind: BlockFlagKind, detail: &str) -> BlockFlag {
        BlockFlag {
            kind,
            detail: Some(detail.into()),
        }
    }

    /// Notes saved before the Enhance check have no `flags`; they must still
    /// load, and an unflagged block must still serialize exactly as before
    /// (older builds, note.json mirrors and exports see no new field).
    #[test]
    fn flags_are_optional_on_disk() {
        let old =
            r#"{"id":"b1","origin":{"kind":"ai","source_segment_ids":["s1"]},"markdown":"x"}"#;
        let b: NoteBlock = serde_json::from_str(old).unwrap();
        assert!(b.flags.is_empty());
        assert_eq!(serde_json::to_string(&b).unwrap(), old);

        let flagged = NoteBlock::ai_flagged(
            "Priya asked for $63k.",
            vec!["s1".into()],
            vec![flag(BlockFlagKind::Number, "63000")],
        );
        let json = serde_json::to_string(&flagged).unwrap();
        assert!(
            json.contains(r#""flags":[{"kind":"number","detail":"63000"}]"#),
            "{json}"
        );
        assert_eq!(serde_json::from_str::<NoteBlock>(&json).unwrap(), flagged);
        // A flag without a detail round-trips without one.
        let bare = NoteBlock::ai_flagged(
            "x",
            vec![],
            vec![BlockFlag {
                kind: BlockFlagKind::UncitedClaim,
                detail: None,
            }],
        );
        assert!(serde_json::to_string(&bare)
            .unwrap()
            .contains(r#""flags":[{"kind":"uncited_claim"}]"#));
    }

    /// The review-round flags and the partial-read marker are optional on
    /// disk too, in the names the frontend and the MCP read.
    #[test]
    fn the_partial_read_and_the_newer_flags_serialize_as_the_clients_read_them() {
        for (kind, name) in [
            (BlockFlagKind::AimedAtAi, "aimed_at_ai"),
            (BlockFlagKind::Instruction, "instruction"),
            (BlockFlagKind::Reversed, "reversed"),
        ] {
            let json = serde_json::to_string(&BlockFlag { kind, detail: None }).unwrap();
            assert_eq!(json, format!(r#"{{"kind":"{name}"}}"#));
        }
        let mut b = NoteBlock::ai("x", vec!["s1".into()]);
        assert!(!serde_json::to_string(&b).unwrap().contains("partial"));
        b.partial = Some(PartialRead {
            shown: 412,
            total: 1980,
            cut_off: false,
        });
        let json = serde_json::to_string(&b).unwrap();
        // No cut: the same bytes as before the field existed.
        assert!(
            json.contains(r#""partial":{"shown":412,"total":1980}"#),
            "{json}"
        );
        assert_eq!(serde_json::from_str::<NoteBlock>(&json).unwrap(), b);
        // A note saved before `cut_off` existed loads unchanged.
        let old = json.clone();
        assert_eq!(
            serde_json::from_str::<NoteBlock>(&old).unwrap().partial,
            b.partial
        );
        // A cut-off reply says so, on a whole read too.
        b.partial = Some(PartialRead {
            shown: 1980,
            total: 1980,
            cut_off: true,
        });
        let json = serde_json::to_string(&b).unwrap();
        assert!(
            json.contains(r#""partial":{"shown":1980,"total":1980,"cut_off":true}"#),
            "{json}"
        );
        assert_eq!(serde_json::from_str::<NoteBlock>(&json).unwrap(), b);
        // Keeping the line drops it with the AI origin; the user's own text
        // never carries one.
        b.reclaim();
        assert!(b.partial.is_none());
        assert!(NoteBlock::user("mine").partial.is_none());
    }

    /// Keeping or editing a flagged AI line makes it the user's text, and
    /// the flags (which describe AI text) go with the AI origin.
    #[test]
    fn keeping_or_editing_a_flagged_block_clears_its_flags() {
        let mk = || {
            NoteBlock::ai_flagged(
                "Priya asked for $63k.",
                vec!["s1".into()],
                vec![flag(BlockFlagKind::Number, "63000")],
            )
        };
        let mut kept = mk();
        kept.reclaim();
        assert_eq!(kept.origin, BlockOrigin::User);
        assert!(kept.flags.is_empty());
        let mut edited = mk();
        edited.apply_edit("Priya asked for $6,300.");
        assert!(edited.flags.is_empty() && edited.origin == BlockOrigin::User);
        // An edit that changes nothing leaves the AI block (and its flags).
        let mut same = mk();
        same.apply_edit("Priya asked for $63k.");
        assert_eq!(same.flags.len(), 1);
    }
}
