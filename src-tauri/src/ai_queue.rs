//! The queue for the post-transcription AI pass (transcript cleanup, then item
//! extraction).
//!
//! The pass is enrichment: it must never hold up transcription. The
//! transcription worker (`scheduler::spawn`) only QUEUES a pass here and goes
//! straight on to the next job; a second worker drains this queue one pass at
//! a time. That keeps the rule "one foreground model task from this path at a
//! time", and it means a slow or runaway model costs the AI pass its own time
//! and nobody else's: the next meeting is transcribed meanwhile.
//!
//! The queue itself is in memory; the scheduler writes what is in it (waiting
//! and running) to a settings row after every change and reads it back at
//! startup (`scheduler::recover_ai_passes`), so a quit or restart loses no pass.

use std::collections::VecDeque;
use std::sync::Mutex;

use serde::{Deserialize, Serialize};
use tokio::sync::Notify;

/// Why a pass was queued; decides what it does and what happens when it ends.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AiJobKind {
    /// Straight after a transcription: cleanup, then extraction. When it ends
    /// the transcript is final, so the semantic index, recording compression
    /// and the import's video copy are nudged too.
    AfterTranscription,
    /// "Review and allow": the pass again for a meeting that was skipped for
    /// missing cloud consent. The transcript was already final.
    ConsentRerun,
    /// Extraction alone, after the speakers changed (re-analyze, undo): the
    /// transcript text is untouched, so no cleanup, and the caller has
    /// already told the app it's done.
    ExtractOnly,
}

impl AiJobKind {
    /// What a pass of this kind covers: a meeting queued twice keeps the
    /// fuller one.
    fn rank(self) -> u8 {
        match self {
            AiJobKind::ExtractOnly => 0,
            AiJobKind::ConsentRerun => 1,
            AiJobKind::AfterTranscription => 2,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AiJob {
    pub meeting_id: String,
    pub kind: AiJobKind,
}

#[derive(Default)]
struct Inner {
    pending: VecDeque<AiJob>,
    /// The pass the worker is running right now.
    running: Option<AiJob>,
}

#[derive(Default)]
pub struct AiQueue {
    inner: Mutex<Inner>,
    /// A job was queued. The worker also polls, so a missed nudge only
    /// delays a pass.
    pub wake: Notify,
    /// The running pass ended: what a transcription waits on when it needs
    /// the same meeting (see [`AiQueue::wait_for_pass_end`]).
    finished: Notify,
}

/// The pass the worker is running; ends it when dropped.
pub struct RunningPass<'a>(&'a AiQueue);

impl Drop for RunningPass<'_> {
    fn drop(&mut self) {
        self.0.finish();
    }
}

impl AiQueue {
    /// Queue a pass. One entry per meeting: a meeting already waiting keeps
    /// its place, upgraded to the fuller kind. Returns whether it was added.
    pub fn push(&self, job: AiJob) -> bool {
        let mut inner = self.inner.lock().unwrap();
        if let Some(queued) = inner
            .pending
            .iter_mut()
            .find(|j| j.meeting_id == job.meeting_id)
        {
            if job.kind.rank() > queued.kind.rank() {
                queued.kind = job.kind;
            }
            return false;
        }
        inner.pending.push_back(job);
        true
    }

    /// Take the oldest job and mark it running until the guard drops (also
    /// when a panic unwinds through the worker).
    pub fn next(&self) -> Option<(AiJob, RunningPass<'_>)> {
        let mut inner = self.inner.lock().unwrap();
        let job = inner.pending.pop_front()?;
        inner.running = Some(job.clone());
        Some((job, RunningPass(self)))
    }

    fn finish(&self) {
        self.inner.lock().unwrap().running = None;
        self.finished.notify_one();
    }

    /// Drop a job that hasn't started (its meeting is being deleted,
    /// cancelled or transcribed again). A running pass is not touched.
    pub fn remove_pending(&self, meeting_id: &str) -> bool {
        let mut inner = self.inner.lock().unwrap();
        let before = inner.pending.len();
        inner.pending.retain(|j| j.meeting_id != meeting_id);
        inner.pending.len() != before
    }

    pub fn is_pending(&self, meeting_id: &str) -> bool {
        let inner = self.inner.lock().unwrap();
        inner.pending.iter().any(|j| j.meeting_id == meeting_id)
    }

    pub fn is_running(&self, meeting_id: &str) -> bool {
        let inner = self.inner.lock().unwrap();
        inner
            .running
            .as_ref()
            .is_some_and(|j| j.meeting_id == meeting_id)
    }

    /// Every pass not yet finished, the running one first: what survives a
    /// restart.
    pub fn snapshot(&self) -> Vec<AiJob> {
        let inner = self.inner.lock().unwrap();
        inner
            .running
            .iter()
            .chain(inner.pending.iter())
            .cloned()
            .collect()
    }

    /// Wait until no pass is running for `meeting_id`. A pass reads and
    /// rewrites the meeting's transcript, so a transcription of the same
    /// meeting starts after it, not under it. Rare: it takes a re-transcribe
    /// within the minutes a pass lasts. `poll` bounds a missed wake-up.
    pub async fn wait_for_pass_end(&self, meeting_id: &str, poll: std::time::Duration) {
        while self.is_running(meeting_id) {
            let _ = tokio::time::timeout(poll, self.finished.notified()).await;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn job(id: &str, kind: AiJobKind) -> AiJob {
        AiJob {
            meeting_id: id.into(),
            kind,
        }
    }

    #[test]
    fn jobs_run_oldest_first_one_at_a_time() {
        let q = AiQueue::default();
        assert!(q.push(job("a", AiJobKind::AfterTranscription)));
        assert!(q.push(job("b", AiJobKind::AfterTranscription)));
        let (first, running) = q.next().unwrap();
        assert_eq!(first.meeting_id, "a");
        assert!(q.is_running("a") && !q.is_running("b"));
        assert!(q.is_pending("b"));
        drop(running);
        assert!(!q.is_running("a"));
        let (second, running) = q.next().unwrap();
        assert_eq!(second.meeting_id, "b");
        drop(running);
        assert!(q.next().is_none());
    }

    #[test]
    fn a_meeting_queues_once_and_keeps_the_fuller_kind() {
        let q = AiQueue::default();
        assert!(q.push(job("a", AiJobKind::ExtractOnly)));
        assert!(!q.push(job("a", AiJobKind::ConsentRerun)));
        assert!(!q.push(job("a", AiJobKind::AfterTranscription)));
        // never downgraded
        assert!(!q.push(job("a", AiJobKind::ExtractOnly)));
        let (a, running) = q.next().unwrap();
        assert_eq!(a.kind, AiJobKind::AfterTranscription);
        drop(running);
        assert!(q.push(job("b", AiJobKind::AfterTranscription)));
        assert!(!q.push(job("b", AiJobKind::ConsentRerun)));
        assert_eq!(q.next().unwrap().0.kind, AiJobKind::AfterTranscription);
    }

    #[test]
    fn only_a_job_that_has_not_started_can_be_removed() {
        let q = AiQueue::default();
        q.push(job("a", AiJobKind::AfterTranscription));
        q.push(job("b", AiJobKind::AfterTranscription));
        let _a = q.next().unwrap(); // a is running
        assert!(!q.remove_pending("a"));
        assert!(q.is_running("a"));
        assert!(q.remove_pending("b"));
        assert!(!q.is_pending("b"));
        assert!(!q.remove_pending("b"));
    }

    /// What survives a restart is everything not finished: the pass that was
    /// running (it is run again from the start) and the ones waiting.
    #[test]
    fn the_snapshot_is_the_running_pass_then_the_waiting_ones() {
        let q = AiQueue::default();
        assert!(q.snapshot().is_empty());
        q.push(job("a", AiJobKind::AfterTranscription));
        q.push(job("b", AiJobKind::ExtractOnly));
        q.push(job("c", AiJobKind::ConsentRerun));
        let (_, running) = q.next().unwrap();
        let ids: Vec<_> = q.snapshot().into_iter().map(|j| j.meeting_id).collect();
        assert_eq!(ids, ["a", "b", "c"]);
        drop(running);
        let ids: Vec<_> = q.snapshot().into_iter().map(|j| j.meeting_id).collect();
        assert_eq!(ids, ["b", "c"]);

        // and it round-trips through the settings row
        let json = serde_json::to_string(&q.snapshot()).unwrap();
        assert!(json.contains("extract_only") && json.contains("consent_rerun"));
        let back: Vec<AiJob> = serde_json::from_str(&json).unwrap();
        assert_eq!(back, q.snapshot());
    }

    #[tokio::test]
    async fn waiting_for_a_pass_ends_when_it_finishes() {
        let q = std::sync::Arc::new(AiQueue::default());
        // nothing running: returns at once
        tokio::time::timeout(
            std::time::Duration::from_millis(200),
            q.wait_for_pass_end("a", std::time::Duration::from_secs(30)),
        )
        .await
        .expect("no pass running");

        q.push(job("a", AiJobKind::AfterTranscription));
        let (_, running) = q.next().unwrap();
        let waiter = {
            let q = q.clone();
            tokio::spawn(async move {
                q.wait_for_pass_end("a", std::time::Duration::from_secs(30))
                    .await
            })
        };
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;
        assert!(!waiter.is_finished(), "pass still running");
        // another meeting's pass is none of its business
        assert!(!q.is_running("b"));
        drop(running);
        tokio::time::timeout(std::time::Duration::from_secs(5), waiter)
            .await
            .expect("woken by finish, not by the 30 s poll")
            .unwrap();
    }
}
