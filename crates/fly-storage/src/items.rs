//! Extracted meeting items: the structured context layer over transcripts.
//! One row per typed fact (decision, action item, question, commitment,
//! figure) with full provenance (meeting_id, source segment ids, speaker).
//! The app writes them once per meeting after transcription (plus on-demand
//! backfill); the MCP server only reads.

use chrono::{DateTime, Utc};
use fly_core::{ItemKind, Meeting, MeetingItem};

use crate::{Result, Storage};

/// Filters for [`Storage::query_items`]; `None` fields don't constrain.
#[derive(Debug, Default, Clone)]
pub struct ItemFilter {
    pub kind: Option<ItemKind>,
    /// Case-insensitive substring match on the owner.
    pub owner: Option<String>,
    pub status: Option<String>,
    /// Only items from meetings that STARTED at/after this instant.
    pub since: Option<DateTime<Utc>>,
    /// Restrict to these meetings (e.g. one recurring series).
    pub meeting_ids: Option<Vec<String>>,
    pub limit: usize,
    /// Drop items of meetings whose note is keep-local (own flag, a sibling
    /// meeting's flag, or a local-only folder chain) BEFORE the limit —
    /// for readers that hand results to external AI tools (MCP).
    pub exclude_keep_local: bool,
}

/// `extracted_by` prefix of items the user added (an approved Ask
/// proposal). Re-extraction keeps them, and they don't count as "extraction
/// ran" for backfill.
pub const USER_ITEM_PREFIX: &str = "user:";

impl Storage {
    /// Replace a meeting's extracted items wholesale (re-extraction is
    /// idempotent: each run rebuilds the meeting's rows from its transcript).
    /// Items the user added ([`USER_ITEM_PREFIX`]) are kept.
    pub fn replace_meeting_items(&self, meeting_id: &str, items: &[MeetingItem]) -> Result<()> {
        self.conn.execute(
            "DELETE FROM meeting_items WHERE meeting_id = ?1
               AND extracted_by NOT LIKE 'user:%'",
            [meeting_id],
        )?;
        let mut stmt = self.conn.prepare(
            "INSERT INTO meeting_items
             (id, meeting_id, kind, text, owner, status, speaker_key, segment_ids_json,
              created_at, extracted_by)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10)",
        )?;
        for item in items {
            stmt.execute((
                &item.id,
                meeting_id,
                item.kind.as_str(),
                &item.text,
                &item.owner,
                &item.status,
                &item.speaker_key,
                serde_json::to_string(&item.segment_ids)?,
                item.created_at.to_rfc3339(),
                &item.extracted_by,
            ))?;
        }
        self.sync_item_citations(meeting_id)?;
        Ok(())
    }

    pub fn get_meeting_items(&self, meeting_id: &str) -> Result<Vec<MeetingItem>> {
        let mut stmt = self.conn.prepare(
            "SELECT id, meeting_id, kind, text, owner, status, speaker_key,
                    segment_ids_json, created_at, extracted_by
             FROM meeting_items WHERE meeting_id = ?1 ORDER BY rowid",
        )?;
        let rows = stmt.query_map([meeting_id], row_to_item)?;
        Ok(rows
            .collect::<std::result::Result<Vec<_>, _>>()?
            .into_iter()
            .flatten()
            .collect())
    }

    /// Whether extraction has run for this meeting (even if it found nothing:
    /// an empty extraction still writes a marker row, see `mark_extracted`).
    /// Items the user added don't count.
    pub fn has_meeting_items(&self, meeting_id: &str) -> Result<bool> {
        let n: i64 = self.conn.query_row(
            "SELECT COUNT(*) FROM meeting_items WHERE meeting_id = ?1
               AND extracted_by NOT LIKE 'user:%'",
            [meeting_id],
            |r| r.get(0),
        )?;
        Ok(n > 0)
    }

    /// Add one item the user approved (an Ask `add_action_item` proposal).
    /// Its `extracted_by` must start with [`USER_ITEM_PREFIX`]. Records its
    /// citation locators like extracted items.
    pub fn add_meeting_item(&self, item: &MeetingItem) -> Result<()> {
        if !item.extracted_by.starts_with(USER_ITEM_PREFIX) {
            return Err(crate::StorageError::Invalid(format!(
                "user items are marked {USER_ITEM_PREFIX}…"
            )));
        }
        self.conn.execute(
            "INSERT INTO meeting_items
             (id, meeting_id, kind, text, owner, status, speaker_key, segment_ids_json,
              created_at, extracted_by)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10)",
            (
                &item.id,
                &item.meeting_id,
                item.kind.as_str(),
                &item.text,
                &item.owner,
                &item.status,
                &item.speaker_key,
                serde_json::to_string(&item.segment_ids)?,
                item.created_at.to_rfc3339(),
                &item.extracted_by,
            ),
        )?;
        self.sync_item_citations(&item.meeting_id)?;
        Ok(())
    }

    /// A speaker was relabelled: items of that meeting owned by the old
    /// label now name the new one. Returns how many changed.
    pub(crate) fn rename_item_owner(
        &self,
        meeting_id: &str,
        old: &str,
        new: &str,
    ) -> Result<usize> {
        let (old, new) = (old.trim(), new.trim());
        if old.is_empty() || new.is_empty() || old == new {
            return Ok(0);
        }
        Ok(self.conn.execute(
            "UPDATE meeting_items SET owner = ?3
              WHERE meeting_id = ?1 AND owner IS NOT NULL
                AND lower(trim(owner)) = lower(?2)",
            (meeting_id, old, new),
        )?)
    }

    /// Query items across meetings, newest meeting first (then row order).
    pub fn query_items(&self, filter: &ItemFilter) -> Result<Vec<MeetingItem>> {
        let mut sql = String::new();
        if filter.exclude_keep_local {
            sql.push_str(crate::privacy::KEEP_LOCAL_CTE);
        }
        sql.push_str(
            "SELECT i.id, i.meeting_id, i.kind, i.text, i.owner, i.status, i.speaker_key,
                    i.segment_ids_json, i.created_at, i.extracted_by
             FROM meeting_items i JOIN meetings m ON m.id = i.meeting_id
             WHERE i.kind != '_none'",
        );
        if filter.exclude_keep_local {
            sql.push_str(" AND m.note_id NOT IN (SELECT id FROM local_notes)");
        }
        let mut params: Vec<Box<dyn rusqlite::ToSql>> = Vec::new();
        if let Some(kind) = filter.kind {
            sql.push_str(&format!(" AND i.kind = ?{}", params.len() + 1));
            params.push(Box::new(kind.as_str().to_string()));
        }
        if let Some(owner) = &filter.owner {
            sql.push_str(&format!(" AND lower(i.owner) LIKE ?{}", params.len() + 1));
            params.push(Box::new(format!("%{}%", owner.to_lowercase())));
        }
        if let Some(status) = &filter.status {
            sql.push_str(&format!(" AND i.status = ?{}", params.len() + 1));
            params.push(Box::new(status.clone()));
        }
        if let Some(since) = &filter.since {
            sql.push_str(&format!(" AND m.started_at >= ?{}", params.len() + 1));
            params.push(Box::new(since.to_rfc3339()));
        }
        if let Some(ids) = &filter.meeting_ids {
            let placeholders: Vec<String> = (0..ids.len())
                .map(|i| format!("?{}", params.len() + 1 + i))
                .collect();
            sql.push_str(&format!(
                " AND i.meeting_id IN ({})",
                placeholders.join(",")
            ));
            for id in ids {
                params.push(Box::new(id.clone()));
            }
        }
        sql.push_str(" ORDER BY m.started_at DESC, i.rowid");
        let limit = if filter.limit == 0 { 200 } else { filter.limit };
        sql.push_str(&format!(" LIMIT {limit}"));

        let mut stmt = self.conn.prepare(&sql)?;
        let rows = stmt.query_map(
            rusqlite::params_from_iter(params.iter().map(|p| p.as_ref())),
            row_to_item,
        )?;
        Ok(rows
            .collect::<std::result::Result<Vec<_>, _>>()?
            .into_iter()
            .flatten()
            .collect())
    }

    /// Record that extraction ran and found nothing, so backfill doesn't
    /// re-run it forever. The marker row (`kind = '_none'`) is filtered out
    /// of every read path.
    pub fn mark_extracted(&self, meeting_id: &str, extracted_by: &str) -> Result<()> {
        self.conn.execute(
            "INSERT INTO meeting_items
             (id, meeting_id, kind, text, owner, status, speaker_key, segment_ids_json,
              created_at, extracted_by)
             VALUES (?1, ?2, '_none', '', NULL, NULL, NULL, '[]', ?3, ?4)",
            (
                fly_core::new_id(),
                meeting_id,
                Utc::now().to_rfc3339(),
                extracted_by,
            ),
        )?;
        Ok(())
    }

    /// Meetings that have a transcript but no extraction yet — the backfill
    /// work list, oldest first so history fills in chronologically.
    pub fn meetings_missing_items(&self, limit: usize) -> Result<Vec<String>> {
        let mut stmt = self.conn.prepare(
            "SELECT t.meeting_id FROM transcripts t
             JOIN meetings m ON m.id = t.meeting_id
             WHERE NOT EXISTS (SELECT 1 FROM meeting_items i WHERE i.meeting_id = t.meeting_id
                                 AND i.extracted_by NOT LIKE 'user:%')
             ORDER BY m.started_at LIMIT ?1",
        )?;
        let rows = stmt.query_map([limit as i64], |r| r.get::<_, String>(0))?;
        Ok(rows.collect::<std::result::Result<_, _>>()?)
    }

    /// All meetings, newest first, optionally only those started at/after
    /// `since`. Powers the MCP context/timeline assembly.
    pub fn list_meetings(
        &self,
        since: Option<DateTime<Utc>>,
        limit: usize,
    ) -> Result<Vec<Meeting>> {
        let mut sql = String::from(
            "SELECT id, title, note_id, attendees_json, started_at, ended_at, recording_json, attendees_confirmed
             FROM meetings",
        );
        if since.is_some() {
            sql.push_str(" WHERE started_at >= ?1");
        }
        sql.push_str(" ORDER BY started_at DESC LIMIT ?2");
        let mut stmt = self.conn.prepare(&sql)?;
        let map = crate::meetings::row_to_meeting;
        let rows = match since {
            Some(s) => stmt.query_map(rusqlite::params![s.to_rfc3339(), limit as i64], map)?,
            None => {
                // keep the ?2 placeholder count consistent
                sql = sql.replace("?2", "?1");
                let mut stmt2 = self.conn.prepare(&sql)?;
                let rows = stmt2.query_map(rusqlite::params![limit as i64], map)?;
                return Ok(rows.collect::<std::result::Result<_, _>>()?);
            }
        };
        Ok(rows.collect::<std::result::Result<_, _>>()?)
    }
}

/// `Ok(None)` for the `_none` extraction marker row — filtered from reads.
fn row_to_item(r: &rusqlite::Row<'_>) -> rusqlite::Result<Option<MeetingItem>> {
    let kind_str: String = r.get(2)?;
    let Some(kind) = ItemKind::parse(&kind_str) else {
        return Ok(None);
    };
    let segment_ids: Vec<String> =
        serde_json::from_str(&r.get::<_, String>(7)?).unwrap_or_default();
    Ok(Some(MeetingItem {
        id: r.get(0)?,
        meeting_id: r.get(1)?,
        kind,
        text: r.get(3)?,
        owner: r.get(4)?,
        status: r.get(5)?,
        speaker_key: r.get(6)?,
        segment_ids,
        created_at: crate::folders::parse_ts(r.get::<_, String>(8)?),
        extracted_by: r.get(9)?,
    }))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_storage;

    fn item(meeting_id: &str, kind: ItemKind, text: &str, owner: Option<&str>) -> MeetingItem {
        MeetingItem {
            id: fly_core::new_id(),
            meeting_id: meeting_id.into(),
            kind,
            text: text.into(),
            owner: owner.map(String::from),
            status: (kind == ItemKind::ActionItem).then(|| "open".to_string()),
            speaker_key: Some("mic".into()),
            segment_ids: vec!["s1".into(), "s2".into()],
            created_at: Utc::now(),
            extracted_by: "mock".into(),
        }
    }

    fn meeting(s: &Storage, title: &str) -> String {
        let note = s.create_note(title, None).unwrap();
        s.create_meeting(title, &note.id, &[]).unwrap().id
    }

    #[test]
    fn user_items_survive_reextraction_and_dont_count_as_extracted() {
        let (_d, s) = test_storage();
        let m = meeting(&s, "Sync");
        let mut mine = item(&m, ItemKind::ActionItem, "Send the draft", Some("Dana"));
        assert!(
            s.add_meeting_item(&mine).is_err(),
            "must be marked as a user item"
        );
        mine.extracted_by = format!("{USER_ITEM_PREFIX}ask");
        s.add_meeting_item(&mine).unwrap();
        assert!(!s.has_meeting_items(&m).unwrap(), "extraction hasn't run");
        assert_eq!(
            s.meetings_missing_items(10).unwrap().len(),
            0,
            "no transcript yet"
        );
        s.replace_meeting_items(&m, &[item(&m, ItemKind::Decision, "Ship it", None)])
            .unwrap();
        let all = s.get_meeting_items(&m).unwrap();
        assert_eq!(all.len(), 2, "re-extraction kept the user's item");
        assert!(s.has_meeting_items(&m).unwrap());
        s.replace_meeting_items(&m, &[]).unwrap();
        assert_eq!(s.get_meeting_items(&m).unwrap()[0].text, "Send the draft");
    }

    #[test]
    fn relabelling_a_speaker_renames_item_owners() {
        let (_d, s) = test_storage();
        let m = meeting(&s, "Sync");
        s.save_transcript(&fly_core::Transcript {
            meeting_id: m.clone(),
            language: None,
            engine: "test".into(),
            segments: vec![fly_core::TranscriptSegment {
                id: "s1".into(),
                speaker_key: "spk_1".into(),
                start_ms: 0,
                end_ms: 1000,
                text: "I'll send it.".into(),
                words: vec![],
            }],
            speakers: vec![fly_core::Speaker {
                key: "spk_1".into(),
                label: "Speaker 2".into(),
            }],
        })
        .unwrap();
        s.replace_meeting_items(
            &m,
            &[
                item(&m, ItemKind::ActionItem, "Send it", Some("speaker 2")),
                item(&m, ItemKind::ActionItem, "Review it", Some("Dana")),
            ],
        )
        .unwrap();
        s.relabel_speaker(&m, "spk_1", "Priya").unwrap();
        let owners: Vec<Option<String>> = s
            .get_meeting_items(&m)
            .unwrap()
            .into_iter()
            .map(|i| i.owner)
            .collect();
        assert_eq!(owners, vec![Some("Priya".into()), Some("Dana".into())]);
    }

    #[test]
    fn items_roundtrip_with_provenance() {
        let (_d, s) = test_storage();
        let m = meeting(&s, "Weekly sync");
        s.replace_meeting_items(
            &m,
            &[
                item(&m, ItemKind::Decision, "ship v2 next week", None),
                item(&m, ItemKind::ActionItem, "send the SOW", Some("Dana")),
            ],
        )
        .unwrap();
        let items = s.get_meeting_items(&m).unwrap();
        assert_eq!(items.len(), 2);
        assert_eq!(items[0].kind, ItemKind::Decision);
        assert_eq!(items[0].segment_ids, vec!["s1", "s2"]);
        assert_eq!(items[1].owner.as_deref(), Some("Dana"));
        assert!(s.has_meeting_items(&m).unwrap());

        // re-extraction replaces, never duplicates
        s.replace_meeting_items(&m, &[item(&m, ItemKind::Figure, "ARR $2.4M", None)])
            .unwrap();
        let items = s.get_meeting_items(&m).unwrap();
        assert_eq!(items.len(), 1);
        assert_eq!(items[0].kind, ItemKind::Figure);
    }

    #[test]
    fn query_items_filters_kind_owner_status() {
        let (_d, s) = test_storage();
        let m1 = meeting(&s, "Sync A");
        let m2 = meeting(&s, "Sync B");
        s.replace_meeting_items(
            &m1,
            &[
                item(&m1, ItemKind::ActionItem, "send deck", Some("Dana")),
                item(&m1, ItemKind::Decision, "go with plan B", None),
            ],
        )
        .unwrap();
        s.replace_meeting_items(
            &m2,
            &[item(&m2, ItemKind::ActionItem, "book venue", Some("Lee"))],
        )
        .unwrap();

        let actions = s
            .query_items(&ItemFilter {
                kind: Some(ItemKind::ActionItem),
                ..Default::default()
            })
            .unwrap();
        assert_eq!(actions.len(), 2);

        let danas = s
            .query_items(&ItemFilter {
                owner: Some("dana".into()),
                ..Default::default()
            })
            .unwrap();
        assert_eq!(danas.len(), 1);
        assert_eq!(danas[0].text, "send deck");

        let scoped = s
            .query_items(&ItemFilter {
                meeting_ids: Some(vec![m2.clone()]),
                ..Default::default()
            })
            .unwrap();
        assert_eq!(scoped.len(), 1);
        assert_eq!(scoped[0].meeting_id, m2);
    }

    #[test]
    fn query_items_can_exclude_keep_local_before_the_limit() {
        let (_d, s) = test_storage();
        let visible = meeting(&s, "Open sync");
        let flagged = meeting(&s, "Board");
        let folder = s.create_folder("Legal", None).unwrap();
        let sub = s.create_folder("Cases", Some(&folder.id)).unwrap();
        let filed_note = s.create_note("Deposition", Some(&sub.id)).unwrap();
        let filed = s
            .create_meeting("Deposition", &filed_note.id, &[])
            .unwrap()
            .id;
        for m in [&visible, &flagged, &filed] {
            s.replace_meeting_items(m, &[item(m, ItemKind::Decision, m, None)])
                .unwrap();
        }
        s.set_note_keep_local(&s.get_meeting(&flagged).unwrap().note_id, true)
            .unwrap();
        s.set_folder_keep_local(&folder.id, true).unwrap();

        let all = s.query_items(&ItemFilter::default()).unwrap();
        assert_eq!(all.len(), 3);
        let shared = s
            .query_items(&ItemFilter {
                exclude_keep_local: true,
                limit: 1,
                ..Default::default()
            })
            .unwrap();
        assert_eq!(shared.len(), 1, "filtered before LIMIT");
        assert_eq!(shared[0].meeting_id, visible);
    }

    #[test]
    fn empty_extraction_marker_hides_from_reads_but_counts_as_done() {
        let (_d, s) = test_storage();
        let m = meeting(&s, "Quiet meeting");
        s.mark_extracted(&m, "mock").unwrap();
        assert!(s.get_meeting_items(&m).unwrap().is_empty());
        assert!(s.has_meeting_items(&m).unwrap());
        assert!(s.query_items(&ItemFilter::default()).unwrap().is_empty());
    }

    #[test]
    fn missing_items_backfill_list_is_transcribed_meetings_only() {
        let (_d, s) = test_storage();
        let m1 = meeting(&s, "Has transcript");
        let _m2 = meeting(&s, "No transcript");
        s.save_transcript(&fly_core::Transcript {
            meeting_id: m1.clone(),
            language: None,
            engine: "whisper.cpp".into(),
            segments: vec![],
            speakers: vec![],
        })
        .unwrap();
        assert_eq!(s.meetings_missing_items(10).unwrap(), vec![m1.clone()]);
        s.mark_extracted(&m1, "mock").unwrap();
        assert!(s.meetings_missing_items(10).unwrap().is_empty());
    }
}
