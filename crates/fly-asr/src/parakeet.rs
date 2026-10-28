//! NVIDIA Parakeet TDT engine via the `parakeet-cli` sidecar that ships in
//! the pinned whisper.cpp release zip (same ggml backends as whisper-cli).
//!
//! Preprocessing is identical to the whisper engine (crate::batch: adaptive
//! VAD, ≤ 120 s speech batches, peak normalization, timestamps mapped back
//! to the original timeline). `parakeet-cli` has no JSON output, so word
//! timestamps come from its `-ps` token dump on stderr: one line per
//! SentencePiece token with `t0`/`t1` and a `word_start` flag. Tokens are
//! glued into words at each `word_start`, which yields the same shape the
//! whisper parser produces (punctuation attached to the preceding word).
//!
//! English only for now: parakeet-cli has no language flag, so the pipeline
//! routes other languages (and auto-detect) to whisper.

use std::path::{Path, PathBuf};

use fly_core::Word;

use crate::{AsrError, RawTranscript, Result, TranscribeOptions, TranscriptionEngine};

/// `t0`/`t1` in the token dump count 10 ms ticks: each encoder frame is
/// 80 ms (10 ms mel hop × subsampling 8) and is printed as `frame × 8`.
const TOKEN_TICK_MS: u64 = 10;

/// Longest audio one parakeet-cli call may see. The encoder attends over
/// the whole input (context 5000 frames ≈ 400 s) and its cost grows faster
/// than linearly: measured on this repo's dev laptop, a single 480 s call
/// took 213 s to encode AND silently dropped everything after ~274 s.
/// Over-long speech spans are therefore cut at quiet points (crate::batch).
const MAX_SPAN_MS: u64 = 120_000;

/// SentencePiece word-boundary marker that prefixes word-initial tokens.
const SP_SPACE: char = '\u{2581}';

pub struct ParakeetEngine {
    /// Path to parakeet-cli(.exe).
    pub exe: PathBuf,
    /// Path to the GGML Parakeet model (ggml-org/parakeet-GGUF).
    pub model: PathBuf,
    pub threads: usize,
    /// Pass `-ng` so a GPU-capable build decodes on CPU.
    pub force_cpu: bool,
    /// Checkpoint finished batches next to the input wav (see whisper_cpp).
    pub resume: bool,
}

#[async_trait::async_trait]
impl TranscriptionEngine for ParakeetEngine {
    fn id(&self) -> &'static str {
        "parakeet"
    }

    fn is_local(&self) -> bool {
        true
    }

    fn model_tag(&self) -> Option<String> {
        model_tag_for(&self.model)
    }

    async fn transcribe(&self, wav_path: &Path, opts: &TranscribeOptions) -> Result<RawTranscript> {
        self.transcribe_with_progress(wav_path, opts, &|_| {}, &|| false)
            .await
    }

    async fn transcribe_with_progress(
        &self,
        wav_path: &Path,
        opts: &TranscribeOptions,
        on_progress: crate::TranscribeProgressFn<'_>,
        cancel: crate::CancelFn<'_>,
    ) -> Result<RawTranscript> {
        crate::batch::transcribe_batched(
            self.id(),
            &self.model,
            wav_path,
            opts,
            self.resume,
            Some(MAX_SPAN_MS),
            on_progress,
            cancel,
            |chunk, rate| async move { self.run_parakeet_cli(&chunk, rate, opts).await },
        )
        .await
    }
}

/// The model file's stem ("ggml-parakeet-tdt-0.6b-v3-q8_0") — recorded in
/// transcript metadata so a transcript says which weights produced it.
pub fn model_tag_for(model: &Path) -> Option<String> {
    model
        .file_stem()
        .map(|s| s.to_string_lossy().into_owned())
        .filter(|s| !s.is_empty())
}

impl ParakeetEngine {
    /// One batch: decode, then re-decode any speech stretch the decoder
    /// skipped (see [`find_holes`]) and splice those words in.
    async fn run_parakeet_cli(
        &self,
        samples: &[f32],
        rate: u32,
        opts: &TranscribeOptions,
    ) -> Result<RawTranscript> {
        let mut words = self.decode_once(samples, rate).await?;
        let holes = find_holes(&words, samples, rate);
        if !holes.is_empty() {
            let per_ms = rate as usize / 1000;
            let mut recovered = 0usize;
            let mut accepted = Vec::new();
            for (from, to) in &holes {
                let a = from.saturating_sub(HOLE_PAD_MS) as usize * per_ms;
                let b = ((to + HOLE_PAD_MS) as usize * per_ms).min(samples.len());
                let offset = (a / per_ms) as u64;
                // best-effort: a failed re-decode keeps the batch's own
                // (already successful) decode instead of failing the batch
                let found = match self.decode_once(&samples[a..b], rate).await {
                    Ok(found) => found,
                    Err(e) => {
                        tracing::warn!(error = %e, from, to, "parakeet: hole re-decode failed");
                        continue;
                    }
                };
                let shifted = found.into_iter().map(|mut w| {
                    w.start_ms += offset;
                    w.end_ms += offset;
                    w
                });
                accepted.extend(splice_recovered(&words, shifted, *from, *to));
            }
            recovered += accepted.len();
            words.extend(accepted);
            words.sort_by_key(|w| w.start_ms);
            tracing::info!(
                holes = holes.len(),
                recovered,
                "parakeet: re-decoded speech the decoder skipped"
            );
        }
        Ok(RawTranscript {
            language: opts.language.clone(),
            words: crate::drop_non_speech_spans(words),
            segments: vec![],
        })
    }

    /// One parakeet-cli invocation over prepared samples; words are timed
    /// relative to those samples.
    async fn decode_once(&self, samples: &[f32], rate: u32) -> Result<Vec<Word>> {
        let wav_path = std::env::temp_dir().join(format!(
            "flyonthewall-parakeet-{}.wav",
            uuid::Uuid::new_v4()
        ));
        fly_audio::mix::write_wav_mono_16(&wav_path, samples, rate)
            .map_err(|e| AsrError::Engine(format!("write chunk wav: {e}")))?;

        let mut cmd = tokio::process::Command::new(&self.exe);
        cmd.arg("-m")
            .arg(&self.model)
            .arg("-f")
            .arg(&wav_path)
            .arg("-t")
            .arg(self.threads.max(1).to_string())
            // token dump with word_start flags + timestamps (stderr)
            .arg("-ps");
        if self.force_cpu {
            cmd.arg("-ng");
        }
        // ggml loads its backend DLLs (ggml-cpu-*, ggml-vulkan) from the
        // current directory as well as the exe's: run in the exe's own
        // folder so only the pinned build's backends can ever be picked up.
        if let Some(dir) = self.exe.parent().filter(|d| d.is_dir()) {
            cmd.current_dir(dir);
        }
        #[cfg(windows)]
        {
            // CREATE_NO_WINDOW | BELOW_NORMAL_PRIORITY_CLASS, as for whisper-cli
            cmd.creation_flags(0x0800_0000 | 0x0000_4000);
        }

        let output = cmd
            .output()
            .await
            .map_err(|e| AsrError::Engine(format!("failed to launch parakeet-cli: {e}")));
        let _ = std::fs::remove_file(&wav_path);
        let output = output?;
        let stderr = String::from_utf8_lossy(&output.stderr);
        log_device_lines(&stderr);
        if !output.status.success() {
            return Err(AsrError::Engine(format!(
                "parakeet-cli exited with {}: {}",
                output.status,
                stderr
                    .lines()
                    .rev()
                    .take(8)
                    .collect::<Vec<_>>()
                    .into_iter()
                    .rev()
                    .collect::<Vec<_>>()
                    .join(" | ")
                    .chars()
                    .take(500)
                    .collect::<String>()
            )));
        }
        // The dump says how many tokens it holds ("Tokens [93]:"); if it
        // announces some and none parse, the format changed under us — fail
        // loudly instead of returning an empty batch that silently drops two
        // minutes of speech.
        if declared_tokens(&stderr) > 0 && !stderr.lines().any(|l| parse_token_line(l).is_some()) {
            return Err(AsrError::Engine(
                "parakeet-cli reported tokens but none parsed (token dump format changed?)".into(),
            ));
        }
        Ok(parse_parakeet_tokens(&stderr))
    }
}

/// Sum of the `Tokens [N]:` headers in a `-ps` dump (one per segment).
fn declared_tokens(stderr: &str) -> usize {
    stderr
        .lines()
        .filter_map(|l| l.trim().strip_prefix("Tokens ["))
        .filter_map(|rest| rest.split(']').next()?.trim().parse::<usize>().ok())
        .sum()
}

/// One encoder frame (80 ms): a re-decoded window starts off the batch's
/// frame grid, so the same word can come back up to a frame early or late.
const SEAM_TOLERANCE_MS: u64 = 80;
/// A recovered word this close to an original word with the same text is
/// that word decoded a second time, not new speech.
const SEAM_DUPLICATE_MS: u64 = 250;

fn word_key(text: &str) -> String {
    text.chars()
        .filter(|c| c.is_alphanumeric())
        .flat_map(char::to_lowercase)
        .collect()
}

/// The re-decoded words to splice into a hole (`from`..`to`): those that
/// start inside it (with a frame of tolerance on both edges) and are not a
/// second copy of an original neighbour word at the seam.
fn splice_recovered(
    original: &[Word],
    recovered: impl IntoIterator<Item = Word>,
    from: u64,
    to: u64,
) -> Vec<Word> {
    recovered
        .into_iter()
        .filter(|w| w.start_ms + SEAM_TOLERANCE_MS >= from && w.start_ms < to + SEAM_TOLERANCE_MS)
        .filter(|w| {
            let key = word_key(&w.text);
            !original.iter().any(|o| {
                o.start_ms.abs_diff(w.start_ms) <= SEAM_DUPLICATE_MS && word_key(&o.text) == key
            })
        })
        .collect()
}

/// A word-free stretch at least this long inside a batch is checked for
/// skipped speech. Measured on a real Teams meeting: parakeet-cli's greedy
/// TDT decoder can fall silent mid-input (sometimes for the rest of the
/// call) while the same audio decodes fully when the call starts
/// elsewhere — 73 words lost at one spot, 69 at another, in a 39 min
/// meeting. Padding the input with silence does not help.
const HOLE_MS: u64 = 5_000;
/// Context added on each side when a hole is re-decoded on its own.
const HOLE_PAD_MS: u64 = 1_000;
/// Analysis frame for the speech-energy check.
const FRAME_MS: usize = 100;
/// A hole counts as skipped speech when at least this share of its frames
/// carry speech-level energy.
const HOLE_SPEECH_SHARE: f64 = 0.4;

/// Word-free stretches (head, between words, tail) of ≥ [`HOLE_MS`] whose
/// audio is mostly speech-level energy — where the decoder skipped speech
/// rather than the speaker pausing. Batches are VAD-stitched speech, so a
/// genuine 5 s silence inside one is rare; the energy check guards it.
/// "Speech level" = a quarter of the batch's median non-silent frame RMS.
fn find_holes(words: &[Word], samples: &[f32], rate: u32) -> Vec<(u64, u64)> {
    let per_frame = (rate as usize * FRAME_MS / 1000).max(1);
    let rms: Vec<f64> = samples
        .chunks(per_frame)
        .map(|c| (c.iter().map(|x| (*x as f64).powi(2)).sum::<f64>() / c.len() as f64).sqrt())
        .collect();
    let mut voiced: Vec<f64> = rms.iter().copied().filter(|r| *r > 1e-3).collect();
    if voiced.is_empty() {
        return Vec::new();
    }
    voiced.sort_by(|a, b| a.total_cmp(b));
    let threshold = voiced[voiced.len() / 2] * 0.25;
    let total_ms = (samples.len() * 1000 / rate.max(1) as usize) as u64;

    let mut edges: Vec<(u64, u64)> = vec![(0, 0)];
    edges.extend(words.iter().map(|w| (w.start_ms, w.end_ms)));
    edges.push((total_ms, total_ms));
    let mut holes = Vec::new();
    let mut covered_to = 0u64;
    for (start, end) in edges {
        if start >= covered_to + HOLE_MS {
            let (a, b) = (covered_to as usize / FRAME_MS, start as usize / FRAME_MS);
            let frames = &rms[a.min(rms.len())..b.min(rms.len())];
            let speech = frames.iter().filter(|r| **r >= threshold).count();
            if !frames.is_empty() && speech as f64 / frames.len() as f64 >= HOLE_SPEECH_SHARE {
                holes.push((covered_to, start));
            }
        }
        covered_to = covered_to.max(end);
    }
    holes
}

fn log_device_lines(stderr: &str) {
    for line in stderr.lines() {
        let l = line.trim();
        let lower = l.to_ascii_lowercase();
        if lower.starts_with("ggml_vulkan")
            || lower.starts_with("ggml_metal")
            || lower.starts_with("parakeet_backend_init_gpu")
            || lower.contains("encode time")
        {
            tracing::debug!("parakeet device: {l}");
        }
    }
}

/// One token line of the `-ps` dump.
#[derive(Debug, PartialEq)]
struct Token<'a> {
    t0: u64,
    t1: u64,
    word_start: bool,
    text: &'a str,
}

/// Value of `key=` in a token line (whitespace after `=` allowed: the CLI
/// pads numbers, e.g. `t0= 912`).
fn field<'a>(line: &'a str, key: &str) -> Option<&'a str> {
    let at = line.find(key)? + key.len();
    line[at..].split_whitespace().next()
}

/// Parse one dump line:
/// `  [ 0] id= 2336 frame=114 … t0= 912 t1= 936 word_start=true "▁It"`.
fn parse_token_line(line: &str) -> Option<Token<'_>> {
    let l = line.trim_start();
    if !l.starts_with('[') || !l.contains(" id=") {
        return None;
    }
    let t0 = field(l, " t0=")?.parse().ok()?;
    let t1 = field(l, " t1=")?.parse().ok()?;
    let ws_at = l.find(" word_start=")? + " word_start=".len();
    let rest = &l[ws_at..];
    let (flag, quoted) = rest.split_once(' ')?;
    let word_start = match flag {
        "true" => true,
        "false" => false,
        _ => return None,
    };
    // the text is everything between the first and the LAST quote, so a
    // quote character inside the token survives
    let quoted = quoted.trim_end();
    let text = quoted.strip_prefix('"')?.strip_suffix('"')?;
    Some(Token {
        t0,
        t1,
        word_start,
        text,
    })
}

/// Build words from a `parakeet-cli -ps` stderr dump. Each token flagged
/// `word_start` (or carrying the `▁` marker) opens a new word; the others
/// append to the current one ("that" + "'" + "s" → "that's", "cool" + "."
/// → "cool."). A word spans its first token's `t0` to its last token's `t1`.
pub fn parse_parakeet_tokens(stderr: &str) -> Vec<Word> {
    let mut words: Vec<Word> = Vec::new();
    for tok in stderr.lines().filter_map(parse_token_line) {
        let start_ms = tok.t0 * TOKEN_TICK_MS;
        let end_ms = tok.t1.max(tok.t0) * TOKEN_TICK_MS;
        let opens = tok.word_start || tok.text.starts_with(SP_SPACE);
        let piece = tok.text.trim_start_matches(SP_SPACE);
        match words.last_mut() {
            Some(w) if !opens => {
                w.text.push_str(piece);
                w.end_ms = w.end_ms.max(end_ms);
            }
            _ => words.push(Word {
                text: piece.to_string(),
                start_ms,
                end_ms,
            }),
        }
    }
    words
        .into_iter()
        .map(|mut w| {
            w.text = crate::clean_word_text(&w.text);
            w
        })
        .filter(|w| !w.text.is_empty() && !crate::is_non_speech_token(&w.text))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Verbatim excerpt of a real `parakeet-cli -ps` run (whisper.cpp
    /// v1.9.5, ggml-parakeet-tdt-0.6b-v3-q8_0), surrounding log lines kept.
    const DUMP: &str = r#"parakeet_print_timings:    total time = 11409.31 ms

Segments (1):
Segment 0: [0 -> 815] "It's recording. Listen to you, and that's cool."
Tokens [93]:
  [ 0] id= 2336 frame=114 dur_idx= 3 dur_val= 3 p=0.9814 plog=-0.0187 t0= 912 t1= 936 word_start=true "▁It"
  [ 1] id= 7931 frame=117 dur_idx= 0 dur_val= 0 p=0.9990 plog=-0.0010 t0= 936 t1= 936 word_start=false "'"
  [ 2] id= 7870 frame=117 dur_idx= 2 dur_val= 2 p=0.9998 plog=-0.0002 t0= 936 t1= 952 word_start=false "s"
  [72] id=  439 frame=654 dur_idx= 1 dur_val= 1 p=0.5794 plog=-0.5458 t0=5232 t1=5240 word_start=true "▁L"
  [73] id= 2275 frame=655 dur_idx= 1 dur_val= 1 p=0.9985 plog=-0.0015 t0=5240 t1=5248 word_start=false "isten"
  [74] id=  366 frame=657 dur_idx= 1 dur_val= 1 p=0.5520 plog=-0.5943 t0=5256 t1=5264 word_start=true "▁to"
  [75] id= 1180 frame=659 dur_idx= 1 dur_val= 1 p=0.9689 plog=-0.0316 t0=5272 t1=5280 word_start=true "▁you"
  [76] id= 7877 frame=662 dur_idx= 1 dur_val= 1 p=0.3320 plog=-1.1028 t0=5280 t1=5280 word_start=false ","
  [78] id= 1050 frame=669 dur_idx= 1 dur_val= 1 p=0.9911 plog=-0.0089 t0=5352 t1=5360 word_start=true "▁that"
  [79] id= 7931 frame=670 dur_idx= 1 dur_val= 1 p=0.9883 plog=-0.0117 t0=5360 t1=5360 word_start=false "'"
  [80] id= 7870 frame=671 dur_idx= 1 dur_val= 1 p=0.9995 plog=-0.0005 t0=5368 t1=5376 word_start=false "s"
  [81] id=  867 frame=672 dur_idx= 1 dur_val= 1 p=0.9933 plog=-0.0067 t0=5376 t1=5384 word_start=true "▁co"
  [82] id=  324 frame=673 dur_idx= 1 dur_val= 1 p=1.0000 plog=-0.0000 t0=5384 t1=5392 word_start=false "ol"
  [83] id= 7883 frame=674 dur_idx= 2 dur_val= 2 p=0.5866 plog=-0.5334 t0=5392 t1=5392 word_start=false "."
"#;

    #[test]
    fn tokens_glue_into_timed_words() {
        let words = parse_parakeet_tokens(DUMP);
        let texts: Vec<&str> = words.iter().map(|w| w.text.as_str()).collect();
        assert_eq!(texts, ["It's", "Listen", "to", "you,", "that's", "cool."]);
        // t0/t1 are 10 ms ticks: "It's" = 912..952 → 9.12 s..9.52 s
        assert_eq!((words[0].start_ms, words[0].end_ms), (9120, 9520));
        assert_eq!((words[5].start_ms, words[5].end_ms), (53_760, 53_920));
        assert!(words.windows(2).all(|p| p[0].start_ms <= p[1].start_ms));
    }

    #[test]
    fn segment_and_log_lines_are_ignored() {
        assert!(parse_parakeet_tokens("Segment 0: [0 -> 815] \"hello world\"\n").is_empty());
        assert!(parse_parakeet_tokens("parakeet_model_load: n_vocab = 8192\n").is_empty());
    }

    #[test]
    fn quote_token_and_marker_without_flag() {
        let dump = concat!(
            "  [ 0] id=1 frame=1 t0= 10 t1= 20 word_start=true \"▁say\"\n",
            "  [ 1] id=2 frame=2 t0= 20 t1= 20 word_start=false \"\"\"\n",
            // marker present but flag false: still a word boundary
            "  [ 2] id=3 frame=3 t0= 30 t1= 40 word_start=false \"▁hi\"\n",
        );
        let words = parse_parakeet_tokens(dump);
        let texts: Vec<&str> = words.iter().map(|w| w.text.as_str()).collect();
        assert_eq!(texts, ["say\"", "hi"]);
        assert_eq!(words[1].start_ms, 300);
    }

    fn w(at_ms: u64) -> Word {
        Word {
            text: "x".into(),
            start_ms: at_ms,
            end_ms: at_ms + 300,
        }
    }

    /// 30 s of steady "speech" at 16 kHz with words only in the first 10 s:
    /// the 20 s word-free tail is a hole; the same tail as digital silence
    /// is a pause, not a hole.
    #[test]
    fn skipped_speech_is_a_hole_and_silence_is_not() {
        let rate = 16_000u32;
        let speech: Vec<f32> = (0..30 * rate as usize)
            .map(|i| (i as f32 * 0.05).sin() * 0.3)
            .collect();
        let words: Vec<Word> = (0..10).map(|s| w(s * 1000)).collect();
        assert_eq!(find_holes(&words, &speech, rate), [(9_300, 30_000)]);

        let mut paused = speech.clone();
        paused[10 * rate as usize..].fill(0.0);
        assert!(find_holes(&words, &paused, rate).is_empty());
    }

    #[test]
    fn short_gaps_and_dense_words_have_no_holes() {
        let rate = 16_000u32;
        let speech = vec![0.2f32; 20 * rate as usize];
        // a word every 4 s: gaps of 3.7 s < HOLE_MS
        let words: Vec<Word> = (0..5).map(|s| w(s * 4_000 + 1_000)).collect();
        assert!(find_holes(&words, &speech, rate).is_empty());
        // no words at all over speech: the whole batch is one hole
        assert_eq!(find_holes(&[], &speech, rate), [(0, 20_000)]);
    }

    fn wt(text: &str, at_ms: u64) -> Word {
        Word {
            text: text.into(),
            start_ms: at_ms,
            end_ms: at_ms + 300,
        }
    }

    /// The re-decoded window starts 40 ms off the frame grid, so the word
    /// AFTER the hole comes back at `to - 40` (and the one before at its own
    /// time): both are second copies and must not be spliced in; real new
    /// words inside the hole are.
    #[test]
    fn seam_copies_are_dropped_and_new_words_kept() {
        let original = [wt("before.", 9_000), wt("after", 20_000)];
        let recovered = vec![
            wt("before.", 8_960),   // copy of the word before the hole
            wt("budget", 10_400),   // new
            wt("approved", 15_000), // new
            wt("after", 19_960),    // copy at the seam (to - 40)
            wt("later", 21_500),    // outside the hole
        ];
        let kept = splice_recovered(&original, recovered, 9_300, 20_000);
        let texts: Vec<&str> = kept.iter().map(|w| w.text.as_str()).collect();
        assert_eq!(texts, ["budget", "approved"]);
    }

    /// A genuine word right at the hole's edge, up to a frame early, is kept.
    #[test]
    fn edge_words_within_a_frame_are_kept() {
        let original = [wt("one", 0), wt("three", 10_000)];
        let kept = splice_recovered(&original, vec![wt("two", 260)], 300, 10_000);
        assert_eq!(kept.len(), 1);
    }

    #[test]
    fn declared_tokens_sum_segment_headers() {
        assert_eq!(
            declared_tokens("Tokens [93]:\n  [ 0] x\nTokens [7]:\n"),
            100
        );
        assert_eq!(declared_tokens("no dump here"), 0);
        assert_eq!(declared_tokens(DUMP), 93);
    }

    #[test]
    fn model_tag_is_the_file_stem() {
        assert_eq!(
            model_tag_for(Path::new("models/asr/ggml-parakeet-tdt-0.6b-v3-q8_0.bin")).as_deref(),
            Some("ggml-parakeet-tdt-0.6b-v3-q8_0")
        );
    }

    /// With cancel already requested the engine aborts before launching the
    /// sidecar (the exe here doesn't exist — a launch would be a different
    /// error), exactly like whisper.
    #[tokio::test]
    async fn cancel_aborts_before_decoding() {
        let dir = tempfile::tempdir().unwrap();
        let rate = 16_000u32;
        let samples: Vec<f32> = (0..(2 * rate) as usize)
            .map(|i| (i as f32 * 440.0 * std::f32::consts::TAU / rate as f32).sin() * 0.4)
            .collect();
        let wav = dir.path().join("track.16k.wav");
        fly_audio::mix::write_wav_mono_16(&wav, &samples, rate).unwrap();
        let model = dir.path().join("model.bin");
        std::fs::write(&model, b"fake").unwrap();
        let engine = ParakeetEngine {
            exe: dir.path().join("does-not-exist.exe"),
            model,
            threads: 1,
            force_cpu: true,
            resume: false,
        };
        let err = engine
            .transcribe_with_progress(&wav, &TranscribeOptions::default(), &|_| {}, &|| true)
            .await
            .unwrap_err();
        assert!(matches!(err, AsrError::Cancelled), "{err:?}");
        // and without cancel, the missing sidecar is an engine error
        let err = engine
            .transcribe(&wav, &TranscribeOptions::default())
            .await
            .unwrap_err();
        assert!(matches!(err, AsrError::Engine(_)), "{err:?}");
    }
}
