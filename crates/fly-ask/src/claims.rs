//! The claim check (spec §4.2a #1): every number, date and proper name in
//! a sentence shown as sourced must appear in the segments it cites, and a
//! quoted phrase must appear in them word for word.
//!
//! * **Numbers** compare by value, with spoken forms normalized: "sixty
//!   three hundred" = 6300 = "$6,300", but not "$63k"; "four point six
//!   million" = 4.6M; "twenty-sixth" = 26; "fifty two eighty three" can
//!   also mean 5283 (both readings are kept). A spoken number never runs
//!   across punctuation. A lone "one", "first" or "second" is ordinary
//!   English, not a claim.
//! * **Dates** are month and weekday names.
//! * **Names** are capitalized words outside a short list of common words;
//!   a name matches the cited text or the cited segment's speaker label
//!   (attribution: "Dana said …" needs a Dana).
//! * **Quotes** of three or more words inside quotation marks must appear in
//!   one cited segment (case, spacing and punctuation ignored).
//!
//! These are the same rules the system evals score with; this module is
//! the product's own implementation of them.

use std::collections::BTreeSet;

use serde::{Deserialize, Serialize};

const UNITS: &[(&str, f64)] = &[
    ("zero", 0.0),
    ("one", 1.0),
    ("two", 2.0),
    ("three", 3.0),
    ("four", 4.0),
    ("five", 5.0),
    ("six", 6.0),
    ("seven", 7.0),
    ("eight", 8.0),
    ("nine", 9.0),
    ("ten", 10.0),
    ("eleven", 11.0),
    ("twelve", 12.0),
    ("thirteen", 13.0),
    ("fourteen", 14.0),
    ("fifteen", 15.0),
    ("sixteen", 16.0),
    ("seventeen", 17.0),
    ("eighteen", 18.0),
    ("nineteen", 19.0),
];
const TENS: &[(&str, f64)] = &[
    ("twenty", 20.0),
    ("thirty", 30.0),
    ("forty", 40.0),
    ("fifty", 50.0),
    ("sixty", 60.0),
    ("seventy", 70.0),
    ("eighty", 80.0),
    ("ninety", 90.0),
];
const ORDINALS: &[(&str, f64)] = &[
    ("first", 1.0),
    ("second", 2.0),
    ("third", 3.0),
    ("fourth", 4.0),
    ("fifth", 5.0),
    ("sixth", 6.0),
    ("seventh", 7.0),
    ("eighth", 8.0),
    ("ninth", 9.0),
    ("tenth", 10.0),
    ("eleventh", 11.0),
    ("twelfth", 12.0),
    ("thirteenth", 13.0),
    ("fourteenth", 14.0),
    ("fifteenth", 15.0),
    ("sixteenth", 16.0),
    ("seventeenth", 17.0),
    ("eighteenth", 18.0),
    ("nineteenth", 19.0),
    ("twentieth", 20.0),
    ("thirtieth", 30.0),
];
const SCALES: &[(&str, f64)] = &[
    ("hundred", 100.0),
    ("thousand", 1_000.0),
    ("million", 1_000_000.0),
    ("billion", 1_000_000_000.0),
];
const MONTHS: &[&str] = &[
    "january",
    "february",
    "march",
    "april",
    "may",
    "june",
    "july",
    "august",
    "september",
    "october",
    "november",
    "december",
];
const WEEKDAYS: &[&str] = &[
    "monday",
    "tuesday",
    "wednesday",
    "thursday",
    "friday",
    "saturday",
    "sunday",
];
/// Capitalized words that aren't names: sentence starters, pronouns,
/// function words and answer framing.
const COMMON: &[&str] = &[
    "a",
    "an",
    "the",
    "and",
    "but",
    "or",
    "so",
    "yes",
    "no",
    "not",
    "it",
    "it's",
    "its",
    "they",
    "them",
    "their",
    "we",
    "we're",
    "we'll",
    "our",
    "us",
    "you",
    "your",
    "i",
    "i'm",
    "i'll",
    "i'd",
    "he",
    "she",
    "his",
    "her",
    "this",
    "that",
    "these",
    "those",
    "there",
    "here",
    "then",
    "than",
    "what",
    "when",
    "where",
    "who",
    "why",
    "how",
    "which",
    "if",
    "on",
    "in",
    "at",
    "by",
    "for",
    "from",
    "to",
    "with",
    "without",
    "after",
    "before",
    "about",
    "around",
    "as",
    "of",
    "also",
    "all",
    "any",
    "each",
    "every",
    "some",
    "both",
    "none",
    "nothing",
    "still",
    "just",
    "only",
    "okay",
    "ok",
    "sure",
    "great",
    "good",
    "fine",
    "thanks",
    "right",
    "next",
    "last",
    "until",
    "once",
    "while",
    "because",
    "since",
    "per",
    "nobody",
    "everyone",
    "everything",
    "according",
    "based",
    "currently",
    "today",
    "tomorrow",
    "yesterday",
    "now",
    "later",
    "note",
    "summary",
    "answer",
    "sources",
    "source",
    "see",
];

fn lookup(table: &[(&str, f64)], w: &str) -> Option<f64> {
    table.iter().find(|(k, _)| *k == w).map(|(_, v)| *v)
}

/// Ordinals, including the tens forms ("fortieth").
fn ordinal(w: &str) -> Option<f64> {
    lookup(ORDINALS, w).or_else(|| {
        TENS.iter()
            .find(|(t, _)| w == format!("{}ieth", &t[..t.len() - 1]))
            .map(|(_, v)| *v)
    })
}

fn is_number_word(w: &str) -> bool {
    lookup(UNITS, w).is_some()
        || lookup(TENS, w).is_some()
        || lookup(SCALES, w).is_some()
        || ordinal(w).is_some()
}

/// Words with surrounding punctuation trimmed (hyphens, slashes and dashes
/// split words; apostrophes, `$`, `%`, `.` and `,` inside a word stay),
/// each with whether punctuation ended it.
fn clause_words(text: &str) -> Vec<(String, bool)> {
    // Keep these splitting rules in step with the eval scorer's: a looser
    // reading of the SOURCE side would accept numbers the scorer doesn't.
    text.split(|c: char| c.is_whitespace() || c == '-' || c == '/' || c == '—')
        .filter_map(|raw| {
            let tail = raw.trim_end_matches(['"', '\'', ')', ']', '*', '_']);
            let breaks = tail.ends_with(['.', ',', ':', ';', '!', '?']);
            let w = raw
                .trim_matches(|c: char| {
                    !(c.is_alphanumeric() || matches!(c, '$' | '%' | '.' | ',' | '\''))
                })
                .trim_end_matches(['.', ',', '\''])
                .to_string();
            (!w.is_empty()).then_some((w, breaks))
        })
        .collect()
}

fn words(text: &str) -> Vec<String> {
    clause_words(text).into_iter().map(|(w, _)| w).collect()
}

/// "6,300" / "$6,300" / "$63k" / "4.6M" / "106%" / "26th" → value.
fn digit_value(w: &str) -> Option<f64> {
    let w = w.trim_start_matches('$').trim_end_matches('%');
    if !w.starts_with(|c: char| c.is_ascii_digit()) {
        return None;
    }
    let lower = w.to_lowercase();
    let (body, mult) = if let Some(b) = lower.strip_suffix("bn") {
        (b.to_string(), 1e9)
    } else if let Some(b) = lower.strip_suffix('k') {
        (b.to_string(), 1e3)
    } else if let Some(b) = lower.strip_suffix('m') {
        (b.to_string(), 1e6)
    } else {
        let b = ["st", "nd", "rd", "th"]
            .iter()
            .find_map(|s| lower.strip_suffix(s))
            .unwrap_or(&lower);
        (b.to_string(), 1.0)
    };
    body.replace(',', "").parse::<f64>().ok().map(|v| v * mult)
}

/// Every number a text states, as values ×1000 (a spoken run of digit
/// groups yields both readings).
pub fn numbers(text: &str) -> BTreeSet<u64> {
    let cw = clause_words(text);
    let ws: Vec<String> = cw.iter().map(|(w, _)| w.to_lowercase()).collect();
    let ends: Vec<bool> = cw.iter().map(|(_, b)| *b).collect();
    let mut out = BTreeSet::new();
    let mut push = |v: f64| {
        if v.is_finite() && v >= 0.0 {
            out.insert((v * 1000.0).round() as u64);
        }
    };
    let is_scale = |w: Option<&String>| w.is_some_and(|n| lookup(SCALES, n).is_some());
    let mut i = 0;
    while i < ws.len() {
        let w = ws[i].as_str();
        if let Some(v) = digit_value(w) {
            push(v);
            // "$4.6 million": the scale word belongs to the digits.
            match ws.get(i + 1).map(String::as_str) {
                Some("thousand") => {
                    push(v * 1e3);
                    i += 1;
                }
                Some("million") => {
                    push(v * 1e6);
                    i += 1;
                }
                _ => {}
            }
            i += 1;
            continue;
        }
        let a_scale = w == "a" && is_scale(ws.get(i + 1));
        if !(is_number_word(w) || a_scale) || w == "first" || w == "second" {
            i += 1;
            continue;
        }
        let (mut total, mut current) = (0f64, 0f64);
        let (mut decimal, mut dec) = (false, 0.1f64);
        let mut had_scale = false;
        let mut claim = false;
        // Digit groups for "fifty two eighty three" = 5283: (value, open
        // for a unit after a tens word).
        let mut groups: Vec<(u64, bool)> = Vec::new();
        let mut j = i;
        while j < ws.len() {
            let t = ws[j].as_str();
            let next_number = ws.get(j + 1).is_some_and(|n| is_number_word(n));
            if t == "a" && is_scale(ws.get(j + 1)) {
                current = 1.0;
                claim = true;
            } else if t == "and" && j > i && next_number {
            } else if t == "point"
                && j > i
                && ws.get(j + 1).is_some_and(|n| lookup(UNITS, n).is_some())
            {
                decimal = true;
            } else if let Some(u) = lookup(UNITS, t) {
                claim |= t != "one";
                if decimal {
                    current += u * dec;
                    dec /= 10.0;
                } else {
                    current += u;
                    match groups.last_mut() {
                        Some((g, open)) if *open && u < 10.0 => {
                            *g += u as u64;
                            *open = false;
                        }
                        _ => groups.push((u as u64, false)),
                    }
                }
            } else if let Some(tn) = lookup(TENS, t) {
                claim = true;
                current += tn;
                groups.push((tn as u64, true));
            } else if let Some(s) = lookup(SCALES, t) {
                claim = true;
                had_scale = true;
                if s >= 1000.0 {
                    total += current.max(1.0) * s;
                    current = 0.0;
                } else {
                    current = current.max(1.0) * s;
                }
            } else if let Some(o) = ordinal(t) {
                claim = true;
                current += o;
                j += 1;
                break;
            } else {
                break;
            }
            j += 1;
            if ends[j - 1] {
                break;
            }
        }
        if claim {
            push(total + current);
            if !had_scale && groups.len() >= 2 {
                let joined: String = groups.iter().map(|(g, _)| g.to_string()).collect();
                if let Ok(v) = joined.parse::<f64>() {
                    push(v);
                }
            }
        }
        i = j.max(i + 1);
    }
    out
}

/// Month and weekday names (lowercase).
pub fn dates(text: &str) -> BTreeSet<String> {
    words(text)
        .into_iter()
        .map(|w| w.to_lowercase())
        .filter(|w| MONTHS.contains(&w.as_str()) || WEEKDAYS.contains(&w.as_str()))
        .collect()
}

/// Proper names: capitalized words that aren't common words, numbers or
/// dates (lowercase, possessive stripped).
pub fn names(text: &str) -> BTreeSet<String> {
    words(text)
        .into_iter()
        .filter(|w| w.starts_with(|c: char| c.is_uppercase()))
        .map(|w| {
            let l = w.to_lowercase();
            l.strip_suffix("'s").map(str::to_string).unwrap_or(l)
        })
        .filter(|w| {
            !COMMON.contains(&w.as_str())
                && !is_number_word(w)
                && !MONTHS.contains(&w.as_str())
                && !WEEKDAYS.contains(&w.as_str())
                && w.chars().any(|c| c.is_alphabetic())
        })
        .collect()
}

/// Normalized words of a text (for quote matching).
fn norm_words(text: &str) -> Vec<String> {
    text.split(|c: char| !c.is_alphanumeric() && c != '\'')
        .map(|w| w.trim_matches('\'').to_lowercase())
        .filter(|w| !w.is_empty())
        .collect()
}

/// Quoted phrases of 3+ words (straight or curly double quotes).
pub fn quotes(text: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut open: Option<usize> = None;
    for (i, c) in text.char_indices() {
        match (c, open) {
            ('"' | '“', None) => open = Some(i + c.len_utf8()),
            ('"' | '”', Some(start)) => {
                let q = &text[start..i];
                if norm_words(q).len() >= 3 {
                    out.push(q.trim().to_string());
                }
                open = None;
            }
            _ => {}
        }
    }
    out
}

fn contains_run(hay: &[String], needle: &[String]) -> bool {
    !needle.is_empty() && hay.windows(needle.len()).any(|w| w == needle)
}

/// What a cited segment offers as support.
#[derive(Debug, Clone, Copy)]
pub struct Source<'a> {
    pub text: &'a str,
    pub speaker: &'a str,
}

/// Why a claim failed.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ClaimKind {
    Number,
    Date,
    Name,
    Quote,
}

/// One unsupported claim. `value` is for the user's display only (it is
/// answer text) and never goes into the run log.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ClaimFailure {
    pub kind: ClaimKind,
    pub value: String,
}

/// Claims in `sentence` its sources don't support (empty = passes).
pub fn unsupported(sentence: &str, sources: &[Source<'_>]) -> Vec<ClaimFailure> {
    let mut src_numbers = BTreeSet::new();
    let mut src_words: BTreeSet<String> = BTreeSet::new();
    for s in sources {
        src_numbers.extend(numbers(s.text));
        for w in words(s.text).into_iter().chain(words(s.speaker)) {
            let l = w.to_lowercase();
            src_words.insert(l.strip_suffix("'s").map(str::to_string).unwrap_or(l));
        }
    }
    let mut missing = Vec::new();
    for n in numbers(sentence) {
        if !src_numbers.contains(&n) {
            let v = n as f64 / 1000.0;
            missing.push(ClaimFailure {
                kind: ClaimKind::Number,
                value: if v.fract() == 0.0 {
                    format!("{v:.0}")
                } else {
                    format!("{v}")
                },
            });
        }
    }
    for d in dates(sentence) {
        if !src_words.contains(&d) {
            missing.push(ClaimFailure {
                kind: ClaimKind::Date,
                value: d,
            });
        }
    }
    for n in names(sentence) {
        if !src_words.contains(&n) {
            missing.push(ClaimFailure {
                kind: ClaimKind::Name,
                value: n,
            });
        }
    }
    let src_texts: Vec<Vec<String>> = sources.iter().map(|s| norm_words(s.text)).collect();
    for q in quotes(sentence) {
        let qw = norm_words(&q);
        if !src_texts.iter().any(|t| contains_run(t, &qw)) {
            missing.push(ClaimFailure {
                kind: ClaimKind::Quote,
                value: q,
            });
        }
    }
    missing
}

/// Whether a sentence states anything checkable (a number, date, name or
/// quote). Such a sentence with no valid citation is flagged, not merely
/// unsourced.
pub fn has_claims(sentence: &str) -> bool {
    !numbers(sentence).is_empty() || !dates(sentence).is_empty() || !quotes(sentence).is_empty()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn n(text: &str) -> Vec<f64> {
        numbers(text).iter().map(|v| *v as f64 / 1000.0).collect()
    }

    fn kinds(f: &[ClaimFailure]) -> Vec<(ClaimKind, &str)> {
        f.iter().map(|x| (x.kind, x.value.as_str())).collect()
    }

    #[test]
    fn spoken_numbers() {
        assert_eq!(n("sixty three hundred dollars"), vec![6300.0]);
        assert_eq!(
            n("from sixty three hundred to seventy one hundred"),
            vec![6300.0, 7100.0]
        );
        assert_eq!(
            n("thirty eight thousand euros per season for fifty seats"),
            vec![50.0, 38000.0]
        );
        assert_eq!(
            n("a hundred and thirty thousand, maybe a hundred and forty five thousand"),
            vec![130000.0, 145000.0]
        );
        assert_eq!(n("four point six million"), vec![4_600_000.0]);
        assert_eq!(n("one hundred and eight percent"), vec![108.0]);
        assert_eq!(n("March twenty-sixth"), vec![26.0]);
        assert_eq!(n("on March ninth at four Eastern"), vec![4.0, 9.0]);
        assert!(n("fifty two eighty three").contains(&5283.0));
        assert!(n("the opening month, one of them").is_empty());
        assert!(n("one week later").is_empty());
        assert_eq!(n("two hundred requests"), vec![200.0]);
        assert_eq!(
            n("Five: three in design and two in sales."),
            vec![2.0, 3.0, 5.0]
        );
        assert_eq!(n("two, four"), vec![2.0, 4.0]);
        assert_eq!(n("fifty two eighty three today."), vec![135.0, 5283.0]);
    }

    #[test]
    fn digit_numbers() {
        assert_eq!(n("$6,300"), vec![6300.0]);
        assert_eq!(n("$63k"), vec![63000.0]);
        assert_eq!(n("4.6M ARR"), vec![4_600_000.0]);
        assert_eq!(n("108% and 15%"), vec![15.0, 108.0]);
        assert_eq!(n("March 26th."), vec![26.0]);
        assert_eq!(n("$4.6 million"), vec![4.6, 4_600_000.0]);
    }

    #[test]
    fn a_scaled_number_is_caught() {
        let src = [Source {
            text: "All told, I'd like sixty three hundred dollars for studio rental.",
            speaker: "Priya Anand",
        }];
        assert!(unsupported("Priya asked for $6,300.", &src).is_empty());
        assert!(unsupported("Priya asked for sixty three hundred dollars.", &src).is_empty());
        assert_eq!(
            kinds(&unsupported("Priya asked for $63k.", &src)),
            vec![(ClaimKind::Number, "63000")]
        );
        assert_eq!(
            kinds(&unsupported("Ewan asked for $6,300.", &src)),
            vec![(ClaimKind::Name, "ewan")]
        );
        assert_eq!(
            kinds(&unsupported("By Friday, $6,300.", &src)),
            vec![(ClaimKind::Date, "friday")]
        );
    }

    #[test]
    fn quotes_must_appear_word_for_word() {
        let src = [Source {
            text: "Honestly, we will ship the planner on the twenty-sixth.",
            speaker: "Ilka",
        }];
        assert!(unsupported("Ilka said \"we will ship the planner\".", &src).is_empty());
        assert_eq!(
            kinds(&unsupported("Ilka said “we will not ship it”.", &src)),
            vec![(ClaimKind::Quote, "we will not ship it")]
        );
        // Two-word quotes are emphasis, not a quote claim.
        assert!(quotes("the \"fast path\" option").is_empty());
    }

    #[test]
    fn names_skip_common_words() {
        assert_eq!(
            names("Juniper, Basalt and Lumen. It's Yuki's call, not API's."),
            ["api", "basalt", "juniper", "lumen", "yuki"]
                .iter()
                .map(|s| s.to_string())
                .collect()
        );
        assert!(has_claims("Due on Friday."));
        assert!(!has_claims("They agreed to revisit it."));
    }
}
