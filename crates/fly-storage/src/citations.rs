//! Citation locators (schema v3, spec §4.2a #6).
//!
//! AI note blocks and extracted items cite transcript segments by id. A new
//! transcription run generates new ids (`align.rs` `new_id()`), so without
//! help every citation silently dangles. A locator remembers, per (citing
//! thing, segment), where the segment was (`start_ms`/`end_ms`), who said it
//! (`speaker_key`) and what it said (`text_hash`), so that:
//!
//! * a re-transcribe can **re-anchor** each citation to the new segment that
//!   best matches it, and rewrite the id in the block / item so the
//!   existing "zoom in" keeps working ([`Storage::save_transcript`]);
//! * a reader can tell when the cited segment no longer says what it said
//!   (an edit, a re-transcribe that heard something else, a re-attributed
//!   speaker) and show "source changed — check".
//!
//! **The cited hash is never overwritten.** It's taken once, from the RAW
//! segment text (the source of truth; a re-polish can't move it) when the
//! citation is first recorded. Re-anchoring moves the pointer to the best
//! match but keeps the hash, and calls the result `reanchored` only when
//! the new segment says exactly the same thing (normalized) with the same
//! speaker key — otherwise `changed`, still pointing at the best match so
//! zoom-in lands in the right place. "We will ship" → "we will NOT ship" is
//! `changed`, not `reanchored`.
//!
//! Locators are written automatically by the storage write paths for note
//! blocks (`update_note_blocks`) and items (`replace_meeting_items`), and
//! after a recording self-heal; Ask inserts can use
//! [`Storage::record_citations`]. A cited id no transcript of the owner's
//! meetings has (a model citing ids a re-transcribe replaced while it ran)
//! gets a `changed` placeholder, so nothing reads as fine by omission.
//! Readers must still treat a cited id with no locator as changed.
//!
//! **No visibility filter.** [`Storage::citation_locators`] answers for any
//! owner it's asked about; callers that serve an AI reader (Ask, MCP, a
//! future `segments_by_ids`) must gate on the owner's keep-local status
//! first, like every other by-id read.
//!
//! An older build that opens a v3 file never writes locators. The
//! `transcripts` triggers (segments.rs) still mark citations whose segment
//! disappeared as `changed` and drop a deleted meeting's locators, and the
//! v3 step re-syncs everything when a newer build opens the file again.

use std::collections::{HashMap, HashSet};

use fly_core::{BlockOrigin, Note, Transcript, TranscriptSegment};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::{Result, Storage};

/// Locator owner kinds.
pub const OWNER_NOTE_BLOCK: &str = "note_block";
pub const OWNER_ITEM: &str = "item";
/// Reserved for Ask answers inserted into a note (v2.5 Ask).
pub const OWNER_ASK_INSERT: &str = "ask_insert";

/// Time padding around a lost segment's span when looking for its
/// replacement in a new transcript.
const REANCHOR_PAD_MS: u64 = 2_000;
/// Match score = 0.45 × time IoU + 0.45 × token Jaccard + 0.10 × same
/// speaker key; a lost segment re-points only at or above this.
const REANCHOR_MIN_SCORE: f64 = 0.6;

pub(crate) const SCHEMA_SQL: &str = r#"
    CREATE TABLE IF NOT EXISTS citation_locators (
        owner_kind  TEXT NOT NULL,  -- 'note_block' | 'item' | 'ask_insert'
        owner_id    TEXT NOT NULL,  -- NoteBlock.id / MeetingItem.id / insert id
        note_id     TEXT,           -- note holding the block (note_block, ask_insert)
        meeting_id  TEXT NOT NULL,
        seg_id      TEXT NOT NULL,  -- current pointer (re-anchoring moves it)
        start_ms    INTEGER NOT NULL,
        end_ms      INTEGER NOT NULL,
        text_hash   TEXT NOT NULL,  -- text_hash() of the RAW text when cited; '' = unresolved
        speaker_key TEXT,           -- speaker key when cited
        status      TEXT NOT NULL DEFAULT 'ok',  -- 'ok' | 'reanchored' | 'changed'
        PRIMARY KEY (owner_kind, owner_id, seg_id)
    );
    CREATE INDEX IF NOT EXISTS idx_citation_locators_meeting
        ON citation_locators(meeting_id, seg_id);
    CREATE INDEX IF NOT EXISTS idx_citation_locators_note
        ON citation_locators(note_id);
"#;

/// Whether a citation still points at what was cited.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CitationStatus {
    /// The segment exists and says what it said when cited, by the same
    /// speaker.
    Ok,
    /// A re-transcribe replaced the segment with one that says exactly the
    /// same thing (normalized), by the same speaker; the citation points at
    /// it now.
    Reanchored,
    /// The segment is gone, or what it says (or who says it) differs from
    /// what was cited. If a re-transcribe found a likely replacement, the
    /// citation points at it — check before trusting.
    Changed,
}

impl CitationStatus {
    fn parse(s: &str) -> Self {
        match s {
            "ok" => Self::Ok,
            "reanchored" => Self::Reanchored,
            _ => Self::Changed,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CitationLocator {
    pub owner_kind: String,
    pub owner_id: String,
    pub meeting_id: String,
    pub seg_id: String,
    pub start_ms: u64,
    pub end_ms: u64,
    /// Hash of the raw text when cited (`""` for an id that never resolved).
    pub text_hash: String,
    /// Speaker key when cited.
    pub speaker_key: Option<String>,
    /// Computed when read: stored state, plus a check of the segment's
    /// current text and speaker against what was cited.
    pub status: CitationStatus,
}

/// Hex SHA-256 (first 16 chars) of the text's words: lowercased
/// alphanumeric runs joined by one space. Casing, spacing and punctuation
/// (which every decode and polish varies) never read as a change; any
/// different, added or missing word does ("not", "fourteen", "4").
pub fn text_hash(text: &str) -> String {
    let norm = text
        .split(|c: char| !c.is_alphanumeric())
        .filter(|w| !w.is_empty())
        .collect::<Vec<_>>()
        .join(" ")
        .to_lowercase();
    let digest = Sha256::digest(norm.as_bytes());
    digest.iter().take(8).map(|b| format!("{b:02x}")).collect()
}

fn tokens(text: &str) -> HashSet<String> {
    text.split(|c: char| !c.is_alphanumeric())
        .filter(|w| !w.is_empty())
        .map(str::to_lowercase)
        .collect()
}

fn jaccard(a: &HashSet<String>, b: &HashSet<String>) -> f64 {
    if a.is_empty() && b.is_empty() {
        return 0.0;
    }
    let inter = a.intersection(b).count() as f64;
    let union = a.union(b).count() as f64;
    inter / union
}

/// Intersection over union of two time spans (a zero-length span counts as
/// 1 ms so identical instants still match).
fn time_iou(a: (u64, u64), b: (u64, u64)) -> f64 {
    let (a0, a1) = (a.0, a.1.max(a.0 + 1));
    let (b0, b1) = (b.0, b.1.max(b.0 + 1));
    let inter = a1.min(b1).saturating_sub(a0.max(b0)) as f64;
    let union = (a1.max(b1) - a0.min(b0)) as f64;
    inter / union
}

/// A cited segment that the incoming transcript no longer has.
struct Lost {
    seg_id: String,
    span: (u64, u64),
    words: HashSet<String>,
    speaker_key: Option<String>,
}

/// Best new segment for each lost one: candidates overlap the lost span
/// padded by [`REANCHOR_PAD_MS`], scored as documented on
/// [`REANCHOR_MIN_SCORE`]. Matching only decides where the pointer goes;
/// whether the citation still holds is decided by the hash (see
/// [`Storage::apply_reanchor_db`]).
fn match_lost(lost: &[Lost], new: &Transcript) -> Vec<(String, String)> {
    let mut out = Vec::new();
    for l in lost {
        let lo = l.span.0.saturating_sub(REANCHOR_PAD_MS);
        let hi = l.span.1 + REANCHOR_PAD_MS;
        let best = new
            .segments
            .iter()
            .filter(|s| s.start_ms < hi && s.end_ms > lo)
            .map(|s| {
                let same_speaker = l.speaker_key.as_deref() == Some(s.speaker_key.as_str());
                let score = 0.45 * time_iou(l.span, (s.start_ms, s.end_ms))
                    + 0.45 * jaccard(&l.words, &tokens(&s.text))
                    + if same_speaker { 0.10 } else { 0.0 };
                (score, s)
            })
            .max_by(|a, b| a.0.total_cmp(&b.0));
        if let Some((score, s)) = best {
            if score >= REANCHOR_MIN_SCORE {
                out.push((l.seg_id.clone(), s.id.clone()));
            }
        }
    }
    out
}

/// Share of a speaker cluster's duration that must land on ONE counterpart,
/// in both directions, for two keys to count as the same speaker — the rule
/// `fly_core::rediarize::rebuild_speakers` uses to carry a label.
const STABLE_SHARE: f64 = 0.8;

/// Old speaker key → new key for clusters that are the same speaker under a
/// new number. Keys are numbered by first appearance, so a re-diarize or a
/// re-transcribe routinely renumbers a stable speaker (Karim spk_2 →
/// spk_1); a citation must follow that, while a line that genuinely moved
/// to another cluster still reads `changed`. Weights are segment durations
/// matched by id when the transcripts share ids (re-diarize), else by time
/// overlap (re-transcribe). Only stable pairs are listed (identity
/// included); a cluster that split or merged maps to nothing.
pub(crate) fn stable_key_map(old: &Transcript, new: &Transcript) -> HashMap<String, String> {
    let mut weights: HashMap<(String, String), u64> = HashMap::new();
    let old_by_id: HashMap<&str, &TranscriptSegment> =
        old.segments.iter().map(|s| (s.id.as_str(), s)).collect();
    let shared = new
        .segments
        .iter()
        .any(|s| old_by_id.contains_key(s.id.as_str()));
    if shared {
        for n in &new.segments {
            if let Some(o) = old_by_id.get(n.id.as_str()) {
                let dur = n.end_ms.saturating_sub(n.start_ms).max(1);
                *weights
                    .entry((o.speaker_key.clone(), n.speaker_key.clone()))
                    .or_default() += dur;
            }
        }
    } else {
        let mut olds: Vec<&TranscriptSegment> = old.segments.iter().collect();
        let mut news: Vec<&TranscriptSegment> = new.segments.iter().collect();
        olds.sort_by_key(|s| s.start_ms);
        news.sort_by_key(|s| s.start_ms);
        let mut j0 = 0;
        for o in &olds {
            while j0 < news.len() && news[j0].end_ms <= o.start_ms {
                j0 += 1;
            }
            for n in news[j0..].iter().take_while(|n| n.start_ms < o.end_ms) {
                let overlap = o
                    .end_ms
                    .min(n.end_ms)
                    .saturating_sub(o.start_ms.max(n.start_ms));
                if overlap > 0 {
                    *weights
                        .entry((o.speaker_key.clone(), n.speaker_key.clone()))
                        .or_default() += overlap;
                }
            }
        }
    }
    // Each key's dominant counterpart, kept when it holds ≥ STABLE_SHARE of
    // the key's duration; `reverse` reads the pairs new → old.
    let dominant = |reverse: bool| {
        let mut totals: HashMap<&String, u64> = HashMap::new();
        let mut best: HashMap<&String, (&String, u64)> = HashMap::new();
        for ((old_key, new_key), w) in &weights {
            let (from, to) = if reverse {
                (new_key, old_key)
            } else {
                (old_key, new_key)
            };
            *totals.entry(from).or_default() += w;
            let e = best.entry(from).or_insert((to, 0));
            if *w > e.1 || (*w == e.1 && to < e.0) {
                *e = (to, *w);
            }
        }
        best.into_iter()
            .filter(|(from, (_, w))| *w as f64 >= STABLE_SHARE * totals[from] as f64)
            .map(|(from, (to, _))| (from.clone(), to.clone()))
            .collect::<HashMap<String, String>>()
    };
    let forward = dominant(false);
    let backward = dominant(true);
    forward
        .into_iter()
        .filter(|(o, n)| backward.get(n) == Some(o))
        .collect()
}

/// What `save_transcript` does to citations when a transcript is replaced.
#[derive(Default)]
pub(crate) struct ReanchorPlan {
    /// Renumbered speakers: cited key → its key in the new transcript.
    key_map: HashMap<String, String>,
    /// Lost cited segment id → best new segment id.
    pairs: Vec<(String, String)>,
}

impl ReanchorPlan {
    pub(crate) fn is_empty(&self) -> bool {
        self.key_map.is_empty() && self.pairs.is_empty()
    }
}

fn map_ids(ids: &[String], map: &HashMap<String, String>) -> (Vec<String>, bool) {
    let mut changed = false;
    let mut out: Vec<String> = Vec::with_capacity(ids.len());
    for id in ids {
        let to = map.get(id).cloned().unwrap_or_else(|| id.clone());
        changed |= &to != id;
        if !out.contains(&to) {
            out.push(to);
        }
    }
    (out, changed)
}

/// Raw transcripts read once per call; an unreadable one reads as absent
/// (logged) so a damaged row never fails a note or item save.
#[derive(Default)]
struct RawCache(HashMap<String, Option<Transcript>>);

impl RawCache {
    fn get<'a>(&'a mut self, s: &Storage, meeting_id: &str) -> Option<&'a Transcript> {
        self.0
            .entry(meeting_id.to_string())
            .or_insert_with(|| match s.get_transcript(meeting_id) {
                Ok(t) => t,
                Err(e) => {
                    tracing::warn!(meeting_id, error = %e, "citations: unreadable transcript");
                    None
                }
            })
            .as_ref()
    }

    fn segment<'a>(
        &'a mut self,
        s: &Storage,
        meeting_id: &str,
        seg_id: &str,
    ) -> Option<&'a TranscriptSegment> {
        self.get(s, meeting_id)?
            .segments
            .iter()
            .find(|x| x.id == seg_id)
    }
}

/// A row about to be re-pointed.
struct LocRow {
    kind: String,
    owner: String,
    hash: String,
    speaker_key: Option<String>,
}

/// Note whose block provenance a re-anchor rewrote: its on-disk mirrors
/// need a refresh (done after the database work, best-effort).
pub(crate) struct ReanchoredNote(pub(crate) Note);

impl Storage {
    /// Locators for one citing thing, with live status. No visibility
    /// filter — see the module docs.
    pub fn citation_locators(
        &self,
        owner_kind: &str,
        owner_id: &str,
    ) -> Result<Vec<CitationLocator>> {
        let mut stmt = self.conn.prepare(
            "SELECT meeting_id, seg_id, start_ms, end_ms, text_hash, speaker_key, status
               FROM citation_locators WHERE owner_kind = ?1 AND owner_id = ?2
              ORDER BY start_ms, seg_id",
        )?;
        type Row = (String, String, i64, i64, String, Option<String>, String);
        let rows: Vec<Row> = stmt
            .query_map((owner_kind, owner_id), |r| {
                Ok((
                    r.get(0)?,
                    r.get(1)?,
                    r.get(2)?,
                    r.get(3)?,
                    r.get(4)?,
                    r.get(5)?,
                    r.get(6)?,
                ))
            })?
            .collect::<std::result::Result<_, _>>()?;
        let mut raw = RawCache::default();
        let mut out = Vec::with_capacity(rows.len());
        for (meeting_id, seg_id, start, end, hash, speaker_key, stored) in rows {
            let mut status = CitationStatus::parse(&stored);
            if status != CitationStatus::Changed {
                let same_speaker = |seg: &TranscriptSegment| {
                    speaker_key.as_deref().is_none_or(|k| k == seg.speaker_key)
                };
                let holds = match raw.segment(self, &meeting_id, &seg_id) {
                    None => false,
                    Some(seg) if !same_speaker(seg) => false,
                    Some(seg) if text_hash(&seg.text) == hash => true,
                    // Rows recorded before raw-text hashing (no speaker key
                    // either) hashed the indexed (polished) text.
                    Some(_) if speaker_key.is_none() => self
                        .indexed_segment(&meeting_id, &seg_id)?
                        .is_some_and(|seg| text_hash(&seg.text) == hash),
                    Some(_) => false,
                };
                if !holds {
                    status = CitationStatus::Changed;
                }
            }
            out.push(CitationLocator {
                owner_kind: owner_kind.to_string(),
                owner_id: owner_id.to_string(),
                meeting_id,
                seg_id,
                start_ms: start.max(0) as u64,
                end_ms: end.max(0) as u64,
                text_hash: hash,
                speaker_key,
                status,
            });
        }
        Ok(out)
    }

    /// Make one citing thing's locators match `seg_ids`: pairs it no longer
    /// cites are dropped; new ones are recorded from the current raw
    /// transcript (each id resolved in the first of `meeting_ids` whose
    /// transcript has it). An id none of them has gets a `changed`
    /// placeholder in the first meeting (upgraded to a real locator if it
    /// resolves on a later sync); with no meeting at all it's skipped.
    /// Pairs already recorded are kept as they are, so their hash still
    /// describes the cited text. Returns how many locators the owner has.
    pub fn record_citations(
        &self,
        owner_kind: &str,
        owner_id: &str,
        note_id: Option<&str>,
        meeting_ids: &[String],
        seg_ids: &[String],
    ) -> Result<usize> {
        self.record_citations_with(
            &mut RawCache::default(),
            owner_kind,
            owner_id,
            note_id,
            meeting_ids,
            seg_ids,
        )
    }

    fn record_citations_with(
        &self,
        raw: &mut RawCache,
        owner_kind: &str,
        owner_id: &str,
        note_id: Option<&str>,
        meeting_ids: &[String],
        seg_ids: &[String],
    ) -> Result<usize> {
        // seg_id → resolved (a real hash, not a placeholder)
        let existing: HashMap<String, bool> = self
            .conn
            .prepare(
                "SELECT seg_id, text_hash != '' FROM citation_locators
                  WHERE owner_kind = ?1 AND owner_id = ?2",
            )?
            .query_map((owner_kind, owner_id), |r| Ok((r.get(0)?, r.get(1)?)))?
            .collect::<std::result::Result<_, _>>()?;
        let wanted: HashSet<&String> = seg_ids.iter().collect();
        for gone in existing.keys().filter(|s| !wanted.contains(s)) {
            self.conn.execute(
                "DELETE FROM citation_locators
                  WHERE owner_kind = ?1 AND owner_id = ?2 AND seg_id = ?3",
                (owner_kind, owner_id, gone),
            )?;
        }
        for seg_id in seg_ids {
            let had = existing.get(seg_id).copied();
            if had == Some(true) {
                continue;
            }
            let found = meeting_ids.iter().find_map(|mid| {
                raw.segment(self, mid, seg_id)
                    .map(|s| (mid.clone(), s.clone()))
            });
            match found {
                Some((meeting_id, seg)) => {
                    self.conn.execute(
                        "INSERT OR REPLACE INTO citation_locators
                         (owner_kind, owner_id, note_id, meeting_id, seg_id,
                          start_ms, end_ms, text_hash, speaker_key, status)
                         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, 'ok')",
                        rusqlite::params![
                            owner_kind,
                            owner_id,
                            note_id,
                            meeting_id,
                            seg_id,
                            seg.start_ms as i64,
                            seg.end_ms.max(seg.start_ms) as i64,
                            text_hash(&seg.text),
                            seg.speaker_key
                        ],
                    )?;
                }
                None if had.is_none() => {
                    if let Some(meeting_id) = meeting_ids.first() {
                        self.conn.execute(
                            "INSERT OR IGNORE INTO citation_locators
                             (owner_kind, owner_id, note_id, meeting_id, seg_id,
                              start_ms, end_ms, text_hash, speaker_key, status)
                             VALUES (?1, ?2, ?3, ?4, ?5, 0, 0, '', NULL, 'changed')",
                            (owner_kind, owner_id, note_id, meeting_id, seg_id),
                        )?;
                    }
                }
                None => {} // placeholder already there
            }
        }
        let n: i64 = self.conn.query_row(
            "SELECT COUNT(*) FROM citation_locators WHERE owner_kind = ?1 AND owner_id = ?2",
            (owner_kind, owner_id),
            |r| r.get(0),
        )?;
        Ok(n as usize)
    }

    /// Re-sync the locators of a note's AI blocks (blocks whose provenance
    /// lists source segments). Blocks that were removed, or edited into user
    /// text, lose theirs.
    pub(crate) fn sync_note_citations(&self, note: &Note) -> Result<()> {
        let meeting_ids: Vec<String> = self
            .meetings_for_note(&note.id)?
            .into_iter()
            .map(|m| m.id)
            .collect();
        let mut raw = RawCache::default();
        let mut cited: HashSet<&str> = HashSet::new();
        for block in &note.blocks {
            if let BlockOrigin::Ai { source_segment_ids } = &block.origin {
                if source_segment_ids.is_empty() {
                    continue;
                }
                cited.insert(&block.id);
                self.record_citations_with(
                    &mut raw,
                    OWNER_NOTE_BLOCK,
                    &block.id,
                    Some(&note.id),
                    &meeting_ids,
                    source_segment_ids,
                )?;
            }
        }
        let owners: Vec<String> = self
            .conn
            .prepare(
                "SELECT DISTINCT owner_id FROM citation_locators
                  WHERE owner_kind = ?1 AND note_id = ?2",
            )?
            .query_map((OWNER_NOTE_BLOCK, &note.id), |r| r.get(0))?
            .collect::<std::result::Result<_, _>>()?;
        for owner in owners.iter().filter(|o| !cited.contains(o.as_str())) {
            self.conn.execute(
                "DELETE FROM citation_locators WHERE owner_kind = ?1 AND owner_id = ?2",
                (OWNER_NOTE_BLOCK, owner),
            )?;
        }
        Ok(())
    }

    /// Re-sync the locators of a meeting's extracted items.
    pub(crate) fn sync_item_citations(&self, meeting_id: &str) -> Result<()> {
        let items = self.get_meeting_items(meeting_id)?;
        let scope = vec![meeting_id.to_string()];
        let mut raw = RawCache::default();
        let mut cited: HashSet<&str> = HashSet::new();
        for item in &items {
            cited.insert(&item.id);
            self.record_citations_with(
                &mut raw,
                OWNER_ITEM,
                &item.id,
                None,
                &scope,
                &item.segment_ids,
            )?;
        }
        let owners: Vec<String> = self
            .conn
            .prepare(
                "SELECT DISTINCT owner_id FROM citation_locators
                  WHERE owner_kind = ?1 AND meeting_id = ?2",
            )?
            .query_map((OWNER_ITEM, meeting_id), |r| r.get(0))?
            .collect::<std::result::Result<_, _>>()?;
        for owner in owners.iter().filter(|o| !cited.contains(o.as_str())) {
            self.conn.execute(
                "DELETE FROM citation_locators WHERE owner_kind = ?1 AND owner_id = ?2",
                (OWNER_ITEM, owner),
            )?;
        }
        Ok(())
    }

    /// Re-sync the citations of the meetings a recording self-heal restored
    /// (it writes notes and transcripts without going through the normal
    /// write paths). Best-effort: logged, never fatal.
    pub(crate) fn sync_citations_for_meetings(&self, meeting_ids: &[String]) {
        for id in meeting_ids {
            let res = (|| -> Result<()> {
                let meeting = self.get_meeting(id)?;
                if let Ok(note) = self.get_note(&meeting.note_id) {
                    self.sync_note_citations(&note)?;
                }
                self.sync_item_citations(id)
            })();
            if let Err(e) = res {
                tracing::warn!(meeting_id = %id, error = %e, "citations: re-sync after heal failed");
            }
        }
    }

    /// Before `new` replaces a meeting's transcript (old transcript still
    /// readable): work out which cited speakers were merely renumbered, and
    /// for every cited segment the new transcript no longer has, the new
    /// segment that best matches it. Cited segments without a match aren't
    /// listed (they stay `changed`). Skips reading the old transcript when
    /// every cited segment survives and no speaker key changed.
    pub(crate) fn plan_reanchor(&self, new: &Transcript) -> Result<ReanchorPlan> {
        let cited: Vec<(String, i64, i64, Option<String>)> = self
            .conn
            .prepare(
                "SELECT seg_id, MIN(start_ms), MAX(end_ms), MAX(speaker_key)
                   FROM citation_locators
                  WHERE meeting_id = ?1 AND text_hash != '' GROUP BY seg_id",
            )?
            .query_map([&new.meeting_id], |r| {
                Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?))
            })?
            .collect::<std::result::Result<_, _>>()?;
        if cited.is_empty() {
            return Ok(ReanchorPlan::default());
        }
        let new_keys: HashMap<&str, &str> = new
            .segments
            .iter()
            .map(|s| (s.id.as_str(), s.speaker_key.as_str()))
            .collect();
        // Fast path: every cited segment survives and no segment of the
        // meeting changed speaker key (read from the index, which still
        // holds the OLD transcript: no JSON parse). Any key change at all
        // may be a renumbering that moves what a cited key means.
        let dangling = cited
            .iter()
            .any(|(id, ..)| !new_keys.contains_key(id.as_str()));
        if !dangling {
            let old_keys: HashMap<String, String> = self
                .conn
                .prepare(
                    "SELECT segment_id, speaker_key FROM transcript_segments
                      WHERE meeting_id = ?1",
                )?
                .query_map([&new.meeting_id], |r| {
                    Ok((
                        r.get::<_, Option<String>>(0)?.unwrap_or_default(),
                        r.get::<_, Option<String>>(1)?.unwrap_or_default(),
                    ))
                })?
                .collect::<std::result::Result<_, _>>()?;
            let keys_same = old_keys.len() == new.segments.len()
                && new
                    .segments
                    .iter()
                    .all(|s| old_keys.get(&s.id).is_some_and(|k| k == &s.speaker_key));
            if keys_same {
                return Ok(ReanchorPlan::default());
            }
        }
        let old = self.get_transcript(&new.meeting_id).ok().flatten();
        let key_map: HashMap<String, String> = old
            .as_ref()
            .map(|o| stable_key_map(o, new))
            .unwrap_or_default()
            .into_iter()
            .filter(|(o, n)| o != n)
            .collect();
        let lost: Vec<Lost> = cited
            .into_iter()
            .filter(|(id, _, _, _)| !new_keys.contains_key(id.as_str()))
            .map(|(seg_id, start, end, speaker_key)| {
                let old_seg = old
                    .as_ref()
                    .and_then(|t| t.segments.iter().find(|s| s.id == seg_id));
                let (span, words, key) = match old_seg {
                    Some(s) => (
                        (s.start_ms, s.end_ms),
                        tokens(&s.text),
                        Some(s.speaker_key.clone()),
                    ),
                    // Unknown text: time + speaker alone stay below the
                    // threshold, so such a citation is left `changed`.
                    None => (
                        (start.max(0) as u64, end.max(0) as u64),
                        HashSet::new(),
                        speaker_key,
                    ),
                };
                Lost {
                    span,
                    words,
                    speaker_key: key.map(|k| key_map.get(&k).cloned().unwrap_or(k)),
                    seg_id,
                }
            })
            .collect();
        let pairs = match_lost(&lost, new);
        Ok(ReanchorPlan { key_map, pairs })
    }

    /// After the new transcript is saved, database side only: point the
    /// re-anchored locators at their new segments (status `reanchored` only
    /// when the new raw text hashes to the cited hash and the speaker key is
    /// the cited one; else `changed`; the cited hash is never touched), and
    /// rewrite the ids in the meeting's items and in its note's AI blocks.
    /// Only `segment_ids_json` and `source_segment_ids` change — block
    /// markdown, origin kind, order and the note's `updated_at` don't.
    /// Returns the note whose blocks changed, for a mirror refresh.
    pub(crate) fn apply_reanchor_db(
        &self,
        new: &Transcript,
        plan: &ReanchorPlan,
    ) -> Result<Option<ReanchoredNote>> {
        let meeting_id = new.meeting_id.as_str();
        // Renumbered speakers first: cited keys follow their cluster, so the
        // checks below (and every later read) compare like with like. Row by
        // row, so a swap (spk_0 ↔ spk_1) can't chain.
        if !plan.key_map.is_empty() {
            let rows: Vec<(String, String, String, String)> = self
                .conn
                .prepare(
                    "SELECT owner_kind, owner_id, seg_id, speaker_key FROM citation_locators
                      WHERE meeting_id = ?1 AND speaker_key IS NOT NULL",
                )?
                .query_map([meeting_id], |r| {
                    Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?))
                })?
                .collect::<std::result::Result<_, _>>()?;
            for (kind, owner, seg_id, key) in rows {
                if let Some(to) = plan.key_map.get(&key) {
                    self.conn.execute(
                        "UPDATE citation_locators SET speaker_key = ?1
                          WHERE owner_kind = ?2 AND owner_id = ?3 AND seg_id = ?4",
                        (to, &kind, &owner, &seg_id),
                    )?;
                }
            }
        }
        let pairs = &plan.pairs;
        if pairs.is_empty() {
            return Ok(None);
        }
        for (old, new_id) in pairs {
            let Some(seg) = new.segments.iter().find(|s| &s.id == new_id) else {
                continue;
            };
            let new_hash = text_hash(&seg.text);
            let rows: Vec<LocRow> = self
                .conn
                .prepare(
                    "SELECT owner_kind, owner_id, text_hash, speaker_key FROM citation_locators
                      WHERE meeting_id = ?1 AND seg_id = ?2 AND text_hash != ''",
                )?
                .query_map((meeting_id, old), |r| {
                    Ok(LocRow {
                        kind: r.get(0)?,
                        owner: r.get(1)?,
                        hash: r.get(2)?,
                        speaker_key: r.get(3)?,
                    })
                })?
                .collect::<std::result::Result<_, _>>()?;
            for row in rows {
                let same = row.hash == new_hash
                    && row
                        .speaker_key
                        .as_deref()
                        .is_none_or(|k| k == seg.speaker_key);
                // Two old segments can map to one new one: keep one row.
                let n = self.conn.execute(
                    "UPDATE OR IGNORE citation_locators
                        SET seg_id = ?1, start_ms = ?2, end_ms = ?3, status = ?4
                      WHERE owner_kind = ?5 AND owner_id = ?6 AND seg_id = ?7",
                    rusqlite::params![
                        new_id,
                        seg.start_ms as i64,
                        seg.end_ms.max(seg.start_ms) as i64,
                        if same { "reanchored" } else { "changed" },
                        row.kind,
                        row.owner,
                        old
                    ],
                )?;
                if n == 0 {
                    // The owner already cites that new segment (two old
                    // segments matched one new one). One row survives; if
                    // this one no longer holds, the survivor says so.
                    if !same {
                        self.conn.execute(
                            "UPDATE citation_locators SET status = 'changed'
                              WHERE owner_kind = ?1 AND owner_id = ?2 AND seg_id = ?3",
                            (&row.kind, &row.owner, new_id),
                        )?;
                    }
                    self.conn.execute(
                        "DELETE FROM citation_locators
                          WHERE owner_kind = ?1 AND owner_id = ?2 AND seg_id = ?3",
                        (&row.kind, &row.owner, old),
                    )?;
                }
            }
        }
        let map: HashMap<String, String> = pairs.iter().cloned().collect();

        // Items: rewrite segment_ids_json in place.
        let items: Vec<(String, String)> = self
            .conn
            .prepare("SELECT id, segment_ids_json FROM meeting_items WHERE meeting_id = ?1")?
            .query_map([meeting_id], |r| Ok((r.get(0)?, r.get(1)?)))?
            .collect::<std::result::Result<_, _>>()?;
        for (id, json) in items {
            let ids: Vec<String> = serde_json::from_str(&json).unwrap_or_default();
            let (mapped, changed) = map_ids(&ids, &map);
            if changed {
                self.conn.execute(
                    "UPDATE meeting_items SET segment_ids_json = ?1 WHERE id = ?2",
                    (serde_json::to_string(&mapped)?, &id),
                )?;
            }
        }

        // The note's AI blocks: rewrite provenance only, without bumping
        // updated_at (the user's text didn't change).
        let Ok(meeting) = self.get_meeting(meeting_id) else {
            return Ok(None);
        };
        let Ok(mut note) = self.get_note(&meeting.note_id) else {
            return Ok(None);
        };
        let mut any = false;
        for block in &mut note.blocks {
            if let BlockOrigin::Ai { source_segment_ids } = &mut block.origin {
                let (mapped, changed) = map_ids(source_segment_ids, &map);
                if changed {
                    *source_segment_ids = mapped;
                    any = true;
                }
            }
        }
        if !any {
            return Ok(None);
        }
        self.conn.execute(
            "UPDATE notes SET blocks_json = ?1 WHERE id = ?2",
            (serde_json::to_string(&note.blocks)?, &note.id),
        )?;
        self.sync_note_citations(&note)?;
        Ok(Some(ReanchoredNote(note)))
    }

    /// Full re-sync for the v3 step: drop locators whose owner or meeting is
    /// gone (an older build may have deleted them), then record every AI
    /// block's and item's citations. Rows that already exist keep their
    /// hash and status. A note or meeting that fails is logged and skipped,
    /// never fatal (a failing step would stop the app from opening).
    pub(crate) fn sync_all_citations(&self) -> Result<()> {
        self.conn.execute_batch(
            "DELETE FROM citation_locators
              WHERE meeting_id NOT IN (SELECT meeting_id FROM transcripts);
             DELETE FROM citation_locators
              WHERE owner_kind IN ('note_block', 'ask_insert')
                AND (note_id IS NULL OR note_id NOT IN (SELECT id FROM notes));
             DELETE FROM citation_locators
              WHERE owner_kind = 'item' AND owner_id NOT IN (SELECT id FROM meeting_items);
             UPDATE citation_locators SET status = 'changed'
              WHERE status != 'changed'
                AND NOT EXISTS (SELECT 1 FROM transcript_segments ts
                                 WHERE ts.meeting_id = citation_locators.meeting_id
                                   AND ts.segment_id = citation_locators.seg_id);",
        )?;
        let note_ids: Vec<String> = self
            .conn
            .prepare("SELECT id FROM notes WHERE blocks_json LIKE '%source_segment_ids%'")?
            .query_map([], |r| r.get(0))?
            .collect::<std::result::Result<_, _>>()?;
        for id in note_ids {
            let res = self
                .get_note(&id)
                .and_then(|note| self.sync_note_citations(&note));
            if let Err(e) = res {
                tracing::warn!(note = %id, error = %e, "v3: skipping note citations");
            }
        }
        let meeting_ids: Vec<String> = self
            .conn
            .prepare("SELECT DISTINCT meeting_id FROM meeting_items")?
            .query_map([], |r| r.get(0))?
            .collect::<std::result::Result<_, _>>()?;
        for id in meeting_ids {
            if let Err(e) = self.sync_item_citations(&id) {
                tracing::warn!(meeting_id = %id, error = %e, "v3: skipping item citations");
            }
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_storage;
    use fly_core::{ItemKind, MeetingItem, NoteBlock, Speaker, TranscriptSegment};

    fn seg(id: &str, start_ms: u64, text: &str) -> TranscriptSegment {
        TranscriptSegment {
            id: id.into(),
            speaker_key: "spk_0".into(),
            start_ms,
            end_ms: start_ms + 2_000,
            text: text.into(),
            words: vec![],
        }
    }

    fn transcript(meeting_id: &str, segs: Vec<TranscriptSegment>) -> Transcript {
        Transcript {
            meeting_id: meeting_id.into(),
            language: Some("en".into()),
            engine: "test".into(),
            segments: segs,
            speakers: vec![Speaker {
                key: "spk_0".into(),
                label: "Dana".into(),
            }],
        }
    }

    fn item(meeting_id: &str, segs: &[&str]) -> MeetingItem {
        MeetingItem {
            id: fly_core::new_id(),
            meeting_id: meeting_id.into(),
            kind: ItemKind::Decision,
            text: "ship friday".into(),
            owner: None,
            status: None,
            speaker_key: None,
            segment_ids: segs.iter().map(|s| s.to_string()).collect(),
            created_at: chrono::Utc::now(),
            extracted_by: "test".into(),
        }
    }

    const FIRST: [(&str, u64, &str); 3] = [
        ("a1", 0, "we agreed the fuel bid goes out at four ninety"),
        (
            "a2",
            2_000,
            "legal still needs to sign off on the indemnity",
        ),
        ("a3", 4_000, "and we ship the renewal on friday"),
    ];

    fn setup() -> (tempfile::TempDir, Storage, String, String) {
        let (dir, s) = test_storage();
        let note = s.create_note("Airline deal", None).unwrap();
        let m = s.create_meeting("Airline deal", &note.id, &[]).unwrap();
        s.save_transcript(&transcript(
            &m.id,
            FIRST.iter().map(|(i, t, x)| seg(i, *t, x)).collect(),
        ))
        .unwrap();
        (dir, s, note.id, m.id)
    }

    #[test]
    fn hash_ignores_case_whitespace_and_punctuation() {
        assert_eq!(text_hash("Ship  it\nFriday"), text_hash("ship it friday"));
        assert_eq!(text_hash("Okay, sure."), text_hash("okay sure"));
        assert_ne!(text_hash("$4.90"), text_hash("$49.0"));
        assert_ne!(text_hash("we will ship"), text_hash("we will not ship"));
        assert_ne!(text_hash("ship it friday"), text_hash("ship it monday"));
        assert_eq!(text_hash("x").len(), 16);
    }

    #[test]
    fn blocks_and_items_record_locators_automatically() {
        let (_d, s, note_id, mid) = setup();
        let block = NoteBlock::ai("- Bid at $4.90", vec!["a1".into(), "nope".into()]);
        s.update_note_blocks(&note_id, &[block.clone(), NoteBlock::user("mine")])
            .unwrap();
        let locs = s.citation_locators(OWNER_NOTE_BLOCK, &block.id).unwrap();
        assert_eq!(locs.len(), 2);
        assert_eq!(locs[0].seg_id, "a1");
        assert_eq!((locs[0].start_ms, locs[0].end_ms), (0, 2_000));
        assert_eq!(locs[0].status, CitationStatus::Ok);
        assert_eq!(locs[0].text_hash, text_hash(FIRST[0].2), "raw text hashed");
        assert_eq!(locs[0].speaker_key.as_deref(), Some("spk_0"));
        // An id no transcript has gets a `changed` placeholder, so it can't
        // read as fine by omission.
        assert_eq!(
            (
                locs[1].seg_id.as_str(),
                locs[1].text_hash.as_str(),
                locs[1].status
            ),
            ("nope", "", CitationStatus::Changed)
        );

        let it = item(&mid, &["a3"]);
        s.replace_meeting_items(&mid, std::slice::from_ref(&it))
            .unwrap();
        assert_eq!(s.citation_locators(OWNER_ITEM, &it.id).unwrap().len(), 1);

        // Editing the AI block reclaims it as user text: locator dropped.
        s.edit_note_block(&note_id, &block.id, "- my own words")
            .unwrap();
        assert!(s
            .citation_locators(OWNER_NOTE_BLOCK, &block.id)
            .unwrap()
            .is_empty());
        // Re-extraction replaces items: old item's locators go.
        s.replace_meeting_items(&mid, &[]).unwrap();
        assert!(s.citation_locators(OWNER_ITEM, &it.id).unwrap().is_empty());
    }

    #[test]
    fn segment_edit_reads_as_changed_but_polish_of_cited_text_does_not() {
        let (_d, s, note_id, mid) = setup();
        let block = NoteBlock::ai("- Legal sign-off", vec!["a2".into()]);
        s.update_note_blocks(&note_id, std::slice::from_ref(&block))
            .unwrap();

        // A polish never moves the citation (the hash is of the raw text).
        let mut polished = s.get_transcript(&mid).unwrap().unwrap();
        polished.segments[1].text = "Legal still needs to sign off on the indemnity.".into();
        s.save_cleaned_transcript(&polished).unwrap();
        assert_eq!(
            s.citation_locators(OWNER_NOTE_BLOCK, &block.id).unwrap()[0].status,
            CitationStatus::Ok,
            "cited text still present in the raw variant"
        );

        // A user edit to the segment changes both variants.
        s.edit_segment_text(&mid, "a2", "legal signed off yesterday")
            .unwrap();
        assert_eq!(
            s.citation_locators(OWNER_NOTE_BLOCK, &block.id).unwrap()[0].status,
            CitationStatus::Changed
        );
    }

    #[test]
    fn retranscribe_reanchors_same_speech_and_flags_changed_speech() {
        let (_d, s, note_id, mid) = setup();
        let b1 = NoteBlock::ai("- Bid", vec!["a1".into()]);
        let b2 = NoteBlock::ai("- Legal", vec!["a2".into(), "a3".into()]);
        let mine = NoteBlock::user("- my own line");
        s.update_note_blocks(&note_id, &[b1.clone(), b2.clone(), mine.clone()])
            .unwrap();
        let it = item(&mid, &["a1", "a3"]);
        s.replace_meeting_items(&mid, std::slice::from_ref(&it))
            .unwrap();
        let note_before = s.get_note(&note_id).unwrap();
        let item_before = s.get_meeting_items(&mid).unwrap().remove(0);

        // New run: new ids, slightly shifted times; a1/a3 same speech, a2
        // decoded as something else entirely.
        s.save_transcript(&transcript(
            &mid,
            vec![
                seg("b1", 100, "we agreed the fuel bid goes out at 4 ninety"),
                seg("b2", 2_100, "music playing in the background"),
                seg("b3", 4_050, "and we ship the renewal on friday"),
            ],
        ))
        .unwrap();

        // "four ninety" → "4 ninety": the best match (pointer moves, so
        // zoom-in lands there), but not the same words → changed, with the
        // cited hash kept.
        let l1 = s.citation_locators(OWNER_NOTE_BLOCK, &b1.id).unwrap();
        assert_eq!(l1.len(), 1);
        assert_eq!(
            (l1[0].seg_id.as_str(), l1[0].status),
            ("b1", CitationStatus::Changed)
        );
        assert_eq!(l1[0].text_hash, text_hash(FIRST[0].2), "cited hash kept");
        let l2 = s.citation_locators(OWNER_NOTE_BLOCK, &b2.id).unwrap();
        let by_seg: HashMap<_, _> = l2.iter().map(|l| (l.seg_id.as_str(), l.status)).collect();
        assert_eq!(by_seg.get("a2"), Some(&CitationStatus::Changed), "{l2:?}");
        assert_eq!(
            by_seg.get("b3"),
            Some(&CitationStatus::Reanchored),
            "{l2:?}"
        );

        // Ids rewritten in the note blocks and the items.
        let note = s.get_note(&note_id).unwrap();
        let ids = |b: &NoteBlock| match &b.origin {
            BlockOrigin::Ai { source_segment_ids } => source_segment_ids.clone(),
            BlockOrigin::User => vec![],
        };
        assert_eq!(ids(&note.blocks[0]), vec!["b1".to_string()]);
        assert_eq!(
            ids(&note.blocks[1]),
            vec!["a2".to_string(), "b3".to_string()]
        );
        let items = s.get_meeting_items(&mid).unwrap();
        assert_eq!(
            items[0].segment_ids,
            vec!["b1".to_string(), "b3".to_string()]
        );

        // Re-anchoring rewrites ONLY the segment ids: block ids, order,
        // markdown and origin kind, user blocks, the note's updated_at, and
        // every other item field stay exactly as they were.
        assert_eq!(
            note.updated_at, note_before.updated_at,
            "no updated_at bump"
        );
        assert_eq!(note.title, note_before.title);
        assert_eq!(note.scratchpad, note_before.scratchpad);
        assert_eq!(note.blocks.len(), note_before.blocks.len());
        for (after, before) in note.blocks.iter().zip(&note_before.blocks) {
            assert_eq!(after.id, before.id);
            assert_eq!(after.markdown, before.markdown);
            assert_eq!(
                std::mem::discriminant(&after.origin),
                std::mem::discriminant(&before.origin),
                "origin kind unchanged"
            );
        }
        assert_eq!(note.blocks[2], mine, "user block untouched");
        let item_after = &items[0];
        assert_eq!(
            fly_core::MeetingItem {
                segment_ids: item_before.segment_ids.clone(),
                ..item_after.clone()
            },
            item_before,
            "only segment_ids changed on the item"
        );

        // A re-save of the same blocks keeps the re-pointed locators as
        // they are (no reset to `ok`).
        s.update_note_blocks(&note_id, &note.blocks).unwrap();
        let statuses = |id: &str| -> Vec<CitationStatus> {
            s.citation_locators(OWNER_NOTE_BLOCK, id)
                .unwrap()
                .iter()
                .map(|l| l.status)
                .collect()
        };
        assert_eq!(statuses(&b1.id), [CitationStatus::Changed]);
        assert!(statuses(&b2.id).contains(&CitationStatus::Reanchored));
    }

    /// Re-anchoring must never hide a change of meaning: the matcher only
    /// picks where the pointer goes; `reanchored` needs the same words (after
    /// case/whitespace normalization) from the same speaker.
    #[test]
    fn adversarial_retranscribes_read_as_changed() {
        let cases: [(&str, &str, &str, &str, u64, CitationStatus); 6] = [
            // (cited text, new text, cited speaker, new speaker, shift ms, want)
            (
                "we will ship the renewal on friday",
                "we will not ship the renewal on friday",
                "spk_0",
                "spk_0",
                0,
                CitationStatus::Changed,
            ),
            (
                "the bid goes out at four ninety",
                "the bid goes out at fourteen ninety",
                "spk_0",
                "spk_0",
                0,
                CitationStatus::Changed,
            ),
            (
                "okay sure",
                "okay sure",
                "spk_0",
                "spk_1",
                600,
                CitationStatus::Changed,
            ),
            (
                "okay sure",
                "Okay,  sure.",
                "spk_0",
                "spk_0",
                300,
                CitationStatus::Reanchored,
            ),
            (
                "okay sure",
                "okay so sure",
                "spk_0",
                "spk_0",
                0,
                CitationStatus::Changed,
            ),
            (
                "okay sure",
                "OKAY   sure",
                "spk_0",
                "spk_0",
                300,
                CitationStatus::Reanchored,
            ),
        ];
        // Both speakers also talk elsewhere, unchanged across the run, so a
        // single line changing hands is a re-attribution, not a renumbering.
        let context = |prefix: &str| -> Vec<TranscriptSegment> {
            (0..6)
                .flat_map(|i| {
                    let mut a = seg(
                        &format!("{prefix}a{i}"),
                        20_000 + i * 8_000,
                        "context from zero",
                    );
                    a.speaker_key = "spk_0".into();
                    let mut b = seg(
                        &format!("{prefix}b{i}"),
                        24_000 + i * 8_000,
                        "context from one",
                    );
                    b.speaker_key = "spk_1".into();
                    [a, b]
                })
                .collect()
        };
        for (cited, heard, k0, k1, shift, want) in cases {
            let (_d, s) = test_storage();
            let note = s.create_note("n", None).unwrap();
            let m = s.create_meeting("n", &note.id, &[]).unwrap();
            let mut first = seg("old", 10_000, cited);
            first.speaker_key = k0.into();
            let mut segs = vec![first];
            segs.extend(context("o"));
            s.save_transcript(&transcript(&m.id, segs)).unwrap();
            let b = NoteBlock::ai("- claim", vec!["old".into()]);
            s.update_note_blocks(&note.id, std::slice::from_ref(&b))
                .unwrap();

            let mut again = seg("new", 10_000 + shift, heard);
            again.speaker_key = k1.into();
            let mut segs = vec![again];
            segs.extend(context("n"));
            s.save_transcript(&transcript(&m.id, segs)).unwrap();

            let l = s.citation_locators(OWNER_NOTE_BLOCK, &b.id).unwrap();
            assert_eq!(l.len(), 1, "{cited:?} → {heard:?}");
            assert_eq!(
                l[0].seg_id, "new",
                "{cited:?} → {heard:?}: points at best match"
            );
            assert_eq!(l[0].status, want, "{cited:?} → {heard:?} ({k0}→{k1})");
            assert_eq!(
                l[0].text_hash,
                text_hash(cited),
                "cited hash never overwritten"
            );
        }
    }

    #[test]
    fn repolish_never_marks_changed_but_reattribution_does() {
        let (_d, s, note_id, mid) = setup();
        let b = NoteBlock::ai("- Renewal", vec!["a3".into()]);
        s.update_note_blocks(&note_id, std::slice::from_ref(&b))
            .unwrap();
        let status = || s.citation_locators(OWNER_NOTE_BLOCK, &b.id).unwrap()[0].status;

        for wording in [
            "And we ship the renewal on Friday.",
            "We're shipping the renewal Friday.",
        ] {
            let mut polished = s.get_transcript(&mid).unwrap().unwrap();
            polished.segments[2].text = wording.into();
            s.save_cleaned_transcript(&polished).unwrap();
            assert_eq!(status(), CitationStatus::Ok, "re-polish: {wording}");
        }

        // A re-diarize moving the line to another speaker changes meaning.
        let mut raw = s.get_transcript(&mid).unwrap().unwrap();
        raw.segments[2].speaker_key = "spk_1".into();
        s.save_transcript(&raw).unwrap();
        assert_eq!(status(), CitationStatus::Changed);
    }

    #[test]
    fn citing_ids_replaced_meanwhile_is_recorded_as_changed() {
        // Enhance read a1..a3, then a re-transcribe replaced them while the
        // model ran; the blocks it saves cite ids that no longer exist.
        let (_d, s, note_id, mid) = setup();
        s.save_transcript(&transcript(&mid, vec![seg("z1", 0, "fresh run")]))
            .unwrap();
        let b = NoteBlock::ai("- stale", vec!["a1".into(), "z1".into()]);
        s.update_note_blocks(&note_id, std::slice::from_ref(&b))
            .unwrap();
        let l = s.citation_locators(OWNER_NOTE_BLOCK, &b.id).unwrap();
        let by_seg: HashMap<_, _> = l.iter().map(|l| (l.seg_id.as_str(), l.status)).collect();
        assert_eq!(by_seg.get("a1"), Some(&CitationStatus::Changed));
        assert_eq!(by_seg.get("z1"), Some(&CitationStatus::Ok));
    }

    #[test]
    fn healed_rows_get_locators() {
        // The self-heal writes blocks_json directly (no write path runs).
        let (_d, s, note_id, mid) = setup();
        let b = NoteBlock::ai("- healed", vec!["a2".into()]);
        s.conn
            .execute(
                "UPDATE notes SET blocks_json = ?1 WHERE id = ?2",
                (
                    serde_json::to_string(std::slice::from_ref(&b)).unwrap(),
                    &note_id,
                ),
            )
            .unwrap();
        assert!(s
            .citation_locators(OWNER_NOTE_BLOCK, &b.id)
            .unwrap()
            .is_empty());
        s.sync_citations_for_meetings(std::slice::from_ref(&mid));
        assert_eq!(
            s.citation_locators(OWNER_NOTE_BLOCK, &b.id).unwrap()[0].status,
            CitationStatus::Ok
        );
    }

    /// The note mirror can't be written (locked, permissions): the new
    /// transcript still saves and the re-anchor still lands; only the
    /// mirror refresh is skipped (logged).
    #[test]
    fn reanchor_mirror_failure_never_fails_the_save() {
        let (_d, s, note_id, mid) = setup();
        let b = NoteBlock::ai("- Renewal", vec!["a3".into()]);
        s.update_note_blocks(&note_id, std::slice::from_ref(&b))
            .unwrap();
        let mirror = s.note_mirror_abs(&note_id);
        std::fs::remove_file(&mirror).unwrap();
        std::fs::create_dir_all(mirror.join("blocker")).unwrap();
        // The fault is real: a plain blocks save now fails on the mirror.
        assert!(s
            .update_note_blocks(&note_id, std::slice::from_ref(&b))
            .is_err());

        s.save_transcript(&transcript(
            &mid,
            vec![seg("c3", 4_000, "and we ship the renewal on friday")],
        ))
        .expect("save must succeed despite the mirror error");
        assert_eq!(
            s.get_transcript(&mid).unwrap().unwrap().segments[0].id,
            "c3"
        );
        let l = s.citation_locators(OWNER_NOTE_BLOCK, &b.id).unwrap();
        assert_eq!(
            (l[0].seg_id.as_str(), l[0].status),
            ("c3", CitationStatus::Reanchored)
        );
    }

    #[test]
    fn plain_saves_never_touch_locators() {
        let (_d, s, note_id, mid) = setup();
        let b = NoteBlock::ai("- Bid", vec!["a1".into()]);
        s.update_note_blocks(&note_id, std::slice::from_ref(&b))
            .unwrap();
        s.relabel_speaker(&mid, "spk_0", "Karim").unwrap();
        s.edit_segment_text(&mid, "a3", "and we ship on monday")
            .unwrap();
        let l = s.citation_locators(OWNER_NOTE_BLOCK, &b.id).unwrap();
        assert_eq!(
            (l[0].seg_id.as_str(), l[0].status),
            ("a1", CitationStatus::Ok)
        );
    }

    #[test]
    fn deleting_the_meeting_or_note_drops_locators() {
        let (_d, s, note_id, mid) = setup();
        let b = NoteBlock::ai("- Bid", vec!["a1".into()]);
        s.update_note_blocks(&note_id, std::slice::from_ref(&b))
            .unwrap();
        s.purge_meeting(&mid).unwrap();
        let n: i64 = s
            .conn
            .query_row("SELECT COUNT(*) FROM citation_locators", [], |r| r.get(0))
            .unwrap();
        assert_eq!(n, 0);
    }

    /// Two speakers, six lines each; the cited line is `b2` (spk_1).
    fn two_speakers(meeting_id: &str, prefix: &str) -> Transcript {
        let segs = (0..6)
            .flat_map(|i| {
                let mut a = seg(
                    &format!("{prefix}a{i}"),
                    i * 8_000,
                    &format!("dana line {i}"),
                );
                a.speaker_key = "spk_0".into();
                let mut b = seg(
                    &format!("{prefix}b{i}"),
                    i * 8_000 + 4_000,
                    &format!("karim line {i}"),
                );
                b.speaker_key = "spk_1".into();
                [a, b]
            })
            .collect();
        transcript(meeting_id, segs)
    }

    fn swap_keys(t: &mut Transcript) {
        for s in &mut t.segments {
            s.speaker_key = if s.speaker_key == "spk_0" {
                "spk_1"
            } else {
                "spk_0"
            }
            .into();
        }
    }

    fn cite_karim(s: &Storage, mid: &str, prefix: &str) -> (String, NoteBlock) {
        let note = s.get_meeting(mid).unwrap().note_id;
        let b = NoteBlock::ai("- Karim said", vec![format!("{prefix}b2")]);
        s.update_note_blocks(&note, std::slice::from_ref(&b))
            .unwrap();
        (note, b)
    }

    fn setup_two() -> (tempfile::TempDir, Storage, String) {
        let (dir, s) = test_storage();
        let note = s.create_note("1:1", None).unwrap();
        let m = s.create_meeting("1:1", &note.id, &[]).unwrap();
        s.save_transcript(&two_speakers(&m.id, "")).unwrap();
        (dir, s, m.id)
    }

    #[test]
    fn stable_key_map_needs_80_percent_both_ways() {
        let old = two_speakers("m", "");
        let mut new = old.clone();
        swap_keys(&mut new);
        let m = stable_key_map(&old, &new);
        assert_eq!(m.get("spk_0").map(String::as_str), Some("spk_1"));
        assert_eq!(m.get("spk_1").map(String::as_str), Some("spk_0"));
        // One line of six moving keeps both clusters stable (5/6 ≥ 0.8)...
        let mut moved = old.clone();
        moved.segments[5].speaker_key = "spk_0".into();
        assert_eq!(
            stable_key_map(&old, &moved)
                .get("spk_1")
                .map(String::as_str),
            Some("spk_1")
        );
        // ...a cluster split in half maps to nothing.
        let mut split = old.clone();
        for s in split
            .segments
            .iter_mut()
            .filter(|s| s.speaker_key == "spk_1")
            .take(3)
        {
            s.speaker_key = "spk_2".into();
        }
        assert!(!stable_key_map(&old, &split).contains_key("spk_1"));
        // Time overlap when ids differ (re-transcribe).
        let mut renamed = two_speakers("m", "x");
        swap_keys(&mut renamed);
        assert_eq!(
            stable_key_map(&old, &renamed)
                .get("spk_1")
                .map(String::as_str),
            Some("spk_0")
        );
    }

    #[test]
    fn renumbering_rediarize_keeps_citations_ok() {
        let (_d, s, mid) = setup_two();
        let (_, b) = cite_karim(&s, &mid, "");
        let mut t = s.get_transcript(&mid).unwrap().unwrap();
        swap_keys(&mut t); // Karim spk_1 → spk_0, Dana spk_0 → spk_1
        s.save_transcript(&t).unwrap();
        let l = s.citation_locators(OWNER_NOTE_BLOCK, &b.id).unwrap();
        assert_eq!(
            (l[0].status, l[0].speaker_key.as_deref()),
            (CitationStatus::Ok, Some("spk_0"))
        );
        // And back again (revert / another re-diarize).
        swap_keys(&mut t);
        s.save_transcript(&t).unwrap();
        let l = s.citation_locators(OWNER_NOTE_BLOCK, &b.id).unwrap();
        assert_eq!(
            (l[0].status, l[0].speaker_key.as_deref()),
            (CitationStatus::Ok, Some("spk_1"))
        );
    }

    #[test]
    fn genuine_reattribution_reads_changed_even_with_renumbering() {
        // Cited line moves to the other cluster; clusters stay stable.
        let (_d, s, mid) = setup_two();
        let (_, b) = cite_karim(&s, &mid, "");
        let mut t = s.get_transcript(&mid).unwrap().unwrap();
        let i = t.segments.iter().position(|x| x.id == "b2").unwrap();
        t.segments[i].speaker_key = "spk_0".into();
        s.save_transcript(&t).unwrap();
        assert_eq!(
            s.citation_locators(OWNER_NOTE_BLOCK, &b.id).unwrap()[0].status,
            CitationStatus::Changed
        );
        // Same, combined with a renumbering of both clusters.
        let (_d2, s2, mid2) = setup_two();
        let (_, b2) = cite_karim(&s2, &mid2, "");
        let mut t2 = s2.get_transcript(&mid2).unwrap().unwrap();
        swap_keys(&mut t2);
        let i = t2.segments.iter().position(|x| x.id == "b2").unwrap();
        t2.segments[i].speaker_key = "spk_1".into(); // stayed on the old number = moved
        s2.save_transcript(&t2).unwrap();
        assert_eq!(
            s2.citation_locators(OWNER_NOTE_BLOCK, &b2.id).unwrap()[0].status,
            CitationStatus::Changed
        );
    }

    #[test]
    fn renumbering_retranscribe_reanchors() {
        let (_d, s, mid) = setup_two();
        let (_, b) = cite_karim(&s, &mid, "");
        let mut fresh = two_speakers(&mid, "x"); // new ids, same speech
        swap_keys(&mut fresh);
        s.save_transcript(&fresh).unwrap();
        let l = s.citation_locators(OWNER_NOTE_BLOCK, &b.id).unwrap();
        assert_eq!(
            (
                l[0].seg_id.as_str(),
                l[0].status,
                l[0].speaker_key.as_deref()
            ),
            ("xb2", CitationStatus::Reanchored, Some("spk_0"))
        );
    }

    /// Two cited segments re-anchor to the same new one for the same owner:
    /// one row survives, and if either no longer holds, it reads changed.
    #[test]
    fn colliding_reanchors_keep_the_changed_signal() {
        for order in [["p1", "p2"], ["p2", "p1"]] {
            let (_d, s) = test_storage();
            let note = s.create_note("n", None).unwrap();
            let m = s.create_meeting("n", &note.id, &[]).unwrap();
            let texts = |id: &str| {
                if id == "p1" {
                    "ship it friday"
                } else {
                    "ship it friday too"
                }
            };
            let segs: Vec<_> = order
                .iter()
                .enumerate()
                .map(|(i, id)| {
                    let mut x = seg(id, i as u64 * 2_000, texts(id));
                    x.end_ms = x.start_ms + 2_000;
                    x
                })
                .collect();
            s.save_transcript(&transcript(&m.id, segs)).unwrap();
            let b = NoteBlock::ai("- ship", vec!["p1".into(), "p2".into()]);
            s.update_note_blocks(&note.id, std::slice::from_ref(&b))
                .unwrap();
            let mut merged = seg("q", 0, "ship it friday");
            merged.end_ms = 4_000;
            s.save_transcript(&transcript(&m.id, vec![merged])).unwrap();
            let l = s.citation_locators(OWNER_NOTE_BLOCK, &b.id).unwrap();
            assert_eq!(l.len(), 1, "{order:?}: {l:?}");
            assert_eq!(
                (l[0].seg_id.as_str(), l[0].status),
                ("q", CitationStatus::Changed),
                "{order:?}"
            );
        }
    }

    /// A database error inside the re-anchor rolls back only the re-anchor:
    /// the transcript saves, the citations stay `changed` on their old ids,
    /// items and blocks are untouched, and no savepoint is left open.
    #[test]
    fn reanchor_db_error_rolls_back_only_the_reanchor() {
        let (_d, s, note_id, mid) = setup();
        let b = NoteBlock::ai("- Renewal", vec!["a3".into()]);
        s.update_note_blocks(&note_id, std::slice::from_ref(&b))
            .unwrap();
        let it = item(&mid, &["a3"]);
        s.replace_meeting_items(&mid, std::slice::from_ref(&it))
            .unwrap();
        s.conn
            .execute_batch(
                "CREATE TRIGGER boom BEFORE UPDATE ON meeting_items
                 BEGIN SELECT RAISE(ABORT, 'injected'); END;",
            )
            .unwrap();
        s.save_transcript(&transcript(
            &mid,
            vec![seg("c3", 4_000, "and we ship the renewal on friday")],
        ))
        .expect("save survives a re-anchor failure");
        assert_eq!(
            s.get_transcript(&mid).unwrap().unwrap().segments[0].id,
            "c3"
        );
        let l = s.citation_locators(OWNER_NOTE_BLOCK, &b.id).unwrap();
        assert_eq!(
            (l[0].seg_id.as_str(), l[0].status),
            ("a3", CitationStatus::Changed)
        );
        assert_eq!(s.get_meeting_items(&mid).unwrap()[0].segment_ids, ["a3"]);
        let note = s.get_note(&note_id).unwrap();
        assert!(matches!(&note.blocks[0].origin,
            BlockOrigin::Ai { source_segment_ids } if source_segment_ids == &["a3".to_string()]));
        assert!(s.conn.is_autocommit(), "no savepoint left open");
    }

    #[test]
    fn deleting_the_note_drops_its_block_locators() {
        let (_d, s, note_id, _mid) = setup();
        let b = NoteBlock::ai("- Bid", vec!["a1".into()]);
        s.update_note_blocks(&note_id, std::slice::from_ref(&b))
            .unwrap();
        s.delete_note(&note_id).unwrap();
        assert!(s
            .citation_locators(OWNER_NOTE_BLOCK, &b.id)
            .unwrap()
            .is_empty());
    }

    #[test]
    fn matcher_scores() {
        assert!((time_iou((0, 10), (0, 10)) - 1.0).abs() < 1e-9);
        assert_eq!(time_iou((0, 10), (20, 30)), 0.0);
        let a = tokens("Ship it Friday!");
        assert!((jaccard(&a, &tokens("ship it friday")) - 1.0).abs() < 1e-9);
    }
}
