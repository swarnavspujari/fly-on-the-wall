//! Corpus helpers for the accuracy harness:
//! - the GOLD excerpt format (a hand-corrected `[mm:ss] Speaker: text` file,
//!   `[inaudible]` lines excluded from scoring) and its draft exporter;
//! - Teams-folder discovery (video + `.vtt` paired by title prefix, audio
//!   extracted to a 16 kHz cache — the originals are never touched);
//! - engine-vs-engine agreement (one transcript scored as the other's
//!   reference), for meetings that have no human reference.

use std::path::{Path, PathBuf};

use fly_core::Transcript;

use super::{parse_mmss, RefTurn, Reference};

// ---------------------------------------------------------------------------
// Gold excerpt
// ---------------------------------------------------------------------------

/// `[mm:ss]` / `[h:mm:ss]` at the start of a gold/draft line → (ms, rest).
fn line_stamp(line: &str) -> Option<(u64, &str)> {
    let rest = line.strip_prefix('[')?;
    let (ts, rest) = rest.split_once(']')?;
    Some((parse_mmss(ts.trim())?, rest.trim_start()))
}

/// True when a text file is in the gold/draft line format (first content
/// line starts with a `[mm:ss]` stamp).
pub fn looks_like_gold(raw: &str) -> bool {
    raw.lines()
        .map(str::trim)
        .find(|l| !l.is_empty() && !l.starts_with('#'))
        .is_some_and(|l| line_stamp(l).is_some())
}

/// Parse a gold (or draft) excerpt. One utterance per stamped line,
/// `Speaker: text`; unstamped lines continue the previous utterance; `#`
/// lines are comments. A line containing `[inaudible]` is excluded from
/// scoring along with every hypothesis word from its stamp to the next
/// line's stamp — the reference can't say what was there, so neither side
/// may count it. Words are aligned globally, so the user may split or merge
/// lines freely.
pub fn parse_gold(raw: &str, tokenize: impl Fn(&str) -> Vec<String>) -> Reference {
    struct Line {
        start_ms: u64,
        speaker: String,
        text: String,
    }
    let mut lines: Vec<Line> = Vec::new();
    for l in raw.lines().map(str::trim) {
        if l.is_empty() || l.starts_with('#') {
            continue;
        }
        if let Some((start_ms, rest)) = line_stamp(l) {
            let (speaker, text) = rest.split_once(':').unwrap_or(("Unknown", rest));
            lines.push(Line {
                start_ms,
                speaker: speaker.trim().to_string(),
                text: text.trim().to_string(),
            });
        } else if let Some(last) = lines.last_mut() {
            last.text.push(' ');
            last.text.push_str(l);
        }
    }
    let mut speakers: Vec<String> = Vec::new();
    let mut tokens = Vec::new();
    let mut exclude = Vec::new();
    for (i, line) in lines.iter().enumerate() {
        let next = lines.get(i + 1).map(|n| n.start_ms).unwrap_or(u64::MAX);
        if line.text.to_ascii_lowercase().contains("[inaudible]") {
            exclude.push((line.start_ms, next.max(line.start_ms + 1)));
            continue;
        }
        let idx = speakers
            .iter()
            .position(|s| *s == line.speaker)
            .unwrap_or_else(|| {
                speakers.push(line.speaker.clone());
                speakers.len() - 1
            });
        for tok in tokenize(&line.text) {
            tokens.push((tok, idx));
        }
    }
    assert!(!tokens.is_empty(), "gold excerpt parsed to zero words");
    Reference {
        speakers,
        tokens,
        // stamps are line starts only: no utterance ends → no DER
        turns: Vec::new(),
        exclude,
    }
}

fn mmss(ms: u64) -> String {
    let s = ms / 1000;
    if s >= 3600 {
        format!("{}:{:02}:{:02}", s / 3600, s / 60 % 60, s % 60)
    } else {
        format!("{:02}:{:02}", s / 60, s % 60)
    }
}

/// The draft the user corrects: header comments, then one `[mm:ss]
/// Speaker: text` line per transcript segment, times relative to the clip
/// start (the transcript was produced from the clip window itself).
pub fn render_draft(t: &Transcript, meeting: &str, start_secs: u64, secs: u64) -> String {
    let mut out = String::new();
    out.push_str("# Fly on the Wall gold excerpt draft. Correct the words and speaker names,\n");
    out.push_str("# then save as gold.txt beside this file (see HOW-TO.md).\n");
    out.push_str(&format!("# meeting: {meeting}\n"));
    out.push_str(&format!(
        "# window: starts {} into the recording, {} long; times below are relative to the clip start\n",
        mmss(start_secs * 1000),
        mmss(secs * 1000)
    ));
    out.push_str(&format!("# draft engine: {}\n", t.engine));
    // Words of ALL segments in time order, a new line at every speaker
    // change: overlapping segments (you + far end talking over each other)
    // then read in the same order the scorer aligns them in.
    let mut words: Vec<(&str, &fly_core::Word)> = t
        .segments
        .iter()
        .flat_map(|s| s.words.iter().map(move |w| (s.speaker_key.as_str(), w)))
        .collect();
    words.sort_by_key(|(_, w)| w.start_ms);
    let mut i = 0;
    while i < words.len() {
        let key = words[i].0;
        let j = (i..words.len())
            .find(|&j| words[j].0 != key)
            .unwrap_or(words.len());
        let run: Vec<fly_core::Word> = words[i..j].iter().map(|(_, w)| (*w).clone()).collect();
        let label = t.label_for(key);
        for (start_ms, text) in draft_lines(&run) {
            out.push_str(&format!("[{}] {label}: {text}\n", mmss(start_ms)));
        }
        i = j;
    }
    out
}

/// Split one segment's words into correction-sized lines: break after a
/// sentence end once a line has `MIN_LINE_WORDS`, and always by
/// `MAX_LINE_WORDS`. Each line keeps its first word's start time.
fn draft_lines(words: &[fly_core::Word]) -> Vec<(u64, String)> {
    const MIN_LINE_WORDS: usize = 8;
    const MAX_LINE_WORDS: usize = 30;
    let mut out = Vec::new();
    let mut line: Vec<&str> = Vec::new();
    let mut start = 0;
    for w in words {
        let text = w.text.trim();
        if text.is_empty() {
            continue;
        }
        if line.is_empty() {
            start = w.start_ms;
        }
        line.push(text);
        let sentence_end = text.ends_with(['.', '?', '!']);
        if (sentence_end && line.len() >= MIN_LINE_WORDS) || line.len() >= MAX_LINE_WORDS {
            out.push((start, line.join(" ")));
            line.clear();
        }
    }
    if !line.is_empty() {
        out.push((start, line.join(" ")));
    }
    out
}

// ---------------------------------------------------------------------------
// Engine-vs-engine agreement
// ---------------------------------------------------------------------------

/// Treat a transcript as a reference: speaker keys become reference
/// speakers (the mic channel, when present, is named "mic" so `ref_self`
/// "mic" pins it), segments become timed turns (so DER-vs-each-other works).
pub fn reference_from_transcript(
    t: &Transcript,
    normalize_stream: impl Fn(&[&str]) -> Vec<(String, usize)>,
) -> Reference {
    let mut speakers: Vec<String> = Vec::new();
    let mut idx_of = |key: &str| {
        speakers.iter().position(|k| k == key).unwrap_or_else(|| {
            speakers.push(key.to_string());
            speakers.len() - 1
        })
    };
    let mut segs: Vec<&fly_core::TranscriptSegment> = t.segments.iter().collect();
    segs.sort_by_key(|s| s.start_ms);
    let turns: Vec<RefTurn> = segs
        .iter()
        .map(|s| RefTurn {
            speaker: idx_of(&s.speaker_key),
            start_ms: s.start_ms,
            end_ms: s.end_ms,
        })
        .collect();
    // tokens from ALL words in time order (overlapping segments of a
    // two-channel recording interleave exactly as the scorer orders the
    // hypothesis), normalized per same-speaker run so numbers still merge
    let mut words: Vec<(usize, &str, u64)> = t
        .segments
        .iter()
        .flat_map(|s| {
            let i = idx_of(&s.speaker_key);
            s.words
                .iter()
                .map(move |w| (i, w.text.as_str(), w.start_ms))
        })
        .collect();
    words.sort_by_key(|(_, _, at)| *at);
    let mut tokens = Vec::new();
    let mut i = 0;
    while i < words.len() {
        let spk = words[i].0;
        let j = (i..words.len())
            .find(|&j| words[j].0 != spk)
            .unwrap_or(words.len());
        let texts: Vec<&str> = words[i..j].iter().map(|(_, t, _)| *t).collect();
        for (tok, _) in normalize_stream(&texts) {
            tokens.push((tok, spk));
        }
        i = j;
    }
    Reference {
        speakers,
        tokens,
        turns,
        exclude: Vec::new(),
    }
}

// ---------------------------------------------------------------------------
// Teams folder discovery
// ---------------------------------------------------------------------------

/// One discovered Teams meeting: the original video and its transcript.
pub struct TeamsMeeting {
    pub title: String,
    pub media: PathBuf,
    pub vtt: PathBuf,
}

/// `.vtt` stem without a trailing " (n)" download-duplicate counter.
fn vtt_title(stem: &str) -> &str {
    let s = stem.trim_end();
    if let Some(open) = s.rfind(" (") {
        let inner = &s[open + 2..];
        if inner.ends_with(')') && inner[..inner.len() - 1].bytes().all(|b| b.is_ascii_digit()) {
            return &s[..open];
        }
    }
    s
}

/// Pair every video/audio file in `dir` with the `.vtt` whose title (stem
/// minus any " (n)" counter) prefixes the media file name. Longest title
/// wins; unpaired files are skipped. Sorted by title for stable numbering.
pub fn discover_teams(dir: &Path) -> Vec<TeamsMeeting> {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return Vec::new();
    };
    let files: Vec<PathBuf> = entries.flatten().map(|e| e.path()).collect();
    let ext = |p: &Path| {
        p.extension()
            .map(|e| e.to_string_lossy().to_ascii_lowercase())
            .unwrap_or_default()
    };
    let vtts: Vec<(&str, &PathBuf)> = files
        .iter()
        .filter(|p| ext(p) == "vtt")
        .filter_map(|p| Some((vtt_title(p.file_stem()?.to_str()?), p)))
        .collect();
    let mut out: Vec<TeamsMeeting> = files
        .iter()
        .filter(|p| matches!(ext(p).as_str(), "mp4" | "m4a" | "wav" | "mkv" | "webm"))
        .filter_map(|media| {
            let name = media.file_name()?.to_str()?;
            let (title, vtt) = vtts
                .iter()
                .filter(|(title, _)| name.starts_with(title))
                .max_by_key(|(title, _)| title.len())?;
            Some(TeamsMeeting {
                title: title.to_string(),
                media: media.clone(),
                vtt: (*vtt).clone(),
            })
        })
        .collect();
    out.sort_by(|a, b| a.title.cmp(&b.title));
    out
}

/// Extract (once) a 16 kHz mono `recording.mixed.wav` from `media` into
/// `dir` with ffmpeg. Reads the original, writes only into `dir`.
pub fn extract_mixed(ffmpeg: &Path, media: &Path, dir: &Path) -> Result<PathBuf, String> {
    let dst = dir.join("recording.mixed.wav");
    if dst.exists() {
        return Ok(dst);
    }
    std::fs::create_dir_all(dir).map_err(|e| e.to_string())?;
    let tmp = dir.join("recording.mixed.partial.wav");
    let status = std::process::Command::new(ffmpeg)
        .args(["-loglevel", "error", "-y", "-i"])
        .arg(media)
        .args(["-vn", "-ac", "1", "-ar", "16000", "-c:a", "pcm_s16le"])
        .arg(&tmp)
        .status()
        .map_err(|e| format!("launch ffmpeg: {e}"))?;
    if !status.success() {
        return Err(format!("ffmpeg failed on {}", media.display()));
    }
    std::fs::rename(&tmp, &dst).map_err(|e| e.to_string())?;
    Ok(dst)
}

/// Encode the listening clip for the gold excerpt (AAC in .m4a).
pub fn export_clip(
    ffmpeg: &Path,
    src: &Path,
    start_secs: u64,
    secs: u64,
    dst: &Path,
) -> Result<(), String> {
    let status = std::process::Command::new(ffmpeg)
        .args(["-loglevel", "error", "-y", "-ss"])
        .arg(start_secs.to_string())
        .arg("-t")
        .arg(secs.to_string())
        .arg("-i")
        .arg(src)
        .args(["-vn", "-c:a", "aac", "-b:a", "96k"])
        .arg(dst)
        .status()
        .map_err(|e| format!("launch ffmpeg: {e}"))?;
    status
        .success()
        .then_some(())
        .ok_or_else(|| format!("ffmpeg failed encoding {}", dst.display()))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tok(s: &str) -> Vec<String> {
        super::super::normalize::normalize(s)
    }

    #[test]
    fn gold_lines_parse_with_continuations_comments_and_inaudible() {
        let raw = "# header\n\
                   [00:03] Dana: I can send two pricing options\n\
                   by Thursday.\n\
                   [00:09] You: [inaudible] maybe\n\
                   [00:15] Dana: Great, $8,400 it is.\n";
        assert!(looks_like_gold(raw));
        let r = parse_gold(raw, tok);
        assert_eq!(r.speakers, ["Dana"]);
        let toks: Vec<&str> = r.tokens.iter().map(|(t, _)| t.as_str()).collect();
        assert_eq!(
            toks,
            [
                "i", "can", "send", "2", "pricing", "options", "by", "thursday", "great", "8400",
                "dollars", "it", "is"
            ]
        );
        assert_eq!(r.exclude, [(9_000, 15_000)]);
    }

    #[test]
    fn draft_lines_break_at_sentences_with_a_floor_and_a_cap() {
        let words: Vec<fly_core::Word> = "a b c d e f g h. i j. k"
            .split(' ')
            .enumerate()
            .map(|(i, t)| fly_core::Word {
                text: t.into(),
                start_ms: i as u64 * 1000,
                end_ms: i as u64 * 1000 + 500,
            })
            .collect();
        // "j." ends a sentence but its line is too short to break there
        assert_eq!(
            draft_lines(&words),
            [
                (0, "a b c d e f g h.".to_string()),
                (8000, "i j. k".to_string())
            ]
        );
        let long: Vec<fly_core::Word> = (0..65)
            .map(|i| fly_core::Word {
                text: "w".into(),
                start_ms: i,
                end_ms: i,
            })
            .collect();
        assert_eq!(draft_lines(&long).len(), 3);
    }

    /// Overlapping segments (you talking over the far end) must produce the
    /// same token order on the reference side as the scorer's hypothesis
    /// side, so a transcript agrees with itself exactly.
    #[test]
    fn transcript_agrees_with_itself_despite_overlap() {
        let seg = |key: &str, words: &[(&str, u64)]| fly_core::TranscriptSegment {
            id: key.into(),
            speaker_key: key.into(),
            start_ms: words[0].1,
            end_ms: words[words.len() - 1].1 + 300,
            text: String::new(),
            words: words
                .iter()
                .map(|(t, at)| fly_core::Word {
                    text: (*t).into(),
                    start_ms: *at,
                    end_ms: at + 300,
                })
                .collect(),
        };
        let t = Transcript {
            meeting_id: "m".into(),
            language: None,
            engine: "x".into(),
            segments: vec![
                seg(
                    "mic",
                    &[("so", 0), ("twenty", 2_000), ("five", 2_400), ("ok", 8_000)],
                ),
                seg("spk_0", &[("right", 1_000), ("yes", 5_000)]),
            ],
            speakers: vec![],
        };
        let r = reference_from_transcript(&t, super::super::normalize::normalize_stream);
        let hyp: Vec<String> = super::super::hyp_words(&t, |_| true)
            .into_iter()
            .map(|w| w.tok)
            .collect();
        let refs: Vec<String> = r.tokens.iter().map(|(t, _)| t.clone()).collect();
        assert_eq!(refs, hyp);
        assert_eq!(refs, ["so", "right", "25", "yes", "okay"]);
    }

    #[test]
    fn fathom_text_is_not_gold() {
        assert!(!looks_like_gold("Meeting title\n0:03 - Dana\n  hello\n"));
    }

    #[test]
    fn vtt_titles_drop_download_counters() {
        assert_eq!(vtt_title("Budget Check-in (1)"), "Budget Check-in");
        assert_eq!(
            vtt_title("Vendor Review (Phase 2) (2)"),
            "Vendor Review (Phase 2)"
        );
        assert_eq!(vtt_title("Plain (draft)"), "Plain (draft)");
    }

    #[test]
    fn teams_media_pairs_with_its_vtt_by_title_prefix() {
        let dir = tempfile::tempdir().unwrap();
        for f in [
            "Quarterly Planning Sync-20260101_090000-Meeting Recording.mp4",
            "Quarterly Planning Sync.vtt",
            "Vendor Review (Phase 2)-20260102_140000-Meeting Recording.mp4",
            "Vendor Review (Phase 2) (2).vtt",
            "orphan.mp4",
        ] {
            std::fs::write(dir.path().join(f), b"x").unwrap();
        }
        let found = discover_teams(dir.path());
        let titles: Vec<&str> = found.iter().map(|m| m.title.as_str()).collect();
        assert_eq!(
            titles,
            ["Quarterly Planning Sync", "Vendor Review (Phase 2)"]
        );
        assert!(found[1].vtt.ends_with("Vendor Review (Phase 2) (2).vtt"));
    }
}
