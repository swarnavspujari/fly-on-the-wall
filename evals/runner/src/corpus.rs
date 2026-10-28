//! The synthetic fixture corpus (`evals/fixtures/*.json`): loading,
//! validation, and building a fresh temporary database from it through
//! `fly-storage`'s public API only (so it survives schema changes).

use std::collections::{BTreeMap, HashSet};
use std::path::Path;

use anyhow::{bail, ensure, Context as _, Result};
use chrono::{DateTime, Utc};
use fly_core::{Attendee, ItemKind, MeetingItem, Speaker, Transcript, TranscriptSegment};
use fly_storage::Storage;
use serde::Deserialize;

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AttendeeFixture {
    pub name: String,
    #[serde(default)]
    pub email: Option<String>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ItemFixture {
    pub kind: String,
    pub text: String,
    #[serde(default)]
    pub owner: Option<String>,
    pub segments: Vec<String>,
}

/// `[id, speaker_key, "m:ss", text]`.
pub type SegmentRow = (String, String, String, String);

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct MeetingFixture {
    pub id: String,
    pub title: String,
    pub started_at: DateTime<Utc>,
    /// Folder path, `/`-separated ("Customers/Harrowgate").
    #[serde(default)]
    pub folder: Option<String>,
    pub keep_local: bool,
    /// Phrases that occur only in this (keep-local) meeting.
    #[serde(default)]
    pub canaries: Vec<String>,
    /// Holds injected speech for red-team scenarios.
    #[serde(default)]
    pub red_team: bool,
    pub attendees: Vec<AttendeeFixture>,
    pub speakers: BTreeMap<String, String>,
    #[serde(default)]
    pub scratchpad: String,
    pub segments: Vec<SegmentRow>,
    #[serde(default)]
    pub items: Vec<ItemFixture>,
}

/// One transcript line, resolved.
#[derive(Debug, Clone)]
pub struct Segment {
    pub id: String,
    pub meeting: String,
    pub speaker_key: String,
    pub speaker: String,
    pub start_ms: u64,
    pub end_ms: u64,
    pub text: String,
}

#[derive(Debug, Clone)]
pub struct Corpus {
    pub meetings: Vec<MeetingFixture>,
    segments: BTreeMap<String, Segment>,
}

fn parse_mss(s: &str) -> Result<u64> {
    let (m, sec) = s
        .split_once(':')
        .with_context(|| format!("time {s:?} is not m:ss"))?;
    let m: u64 = m.parse().with_context(|| format!("time {s:?}"))?;
    let sec: u64 = sec.parse().with_context(|| format!("time {s:?}"))?;
    ensure!(sec < 60, "time {s:?}: seconds must be < 60");
    Ok((m * 60 + sec) * 1000)
}

impl Corpus {
    pub fn load(dir: &Path) -> Result<Self> {
        let mut meetings = Vec::new();
        let mut paths: Vec<_> = std::fs::read_dir(dir)
            .with_context(|| format!("read {}", dir.display()))?
            .filter_map(|e| e.ok().map(|e| e.path()))
            .filter(|p| p.extension().is_some_and(|e| e == "json"))
            .collect();
        paths.sort();
        for p in paths {
            let text = std::fs::read_to_string(&p)?;
            let m: MeetingFixture =
                serde_json::from_str(&text).with_context(|| format!("parse {}", p.display()))?;
            meetings.push(m);
        }
        meetings.sort_by(|a, b| a.id.cmp(&b.id));
        let mut segments = BTreeMap::new();
        for m in &meetings {
            let rows = &m.segments;
            for (i, (id, key, start, text)) in rows.iter().enumerate() {
                let start_ms = parse_mss(start).with_context(|| id.to_string())?;
                let next = rows.get(i + 1).map(|r| parse_mss(&r.2)).transpose()?;
                let words = text.split_whitespace().count() as u64;
                let natural = start_ms + 600 + words * 380;
                let end_ms = next.map_or(natural, |n| natural.min(n.saturating_sub(200)));
                let seg = Segment {
                    id: id.clone(),
                    meeting: m.id.clone(),
                    speaker_key: key.clone(),
                    speaker: m.speakers.get(key).cloned().unwrap_or_default(),
                    start_ms,
                    end_ms: end_ms.max(start_ms + 500),
                    text: text.clone(),
                };
                if segments.insert(id.clone(), seg).is_some() {
                    bail!("segment id {id} appears twice");
                }
            }
        }
        let c = Corpus { meetings, segments };
        c.validate()?;
        Ok(c)
    }

    /// Structural checks; any failure means the fixtures are wrong.
    fn validate(&self) -> Result<()> {
        ensure!(!self.meetings.is_empty(), "no fixtures");
        let mut ids = HashSet::new();
        for m in &self.meetings {
            ensure!(ids.insert(&m.id), "meeting id {} appears twice", m.id);
            ensure!(!m.segments.is_empty(), "{} has no segments", m.id);
            ensure!(
                m.speakers.contains_key("mic"),
                "{} has no mic speaker",
                m.id
            );
            let mut last = 0;
            for (id, key, start, _) in &m.segments {
                ensure!(
                    id.starts_with(&format!("{}-", m.id)),
                    "segment {id} must start with {}-",
                    m.id
                );
                ensure!(m.speakers.contains_key(key), "{id}: unknown speaker {key}");
                let t = parse_mss(start)?;
                ensure!(t >= last, "{id}: start time goes backwards");
                last = t;
            }
            for item in &m.items {
                ensure!(
                    ItemKind::parse(&item.kind).is_some(),
                    "{}: unknown item kind {}",
                    m.id,
                    item.kind
                );
                for s in &item.segments {
                    ensure!(
                        self.segments.get(s).is_some_and(|x| x.meeting == m.id),
                        "{}: item cites {s}, which isn't in this meeting",
                        m.id
                    );
                }
            }
            if m.keep_local {
                ensure!(
                    !m.canaries.is_empty(),
                    "keep-local meeting {} needs canaries",
                    m.id
                );
            }
        }
        ensure!(
            self.meetings.iter().any(|m| m.keep_local),
            "the corpus needs a keep-local meeting"
        );
        // Canaries must be unique to their meeting, or a leak check would
        // fire on shareable text.
        for m in self.meetings.iter().filter(|m| m.keep_local) {
            for canary in &m.canaries {
                let needle = canary.to_lowercase();
                ensure!(
                    self.meeting_text(m).contains(&needle),
                    "canary {canary:?} doesn't occur in {}",
                    m.id
                );
                for other in self.meetings.iter().filter(|o| !o.keep_local) {
                    ensure!(
                        !self.meeting_text(other).contains(&needle),
                        "canary {canary:?} of {} also occurs in {}",
                        m.id,
                        other.id
                    );
                }
            }
        }
        Ok(())
    }

    /// Everything a meeting contributes, lowercased (for canary checks).
    fn meeting_text(&self, m: &MeetingFixture) -> String {
        let mut s = format!("{}\n{}\n", m.title, m.scratchpad);
        for (_, _, _, t) in &m.segments {
            s.push_str(t);
            s.push('\n');
        }
        for i in &m.items {
            s.push_str(&i.text);
            s.push('\n');
        }
        s.to_lowercase()
    }

    pub fn segment(&self, id: &str) -> Option<&Segment> {
        self.segments.get(id)
    }

    pub fn segments(&self) -> impl Iterator<Item = &Segment> {
        self.segments.values()
    }

    pub fn meeting(&self, id: &str) -> Option<&MeetingFixture> {
        self.meetings.iter().find(|m| m.id == id)
    }

    pub fn keep_local_meetings(&self) -> impl Iterator<Item = &MeetingFixture> {
        self.meetings.iter().filter(|m| m.keep_local)
    }

    /// Lowercased canary phrases of every keep-local meeting.
    pub fn canaries(&self) -> Vec<String> {
        self.keep_local_meetings()
            .flat_map(|m| m.canaries.iter().map(|c| c.to_lowercase()))
            .collect()
    }

    pub fn is_keep_local_segment(&self, id: &str) -> bool {
        self.segment(id)
            .and_then(|s| self.meeting(&s.meeting))
            .is_some_and(|m| m.keep_local)
    }

    /// True when `folder_path` is `meeting`'s folder or one of its parents.
    pub fn in_folder(&self, meeting: &str, folder_path: &str) -> bool {
        self.meeting(meeting)
            .and_then(|m| m.folder.as_deref())
            .is_some_and(|f| f == folder_path || f.starts_with(&format!("{folder_path}/")))
    }

    /// Every folder path, parents included.
    pub fn folder_paths(&self) -> Vec<String> {
        let mut out: Vec<String> = Vec::new();
        for m in &self.meetings {
            if let Some(f) = &m.folder {
                let parts: Vec<&str> = f.split('/').collect();
                for i in 1..=parts.len() {
                    let p = parts[..i].join("/");
                    if !out.contains(&p) {
                        out.push(p);
                    }
                }
            }
        }
        out
    }
}

/// A fixture database in a temp dir, plus fixture-id → database-id maps.
pub struct Db {
    _dir: tempfile::TempDir,
    pub storage: Storage,
    /// fixture meeting id → meeting id
    pub meeting_ids: BTreeMap<String, String>,
    /// fixture meeting id → note id
    pub note_ids: BTreeMap<String, String>,
    /// folder path → folder id
    pub folder_ids: BTreeMap<String, String>,
}

impl Db {
    /// Build the database. `embed` also stores `hashed-bow-v1` vectors for
    /// every chunk, so hybrid search has a vector list.
    pub fn build(corpus: &Corpus, embed: bool) -> Result<Self> {
        let dir = tempfile::tempdir()?;
        let storage = Storage::open(dir.path())?;
        let mut folder_ids: BTreeMap<String, String> = BTreeMap::new();
        for path in corpus.folder_paths() {
            let (parent, name) = match path.rsplit_once('/') {
                Some((p, n)) => (Some(folder_ids[p].clone()), n.to_string()),
                None => (None, path.clone()),
            };
            let f = storage.create_folder(&name, parent.as_deref())?;
            folder_ids.insert(path.clone(), f.id);
        }
        let mut meeting_ids = BTreeMap::new();
        let mut note_ids = BTreeMap::new();
        for m in &corpus.meetings {
            let folder = m.folder.as_ref().map(|f| folder_ids[f].clone());
            let note = storage.create_note(&m.title, folder.as_deref())?;
            if !m.scratchpad.is_empty() {
                storage.update_note_scratchpad(&note.id, &m.scratchpad)?;
            }
            let attendees: Vec<Attendee> = m
                .attendees
                .iter()
                .map(|a| Attendee {
                    name: a.name.clone(),
                    email: a.email.clone(),
                })
                .collect();
            let meeting = storage.create_meeting(&m.title, &note.id, &attendees)?;
            storage.set_meeting_started_at(&meeting.id, m.started_at)?;
            let mut speakers: Vec<Speaker> = vec![Speaker {
                key: "mic".into(),
                label: m.speakers["mic"].clone(),
            }];
            speakers.extend(m.speakers.iter().filter(|(k, _)| k.as_str() != "mic").map(
                |(k, v)| Speaker {
                    key: k.clone(),
                    label: v.clone(),
                },
            ));
            let segments: Vec<TranscriptSegment> = m
                .segments
                .iter()
                .map(|(id, ..)| {
                    let s = corpus.segment(id).expect("validated");
                    TranscriptSegment {
                        id: s.id.clone(),
                        speaker_key: s.speaker_key.clone(),
                        start_ms: s.start_ms,
                        end_ms: s.end_ms,
                        text: s.text.clone(),
                        words: vec![],
                    }
                })
                .collect();
            storage.save_transcript(&Transcript {
                meeting_id: meeting.id.clone(),
                language: Some("en".into()),
                engine: "fixture".into(),
                segments,
                speakers,
            })?;
            let items: Vec<MeetingItem> = m
                .items
                .iter()
                .map(|i| {
                    let kind = ItemKind::parse(&i.kind).expect("validated");
                    MeetingItem {
                        id: fly_core::new_id(),
                        meeting_id: meeting.id.clone(),
                        kind,
                        text: i.text.clone(),
                        owner: i.owner.clone(),
                        status: (kind == ItemKind::ActionItem).then(|| "open".to_string()),
                        speaker_key: i
                            .segments
                            .first()
                            .and_then(|s| corpus.segment(s))
                            .map(|s| s.speaker_key.clone()),
                        segment_ids: i.segments.clone(),
                        created_at: m.started_at,
                        extracted_by: "fixture".into(),
                    }
                })
                .collect();
            storage.replace_meeting_items(&meeting.id, &items)?;
            if m.keep_local {
                storage.set_note_keep_local(&note.id, true)?;
            }
            meeting_ids.insert(m.id.clone(), meeting.id);
            note_ids.insert(m.id.clone(), note.id);
        }
        if embed {
            crate::embed::embed_pending(&storage)?;
        }
        Ok(Db {
            _dir: dir,
            storage,
            meeting_ids,
            note_ids,
            folder_ids,
        })
    }

    /// A second, fresh connection to the same database (cold-start timing).
    pub fn reopen(&self) -> Result<Storage> {
        Ok(Storage::open(self._dir.path())?)
    }

    /// Fixture meeting id for a database meeting id.
    pub fn fixture_of(&self, meeting_id: &str) -> Option<&str> {
        self.meeting_ids
            .iter()
            .find(|(_, v)| v.as_str() == meeting_id)
            .map(|(k, _)| k.as_str())
    }

    /// Replace `{meeting:mNN}`, `{note:mNN}` and `{folder:Path}` in every
    /// string of a JSON value.
    pub fn substitute(&self, v: &serde_json::Value) -> Result<serde_json::Value> {
        use serde_json::Value;
        Ok(match v {
            Value::String(s) => Value::String(self.substitute_str(s)?),
            Value::Array(a) => Value::Array(
                a.iter()
                    .map(|x| self.substitute(x))
                    .collect::<Result<_>>()?,
            ),
            Value::Object(o) => Value::Object(
                o.iter()
                    .map(|(k, x)| Ok((k.clone(), self.substitute(x)?)))
                    .collect::<Result<_>>()?,
            ),
            other => other.clone(),
        })
    }

    pub fn substitute_str(&self, s: &str) -> Result<String> {
        let mut out = String::new();
        let mut rest = s;
        while let Some(i) = rest.find('{') {
            let Some(j) = rest[i..].find('}') else { break };
            let inner = &rest[i + 1..i + j];
            let value = match inner.split_once(':') {
                Some(("meeting", id)) => self.meeting_ids.get(id),
                Some(("note", id)) => self.note_ids.get(id),
                Some(("folder", p)) => self.folder_ids.get(p),
                _ => None,
            };
            out.push_str(&rest[..i]);
            match value {
                Some(v) => out.push_str(v),
                None if inner.contains(':') => bail!("unknown placeholder {{{inner}}}"),
                None => out.push_str(&rest[i..i + j + 1]),
            }
            rest = &rest[i + j + 1..];
        }
        out.push_str(rest);
        Ok(out)
    }

    /// The database contents an Ask must never change: every note's
    /// scratchpad and blocks and every meeting's items (JSON, stable order).
    pub fn snapshot(&self) -> Result<String> {
        // Everything an Ask could plausibly write: each fixture meeting's
        // note (title, folder, scratchpad, blocks, timestamps), meeting row
        // (title, time, attendees, confirmation), raw and cleaned
        // transcripts (segments and speakers), items and keep-local flags,
        // plus the folder tree and the full note and meeting lists (so a
        // new row shows up too).
        let mut parts = Vec::new();
        for (fixture, mid) in &self.meeting_ids {
            let note_id = &self.note_ids[fixture];
            let mut items = self.storage.get_meeting_items(mid)?;
            items.sort_by(|a, b| a.id.cmp(&b.id));
            parts.push(serde_json::json!({
                "meeting": fixture,
                "note": self.storage.get_note(note_id)?,
                "row": self.storage.get_meeting(mid)?,
                "transcript": self.storage.get_transcript(mid)?,
                "cleaned": self.storage.get_cleaned_transcript(mid)?,
                "items": items,
                "note_keep_local": self.storage.note_keep_local(note_id),
                "meeting_keep_local": self.storage.meeting_keep_local(mid),
            }));
        }
        let mut folders = self.storage.list_folders()?;
        folders.sort_by(|a, b| a.id.cmp(&b.id));
        let mut meetings: Vec<String> = self
            .storage
            .list_meetings(None, usize::MAX >> 1)?
            .into_iter()
            .map(|m| m.id)
            .collect();
        meetings.sort();
        let mut notes: Vec<String> = self
            .storage
            .list_notes_in_folder(None)?
            .into_iter()
            .map(|n| n.id)
            .collect();
        for f in &folders {
            notes.extend(
                self.storage
                    .list_notes_in_folder(Some(&f.id))?
                    .into_iter()
                    .map(|n| n.id),
            );
        }
        notes.sort();
        parts.push(serde_json::json!({ "folders": folders, "meetings": meetings, "notes": notes }));
        Ok(serde_json::to_string(&parts)?)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_committed_fixtures_load_and_validate() {
        let c = Corpus::load(&crate::evals_dir().join("fixtures")).unwrap();
        assert!(c.meetings.len() >= 10);
        assert_eq!(c.keep_local_meetings().count(), 1);
        // Long meetings: many ~160-word passages, so recall@8 measures
        // passage ranking, not just finding the right meeting.
        let long = c
            .meetings
            .iter()
            .filter(|m| {
                m.segments
                    .iter()
                    .map(|s| s.3.split_whitespace().count())
                    .sum::<usize>()
                    >= 2000
            })
            .count();
        assert!(long >= 3, "{long} meetings of 2,000+ words");
        assert!(c.meetings.iter().any(|m| m.red_team));
        let s = c.segment("m02-04").unwrap();
        assert!(s.end_ms > s.start_ms && s.speaker == "Ines Carvalho");
    }

    #[test]
    fn the_database_builds_with_ids_preserved() {
        let c = Corpus::load(&crate::evals_dir().join("fixtures")).unwrap();
        let db = Db::build(&c, true).unwrap();
        let t = db
            .storage
            .get_transcript(&db.meeting_ids["m05"])
            .unwrap()
            .unwrap();
        assert_eq!(t.segments[0].id, "m05-01");
        assert!(db.storage.note_keep_local(&db.note_ids["m10"]));
        assert_eq!(
            db.substitute_str("x {meeting:m01} {folder:Customers/Harrowgate} {y}")
                .unwrap(),
            format!(
                "x {} {} {{y}}",
                db.meeting_ids["m01"], db.folder_ids["Customers/Harrowgate"]
            )
        );
        assert!(db.substitute_str("{meeting:m99}").is_err());
        assert_eq!(db.snapshot().unwrap(), db.snapshot().unwrap());
    }
}
