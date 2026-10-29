//! Capture-timestamp gap classification, kept pure so every rule is tested.
//!
//! Each WASAPI packet carries the capture time of its first frame. A packet
//! that starts later than the previous one ended means frames are missing
//! from the device's timeline. The capture fills such a gap with the same
//! length of silence (so the channels stay aligned) and decides whether it
//! was **lost** audio or **idle** output:
//! - mic: always lost (a microphone never stops delivering);
//! - loopback: idle when the callback simply waited the gap out (loopback
//!   delivers nothing while nothing plays) *and* either the output before it
//!   was silent or the gap is long. Meeting apps render continuous silence
//!   during a call, so a short gap right after audible output is lost audio;
//!   a long one is a paused video or a call that ended mid-word;
//! - either: lost, never idle, when this app's buffer was nearly full just
//!   before (this app fell behind).
//!
//! A timestamp that can't be right — zero (cpal passes WASAPI's
//! TIMESTAMP_ERROR packets through), or a jump the recording clock can't
//! account for — is a **resync**: re-anchor on the packet, insert nothing,
//! count nothing. Only the silence actually inserted counts as lost.

/// Which kind of channel a tracker follows.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum ChannelKind {
    Mic,
    Loopback,
}

/// What to put on the channel's timeline before this packet's audio.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Gap {
    /// Contiguous, or nothing to compare with yet.
    None,
    /// Insert `frames` of silence. `lost`: audio went missing (else idle
    /// output). `upstream`: lost while this app's buffer wasn't near full —
    /// the OS audio engine or driver dropped it.
    Fill {
        frames: u64,
        lost: bool,
        upstream: bool,
    },
    /// The timestamp can't be right: re-anchored, nothing inserted.
    Resync,
}

/// One packet as the callback sees it.
#[derive(Clone, Copy, Debug)]
pub(crate) struct Packet {
    /// Capture time of the first frame (QPC ns).
    pub capture_ns: i128,
    /// cpal's `callback` timestamp (WASAPI: the device position's QPC time).
    pub callback_ns: i128,
    /// How long the packet waited in the OS buffer, when measured.
    pub backlog_ns: Option<i128>,
    pub frames: u64,
    /// The recording clock's resume epoch (a pause jumps the timestamps).
    pub epoch: u64,
    /// How many frames the recording clock lets this channel grow by before
    /// this packet's own audio (its expected length + slack − what it has).
    pub room: u64,
}

/// Smaller timestamp jumps are clock jitter, not a dropped packet (one
/// WASAPI period is 10 ms).
pub(crate) const GAP_MIN_NS: i128 = 5_000_000;
/// About -60 dBFS: a packet this quiet counts as silence.
pub(crate) const SILENT_PEAK: i16 = 33;
/// A waited-out loopback gap this long is idle output whatever came before
/// it: the audio engine doesn't stall that long.
pub(crate) const IDLE_LONG_NS: i128 = 1_500_000_000;
/// How long after a near-full buffer a loss is still blamed on this app.
pub(crate) const BACKLOG_BLAME_NS: i128 = 1_000_000_000;
/// A backlog above this is implausible (a bad timestamp), not measured.
pub(crate) const BACKLOG_SANE_NS: i128 = 10_000_000_000;

pub(crate) struct GapTracker {
    kind: ChannelKind,
    rate: u32,
    /// The OS buffer's length: a real gap can exceed the clock's room by at
    /// most this much (audio that was still buffered when the clock moved on).
    buffer_ns: i128,
    /// Backlog at or above this means the buffer was nearly full.
    high_water_ns: i128,
    epoch: u64,
    /// Expected capture time of the next packet's first frame.
    next_capture_ns: Option<i128>,
    last_callback_ns: i128,
    last_backlog_high_ns: Option<i128>,
    /// Peak |sample| of the previous packet.
    prev_peak: i16,
}

impl GapTracker {
    pub(crate) fn new(
        kind: ChannelKind,
        rate: u32,
        buffer_ns: i128,
        high_water_ns: i128,
        epoch: u64,
    ) -> Self {
        Self {
            kind,
            rate: rate.max(1),
            buffer_ns,
            high_water_ns,
            epoch,
            next_capture_ns: None,
            last_callback_ns: 0,
            last_backlog_high_ns: None,
            prev_peak: 0,
        }
    }

    fn frames_for(&self, ns: i128) -> u64 {
        (ns.max(0) * self.rate as i128 / 1_000_000_000) as u64
    }

    /// Classify the gap (if any) before `p`, and move past it.
    pub(crate) fn observe(&mut self, p: &Packet) -> Gap {
        if p.epoch != self.epoch {
            // resumed after a pause: the jump is the pause
            self.epoch = p.epoch;
            self.next_capture_ns = None;
        }
        if let Some(b) = p.backlog_ns {
            if (0..BACKLOG_SANE_NS).contains(&b) && b >= self.high_water_ns {
                self.last_backlog_high_ns = Some(p.callback_ns);
            }
        }
        if p.capture_ns <= 0 {
            self.next_capture_ns = None;
            return Gap::Resync;
        }
        let verdict = match self.next_capture_ns {
            None => Gap::None,
            Some(next) => {
                let gap_ns = p.capture_ns - next;
                if gap_ns <= GAP_MIN_NS {
                    Gap::None
                } else {
                    let frames = self.frames_for(gap_ns);
                    if frames > p.room + self.frames_for(self.buffer_ns) {
                        Gap::Resync
                    } else {
                        let ours = self
                            .last_backlog_high_ns
                            .is_some_and(|at| p.callback_ns - at < BACKLOG_BLAME_NS);
                        let waited_ns = p.callback_ns - self.last_callback_ns;
                        let idle = self.kind == ChannelKind::Loopback
                            && !ours
                            && waited_ns * 2 >= gap_ns
                            && (self.prev_peak <= SILENT_PEAK || gap_ns >= IDLE_LONG_NS);
                        Gap::Fill {
                            frames: frames.min(p.room),
                            lost: !idle,
                            upstream: !idle && !ours,
                        }
                    }
                }
            }
        };
        self.next_capture_ns =
            Some(p.capture_ns + p.frames as i128 * 1_000_000_000 / self.rate as i128);
        self.last_callback_ns = p.callback_ns;
        verdict
    }

    /// Record the packet's loudness once its audio has been read.
    pub(crate) fn end_packet(&mut self, peak: i16) {
        self.prev_peak = peak;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const RATE: u32 = 48_000;
    const MS: i128 = 1_000_000;
    const BUFFER: i128 = 400 * MS;
    const HIGH: i128 = 300 * MS;
    const BASE: i128 = 1_000_000 * MS; // QPC is never near zero in practice

    /// Feed packets of 10 ms (480 frames) and return the verdicts.
    struct Run {
        t: GapTracker,
        /// capture time of the next packet if nothing is missing
        cap: i128,
        /// device time the callback reads it at
        cb: i128,
    }

    impl Run {
        fn new(kind: ChannelKind) -> Self {
            let mut r = Self {
                t: GapTracker::new(kind, RATE, BUFFER, HIGH, 0),
                cap: BASE,
                cb: BASE + 10 * MS,
            };
            // three contiguous audible packets to anchor on
            for _ in 0..3 {
                assert_eq!(r.packet(1000), Gap::None);
            }
            r
        }

        fn packet_with(&mut self, peak: i16, backlog: Option<i128>, epoch: u64, room: u64) -> Gap {
            let g = self.t.observe(&Packet {
                capture_ns: self.cap,
                callback_ns: self.cb,
                backlog_ns: backlog,
                frames: 480,
                epoch,
                room,
            });
            self.t.end_packet(peak);
            self.cap += 10 * MS;
            self.cb += 10 * MS;
            g
        }

        fn packet(&mut self, peak: i16) -> Gap {
            self.packet_with(peak, Some(10 * MS), 0, 10 * RATE as u64)
        }

        /// Frames go missing: the next packet starts `ms` later; the
        /// callback ran on time (`waited` false) or waited the gap out.
        fn skip(&mut self, ms: i128, waited: bool) {
            self.cap += ms * MS;
            if waited {
                self.cb += ms * MS;
            }
        }
    }

    fn lost(frames: u64, upstream: bool) -> Gap {
        Gap::Fill {
            frames,
            lost: true,
            upstream,
        }
    }

    fn idle(frames: u64) -> Gap {
        Gap::Fill {
            frames,
            lost: false,
            upstream: false,
        }
    }

    #[test]
    fn contiguous_packets_have_no_gap() {
        let mut r = Run::new(ChannelKind::Mic);
        for _ in 0..20 {
            assert_eq!(r.packet(1000), Gap::None);
        }
        // jitter under 5 ms is not a gap
        r.skip(3, false);
        assert_eq!(r.packet(1000), Gap::None);
    }

    #[test]
    fn a_mic_gap_is_always_lost_and_filled_exactly() {
        let mut r = Run::new(ChannelKind::Mic);
        r.skip(100, false);
        assert_eq!(r.packet(1000), lost(4800, true));
        // even when the callback waited it out (the engine stalled)
        let mut r = Run::new(ChannelKind::Mic);
        r.skip(300, true);
        assert_eq!(r.packet(0), lost(14_400, true));
    }

    #[test]
    fn loopback_waiting_after_silence_is_idle() {
        let mut r = Run::new(ChannelKind::Loopback);
        r.packet(0); // the output went silent
        r.skip(2_000, true);
        assert_eq!(r.packet(1000), idle(96_000));
    }

    #[test]
    fn loopback_short_gap_after_audible_output_is_lost() {
        let mut r = Run::new(ChannelKind::Loopback);
        r.skip(300, true); // audio engine stalled mid-sound
        assert_eq!(r.packet(1000), lost(14_400, true));
        // and when the callback was on time it's lost too
        let mut r = Run::new(ChannelKind::Loopback);
        r.skip(40, false);
        assert_eq!(r.packet(1000), lost(1_920, true));
    }

    #[test]
    fn loopback_long_gap_after_audible_output_is_idle() {
        // a paused video, a call torn down mid-word: minutes later, sound
        let mut r = Run::new(ChannelKind::Loopback);
        r.skip(300_000, true);
        let g = r.packet_with(1000, Some(10 * MS), 0, 400 * RATE as u64);
        assert_eq!(g, idle(14_400_000));
    }

    #[test]
    fn a_nearly_full_buffer_makes_it_ours_even_on_loopback() {
        let mut r = Run::new(ChannelKind::Loopback);
        r.packet(0); // silence before: would be idle by every other rule
                     // this app froze: the next packets had sat in the buffer ~400 ms
        r.skip(600, true);
        let g = r.packet_with(1000, Some(395 * MS), 0, 10 * RATE as u64);
        assert_eq!(g, lost(28_800, false));
        // a mic loss right after a near-full buffer is ours too
        let mut r = Run::new(ChannelKind::Mic);
        assert_eq!(r.packet_with(1000, Some(350 * MS), 0, 480_000), Gap::None);
        r.skip(50, false);
        assert_eq!(r.packet(1000), lost(2_400, false));
    }

    #[test]
    fn a_pause_resets_the_timeline() {
        let mut r = Run::new(ChannelKind::Mic);
        r.skip(5_000, true); // paused for 5 s
        assert_eq!(r.packet_with(1000, Some(10 * MS), 1, 480_000), Gap::None);
        assert_eq!(r.packet_with(1000, Some(10 * MS), 1, 480_000), Gap::None);
    }

    #[test]
    fn bogus_timestamps_resync_without_inserting_anything() {
        // a TIMESTAMP_ERROR packet reads as capture time 0
        let mut r = Run::new(ChannelKind::Mic);
        let g = r.t.observe(&Packet {
            capture_ns: 0,
            callback_ns: r.cb,
            backlog_ns: None,
            frames: 480,
            epoch: 0,
            room: 480_000,
        });
        assert_eq!(g, Gap::Resync);
        // the next good packet re-anchors: no "uptime-sized" gap
        r.skip(10, false);
        assert_eq!(r.packet(1000), Gap::None);
        assert_eq!(r.packet(1000), Gap::None);

        // a jump the clock can't allow (an hour, with 0.5 s of room) too
        let mut r = Run::new(ChannelKind::Mic);
        r.skip(3_600_000, false);
        assert_eq!(r.packet_with(1000, None, 0, RATE as u64 / 2), Gap::Resync);
        assert_eq!(r.packet(1000), Gap::None);
    }

    #[test]
    fn only_what_the_clock_allows_is_filled() {
        // 600 ms missing, the clock has room for 450 ms (within one buffer
        // length of slack): fill — and count — 450 ms, not 600
        let mut r = Run::new(ChannelKind::Mic);
        r.skip(600, false);
        let room = 450 * RATE as u64 / 1000;
        assert_eq!(r.packet_with(1000, None, 0, room), lost(room, true));
    }
}
