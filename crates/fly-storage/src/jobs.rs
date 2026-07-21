//! Transcription job queue: pending pipeline runs persisted so back-to-back
//! meetings transcribe after recording ends and queued work survives an app
//! restart. A job stores ONLY the meeting id — recording paths are resolved
//! from the meeting's `recording_json` at execution time, so jobs stay valid
//! if recording files are later moved/renamed.

use chrono::Utc;
use rusqlite::OptionalExtension;
use serde::Serialize;

use crate::{Result, Storage};

pub const JOB_QUEUED: &str = "queued";
pub const JOB_RUNNING: &str = "running";
pub const JOB_DONE: &str = "done";
pub const JOB_FAILED: &str = "failed";

#[derive(Debug, Clone, Serialize)]
pub struct TranscriptionJob {
    pub meeting_id: String,
    pub status: String,
    pub attempts: u32,
    pub last_error: Option<String>,
}

impl Storage {
    /// Queue a meeting for transcription. Idempotent: a job already queued or
    /// running is left untouched; a done/failed job is reset and re-queued
    /// (the user asked again). Returns whether the job is now (re)queued.
    pub fn enqueue_transcription(&self, meeting_id: &str) -> Result<bool> {
        let now = Utc::now().to_rfc3339();
        let n = self.conn.execute(
            "INSERT INTO transcription_jobs (meeting_id, status, attempts, last_error, created_at, updated_at)
             VALUES (?1, 'queued', 0, NULL, ?2, ?2)
             ON CONFLICT(meeting_id) DO UPDATE
                SET status = 'queued', attempts = 0, last_error = NULL, updated_at = ?2
                WHERE status IN ('done', 'failed')",
            (meeting_id, &now),
        )?;
        Ok(n > 0)
    }

    /// Oldest queued job (FIFO by last state change, so a retried job goes to
    /// the back of the queue).
    pub fn next_transcription_job(&self) -> Result<Option<TranscriptionJob>> {
        Ok(self
            .conn
            .query_row(
                "SELECT meeting_id, status, attempts, last_error FROM transcription_jobs
                 WHERE status = 'queued' ORDER BY updated_at, rowid LIMIT 1",
                [],
                row_to_job,
            )
            .optional()?)
    }

    pub fn transcription_job(&self, meeting_id: &str) -> Result<Option<TranscriptionJob>> {
        Ok(self
            .conn
            .query_row(
                "SELECT meeting_id, status, attempts, last_error FROM transcription_jobs
                 WHERE meeting_id = ?1",
                [meeting_id],
                row_to_job,
            )
            .optional()?)
    }

    pub fn queued_transcription_ids(&self) -> Result<Vec<String>> {
        let mut stmt = self.conn.prepare(
            "SELECT meeting_id FROM transcription_jobs
             WHERE status = 'queued' ORDER BY updated_at, rowid",
        )?;
        let ids = stmt
            .query_map([], |r| r.get(0))?
            .collect::<std::result::Result<_, _>>()?;
        Ok(ids)
    }

    pub fn mark_transcription_running(&self, meeting_id: &str) -> Result<()> {
        self.set_job(meeting_id, JOB_RUNNING, None, None)
    }

    pub fn mark_transcription_done(&self, meeting_id: &str) -> Result<()> {
        self.set_job(meeting_id, JOB_DONE, None, None)
    }

    /// Failed attempt but retries remain: back to the queue with the error
    /// recorded and the attempt counted.
    pub fn requeue_transcription(
        &self,
        meeting_id: &str,
        attempts: u32,
        error: &str,
    ) -> Result<()> {
        self.set_job(meeting_id, JOB_QUEUED, Some(attempts), Some(error))
    }

    /// Out of retries: keep the job (and its error) visible, stop trying.
    pub fn mark_transcription_failed(
        &self,
        meeting_id: &str,
        attempts: u32,
        error: &str,
    ) -> Result<()> {
        self.set_job(meeting_id, JOB_FAILED, Some(attempts), Some(error))
    }

    /// Remove a meeting's job row entirely (transcription cancelled or the
    /// note deleted). Unlike the failed state there is nothing left to show:
    /// the user asked for the work to stop. Missing rows are a no-op.
    pub fn delete_transcription_job(&self, meeting_id: &str) -> Result<()> {
        self.conn.execute(
            "DELETE FROM transcription_jobs WHERE meeting_id = ?1",
            [meeting_id],
        )?;
        Ok(())
    }

    /// Startup recovery: anything left 'running' by a previous process died
    /// with it — put it back in the queue.
    pub fn reset_running_transcriptions(&self) -> Result<usize> {
        let now = Utc::now().to_rfc3339();
        Ok(self.conn.execute(
            "UPDATE transcription_jobs SET status = 'queued', updated_at = ?1
             WHERE status = 'running'",
            [now],
        )?)
    }

    fn set_job(
        &self,
        meeting_id: &str,
        status: &str,
        attempts: Option<u32>,
        error: Option<&str>,
    ) -> Result<()> {
        let now = Utc::now().to_rfc3339();
        self.conn.execute(
            "UPDATE transcription_jobs
             SET status = ?2,
                 attempts = COALESCE(?3, attempts),
                 last_error = ?4,
                 updated_at = ?5
             WHERE meeting_id = ?1",
            (meeting_id, status, attempts, error, now),
        )?;
        Ok(())
    }
}

fn row_to_job(r: &rusqlite::Row<'_>) -> rusqlite::Result<TranscriptionJob> {
    Ok(TranscriptionJob {
        meeting_id: r.get(0)?,
        status: r.get(1)?,
        attempts: r.get(2)?,
        last_error: r.get(3)?,
    })
}

#[cfg(test)]
mod tests {
    use crate::test_storage;

    #[test]
    fn enqueue_is_idempotent_while_pending() {
        let (_dir, s) = test_storage();
        assert!(s.enqueue_transcription("m1").unwrap());
        // already queued → no reset
        assert!(!s.enqueue_transcription("m1").unwrap());
        s.mark_transcription_running("m1").unwrap();
        assert!(!s.enqueue_transcription("m1").unwrap());
        // finished → asking again re-queues
        s.mark_transcription_done("m1").unwrap();
        assert!(s.enqueue_transcription("m1").unwrap());
    }

    #[test]
    fn fifo_order_and_retry_goes_to_the_back() {
        let (_dir, s) = test_storage();
        s.enqueue_transcription("m1").unwrap();
        s.enqueue_transcription("m2").unwrap();
        assert_eq!(
            s.next_transcription_job().unwrap().unwrap().meeting_id,
            "m1"
        );

        s.mark_transcription_running("m1").unwrap();
        s.requeue_transcription("m1", 1, "boom").unwrap();
        // m1 was retried → m2 now goes first
        assert_eq!(
            s.next_transcription_job().unwrap().unwrap().meeting_id,
            "m2"
        );
        assert_eq!(s.queued_transcription_ids().unwrap(), vec!["m2", "m1"]);

        let m1 = s.transcription_job("m1").unwrap().unwrap();
        assert_eq!(m1.attempts, 1);
        assert_eq!(m1.last_error.as_deref(), Some("boom"));
    }

    #[test]
    fn failed_jobs_stay_visible_but_not_schedulable() {
        let (_dir, s) = test_storage();
        s.enqueue_transcription("m1").unwrap();
        s.mark_transcription_running("m1").unwrap();
        s.mark_transcription_failed("m1", 3, "no recording files")
            .unwrap();
        assert!(s.next_transcription_job().unwrap().is_none());
        let job = s.transcription_job("m1").unwrap().unwrap();
        assert_eq!(job.status, "failed");
        assert_eq!(job.attempts, 3);
        // user asks again → failure resets
        assert!(s.enqueue_transcription("m1").unwrap());
        assert_eq!(s.transcription_job("m1").unwrap().unwrap().attempts, 0);
    }

    #[test]
    fn delete_removes_the_job_row_entirely() {
        let (_dir, s) = test_storage();
        s.enqueue_transcription("m1").unwrap();
        s.delete_transcription_job("m1").unwrap();
        assert!(s.transcription_job("m1").unwrap().is_none());
        assert!(s.next_transcription_job().unwrap().is_none());
        // a running job's row goes too (cancel of an in-flight run)...
        s.enqueue_transcription("m2").unwrap();
        s.mark_transcription_running("m2").unwrap();
        s.delete_transcription_job("m2").unwrap();
        assert!(s.transcription_job("m2").unwrap().is_none());
        // ...and deleting a missing job is a no-op, not an error
        s.delete_transcription_job("m2").unwrap();
    }

    #[test]
    fn restart_recovers_running_jobs() {
        let (_dir, s) = test_storage();
        s.enqueue_transcription("m1").unwrap();
        s.mark_transcription_running("m1").unwrap();
        assert!(s.next_transcription_job().unwrap().is_none());
        assert_eq!(s.reset_running_transcriptions().unwrap(), 1);
        assert_eq!(
            s.next_transcription_job().unwrap().unwrap().meeting_id,
            "m1"
        );
    }
}
