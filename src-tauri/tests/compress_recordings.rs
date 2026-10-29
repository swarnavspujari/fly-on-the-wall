//! Recording storage v2 (lossless FLAC) end to end, without sidecars:
//! compressing a meeting folder, every layout the app stores, resume after
//! an interruption at each commit step, a real process kill mid-compress,
//! and the readers (pipeline inputs, mixdown, playback) staying
//! bit-identical before and after.

use std::path::{Path, PathBuf};
use std::sync::Mutex;

use fly_app_lib::audio_store::{
    compress_meeting, is_storage_v2, needs_work, needs_work_skipping, MeetingCtx, MeetingError,
    MeetingOutcome,
};
use fly_audio::derive::DerivedKind;
use fly_audio::mix;
use fly_core::RecordingRef;
use fly_storage::Storage;

const FOLDER: &str = "2026-01-05 Design review";

/// Speech-like synthetic audio: a few tones plus noise, some silence.
fn signal(secs: f32, rate: u32, seed: u32) -> Vec<f32> {
    let n = (secs * rate as f32) as usize;
    let mut x = seed.wrapping_mul(2_654_435_761).max(1);
    (0..n)
        .map(|i| {
            x ^= x << 13;
            x ^= x >> 17;
            x ^= x << 5;
            let t = i as f32 / rate as f32;
            if (t * 2.0) as u32 % 5 == 4 {
                return 0.0;
            }
            let noise = (x % 2000) as f32 / 2000.0 - 0.5;
            0.3 * (t * 220.0 * (1.0 + seed as f32 * 0.1)).sin()
                + 0.2 * (t * 523.0).sin()
                + 0.05 * noise
        })
        .collect()
}

struct Meeting {
    _dir: tempfile::TempDir,
    data_dir: PathBuf,
    storage: Mutex<Storage>,
    id: String,
    /// Free disk space the job sees (`None`: unknown, never blocks).
    free_space: &'static dyn Fn(&Path) -> Option<u64>,
}

impl Meeting {
    fn folder(&self) -> PathBuf {
        self.data_dir.join("recordings").join(FOLDER)
    }

    fn rec(&self) -> RecordingRef {
        self.storage
            .lock()
            .unwrap()
            .get_meeting(&self.id)
            .unwrap()
            .recording
            .unwrap()
    }

    fn file(&self, name: &str) -> PathBuf {
        self.folder().join(name)
    }

    fn compress(&self) -> Result<MeetingOutcome, MeetingError> {
        self.compress_with(&mut || true, &|_| Ok(()))
    }

    fn compress_with(
        &self,
        checkpoint: &mut dyn FnMut() -> bool,
        may_commit: &dyn Fn(&Storage) -> Result<(), String>,
    ) -> Result<MeetingOutcome, MeetingError> {
        let mut done = |_| {};
        let mut ctx = MeetingCtx {
            data_dir: &self.data_dir,
            checkpoint,
            may_commit,
            storage: &self.storage,
            on_file_done: &mut done,
            skip_leftover: &|_| false,
            free_space: self.free_space,
        };
        compress_meeting(&mut ctx, &self.id)
    }
}

#[derive(Clone, Copy)]
enum Layout {
    /// What capture writes with system audio: mic, system, mixed, playback.
    TwoChannel,
    /// A mic-only capture: playback points at the mic file.
    MicOnly,
    /// Media import: only the 16 kHz mixed track (plus the original copy).
    Import,
    /// Two channels, but the stored mixdown was made by different code
    /// (it must be kept, losslessly, not dropped).
    ForeignMixdown,
}

fn setup(layout: Layout) -> Meeting {
    setup_scaled(layout, 1.0)
}

/// `scale` shortens the audio (the kill test runs many child processes).
fn setup_scaled(layout: Layout, scale: f32) -> Meeting {
    let signal = |secs: f32, rate: u32, seed: u32| signal(secs * scale, rate, seed);
    let dir = tempfile::tempdir().unwrap();
    let data_dir = dir.path().to_path_buf();
    let storage = Storage::open(&data_dir).unwrap();
    let note = storage.create_note("Design review", None).unwrap();
    let meeting = storage
        .create_meeting("Design review", &note.id, &[])
        .unwrap();
    let folder = data_dir.join("recordings").join(FOLDER);
    std::fs::create_dir_all(&folder).unwrap();
    let rel = |n: &str| Some(format!("recordings/{FOLDER}/{n}"));
    let mic = folder.join("recording.mic.wav");
    let sys = folder.join("recording.system.wav");
    let mixed = folder.join("recording.mixed.wav");
    let playback = folder.join("recording.playback.wav");
    let rec = match layout {
        Layout::TwoChannel | Layout::ForeignMixdown => {
            mix::write_wav_mono_16(&mic, &signal(7.3, 48_000, 1), 48_000).unwrap();
            // a different native rate on the loopback, as on many machines
            mix::write_wav_mono_16(&sys, &signal(7.2, 44_100, 2), 44_100).unwrap();
            if matches!(layout, Layout::ForeignMixdown) {
                mix::write_wav_mono_16(&mixed, &signal(7.3, 16_000, 3), 16_000).unwrap();
            } else {
                mix::write_mixdown(Some(&mic), Some(&sys), &mixed).unwrap();
            }
            mix::write_playback_mix(Some(&mic), Some(&sys), &playback).unwrap();
            RecordingRef {
                mic_path: rel("recording.mic.wav"),
                system_path: rel("recording.system.wav"),
                mixed_path: rel("recording.mixed.wav"),
                playback_path: rel("recording.playback.wav"),
                duration_ms: 7300,
            }
        }
        Layout::MicOnly => {
            mix::write_wav_mono_16(&mic, &signal(5.0, 48_000, 4), 48_000).unwrap();
            mix::write_mixdown(Some(&mic), None, &mixed).unwrap();
            RecordingRef {
                mic_path: rel("recording.mic.wav"),
                system_path: None,
                mixed_path: rel("recording.mixed.wav"),
                playback_path: rel("recording.mic.wav"),
                duration_ms: 5000,
            }
        }
        Layout::Import => {
            mix::write_wav_mono_16(&mixed, &signal(6.0, 16_000, 5), 16_000).unwrap();
            std::fs::write(folder.join("Board call.mp4"), b"not really a video").unwrap();
            RecordingRef {
                mic_path: None,
                system_path: None,
                mixed_path: rel("recording.mixed.wav"),
                playback_path: None,
                duration_ms: 6000,
            }
        }
    };
    storage.end_meeting(&meeting.id, &rec).unwrap();
    Meeting {
        _dir: dir,
        data_dir,
        storage: Mutex::new(storage),
        id: meeting.id,
        free_space: &|_| None,
    }
}

/// Exactly what `pipeline::run_with`'s `prep` writes for the ASR engines.
fn asr_input(src: &Path, out: &Path) -> Vec<u8> {
    let (s, r) = mix::read_audio_mono(src).unwrap();
    mix::write_wav_mono_16(out, &mix::resample_linear(&s, r, 16_000), 16_000).unwrap();
    std::fs::read(out).unwrap()
}

fn bits(v: &[f32]) -> Vec<u32> {
    v.iter().map(|x| x.to_bits()).collect()
}

fn names(dir: &Path) -> Vec<String> {
    let mut v: Vec<String> = std::fs::read_dir(dir)
        .unwrap()
        .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
        .collect();
    v.sort();
    v
}

#[test]
fn two_channel_meeting_keeps_lossless_sources_and_drops_regenerable_mixes() {
    let m = setup(Layout::TwoChannel);
    let before_mic = mix::read_wav_mono(&m.file("recording.mic.wav")).unwrap();
    let before_sys = mix::read_wav_mono(&m.file("recording.system.wav")).unwrap();
    let before_mixed = std::fs::read(m.file("recording.mixed.wav")).unwrap();
    let before_playback = std::fs::read(m.file("recording.playback.wav")).unwrap();
    let scratch = tempfile::tempdir().unwrap();
    let asr_before = [
        asr_input(&m.file("recording.mic.wav"), &scratch.path().join("a")),
        asr_input(&m.file("recording.system.wav"), &scratch.path().join("b")),
    ];
    let mic_mtime = std::fs::metadata(m.file("recording.mic.wav"))
        .unwrap()
        .modified()
        .unwrap();
    assert!(needs_work(&m.data_dir, &m.rec()).0);

    let out = m.compress().unwrap();
    assert!(out.kept.is_empty(), "{:?}", out.kept);
    assert!(out.saved() > 0);

    let rec = m.rec();
    assert!(is_storage_v2(&rec));
    assert_eq!(
        rec.mic_path.as_deref(),
        Some(format!("recordings/{FOLDER}/recording.mic.flac").as_str())
    );
    assert_eq!(
        rec.mixed_path, None,
        "the mixdown regenerates, so it isn't kept"
    );
    assert_eq!(rec.playback_path, None, "so does the playback mix");
    assert_eq!(
        names(&m.folder()),
        vec![
            "recording.manifest.json",
            "recording.mic.flac",
            "recording.system.flac",
        ]
    );
    // The manifest (portability, self-heal) points at the new files too.
    let manifest: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(m.file("recording.manifest.json")).unwrap())
            .unwrap();
    assert_eq!(
        manifest["recording"]["mic_path"],
        rec.mic_path.clone().unwrap()
    );
    assert_eq!(manifest["recording"]["mixed_path"], serde_json::Value::Null);
    // Retention ages audio by file time: unchanged.
    assert_eq!(
        std::fs::metadata(m.file("recording.mic.flac"))
            .unwrap()
            .modified()
            .unwrap(),
        mic_mtime
    );

    // Readers: the same f32 samples, the same ASR input bytes...
    let after_mic = mix::read_audio_mono(&m.file("recording.mic.flac")).unwrap();
    let after_sys = mix::read_audio_mono(&m.file("recording.system.flac")).unwrap();
    assert_eq!(
        (bits(&after_mic.0), after_mic.1),
        (bits(&before_mic.0), before_mic.1)
    );
    assert_eq!(
        (bits(&after_sys.0), after_sys.1),
        (bits(&before_sys.0), before_sys.1)
    );
    assert_eq!(
        asr_input(&m.file("recording.mic.flac"), &scratch.path().join("c")),
        asr_before[0]
    );
    assert_eq!(
        asr_input(&m.file("recording.system.flac"), &scratch.path().join("d")),
        asr_before[1]
    );
    // ...and the dropped files come back byte for byte.
    let regen = scratch.path().join("mixed.wav");
    fly_audio::derive::regenerate(
        DerivedKind::Mixdown,
        Some(&m.file("recording.mic.flac")),
        Some(&m.file("recording.system.flac")),
        &regen,
        &mut || true,
    )
    .unwrap();
    assert_eq!(std::fs::read(&regen).unwrap(), before_mixed);
    fly_audio::derive::regenerate(
        DerivedKind::Playback,
        Some(&m.file("recording.mic.flac")),
        Some(&m.file("recording.system.flac")),
        &regen,
        &mut || true,
    )
    .unwrap();
    assert_eq!(std::fs::read(&regen).unwrap(), before_playback);

    // Idempotent: nothing left to do.
    assert!(!needs_work(&m.data_dir, &m.rec()).0);
    assert_eq!(m.compress().unwrap().saved(), 0);
}

#[test]
fn mic_only_capture_plays_the_mic_flac_and_rebuilds_its_asr_track() {
    let m = setup(Layout::MicOnly);
    let scratch = tempfile::tempdir().unwrap();
    // The single-track path transcribes the stored mixdown.
    let asr_before = asr_input(&m.file("recording.mixed.wav"), &scratch.path().join("a"));
    m.compress().unwrap();
    let rec = m.rec();
    let mic = format!("recordings/{FOLDER}/recording.mic.flac");
    assert_eq!(rec.mic_path.as_deref(), Some(mic.as_str()));
    assert_eq!(rec.playback_path.as_deref(), Some(mic.as_str()));
    assert_eq!(rec.mixed_path, None);
    let regen = fly_app_lib::audio_store::regenerate_mixdown_for_asr(
        &rec,
        None,
        Some(&m.file("recording.mic.flac")),
        None,
        scratch.path(),
    )
    .unwrap()
    .expect("a dropped mixdown is rebuilt");
    assert_eq!(asr_input(&regen, &scratch.path().join("b")), asr_before);
}

#[test]
fn import_keeps_its_mixed_track_as_flac_and_never_touches_the_original() {
    let m = setup(Layout::Import);
    let scratch = tempfile::tempdir().unwrap();
    let asr_before = asr_input(&m.file("recording.mixed.wav"), &scratch.path().join("a"));
    m.compress().unwrap();
    let rec = m.rec();
    assert_eq!(
        rec.mixed_path.as_deref(),
        Some(format!("recordings/{FOLDER}/recording.mixed.flac").as_str())
    );
    assert!(!is_storage_v2(&rec), "no sources: nothing is derived");
    assert!(m.file("Board call.mp4").exists());
    assert!(!m.file("recording.mixed.wav").exists());
    assert_eq!(
        asr_input(&m.file("recording.mixed.flac"), &scratch.path().join("b")),
        asr_before
    );
}

#[test]
fn a_mixdown_that_does_not_regenerate_is_kept_losslessly() {
    let m = setup(Layout::ForeignMixdown);
    let before = mix::read_wav_mono(&m.file("recording.mixed.wav")).unwrap();
    m.compress().unwrap();
    let rec = m.rec();
    assert_eq!(
        rec.mixed_path.as_deref(),
        Some(format!("recordings/{FOLDER}/recording.mixed.flac").as_str())
    );
    let after = mix::read_audio_mono(&m.file("recording.mixed.flac")).unwrap();
    assert_eq!(bits(&after.0), bits(&before.0));
    // The playback mix did regenerate, so it went.
    assert_eq!(rec.playback_path, None);
}

#[test]
fn a_wav_capture_never_finalized_is_left_alone() {
    let m = setup(Layout::TwoChannel);
    // Simulate a crash before finalize: hound leaves the size fields at 0.
    let mic = m.file("recording.mic.wav");
    let mut bytes = std::fs::read(&mic).unwrap();
    bytes[4..8].copy_from_slice(&0u32.to_le_bytes());
    bytes[40..44].copy_from_slice(&0u32.to_le_bytes());
    std::fs::write(&mic, &bytes).unwrap();
    assert_eq!(
        needs_work(&m.data_dir, &m.rec()).2,
        1,
        "counted before the run"
    );
    let out = m.compress().unwrap();
    assert_eq!(out.kept.len(), 1, "{:?}", out.kept);
    let after = needs_work(&m.data_dir, &m.rec());
    assert_eq!(
        (after.0, after.2),
        (false, 1),
        "nothing left to do, one file stays WAV"
    );
    assert_eq!(std::fs::read(&mic).unwrap(), bytes, "untouched");
    let rec = m.rec();
    assert!(rec.mic_path.unwrap().ends_with("recording.mic.wav"));
    assert!(rec.system_path.unwrap().ends_with(".flac"));
    // The mixes were made from the full mic samples, which the broken
    // header no longer exposes: they don't regenerate, so they're kept.
    assert!(rec.mixed_path.unwrap().ends_with(".flac"));
    assert!(rec.playback_path.unwrap().ends_with(".flac"));
}

#[test]
fn stopping_mid_encode_changes_nothing_and_the_next_run_finishes() {
    let m = setup(Layout::TwoChannel);
    let original = names(&m.folder());
    let rec = m.rec();
    // Stop at the third checkpoint (inside the first file).
    let mut n = 0;
    let mut stop = || {
        n += 1;
        n < 3
    };
    assert_eq!(
        m.compress_with(&mut stop, &|_| Ok(())),
        Err(MeetingError::Stopped)
    );
    assert_eq!(
        names(&m.folder()),
        original,
        "temp removed, nothing else touched"
    );
    assert_eq!(m.rec(), rec);
    m.compress().unwrap();
    assert!(is_storage_v2(&m.rec()));
}

#[test]
fn a_refused_commit_deletes_nothing_and_the_retry_reuses_the_verified_flacs() {
    let m = setup(Layout::TwoChannel);
    let rec = m.rec();
    let err = m
        .compress_with(&mut || true, &|_| Err("transcription queued".into()))
        .unwrap_err();
    assert!(matches!(err, MeetingError::Busy(_)));
    assert_eq!(m.rec(), rec, "row untouched");
    for f in [
        "recording.mic.wav",
        "recording.system.wav",
        "recording.mixed.wav",
        "recording.playback.wav",
        "recording.mic.flac",
        "recording.system.flac",
    ] {
        assert!(m.file(f).exists(), "{f}");
    }
    // Next pass: the FLACs are re-verified and reused, then committed.
    m.compress().unwrap();
    assert!(!m.file("recording.mic.wav").exists());
}

#[test]
fn a_damaged_flac_from_an_earlier_run_is_rebuilt_not_trusted() {
    let m = setup(Layout::TwoChannel);
    m.compress_with(&mut || true, &|_| Err("busy".into()))
        .unwrap_err();
    let flac = m.file("recording.mic.flac");
    let mut b = std::fs::read(&flac).unwrap();
    let mid = b.len() / 2;
    b[mid] ^= 0x40;
    std::fs::write(&flac, &b).unwrap();
    let before = mix::read_wav_mono(&m.file("recording.mic.wav")).unwrap();
    m.compress().unwrap();
    let after = mix::read_audio_mono(&flac).unwrap();
    assert_eq!(bits(&after.0), bits(&before.0));
}

#[test]
fn wavs_left_after_a_commit_are_deleted_only_once_re_proven() {
    let m = setup(Layout::TwoChannel);
    let wavs: Vec<(String, Vec<u8>)> = [
        "recording.mic.wav",
        "recording.system.wav",
        "recording.mixed.wav",
        "recording.playback.wav",
    ]
    .iter()
    .map(|n| (n.to_string(), std::fs::read(m.file(n)).unwrap()))
    .collect();
    m.compress().unwrap();
    // Put the WAVs back: the state a crash between commit and delete leaves.
    for (n, b) in &wavs {
        std::fs::write(m.file(n), b).unwrap();
    }
    // One leftover that is NOT what the FLAC holds must survive.
    let mut odd = wavs[1].1.clone();
    let last = odd.len() - 1;
    odd[last] ^= 1;
    std::fs::write(m.file("recording.system.wav"), &odd).unwrap();
    assert!(needs_work(&m.data_dir, &m.rec()).0);
    let out = m.compress().unwrap();
    assert!(!m.file("recording.mic.wav").exists());
    assert!(!m.file("recording.mixed.wav").exists());
    assert!(!m.file("recording.playback.wav").exists());
    assert_eq!(std::fs::read(m.file("recording.system.wav")).unwrap(), odd);
    // ... said why, and isn't decoded again every pass while unchanged.
    assert_eq!(out.unproven, vec![m.file("recording.system.wav")]);
    assert!(
        out.kept.iter().any(|k| k.contains("doesn't match")),
        "{:?}",
        out.kept
    );
    let known = out.unproven.clone();
    let skip = |p: &Path| known.iter().any(|k| k == p);
    assert_eq!(
        needs_work_skipping(&m.data_dir, &m.rec(), &skip),
        (false, 0, 1)
    );
}

#[test]
fn a_hard_failure_before_the_commit_leaves_no_extra_copy() {
    let m = setup(Layout::TwoChannel);
    // The system FLAC's path is taken by a folder: its conversion fails
    // after the mic FLAC was already put in place.
    std::fs::create_dir(m.file("recording.system.flac")).unwrap();
    let err = m.compress().unwrap_err();
    assert!(matches!(err, MeetingError::Failed(_)), "{err:?}");
    assert!(
        !m.file("recording.mic.flac").exists(),
        "uncommitted FLAC removed"
    );
    assert!(m.file("recording.mic.wav").exists());
    assert!(m.rec().mic_path.unwrap().ends_with(".wav"), "row untouched");
    std::fs::remove_dir(m.file("recording.system.flac")).unwrap();
    m.compress().unwrap();
    assert!(is_storage_v2(&m.rec()));
}

#[test]
fn an_unreadable_stored_mix_stays_as_it_is() {
    let m = setup(Layout::TwoChannel);
    std::fs::write(m.file("recording.mixed.wav"), b"RIFF\0\0\0\0WAVEjunk").unwrap();
    let out = m.compress().unwrap();
    assert_eq!(out.kept.len(), 1, "{:?}", out.kept);
    let rec = m.rec();
    assert!(rec.mixed_path.unwrap().ends_with("recording.mixed.wav"));
    assert!(m.file("recording.mixed.wav").exists());
    assert!(rec.mic_path.unwrap().ends_with(".flac"));
    assert!(!needs_work(&m.data_dir, &m.rec()).0, "no retry loop");
}

#[test]
fn a_keep_local_toggle_mid_compress_cant_leave_a_stale_manifest() {
    let m = setup(Layout::TwoChannel);
    let note_id = m
        .storage
        .lock()
        .unwrap()
        .get_meeting(&m.id)
        .unwrap()
        .note_id;
    // Toggle Keep local while the job is mid-file (it doesn't hold the
    // storage lock there). Any manifest rewrite this triggers is built from
    // the row, which still names the WAVs: still true at that moment.
    let mut toggled = false;
    let mut checkpoint = || {
        if !toggled {
            toggled = true;
            m.storage
                .lock()
                .unwrap()
                .set_note_keep_local(&note_id, true)
                .unwrap();
        }
        true
    };
    m.compress_with(&mut checkpoint, &|_| Ok(())).unwrap();
    assert!(toggled);
    let rec = m.rec();
    assert!(is_storage_v2(&rec));
    let manifest: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(m.file("recording.manifest.json")).unwrap())
            .unwrap();
    // The commit wrote the manifest from the row under the same lock that
    // serializes the toggle: it names the files that exist now.
    assert_eq!(
        manifest["recording"]["mic_path"],
        rec.mic_path.clone().unwrap()
    );
    assert_eq!(
        manifest["recording"]["playback_path"],
        serde_json::Value::Null
    );
    assert!(m.file("recording.mic.flac").exists() && !m.file("recording.mic.wav").exists());
    assert!(m.storage.lock().unwrap().meeting_keep_local(&m.id));
    // Manifest v3 carries the toggle too, and the version never drops.
    assert_eq!(manifest["keep_local"], serde_json::Value::Bool(true));
    assert_eq!(
        manifest["version"],
        u64::from(fly_storage::MANIFEST_VERSION)
    );
    // A toggle after the commit rewrites it from the row again: still FLAC.
    m.storage
        .lock()
        .unwrap()
        .set_note_keep_local(&note_id, false)
        .unwrap();
    let after: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(m.file("recording.manifest.json")).unwrap())
            .unwrap();
    assert_eq!(after["recording"]["mic_path"], rec.mic_path.unwrap());
    assert_eq!(after["keep_local"], serde_json::Value::Bool(false));
}

#[test]
fn a_folder_whose_manifest_is_from_a_newer_build_is_left_alone() {
    let m = setup(Layout::TwoChannel);
    let future = r#"{"version": 99, "meeting_id": "x", "note_id": "y", "ended_at": "2026-01-01T00:00:00Z", "recording": {"mic_path": null, "system_path": null, "mixed_path": null, "duration_ms": 0}}"#;
    std::fs::write(m.file("recording.manifest.json"), future).unwrap();
    let before = names(&m.folder());
    assert!(!needs_work(&m.data_dir, &m.rec()).0);
    assert!(matches!(m.compress(), Err(MeetingError::Busy(_))));
    assert_eq!(names(&m.folder()), before);
    assert_eq!(
        std::fs::read_to_string(m.file("recording.manifest.json")).unwrap(),
        future
    );
}

#[test]
fn a_stray_temp_file_is_cleared() {
    let m = setup(Layout::TwoChannel);
    std::fs::write(m.file(".recording.mic.compressing.flac"), b"half").unwrap();
    m.compress().unwrap();
    assert!(!m.file(".recording.mic.compressing.flac").exists());
}

// ---------------------------------------------------------------------------
// A real kill: a child process compresses and is killed at a spread of
// moments; after every kill the audio is intact and a fresh run finishes.
// ---------------------------------------------------------------------------

const CHILD_ENV: &str = "FOTW_COMPRESS_KILL_CHILD_DIR";
/// The child aborts the process at its Nth checkpoint (0 = never).
const ABORT_AT_ENV: &str = "FOTW_COMPRESS_ABORT_AT";

/// Runs only as the child of `killed_at_any_moment_loses_nothing`.
#[test]
#[ignore = "child process of killed_at_any_moment_loses_nothing"]
fn kill_child_entry() {
    let Ok(dir) = std::env::var(CHILD_ENV) else {
        return;
    };
    let abort_at: usize = std::env::var(ABORT_AT_ENV)
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(0);
    let data_dir = PathBuf::from(dir);
    let id = std::fs::read_to_string(data_dir.join("meeting-id.txt")).unwrap();
    let storage = Mutex::new(Storage::open(&data_dir).unwrap());
    let mut calls = 0usize;
    let mut checkpoint = || {
        calls += 1;
        if calls == abort_at {
            // As abrupt as a kill: no unwinding, no destructors.
            std::process::abort();
        }
        if abort_at == 0 {
            // timed kills: slow down so the kill lands mid-way
            std::thread::sleep(std::time::Duration::from_millis(3));
        }
        true
    };
    let mut done = |_| {};
    let mut ctx = MeetingCtx {
        data_dir: &data_dir,
        checkpoint: &mut checkpoint,
        may_commit: &|_| Ok(()),
        storage: &storage,
        on_file_done: &mut done,
        skip_leftover: &|_| false,
        free_space: &|_| None,
    };
    let _ = compress_meeting(&mut ctx, &id);
    std::fs::write(data_dir.join("child-finished.txt"), calls.to_string()).unwrap();
}

/// Every original sample is still somewhere a reader finds it.
fn assert_audio_intact(m: &Meeting, originals: &[(&str, Vec<u32>)]) {
    let rec = m.rec();
    let mic = rec.mic_path.as_ref().map(|r| m.data_dir.join(r)).unwrap();
    let sys = rec
        .system_path
        .as_ref()
        .map(|r| m.data_dir.join(r))
        .unwrap();
    assert_eq!(bits(&mix::read_audio_mono(&mic).unwrap().0), originals[0].1);
    assert_eq!(bits(&mix::read_audio_mono(&sys).unwrap().0), originals[1].1);
    // The derived files exist or regenerate.
    for (role, path, kind) in [
        ("mixed", &rec.mixed_path, DerivedKind::Mixdown),
        ("playback", &rec.playback_path, DerivedKind::Playback),
    ] {
        let want = &originals.iter().find(|(n, _)| *n == role).unwrap().1;
        let got = match path {
            Some(p) => mix::read_audio_mono(&m.data_dir.join(p)).unwrap().0,
            None => {
                let out = m.data_dir.join("regen-check.wav");
                fly_audio::derive::regenerate(kind, Some(&mic), Some(&sys), &out, &mut || true)
                    .unwrap();
                let v = mix::read_wav_mono(&out).unwrap().0;
                std::fs::remove_file(out).unwrap();
                v
            }
        };
        assert_eq!(&bits(&got), want, "{role}");
    }
}

/// Where a killed run left the meeting.
fn stage_of(m: &Meeting) -> &'static str {
    let committed = is_storage_v2(&m.rec());
    let wavs = names(&m.folder()).iter().any(|n| n.ends_with(".wav"));
    let flacs = names(&m.folder()).iter().any(|n| n.ends_with(".flac"));
    match (committed, wavs, flacs) {
        (false, _, false) => "untouched",
        (false, _, true) => "flac written, not committed",
        (true, true, _) => "committed, wavs left",
        (true, false, _) => "done",
    }
}

#[test]
fn killed_at_any_moment_loses_nothing() {
    let exe = std::env::current_exe().unwrap();
    // Abort at every checkpoint in turn (encode, verify, derived checks of
    // each file), then a few wall-clock kills, until a run gets through.
    let mut runs: Vec<(usize, u64)> = (1..=12).map(|n| (n, 0)).collect();
    runs.extend([(0, 60), (0, 250), (0, 700)]);
    let mut stages = std::collections::BTreeSet::new();
    for (abort_at, kill_after_ms) in runs {
        let m = setup_scaled(Layout::TwoChannel, 0.4);
        std::fs::write(m.data_dir.join("meeting-id.txt"), &m.id).unwrap();
        let originals: Vec<(&str, Vec<u32>)> = ["mic", "system", "mixed", "playback"]
            .iter()
            .map(|r| {
                let p = m.file(&format!("recording.{r}.wav"));
                (*r, bits(&mix::read_wav_mono(&p).unwrap().0))
            })
            .collect();
        let mut child = std::process::Command::new(&exe)
            .args(["kill_child_entry", "--exact", "--ignored", "--nocapture"])
            .env(CHILD_ENV, &m.data_dir)
            .env(ABORT_AT_ENV, abort_at.to_string())
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .spawn()
            .unwrap();
        if kill_after_ms > 0 {
            std::thread::sleep(std::time::Duration::from_millis(kill_after_ms));
            let _ = child.kill();
        }
        let _ = child.wait();
        let finished = m.data_dir.join("child-finished.txt").exists();

        // Whatever moment it died at, nothing is lost (a fresh connection
        // sees what the child committed)...
        let m = Meeting {
            storage: Mutex::new(Storage::open(&m.data_dir).unwrap()),
            ..m
        };
        let label = format!("abort at checkpoint {abort_at}, kill at {kill_after_ms} ms");
        stages.insert(stage_of(&m));
        assert_audio_intact(&m, &originals);
        // ...and a normal run finishes the job.
        m.compress().unwrap();
        let rec = m.rec();
        assert!(is_storage_v2(&rec), "{label}");
        assert!(!needs_work(&m.data_dir, &rec).0, "{label}");
        assert_audio_intact(&m, &originals);
        assert_eq!(
            names(&m.folder()),
            vec![
                "recording.manifest.json",
                "recording.mic.flac",
                "recording.system.flac"
            ],
            "{label}"
        );
        if finished && abort_at > 0 {
            break; // past the last checkpoint: every phase was hit
        }
    }
    // The deterministic aborts reach the middle of the protocol, not just
    // its start.
    assert!(
        stages.contains("flac written, not committed"),
        "stages reached: {stages:?}"
    );
}

/// A media import keeps its mixed track as FLAC (not storage v2): its WAV
/// left behind by a crash after the commit is still found and re-proven.
#[test]
fn an_imports_leftover_wav_is_cleaned_up_too() {
    let m = setup(Layout::Import);
    let wav = std::fs::read(m.file("recording.mixed.wav")).unwrap();
    m.compress().unwrap();
    assert!(!is_storage_v2(&m.rec()));
    std::fs::write(m.file("recording.mixed.wav"), &wav).unwrap();
    assert!(needs_work(&m.data_dir, &m.rec()).0);
    let out = m.compress().unwrap();
    assert!(!m.file("recording.mixed.wav").exists());
    assert_eq!(out.wav_bytes_removed, wav.len() as u64);
    assert!(!needs_work(&m.data_dir, &m.rec()).0);
}

/// The meeting's audio was deleted while the job worked, and the sweep
/// missed a FLAC the job had just put in place: nothing is committed and
/// that FLAC goes too, so no copy outlives the deletion.
#[test]
fn audio_deleted_mid_run_leaves_no_flac_behind() {
    let m = setup(Layout::TwoChannel);
    let before = m.rec();
    let mut files_done = 0;
    let mut on_file_done = |_: u64| {
        files_done += 1;
        if files_done == 1 {
            m.storage
                .lock()
                .unwrap()
                .delete_meeting_audio(&m.id)
                .unwrap();
            // the file the sweep's directory listing missed
            mix::write_wav_mono_16(&m.file("recording.mic.flac"), &[0.0; 16], 16_000).unwrap();
        }
    };
    let mut go = || true;
    let mut ctx = MeetingCtx {
        data_dir: &m.data_dir,
        checkpoint: &mut go,
        may_commit: &|_| Ok(()),
        storage: &m.storage,
        on_file_done: &mut on_file_done,
        skip_leftover: &|_| false,
        free_space: &|_| None,
    };
    let err = compress_meeting(&mut ctx, &m.id).unwrap_err();
    assert!(matches!(err, MeetingError::Gone(_)), "{err:?}");
    assert!(!m.file("recording.mic.flac").exists());
    assert_eq!(m.rec(), before, "nothing committed");
}

#[test]
fn a_full_disk_fails_before_encoding_and_keeps_the_originals() {
    let mut m = setup(Layout::TwoChannel);
    m.free_space = &|_| Some(4096);
    let before = m.rec();
    match m.compress().unwrap_err() {
        MeetingError::Failed(e) => assert!(e.contains("free disk space"), "{e}"),
        e => panic!("{e:?}"),
    }
    assert_eq!(m.rec(), before);
    assert!(m.file("recording.mic.wav").exists());
    assert!(!names(&m.folder()).iter().any(|n| n.ends_with(".flac")));
    m.free_space = &|_| Some(u64::MAX);
    m.compress().unwrap();
    assert!(is_storage_v2(&m.rec()));
}

/// A recording path pointing outside its own folder (a copied-in manifest,
/// another meeting's folder) is never converted, replaced or deleted.
#[test]
fn a_recording_path_outside_its_folder_leaves_the_meeting_alone() {
    let m = setup(Layout::TwoChannel);
    let other = m.data_dir.join("recordings").join("Another meeting");
    std::fs::create_dir_all(&other).unwrap();
    std::fs::copy(
        m.file("recording.playback.wav"),
        other.join("recording.playback.wav"),
    )
    .unwrap();
    let mut rec = m.rec();
    rec.playback_path = Some("recordings/Another meeting/recording.playback.wav".into());
    m.storage.lock().unwrap().end_meeting(&m.id, &rec).unwrap();
    assert_eq!(needs_work(&m.data_dir, &rec), (false, 0, 1));
    assert!(matches!(m.compress(), Err(MeetingError::Failed(_))));
    assert_eq!(m.rec(), rec);
    assert!(other.join("recording.playback.wav").exists());
    assert!(m.file("recording.mic.wav").exists());
    assert!(!m.file("recording.mic.flac").exists());
}
