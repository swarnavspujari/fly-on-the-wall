//! Ask approvals (spec §4.2a #5): what the user may approve from an Ask
//! answer (the model's proposals, and the answer's own "Insert") is kept
//! here by id until it expires. The approval commands take only that id,
//! so they apply exactly what the harness validated, and validate it again
//! against storage. Approving is idempotent: a second click gets the first
//! result back and adds nothing. Every attempt writes an audit row of ids,
//! codes and a time, never text.

use std::collections::HashMap;
use std::sync::{LazyLock, Mutex};

use chrono::{DateTime, Duration, SecondsFormat, Utc};
use fly_ask::proposals::{Proposal, ProposalTool, PROPOSAL_TTL_MINUTES};
use fly_core::{MeetingItem, Note};
use fly_storage::Storage;
use serde::Serialize;
use sha2::{Digest, Sha256};

/// Longest AI block an Ask insert may add.
const MAX_INSERT_CHARS: usize = 8_000;
/// Longest action item an Ask proposal may add.
const MAX_ITEM_CHARS: usize = 400;
/// An applied approval is remembered this long past its expiry, so a late
/// second click still gets the first result instead of an error.
const KEEP_AFTER_EXPIRY_MINUTES: i64 = 60;
/// Most approvals held at once (the soonest to expire go first).
const MAX_PENDING: usize = 500;

/// The app's approvals (one per process; nothing survives a restart, and
/// an approval outlives its answer by at most `PROPOSAL_TTL_MINUTES`).
pub static APPROVALS: LazyLock<Approvals> = LazyLock::new(Approvals::default);

/// What an approval does.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Kind {
    /// An AI block in a note (an answer's Insert, or `insert_into_note`).
    Insert,
    /// A user action item (`add_action_item`).
    ActionItem,
}

#[derive(Debug, Clone)]
enum Approval {
    Insert {
        note_id: String,
        markdown: String,
        segment_ids: Vec<String>,
    },
    ActionItem {
        meeting_id: String,
        text: String,
        owner: Option<String>,
        segment_ids: Vec<String>,
    },
}

impl Approval {
    fn kind(&self) -> Kind {
        match self {
            Approval::Insert { .. } => Kind::Insert,
            Approval::ActionItem { .. } => Kind::ActionItem,
        }
    }
}

/// What an approval wrote.
#[derive(Debug, Clone)]
pub enum Applied {
    Note(Note),
    Item(MeetingItem),
}

struct Pending {
    approval: Approval,
    expires_at: DateTime<Utc>,
    applied: Option<Applied>,
}

/// How an approval attempt ended (the audit row's code).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Outcome {
    Applied,
    /// Approved before: the first result, nothing added.
    AlreadyApplied,
    Expired,
    Unknown,
    /// The id is for the other command.
    WrongKind,
    /// It failed validation against storage now.
    Rejected,
}

/// One audit row: ids, codes and a time; never text.
#[derive(Debug, Clone, Serialize)]
pub struct AuditRow {
    pub at: String,
    pub approval_id: String,
    pub kind: Kind,
    pub outcome: Outcome,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub note_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub meeting_id: Option<String>,
    /// The block or item it created.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub created_id: Option<String>,
}

#[derive(Default)]
pub struct Approvals {
    pending: Mutex<HashMap<String, Pending>>,
}

impl Approvals {
    /// Hold a model proposal for approval (an id already held keeps its
    /// state: the same proposal from a repeated question is one approval).
    pub fn register_proposal(&self, p: &Proposal, now: DateTime<Utc>) {
        let approval = match p.tool {
            ProposalTool::InsertIntoNote => Approval::Insert {
                note_id: p.note_id.clone(),
                markdown: p.markdown.clone().unwrap_or_default(),
                segment_ids: p.segment_ids.clone(),
            },
            ProposalTool::AddActionItem => Approval::ActionItem {
                meeting_id: p.meeting_id.clone(),
                text: p.text.clone().unwrap_or_default(),
                owner: p.owner.clone(),
                segment_ids: p.segment_ids.clone(),
            },
        };
        self.hold(p.id.clone(), approval, p.expires_at, now);
    }

    /// Hold an answer's own "Insert into note"; returns its id.
    pub fn register_answer(
        &self,
        note_id: &str,
        markdown: &str,
        segment_ids: &[String],
        now: DateTime<Utc>,
    ) -> String {
        let id = answer_id(note_id, markdown, segment_ids);
        self.hold(
            id.clone(),
            Approval::Insert {
                note_id: note_id.to_string(),
                markdown: markdown.to_string(),
                segment_ids: segment_ids.to_vec(),
            },
            now + Duration::minutes(PROPOSAL_TTL_MINUTES),
            now,
        );
        id
    }

    fn hold(&self, id: String, approval: Approval, expires_at: DateTime<Utc>, now: DateTime<Utc>) {
        let mut map = self.pending.lock().unwrap();
        prune(&mut map, now);
        let p = map.entry(id).or_insert(Pending {
            approval,
            expires_at,
            applied: None,
        });
        // Asked again before it ran out: the button lives on.
        if p.applied.is_none() && expires_at > p.expires_at {
            p.expires_at = expires_at;
        }
    }

    /// Approve `id` as `want`: write it (validated again) or return what
    /// an earlier approval wrote. The lock is held across the write, so two
    /// clicks at once write once.
    pub fn approve(
        &self,
        storage: &Storage,
        id: &str,
        want: Kind,
        now: DateTime<Utc>,
    ) -> (Result<Applied, String>, AuditRow) {
        let mut row = AuditRow {
            at: now.to_rfc3339_opts(SecondsFormat::Millis, true),
            approval_id: safe_id(id),
            kind: want,
            outcome: Outcome::Unknown,
            note_id: None,
            meeting_id: None,
            created_id: None,
        };
        let mut map = self.pending.lock().unwrap();
        prune(&mut map, now);
        let Some(p) = map.get_mut(id) else {
            return (Err(EXPIRED.into()), row);
        };
        match &p.approval {
            Approval::Insert { note_id, .. } => row.note_id = Some(note_id.clone()),
            Approval::ActionItem { meeting_id, .. } => row.meeting_id = Some(meeting_id.clone()),
        }
        if p.approval.kind() != want {
            row.outcome = Outcome::WrongKind;
            return (Err("That isn't this kind of suggestion.".into()), row);
        }
        if let Some(done) = &p.applied {
            row.outcome = Outcome::AlreadyApplied;
            row.created_id = created_id(done);
            return (Ok(done.clone()), row);
        }
        if now > p.expires_at {
            row.outcome = Outcome::Expired;
            return (Err(EXPIRED.into()), row);
        }
        let result = match &p.approval {
            Approval::Insert {
                note_id,
                markdown,
                segment_ids,
            } => apply_insert(storage, note_id, markdown, segment_ids).map(Applied::Note),
            Approval::ActionItem {
                meeting_id,
                text,
                owner,
                segment_ids,
            } => apply_item(storage, meeting_id, text, owner.as_deref(), segment_ids)
                .map(Applied::Item),
        };
        match result {
            Ok(done) => {
                row.outcome = Outcome::Applied;
                row.created_id = created_id(&done);
                p.applied = Some(done.clone());
                (Ok(done), row)
            }
            Err(e) => {
                row.outcome = Outcome::Rejected;
                (Err(e), row)
            }
        }
    }
}

const EXPIRED: &str = "This suggestion has expired. Ask again to get a fresh one.";

fn prune(map: &mut HashMap<String, Pending>, now: DateTime<Utc>) {
    let keep = Duration::minutes(KEEP_AFTER_EXPIRY_MINUTES);
    map.retain(|_, p| p.expires_at + keep > now);
    while map.len() >= MAX_PENDING {
        let Some(first) = map
            .iter()
            .min_by_key(|(_, p)| p.expires_at)
            .map(|(k, _)| k.clone())
        else {
            break;
        };
        map.remove(&first);
    }
}

/// An answer insert's id: the same answer for the same note is one button.
fn answer_id(note_id: &str, markdown: &str, ids: &[String]) -> String {
    let mut h = Sha256::new();
    h.update("answer_insert");
    h.update([0]);
    h.update(note_id);
    h.update([0]);
    h.update(markdown.trim());
    for id in ids {
        h.update([0]);
        h.update(id);
    }
    h.finalize()
        .iter()
        .take(8)
        .map(|b| format!("{b:02x}"))
        .collect()
}

/// The id as logged: the webview passes it, so only a bounded token.
fn safe_id(id: &str) -> String {
    id.chars()
        .filter(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_'))
        .take(64)
        .collect()
}

fn created_id(done: &Applied) -> Option<String> {
    match done {
        Applied::Note(n) => n.blocks.last().map(|b| b.id.clone()),
        Applied::Item(i) => Some(i.id.clone()),
    }
}

/// Segment ids of a note's transcripts.
fn note_segment_ids(
    storage: &Storage,
    note_id: &str,
) -> Result<std::collections::HashSet<String>, String> {
    let mut ids = std::collections::HashSet::new();
    for m in storage
        .meetings_for_note(note_id)
        .map_err(|e| e.to_string())?
    {
        if let Some(t) = storage.get_transcript(&m.id).map_err(|e| e.to_string())? {
            ids.extend(t.segments.into_iter().map(|s| s.id));
        }
    }
    Ok(ids)
}

/// Append an AI block citing its source lines (spec §3.5 #6). Validated
/// again: links removed, only segment ids of this note's own transcripts
/// kept (citation locators are recorded by the block write).
fn apply_insert(
    storage: &Storage,
    note_id: &str,
    markdown: &str,
    segment_ids: &[String],
) -> Result<Note, String> {
    let markdown = fly_ask::text::strip_links(markdown.trim()).0;
    if markdown.trim().is_empty() {
        return Err("nothing to insert".into());
    }
    if markdown.chars().count() > MAX_INSERT_CHARS {
        return Err("that answer is too long to insert".into());
    }
    let mut note = storage.get_note(note_id).map_err(|e| e.to_string())?;
    let known = note_segment_ids(storage, note_id)?;
    let mut ids: Vec<String> = Vec::new();
    for id in segment_ids {
        if known.contains(id) && !ids.contains(id) {
            ids.push(id.clone());
        }
    }
    note.blocks.push(fly_core::NoteBlock::ai(markdown, ids));
    storage
        .update_note_blocks(note_id, &note.blocks)
        .map_err(|e| e.to_string())
}

/// Add a user action item. Validated again: it must cite lines of this
/// meeting; its owner is nobody, or a speaker or attendee of the meeting
/// (never inferred). Kept across re-extraction (`extracted_by` =
/// `user:ask`).
fn apply_item(
    storage: &Storage,
    meeting_id: &str,
    text: &str,
    owner: Option<&str>,
    segment_ids: &[String],
) -> Result<MeetingItem, String> {
    let text = fly_ask::text::strip_links(text.trim()).0;
    if text.trim().is_empty() {
        return Err("the action item is empty".into());
    }
    if text.chars().count() > MAX_ITEM_CHARS {
        return Err("that action item is too long".into());
    }
    let meeting = storage.get_meeting(meeting_id).map_err(|e| e.to_string())?;
    let transcript = storage
        .get_transcript(&meeting.id)
        .map_err(|e| e.to_string())?
        .ok_or("this meeting has no transcript yet")?;
    let mut ids: Vec<String> = Vec::new();
    for id in segment_ids {
        if transcript.segments.iter().any(|s| &s.id == id) && !ids.contains(id) {
            ids.push(id.clone());
        }
    }
    let Some(first) = ids
        .first()
        .and_then(|id| transcript.segments.iter().find(|s| &s.id == id))
    else {
        return Err("an action item needs a source line from this meeting".into());
    };
    let owner = owner.map(str::trim).filter(|o| !o.is_empty());
    if let Some(o) = owner {
        let known = transcript
            .speakers
            .iter()
            .map(|s| s.label.as_str())
            .chain(meeting.attendees.iter().map(|a| a.display_name()));
        if !known.into_iter().any(|k| k.trim().eq_ignore_ascii_case(o)) {
            return Err("the owner must be a speaker or attendee of this meeting".into());
        }
    }
    let item = MeetingItem {
        id: fly_core::new_id(),
        meeting_id: meeting.id.clone(),
        kind: fly_core::ItemKind::ActionItem,
        text,
        owner: owner.map(str::to_string),
        status: Some("open".into()),
        speaker_key: Some(first.speaker_key.clone()),
        segment_ids: ids,
        created_at: Utc::now(),
        extracted_by: format!("{}ask", fly_storage::USER_ITEM_PREFIX),
    };
    storage.add_meeting_item(&item).map_err(|e| e.to_string())?;
    Ok(item)
}

#[cfg(test)]
mod tests {
    use super::*;
    use fly_core::{Speaker, Transcript, TranscriptSegment};

    struct Fx {
        _dir: tempfile::TempDir,
        s: Storage,
        note: String,
        meeting: String,
    }

    fn fx() -> Fx {
        let dir = tempfile::tempdir().unwrap();
        let s = Storage::open(dir.path()).unwrap();
        let note = s.create_note("Kiln rota review", None).unwrap();
        let meeting = s.create_meeting("Kiln rota review", &note.id, &[]).unwrap();
        s.save_transcript(&Transcript {
            meeting_id: meeting.id.clone(),
            language: Some("en".into()),
            engine: "test".into(),
            segments: vec![TranscriptSegment {
                id: "r01".into(),
                speaker_key: "spk_0".into(),
                start_ms: 0,
                end_ms: 4_000,
                text: "I'll post the kiln rota by Thursday.".into(),
                words: vec![],
            }],
            speakers: vec![Speaker {
                key: "spk_0".into(),
                label: "Ilka".into(),
            }],
        })
        .unwrap();
        Fx {
            _dir: dir,
            s,
            note: note.id,
            meeting: meeting.id,
        }
    }

    fn item_proposal(f: &Fx, now: DateTime<Utc>) -> Proposal {
        Proposal {
            id: "p1".into(),
            tool: ProposalTool::AddActionItem,
            gate: fly_ask::proposals::Gate::Review,
            meeting_id: f.meeting.clone(),
            note_id: f.note.clone(),
            markdown: None,
            text: Some("Ilka posts the kiln rota by Thursday".into()),
            owner: Some("Ilka".into()),
            segment_ids: vec!["r01".into()],
            expires_at: now + Duration::minutes(PROPOSAL_TTL_MINUTES),
            executed: false,
        }
    }

    #[test]
    fn a_second_click_adds_nothing_and_returns_the_first_result() {
        let f = fx();
        let a = Approvals::default();
        let now = Utc::now();
        a.register_proposal(&item_proposal(&f, now), now);
        let (first, row) = a.approve(&f.s, "p1", Kind::ActionItem, now);
        let Ok(Applied::Item(first)) = first else {
            panic!("{first:?}")
        };
        assert_eq!(row.outcome, Outcome::Applied);
        assert_eq!(row.created_id.as_deref(), Some(first.id.as_str()));
        let (again, row) = a.approve(&f.s, "p1", Kind::ActionItem, now);
        let Ok(Applied::Item(again)) = again else {
            panic!("{again:?}")
        };
        assert_eq!(again.id, first.id);
        assert_eq!(row.outcome, Outcome::AlreadyApplied);
        let items =
            f.s.get_meeting_items(&f.meeting)
                .unwrap()
                .into_iter()
                .filter(|i| i.extracted_by.starts_with(fly_storage::USER_ITEM_PREFIX))
                .count();
        assert_eq!(items, 1, "one item for two clicks");
    }

    #[test]
    fn unknown_expired_and_wrong_kind_ids_write_nothing() {
        let f = fx();
        let a = Approvals::default();
        let now = Utc::now();
        let (r, row) = a.approve(&f.s, "nope", Kind::ActionItem, now);
        assert!(r.is_err());
        assert_eq!(row.outcome, Outcome::Unknown);

        a.register_proposal(&item_proposal(&f, now), now);
        let (r, row) = a.approve(&f.s, "p1", Kind::Insert, now);
        assert!(r.is_err());
        assert_eq!(row.outcome, Outcome::WrongKind);

        let later = now + Duration::minutes(PROPOSAL_TTL_MINUTES + 1);
        let (r, row) = a.approve(&f.s, "p1", Kind::ActionItem, later);
        assert_eq!(r.unwrap_err(), EXPIRED);
        assert_eq!(row.outcome, Outcome::Expired);
        assert!(f.s.get_meeting_items(&f.meeting).unwrap().is_empty());
    }

    #[test]
    fn an_answer_insert_is_validated_again_and_logged_without_text() {
        let f = fx();
        let a = Approvals::default();
        let now = Utc::now();
        let ids = vec!["r01".to_string(), "not-in-this-note".to_string()];
        let md = "Ilka posts the kiln rota by Thursday. See [the rota](https://x.example).";
        let id = a.register_answer(&f.note, md, &ids, now);
        assert_eq!(id, a.register_answer(&f.note, md, &ids, now), "one button");
        let (r, row) = a.approve(&f.s, &id, Kind::Insert, now);
        let Ok(Applied::Note(note)) = r else {
            panic!("{r:?}")
        };
        let block = note.blocks.last().unwrap();
        assert!(!block.markdown.contains("http"), "{}", block.markdown);
        match &block.origin {
            fly_core::BlockOrigin::Ai { source_segment_ids } => {
                assert_eq!(source_segment_ids, &vec!["r01".to_string()])
            }
            o => panic!("{o:?}"),
        }
        let logged = serde_json::to_string(&row).unwrap();
        assert!(
            !logged.contains("kiln") && !logged.contains("Ilka"),
            "{logged}"
        );
        assert_eq!(row.outcome, Outcome::Applied);
        // A webview-supplied id is logged as a bounded token.
        let (_, row) = a.approve(&f.s, "<script>x</script>", Kind::Insert, now);
        assert_eq!(row.approval_id, "scriptxscript");
    }
}
