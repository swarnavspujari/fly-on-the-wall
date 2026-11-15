//! Real-sidecar regression tests for the "non-ASCII Windows profile path"
//! bug (issue #122): the C++ sidecars read argv through the ANSI code page,
//! mangle a `ñ`/`ú` in the user's profile folder and die with 0xC0000409.
//!
//! `#[ignore]`d — they need the installed app's sidecars and models (read
//! only, from `%APPDATA%\FlyOnTheWall`; override the root with
//! `FLY_TEST_APP_DATA`). Everything they write goes to a temp folder whose
//! name contains non-ASCII characters. Run locally with:
//!
//!   cargo test -p fly-app --test non_ascii_sidecar_e2e -- --ignored --nocapture --test-threads=1

#![cfg(windows)]

use std::path::{Path, PathBuf};

use fly_asr::whisper_cpp::WhisperCppEngine;
use fly_asr::{TranscribeOptions, TranscriptionEngine};
use fly_diarize::sherpa::SherpaDiarizeEngine;
use fly_diarize::{DiarizationEngine, DiarizeOptions};

const NON_ASCII_DIR: &str = "Ünicode Tëst ñ ú";

fn app_data() -> PathBuf {
    std::env::var_os("FLY_TEST_APP_DATA")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from(std::env::var_os("APPDATA").unwrap()).join("FlyOnTheWall"))
}

fn find_file(root: &Path, name: &str) -> PathBuf {
    fn walk(dir: &Path, name: &str, depth: usize) -> Option<PathBuf> {
        for e in std::fs::read_dir(dir).ok()?.flatten() {
            let p = e.path();
            if p.is_file() && p.file_name().is_some_and(|n| n == name) {
                return Some(p);
            }
            if depth > 0 && p.is_dir() {
                if let Some(f) = walk(&p, name, depth - 1) {
                    return Some(f);
                }
            }
        }
        None
    }
    walk(root, name, 6).unwrap_or_else(|| panic!("{name} not found under {}", root.display()))
}

fn copy_dir(from: &Path, to: &Path) {
    std::fs::create_dir_all(to).unwrap();
    for e in std::fs::read_dir(from).unwrap().flatten() {
        let (src, dst) = (e.path(), to.join(e.file_name()));
        if src.is_dir() {
            copy_dir(&src, &dst);
        } else {
            std::fs::copy(&src, &dst).unwrap();
        }
    }
}

/// Copy a sidecar's whole folder (exe + DLLs + ggml backends) under `dir`,
/// as it sits on a real affected user's machine, and return the new exe.
fn copy_sidecar(exe: &Path, dir: &Path) -> PathBuf {
    let src_dir = exe.parent().unwrap();
    let dst_dir = dir.join("bin");
    copy_dir(src_dir, &dst_dir);
    dst_dir.join(exe.file_name().unwrap())
}

fn fixture() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("fixtures/meeting-fixture.wav")
}

fn non_ascii_dir() -> tempfile::TempDir {
    // Earlier tests point TMP/TEMP into their (since deleted) folder: always
    // start from the real temp dir.
    static ORIG: std::sync::OnceLock<(std::ffi::OsString, std::ffi::OsString)> =
        std::sync::OnceLock::new();
    let (tmp, temp) = ORIG.get_or_init(|| {
        (
            std::env::var_os("TMP").unwrap_or_default(),
            std::env::var_os("TEMP").unwrap_or_default(),
        )
    });
    std::env::set_var("TMP", tmp);
    std::env::set_var("TEMP", temp);
    let base = tempfile::Builder::new().prefix("fly-").tempdir().unwrap();
    let dir = base.path().join(NON_ASCII_DIR);
    std::fs::create_dir_all(&dir).unwrap();
    // The guard owns the ASCII parent, so dropping it removes the non-ASCII child too.
    assert!(!dir.to_str().unwrap().is_ascii());
    base
}

#[tokio::test]
#[ignore = "needs the installed sherpa sidecar + diarization models"]
async fn sherpa_diarizes_when_every_input_sits_in_a_non_ascii_folder() {
    let data = app_data();
    let exe = find_file(
        &data.join("bin/sherpa"),
        "sherpa-onnx-offline-speaker-diarization.exe",
    );
    let seg_src = find_file(&data.join("models/diarize"), "model.onnx");
    let emb_src = find_file(&data.join("models/diarize"), "campplus.onnx");

    let base = non_ascii_dir();
    let dir = base.path().join(NON_ASCII_DIR);
    let (wav, seg, emb) = (
        dir.join("meeting.wav"),
        dir.join("segmentation.onnx"),
        dir.join("embedding.onnx"),
    );
    std::fs::copy(fixture(), &wav).unwrap();
    std::fs::copy(seg_src, &seg).unwrap();
    std::fs::copy(emb_src, &emb).unwrap();

    // exe + DLLs live under the non-ASCII folder too, like a real install
    let exe = copy_sidecar(&exe, &dir);
    let engine = SherpaDiarizeEngine {
        exe,
        segmentation_model: seg,
        embedding_model: emb,
        threads: 2,
        refine: None,
    };
    let turns = engine
        .diarize(
            &wav,
            &DiarizeOptions {
                num_speakers: None,
                cluster_threshold: Some(0.9),
                speaker_key_prefix: "spk".into(),
            },
        )
        .await
        .expect("diarization must work from a non-ASCII folder");
    assert!(!turns.is_empty(), "expected speaker turns");
}

#[tokio::test]
#[ignore = "needs the installed whisper-cli + the small model"]
async fn whisper_transcribes_when_model_and_temp_dir_are_non_ascii() {
    let data = app_data();
    let exe = find_file(&data.join("bin/whisper"), "whisper-cli.exe");
    let model_src = find_file(&data.join("models/asr"), "ggml-small-q5_1.bin");

    let base = non_ascii_dir();
    let dir = base.path().join(NON_ASCII_DIR);
    let model = dir.join("model.bin");
    let wav = dir.join("meeting.wav");
    std::fs::copy(model_src, &model).unwrap();
    std::fs::copy(fixture(), &wav).unwrap();
    // The engine writes its chunk wav + JSON under the temp dir, which on a
    // real user's machine sits inside the (non-ASCII) profile folder.
    std::env::set_var("TMP", &dir);
    std::env::set_var("TEMP", &dir);

    let exe = copy_sidecar(&exe, &dir);
    let engine = WhisperCppEngine {
        exe,
        model,
        threads: 4,
        force_cpu: true,
        resume: false,
    };
    let out = engine
        .transcribe(
            &wav,
            &TranscribeOptions {
                language: Some("en".into()),
                ..Default::default()
            },
        )
        .await
        .expect("whisper must work with non-ASCII model/temp paths");
    assert!(!out.words.is_empty(), "expected transcribed words");
}

#[tokio::test]
#[ignore = "needs the installed whisper-cli (parakeet-cli) + a parakeet model"]
async fn parakeet_transcribes_when_exe_model_and_temp_dir_are_non_ascii() {
    use fly_asr::parakeet::ParakeetEngine;
    let data = app_data();
    let exe = find_file(&data.join("bin/whisper"), "parakeet-cli.exe");
    let model_src = find_file(
        &data.join("models/asr"),
        "ggml-parakeet-tdt-0.6b-v3-q8_0.bin",
    );

    let base = non_ascii_dir();
    let dir = base.path().join(NON_ASCII_DIR);
    let model = dir.join("parakeet.bin");
    let wav = dir.join("meeting.wav");
    std::fs::copy(model_src, &model).unwrap();
    std::fs::copy(fixture(), &wav).unwrap();
    std::env::set_var("TMP", &dir);
    std::env::set_var("TEMP", &dir);
    let exe = copy_sidecar(&exe, &dir);

    let engine = ParakeetEngine {
        exe,
        model,
        threads: 4,
        force_cpu: true,
        resume: false,
    };
    let out = engine
        .transcribe(&wav, &TranscribeOptions::default())
        .await
        .expect("parakeet must work with non-ASCII exe/model/temp paths");
    assert!(!out.words.is_empty());
}

/// The common-ancestor fallback (short names disabled) against the real
/// sidecar: ASCII relative arguments, cwd = the non-ASCII folder.
#[test]
#[ignore = "needs the installed sherpa sidecar + diarization models"]
fn sherpa_runs_through_the_relative_path_fallback() {
    let data = app_data();
    let exe = find_file(
        &data.join("bin/sherpa"),
        "sherpa-onnx-offline-speaker-diarization.exe",
    );
    let seg_src = find_file(&data.join("models/diarize"), "model.onnx");
    let emb_src = find_file(&data.join("models/diarize"), "campplus.onnx");

    let base = non_ascii_dir();
    let dir = base.path().join(NON_ASCII_DIR);
    let (wav, seg, emb) = (
        dir.join("meeting.wav"),
        dir.join("segmentation.onnx"),
        dir.join("embedding.onnx"),
    );
    std::fs::copy(fixture(), &wav).unwrap();
    std::fs::copy(seg_src, &seg).unwrap();
    std::fs::copy(emb_src, &emb).unwrap();
    let exe = copy_sidecar(&exe, &dir);

    let sp = fly_sidecar::prepare_relative_only(&[&seg, &emb, &wav]).unwrap();
    assert_eq!(sp.current_dir.as_deref(), Some(dir.as_path()));
    assert!(sp.args.iter().all(|a| a.to_str().unwrap().is_ascii()));
    let out = std::process::Command::new(&exe)
        .current_dir(sp.current_dir.unwrap())
        .arg(format!(
            "--segmentation.pyannote-model={}",
            sp.args[0].display()
        ))
        .arg(format!("--embedding.model={}", sp.args[1].display()))
        .arg("--clustering.cluster-threshold=0.9")
        .arg(&sp.args[2])
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "sherpa via relative paths: {} {}",
        out.status,
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(String::from_utf8_lossy(&out.stdout).contains("speaker_"));
}
