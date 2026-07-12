//! Transcript persistence: structured JSON in SQLite (+ FTS) and portable
//! markdown/JSON mirrors inside the meeting's folder (`transcript.md`,
//! `transcript.json`), so one folder holds a meeting's audio + transcript.

use std::path::PathBuf;

use fly_core::Transcript;
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

    /// Store the LLM-polished transcript variant ALONGSIDE the raw one, in the
    /// raw row's `cleaned_json` column — the raw `json` is never touched, so a
    /// bad or re-run polish can't corrupt the source of truth. Re-runnable:
    /// each call replaces the cleaned variant. Requires the raw transcript to
    /// exist first (the polish pass reads it), and mirrors to
    /// `transcript.cleaned.{md,json}` next to `transcript.{md,json}`.
    ///
    /// The caller is responsible for the provenance contract: this persists
    /// whatever it's given, so `cleaned` must share the raw's segment ids,
    /// speaker keys, and timestamps (see `fly_core::enhance::apply_cleanup`).
    pub fn save_cleaned_transcript(&self, cleaned: &Transcript) -> Result<()> {
        let json = serde_json::to_string(cleaned)?;
        let n = self.conn.execute(
            "UPDATE transcripts SET cleaned_json = ?1 WHERE meeting_id = ?2",
            (&json, &cleaned.meeting_id),
        )?;
        if n == 0 {
            return Err(StorageError::NotFound(format!(
                "raw transcript for {} (transcribe before polishing)",
                cleaned.meeting_id
            )));
        }
        let (md, json_path) = self.transcript_mirror_paths(&cleaned.meeting_id, ".cleaned");
        std::fs::write(md, cleaned.to_markdown())?;
        std::fs::write(json_path, serde_json::to_string_pretty(cleaned)?)?;
        Ok(())
    }

    /// The polished variant if the polish pass has run, else `None` (the raw
    /// transcript is always available via `get_transcript`).
    pub fn get_cleaned_transcript(&self, meeting_id: &str) -> Result<Option<Transcript>> {
        let json: Option<Option<String>> = self
            .conn
            .query_row(
                "SELECT cleaned_json FROM transcripts WHERE meeting_id = ?1",
                [meeting_id],
                |r| r.get(0),
            )
            .optional()?;
        match json.flatten() {
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
            None => transcript.speakers.push(fly_core::Speaker {
                key: speaker_key.to_string(),
                label: label.to_string(),
            }),
        }
        self.save_transcript(&transcript)?;
        Ok(transcript)
    }

    /// Edit a segment's text in place (stable ids never change); re-syncs FTS
    /// and the on-disk markdown/JSON mirrors so an edit survives reload.
    pub fn edit_segment_text(
        &self,
        meeting_id: &str,
        segment_id: &str,
        text: &str,
    ) -> Result<Transcript> {
        let mut transcript = self
            .get_transcript(meeting_id)?
            .ok_or_else(|| StorageError::NotFound(format!("transcript for {meeting_id}")))?;
        let seg = transcript
            .segments
            .iter_mut()
            .find(|s| s.id == segment_id)
            .ok_or_else(|| StorageError::NotFound(format!("segment {segment_id}")))?;
        seg.text = text.to_string();
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

        let (md, json) = self.transcript_mirror_paths(&t.meeting_id, "");
        std::fs::write(md, t.to_markdown())?;
        std::fs::write(json, serde_json::to_string_pretty(t)?)?;
        Ok(())
    }

    /// Resolve the on-disk `(markdown, json)` mirror paths for a transcript
    /// variant. Mirrors live next to the recording; a meeting without a
    /// resolvable folder (no recording attached, folder gone) falls back to
    /// the legacy top-level `transcripts/` dir. `variant` is `""` for the raw
    /// transcript or `".cleaned"` for the polished one — so raw and polished
    /// sit side by side (`transcript.md` / `transcript.cleaned.md`).
    fn transcript_mirror_paths(&self, meeting_id: &str, variant: &str) -> (PathBuf, PathBuf) {
        let meeting_dir = self
            .get_meeting(meeting_id)
            .ok()
            .and_then(|m| m.recording)
            .and_then(|r| crate::meetings::recording_dir_rel(&r))
            .map(|rel| self.data_dir.join(rel))
            .filter(|d| d.is_dir());
        match meeting_dir {
            Some(dir) => (
                dir.join(format!("transcript{variant}.md")),
                dir.join(format!("transcript{variant}.json")),
            ),
            None => {
                let dir = self.data_dir.join("transcripts");
                (
                    dir.join(format!("{meeting_id}{variant}.md")),
                    dir.join(format!("{meeting_id}{variant}.json")),
                )
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use crate::test_storage;
    use fly_core::{Speaker, Transcript, TranscriptSegment, Word};

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

    /// The polished variant is stored alongside the raw one: `get_transcript`
    /// keeps returning the raw text, `get_cleaned_transcript` returns the
    /// polished text, and both mirror files exist side by side.
    #[test]
    fn cleaned_transcript_stored_alongside_raw() {
        let (dir, s) = test_storage();
        let note = s.create_note("Polish mtg", None).unwrap();
        let meeting = s.create_meeting("Polish mtg", &note.id, &[]).unwrap();
        let raw = sample_transcript(&meeting.id);
        s.save_transcript(&raw).unwrap();

        // no polish yet
        assert!(s.get_cleaned_transcript(&meeting.id).unwrap().is_none());

        let mut cleaned = raw.clone();
        cleaned.segments[0].text = "The budget is approved.".into();
        s.save_cleaned_transcript(&cleaned).unwrap();

        // raw is untouched; cleaned is retrievable
        assert_eq!(
            s.get_transcript(&meeting.id).unwrap().unwrap().segments[0].text,
            "the budget is approved"
        );
        assert_eq!(
            s.get_cleaned_transcript(&meeting.id).unwrap().unwrap().segments[0].text,
            "The budget is approved."
        );

        // both mirror files exist side by side (legacy dir: no recording)
        let tdir = dir.path().join("transcripts");
        assert!(tdir.join(format!("{}.md", meeting.id)).exists());
        assert!(tdir.join(format!("{}.cleaned.md", meeting.id)).exists());
        assert!(tdir.join(format!("{}.cleaned.json", meeting.id)).exists());

        // re-running polish replaces the cleaned variant, still not the raw
        let mut cleaned2 = raw.clone();
        cleaned2.segments[0].text = "Budget approved for the year.".into();
        s.save_cleaned_transcript(&cleaned2).unwrap();
        assert_eq!(
            s.get_cleaned_transcript(&meeting.id).unwrap().unwrap().segments[0].text,
            "Budget approved for the year."
        );
        assert_eq!(
            s.get_transcript(&meeting.id).unwrap().unwrap().segments[0].text,
            "the budget is approved"
        );
    }

    /// Polishing before the raw transcript exists is an error, not a silent
    /// insert — the polish pass reads the raw transcript as its source.
    #[test]
    fn cleaned_transcript_requires_raw_first() {
        let (_dir, s) = test_storage();
        let note = s.create_note("m", None).unwrap();
        let meeting = s.create_meeting("m", &note.id, &[]).unwrap();
        let err = s
            .save_cleaned_transcript(&sample_transcript(&meeting.id))
            .unwrap_err();
        assert!(matches!(err, crate::StorageError::NotFound(_)));
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
            &fly_core::RecordingRef {
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
