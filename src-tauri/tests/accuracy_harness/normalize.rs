//! English scoring normalizer (Whisper-`EnglishTextNormalizer`-style) for the
//! accuracy harness. SCORING ONLY — stored transcripts are never touched.
//!
//! Engines and references disagree on surface form, not words: "it's" vs
//! "it is", "twenty-five" vs "25", "5%" vs "five percent", "okay" vs "OK".
//! Without this, a cross-engine comparison measures house style (Parakeet
//! writes digits and contractions differently from Whisper) instead of
//! recognition. Both the reference and the hypothesis go through the same
//! function, so any residual ambiguity ("'d" = had/would) cancels out.
//!
//! Steps, in order: lowercase → split on non-word characters (keeping
//! in-word apostrophes, digit separators and decimal points; `%`, `$`, `&`
//! become words) → expand contractions and spoken-style variants → drop
//! apostrophes → spelled-out numbers to digits ("twenty five" → "25",
//! "one hundred and five" → "105", "twenty first" → "21st") → drop
//! non-lexical fillers.

/// Unambiguous non-lexical fillers (never a content word).
pub fn is_filler(tok: &str) -> bool {
    matches!(tok, "um" | "uh" | "mm" | "mhm" | "hmm" | "erm")
}

/// The harness's original tokenizer, kept for `FLYONTHEWALL_HARNESS_NORMALIZE=0`
/// (reproduces pre-normalizer numbers): lowercase alphanumeric runs,
/// apostrophes dropped, other punctuation splits, fillers removed.
pub fn legacy_tokenize(s: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut cur = String::new();
    for c in s.chars() {
        if c.is_ascii_alphanumeric() {
            cur.push(c.to_ascii_lowercase());
        } else if c != '\'' && !cur.is_empty() {
            out.push(std::mem::take(&mut cur));
        }
    }
    if !cur.is_empty() {
        out.push(cur);
    }
    out.retain(|t| !is_filler(t));
    out
}

fn is_apostrophe(c: char) -> bool {
    c == '\'' || c == '\u{2019}' || c == '\u{2018}'
}

/// Raw word split. Keeps `don't`, `3.5`, `1,000` together; emits `percent`,
/// `dollars` (after the amount), `and` for `&`.
fn split_words(s: &str) -> Vec<String> {
    let chars: Vec<char> = s.chars().flat_map(char::to_lowercase).collect();
    let mut out: Vec<String> = Vec::new();
    let mut cur = String::new();
    let mut dollar = false;
    let flush = |cur: &mut String, out: &mut Vec<String>, dollar: &mut bool| {
        if cur.is_empty() {
            return;
        }
        let word = std::mem::take(cur);
        let numeric = word.chars().next().is_some_and(|c| c.is_ascii_digit());
        out.push(word);
        if *dollar && numeric {
            out.push("dollars".into());
        }
        *dollar = false;
    };
    for (i, &c) in chars.iter().enumerate() {
        let prev = i.checked_sub(1).map(|j| chars[j]);
        let next = chars.get(i + 1).copied();
        if c.is_alphanumeric() {
            cur.push(c);
        } else if is_apostrophe(c)
            && prev.is_some_and(char::is_alphanumeric)
            && next.is_some_and(char::is_alphanumeric)
        {
            cur.push('\'');
        } else if (c == '.' || c == ',')
            && prev.is_some_and(|p| p.is_ascii_digit())
            && next.is_some_and(|n| n.is_ascii_digit())
        {
            if c == '.' {
                cur.push('.'); // decimal point; thousands separators vanish
            }
        } else {
            flush(&mut cur, &mut out, &mut dollar);
            match c {
                '%' => out.push("percent".into()),
                '&' => out.push("and".into()),
                '$' => dollar = true,
                _ => {}
            }
        }
    }
    flush(&mut cur, &mut out, &mut dollar);
    out
}

/// Contractions and spoken-style variants → canonical words. Only forms
/// whose expansion is unambiguous enough for scoring; possessive 's just
/// loses its apostrophe (later step), matching "johns" on both sides.
fn expand(word: &str) -> Vec<&'static str> {
    let fixed: &[(&str, &[&'static str])] = &[
        ("won't", &["will", "not"]),
        ("can't", &["can", "not"]),
        ("cannot", &["can", "not"]),
        ("shan't", &["shall", "not"]),
        ("ain't", &["is", "not"]),
        ("let's", &["let", "us"]),
        ("it's", &["it", "is"]),
        ("that's", &["that", "is"]),
        ("there's", &["there", "is"]),
        ("here's", &["here", "is"]),
        ("what's", &["what", "is"]),
        ("where's", &["where", "is"]),
        ("who's", &["who", "is"]),
        ("how's", &["how", "is"]),
        ("he's", &["he", "is"]),
        ("she's", &["she", "is"]),
        ("gonna", &["going", "to"]),
        ("wanna", &["want", "to"]),
        ("gotta", &["got", "to"]),
        ("kinda", &["kind", "of"]),
        ("sorta", &["sort", "of"]),
        ("lemme", &["let", "me"]),
        ("gimme", &["give", "me"]),
        ("'cause", &["because"]),
        ("cuz", &["because"]),
        ("alright", &["all", "right"]),
        ("ok", &["okay"]),
        ("o.k", &["okay"]),
        ("yep", &["yes"]),
        ("yeah", &["yes"]),
        ("yup", &["yes"]),
    ];
    if let Some((_, to)) = fixed.iter().find(|(from, _)| *from == word) {
        return to.to_vec();
    }
    Vec::new()
}

/// Suffix contractions handled generically: X + n't / 're / 'll / 've / 'm / 'd.
fn expand_suffix(word: &str) -> Option<(String, &'static str)> {
    for (suffix, full) in [
        ("n't", "not"),
        ("'re", "are"),
        ("'ll", "will"),
        ("'ve", "have"),
        ("'m", "am"),
        ("'d", "would"),
    ] {
        if let Some(stem) = word.strip_suffix(suffix) {
            if !stem.is_empty() {
                return Some((stem.to_string(), full));
            }
        }
    }
    None
}

fn units(w: &str) -> Option<u64> {
    const U: [&str; 20] = [
        "zero",
        "one",
        "two",
        "three",
        "four",
        "five",
        "six",
        "seven",
        "eight",
        "nine",
        "ten",
        "eleven",
        "twelve",
        "thirteen",
        "fourteen",
        "fifteen",
        "sixteen",
        "seventeen",
        "eighteen",
        "nineteen",
    ];
    U.iter().position(|u| *u == w).map(|i| i as u64)
}

fn tens(w: &str) -> Option<u64> {
    const T: [&str; 8] = [
        "twenty", "thirty", "forty", "fifty", "sixty", "seventy", "eighty", "ninety",
    ];
    T.iter().position(|t| *t == w).map(|i| (i as u64 + 2) * 10)
}

fn scale(w: &str) -> Option<u64> {
    match w {
        "thousand" => Some(1_000),
        "million" => Some(1_000_000),
        "billion" => Some(1_000_000_000),
        _ => None,
    }
}

/// Ordinal number words → (value, suffix).
fn ordinal(w: &str) -> Option<u64> {
    const O: [(&str, u64); 21] = [
        ("first", 1),
        ("second", 2),
        ("third", 3),
        ("fourth", 4),
        ("fifth", 5),
        ("sixth", 6),
        ("seventh", 7),
        ("eighth", 8),
        ("ninth", 9),
        ("tenth", 10),
        ("eleventh", 11),
        ("twelfth", 12),
        ("thirteenth", 13),
        ("fourteenth", 14),
        ("fifteenth", 15),
        ("sixteenth", 16),
        ("seventeenth", 17),
        ("eighteenth", 18),
        ("nineteenth", 19),
        ("twentieth", 20),
        ("thirtieth", 30),
    ];
    O.iter().find(|(o, _)| *o == w).map(|(_, v)| *v)
}

fn ordinal_suffix(n: u64) -> &'static str {
    match (n % 10, n % 100) {
        (_, 11..=13) => "th",
        (1, _) => "st",
        (2, _) => "nd",
        (3, _) => "rd",
        _ => "th",
    }
}

/// Spelled-out numbers → digits. A new number starts whenever the next word
/// can't extend the current one ("five five" → "5 5", "twenty twenty six"
/// → "20 26": the same on both sides, so it scores consistently).
fn numbers_to_digits(words: Vec<(String, usize)>) -> Vec<(String, usize)> {
    #[derive(PartialEq, Clone, Copy)]
    enum Last {
        None,
        Unit,
        Tens,
        Hundred,
        Scale,
    }
    let mut out: Vec<(String, usize)> = Vec::with_capacity(words.len());
    // tag of the number being built = its first word's tag
    let (mut total, mut cur, mut last, mut tag) = (0u64, 0u64, Last::None, 0usize);
    let flush = |out: &mut Vec<(String, usize)>,
                 total: &mut u64,
                 cur: &mut u64,
                 last: &mut Last,
                 tag: usize| {
        if *last != Last::None {
            out.push(((*total + *cur).to_string(), tag));
        }
        (*total, *cur, *last) = (0, 0, Last::None);
    };
    let mut i = 0;
    while i < words.len() {
        let (w, wtag) = (words[i].0.as_str(), words[i].1);
        let digits: Option<u64> = w
            .bytes()
            .all(|b| b.is_ascii_digit())
            .then(|| w.parse().ok())
            .flatten();
        if let Some(u) = units(w) {
            let extends = matches!(last, Last::Hundred | Last::Scale)
                || (last == Last::Tens && u < 10 && u > 0);
            if !extends {
                flush(&mut out, &mut total, &mut cur, &mut last, tag);
            }
            if last == Last::None {
                tag = wtag;
            }
            cur += u;
            last = Last::Unit;
        } else if let Some(t) = tens(w) {
            if !matches!(last, Last::Hundred | Last::Scale) {
                flush(&mut out, &mut total, &mut cur, &mut last, tag);
            }
            if last == Last::None {
                tag = wtag;
            }
            cur += t;
            last = Last::Tens;
        } else if w == "hundred" && matches!(last, Last::Unit | Last::Tens) && cur % 100 != 0 {
            cur *= 100;
            last = Last::Hundred;
        } else if let (Some(s), true) = (scale(w), last != Last::None && last != Last::Scale) {
            total += cur.max(1) * s;
            cur = 0;
            last = Last::Scale;
        } else if let (Some(d), Some(s)) = (
            digits,
            words
                .get(i + 1)
                .and_then(|(n, _)| scale(n).or((n == "hundred").then_some(100))),
        ) {
            // "5 million" / "3 hundred" → one number, like "five million"
            flush(&mut out, &mut total, &mut cur, &mut last, tag);
            out.push(((d * s).to_string(), wtag));
            i += 2;
            continue;
        } else if w == "and"
            && matches!(last, Last::Hundred | Last::Scale)
            && words
                .get(i + 1)
                .is_some_and(|(n, _)| units(n).is_some() || tens(n).is_some())
        {
            // "one hundred and five": the "and" belongs to the number
        } else if let Some(o) = ordinal(w) {
            let (base, otag) = if matches!(last, Last::Tens | Last::Hundred | Last::Scale) {
                let b = total + cur;
                (total, cur, last) = (0, 0, Last::None);
                (b, tag)
            } else {
                flush(&mut out, &mut total, &mut cur, &mut last, tag);
                (0, wtag)
            };
            let n = base + o;
            out.push((format!("{n}{}", ordinal_suffix(n)), otag));
        } else {
            flush(&mut out, &mut total, &mut cur, &mut last, tag);
            out.push(words[i].clone());
        }
        i += 1;
    }
    flush(&mut out, &mut total, &mut cur, &mut last, tag);
    out
}

/// The full normalizer: text → scoring tokens.
pub fn normalize(s: &str) -> Vec<String> {
    normalize_stream(&[s]).into_iter().map(|(t, _)| t).collect()
}

/// Normalize a word STREAM (e.g. one transcript segment's ASR words) as a
/// whole, so multi-word forms ("twenty" + "five" → "25") merge across word
/// boundaries. Each output token carries the index of the input word it
/// came from (a merged number: its first word) — how the harness keeps
/// per-token timing and speaker.
pub fn normalize_stream(inputs: &[&str]) -> Vec<(String, usize)> {
    let mut words: Vec<(String, usize)> = Vec::new();
    for (idx, text) in inputs.iter().enumerate() {
        for w in split_words(text) {
            let expanded = expand(&w);
            if !expanded.is_empty() {
                words.extend(expanded.into_iter().map(|e| (e.to_string(), idx)));
            } else if let Some((stem, full)) = expand_suffix(&w) {
                // "don't" → "do not"; "can't" etc. were caught above
                words.push((stem.replace('\'', ""), idx));
                words.push((full.to_string(), idx));
            } else if let Some((int, frac)) = w.split_once('.') {
                // "3.5" → "3 point 5", matching "three point five"
                for t in [int, "point", frac] {
                    words.push((t.to_string(), idx));
                }
            } else {
                words.push((w.replace('\'', ""), idx));
            }
        }
    }
    words.retain(|(w, _)| !w.is_empty() && !is_filler(w));
    numbers_to_digits(words)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn n(s: &str) -> String {
        normalize(s).join(" ")
    }

    #[test]
    fn contractions_expand_identically_on_both_sides() {
        assert_eq!(n("It's fine, don't worry."), "it is fine do not worry");
        assert_eq!(n("it is fine do not worry"), "it is fine do not worry");
        assert_eq!(
            n("We'll see, they're here, I'm in."),
            "we will see they are here i am in"
        );
        assert_eq!(n("I can't, won't."), "i can not will not");
        assert_eq!(n("John's car"), "johns car");
        assert_eq!(
            n("gonna wanna OK alright"),
            "going to want to okay all right"
        );
        // curly apostrophes (Teams/Fathom exports)
        assert_eq!(n("it\u{2019}s"), "it is");
    }

    #[test]
    fn spelled_numbers_become_digits() {
        assert_eq!(n("twenty-five people"), "25 people");
        assert_eq!(n("25 people"), "25 people");
        assert_eq!(n("one hundred and five"), "105");
        assert_eq!(n("two thousand three hundred"), "2300");
        assert_eq!(n("1,000 users"), "1000 users");
        assert_eq!(n("five million"), "5000000");
        assert_eq!(n("5 million"), "5000000");
        assert_eq!(n("three point five"), "3 point 5");
        assert_eq!(n("3.5"), "3 point 5");
        assert_eq!(n("the twenty first"), "the 21st");
        assert_eq!(n("first second"), "1st 2nd");
        assert_eq!(n("five five five"), "5 5 5");
    }

    #[test]
    fn symbols_become_words() {
        assert_eq!(n("5%"), "5 percent");
        assert_eq!(n("five percent"), "5 percent");
        assert_eq!(n("$40"), "40 dollars");
        assert_eq!(n("forty dollars"), "40 dollars");
        assert_eq!(n("R&D"), "r and d");
    }

    #[test]
    fn hyphens_and_fillers() {
        assert_eq!(n("back-to-back"), "back to back");
        assert_eq!(n("Um, uh, so, yeah."), "so yes");
    }

    #[test]
    fn plain_words_are_untouched_and_and_survives() {
        assert_eq!(n("salt and pepper"), "salt and pepper");
        assert_eq!(n("one and two"), "1 and 2");
    }

    #[test]
    fn stream_merges_numbers_across_words_and_keeps_tags() {
        let out = normalize_stream(&["about", "twenty", "five,", "it's", "fine"]);
        let toks: Vec<(&str, usize)> = out.iter().map(|(t, i)| (t.as_str(), *i)).collect();
        assert_eq!(
            toks,
            [("about", 0), ("25", 1), ("it", 3), ("is", 3), ("fine", 4)]
        );
    }

    #[test]
    fn legacy_tokenizer_unchanged() {
        assert_eq!(
            legacy_tokenize("It's back-to-back, um, 25%!"),
            ["its", "back", "to", "back", "25"]
        );
    }
}
