//! Citation locators (schema v3, spec §4.2a #6).
//!
//! AI note blocks and extracted items cite transcript segments by id. A new
//! transcription run generates new ids (`align.rs` `new_id()`), so without
//! help every citation silently dangles. A locator remembers, per (citing
//! thing, segment), where the segment was (`start_ms`/`end_ms`) and what it
//! said (`text_hash`), so that:
//!
//! * a re-transcribe can **re-anchor** each citation to the new segment that
//!   best matches it, and rewrite the id in the block / item so the
//!   existing "zoom in" keeps working ([`Storage::save_transcript`]);
//! * a reader can tell when a cited segment's text has changed since it was
//!   cited (an edit, a different polish) and show "source changed — check".
//!
//! Locators are written automatically by the storage write paths for note
//! blocks (`update_note_blocks`) and items (`replace_meeting_items`); Ask
//! inserts can use [`Storage::record_citations`]. The hash is taken from
//! the text as indexed when the citation is first recorded (polished when
//! present) and is never refreshed by later saves, which is what makes
//! drift detectable.
//!
//! An older build that opens a v3 file never writes locators. The
//! `transcripts` triggers (segments.rs) still mark citations whose segment
//! disappeared as `changed` and drop a deleted meeting's locators, and the
//! v3 step re-syncs everything when a newer build opens the file again.

use std::collections::{HashMap, HashSet};

use fly_core::{BlockOrigin, Note, Transcript};
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
/// Minimum match score (0.5 × time IoU + 0.5 × token Jaccard).
const REANCHOR_MIN_SCORE: f64 = 0.6;

pub(crate) const SCHEMA_SQL: &str = r#"
    CREATE TABLE IF NOT EXISTS citation_locators (
        owner_kind TEXT NOT NULL,   -- 'note_block' | 'item' | 'ask_insert'
        owner_id   TEXT NOT NULL,   -- NoteBlock.id / MeetingItem.id / insert id
        note_id    TEXT,            -- note holding the block (note_block, ask_insert)
        meeting_id TEXT NOT NULL,
        seg_id     TEXT NOT NULL,
        start_ms   INTEGER NOT NULL,
        end_ms     INTEGER NOT NULL,
        text_hash  TEXT NOT NULL,   -- see text_hash()
        status     TEXT NOT NULL DEFAULT 'ok',  -- 'ok' | 'reanchored' | 'changed'
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
    /// The segment exists and says what it said when cited.
    Ok,
    /// A re-transcribe replaced the segment; the citation now points at the
    /// best-matching new segment.
    Reanchored,
    /// The segment is gone, or its text changed since it was cited.
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
    pub text_hash: String,
    /// Computed when read: stored state, plus a hash check against the
    /// segment's current text.
    pub status: CitationStatus,
}

/// Hex SHA-256 (first 16 chars) of the text lowercased with whitespace
/// collapsed, so re-wrapping or casing never reads as a change.
pub fn text_hash(text: &str) -> String {
    let norm = text
        .split_whitespace()
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
}

/// Best new segment for each lost one: candidates overlap the lost span
/// padded by [`REANCHOR_PAD_MS`]; score = 0.5 × time IoU + 0.5 × token
/// Jaccard; accepted at [`REANCHOR_MIN_SCORE`] or above.
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
                let score = 0.5 * time_iou(l.span, (s.start_ms, s.end_ms))
                    + 0.5 * jaccard(&l.words, &tokens(&s.text));
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

impl Storage {
    /// Locators for one citing thing, with live status.
    pub fn citation_locators(
        &self,
        owner_kind: &str,
        owner_id: &str,
    ) -> Result<Vec<CitationLocator>> {
        let mut stmt = self.conn.prepare(
            "SELECT meeting_id, seg_id, start_ms, end_ms, text_hash, status
               FROM citation_locators WHERE owner_kind = ?1 AND owner_id = ?2
              ORDER BY start_ms, seg_id",
        )?;
        let rows: Vec<(String, String, i64, i64, String, String)> = stmt
            .query_map((owner_kind, owner_id), |r| {
                Ok((
                    r.get(0)?,
                    r.get(1)?,
                    r.get(2)?,
                    r.get(3)?,
                    r.get(4)?,
                    r.get(5)?,
                ))
            })?
            .collect::<std::result::Result<_, _>>()?;
        let mut raw_cache: HashMap<String, Option<Transcript>> = HashMap::new();
        let mut out = Vec::with_capacity(rows.len());
        for (meeting_id, seg_id, start, end, hash, stored) in rows {
            let mut status = CitationStatus::parse(&stored);
            if status != CitationStatus::Changed {
                status = match self.indexed_segment(&meeting_id, &seg_id)? {
                    None => CitationStatus::Changed,
                    Some(seg) if text_hash(&seg.text) == hash => status,
                    Some(_) => {
                        // Cited from the other variant (raw vs polished)?
                        if !raw_cache.contains_key(&meeting_id) {
                            raw_cache.insert(meeting_id.clone(), self.get_transcript(&meeting_id)?);
                        }
                        let raw_matches = raw_cache[&meeting_id].as_ref().is_some_and(|t| {
                            t.segments
                                .iter()
                                .any(|s| s.id == seg_id && text_hash(&s.text) == hash)
                        });
                        if raw_matches {
                            status
                        } else {
                            CitationStatus::Changed
                        }
                    }
                };
            }
            out.push(CitationLocator {
                owner_kind: owner_kind.to_string(),
                owner_id: owner_id.to_string(),
                meeting_id,
                seg_id,
                start_ms: start.max(0) as u64,
                end_ms: end.max(0) as u64,
                text_hash: hash,
                status,
            });
        }
        Ok(out)
    }

    /// Make one citing thing's locators match `seg_ids`: pairs it no longer
    /// cites are dropped, new ones are recorded from the current index (each
    /// id resolved in the first of `meeting_ids` whose transcript has it;
    /// ids no transcript has are skipped), and pairs already recorded are
    /// kept as they are, so their hash still describes the cited text.
    /// Returns how many locators the owner has afterwards.
    pub fn record_citations(
        &self,
        owner_kind: &str,
        owner_id: &str,
        note_id: Option<&str>,
        meeting_ids: &[String],
        seg_ids: &[String],
    ) -> Result<usize> {
        let existing: HashSet<String> = self
            .conn
            .prepare(
                "SELECT seg_id FROM citation_locators WHERE owner_kind = ?1 AND owner_id = ?2",
            )?
            .query_map((owner_kind, owner_id), |r| r.get(0))?
            .collect::<std::result::Result<_, _>>()?;
        let wanted: HashSet<&String> = seg_ids.iter().collect();
        for gone in existing.iter().filter(|s| !wanted.contains(s)) {
            self.conn.execute(
                "DELETE FROM citation_locators
                  WHERE owner_kind = ?1 AND owner_id = ?2 AND seg_id = ?3",
                (owner_kind, owner_id, gone),
            )?;
        }
        let mut count = existing.iter().filter(|s| wanted.contains(s)).count();
        for seg_id in seg_ids {
            if existing.contains(seg_id) {
                continue;
            }
            for meeting_id in meeting_ids {
                if let Some(seg) = self.indexed_segment(meeting_id, seg_id)? {
                    let start = seg.start_ms.unwrap_or(0);
                    let n = self.conn.execute(
                        "INSERT OR IGNORE INTO citation_locators
                         (owner_kind, owner_id, note_id, meeting_id, seg_id,
                          start_ms, end_ms, text_hash, status)
                         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, 'ok')",
                        rusqlite::params![
                            owner_kind,
                            owner_id,
                            note_id,
                            meeting_id,
                            seg_id,
                            start as i64,
                            seg.end_ms.unwrap_or(start) as i64,
                            text_hash(&seg.text)
                        ],
                    )?;
                    count += n;
                    break;
                }
            }
        }
        Ok(count)
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
        let mut cited: HashSet<&str> = HashSet::new();
        for block in &note.blocks {
            if let BlockOrigin::Ai { source_segment_ids } = &block.origin {
                if source_segment_ids.is_empty() {
                    continue;
                }
                cited.insert(&block.id);
                self.record_citations(
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
        let mut cited: HashSet<&str> = HashSet::new();
        for item in &items {
            cited.insert(&item.id);
            self.record_citations(OWNER_ITEM, &item.id, None, &scope, &item.segment_ids)?;
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

    /// Before `new` replaces a meeting's transcript: for every cited segment
    /// it no longer has, find the new segment that best matches the old one
    /// (old text read from the transcript about to be replaced). Returns
    /// old id → new id; cited segments without a good match aren't listed.
    pub(crate) fn plan_reanchor(&self, new: &Transcript) -> Result<Vec<(String, String)>> {
        let cited: Vec<(String, i64, i64)> = self
            .conn
            .prepare(
                "SELECT seg_id, MIN(start_ms), MAX(end_ms) FROM citation_locators
                  WHERE meeting_id = ?1 GROUP BY seg_id",
            )?
            .query_map([&new.meeting_id], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)))?
            .collect::<std::result::Result<_, _>>()?;
        if cited.is_empty() {
            return Ok(vec![]);
        }
        let new_ids: HashSet<&str> = new.segments.iter().map(|s| s.id.as_str()).collect();
        let dangling: Vec<_> = cited
            .into_iter()
            .filter(|(id, _, _)| !new_ids.contains(id.as_str()))
            .collect();
        if dangling.is_empty() {
            return Ok(vec![]);
        }
        let old = self.get_transcript(&new.meeting_id).ok().flatten();
        let lost: Vec<Lost> = dangling
            .into_iter()
            .map(|(seg_id, start, end)| {
                let old_seg = old
                    .as_ref()
                    .and_then(|t| t.segments.iter().find(|s| s.id == seg_id));
                let (span, words) = match old_seg {
                    Some(s) => ((s.start_ms, s.end_ms), tokens(&s.text)),
                    // Unknown text: time alone caps the score at 0.5 (< the
                    // threshold), so such a citation is marked changed.
                    None => ((start.max(0) as u64, end.max(0) as u64), HashSet::new()),
                };
                Lost {
                    seg_id,
                    span,
                    words,
                }
            })
            .collect();
        Ok(match_lost(&lost, new))
    }

    /// After the new transcript is saved: point re-anchored locators at
    /// their new segments, and rewrite the ids in the meeting's items and in
    /// its note's AI blocks so existing segment links keep working.
    pub(crate) fn apply_reanchor(
        &self,
        meeting_id: &str,
        pairs: &[(String, String)],
    ) -> Result<()> {
        if pairs.is_empty() {
            return Ok(());
        }
        for (old, new) in pairs {
            let Some(seg) = self.indexed_segment(meeting_id, new)? else {
                continue;
            };
            let owners: Vec<(String, String)> = self
                .conn
                .prepare(
                    "SELECT owner_kind, owner_id FROM citation_locators
                      WHERE meeting_id = ?1 AND seg_id = ?2",
                )?
                .query_map((meeting_id, old), |r| Ok((r.get(0)?, r.get(1)?)))?
                .collect::<std::result::Result<_, _>>()?;
            for (kind, owner) in owners {
                let start = seg.start_ms.unwrap_or(0);
                // Two old segments can map to one new one: keep one row.
                let n = self.conn.execute(
                    "UPDATE OR IGNORE citation_locators
                        SET seg_id = ?1, start_ms = ?2, end_ms = ?3, text_hash = ?4,
                            status = 'reanchored'
                      WHERE owner_kind = ?5 AND owner_id = ?6 AND seg_id = ?7",
                    rusqlite::params![
                        new,
                        start as i64,
                        seg.end_ms.unwrap_or(start) as i64,
                        text_hash(&seg.text),
                        kind,
                        owner,
                        old
                    ],
                )?;
                if n == 0 {
                    self.conn.execute(
                        "DELETE FROM citation_locators
                          WHERE owner_kind = ?1 AND owner_id = ?2 AND seg_id = ?3",
                        (&kind, &owner, old),
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

        // The note's AI blocks: rewrite provenance without bumping the
        // note's updated_at (the user's text didn't change), then refresh
        // its mirrors so note.json carries the new ids too.
        let Ok(meeting) = self.get_meeting(meeting_id) else {
            return Ok(());
        };
        let Ok(mut note) = self.get_note(&meeting.note_id) else {
            return Ok(());
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
        if any {
            self.conn.execute(
                "UPDATE notes SET blocks_json = ?1 WHERE id = ?2",
                (serde_json::to_string(&note.blocks)?, &note.id),
            )?;
            self.sync_note_derived(&note)?;
            self.sync_note_citations(&note)?;
        }
        Ok(())
    }

    /// Full re-sync for the v3 step: drop locators whose owner or meeting is
    /// gone (an older build may have deleted them), then record every AI
    /// block's and item's citations. Rows that already exist keep their
    /// hash and status. Unreadable rows are skipped, never fatal.
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
            match self.get_note(&id) {
                Ok(note) => self.sync_note_citations(&note)?,
                Err(e) => tracing::warn!(note = %id, error = %e, "v3: skipping note citations"),
            }
        }
        let meeting_ids: Vec<String> = self
            .conn
            .prepare("SELECT DISTINCT meeting_id FROM meeting_items")?
            .query_map([], |r| r.get(0))?
            .collect::<std::result::Result<_, _>>()?;
        for id in meeting_ids {
            self.sync_item_citations(&id)?;
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
    fn hash_ignores_case_and_whitespace() {
        assert_eq!(text_hash("Ship  it\nFriday"), text_hash("ship it friday"));
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
        assert_eq!(locs.len(), 1, "unknown ids are skipped");
        assert_eq!(locs[0].seg_id, "a1");
        assert_eq!((locs[0].start_ms, locs[0].end_ms), (0, 2_000));
        assert_eq!(locs[0].status, CitationStatus::Ok);

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

        // Polish changes casing/spacing only → same normalized hash.
        let mut polished = s.get_transcript(&mid).unwrap().unwrap();
        polished.segments[1].text = "Legal still needs to sign off on the indemnity.".into();
        s.save_cleaned_transcript(&polished).unwrap();
        // (punctuation counts as text, so this one IS a change in the
        // polished variant; the raw text still matches the hash.)
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

        let l1 = s.citation_locators(OWNER_NOTE_BLOCK, &b1.id).unwrap();
        assert_eq!(l1.len(), 1);
        assert_eq!(
            (l1[0].seg_id.as_str(), l1[0].status),
            ("b1", CitationStatus::Reanchored)
        );
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

        // A re-save of the same blocks keeps the re-anchored locators as
        // they are (no reset to `ok`).
        s.update_note_blocks(&note_id, &note.blocks).unwrap();
        assert_eq!(
            s.citation_locators(OWNER_NOTE_BLOCK, &b1.id).unwrap()[0].status,
            CitationStatus::Reanchored
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

    #[test]
    fn matcher_scores() {
        assert!((time_iou((0, 10), (0, 10)) - 1.0).abs() < 1e-9);
        assert_eq!(time_iou((0, 10), (20, 30)), 0.0);
        let a = tokens("Ship it Friday!");
        assert!((jaccard(&a, &tokens("ship it friday")) - 1.0).abs() < 1e-9);
    }
}
