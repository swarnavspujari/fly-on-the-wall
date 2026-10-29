//! Text handling for model output: `[seg:ID]` markers, sentence splitting,
//! link/image stripping, reasoning-tag removal and refusal detection.
//! Everything here is deterministic and model-agnostic.

/// A sentence of the answer before validation.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RawSentence {
    /// Display text: markers removed, links stripped, list numbering
    /// replaced by a bullet.
    pub display: String,
    /// The same text without any list/heading prefix (what claims are read
    /// from).
    pub claim_text: String,
    /// Segment ids the model cited, in order, de-duplicated.
    pub markers: Vec<String>,
}

fn is_id_char(c: char) -> bool {
    c.is_alphanumeric() || c == '-' || c == '_'
}

/// Parse the inside of one bracket that starts with `seg:` — a single id,
/// or a list (`seg:a, seg:b` / `seg:a; b`). `None` if anything in it isn't
/// an id.
fn bracket_ids(inner: &str) -> Option<Vec<String>> {
    let rest = inner.trim().strip_prefix("seg:")?;
    let mut out = Vec::new();
    for part in rest.split([',', ';']) {
        let p = part.trim();
        let p = p.strip_prefix("seg:").unwrap_or(p).trim();
        if p.is_empty() || !p.chars().all(is_id_char) {
            return None;
        }
        out.push(p.to_string());
    }
    (!out.is_empty()).then_some(out)
}

/// Every `[seg:ID]` marker in `text`, in order (lists inside one bracket
/// count), plus the text with those markers removed.
pub fn take_markers(text: &str) -> (String, Vec<String>) {
    let mut out = String::with_capacity(text.len());
    let mut ids = Vec::new();
    let mut rest = text;
    while let Some(i) = rest.find('[') {
        let after = &rest[i + 1..];
        let close = after.find(']');
        match close.and_then(|j| bracket_ids(&after[..j]).map(|v| (j, v))) {
            Some((j, v)) => {
                out.push_str(&rest[..i]);
                ids.extend(v);
                rest = &after[j + 1..];
            }
            None => {
                out.push_str(&rest[..=i]);
                rest = after;
            }
        }
    }
    out.push_str(rest);
    (tidy_spaces(&out), dedup(ids))
}

fn dedup(ids: Vec<String>) -> Vec<String> {
    let mut seen = std::collections::HashSet::new();
    ids.into_iter().filter(|x| seen.insert(x.clone())).collect()
}

/// Collapse the gaps markers leave behind (`x  .` → `x.`).
fn tidy_spaces(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for (i, line) in s.split('\n').enumerate() {
        if i > 0 {
            out.push('\n');
        }
        let lead: String = line.chars().take_while(|c| *c == ' ').collect();
        let body = line.split_whitespace().collect::<Vec<_>>().join(" ");
        let body = body
            .replace(" .", ".")
            .replace(" ,", ",")
            .replace(" ;", ";")
            .replace(" :", ":")
            .replace(" !", "!")
            .replace(" ?", "?")
            .replace(" )", ")");
        out.push_str(&lead);
        out.push_str(&body);
    }
    out
}

/// Neutralize marker syntax inside meeting content before it's shown to a
/// model, so speech can't plant a citation (`[seg:x]` → `[seg-x]`).
///
/// Any `[` followed by `seg` and `:` — any case, any spacing — loses its
/// bracket, a superset of what [`take_markers`] parses.
pub fn defuse_markers(text: &str) -> String {
    let chars: Vec<char> = text.chars().collect();
    let mut out = String::with_capacity(text.len());
    for (i, &c) in chars.iter().enumerate() {
        if c == '[' && looks_like_marker(&chars[i + 1..]) {
            out.push('(');
        } else {
            out.push(c);
        }
    }
    out
}

fn looks_like_marker(rest: &[char]) -> bool {
    let mut it = rest
        .iter()
        .copied()
        .skip_while(|c| c.is_whitespace())
        .peekable();
    for want in ['s', 'e', 'g'] {
        match it.next() {
            Some(c) if c.eq_ignore_ascii_case(&want) => {}
            _ => return false,
        }
    }
    it.find(|c| !c.is_whitespace()) == Some(':')
}

/// Remove `<think>…</think>` (and an unterminated trailing one) — some
/// local models print their reasoning inline.
pub fn strip_reasoning(text: &str) -> String {
    let mut out = String::new();
    let mut rest = text;
    loop {
        let lower = rest.to_lowercase();
        match lower.find("<think>") {
            Some(i) => {
                out.push_str(&rest[..i]);
                match lower[i..].find("</think>") {
                    Some(j) => rest = &rest[i + j + "</think>".len()..],
                    None => {
                        rest = "";
                        break;
                    }
                }
            }
            None => break,
        }
    }
    out.push_str(rest);
    out.trim().to_string()
}

/// Markdown links → their label; images, HTML anchors/images, autolinks
/// and bare URLs removed. Returns the text and how many were removed.
pub fn strip_links(text: &str) -> (String, usize) {
    let mut removed = 0;
    let mut s = text.to_string();
    // [label](target) and ![alt](target)
    while let Some(i) = s.find("](") {
        let open = s[..i].rfind('[');
        let close = s[i..].find(')').map(|j| i + j + 1);
        let (Some(open), Some(close)) = (open, close) else {
            // Malformed: drop the "](" so it can't render.
            s.replace_range(i..i + 2, " ");
            removed += 1;
            continue;
        };
        let image = open > 0 && s.as_bytes()[open - 1] == b'!';
        let label = s[open + 1..i].to_string();
        let start = if image { open - 1 } else { open };
        s.replace_range(start..close, if image { "" } else { &label });
        removed += 1;
    }
    // Then every word that could still render, load or link, line by line.
    let mut lines = Vec::new();
    for line in s.split('\n') {
        let mut words = Vec::new();
        for w in line.split(' ') {
            if renders(w) {
                removed += 1;
                continue;
            }
            words.push(w);
        }
        lines.push(words.join(" "));
    }
    let mut out = tidy_spaces(&lines.join("\n"));
    // Belt and braces: anything a word filter missed (a pattern spanning a
    // space) loses the character that makes it work.
    for (pat, safe) in [("<a ", "‹a "), ("]: ", "] : ")] {
        if out.to_lowercase().contains(pat) {
            removed += 1;
            out = replace_ci(&out, pat, safe);
        }
    }
    (out, removed)
}

/// Substrings that make text render as a link, image or embed: Markdown
/// inline and reference links, HTML that loads or links, URL schemes.
const RENDERS: &[&str] = &[
    "](",
    "][",
    "<a>",
    "<a",
    "</a",
    "<img",
    "<iframe",
    "<svg",
    "<object",
    "<embed",
    "<video",
    "<audio",
    "<source",
    "<link",
    "<base",
    "<form",
    "<image",
    "<script",
    "<style",
    "<meta",
    "src=",
    "href=",
    "srcset=",
    "action=",
    "http://",
    "https://",
    "ftp://",
    "file://",
    "www.",
    "data:",
    "javascript:",
    "vbscript:",
    "mailto:",
];

/// One word that would render as (part of) a link, image or embed.
fn renders(word: &str) -> bool {
    let l = word.to_lowercase();
    if RENDERS.iter().any(|p| l.contains(p)) {
        return true;
    }
    // Any HTML tag, listed or not: `<` or `</` right before a letter
    // (`<details ontoggle=…>`, `<SVG/onload=…>`). Without its `<name`,
    // the rest of a tag can't render.
    if l.match_indices('<').any(|(i, _)| {
        l[i + 1..]
            .trim_start_matches('/')
            .starts_with(|c: char| c.is_ascii_alphabetic())
    }) {
        return true;
    }
    // A reference definition (`[1]:`, `[ref]:/x`, `[ref]:tel:…`, glued
    // to its target or not) or a footnote link `[the brief][1]`.
    if l.contains("]:") {
        return true;
    }
    // Protocol-relative `//host.tld/...`.
    l.match_indices("//").any(|(i, _)| {
        let rest = &l[i + 2..];
        rest.chars()
            .next()
            .is_some_and(|c| c.is_ascii_alphanumeric())
            && rest
                .split(|c: char| c.is_whitespace() || matches!(c, ')' | '"' | '\''))
                .next()
                .is_some_and(|host| host.contains('.'))
    })
}

/// Case-insensitive replace of an ASCII pattern.
fn replace_ci(s: &str, pat: &str, with: &str) -> String {
    let lower = s.to_lowercase();
    if lower.len() != s.len() {
        return s.replace(pat, with);
    }
    let mut out = String::with_capacity(s.len());
    let mut i = 0;
    while let Some(j) = lower[i..].find(pat) {
        out.push_str(&s[i..i + j]);
        out.push_str(with);
        i += j + pat.len();
    }
    out.push_str(&s[i..]);
    out
}

/// Leading list/heading syntax of a line: `- `, `* `, `• `, `1. `, `2) `,
/// `#… `. Returns (display prefix, rest).
fn split_prefix(line: &str) -> (String, &str) {
    let t = line.trim_start();
    for b in ["- ", "* ", "• ", "+ "] {
        if let Some(rest) = t.strip_prefix(b) {
            return ("- ".into(), rest);
        }
    }
    let digits: String = t.chars().take_while(|c| c.is_ascii_digit()).collect();
    if !digits.is_empty() && digits.len() <= 3 {
        let after = &t[digits.len()..];
        for p in [". ", ") "] {
            if let Some(rest) = after.strip_prefix(p) {
                // Numbering would read as a numeric claim; show a bullet.
                return ("- ".into(), rest);
            }
        }
    }
    if t.starts_with('#') {
        let rest = t.trim_start_matches('#').trim_start();
        return (String::new(), rest);
    }
    (String::new(), t)
}

const ABBREVIATIONS: &[&str] = &[
    "e.g", "i.e", "vs", "mr", "mrs", "ms", "dr", "approx", "etc", "inc", "jr", "sr", "st", "no",
];

/// Where a sentence ends in `chars` at `i` (a `.`/`?`/`!`).
fn ends_sentence(chars: &[char], i: usize) -> bool {
    let c = chars[i];
    if !matches!(c, '.' | '?' | '!') {
        return false;
    }
    let next = chars.get(i + 1);
    // Closing quotes/brackets/emphasis may follow the stop.
    let mut k = i + 1;
    while chars
        .get(k)
        .is_some_and(|n| matches!(n, '"' | '\'' | '”' | '’' | ')' | '*' | '_'))
    {
        k += 1;
    }
    if !chars.get(k).is_none_or(|n| n.is_whitespace() || *n == '[') {
        return false;
    }
    if c == '.' {
        // 3.2 / v1.2
        if i > 0 && chars[i - 1].is_ascii_digit() && next.is_some_and(|n| n.is_ascii_digit()) {
            return false;
        }
        // e.g. / Dr.
        let word: String = chars[..i]
            .iter()
            .rev()
            .take_while(|c| c.is_alphanumeric() || **c == '.')
            .collect::<Vec<_>>()
            .into_iter()
            .rev()
            .collect();
        let w = word.to_lowercase();
        if ABBREVIATIONS.contains(&w.as_str()) {
            return false;
        }
    }
    true
}

/// Split an answer into sentences. Markers right after a sentence's stop
/// (`… dollars. [seg:a]`) belong to that sentence; a run of markers on
/// its own attaches to the sentence before it.
pub fn sentences(answer: &str) -> Vec<RawSentence> {
    let mut out: Vec<RawSentence> = Vec::new();
    for line in answer.lines() {
        if line.trim().is_empty() {
            continue;
        }
        let (prefix, body) = split_prefix(line);
        let chars: Vec<char> = body.chars().collect();
        let mut pieces: Vec<String> = Vec::new();
        let mut cur = String::new();
        let mut i = 0;
        while i < chars.len() {
            cur.push(chars[i]);
            if ends_sentence(&chars, i) {
                // Absorb trailing closers and any markers that follow.
                let mut j = i + 1;
                while j < chars.len()
                    && matches!(chars[j], '"' | '\'' | '”' | '’' | ')' | '*' | '_')
                {
                    cur.push(chars[j]);
                    j += 1;
                }
                loop {
                    let mut k = j;
                    while k < chars.len() && chars[k].is_whitespace() {
                        k += 1;
                    }
                    let rest: String = chars[k..].iter().collect();
                    if rest.starts_with("[seg:") || rest.starts_with("[ seg:") {
                        if let Some(close) = rest.find(']') {
                            cur.push(' ');
                            cur.push_str(&rest[..=close]);
                            j = k + rest[..=close].chars().count();
                            continue;
                        }
                    }
                    break;
                }
                pieces.push(std::mem::take(&mut cur));
                i = j;
                continue;
            }
            i += 1;
        }
        if !cur.trim().is_empty() {
            pieces.push(cur);
        }
        for (n, piece) in pieces.into_iter().enumerate() {
            let (text, markers) = take_markers(&piece);
            let text = text.trim().to_string();
            let only_markers = text.chars().all(|c| !c.is_alphanumeric());
            if only_markers {
                if let Some(prev) = out.last_mut() {
                    for m in markers {
                        if !prev.markers.contains(&m) {
                            prev.markers.push(m);
                        }
                    }
                }
                continue;
            }
            let display = if n == 0 {
                format!("{prefix}{text}")
            } else {
                text.clone()
            };
            out.push(RawSentence {
                display,
                claim_text: text,
                markers,
            });
        }
    }
    attach_trailers(out)
}

/// "Sources: [seg:a] [seg:b]" after an answer: its markers cite the
/// sentences before it that cite nothing, and the trailer itself isn't a
/// sentence.
fn attach_trailers(sentences: Vec<RawSentence>) -> Vec<RawSentence> {
    let is_trailer = |s: &RawSentence| {
        let t = s
            .claim_text
            .trim()
            .trim_end_matches(':')
            .trim()
            .trim_matches(|c: char| c == '*' || c == '_')
            .to_lowercase();
        !s.markers.is_empty()
            && [
                "source",
                "sources",
                "reference",
                "references",
                "citation",
                "citations",
                "cited",
            ]
            .contains(&t.as_str())
    };
    let mut out: Vec<RawSentence> = Vec::new();
    for s in sentences {
        if is_trailer(&s) && !out.is_empty() {
            for prev in out.iter_mut().filter(|p| p.markers.is_empty()) {
                prev.markers = s.markers.clone();
            }
            continue;
        }
        out.push(s);
    }
    out
}

/// Bracketed bare ids (`[x07-14]`) that `known` accepts — a citation
/// without the `seg:` prefix — plus the text with them removed. Brackets
/// holding anything else (`[1]`, `[sic]`) stay.
pub fn take_bare_ids(text: &str, known: impl Fn(&str) -> bool) -> (String, Vec<String>) {
    let mut out = String::with_capacity(text.len());
    let mut ids = Vec::new();
    let mut rest = text;
    while let Some(i) = rest.find('[') {
        let after = &rest[i + 1..];
        let hit = after.find(']').and_then(|j| {
            let inner = after[..j].trim();
            let parts: Vec<&str> = inner.split([',', ';']).map(str::trim).collect();
            (!inner.is_empty() && parts.iter().all(|p| known(p))).then_some((j, parts))
        });
        match hit {
            Some((j, parts)) => {
                out.push_str(&rest[..i]);
                ids.extend(parts.into_iter().map(str::to_string));
                rest = &after[j + 1..];
            }
            None => {
                out.push_str(&rest[..=i]);
                rest = after;
            }
        }
    }
    out.push_str(rest);
    (tidy_spaces(&out), dedup(ids))
}

/// Phrases a model uses to say the meetings don't answer the question.
const REFUSALS: &[&str] = &[
    "not found in your meetings",
    "couldn't find",
    "could not find",
    "can't find",
    "cannot find",
    "wasn't able to find",
    "was not able to find",
    "no information",
    "don't have information",
    "do not have information",
    "don't have any information",
    "not mentioned",
    "isn't mentioned",
    "wasn't mentioned",
    "was not mentioned",
    "not discussed",
    "wasn't discussed",
    "was not discussed",
    "no mention",
    "doesn't say",
    "does not say",
    "don't say",
    "do not say",
    "doesn't contain",
    "does not contain",
    "don't contain",
    "do not contain",
    "not in the excerpts",
    "not covered",
    "doesn't mention",
    "does not mention",
    "don't mention",
    "do not mention",
];

/// True when a sentence says the answer isn't in the meetings.
pub fn is_refusal(sentence: &str) -> bool {
    let l = sentence.to_lowercase().replace('’', "'");
    REFUSALS.iter().any(|p| l.contains(p))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn markers_single_lists_and_junk() {
        let (t, ids) =
            take_markers("a [seg:x01-05] b [seg: x02-1 ] [seg:x, seg:y] [seg:bad id] [1]");
        assert_eq!(ids, vec!["x01-05", "x02-1", "x", "y"]);
        assert_eq!(t, "a b [seg:bad id] [1]");
        let (t, ids) = take_markers("Cap is 9200 [seg:a][seg:a].");
        assert_eq!(ids, vec!["a"]);
        assert_eq!(t, "Cap is 9200.");
    }

    #[test]
    fn sentence_split_keeps_markers_with_their_sentence() {
        let s = sentences("It is 4.6 million [seg:a]. Next? Yes! Done");
        let d: Vec<&str> = s.iter().map(|x| x.display.as_str()).collect();
        assert_eq!(d, vec!["It is 4.6 million.", "Next?", "Yes!", "Done"]);
        assert_eq!(s[0].markers, vec!["a"]);

        // Marker after the stop belongs to the sentence before it.
        let s = sentences(
            "The fee is seventy one hundred. [seg:x02-06] It was agreed with Tamsin. [seg:x02-07]",
        );
        assert_eq!(s.len(), 2);
        assert_eq!(s[0].markers, vec!["x02-06"]);
        assert_eq!(s[1].markers, vec!["x02-07"]);

        // A line of only markers attaches to the previous sentence.
        let s = sentences("Priya is drafting it.\n[seg:x01-10]");
        assert_eq!(s.len(), 1);
        assert_eq!(s[0].markers, vec!["x01-10"]);

        // e.g. and Dr. don't end sentences.
        let s =
            sentences("Tools, e.g. the planner, ship first [seg:a]. Dr. Mensah agreed [seg:b].");
        assert_eq!(s.len(), 2);
    }

    #[test]
    fn list_numbering_becomes_a_bullet() {
        let s =
            sentences("1. Priya drafts the release [seg:a].\n2) Ewan updates the page [seg:b].");
        assert_eq!(s[0].display, "- Priya drafts the release.");
        assert_eq!(s[0].claim_text, "Priya drafts the release.");
        assert_eq!(s[1].markers, vec!["b"]);
        let s = sentences("## Summary\nDone [seg:a].");
        assert_eq!(s[0].display, "Summary");
    }

    #[test]
    fn links_and_images_are_stripped() {
        let (s, n) = strip_links(
            "Logo: ![logo](https://x.example/p.png) and [pilot](https://x.example/p) plus https://x.example/y.",
        );
        assert!(!s.contains("http") && !s.contains("]("), "{s}");
        assert!(s.contains("pilot"));
        assert_eq!(n, 3);
        assert_eq!(strip_links("no links here").1, 0);
    }

    /// What a renderer would turn into a link, image or embed (an
    /// independent list, checked against everything `strip_links` returns).
    fn renders_anything(text: &str) -> bool {
        let l = text.to_lowercase();
        [
            "](",
            "][",
            "]:",
            "<a ",
            "<a>",
            "<img",
            "<iframe",
            "<svg",
            "<object",
            "<embed",
            "<video",
            "<audio",
            "<source",
            "<link",
            "<base",
            "<form",
            "src=",
            "href=",
            "srcset=",
            "http://",
            "https://",
            "ftp://",
            "file://",
            "www.",
            "data:",
            "javascript:",
            "vbscript:",
        ]
        .iter()
        .any(|p| l.contains(p))
            || l.match_indices('<').any(|(i, _)| {
                l[i + 1..]
                    .trim_start_matches('/')
                    .starts_with(|c: char| c.is_ascii_alphabetic())
            })
            || l.match_indices("//").any(|(i, _)| {
                let rest = &l[i + 2..];
                rest.chars()
                    .next()
                    .is_some_and(|c| c.is_ascii_alphanumeric())
                    && rest
                        .split(char::is_whitespace)
                        .next()
                        .is_some_and(|h| h.contains('.'))
            })
    }

    #[test]
    fn every_link_form_is_removed() {
        for t in [
            "Glazing night is the top ask. See [the brief][1].\n\n[1]: //docs.example.invalid/brief",
            "Kiln rotas came up (img src=https://pic.example.invalid/t.png).",
            "Glazing night. <svg onload=go()><image href=\"",
            "Glazing night. <iframe src=\"",
            "Glazing night. ![status](htt",
            "See <a href=x>here</a> or ftp://files.example.invalid and file://c/x",
            "Visit www.example.invalid, data:text/html,hi or javascript:go() / vbscript:x",
            "Protocol relative //cdn.example.invalid/p.png here",
            "<object data=x> <embed src=x> <video> <audio> <source> <link> <base> <form action=x>",
            "Tags not on any list: <details open ontoggle=go()>x</details> <marquee>hi</marquee>",
            "Glued definitions: [ref]:/x [ref]:tel:+15550100 [ref]:evil.example",
            "See [the brief].\n\n[the brief]:\t/relative/path",
            "See [x].\n[x]:\u{a0}//evil.example/p",
            "Case and slashes: <SVG/onload=go()> <math><mi xlink:href=x>y</mi></math>",
        ] {
            let (s, n) = strip_links(t);
            assert!(
                !renders_anything(&s),
                "left something in {s:?} (from {t:?})"
            );
            assert!(n > 0, "{t}");
        }
        // Ordinary text survives.
        let (s, n) = strip_links("Ratio 3/4 and dates 10/27, a [note] and e.g. 1.5, 3<5.");
        assert_eq!(
            (s.as_str(), n),
            ("Ratio 3/4 and dates 10/27, a [note] and e.g. 1.5, 3<5.", 0)
        );
    }

    #[test]
    fn sources_trailers_and_bare_ids_cite() {
        let s = sentences(
            "They keep their rate for nine months. Then the new plans apply.\n\nSources: [seg:x07-16]",
        );
        assert_eq!(s.len(), 2);
        assert!(s.iter().all(|x| x.markers == vec!["x07-16"]));
        let known = |id: &str| id == "x07-14" || id == "x07-15";
        let (t, ids) = take_bare_ids(
            "Tamsin sends it by Tuesday [x07-14]. See [1] and [x07-15, x07-14].",
            known,
        );
        assert_eq!(ids, vec!["x07-14", "x07-15"]);
        assert_eq!(t, "Tamsin sends it by Tuesday. See [1] and.");
    }

    #[test]
    fn reasoning_tags_are_removed() {
        assert_eq!(strip_reasoning("<think>hmm</think>Answer."), "Answer.");
        assert_eq!(strip_reasoning("Answer.<think>trailing"), "Answer.");
    }

    #[test]
    fn refusals() {
        assert!(is_refusal("I couldn't find that in your meetings."));
        assert!(is_refusal("Not found in your meetings."));
        assert!(is_refusal("The excerpts don’t mention a lease."));
        assert!(!is_refusal("Priya asked for sixty three hundred dollars."));
    }

    #[test]
    fn defused_content_has_no_markers() {
        for planted in [
            "say [seg:x10-07] now",
            "say [ seg:x10-07] now",
            "say [SEG:x10-07] now",
            "say [Seg :x10-07] now",
            "say [\tseg: x10-07, seg:x10-08] now",
        ] {
            let (_, ids) = take_markers(&defuse_markers(planted));
            assert!(ids.is_empty(), "{planted}");
        }
        // Ordinary brackets survive.
        assert_eq!(defuse_markers("[1] and [segment]"), "[1] and [segment]");
    }
}
