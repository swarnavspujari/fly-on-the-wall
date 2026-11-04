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
//!
//! A pass can be stopped from outside: the one running is cancelled when its
//! meeting is transcribed again or deleted, and `halt` stops them all (and
//! refuses new ones) for "Delete all my data".

use std::collections::VecDeque;
use std::sync::{Arc, Mutex};

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
    /// Times a pass for this entry has started. Saved with the queue, so a
    /// pass that takes the app down every time it runs is given up on after
    /// a couple of launches (`scheduler::recover_ai_passes`). Rows saved
    /// before this existed read as 0.
    #[serde(default)]
    pub attempts: u32,
}

impl AiJob {
    pub fn new(meeting_id: impl Into<String>, kind: AiJobKind) -> Self {
        Self {
            meeting_id: meeting_id.into(),
            kind,
            attempts: 0,
        }
    }
}

#[derive(Default)]
struct Inner {
    pending: VecDeque<AiJob>,
    /// The pass the worker is running right now, and how to stop it.
    running: Option<(AiJob, Arc<Notify>)>,
    /// No pass starts or is accepted while set ("Delete all my data").
    halted: bool,
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
pub struct RunningPass<'a> {
    queue: &'a AiQueue,
    cancel: Arc<Notify>,
}

impl RunningPass<'_> {
    /// Completes once the pass has been cancelled (also when that happened
    /// before this was first polled). The worker races it against the pass
    /// and drops the pass's future, which closes its model request.
    pub async fn cancelled(&self) {
        self.cancel.notified().await;
    }
}

impl Drop for RunningPass<'_> {
    fn drop(&mut self) {
        self.queue.finish();
    }
}

impl AiQueue {
    /// Queue a pass. One entry per meeting: a meeting already waiting keeps
    /// its place, upgraded to the fuller kind. Returns whether it was added
    /// (not when it was there already, or the queue is halted).
    pub fn push(&self, job: AiJob) -> bool {
        let mut inner = self.inner.lock().unwrap();
        if inner.halted {
            return false;
        }
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
    /// when a panic unwinds through the worker). Counts the attempt: the
    /// returned job (and the snapshot from here on) has `attempts` one higher.
    pub fn next(&self) -> Option<(AiJob, RunningPass<'_>)> {
        let mut inner = self.inner.lock().unwrap();
        if inner.halted {
            return None;
        }
        let mut job = inner.pending.pop_front()?;
        job.attempts = job.attempts.saturating_add(1);
        let cancel = Arc::new(Notify::new());
        inner.running = Some((job.clone(), cancel.clone()));
        Some((
            job,
            RunningPass {
                queue: self,
                cancel,
            },
        ))
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

    /// Stop the pass running for `meeting_id`, if there is one: its meeting is
    /// being transcribed again (the transcript it works on is about to be
    /// replaced) or deleted. The worker drops the pass at its next await.
    /// Returns whether one was running.
    pub fn cancel_running(&self, meeting_id: &str) -> bool {
        let inner = self.inner.lock().unwrap();
        match &inner.running {
            Some((job, cancel)) if job.meeting_id == meeting_id => {
                cancel.notify_one();
                true
            }
            _ => false,
        }
    }

    /// Stop everything: nothing waiting, nothing new accepted or started, and
    /// the running pass cancelled. Undone by [`AiQueue::resume`].
    pub fn halt(&self) {
        let mut inner = self.inner.lock().unwrap();
        inner.halted = true;
        inner.pending.clear();
        if let Some((_, cancel)) = &inner.running {
            cancel.notify_one();
        }
    }

    /// Undo [`AiQueue::halt`] (the wipe was refused or failed).
    pub fn resume(&self) {
        self.inner.lock().unwrap().halted = false;
        self.wake.notify_one();
    }

    pub fn is_halted(&self) -> bool {
        self.inner.lock().unwrap().halted
    }

    /// Whether a pass is running (any meeting's).
    pub fn is_busy(&self) -> bool {
        self.inner.lock().unwrap().running.is_some()
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
            .is_some_and(|(job, _)| job.meeting_id == meeting_id)
    }

    /// Every pass not yet finished, the running one first: what survives a
    /// restart.
    pub fn snapshot(&self) -> Vec<AiJob> {
        let inner = self.inner.lock().unwrap();
        inner
            .running
            .iter()
            .map(|(job, _)| job)
            .chain(inner.pending.iter())
            .cloned()
            .collect()
    }

    /// Wait until no pass is running for `meeting_id`. A pass reads and
    /// rewrites the meeting's transcript, so a transcription of the same
    /// meeting starts after it, not under it (the scheduler cancels the pass
    /// first, so this is the time the pass takes to wind down). `poll` bounds
    /// a missed wake-up.
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
        AiJob::new(id, kind)
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

    /// Starting a pass is an attempt, and the saved queue says so from that
    /// moment on: an app that dies inside the pass leaves the count behind.
    /// A row saved before the count existed reads as no attempts.
    #[test]
    fn a_started_pass_counts_an_attempt_in_the_snapshot() {
        let q = AiQueue::default();
        q.push(job("a", AiJobKind::AfterTranscription));
        q.push(job("b", AiJobKind::AfterTranscription));
        assert_eq!(q.snapshot()[0].attempts, 0);
        let (started, running) = q.next().unwrap();
        assert_eq!(started.attempts, 1);
        let saved = q.snapshot();
        assert_eq!((saved[0].attempts, saved[1].attempts), (1, 0));
        drop(running);

        // a requeued entry keeps what it had
        q.push(AiJob {
            attempts: 1,
            ..job("c", AiJobKind::AfterTranscription)
        });
        let _ = q.next().unwrap(); // b
        assert_eq!(q.next().unwrap().0.attempts, 2);

        let old: Vec<AiJob> =
            serde_json::from_str(r#"[{"meeting_id":"x","kind":"after_transcription"}]"#).unwrap();
        assert_eq!(old[0].attempts, 0);
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

    /// Only the pass running for that meeting is stopped; the stop is kept
    /// for a pass that hasn't reached its next await yet.
    #[tokio::test]
    async fn cancelling_stops_only_the_running_pass_of_that_meeting() {
        let q = AiQueue::default();
        assert!(!q.cancel_running("a"), "nothing running");
        q.push(job("a", AiJobKind::AfterTranscription));
        q.push(job("b", AiJobKind::AfterTranscription));
        let (_, running) = q.next().unwrap();

        assert!(!q.cancel_running("b"), "b is only waiting");
        let still_going =
            tokio::time::timeout(std::time::Duration::from_millis(50), running.cancelled()).await;
        assert!(still_going.is_err(), "not cancelled by b's request");

        // cancelled before the worker looks: it still hears of it
        assert!(q.cancel_running("a"));
        tokio::time::timeout(std::time::Duration::from_secs(2), running.cancelled())
            .await
            .expect("cancelled");
        drop(running);

        // and the next pass starts clean
        let (_, next) = q.next().unwrap();
        let fresh =
            tokio::time::timeout(std::time::Duration::from_millis(50), next.cancelled()).await;
        assert!(fresh.is_err(), "the earlier cancel doesn't carry over");
    }

    /// "Delete all my data": nothing waits, nothing new is accepted, the
    /// running pass is told to stop; resuming undoes it.
    #[tokio::test]
    async fn halting_clears_the_queue_and_refuses_passes_until_resumed() {
        let q = AiQueue::default();
        q.push(job("a", AiJobKind::AfterTranscription));
        q.push(job("b", AiJobKind::AfterTranscription));
        let (_, running) = q.next().unwrap();
        assert!(q.is_busy());

        q.halt();
        assert!(q.is_halted());
        assert!(!q.is_pending("b"), "waiting passes are dropped");
        tokio::time::timeout(std::time::Duration::from_secs(2), running.cancelled())
            .await
            .expect("the running pass is told to stop");
        drop(running);
        assert!(!q.is_busy());
        assert!(!q.push(job("c", AiJobKind::AfterTranscription)), "refused");
        assert!(q.next().is_none());
        assert!(q.snapshot().is_empty());

        q.resume();
        assert!(q.push(job("c", AiJobKind::AfterTranscription)));
        assert!(q.next().is_some());
    }
}
