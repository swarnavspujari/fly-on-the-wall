//! Cloud ASR resilience policy: how one failed chunk upload is classified,
//! whether/when it is retried, and proactive pacing that keeps a whole job
//! inside Groq's free-tier audio-seconds-per-hour quota so long recordings
//! never trade an hour of cloud quota for eleven hours of CPU decoding.
//!
//! Everything here is pure (no clock, no network) so the policy is testable;
//! the engine supplies real time and does the sleeping.

use std::time::Duration;

use crate::AsrError;

/// Groq `on_demand` (free) tier: 7200 seconds of audio per rolling hour.
pub const FREE_TIER_AUDIO_MS_PER_HOUR: u64 = 7_200 * 1000;
/// Headroom under the published quota (other clients, clock skew, the quota
/// window being server-side): pace as if the hour held this much less audio.
pub const QUOTA_SAFETY_MARGIN_MS: u64 = 300 * 1000;
/// The rolling quota window.
pub const QUOTA_WINDOW_MS: u64 = 3_600 * 1000;

/// Bounded backoff for transient network failures of one chunk.
const NETWORK_RETRY_DELAYS: [Duration; 3] = [
    Duration::from_secs(2),
    Duration::from_secs(8),
    Duration::from_secs(30),
];
/// A 429 without a parseable hint waits this long before retrying.
const DEFAULT_RATE_LIMIT_WAIT: Duration = Duration::from_secs(30);
/// Pad on top of the server's hint so the retry lands after the window
/// actually frees (sub-second truncation, clock skew).
const RATE_LIMIT_HINT_PAD: Duration = Duration::from_secs(1);
/// Total rate-limit waiting allowed for ONE chunk before the engine gives up
/// and lets the caller's local fallback rescue the job.
pub const MAX_RATE_LIMIT_WAIT: Duration = Duration::from_secs(20 * 60);

/// Pacer waits longer than this spill the chunk to the local engine instead
/// of sleeping: the validated GPU tier decodes a whole batch in well under
/// half a minute, so any longer idle wait is pure lost time; anything shorter
/// and the cloud upload wins on both speed and CPU.
pub const SPILL_TO_LOCAL_THRESHOLD_MS: u64 = 30_000;

/// Whether a quota-paced chunk should be decoded by the injected local
/// engine instead of waiting `wait_ms` for the cloud window to reopen.
pub fn spill_to_local(wait_ms: u64, has_local: bool) -> bool {
    has_local && wait_ms > SPILL_TO_LOCAL_THRESHOLD_MS
}

/// Map one failed HTTP chunk upload to the error the retry policy consumes.
/// 429 is transient — the quota window reopens — so it must NOT map to
/// [`AsrError::Rejected`], which the app's scheduler treats as permanent.
pub fn classify_http_failure(
    status: u16,
    retry_after_header: Option<&str>,
    body: &str,
) -> AsrError {
    let excerpt: String = body.chars().take(300).collect();
    if status == 429 {
        return AsrError::RateLimited {
            message: format!("groq returned 429 Too Many Requests: {excerpt}"),
            retry_after: parse_retry_hint(retry_after_header, body),
        };
    }
    if (400..500).contains(&status) {
        // 4xx (413 payload too large, 401 bad key, …): the identical request
        // can never succeed — callers must not retry it.
        return AsrError::Rejected(format!("groq returned {status}: {excerpt}"));
    }
    AsrError::Engine(format!("groq returned {status}: {excerpt}"))
}

/// How long the server asked us to wait: the `retry-after` header (seconds),
/// else the "Please try again in 1m43.68s" phrase Groq puts in 429 bodies.
pub fn parse_retry_hint(header: Option<&str>, body: &str) -> Option<Duration> {
    if let Some(h) = header {
        if let Ok(secs) = h.trim().parse::<f64>() {
            if secs >= 0.0 {
                return Some(Duration::from_secs_f64(secs));
            }
        }
    }
    let after = body.split("try again in ").nth(1)?;
    let token: String = after
        .chars()
        .take_while(|c| c.is_ascii_digit() || matches!(c, '.' | 'h' | 'm' | 's'))
        .collect();
    parse_go_duration(token.trim_end_matches('.'))
}

/// Parse Go-style durations ("27s", "1m43.68s", "250ms", "1h2m").
fn parse_go_duration(s: &str) -> Option<Duration> {
    let mut total = Duration::ZERO;
    let mut num = String::new();
    let mut chars = s.chars().peekable();
    let mut any = false;
    while let Some(c) = chars.next() {
        if c.is_ascii_digit() || c == '.' {
            num.push(c);
            continue;
        }
        let unit_secs = match c {
            'h' => 3600.0,
            'm' if chars.peek() == Some(&'s') => {
                chars.next();
                0.001
            }
            'm' => 60.0,
            's' => 1.0,
            _ => return None,
        };
        let value: f64 = num.parse().ok()?;
        total += Duration::from_secs_f64(value * unit_secs);
        num.clear();
        any = true;
    }
    (any && num.is_empty()).then_some(total)
}

/// Whether (and how long) to wait before retrying a failed chunk.
/// `network_attempts` counts network failures already retried for this chunk;
/// `rate_limited_waited` is the total rate-limit sleeping already spent on it.
pub fn retry_delay(
    err: &AsrError,
    network_attempts: u32,
    rate_limited_waited: Duration,
) -> Option<Duration> {
    match err {
        AsrError::Network(_) => NETWORK_RETRY_DELAYS.get(network_attempts as usize).copied(),
        AsrError::RateLimited { retry_after, .. } => {
            if rate_limited_waited >= MAX_RATE_LIMIT_WAIT {
                return None;
            }
            let wait = retry_after.unwrap_or(DEFAULT_RATE_LIMIT_WAIT) + RATE_LIMIT_HINT_PAD;
            Some(wait.min(MAX_RATE_LIMIT_WAIT - rate_limited_waited))
        }
        _ => None,
    }
}

/// Rolling-window audio budget: uploads are delayed so the audio submitted in
/// any trailing hour stays under the free-tier quota (minus safety margin).
/// Timestamps are caller-supplied milliseconds (monotonic), keeping the pacer
/// clock-free and testable.
pub struct QuotaPacer {
    window_ms: u64,
    budget_ms: u64,
    /// (submitted_at_ms, audio_ms) — pruned as entries age out of the window.
    submitted: Vec<(u64, u64)>,
}

impl Default for QuotaPacer {
    fn default() -> Self {
        Self::new(
            QUOTA_WINDOW_MS,
            FREE_TIER_AUDIO_MS_PER_HOUR - QUOTA_SAFETY_MARGIN_MS,
        )
    }
}

impl QuotaPacer {
    pub fn new(window_ms: u64, budget_ms: u64) -> Self {
        Self {
            window_ms,
            budget_ms,
            submitted: Vec::new(),
        }
    }

    /// Milliseconds to wait before `audio_ms` of audio may be uploaded at
    /// `now_ms` without breaching the budget. 0 = go now. A chunk bigger than
    /// the whole budget can never fit — don't wait forever, upload and let
    /// the server's verdict decide.
    pub fn wait_ms(&self, now_ms: u64, audio_ms: u64) -> u64 {
        if audio_ms > self.budget_ms {
            return 0;
        }
        let mut in_window: Vec<&(u64, u64)> = self
            .submitted
            .iter()
            .filter(|(at, _)| at + self.window_ms > now_ms)
            .collect();
        in_window.sort_by_key(|(at, _)| *at);
        let mut used: u64 = in_window.iter().map(|(_, a)| a).sum();
        let mut wait = 0u64;
        for (at, a) in in_window {
            if used + audio_ms <= self.budget_ms {
                break;
            }
            // budget frees when this entry ages out of the window
            wait = (at + self.window_ms).saturating_sub(now_ms);
            used -= a;
        }
        wait
    }

    /// Record an upload that was actually submitted.
    pub fn record(&mut self, now_ms: u64, audio_ms: u64) {
        self.submitted
            .retain(|(at, _)| at + self.window_ms > now_ms);
        self.submitted.push((now_ms, audio_ms));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn status_429_is_rate_limited_not_rejected() {
        let body = r#"{"error":{"message":"Rate limit reached ... Please try again in 27s."}}"#;
        let err = classify_http_failure(429, None, body);
        match &err {
            AsrError::RateLimited {
                retry_after,
                message,
            } => {
                assert_eq!(*retry_after, Some(Duration::from_secs(27)));
                assert!(message.contains("429"));
            }
            other => panic!("429 must be RateLimited, got {other:?}"),
        }
        // the scheduler marks Rejected-marker errors permanent; a rate limit
        // must never read as one
        assert!(!err.to_string().contains(crate::REJECTED_MARKER));
    }

    #[test]
    fn hard_4xx_stays_rejected_and_5xx_stays_engine() {
        assert!(matches!(
            classify_http_failure(413, None, "too large"),
            AsrError::Rejected(_)
        ));
        assert!(matches!(
            classify_http_failure(401, None, "bad key"),
            AsrError::Rejected(_)
        ));
        assert!(matches!(
            classify_http_failure(500, None, "boom"),
            AsrError::Engine(_)
        ));
    }

    #[test]
    fn retry_hints_parse_from_header_and_groq_body_phrases() {
        assert_eq!(
            parse_retry_hint(Some("27"), ""),
            Some(Duration::from_secs(27))
        );
        // header wins over the body
        assert_eq!(
            parse_retry_hint(Some("5"), "try again in 1m"),
            Some(Duration::from_secs(5))
        );
        assert_eq!(
            parse_retry_hint(None, "Please try again in 1m43.68s. Need more?"),
            Some(Duration::from_secs_f64(103.68))
        );
        assert_eq!(
            parse_retry_hint(None, "Please try again in 250ms."),
            Some(Duration::from_millis(250))
        );
        assert_eq!(parse_retry_hint(None, "no hint here"), None);
    }

    #[test]
    fn network_failures_back_off_three_times_then_give_up() {
        let err = AsrError::Network("connection reset".into());
        assert_eq!(
            retry_delay(&err, 0, Duration::ZERO),
            Some(Duration::from_secs(2))
        );
        assert_eq!(
            retry_delay(&err, 1, Duration::ZERO),
            Some(Duration::from_secs(8))
        );
        assert_eq!(
            retry_delay(&err, 2, Duration::ZERO),
            Some(Duration::from_secs(30))
        );
        assert_eq!(retry_delay(&err, 3, Duration::ZERO), None);
    }

    #[test]
    fn rate_limits_wait_the_hinted_time_with_a_bounded_total() {
        let limited = |hint: Option<Duration>| AsrError::RateLimited {
            message: "429".into(),
            retry_after: hint,
        };
        // hint honored (plus a pad so the retry lands after the window frees)
        assert_eq!(
            retry_delay(&limited(Some(Duration::from_secs(27))), 0, Duration::ZERO),
            Some(Duration::from_secs(28))
        );
        // no hint → default wait
        assert_eq!(
            retry_delay(&limited(None), 0, Duration::ZERO),
            Some(Duration::from_secs(31))
        );
        // budget nearly spent → the remaining budget caps the wait
        let waited = MAX_RATE_LIMIT_WAIT - Duration::from_secs(10);
        assert_eq!(
            retry_delay(&limited(Some(Duration::from_secs(120))), 0, waited),
            Some(Duration::from_secs(10))
        );
        // budget spent → give up (the local fallback takes over)
        assert_eq!(retry_delay(&limited(None), 0, MAX_RATE_LIMIT_WAIT), None);
        // network attempts don't consume the rate-limit budget and vice versa
        assert_eq!(
            retry_delay(&limited(None), 3, Duration::ZERO),
            Some(Duration::from_secs(31))
        );
    }

    #[test]
    fn rejected_and_engine_errors_never_retry() {
        assert_eq!(
            retry_delay(&AsrError::Rejected("413".into()), 0, Duration::ZERO),
            None
        );
        assert_eq!(
            retry_delay(&AsrError::Engine("500".into()), 0, Duration::ZERO),
            None
        );
    }

    /// The hybrid routing policy: short pacer waits still sleep (the upload
    /// is cheaper), long waits hand the chunk to the local engine — but only
    /// when a local engine actually exists below the cloud tier.
    #[test]
    fn short_waits_sleep_and_long_waits_spill_to_local() {
        assert!(!spill_to_local(0, true), "no wait — upload now");
        assert!(
            !spill_to_local(SPILL_TO_LOCAL_THRESHOLD_MS, true),
            "at the threshold waiting still wins"
        );
        assert!(spill_to_local(SPILL_TO_LOCAL_THRESHOLD_MS + 1, true));
        assert!(
            !spill_to_local(u64::MAX, false),
            "nothing local to spill to — wait no matter how long"
        );
    }

    #[test]
    fn pacer_lets_uploads_through_until_the_budget_is_reached() {
        // 100s window, 60s audio budget
        let mut p = QuotaPacer::new(100_000, 60_000);
        assert_eq!(p.wait_ms(0, 30_000), 0);
        p.record(0, 30_000);
        assert_eq!(p.wait_ms(1_000, 30_000), 0);
        p.record(1_000, 30_000);
        // budget full: the next 30s chunk must wait until the first entry
        // (at 0ms) ages out of the 100s window
        assert_eq!(p.wait_ms(2_000, 30_000), 98_000);
        // ...and if both must age out for a bigger chunk, wait for the second
        assert_eq!(p.wait_ms(2_000, 60_000), 99_000);
        // once the window has passed, uploads flow again
        assert_eq!(p.wait_ms(101_000, 30_000), 0);
    }

    #[test]
    fn pacer_never_deadlocks_on_a_chunk_bigger_than_the_whole_budget() {
        let p = QuotaPacer::new(100_000, 60_000);
        assert_eq!(p.wait_ms(0, 70_000), 0, "can never fit — don't wait");
    }

    /// The default pacer models the real free tier: a 586-minute job (the
    /// incident) submits ~115 min of audio then waits, instead of burning the
    /// quota in minutes and losing the whole job to a 429.
    #[test]
    fn default_pacer_keeps_a_marathon_job_under_the_hourly_quota() {
        let mut p = QuotaPacer::default();
        let chunk_ms = 559_000; // observed real chunk size
        let mut now = 0u64;
        let mut submitted_this_window = 0u64;
        for _ in 0..12 {
            let wait = p.wait_ms(now, chunk_ms);
            now += wait;
            if wait > 0 {
                submitted_this_window = 0;
            }
            p.record(now, chunk_ms);
            submitted_this_window += chunk_ms;
            assert!(
                submitted_this_window <= FREE_TIER_AUDIO_MS_PER_HOUR - QUOTA_SAFETY_MARGIN_MS,
                "window budget breached"
            );
            now += 1_000; // upload + transcription turnaround
        }
    }
}
