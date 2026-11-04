//! The extraction runaway guard against a real model's loop, captured.
//!
//! `fixtures/llama31_extraction_loop.txt` is the start of what llama3.1
//! (Ollama, temperature 0, the extraction grammar) actually wrote for an
//! invented eight-line kickoff meeting: a handful of items, then the same
//! two action items again and again with the array never closing, until the
//! token cap (at 200 s it was past 1,300 stream pieces and still going). The
//! file stops where the capture was cut mid-element, as a stream does.
//! tests/extraction_runaway_live.rs re-creates the loop against a real
//! Ollama; this one needs nothing and runs in CI.

use fly_app_lib::extraction::{salvage_items, Runaway, RunawayGuard};

const CAPTURED_LOOP: &str = include_str!("fixtures/llama31_extraction_loop.txt");

/// Feed `text` the way a stream delivers it, `piece` bytes at a time (kept on
/// character boundaries). Returns what the guard said, and how much it had
/// been given when it said it.
fn stream_into_guard(text: &str, piece: usize, max_items: usize) -> (Option<Runaway>, String) {
    let mut guard = RunawayGuard::new(max_items);
    let mut at = 0;
    while at < text.len() {
        let mut end = (at + piece).min(text.len());
        while !text.is_char_boundary(end) {
            end += 1;
        }
        if let Some(why) = guard.push(&text[at..end]) {
            return (Some(why), guard.text().to_string());
        }
        at = end;
    }
    (None, guard.text().to_string())
}

#[test]
fn a_real_llama_loop_is_cut_at_the_third_repeat() {
    // the captured reply has 14 whole elements and the start of a 15th
    assert_eq!(salvage_items(CAPTURED_LOOP).len(), 14);

    for piece in [1, 3, 11, 64] {
        let (why, seen) = stream_into_guard(CAPTURED_LOOP, piece, 21);
        assert_eq!(why, Some(Runaway::Repeating), "piece size {piece}");
        // eight distinct items, then the third repeat (the 11th element) ends it
        let written = salvage_items(&seen);
        assert!(
            (11..=12).contains(&written.len()),
            "piece size {piece}: cut after {} elements",
            written.len()
        );
        // it was cut well before the end of the capture, which is itself
        // only the first 3.3 kB of a loop that ran on to the token cap
        assert!(seen.len() < CAPTURED_LOOP.len(), "piece size {piece}");
        let mut distinct: Vec<_> = written.iter().map(|i| i.text.as_str()).collect();
        distinct.sort_unstable();
        distinct.dedup();
        assert_eq!(distinct.len(), 8, "piece size {piece}");
    }
}

#[test]
fn the_first_part_of_the_capture_alone_is_not_a_loop() {
    // up to the sixth element it is a normal reply
    let upto_six = {
        let mut end = 0;
        for _ in 0..6 {
            end += CAPTURED_LOOP[end..].find("\n  }").unwrap() + 4;
        }
        &CAPTURED_LOOP[..end]
    };
    assert_eq!(stream_into_guard(upto_six, 5, 21).0, None);
}
