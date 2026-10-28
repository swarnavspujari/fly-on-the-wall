//! The claim check (spec §4.2a #1): every number, date and proper name in
//! a sentence shown as sourced must appear in the segments it cites.
//!
//! Numbers are compared by value, with spoken forms normalized: "eighty
//! four hundred" = 8400 = "$8,400", but not "$84k"; "three point two
//! million" = 3.2M; "twenty-seventh" = 27; "forty four seventy one" can
//! also mean 4471. Dates are month and weekday names. Names are
//! capitalized words outside a list of common words; a name matches the
//! cited text or the cited segment's speaker label (attribution).

use std::collections::BTreeSet;

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
/// Capitalized words that aren't names (sentence starters, pronouns,
/// function words).
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

/// Compound ordinals ("twenty-seventh" arrives as "twenty", "seventh").
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

/// Words with punctuation trimmed (hyphens split, apostrophes kept).
fn words(text: &str) -> Vec<String> {
    text.split(|c: char| c.is_whitespace() || c == '-' || c == '/' || c == '—')
        .map(|w| {
            w.trim_matches(|c: char| {
                !(c.is_alphanumeric() || c == '$' || c == '%' || c == '.' || c == ',' || c == '\'')
            })
            .trim_end_matches(['.', ',', '\''])
            .to_string()
        })
        .filter(|w| !w.is_empty())
        .collect()
}

/// "8,400" / "$8,400" / "$84k" / "3.2M" / "108%" / "27th" → value.
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

/// Every number a text states, as values ×1000 (spoken runs may yield two
/// readings).
pub fn numbers(text: &str) -> BTreeSet<u64> {
    let ws: Vec<String> = words(text).iter().map(|w| w.to_lowercase()).collect();
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
            // "$3.2 million": the scale word belongs to the digits.
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
        // A lone "first"/"second" is ordinary English, not a claim.
        if !(is_number_word(w) || a_scale) || w == "first" || w == "second" {
            i += 1;
            continue;
        }
        // One run of spoken number words.
        let (mut total, mut current) = (0f64, 0f64);
        let (mut decimal, mut dec) = (false, 0.1f64);
        let mut had_scale = false;
        // A run made only of "one" ("one week") is not a claim.
        let mut claim = false;
        // Digit groups for "forty four seventy one" = 4471: (value, open
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
                // An ordinal ends the run ("twenty seventh" = 27).
                claim = true;
                current += o;
                j += 1;
                break;
            } else {
                break;
            }
            j += 1;
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

/// Month and weekday names in a text (lowercase).
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

/// What a cited segment offers as support.
pub struct Source<'a> {
    pub text: &'a str,
    pub speaker: &'a str,
}

/// Claims in `sentence` that its sources don't support (empty = passes).
pub fn unsupported(sentence: &str, sources: &[Source<'_>]) -> Vec<String> {
    let text = strip_markers(sentence);
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
    for n in numbers(&text) {
        if !src_numbers.contains(&n) {
            missing.push(format!("number {}", n as f64 / 1000.0));
        }
    }
    for d in dates(&text) {
        if !src_words.contains(&d) {
            missing.push(format!("date {d}"));
        }
    }
    for n in names(&text) {
        if !src_words.contains(&n) {
            missing.push(format!("name {n}"));
        }
    }
    missing
}

/// `[seg:ID]` markers in a text, in order.
pub fn markers(text: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut rest = text;
    while let Some(i) = rest.find("[seg:") {
        let after = &rest[i + 5..];
        match after.find(']') {
            Some(j) => {
                let id = after[..j].trim();
                if !id.is_empty()
                    && id
                        .chars()
                        .all(|c| c.is_alphanumeric() || c == '-' || c == '_')
                {
                    out.push(id.to_string());
                }
                rest = &after[j + 1..];
            }
            None => break,
        }
    }
    out
}

/// The text with `[seg:ID]` markers removed.
pub fn strip_markers(text: &str) -> String {
    let mut out = String::new();
    let mut rest = text;
    while let Some(i) = rest.find("[seg:") {
        out.push_str(&rest[..i]);
        match rest[i..].find(']') {
            Some(j) => rest = &rest[i + j + 1..],
            None => {
                rest = "";
                break;
            }
        }
    }
    out.push_str(rest);
    out
}

/// Rendered links or images in displayed text: Markdown links/images,
/// HTML anchors/images, autolinks and bare URLs.
pub fn links(text: &str) -> usize {
    let lower = text.to_lowercase();
    let mut n = 0;
    for pat in [
        "](",
        "<a ",
        "<a>",
        "<img",
        "http://",
        "https://",
        "www.",
        "data:",
        "javascript:",
    ] {
        n += lower.matches(pat).count();
    }
    n
}

#[cfg(test)]
mod tests {
    use super::*;

    fn n(text: &str) -> Vec<f64> {
        numbers(text).iter().map(|v| *v as f64 / 1000.0).collect()
    }

    #[test]
    fn spoken_numbers() {
        assert_eq!(n("eighty four hundred dollars"), vec![8400.0]);
        assert_eq!(
            n("from eighty four hundred to ninety two hundred"),
            vec![8400.0, 9200.0]
        );
        assert_eq!(
            n("forty two thousand dollars a year for sixty seats"),
            vec![60.0, 42000.0]
        );
        assert_eq!(
            n("a hundred and ten thousand to a hundred and twenty five thousand"),
            vec![110000.0, 125000.0]
        );
        assert_eq!(n("three point two million"), vec![3_200_000.0]);
        assert_eq!(n("one hundred and eight percent"), vec![108.0]);
        assert_eq!(n("October twenty-seventh"), vec![27.0]);
        assert_eq!(n("on November third at ten Pacific"), vec![3.0, 10.0]);
        assert!(n("forty four seventy one").contains(&4471.0));
        assert!(n("the first month, one of them").is_empty());
        assert_eq!(
            n("one week later"),
            Vec::<f64>::new(),
            "a lone 'one' is not a claim"
        );
        assert_eq!(n("two hundred requests"), vec![200.0]);
    }

    #[test]
    fn digit_numbers() {
        assert_eq!(n("$8,400"), vec![8400.0]);
        assert_eq!(n("$84k"), vec![84000.0]);
        assert_eq!(n("3.2M ARR"), vec![3_200_000.0]);
        assert_eq!(n("108% and 15%"), vec![15.0, 108.0]);
        assert_eq!(n("October 27th."), vec![27.0]);
        assert_eq!(n("$3.2 million"), vec![3.2, 3_200_000.0]);
    }

    #[test]
    fn the_spoken_number_corruption_is_caught() {
        let src = [Source {
            text: "All in, I'm asking for eighty four hundred dollars for the first month.",
            speaker: "Ines Carvalho",
        }];
        assert!(unsupported("Ines asked for $8,400 [seg:m01-05].", &src).is_empty());
        assert!(unsupported("Ines asked for eighty four hundred dollars.", &src).is_empty());
        assert_eq!(
            unsupported("Ines asked for $84k.", &src),
            vec!["number 84000"]
        );
        assert_eq!(
            unsupported("Marcus asked for $8,400.", &src),
            vec!["name marcus"]
        );
        assert_eq!(unsupported("By Friday, $8,400.", &src), vec!["date friday"]);
    }

    #[test]
    fn markers_links_and_names() {
        assert_eq!(
            markers("a [seg:m01-05] b [seg: m02-1 ] [seg:bad id] [seg:"),
            vec!["m01-05", "m02-1"]
        );
        assert_eq!(strip_markers("x [seg:m01-05]."), "x .");
        assert_eq!(
            links("see ![logo](https://e.example/p.png) and <a href=x>"),
            3
        );
        assert_eq!(links("no links here"), 0);
        assert_eq!(
            names("Starter, Team and Fleet. It's Hana's call, not API's."),
            ["api", "fleet", "hana", "starter", "team"]
                .iter()
                .map(|s| s.to_string())
                .collect()
        );
    }
}
