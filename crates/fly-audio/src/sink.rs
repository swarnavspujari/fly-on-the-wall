//! Real-time-safe channel recording.
//!
//! The capture callback must never wait: no file I/O, no locks, no
//! allocation. Each channel's callback converts its audio to mono i16 and
//! pushes it — plus any gap it detected — into a bounded lock-free ring
//! ([`ChannelProducer`]). One writer thread ([`WriterThread`]) drains every
//! ring into its WAV. The file format is unchanged: mono 16-bit PCM at the
//! device rate, the canonical 44-byte header.
//!
//! Gaps keep the channel's timeline: whatever the reason audio is missing,
//! the writer puts the same number of silent frames in its place, so the
//! channels stay aligned with each other and with the recording clock. Each
//! gap is counted by cause in [`ChannelStats`]:
//! - **lost** — the device delivered a later packet than expected (WASAPI
//!   capture timestamps jump): audio overflowed the OS buffer before the
//!   callback ran, e.g. the capture thread was starved;
//! - **overflow** — the ring was full because the writer could not keep up
//!   (disk stall longer than the ring);
//! - **idle** — the system output went quiet and loopback stopped
//!   delivering packets: nothing was playing, so nothing was lost.
//!
//! Lost + overflow is audio the person will not hear back.

use std::io::BufWriter;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

use serde::{Deserialize, Serialize};

use crate::{AudioError, Result};

/// Frames per ring slot.
const BLOCK: usize = 512;
/// How much audio a channel's ring holds while the writer is stalled.
pub(crate) const RING_SECS: u32 = 30;
/// Writer cadence; also bounds how stale the file is for live captions.
const DRAIN_EVERY: Duration = Duration::from_millis(50);
/// Header + buffer flush cadence: the WAV on disk is valid (sizes current)
/// to within this, so a crash leaves a playable file.
const FLUSH_EVERY: Duration = Duration::from_secs(1);

// Fixed-size ring slots on purpose: boxing the audio would allocate in the
// capture callback.
#[allow(clippy::large_enum_variant)]
enum Block {
    Audio { len: u16, data: [i16; BLOCK] },
    Silence { frames: u64 },
}

/// Per-channel counters, shared lock-free between the callback, the writer
/// thread and the session (live warnings).
#[derive(Default)]
pub(crate) struct ChannelStats {
    /// Device audio frames queued for the file.
    pub captured: AtomicU64,
    /// Frames the device never delivered (capture-timestamp gaps).
    pub lost: AtomicU64,
    pub lost_events: AtomicU64,
    /// Frames dropped because the ring was full.
    pub overflow: AtomicU64,
    pub overflow_events: AtomicU64,
    /// Silence for idle system output / start-up alignment (not a loss).
    pub padded: AtomicU64,
    /// Discontinuities the backend reported (WASAPI flags these on loopback
    /// state changes too, so they are informational, not a loss count).
    pub xruns: AtomicU64,
    /// Frames in the file.
    pub written: AtomicU64,
    /// Recording time (ms) of the most recent loss/overflow, + 1 (0 = none).
    pub last_loss_at_ms: AtomicU64,
    pub write_errors: AtomicU64,
    /// The capture thread got real-time (MMCSS "Pro Audio") scheduling.
    pub boosted: AtomicBool,
    /// Most audio ever waiting in the OS buffer for this app to read (µs).
    pub max_backlog_us: AtomicU64,
    /// Losses that happened while the callback was running on time: audio
    /// the OS audio engine or driver dropped before this app could read it.
    pub upstream_lost_events: AtomicU64,
    /// Capture timestamps that couldn't be right (re-anchored, nothing
    /// inserted).
    pub resyncs: AtomicU64,
    /// The device went away or the default changed: the stream stopped.
    pub device_lost: AtomicBool,
    /// Frames that failed to reach the file (disk full, I/O error).
    pub write_failed: AtomicU64,
    /// Silence still owed to the timeline when the producer closed; the
    /// writer appends it at finalize.
    pub unflushed: AtomicU64,
}

impl ChannelStats {
    pub(crate) fn lost_frames(&self) -> u64 {
        self.lost.load(Ordering::Relaxed) + self.overflow.load(Ordering::Relaxed)
    }

    pub(crate) fn report(&self, rate: u32) -> ChannelReport {
        let ms = |f: u64| f * 1000 / rate.max(1) as u64;
        let last = self.last_loss_at_ms.load(Ordering::Relaxed);
        ChannelReport {
            rate,
            frames_written: self.written.load(Ordering::Relaxed),
            captured_ms: ms(self.captured.load(Ordering::Relaxed)),
            lost_ms: ms(self.lost.load(Ordering::Relaxed)),
            lost_events: self.lost_events.load(Ordering::Relaxed),
            overflow_ms: ms(self.overflow.load(Ordering::Relaxed)),
            overflow_events: self.overflow_events.load(Ordering::Relaxed),
            padded_ms: ms(self.padded.load(Ordering::Relaxed)),
            xruns: self.xruns.load(Ordering::Relaxed),
            last_loss_at_ms: last.checked_sub(1),
            write_errors: self.write_errors.load(Ordering::Relaxed),
            realtime_priority: self.boosted.load(Ordering::Relaxed),
            max_backlog_ms: self.max_backlog_us.load(Ordering::Relaxed) / 1000,
            upstream_lost_events: self.upstream_lost_events.load(Ordering::Relaxed),
            timestamp_resyncs: self.resyncs.load(Ordering::Relaxed),
            device_lost: self.device_lost.load(Ordering::Relaxed),
            write_failed_ms: ms(self.write_failed.load(Ordering::Relaxed)),
        }
    }
}

/// What one channel's capture looked like — stored with the recording
/// (`recording.capture.json` in the meeting folder) and in the app log.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct ChannelReport {
    pub rate: u32,
    pub frames_written: u64,
    pub captured_ms: u64,
    /// Audio the device never delivered (the OS buffer overflowed before the
    /// capture thread ran). Filled with silence in the file.
    pub lost_ms: u64,
    pub lost_events: u64,
    /// Audio dropped because the file writer fell behind. Filled with
    /// silence in the file.
    pub overflow_ms: u64,
    pub overflow_events: u64,
    /// Silence added where nothing was missing: idle system output and
    /// start-of-recording alignment.
    pub padded_ms: u64,
    pub xruns: u64,
    /// Recording time of the most recent loss, if any.
    pub last_loss_at_ms: Option<u64>,
    pub write_errors: u64,
    pub realtime_priority: bool,
    /// Most audio ever waiting in the OS capture buffer for this app to read
    /// (Windows; 0 where not measured). Near the buffer's size (400 ms) the
    /// capture thread fell behind; far below it, any loss happened upstream.
    #[serde(default)]
    pub max_backlog_ms: u64,
    /// Of `lost_events`, the ones with no near-full buffer in the second
    /// before them: dropped upstream (OS audio engine or driver), which no
    /// in-app change can prevent — only report.
    #[serde(default)]
    pub upstream_lost_events: u64,
    /// Capture timestamps that couldn't be right (re-anchored, nothing
    /// inserted or counted).
    #[serde(default)]
    pub timestamp_resyncs: u64,
    /// The device disconnected or the default device changed mid-recording.
    #[serde(default)]
    pub device_lost: bool,
    /// Audio that couldn't be written to disk (full disk, I/O error).
    #[serde(default)]
    pub write_failed_ms: u64,
}

impl ChannelReport {
    pub fn audio_lost_ms(&self) -> u64 {
        self.lost_ms + self.overflow_ms
    }
}

/// The callback side of one channel. Owned by the capture callback; every
/// method is wait-free and allocation-free.
pub(crate) struct ChannelProducer {
    ring: rtrb::Producer<Block>,
    stats: Arc<ChannelStats>,
    /// Frames on the channel's timeline so far (audio + silence, including
    /// frames still in the ring or lost to overflow).
    timeline: u64,
    /// Overflowed frames not yet put back on the timeline as silence.
    owed: u64,
    /// In an overflow episode: until an audio block gets through again, more
    /// dropped blocks are the same event.
    overflowing: bool,
    block: [i16; BLOCK],
    filled: usize,
}

impl Drop for ChannelProducer {
    /// The stream is gone: whatever the ring couldn't take is still owed to
    /// the timeline. The writer appends it at finalize, so the file isn't
    /// short.
    fn drop(&mut self) {
        let mut owed = self.owed;
        if self.filled > 0 {
            // a packet cut short (can't happen between callbacks): its
            // frames are on no timeline yet, so they're lost, not owed
            self.filled = 0;
        }
        if owed > 0
            && self.ring.slots() > 0
            && self.ring.push(Block::Silence { frames: owed }).is_ok()
        {
            owed = 0;
        }
        self.stats.unflushed.fetch_add(owed, Ordering::Relaxed);
    }
}

impl ChannelProducer {
    pub(crate) fn stats(&self) -> &ChannelStats {
        &self.stats
    }

    /// Frames on this channel's timeline (what the file will hold once the
    /// ring drains).
    pub(crate) fn timeline(&self) -> u64 {
        self.timeline
    }

    /// Append one mono frame.
    #[inline]
    pub(crate) fn push(&mut self, sample: i16) {
        self.block[self.filled] = sample;
        self.filled += 1;
        if self.filled == BLOCK {
            self.flush_block();
        }
    }

    /// Hand a partly filled block to the writer (call at the end of every
    /// callback so audio never sits in the producer).
    pub(crate) fn end_packet(&mut self) {
        if self.filled > 0 {
            self.flush_block();
        }
    }

    fn flush_block(&mut self) {
        let len = self.filled;
        self.filled = 0;
        self.settle_owed();
        let frames = len as u64;
        self.timeline += frames;
        if self.owed == 0 && self.ring.slots() > 0 {
            let _ = self.ring.push(Block::Audio {
                len: len as u16,
                data: self.block,
            });
            self.stats.captured.fetch_add(frames, Ordering::Relaxed);
            self.overflowing = false;
        } else {
            if !self.overflowing {
                self.stats.overflow_events.fetch_add(1, Ordering::Relaxed);
                self.overflowing = true;
            }
            self.owed += frames;
            self.stats.overflow.fetch_add(frames, Ordering::Relaxed);
        }
    }

    /// Put silence on the timeline (a detected gap). `lost` says whether it
    /// replaces audio (counted as a loss) or covers idle time.
    pub(crate) fn silence(&mut self, frames: u64, lost: bool, at_ms: u64) {
        if frames == 0 {
            return;
        }
        self.end_packet();
        self.settle_owed();
        self.timeline += frames;
        if lost {
            self.stats.lost.fetch_add(frames, Ordering::Relaxed);
            self.stats.lost_events.fetch_add(1, Ordering::Relaxed);
            self.stats
                .last_loss_at_ms
                .store(at_ms + 1, Ordering::Relaxed);
        } else {
            self.stats.padded.fetch_add(frames, Ordering::Relaxed);
        }
        if self.owed == 0 && self.ring.slots() > 0 {
            let _ = self.ring.push(Block::Silence { frames });
        } else {
            // the writer is behind: the silence joins the owed stretch
            self.owed += frames;
        }
    }

    /// Mark the most recent overflow's time (the callback knows the clock).
    pub(crate) fn note_time(&self, at_ms: u64) {
        if self.owed > 0 {
            self.stats
                .last_loss_at_ms
                .store(at_ms + 1, Ordering::Relaxed);
        }
    }

    /// Once the ring has room again, re-insert the overflowed stretch as
    /// silence so the timeline stays intact.
    fn settle_owed(&mut self) {
        if self.owed > 0 && self.ring.slots() > 0 {
            let _ = self.ring.push(Block::Silence { frames: self.owed });
            self.owed = 0;
        }
    }
}

/// The writer side of one channel.
pub(crate) struct ChannelSink {
    ring: rtrb::Consumer<Block>,
    wav: Option<hound::WavWriter<BufWriter<std::fs::File>>>,
    stats: Arc<ChannelStats>,
    path: PathBuf,
    rate: u32,
    /// Frames to top the file up to at finalize (loopback: the recording's
    /// duration — idle output delivers nothing at the end).
    tail_to: Arc<AtomicU64>,
}

/// Create a channel: the WAV file now (so start-up fails loudly on an
/// unwritable folder), the producer for the callback, the sink for the
/// writer thread.
pub(crate) fn channel(path: PathBuf, rate: u32) -> Result<(ChannelProducer, ChannelSink)> {
    let spec = hound::WavSpec {
        channels: 1,
        sample_rate: rate,
        bits_per_sample: 16,
        sample_format: hound::SampleFormat::Int,
    };
    let file = std::fs::File::create(&path)?;
    let wav = hound::WavWriter::new(BufWriter::with_capacity(1 << 16, file), spec)
        .map_err(|e| AudioError::Backend(e.to_string()))?;
    let slots = (rate as usize * RING_SECS as usize).div_ceil(BLOCK) + 64;
    let (mut tx, mut rx) = rtrb::RingBuffer::new(slots);
    // Touch every slot once now, so the callback's first pass over the ring
    // doesn't take a page fault per 4 KB (slow exactly when memory is tight).
    for _ in 0..slots {
        let _ = tx.push(Block::Silence { frames: 0 });
    }
    while rx.pop().is_ok() {}
    let stats = Arc::new(ChannelStats::default());
    Ok((
        ChannelProducer {
            ring: tx,
            stats: stats.clone(),
            timeline: 0,
            owed: 0,
            overflowing: false,
            block: [0; BLOCK],
            filled: 0,
        },
        ChannelSink {
            ring: rx,
            wav: Some(wav),
            stats,
            path,
            rate,
            tail_to: Arc::new(AtomicU64::new(0)),
        },
    ))
}

impl ChannelSink {
    pub(crate) fn stats(&self) -> Arc<ChannelStats> {
        self.stats.clone()
    }

    pub(crate) fn rate(&self) -> u32 {
        self.rate
    }

    /// Handle for setting the finalize top-up target after the sink moved
    /// into the writer thread.
    pub(crate) fn tail_target(&self) -> Arc<AtomicU64> {
        self.tail_to.clone()
    }

    /// Write everything queued. Returns the frames written.
    fn drain(&mut self) -> u64 {
        let Some(wav) = self.wav.as_mut() else {
            return 0;
        };
        let mut n = 0u64;
        while let Ok(block) = self.ring.pop() {
            let (frames, res) = match block {
                Block::Audio { len, data } => (
                    len as u64,
                    data[..len as usize]
                        .iter()
                        .try_for_each(|s| wav.write_sample(*s)),
                ),
                Block::Silence { frames } => {
                    (frames, (0..frames).try_for_each(|_| wav.write_sample(0i16)))
                }
            };
            if res.is_ok() {
                n += frames;
            } else {
                // the block didn't make it (disk full?): not in the file
                self.stats.write_errors.fetch_add(1, Ordering::Relaxed);
                self.stats.write_failed.fetch_add(frames, Ordering::Relaxed);
            }
        }
        self.stats.written.fetch_add(n, Ordering::Relaxed);
        n
    }

    fn flush(&mut self) {
        if let Some(wav) = self.wav.as_mut() {
            if wav.flush().is_err() {
                self.stats.write_errors.fetch_add(1, Ordering::Relaxed);
            }
        }
    }

    fn finalize(&mut self) -> Result<()> {
        self.drain();
        let Some(mut wav) = self.wav.take() else {
            return Ok(());
        };
        // silence the producer still owed when it closed (the ring was full)
        let owed = self.stats.unflushed.swap(0, Ordering::Relaxed);
        for _ in 0..owed {
            wav.write_sample(0i16)
                .map_err(|e| AudioError::Backend(e.to_string()))?;
        }
        self.stats.written.fetch_add(owed, Ordering::Relaxed);
        let target = self.tail_to.load(Ordering::Relaxed);
        let have = self.stats.written.load(Ordering::Relaxed);
        if target > have {
            for _ in 0..target - have {
                wav.write_sample(0i16)
                    .map_err(|e| AudioError::Backend(e.to_string()))?;
            }
            self.stats
                .padded
                .fetch_add(target - have, Ordering::Relaxed);
            self.stats.written.store(target, Ordering::Relaxed);
        }
        wav.finalize()
            .map_err(|e| AudioError::Backend(e.to_string()))
    }
}

/// One thread draining every channel's ring into its file.
pub(crate) struct WriterThread {
    stop: Arc<AtomicBool>,
    handle: Option<std::thread::JoinHandle<Result<()>>>,
}

impl WriterThread {
    pub(crate) fn spawn(mut sinks: Vec<ChannelSink>) -> Result<Self> {
        let stop = Arc::new(AtomicBool::new(false));
        let flag = stop.clone();
        let handle = std::thread::Builder::new()
            .name("fly-audio-writer".into())
            .spawn(move || {
                let mut last_flush = Instant::now();
                loop {
                    // read the flag BEFORE draining: anything pushed before
                    // stop was set is then guaranteed to be drained
                    let stopping = flag.load(Ordering::Acquire);
                    for s in sinks.iter_mut() {
                        s.drain();
                    }
                    if stopping {
                        break;
                    }
                    if last_flush.elapsed() >= FLUSH_EVERY {
                        for s in sinks.iter_mut() {
                            s.flush();
                        }
                        last_flush = Instant::now();
                    }
                    std::thread::sleep(DRAIN_EVERY);
                }
                let mut first_err = None;
                for s in sinks.iter_mut() {
                    if let Err(e) = s.finalize() {
                        // the file name only: the folder is named after the meeting
                        tracing::error!(
                            file = %s.path.file_name().unwrap_or_default().to_string_lossy(),
                            "finalizing WAV failed: {e}"
                        );
                        first_err.get_or_insert(e);
                    }
                }
                first_err.map_or(Ok(()), Err)
            })
            .map_err(|e| AudioError::Backend(e.to_string()))?;
        Ok(Self {
            stop,
            handle: Some(handle),
        })
    }

    /// Drain what's left, top up, finalize every file. Call only after the
    /// producers stopped (streams dropped).
    pub(crate) fn finish(mut self) -> Result<()> {
        self.stop.store(true, Ordering::Release);
        match self.handle.take().map(|h| h.join()) {
            Some(Ok(res)) => res,
            Some(Err(_)) => Err(AudioError::Backend("WAV writer thread panicked".into())),
            None => Ok(()),
        }
    }
}

impl Drop for WriterThread {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Release);
        if let Some(h) = self.handle.take() {
            let _ = h.join();
        }
    }
}

/// Real-time scheduling for the calling (capture) thread, held for the
/// thread's life: dropping it (with the callback, on the same thread)
/// reverts MMCSS.
pub(crate) struct PriorityGuard {
    raised: bool,
    /// The MMCSS task handle, as an integer so the guard is Send.
    #[cfg(target_os = "windows")]
    mmcss: Option<isize>,
}

impl PriorityGuard {
    pub(crate) fn raised(&self) -> bool {
        self.raised
    }
}

#[cfg(target_os = "windows")]
impl Drop for PriorityGuard {
    fn drop(&mut self) {
        if let Some(h) = self.mmcss.take() {
            use windows::Win32::Foundation::HANDLE;
            use windows::Win32::System::Threading::AvRevertMmThreadCharacteristics;
            let _ = unsafe { AvRevertMmThreadCharacteristics(HANDLE(h as *mut std::ffi::c_void)) };
        }
    }
}

/// Ask Windows to schedule the calling (capture) thread as real-time audio
/// (MMCSS "Pro Audio"); if MMCSS refuses, at least time-critical priority.
/// Without it the capture thread competes at normal priority with
/// transcription and embedding threads, and WASAPI's capture buffer
/// overflows when it is starved. Other OSes: Core Audio's IO threads are
/// already real-time; no-op.
pub(crate) fn boost_capture_thread() -> PriorityGuard {
    #[cfg(target_os = "windows")]
    unsafe {
        use windows::core::w;
        use windows::Win32::System::Threading::{
            AvSetMmThreadCharacteristicsW, GetCurrentThread, SetThreadPriority,
            THREAD_PRIORITY_TIME_CRITICAL,
        };
        let mut task_index = 0u32;
        if let Ok(h) = AvSetMmThreadCharacteristicsW(w!("Pro Audio"), &mut task_index) {
            return PriorityGuard {
                raised: true,
                mmcss: Some(h.0 as isize),
            };
        }
        let raised = SetThreadPriority(GetCurrentThread(), THREAD_PRIORITY_TIME_CRITICAL).is_ok();
        PriorityGuard {
            raised,
            mmcss: None,
        }
    }
    #[cfg(not(target_os = "windows"))]
    {
        PriorityGuard { raised: false }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Wait for `cond`, polling: a loaded CI box can take a while.
    fn eventually(what: &str, mut cond: impl FnMut() -> bool) {
        let deadline = Instant::now() + Duration::from_secs(20);
        while !cond() {
            assert!(Instant::now() < deadline, "timed out waiting for {what}");
            std::thread::sleep(Duration::from_millis(20));
        }
    }

    fn read(path: &std::path::Path) -> Vec<i16> {
        hound::WavReader::open(path)
            .unwrap()
            .samples::<i16>()
            .map(|s| s.unwrap())
            .collect()
    }

    #[test]
    fn audio_and_gaps_reach_the_file_in_order() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("c.wav");
        let (mut tx, sink) = channel(path.clone(), 8_000).unwrap();
        let stats = sink.stats();
        let writer = WriterThread::spawn(vec![sink]).unwrap();
        for i in 0..1000 {
            tx.push(i as i16 + 1);
        }
        tx.silence(300, true, 125);
        for _ in 0..700 {
            tx.push(7);
        }
        tx.silence(50, false, 0);
        tx.end_packet();
        assert_eq!(tx.timeline(), 2050);
        drop(tx);
        writer.finish().unwrap();

        let s = read(&path);
        assert_eq!(s.len(), 2050);
        assert_eq!(s[0], 1);
        assert_eq!(s[999], 1000);
        assert!(s[1000..1300].iter().all(|&v| v == 0));
        assert!(s[1300..2000].iter().all(|&v| v == 7));
        assert!(s[2000..].iter().all(|&v| v == 0));
        let r = stats.report(8_000);
        assert_eq!(r.lost_ms, 37); // 300 frames at 8 kHz
        assert_eq!(r.lost_events, 1);
        assert_eq!(r.last_loss_at_ms, Some(125));
        assert_eq!(r.padded_ms, 6);
        assert_eq!(r.audio_lost_ms(), 37);
    }

    #[test]
    fn overflow_is_counted_and_keeps_the_timeline() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("o.wav");
        // tiny rate → tiny ring: RING_SECS * 100 / 512 + 64 = 70 slots
        let (mut tx, sink) = channel(path.clone(), 100).unwrap();
        let stats = sink.stats();
        // no writer yet: fill the ring past capacity
        let total = 80 * BLOCK;
        for i in 0..total {
            tx.push((i % 100) as i16 + 1);
        }
        tx.note_time(4_000);
        assert!(stats.overflow.load(Ordering::Relaxed) > 0);
        let writer = WriterThread::spawn(vec![sink]).unwrap();
        // the writer empties the ring (70 blocks)
        eventually("the ring to drain", || {
            stats.written.load(Ordering::Relaxed) == (70 * BLOCK) as u64
        });
        // room again: the next block first settles the owed silence
        for _ in 0..BLOCK {
            tx.push(9);
        }
        tx.end_packet();
        drop(tx);
        writer.finish().unwrap();

        let s = read(&path);
        assert_eq!(s.len(), total + BLOCK, "timeline must be intact");
        let r = stats.report(100);
        assert_eq!(r.overflow_events, 1);
        assert_eq!(
            r.overflow_ms * 100 / 1000,
            (10 * BLOCK) as u64,
            "70 slots fit, 10 blocks overflowed"
        );
        assert_eq!(r.last_loss_at_ms, Some(4_000));
        // the overflowed stretch is silence, then the late block's audio
        assert!(s[total - BLOCK..total].iter().all(|&v| v == 0));
        assert!(s[total..].iter().all(|&v| v == 9));
    }

    #[test]
    fn finalize_tops_up_to_the_tail_target() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("t.wav");
        let (mut tx, sink) = channel(path.clone(), 8_000).unwrap();
        let tail = sink.tail_target();
        let writer = WriterThread::spawn(vec![sink]).unwrap();
        for _ in 0..100 {
            tx.push(3);
        }
        tx.end_packet();
        drop(tx);
        tail.store(8_000, Ordering::Relaxed);
        writer.finish().unwrap();
        let s = read(&path);
        assert_eq!(s.len(), 8_000);
        assert!(s[100..].iter().all(|&v| v == 0));
    }

    #[test]
    fn file_is_readable_while_recording() {
        // the live-caption reader goes by file length past a 44-byte header
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("l.wav");
        let (mut tx, sink) = channel(path.clone(), 8_000).unwrap();
        let writer = WriterThread::spawn(vec![sink]).unwrap();
        for _ in 0..16_000 {
            tx.push(5);
        }
        tx.end_packet();
        // the periodic flush puts the audio on disk mid-recording…
        eventually("the audio to reach the file", || {
            std::fs::metadata(&path).is_ok_and(|m| m.len() == 44 + 32_000)
        });
        // …with the header already current (crash-safe)
        eventually("the header to catch up", || {
            hound::WavReader::open(&path).is_ok_and(|r| r.len() == 16_000)
        });
        assert!(read(&path).iter().all(|&v| v == 5));
        drop(tx);
        writer.finish().unwrap();
    }
    #[test]
    fn silence_still_owed_at_close_reaches_the_file() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("c.wav");
        let (mut tx, sink) = channel(path.clone(), 100).unwrap(); // 70 slots
        let stats = sink.stats();
        let total = 80 * BLOCK;
        for _ in 0..total {
            tx.push(4);
        }
        // the stream stops while the ring is still full: 10 blocks owed
        drop(tx);
        assert_eq!(stats.unflushed.load(Ordering::Relaxed), (10 * BLOCK) as u64);
        WriterThread::spawn(vec![sink]).unwrap().finish().unwrap();
        let s = read(&path);
        assert_eq!(s.len(), total, "the file isn't short");
        assert!(s[70 * BLOCK..].iter().all(|&v| v == 0));
    }

    #[test]
    fn one_overflow_episode_is_one_event() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("e.wav");
        let (mut tx, mut sink) = channel(path, 100).unwrap(); // 70 slots
        for _ in 0..80 * BLOCK {
            tx.push(1);
        }
        assert_eq!(sink.stats().overflow_events.load(Ordering::Relaxed), 1);
        // exactly one slot frees: the owed silence takes it, the next block
        // still doesn't fit — the same episode, not a new one
        assert!(sink.ring.pop().is_ok());
        for _ in 0..BLOCK {
            tx.push(1);
        }
        assert_eq!(sink.stats().overflow_events.load(Ordering::Relaxed), 1);
        // the writer catches up, audio flows, then a new overflow is new
        while sink.ring.pop().is_ok() {}
        for _ in 0..BLOCK {
            tx.push(1);
        }
        for _ in 0..80 * BLOCK {
            tx.push(1);
        }
        assert_eq!(sink.stats().overflow_events.load(Ordering::Relaxed), 2);
    }
}
