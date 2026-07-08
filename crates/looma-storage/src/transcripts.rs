//! Transcript persistence: structured JSON in SQLite (+ FTS) and portable
//! markdown/JSON mirrors inside the meeting's folder (`transcript.md`,
//! `transcript.json`), so one folder holds a meeting's audio + transcript.

use looma_core::Transcript;
use rusqlite::OptionalExtension;

use crate::{Result, Storage, StorageError};

impl Storage {
    /// Upsert a meeting's transcript; keeps FTS and on-disk mirrors in sync.
    pub fn save_transcript(&self, transcript: &Transcript) -> Result<()> {
        let json = serde_json::to_string(transcript)?;
        self.conn.execute(
            "INSERT INTO transcripts (meeting_id, json) VALUES (?1, ?2)
             ON CONFLICT(meeting_id) DO UPDATE SET json = excluded.json",
            (&transcript.meeting_id, &json),
        )?;
        self.sync_transcript_derived(transcript)?;
        Ok(())
    }

    pub fn get_transcript(&self, meeting_id: &str) -> Result<Option<Transcript>> {
        let json: Option<String> = self
            .conn
            .query_row(
                "SELECT json FROM transcripts WHERE meeting_id = ?1",
                [meeting_id],
                |r| r.get(0),
            )
            .optional()?;
        match json {
            Some(j) => Ok(Some(serde_json::from_str(&j)?)),
            None => Ok(None),
        }
    }

    /// Rename a speaker's display label (stable keys never change).
    pub fn relabel_speaker(
        &self,
        meeting_id: &str,
        speaker_key: &str,
        label: &str,
    ) -> Result<Transcript> {
        let mut transcript = self
            .get_transcript(meeting_id)?
            .ok_or_else(|| StorageError::NotFound(format!("transcript for {meeting_id}")))?;
        match transcript
            .speakers
            .iter_mut()
            .find(|s| s.key == speaker_key)
        {
            Some(s) => s.label = label.to_string(),
            None => transcript.speakers.push(looma_core::Speaker {
                key: speaker_key.to_string(),
                label: label.to_string(),
            }),
        }
        self.save_transcript(&transcript)?;
        Ok(transcript)
    }

    fn sync_transcript_derived(&self, t: &Transcript) -> Result<()> {
        // FTS body: labeled lines, so a search for a speaker name hits too.
        let body: String = t
            .segments
            .iter()
            .map(|s| format!("{}: {}", t.label_for(&s.speaker_key), s.text))
            .collect::<Vec<_>>()
            .join("\n");
        self.conn.execute(
            "DELETE FROM transcripts_fts WHERE meeting_id = ?1",
            [&t.meeting_id],
        )?;
        self.conn.execute(
            "INSERT INTO transcripts_fts (meeting_id, body) VALUES (?1, ?2)",
            (&t.meeting_id, &body),
        )?;

        // Mirrors live next to the recording; a meeting without a resolvable
        // folder (no recording attached, folder gone) falls back to the
        // legacy top-level transcripts/ dir.
        let meeting_dir = self
            .get_meeting(&t.meeting_id)
            .ok()
            .and_then(|m| m.recording)
            .and_then(|r| crate::meetings::recording_dir_rel(&r))
            .map(|rel| self.data_dir.join(rel))
            .filter(|d| d.is_dir());
        let (md, json) = match meeting_dir {
            Some(dir) => (dir.join("transcript.md"), dir.join("transcript.json")),
            None => {
                let dir = self.data_dir.join("transcripts");
                (
                    dir.join(format!("{}.md", t.meeting_id)),
                    dir.join(format!("{}.json", t.meeting_id)),
                )
            }
        };
        std::fs::write(md, t.to_markdown())?;
        std::fs::write(json, serde_json::to_string_pretty(t)?)?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use crate::test_storage;
    use looma_core::{Speaker, Transcript, TranscriptSegment, Word};

    fn sample_transcript(meeting_id: &str) -> Transcript {
        Transcript {
            meeting_id: meeting_id.into(),
            language: Some("en".into()),
            engine: "whisper.cpp".into(),
            segments: vec![TranscriptSegment {
                id: "seg-1".into(),
                speaker_key: "spk_0".into(),
                start_ms: 0,
                end_ms: 1500,
                text: "the budget is approved".into(),
                words: vec![Word {
                    text: "budget".into(),
                    start_ms: 200,
                    end_ms: 600,
                }],
            }],
            speakers: vec![Speaker {
                key: "spk_0".into(),
                label: "Speaker 1".into(),
            }],
        }
    }

    #[test]
    fn transcript_roundtrip_files_and_search() {
        let (dir, s) = test_storage();
        let note = s.create_note("Budget mtg", None).unwrap();
        let meeting = s.create_meeting("Budget mtg", &note.id, &[]).unwrap();
        let t = sample_transcript(&meeting.id);
        s.save_transcript(&t).unwrap();

        let loaded = s.get_transcript(&meeting.id).unwrap().unwrap();
        assert_eq!(loaded.segments[0].text, "the budget is approved");

        // markdown + json mirrors exist
        assert!(dir
            .path()
            .join("transcripts")
            .join(format!("{}.md", meeting.id))
            .exists());
        assert!(dir
            .path()
            .join("transcripts")
            .join(format!("{}.json", meeting.id))
            .exists());

        // transcript content is searchable and resolves to the note
        let hits = s.search("budget", 10).unwrap();
        assert!(hits
            .iter()
            .any(|h| h.kind == crate::SearchHitKind::Transcript && h.note_id == note.id));
    }

    #[test]
    fn relabel_updates_label_and_markdown() {
        let (dir, s) = test_storage();
        let note = s.create_note("m", None).unwrap();
        let meeting = s.create_meeting("m", &note.id, &[]).unwrap();
        s.save_transcript(&sample_transcript(&meeting.id)).unwrap();

        let updated = s.relabel_speaker(&meeting.id, "spk_0", "Dana").unwrap();
        assert_eq!(updated.speakers[0].label, "Dana");
        let md = std::fs::read_to_string(
            dir.path()
                .join("transcripts")
                .join(format!("{}.md", meeting.id)),
        )
        .unwrap();
        assert!(md.contains("**Dana:**"));
    }

    /// A meeting with a recording keeps its transcript mirrors inside its
    /// folder — one folder = audio + transcript.
    #[test]
    fn mirrors_live_in_the_meeting_folder() {
        let (dir, s) = test_storage();
        let note = s.create_note("Budget mtg", None).unwrap();
        let meeting = s.create_meeting("Budget mtg", &note.id, &[]).unwrap();
        let rec_dir = s
            .allocate_meeting_dir("Budget mtg", meeting.started_at)
            .unwrap();
        std::fs::write(rec_dir.join("recording.mixed.wav"), b"RIFF").unwrap();
        let rel = format!(
            "recordings/{}/recording.mixed.wav",
            rec_dir.file_name().unwrap().to_string_lossy()
        );
        s.end_meeting(
            &meeting.id,
            &looma_core::RecordingRef {
                mic_path: None,
                system_path: None,
                mixed_path: Some(rel),
                duration_ms: 1000,
            },
        )
        .unwrap();

        s.save_transcript(&sample_transcript(&meeting.id)).unwrap();
        assert!(rec_dir.join("transcript.md").exists());
        assert!(rec_dir.join("transcript.json").exists());
        assert!(!dir
            .path()
            .join("transcripts")
            .join(format!("{}.md", meeting.id))
            .exists());
    }
}
