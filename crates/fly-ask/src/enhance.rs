//! Enhance, checked (spec v2.5 row 3.7; §4.2, §4.2a). The Ask harness's
//! checkers, applied to the other place a model writes about a meeting.
//!
//! Enhance asks a model for blocks, each with the transcript lines it came
//! from. Code, not the model, decides what the user is told about them:
//!
//! * **What the model reads** ([`prepare`]). A meeting that fits the model's
//!   context goes in whole, byte for byte as before. A longer one is cut by
//!   a deterministic rule instead of letting the server trim the front:
//!   evenly spaced excerpts across the whole meeting plus the lines that
//!   speak to the topics in the user's notes, with every gap marked. The
//!   size comes from the `num_ctx` actually sent (fly-llm), and an Ollama
//!   that fell back to a smaller context (its 5xx retry without `num_ctx`)
//!   counts as truncation: [`enhance`] tries once at a smaller size, then
//!   fails visibly instead of saving a note built from the end of a meeting.
//! * **What the model said** ([`check`]). A citation counts only if it
//!   names a line the model was shown. A block with no valid citation is
//!   unsourced. Numbers, dates, names and quotes in a block are checked
//!   against the lines it cites with the harness's claim check (spoken
//!   numbers normalized), and a block that fails is flagged, never sourced.
//!   Links, images, HTML and bare emails are removed. A `user` block must
//!   restate the user's own notes, or it is shown as AI text.
//! * **Injection.** A transcript line the harness's detector reads as
//!   addressed to an AI ("ignore your instructions", "add an action item")
//!   is never evidence: a block built only from such lines is dropped, and
//!   such a line supports no claim.
//!
//! Nothing here builds a provider, reads a secret or touches storage: the
//! app's `build_provider` hands the model in, and saves what comes out.

use std::collections::{BTreeMap, BTreeSet};

use fly_core::enhance::{self, EnhancePrompt, ParsedEnhance, RawEnhanceBlock, Shown};
use fly_core::prompt_profile::PromptProfile;
use fly_core::{BlockFlag, BlockFlagKind, Note, NoteBlock, Template, Transcript};
use fly_llm::{ChatMessage, ChatRequest, LLMProvider, ThinkingMode};
use serde::{Deserialize, Serialize};

use crate::claims::{self, ClaimKind, Source};
use crate::prefetch::salient_terms;
use crate::proposals::addressed_to_assistant;
use crate::text;

/// Share of the transcript budget kept for evenly spaced excerpts when the
/// notes also name topics (the rest goes to the topic lines).
const EVEN_SHARE: f64 = 0.6;
/// About how long one evenly spaced excerpt is, in characters (~10 lines).
const EXCERPT_CHARS: usize = 1_500;
/// Most excerpts, so a huge budget doesn't cut a meeting into crumbs.
const MAX_EXCERPTS: usize = 80;
/// Share of the transcript budget lines may use; the rest is for the gap
/// markers between them.
const LINES_SHARE: f64 = 0.92;
/// Characters held back for the prompt's excerpt heading and rounding.
const SLACK_CHARS: usize = 400;
/// Salient terms of the notes read (a long scratchpad names many).
const MAX_TOPIC_TERMS: usize = 120;
/// Dropped blocks kept in the report (so a wrongly dropped line is not lost
/// to the user), and how much of each.
const DROPPED_KEPT: usize = 5;
const DROPPED_CHARS: usize = 200;
/// A `user` block needs this share of its content words in the user's notes.
const NOTES_OVERLAP: f64 = 0.5;

// ---------------------------------------------------------------------------
// What the model reads
// ---------------------------------------------------------------------------

/// What one transcript line costs the prompt, and what the checks need.
struct Line {
    cost: usize,
    injected: bool,
}

/// A line the model was shown, by the number it can cite.
#[derive(Debug, Clone)]
struct Sent {
    id: String,
    speaker: String,
    text: String,
    /// Read as addressed to an AI: never evidence.
    injected: bool,
}

/// The prompt and everything [`check`] needs to judge the reply to it.
#[derive(Debug, Clone)]
pub struct Prepared {
    pub prompt: EnhancePrompt,
    /// Indexed by the number the model cites.
    sent: Vec<Sent>,
    has_transcript: bool,
    /// The note's title and notes: what a block may state without a cite.
    notes_text: String,
    /// Lowercase words that are people or places in this meeting (speaker
    /// labels and capitalized words mid-sentence), for the name check.
    known_names: BTreeSet<String>,
    /// First four letters of every word anywhere in the meeting data, the
    /// notes and the template: a capitalized word outside it is not a word
    /// of this meeting.
    vocabulary: BTreeSet<String>,
    segments_total: usize,
    injected_shown: usize,
}

impl Prepared {
    /// Characters of prompt (system and user) this will send.
    pub fn prompt_chars(&self) -> usize {
        self.prompt.system.len() + self.prompt.user.len()
    }

    pub fn segments_total(&self) -> usize {
        self.segments_total
    }

    /// Transcript lines the model is given.
    pub fn segments_shown(&self) -> usize {
        self.sent.len()
    }

    /// Whether the model reads the whole transcript.
    pub fn is_whole(&self) -> bool {
        self.sent.len() == self.segments_total
    }
}

fn render_cost(label: &str, text: &str) -> usize {
    // "[1234] label: text\n"
    label.len() + text.trim().len() + 10
}

fn lines_of(t: &Transcript) -> Vec<Line> {
    t.segments
        .iter()
        .map(|s| Line {
            cost: render_cost(&t.label_for(&s.speaker_key), &s.text),
            injected: addressed_to_assistant(&s.text),
        })
        .collect()
}

/// Lines spread evenly over the meeting: the transcript is cut into
/// equal-size runs and each contributes its opening lines (that are not
/// chosen yet). Returns what it added.
fn pick_even(lines: &[Line], budget: usize, chosen: &mut BTreeSet<usize>) -> usize {
    let total: usize = lines.iter().map(|l| l.cost).sum();
    if lines.is_empty() || total == 0 || budget == 0 {
        return 0;
    }
    let runs = (budget / EXCERPT_CHARS)
        .clamp(1, MAX_EXCERPTS)
        .min(lines.len());
    let per_run = budget / runs;
    let (mut spent, mut at, mut walked) = (0usize, 0usize, 0usize);
    for run in 0..runs {
        // The run ends where the transcript's running size reaches its share.
        let target = total * (run + 1) / runs;
        let start = at;
        while at < lines.len() && (walked < target || at == start) {
            walked += lines[at].cost;
            at += 1;
        }
        if run + 1 == runs {
            at = lines.len();
        }
        let mut used = 0;
        for (i, line) in lines.iter().enumerate().take(at).skip(start) {
            if chosen.contains(&i) {
                continue;
            }
            if spent + line.cost > budget || (used > 0 && used + line.cost > per_run) {
                break;
            }
            chosen.insert(i);
            used += line.cost;
            spent += line.cost;
        }
    }
    spent
}

/// Which topics of the user's notes each transcript line speaks to.
struct Topics {
    /// `hits[line][term]`.
    hits: Vec<Vec<bool>>,
    /// A term found in few lines says more about where to look.
    weight: Vec<f64>,
}

/// The topics in the user's notes that the transcript speaks to, or `None`
/// when it speaks to none of them.
fn topics_of(note: &Note, t: &Transcript) -> Option<Topics> {
    let all: Vec<String> = salient_terms(&note.scratchpad)
        .into_iter()
        .take(MAX_TOPIC_TERMS)
        .collect();
    if all.is_empty() {
        return None;
    }
    let found: Vec<Vec<bool>> = t
        .segments
        .iter()
        .map(|s| fly_context::passages::matched_terms(&all, &s.text))
        .collect();
    let keep: Vec<usize> = (0..all.len())
        .filter(|&k| found.iter().any(|h| h[k]))
        .collect();
    if keep.is_empty() {
        return None;
    }
    let n = t.segments.len().max(1) as f64;
    let weight = keep
        .iter()
        .map(|&k| {
            let df = found.iter().filter(|h| h[k]).count().max(1);
            (1.0 + n / df as f64).ln()
        })
        .collect();
    let hits = found
        .iter()
        .map(|h| keep.iter().map(|&k| h[k]).collect())
        .collect();
    Some(Topics { hits, weight })
}

/// Lines that speak to the topics, best first, each with the line after it
/// (the reply), until the budget is spent. Returns what it added.
fn pick_topics(
    lines: &[Line],
    topics: &Topics,
    budget: usize,
    chosen: &mut BTreeSet<usize>,
) -> usize {
    let mut ranked: Vec<(usize, f64)> = topics
        .hits
        .iter()
        .enumerate()
        .filter(|(i, _)| !lines[*i].injected && !chosen.contains(i))
        .map(|(i, h)| {
            let score: f64 = h
                .iter()
                .zip(&topics.weight)
                .filter(|(hit, _)| **hit)
                .map(|(_, w)| w)
                .sum();
            (i, score)
        })
        .filter(|(_, s)| *s > 0.0)
        .collect();
    // Best first; ties by position, so the same input always picks the same.
    ranked.sort_by(|a, b| b.1.total_cmp(&a.1).then(a.0.cmp(&b.0)));
    let mut spent = 0;
    for (i, _) in ranked {
        for j in [i, i + 1] {
            let Some(line) = lines.get(j) else { continue };
            if chosen.contains(&j) || (j != i && line.injected) {
                continue;
            }
            if spent + line.cost > budget {
                break;
            }
            chosen.insert(j);
            spent += line.cost;
        }
        if spent >= budget {
            break;
        }
    }
    spent
}

/// The chosen positions, in order, with a marker for every run left out.
fn shown_parts(t: &Transcript, chosen: &BTreeSet<usize>) -> Vec<Shown> {
    let n = t.segments.len();
    let mut out = Vec::new();
    let mut pos = 0;
    while pos < n {
        if chosen.contains(&pos) {
            out.push(Shown::Segment(pos));
            pos += 1;
            continue;
        }
        let start = pos;
        while pos < n && !chosen.contains(&pos) {
            pos += 1;
        }
        out.push(Shown::Gap {
            segments: pos - start,
            ms: t.segments[pos - 1]
                .end_ms
                .saturating_sub(t.segments[start].start_ms),
        });
    }
    out
}

/// Pick the lines for a transcript budget: evenly spaced excerpts, plus the
/// notes' topics. Deterministic: the same input picks the same lines.
fn select(lines: &[Line], topics: Option<&Topics>, budget: usize) -> BTreeSet<usize> {
    let mut chosen = BTreeSet::new();
    let even_budget = if topics.is_some() {
        (budget as f64 * EVEN_SHARE) as usize
    } else {
        budget
    };
    let mut spent = pick_even(lines, even_budget, &mut chosen);
    if let Some(topics) = topics {
        spent += pick_topics(lines, topics, budget.saturating_sub(spent), &mut chosen);
    }
    // Topic lines that ran out leave room: more coverage.
    if spent < budget {
        pick_even(lines, budget - spent, &mut chosen);
    }
    if chosen.is_empty() {
        // Nothing fit (a line bigger than the budget): the shortest line, so
        // the model reads something and the size check can say it is too big.
        if let Some((i, _)) = lines.iter().enumerate().min_by_key(|(_, l)| l.cost) {
            chosen.insert(i);
        }
    }
    chosen
}

/// The transcript text inside a prompt's `<transcript>` fence.
fn transcript_chars(user: &str) -> usize {
    match (user.find("<transcript>\n"), user.find("</transcript>")) {
        (Some(a), Some(b)) if b >= a + 13 => b - (a + 13),
        _ => 0,
    }
}

/// Lowercase words that are names in this meeting: speaker labels (when they
/// are names) and capitalized words in the middle of a sentence anywhere in
/// the transcript, the notes or the title.
fn known_names(note: &Note, t: Option<&Transcript>) -> BTreeSet<String> {
    let mut out = BTreeSet::new();
    let mut add = |w: &str| {
        let l = w.to_lowercase();
        out.insert(l.strip_suffix("'s").map(str::to_string).unwrap_or(l));
    };
    for w in note.title.split_whitespace() {
        add(w.trim_matches(|c: char| !c.is_alphanumeric() && c != '\''));
    }
    for w in mid_sentence_capitals(&note.scratchpad) {
        add(&w);
    }
    if let Some(t) = t {
        for sp in &t.speakers {
            let label = sp.label.trim().to_lowercase();
            let generic = label.is_empty()
                || label == "you"
                || label == "unknown"
                || label.starts_with("speaker ");
            if !generic {
                for w in sp.label.split_whitespace() {
                    add(w.trim_matches(|c: char| !c.is_alphanumeric() && c != '\''));
                }
            }
        }
        for s in &t.segments {
            for w in mid_sentence_capitals(&s.text) {
                add(&w);
            }
        }
    }
    out.retain(|w| !w.is_empty());
    out
}

/// The first (up to) four letters of a word, lowercase.
fn stem4(w: &str) -> String {
    w.chars().take(4).flat_map(char::to_lowercase).collect()
}

/// Every word of the meeting, its notes and the template, by [`stem4`].
fn vocabulary(note: &Note, t: Option<&Transcript>, template: &Template) -> BTreeSet<String> {
    let mut out = BTreeSet::new();
    let mut add = |text: &str| {
        for w in text.split(|c: char| !c.is_alphabetic()) {
            if w.chars().count() >= 2 {
                out.insert(stem4(w));
            }
        }
    };
    add(&note.title);
    add(&note.scratchpad);
    add(&template.name);
    add(&template.structure_hint);
    for s in t.map(|t| t.segments.as_slice()).unwrap_or_default() {
        add(&s.text);
    }
    out
}

/// Words that start with a capital but don't start a sentence.
fn mid_sentence_capitals(text: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut starts_sentence = true;
    for tok in text.split_whitespace() {
        let w = tok.trim_matches(|c: char| !c.is_alphanumeric() && c != '\'');
        if !starts_sentence
            && w.chars().next().is_some_and(char::is_uppercase)
            && w.chars().any(char::is_alphabetic)
        {
            out.push(w.to_string());
        }
        starts_sentence = tok
            .trim_end_matches(['"', '\'', ')', ']', '*', '_'])
            .ends_with(['.', '?', '!', ':']);
    }
    out
}

/// Build the prompt for a note and its transcript, sized to `prompt_chars`
/// (the most prompt text, system and user together, the model can read).
/// A transcript that fits is shown whole; otherwise see the module docs.
pub fn prepare(
    note: &Note,
    transcript: Option<&Transcript>,
    template: &Template,
    profile: &PromptProfile,
    prompt_chars: usize,
) -> Prepared {
    let transcript = transcript.filter(|t| !t.segments.is_empty());
    let notes_text = format!("{}\n{}", note.title, note.scratchpad);
    let base = |prompt: EnhancePrompt, sent: Vec<Sent>, total: usize, injected: usize| Prepared {
        has_transcript: transcript.is_some(),
        notes_text: notes_text.clone(),
        known_names: known_names(note, transcript),
        vocabulary: vocabulary(note, transcript, template),
        segments_total: total,
        injected_shown: injected,
        prompt,
        sent,
    };
    let Some(t) = transcript else {
        let prompt = enhance::build_enhance_prompt(note, None, template, profile);
        return base(prompt, Vec::new(), 0, 0);
    };
    let lines = lines_of(t);
    let all: Vec<Shown> = (0..t.segments.len()).map(Shown::Segment).collect();
    let mut prompt = enhance::build_enhance_prompt_with(note, Some(t), template, profile, &all);
    let mut chosen: BTreeSet<usize> = (0..t.segments.len()).collect();

    if prompt.system.len() + prompt.user.len() > prompt_chars {
        let fixed = prompt.system.len() + prompt.user.len() - transcript_chars(&prompt.user);
        let room = prompt_chars.saturating_sub(fixed + SLACK_CHARS);
        let topics = topics_of(note, t);
        let mut budget = (room as f64 * LINES_SHARE) as usize;
        // The estimate of a line's size is close, not exact: check the real
        // prompt and tighten until it fits.
        for _ in 0..8 {
            chosen = select(&lines, topics.as_ref(), budget);
            prompt = enhance::build_enhance_prompt_with(
                note,
                Some(t),
                template,
                profile,
                &shown_parts(t, &chosen),
            );
            let size = prompt.system.len() + prompt.user.len();
            if size <= prompt_chars || chosen.len() <= 1 {
                break;
            }
            budget = (budget as f64 * (prompt_chars as f64 / size as f64) * 0.95) as usize;
        }
    }

    let sent: Vec<Sent> = chosen
        .iter()
        .map(|&pos| {
            let s = &t.segments[pos];
            Sent {
                id: s.id.clone(),
                speaker: t.label_for(&s.speaker_key),
                text: s.text.clone(),
                injected: lines[pos].injected,
            }
        })
        .collect();
    debug_assert_eq!(
        sent.iter().map(|s| s.id.as_str()).collect::<Vec<_>>(),
        prompt
            .segment_ids
            .iter()
            .map(String::as_str)
            .collect::<Vec<_>>()
    );
    let injected = sent.iter().filter(|s| s.injected).count();
    base(prompt, sent, t.segments.len(), injected)
}

// ---------------------------------------------------------------------------
// What the model said
// ---------------------------------------------------------------------------

/// What an Enhance run did, for the user and the logs. Counts and a few
/// dropped lines (meeting-derived text, shown to the user; never logged).
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct EnhanceReport {
    /// Lines in the transcript, and how many the model was given.
    pub segments_total: u32,
    pub segments_shown: u32,
    /// Of those shown, lines that read as addressed to an AI.
    pub injected_lines: u32,
    /// Blocks saved, by what the user will see.
    pub blocks: u32,
    pub user: u32,
    pub sourced: u32,
    pub flagged: u32,
    pub unsourced: u32,
    /// AI blocks from the notes alone (no transcript to cite).
    pub notes: u32,
    /// Blocks not saved: built only from speech aimed at an AI.
    pub dropped: u32,
    /// What they said, so a line wrongly dropped is not lost (a few, cut
    /// short).
    pub dropped_text: Vec<String>,
    /// `user` blocks that don't restate the user's notes, shown as AI text.
    pub demoted_user: u32,
    /// Citations that named no line the model was shown.
    pub invalid_citations: u32,
    /// Links, images, HTML and emails removed from the text.
    pub links_removed: u32,
    /// The model was run a second time with a smaller prompt because the
    /// first could not be read in full.
    pub retried_smaller: bool,
}

/// Why no note came out of a run.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum EnhanceError {
    /// The provider failed (its message).
    Provider(String),
    /// The reply was an attempt at the block format with nothing usable.
    Unusable,
    /// The model read less of the prompt than was sent.
    Truncated,
    /// Everything it wrote came from speech aimed at an AI.
    NothingLeft,
}

impl std::fmt::Display for EnhanceError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Provider(m) => f.write_str(m),
            Self::Unusable => f.write_str(
                "the AI response came back cut off or malformed — nothing was saved. \
                 Try Enhance again.",
            ),
            Self::Truncated => f.write_str(
                "your model couldn't take in this much of the meeting at once (it ran out of \
                 memory on a long prompt, so it would only have read the end), so nothing was \
                 saved. Try a cloud model in Settings › Ask and Enhance, or close other apps \
                 and try again.",
            ),
            Self::NothingLeft => f.write_str(
                "everything the AI wrote came from speech aimed at an assistant rather than at \
                 the meeting, so nothing was saved.",
            ),
        }
    }
}

impl std::error::Error for EnhanceError {}

/// The blocks to save and what happened.
#[derive(Debug, Clone, PartialEq)]
pub struct Checked {
    pub blocks: Vec<NoteBlock>,
    pub report: EnhanceReport,
}

/// Words that say little about what a line is about (for the notes check).
const STOP: &[&str] = &[
    "the", "and", "for", "with", "that", "this", "from", "have", "has", "had", "will", "are",
    "was", "were", "not", "but", "you", "your", "our", "their", "about", "into", "out", "can",
    "all", "its", "more", "some", "also", "then", "than", "over", "per", "via", "who", "what",
    "when", "where", "how", "why", "any", "one", "two",
];

fn content_words(s: &str) -> Vec<String> {
    s.split(|c: char| !c.is_alphabetic())
        .map(str::to_lowercase)
        .filter(|w| w.chars().count() >= 3 && !STOP.contains(&w.as_str()))
        .collect()
}

/// Same word, allowing an ending ("budgets" ~ "budget"): equal, or the same
/// first four letters.
fn same_word(a: &str, b: &str) -> bool {
    a == b
        || (a.chars().count() >= 4
            && b.chars().count() >= 4
            && a.chars().take(4).eq(b.chars().take(4)))
}

/// The block's text without list or heading syntax, which would read as
/// numbers ("1.") or capitalized words.
fn plain(markdown: &str) -> String {
    markdown
        .lines()
        .map(|l| text::split_prefix(l).1.to_string())
        .collect::<Vec<_>>()
        .join("\n")
}

/// Words that would render as a link, image, embed, HTML or address.
fn link_words(markdown: &str) -> Vec<String> {
    markdown
        .split_whitespace()
        .filter(|w| text::strip_links(w).1 > 0)
        .map(|w| {
            w.trim_matches(|c: char| {
                matches!(
                    c,
                    '.' | ',' | ';' | ':' | '!' | '?' | '"' | '\'' | '*' | '_'
                )
            })
            .to_lowercase()
        })
        .collect()
}

/// Whether a `user` block restates the user's own notes, which is what lets
/// it show as their words. Everything it states must be in the notes (or
/// the title): every link, address and piece of HTML, as written, and every
/// number, date, quote and name. And at least half its content words must be
/// theirs, so a block that borrows a few of their words around something
/// new is not.
fn restates_notes(markdown: &str, p: &Prepared) -> bool {
    let notes = p.notes_text.to_lowercase();
    if link_words(markdown).iter().any(|w| !notes.contains(w)) {
        return false;
    }
    let mine = Source {
        text: &p.notes_text,
        speaker: "",
    };
    if !claims::unsupported(&claim_text(markdown, p), &[mine]).is_empty() {
        return false;
    }
    let words = content_words(&plain(markdown));
    if words.is_empty() {
        return true;
    }
    let theirs = content_words(&p.notes_text);
    let found = words
        .iter()
        .filter(|w| theirs.iter().any(|n| same_word(w, n)))
        .count();
    found as f64 >= words.len() as f64 * NOTES_OVERLAP
}

/// Verbs that follow who is doing something: "Wanda will…", "Eve agreed…".
const AGENT_VERBS: &[&str] = &[
    "will",
    "shall",
    "should",
    "must",
    "needs",
    "agreed",
    "said",
    "asked",
    "volunteered",
    "promised",
    "offered",
    "owns",
    "handles",
    "wants",
    "would",
];

/// `Friday".` and `(Tomas).` as `Friday."` and `(Tomas.)`: the claim check
/// reads a word up to a closing mark that a full stop follows as part of
/// the word (`friday"`), a name that is in no line.
fn stop_before_closer(tok: &str) -> String {
    let mut chars: Vec<char> = tok.chars().collect();
    if let [.., closer, stop] = chars[..] {
        if matches!(closer, '"' | '”' | ')' | ']')
            && matches!(stop, '.' | ',' | ';' | ':' | '!' | '?')
        {
            let n = chars.len();
            chars.swap(n - 2, n - 1);
        }
    }
    chars.into_iter().collect()
}

fn bare_word(tok: &str) -> String {
    let w = tok
        .trim_matches(|c: char| !c.is_alphanumeric() && c != '\'')
        .to_lowercase();
    w.strip_suffix("'s").map(str::to_string).unwrap_or(w)
}

/// A block's text for the claim check: list syntax off, and a capitalized
/// word that only starts a sentence ("Approved the budget") lowercased
/// unless it is a name here. The check's name rule reads a capital as a
/// name; at the start of a sentence a capital says nothing. A word at the
/// start is a name when it is one of this meeting's people, or when it does
/// something ("Wanda will…") and no word of the meeting starts like it.
fn claim_text(markdown: &str, p: &Prepared) -> String {
    let mut out = Vec::new();
    for line in markdown.lines() {
        let toks: Vec<&str> = text::split_prefix(line).1.split_whitespace().collect();
        let mut starts = true;
        let mut words = Vec::new();
        for (k, raw) in toks.iter().enumerate() {
            let mut tok = raw.to_string();
            if starts {
                let w = bare_word(raw);
                let acts = toks
                    .get(k + 1)
                    .is_some_and(|next| AGENT_VERBS.contains(&bare_word(next).as_str()));
                let name =
                    p.known_names.contains(&w) || (acts && !p.vocabulary.contains(&stem4(&w)));
                if !name {
                    if let Some((i, c)) = tok.char_indices().find(|(_, c)| c.is_alphabetic()) {
                        if c.is_uppercase() {
                            let lower: String = c.to_lowercase().collect();
                            tok.replace_range(i..i + c.len_utf8(), &lower);
                        }
                    }
                }
            }
            starts = tok
                .trim_end_matches(['"', '\'', ')', ']', '*', '_'])
                .ends_with(['.', '?', '!', ':']);
            words.push(stop_before_closer(&tok));
        }
        out.push(words.join(" "));
    }
    out.join("\n")
}

fn flag_kind(k: ClaimKind) -> BlockFlagKind {
    match k {
        ClaimKind::Number => BlockFlagKind::Number,
        ClaimKind::Date => BlockFlagKind::Date,
        ClaimKind::Name => BlockFlagKind::Name,
        ClaimKind::Quote => BlockFlagKind::Quote,
    }
}

fn dedup_flags(flags: Vec<BlockFlag>) -> Vec<BlockFlag> {
    let mut seen = BTreeSet::new();
    flags
        .into_iter()
        .filter(|f| seen.insert((format!("{:?}", f.kind), f.detail.clone())))
        .collect()
}

/// Segment numbers the model cited → the lines it was shown, once each in
/// the order cited, and how many named no such line.
fn resolve(cited: &[Option<usize>], shown: usize) -> (Vec<usize>, usize) {
    let mut valid = Vec::new();
    let mut invalid = 0;
    for c in cited {
        match c {
            Some(i) if *i < shown => {
                if !valid.contains(i) {
                    valid.push(*i);
                }
            }
            _ => invalid += 1,
        }
    }
    (valid, invalid)
}

/// What an AI block states that nothing it can lean on supports.
fn judge(md: &str, cited: &[&Sent], invalid: usize, p: &Prepared) -> Vec<BlockFlag> {
    let body = claim_text(md, p);
    let notes = Source {
        text: &p.notes_text,
        speaker: "",
    };
    if cited.is_empty() {
        // Nothing cited: only a number, date or quote makes that a problem
        // (the notes may be where it came from).
        let uncited = claims::unsupported(&body, &[notes]).iter().any(|f| {
            matches!(
                f.kind,
                ClaimKind::Number | ClaimKind::Date | ClaimKind::Quote
            )
        });
        return if uncited {
            vec![BlockFlag {
                kind: BlockFlagKind::UncitedClaim,
                detail: None,
            }]
        } else {
            Vec::new()
        };
    }
    let mut sources: Vec<Source<'_>> = cited
        .iter()
        .map(|s| Source {
            text: &s.text,
            speaker: &s.speaker,
        })
        .collect();
    sources.push(notes);
    let mut flags: Vec<BlockFlag> = claims::unsupported(&body, &sources)
        .into_iter()
        .map(|f| BlockFlag {
            kind: flag_kind(f.kind),
            // (a quote may carry the full stop `stop_before_closer` moved in)
            detail: Some(
                f.value
                    .trim_end_matches(['.', ',', ';', ':', '!', '?'])
                    .to_string(),
            ),
        })
        .collect();
    if !flags.is_empty() && invalid > 0 {
        flags.push(BlockFlag {
            kind: BlockFlagKind::InvalidCitation,
            detail: None,
        });
    }
    dedup_flags(flags)
}

/// Cut a dropped line short for the report.
fn excerpt(md: &str) -> String {
    let one = md.split_whitespace().collect::<Vec<_>>().join(" ");
    if one.chars().count() <= DROPPED_CHARS {
        return one;
    }
    let cut: String = one.chars().take(DROPPED_CHARS).collect();
    format!("{}…", cut.trim_end())
}

/// Judge a model's reply to a [`Prepared`] prompt: the blocks to save and
/// what was done to them. Errors when there is nothing to save, so the
/// caller never replaces a note's enhanced document with nothing.
pub fn check(output: &str, p: &Prepared) -> Result<Checked, EnhanceError> {
    let clean = text::strip_reasoning(output);
    let raw: Vec<RawEnhanceBlock> = match enhance::parse_enhance_output(&clean) {
        ParsedEnhance::Unusable => return Err(EnhanceError::Unusable),
        ParsedEnhance::Blocks(b) => b,
        ParsedEnhance::Prose(paragraphs) => paragraphs
            .into_iter()
            .map(|markdown| RawEnhanceBlock {
                user: false,
                markdown,
                sources: Vec::new(),
            })
            .collect(),
    };
    let mut report = EnhanceReport {
        segments_total: p.segments_total as u32,
        segments_shown: p.sent.len() as u32,
        injected_lines: p.injected_shown as u32,
        ..Default::default()
    };
    let mut blocks = Vec::new();
    for b in raw {
        if b.user && restates_notes(&b.markdown, p) {
            // Their own words, as they wrote them: nothing to strip.
            report.user += 1;
            blocks.push(NoteBlock::user(b.markdown));
            continue;
        }
        let demoted = b.user;
        let (stripped, removed) = text::strip_links(&b.markdown);
        let md = if removed > 0 {
            report.links_removed += removed as u32;
            stripped.trim().to_string()
        } else {
            b.markdown.trim().to_string()
        };
        if md.chars().all(|c| !c.is_alphanumeric()) {
            continue;
        }
        if demoted {
            report.demoted_user += 1;
        }
        let (valid, invalid) = resolve(&b.sources, p.sent.len());
        report.invalid_citations += invalid as u32;
        let trusted: Vec<&Sent> = valid
            .iter()
            .map(|&i| &p.sent[i])
            .filter(|s| !s.injected)
            .collect();
        if !valid.is_empty() && trusted.is_empty() {
            // Built only from speech aimed at an AI.
            report.dropped += 1;
            if report.dropped_text.len() < DROPPED_KEPT {
                report.dropped_text.push(excerpt(&md));
            }
            continue;
        }
        let mut flags = if p.has_transcript {
            judge(&md, &trusted, invalid, p)
        } else {
            Vec::new()
        };
        if demoted {
            flags.insert(
                0,
                BlockFlag {
                    kind: BlockFlagKind::NotFromNotes,
                    detail: None,
                },
            );
        }
        let ids: Vec<String> = trusted.iter().map(|s| s.id.clone()).collect();
        if !flags.is_empty() {
            report.flagged += 1;
        } else if !p.has_transcript {
            report.notes += 1;
        } else if ids.is_empty() {
            report.unsourced += 1;
        } else {
            report.sourced += 1;
        }
        blocks.push(NoteBlock::ai_flagged(md, ids, flags));
    }
    if blocks.is_empty() {
        return Err(if report.dropped > 0 {
            EnhanceError::NothingLeft
        } else {
            EnhanceError::Unusable
        });
    }
    report.blocks = blocks.len() as u32;
    Ok(Checked { blocks, report })
}

// ---------------------------------------------------------------------------
// Running it
// ---------------------------------------------------------------------------

/// One Enhance run.
pub struct EnhanceRequest<'a> {
    pub note: &'a Note,
    pub transcript: Option<&'a Transcript>,
    pub template: &'a Template,
    pub profile: &'a PromptProfile,
    /// The most prompt text, in characters, the model can read in full. For
    /// Ollama that is its sized `num_ctx` (fly-llm `max_prompt_chars`); for
    /// a hosted model, a safe share of its window.
    pub prompt_chars: usize,
    /// A smaller size to try once if the model could not read the first in
    /// full (an Ollama that fell back to a smaller context). `None` fails at
    /// once.
    pub retry_prompt_chars: Option<usize>,
}

/// Prepare, ask, check. The request is the one Enhance has always sent: the
/// profile's output budget, thinking mode, effort and (for profiled local
/// models) the block-array grammar. Only the transcript it carries differs
/// when the meeting is too long.
pub async fn enhance(
    provider: &dyn LLMProvider,
    req: EnhanceRequest<'_>,
) -> Result<Checked, EnhanceError> {
    let profile = req.profile;
    let sizes = std::iter::once(req.prompt_chars).chain(req.retry_prompt_chars);
    let mut retried = false;
    let mut last = None;
    for size in sizes {
        let prepared = prepare(req.note, req.transcript, req.template, profile, size);
        if last.is_some_and(|before: usize| prepared.prompt_chars() >= before) {
            break; // a smaller size didn't make the prompt smaller
        }
        last = Some(prepared.prompt_chars());
        let (output, usage) = provider
            .chat_with_usage(ChatRequest {
                effort: profile.effort.enhance.map(String::from),
                messages: vec![
                    ChatMessage::system(prepared.prompt.system.clone()),
                    ChatMessage::user(prepared.prompt.user.clone()),
                ],
                temperature: Some(0.2),
                max_tokens: Some(profile.max_tokens.enhance.unwrap_or(4096)),
                // Adaptive thinking helps cloud models here; local thinking
                // models opt out via their profile (the trace would eat the
                // output budget — see prompt_profile.rs).
                thinking: if profile.thinking_disabled() {
                    ThinkingMode::Disabled
                } else {
                    ThinkingMode::Default
                },
                // Grammar-constrained output for profiled models (llama3.1);
                // ignored by non-Ollama providers.
                format: profile
                    .constrained_enhance
                    .then(enhance::enhance_blocks_schema),
            })
            .await
            .map_err(|e| EnhanceError::Provider(e.to_string()))?;
        if usage.context_fallback {
            // The server ran with less context than the prompt was sized
            // for: what came back was written from the end of it.
            tracing::warn!(
                prompt_chars = prepared.prompt_chars(),
                "enhance: the model ran with a smaller context than the prompt needs"
            );
            retried = true;
            continue;
        }
        let mut done = check(&output, &prepared)?;
        done.report.retried_smaller = retried && size != req.prompt_chars;
        return Ok(done);
    }
    Err(EnhanceError::Truncated)
}

/// Counts by trust, for tests and the evals: sourced / flagged / unsourced.
pub fn tally(blocks: &[NoteBlock]) -> BTreeMap<&'static str, usize> {
    let mut m = BTreeMap::new();
    for b in blocks {
        let k = match &b.origin {
            fly_core::BlockOrigin::User => "user",
            _ if !b.flags.is_empty() => "flagged",
            fly_core::BlockOrigin::Ai { source_segment_ids } if source_segment_ids.is_empty() => {
                "unsourced"
            }
            _ => "sourced",
        };
        *m.entry(k).or_insert(0) += 1;
    }
    m
}

#[cfg(test)]
mod tests;
