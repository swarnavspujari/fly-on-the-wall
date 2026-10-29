//! The synthetic fixture corpus (`evals/fixtures/*.json`): loading,
//! validation, and building a fresh temporary database from it through
//! `fly-storage`'s public API (so it survives schema changes). The one
//! exception is a read-only look at the `settings` table for the Ask
//! write check (fly-storage has no listing API for settings).
//!
//! Database ids are UUID-shaped like production's (`fly_core::new_id`).
//! Segment, item and block ids are derived from the fixture ids, so they're
//! the same in every build; [`Db`] maps between the two.

use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet};
use std::path::Path;

use anyhow::{bail, ensure, Context as _, Result};
use chrono::{DateTime, Utc};
use fly_core::{
    Attendee, BlockOrigin, ItemKind, MeetingItem, NoteBlock, Speaker, Transcript, TranscriptSegment,
};
use fly_storage::Storage;
use serde::Deserialize;
use sha2::{Digest as _, Sha256};

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

/// One block of the enhanced note. AI blocks cite the segments they were
/// derived from (`sources`); a block without sources is the user's.
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct EnhancedFixture {
    pub markdown: String,
    #[serde(default)]
    pub sources: Vec<String>,
}

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
    /// The enhanced note (Enhance's AI summary blocks and the user's).
    #[serde(default)]
    pub enhanced: Vec<EnhancedFixture>,
    /// The polished (cleaned-up) transcript: segment id → its cleaned text.
    /// Segments not listed keep their raw text.
    #[serde(default)]
    pub cleaned: BTreeMap<String, String>,
    /// A folder flagged Keep local: this meeting's folder or one of its
    /// parents. Every meeting under it is Keep local.
    #[serde(default)]
    pub keep_local_folder: Option<String>,
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
            for b in &m.enhanced {
                ensure!(
                    !b.markdown.trim().is_empty(),
                    "{}: empty enhanced block",
                    m.id
                );
                for s in &b.sources {
                    ensure!(
                        self.segments.get(s).is_some_and(|x| x.meeting == m.id),
                        "{}: enhanced block cites {s}, which isn't in this meeting",
                        m.id
                    );
                }
            }
            for s in m.cleaned.keys() {
                ensure!(
                    self.segments.get(s).is_some_and(|x| x.meeting == m.id),
                    "{}: cleaned text for {s}, which isn't in this meeting",
                    m.id
                );
            }
            if let Some(f) = &m.keep_local_folder {
                ensure!(
                    m.folder
                        .as_deref()
                        .is_some_and(|mf| mf == f || mf.starts_with(&format!("{f}/"))),
                    "{}: keep_local_folder {f} is not its folder or a parent",
                    m.id
                );
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
            if self.is_keep_local(m) {
                ensure!(
                    !m.canaries.is_empty(),
                    "keep-local meeting {} needs canaries",
                    m.id
                );
                // Every derived copy carries a canary, so a leak through
                // any of them is visible by text.
                let has = |t: &str| {
                    let l = t.to_lowercase();
                    m.canaries.iter().any(|c| l.contains(&c.to_lowercase()))
                };
                ensure!(
                    m.scratchpad.is_empty() || has(&m.scratchpad),
                    "{}: the scratchpad needs a canary",
                    m.id
                );
                for (i, b) in m.enhanced.iter().enumerate() {
                    ensure!(
                        has(&b.markdown),
                        "{}: enhanced block {i} needs a canary",
                        m.id
                    );
                }
                ensure!(
                    m.cleaned.is_empty() || m.cleaned.values().any(|t| has(t)),
                    "{}: the polished transcript needs a canary",
                    m.id
                );
                for item in &m.items {
                    ensure!(
                        has(&item.text),
                        "{}: item {:?} needs a canary",
                        m.id,
                        item.text
                    );
                }
            }
        }
        let flagged: BTreeSet<&str> = self
            .meetings
            .iter()
            .filter_map(|m| m.keep_local_folder.as_deref())
            .collect();
        for m in &self.meetings {
            let under = m.folder.as_deref().is_some_and(|mf| {
                flagged
                    .iter()
                    .any(|f| mf == *f || mf.starts_with(&format!("{f}/")))
            });
            ensure!(
                !under || m.keep_local_folder.is_some() || m.keep_local,
                "{} sits in a Keep-local folder; mark it (keep_local_folder) so its canaries are checked",
                m.id
            );
        }
        ensure!(
            self.meetings.iter().any(|m| m.keep_local),
            "the corpus needs a keep-local meeting"
        );
        // Titles are leak needles too, so a keep-local title must be unique.
        for m in self.meetings.iter().filter(|m| self.is_keep_local(m)) {
            ensure!(
                self.meetings
                    .iter()
                    .filter(|o| o.title.eq_ignore_ascii_case(&m.title))
                    .count()
                    == 1,
                "keep-local title {:?} is not unique",
                m.title
            );
        }
        // Canaries must be unique to their meeting, or a leak check would
        // fire on shareable text.
        for m in self.meetings.iter().filter(|m| self.is_keep_local(m)) {
            for canary in &m.canaries {
                let needle = canary.to_lowercase();
                ensure!(
                    self.meeting_text(m).contains(&needle),
                    "canary {canary:?} doesn't occur in {}",
                    m.id
                );
                for other in self.meetings.iter().filter(|o| !self.is_keep_local(o)) {
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
        for b in &m.enhanced {
            s.push_str(&b.markdown);
            s.push('\n');
        }
        for t in m.cleaned.values() {
            s.push_str(t);
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

    /// Keep local, by its note's flag or a flagged folder.
    pub fn is_keep_local(&self, m: &MeetingFixture) -> bool {
        m.keep_local || m.keep_local_folder.is_some()
    }

    pub fn keep_local_meetings(&self) -> impl Iterator<Item = &MeetingFixture> {
        self.meetings.iter().filter(|m| self.is_keep_local(m))
    }

    pub fn is_keep_local_meeting(&self, id: &str) -> bool {
        self.meeting(id).is_some_and(|m| self.is_keep_local(m))
    }

    /// Lowercased canary phrases of every keep-local meeting.
    pub fn canaries(&self) -> Vec<String> {
        self.keep_local_meetings()
            .flat_map(|m| m.canaries.iter().map(|c| c.to_lowercase()))
            .collect()
    }

    pub fn is_keep_local_segment(&self, id: &str) -> bool {
        self.segment(id)
            .is_some_and(|s| self.is_keep_local_meeting(&s.meeting))
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

/// A UUID-shaped id (v4 layout) derived from `kind` and `key`: the same in
/// every build, so replays and fingerprints are reproducible.
pub fn uuid_for(kind: &str, key: &str) -> String {
    let d = Sha256::digest(format!("fly-evals:{kind}:{key}").as_bytes());
    let mut b = [0u8; 16];
    b.copy_from_slice(&d[..16]);
    b[6] = (b[6] & 0x0f) | 0x40;
    b[8] = (b[8] & 0x3f) | 0x80;
    let h: String = b.iter().map(|x| format!("{x:02x}")).collect();
    format!(
        "{}-{}-{}-{}-{}",
        &h[..8],
        &h[8..12],
        &h[12..16],
        &h[16..20],
        &h[20..]
    )
}

/// The settings an Ask must never write (consent and provider config).
const WATCHED_SETTINGS: &str = "SELECT key, value FROM settings \
     WHERE key LIKE 'consent.%' OR key LIKE 'llm.%' ORDER BY key";

/// A fixture database in a temp dir, plus fixture-id ↔ database-id maps.
pub struct Db {
    _dir: tempfile::TempDir,
    pub storage: Storage,
    /// fixture meeting id → meeting id
    pub meeting_ids: BTreeMap<String, String>,
    /// fixture meeting id → note id
    pub note_ids: BTreeMap<String, String>,
    /// folder path → folder id
    pub folder_ids: BTreeMap<String, String>,
    /// fixture segment id → segment id
    pub seg_ids: BTreeMap<String, String>,
    /// every database id this build made → its placeholder or fixture id
    reverse: HashMap<String, String>,
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
        let flagged: BTreeSet<&str> = corpus
            .meetings
            .iter()
            .filter_map(|m| m.keep_local_folder.as_deref())
            .collect();
        for f in flagged {
            storage.set_folder_keep_local(&folder_ids[f], true)?;
        }
        // The provider settings an Ask runs under; consent rows are absent,
        // so a harness writing one shows up as a change.
        storage.set_setting("llm.provider", "ollama")?;
        storage.set_setting("llm.ollama.model", "llama3.1")?;
        let mut seg_ids: BTreeMap<String, String> = BTreeMap::new();
        for s in corpus.segments() {
            seg_ids.insert(s.id.clone(), uuid_for("segment", &s.id));
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
                        id: seg_ids[&s.id].clone(),
                        speaker_key: s.speaker_key.clone(),
                        start_ms: s.start_ms,
                        end_ms: s.end_ms,
                        text: s.text.clone(),
                        words: vec![],
                    }
                })
                .collect();
            let raw = Transcript {
                meeting_id: meeting.id.clone(),
                language: Some("en".into()),
                engine: "fixture".into(),
                segments,
                speakers,
            };
            storage.save_transcript(&raw)?;
            if !m.cleaned.is_empty() {
                let mut cleaned = raw.clone();
                for seg in &mut cleaned.segments {
                    let fixture = &m
                        .segments
                        .iter()
                        .find(|r| seg_ids[&r.0] == seg.id)
                        .expect("own segment")
                        .0;
                    if let Some(t) = m.cleaned.get(fixture) {
                        seg.text = t.clone();
                    }
                }
                storage.save_cleaned_transcript(&cleaned)?;
            }
            if !m.enhanced.is_empty() {
                let blocks: Vec<NoteBlock> = m
                    .enhanced
                    .iter()
                    .enumerate()
                    .map(|(i, b)| NoteBlock {
                        id: uuid_for("block", &format!("{}#{i}", m.id)),
                        origin: if b.sources.is_empty() {
                            BlockOrigin::User
                        } else {
                            BlockOrigin::Ai {
                                source_segment_ids: b
                                    .sources
                                    .iter()
                                    .map(|s| seg_ids[s].clone())
                                    .collect(),
                            }
                        },
                        markdown: b.markdown.clone(),
                    })
                    .collect();
                storage.update_note_blocks(&note.id, &blocks)?;
            }
            let items: Vec<MeetingItem> = m
                .items
                .iter()
                .enumerate()
                .map(|(n, i)| {
                    let kind = ItemKind::parse(&i.kind).expect("validated");
                    MeetingItem {
                        id: uuid_for("item", &format!("{}#{n}", m.id)),
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
                        segment_ids: i.segments.iter().map(|s| seg_ids[s].clone()).collect(),
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
        let mut reverse = HashMap::new();
        for (k, v) in &meeting_ids {
            reverse.insert(v.clone(), format!("{{meeting:{k}}}"));
        }
        for (k, v) in &note_ids {
            reverse.insert(v.clone(), format!("{{note:{k}}}"));
        }
        for (k, v) in &folder_ids {
            reverse.insert(v.clone(), format!("{{folder:{k}}}"));
        }
        for (k, v) in &seg_ids {
            reverse.insert(v.clone(), k.clone());
        }
        for m in &corpus.meetings {
            for n in 0..m.items.len() {
                let key = format!("{}#{n}", m.id);
                reverse.insert(uuid_for("item", &key), format!("{{item:{key}}}"));
            }
            for n in 0..m.enhanced.len() {
                let key = format!("{}#{n}", m.id);
                reverse.insert(uuid_for("block", &key), format!("{{block:{key}}}"));
            }
        }
        Ok(Db {
            _dir: dir,
            storage,
            meeting_ids,
            note_ids,
            folder_ids,
            seg_ids,
            reverse,
        })
    }

    /// The fixture segment id of a database segment id.
    pub fn fixture_seg(&self, id: &str) -> Option<&str> {
        self.reverse
            .get(id)
            .map(String::as_str)
            .filter(|v| !v.starts_with('{'))
    }

    /// Every database id in `text` (UUID-shaped) replaced by its fixture id
    /// or placeholder, so text from two builds compares equal.
    pub fn normalize_ids(&self, text: &str) -> String {
        let b = text.as_bytes();
        let mut out = String::with_capacity(text.len());
        let mut i = 0;
        let mut last = 0;
        while i + 36 <= b.len() {
            let cand = &text[i..i + 36];
            if is_uuid(cand)
                && (i == 0 || !b[i - 1].is_ascii_alphanumeric())
                && b.get(i + 36).is_none_or(|c| !c.is_ascii_alphanumeric())
            {
                if let Some(r) = self.reverse.get(cand) {
                    out.push_str(&text[last..i]);
                    out.push_str(r);
                    i += 36;
                    last = i;
                    continue;
                }
            }
            i += 1;
            while i < b.len() && !text.is_char_boundary(i) {
                i += 1;
            }
        }
        out.push_str(&text[last..]);
        out
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
                Some(("seg", id)) => self.seg_ids.get(id),
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
        Ok(self.map_fixture_seg_ids(&out))
    }

    /// Fixture segment ids written bare (`m13-045`, as in `[seg:m13-045]`)
    /// replaced by their database ids.
    pub fn map_fixture_seg_ids(&self, s: &str) -> String {
        let b = s.as_bytes();
        let mut out = String::with_capacity(s.len());
        let mut last = 0;
        let mut i = 0;
        while i < b.len() {
            let start_ok = i == 0 || !b[i - 1].is_ascii_alphanumeric();
            if start_ok && b[i] == b'm' {
                let mut j = i + 1;
                while j < b.len() && (b[j].is_ascii_digit() || b[j] == b'-') {
                    j += 1;
                }
                if j > i + 1 && b.get(j).is_none_or(|c| !c.is_ascii_alphanumeric()) {
                    if let Some(id) = self.seg_ids.get(&s[i..j]) {
                        out.push_str(&s[last..i]);
                        out.push_str(id);
                        last = j;
                        i = j;
                        continue;
                    }
                }
            }
            i += 1;
        }
        out.push_str(&s[last..]);
        out
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
        let mut local_folders = self.storage.local_only_folders()?;
        local_folders.sort();
        parts.push(serde_json::json!({
            "folders": folders,
            "keep_local_folders": local_folders,
            "meetings": meetings,
            "notes": notes,
            "settings": self.watched_settings()?,
        }));
        Ok(serde_json::to_string(&parts)?)
    }

    /// `consent.*` and `llm.*` settings, read through a separate read-only
    /// connection (fly-storage can't list settings).
    fn watched_settings(&self) -> Result<Vec<(String, String)>> {
        let path = self._dir.path().join("flyonthewall.db");
        let conn = rusqlite::Connection::open_with_flags(
            &path,
            rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY | rusqlite::OpenFlags::SQLITE_OPEN_NO_MUTEX,
        )
        .with_context(|| format!("open {} read-only", path.display()))?;
        let mut stmt = conn.prepare(WATCHED_SETTINGS)?;
        let rows = stmt
            .query_map([], |r| Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?)))?
            .collect::<std::result::Result<Vec<_>, _>>()?;
        Ok(rows)
    }
}

fn is_uuid(s: &str) -> bool {
    s.len() == 36
        && s.bytes().enumerate().all(|(i, c)| match i {
            8 | 13 | 18 | 23 => c == b'-',
            _ => c.is_ascii_hexdigit(),
        })
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
        assert_eq!(t.segments[0].id, db.seg_ids["m05-01"]);
        assert_eq!(db.fixture_seg(&t.segments[0].id), Some("m05-01"));
        assert!(is_uuid(&t.segments[0].id));
        // Bare fixture ids and {seg:} placeholders map to database ids,
        // and normalize_ids maps them back.
        let sub = db
            .substitute_str("[seg:m05-01] {seg:m05-02} xm05-01 m05-999")
            .unwrap();
        assert_eq!(
            sub,
            format!(
                "[seg:{}] {} xm05-01 m05-999",
                db.seg_ids["m05-01"], db.seg_ids["m05-02"]
            )
        );
        assert_eq!(
            db.normalize_ids(&format!("{sub} {}", db.meeting_ids["m05"])),
            "[seg:m05-01] m05-02 xm05-01 m05-999 {meeting:m05}"
        );
        // A settings write shows up in the snapshot.
        let before = db.snapshot().unwrap();
        db.storage
            .set_setting("consent.cloud_llm.anthropic", "true")
            .unwrap();
        assert_ne!(before, db.snapshot().unwrap());
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
