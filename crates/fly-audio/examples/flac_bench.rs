//! Measure recording storage v2 on COPIES of real meeting folders: FLAC size
//! and speed per source, bit-exact verification, and whether the stored
//! mixdown / playback mix regenerate bit-identically from the sources.
//!
//! `cargo run --release -p fly-audio --example flac_bench -- <out_dir> <meeting_folder>...`
//!
//! Reads the folders, writes only into `<out_dir>`. Never point it at the
//! app's live data folder's output; copy the meetings somewhere first.

use std::path::{Path, PathBuf};
use std::time::Instant;

use fly_audio::derive::{self, DerivedKind};
use fly_audio::flac;

fn mb(b: u64) -> f64 {
    b as f64 / 1_048_576.0
}

fn main() {
    let args: Vec<PathBuf> = std::env::args().skip(1).map(PathBuf::from).collect();
    let (out_dir, folders) = args
        .split_first()
        .expect("usage: <out_dir> <meeting_folder>...");
    std::fs::create_dir_all(out_dir).unwrap();
    let mut go = || true;
    let (mut wav_total, mut kept_total, mut derived_total) = (0u64, 0u64, 0u64);
    for (i, folder) in folders.iter().enumerate() {
        println!("meeting {}", i + 1);
        let role = |r: &str| {
            let p = folder.join(format!("recording.{r}.wav"));
            p.exists().then_some(p)
        };
        let (mic, sys) = (role("mic"), role("system"));
        let mut flacs: Vec<(String, PathBuf)> = Vec::new();
        for r in ["mic", "system", "mixed", "playback"] {
            let Some(wav) = role(r) else { continue };
            let wav_bytes = std::fs::metadata(&wav).unwrap().len();
            wav_total += wav_bytes;
            let info = match flac::inspect_pcm16_wav(&wav) {
                Ok(i) => i,
                Err(e) => {
                    println!("  {r:8} {:8.1} MB  kept as WAV: {e}", mb(wav_bytes));
                    kept_total += wav_bytes;
                    continue;
                }
            };
            let out = out_dir.join(format!("m{}-{r}.flac", i + 1));
            let t = Instant::now();
            let enc = flac::encode_pcm16_wav_to_flac(&wav, &out, &mut go).unwrap();
            let enc_s = t.elapsed().as_secs_f64();
            let t = Instant::now();
            flac::verify_flac_matches_wav(&out, &wav, &mut go).unwrap();
            let ver_s = t.elapsed().as_secs_f64();
            let audio_s = enc.frames as f64 / info.sample_rate as f64;
            println!(
                "  {r:8} {:8.1} MB -> {:7.1} MB ({:4.1}%)  {} Hz  {:6.0} s audio  encode {:5.1}x  verify {:5.1}x realtime",
                mb(wav_bytes),
                mb(enc.flac_bytes),
                100.0 * enc.flac_bytes as f64 / wav_bytes as f64,
                info.sample_rate,
                audio_s,
                audio_s / enc_s,
                audio_s / ver_s
            );
            flacs.push((r.to_string(), out));
            if r == "mic" || r == "system" {
                kept_total += enc.flac_bytes;
            } else {
                derived_total += enc.flac_bytes;
            }
        }
        let flac_of = |r: &str| flacs.iter().find(|(n, _)| n == r).map(|(_, p)| p.clone());
        for (r, kind) in [
            ("mixed", DerivedKind::Mixdown),
            ("playback", DerivedKind::Playback),
        ] {
            let Some(stored) = role(r) else { continue };
            if mic.is_none() && sys.is_none() {
                continue;
            }
            let t = Instant::now();
            let same =
                derive::matches_stored(kind, mic.as_deref(), sys.as_deref(), &stored, &mut go)
                    .unwrap_or(false);
            let check_s = t.elapsed().as_secs_f64();
            // Regenerate from the FLAC sources: the player's cold-start cost.
            let regen = out_dir.join(format!("m{}-{r}.regen.wav", i + 1));
            let t = Instant::now();
            let ms = derive::regenerate(
                kind,
                flac_of("mic").as_deref(),
                flac_of("system").as_deref(),
                &regen,
                &mut go,
            )
            .unwrap();
            let regen_s = t.elapsed().as_secs_f64();
            let identical = std::fs::read(&regen).ok() == std::fs::read(&stored).ok();
            println!(
                "  {r:8} regenerates bit-identically: {same} (check {check_s:.1} s); from FLAC {:.0} s audio in {regen_s:.1} s; file identical: {identical}",
                ms as f64 / 1000.0
            );
            let _ = std::fs::remove_file(&regen);
        }
    }
    println!(
        "\nWAV {:.1} MB -> sources as FLAC (+ anything kept) {:.1} MB; derived files would add {:.1} MB as FLAC if kept",
        mb(wav_total),
        mb(kept_total),
        mb(derived_total)
    );
    let _ = Path::new(".");
}
