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
use fly_core::{BlockFlag, BlockFlagKind, Note, NoteBlock, PartialRead, Template, Transcript};
use fly_llm::{ChatMessage, ChatRequest, LLMProvider, ThinkingMode};
use serde::{Deserialize, Serialize};

use crate::claims::{self, ClaimKind, Source};
use crate::prefetch::salient_terms;
use crate::proposals::{aim_of, is_frame_word, Aim};
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
/// Words of time, quantity and place that any line may use: sharing one with
/// an injected line proves nothing.
const GENERIC_WORDS: &[&str] = &[
    "today",
    "tomorrow",
    "yesterday",
    "tonight",
    "week",
    "weeks",
    "month",
    "months",
    "year",
    "years",
    "day",
    "days",
    "morning",
    "afternoon",
    "evening",
    "later",
    "soon",
    "time",
    "times",
    "thing",
    "things",
    "every",
    "everyone",
    "everybody",
    "anyone",
    "someone",
    "people",
    "team",
    "meeting",
    "call",
    "item",
    "items",
    "attendee",
    "attendees",
    "first",
    "last",
    "next",
    "right",
    "good",
    "great",
    "make",
    "need",
    "needs",
    "want",
    "take",
    "give",
    "work",
];
/// Dropped blocks kept in the report (so a wrongly dropped line is not lost
/// to the user), and how much of each.
const DROPPED_KEPT: usize = 5;
const DROPPED_CHARS: usize = 200;
/// What one transcript line costs the prompt, and what the checks need.
struct Line {
    cost: usize,
    /// Read as addressed to an AI assistant: never picked as a topic line.
    injected: bool,
}

/// A line the model was shown, by the number it can cite.
#[derive(Debug, Clone)]
struct Sent {
    id: String,
    speaker: String,
    text: String,
    /// How the line reads. [`Aim::Assistant`] is never evidence; an
    /// [`Aim::Order`] is evidence a reader should check.
    aim: Aim,
}

// ---------------------------------------------------------------------------
// What the model reads
// ---------------------------------------------------------------------------

/// The prompt and everything [`check`] needs to judge the reply to it.
#[derive(Debug, Clone)]
pub struct Prepared {
    pub prompt: EnhancePrompt,
    /// Indexed by the number the model cites.
    sent: Vec<Sent>,
    has_transcript: bool,
    /// The user's own notes, which are not the title (a calendar invite can
    /// set that): what a `user` block may restate, and what an AI block may
    /// state without a cite. The title vouches for neither: it would let a
    /// swapped owner or a number ("Priya and Tomas sync", "...90k") pass.
    scratchpad: String,
    /// The note's title without the names of the people who spoke: a project
    /// or a company in it vouches for a name in an AI block ("Harrowgate
    /// renewal call"), a person never does ("Priya and Tomas sync" must not
    /// let a swapped owner pass). It never vouches for a number, a date or a
    /// quote (`claims::Source::title`).
    vouching_title: String,
    /// Content words of the template's name and headings: a block under
    /// "## Action items" says "action" and "items" without any line saying so.
    template_words: Vec<String>,
    /// Lowercase words that are people or places in this meeting (speaker
    /// labels and capitalized words mid-sentence), for the name check.
    known_names: BTreeSet<String>,
    /// First four letters of every word anywhere in the meeting data, the
    /// notes and the template: a capitalized word outside it is not a word
    /// of this meeting.
    vocabulary: BTreeSet<String>,
    /// Every word of the same data, whole (lowercase), for the closer match
    /// a name needs: "Marcus" is not "March".
    words: BTreeSet<String>,
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
            injected: aim_of(&s.text) == Aim::Assistant,
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

/// The meeting's closing lines, where it wraps up and says what was decided
/// and who does what. Returns what it added.
fn pick_tail(lines: &[Line], budget: usize, chosen: &mut BTreeSet<usize>) -> usize {
    let mut spent = 0;
    for (i, line) in lines.iter().enumerate().rev() {
        if chosen.contains(&i) {
            continue;
        }
        if spent + line.cost > budget {
            break;
        }
        chosen.insert(i);
        spent += line.cost;
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
    // The close of the meeting first, then evenly spaced excerpts (which open
    // each run, so would otherwise miss it).
    let tail = pick_tail(lines, (even_budget / 8).min(EXCERPT_CHARS), &mut chosen);
    let mut spent = tail + pick_even(lines, even_budget.saturating_sub(tail), &mut chosen);
    if let Some(topics) = topics {
        spent += pick_topics(lines, topics, budget.saturating_sub(spent), &mut chosen);
    }
    // Topic lines that ran out leave room: more coverage.
    if spent < budget {
        pick_even(lines, budget - spent, &mut chosen);
    }
    if chosen.is_empty() {
        // Nothing fit (a line bigger than the budget): the shortest line, so
        // the prompt is as small as it can be; `enhance` refuses to send it
        // if it is still over the budget.
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

/// The note's title less the words that are a speaker's name.
fn vouching_title(note: &Note, t: Option<&Transcript>) -> String {
    let people: BTreeSet<String> = t
        .map(|t| {
            t.speakers
                .iter()
                .flat_map(|sp| sp.label.split_whitespace())
                .map(|w| {
                    w.trim_matches(|c: char| !c.is_alphanumeric())
                        .to_lowercase()
                })
                .collect()
        })
        .unwrap_or_default();
    note.title
        .split_whitespace()
        .filter(|w| {
            let l = w
                .trim_matches(|c: char| !c.is_alphanumeric())
                .to_lowercase();
            let l = l.strip_suffix("'s").unwrap_or(&l);
            !people.contains(l)
        })
        .collect::<Vec<_>>()
        .join(" ")
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

/// Every word of the meeting data, the notes and the template, whole.
fn meeting_words(note: &Note, t: Option<&Transcript>, template: &Template) -> BTreeSet<String> {
    let mut out = BTreeSet::new();
    let mut add = |text: &str| {
        for w in text.split(|c: char| !c.is_alphabetic()) {
            if w.chars().count() >= 2 {
                out.insert(w.to_lowercase());
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
    let base = |prompt: EnhancePrompt, sent: Vec<Sent>, total: usize, injected: usize| Prepared {
        has_transcript: transcript.is_some(),
        scratchpad: note.scratchpad.clone(),
        vouching_title: vouching_title(note, transcript),
        template_words: content_words(&format!("{} {}", template.name, template.structure_hint)),
        known_names: known_names(note, transcript),
        vocabulary: vocabulary(note, transcript, template),
        words: meeting_words(note, transcript, template),
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
                aim: aim_of(&s.text),
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
    let injected = sent.iter().filter(|s| s.aim == Aim::Assistant).count();
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
    /// The notes and template alone are more than the model can read, so
    /// there is no room left for the meeting: nothing was sent.
    TooLong,
    /// Everything it wrote came from speech aimed at an AI.
    NothingLeft,
}

impl EnhanceError {
    /// A fixed word for the log: the failure's kind, never its text (a
    /// provider's message can carry what it was sent).
    pub fn code(&self) -> &'static str {
        match self {
            Self::Provider(_) => "provider",
            Self::Unusable => "unusable",
            Self::Truncated => "truncated",
            Self::TooLong => "too_long",
            Self::NothingLeft => "nothing_left",
        }
    }

    /// The failures where a model that reads more at once would have
    /// worked, so a hint to pick one is worth giving (unless the note is
    /// Keep local, which only a local model may read).
    pub fn wants_a_longer_context(&self) -> bool {
        matches!(self, Self::Truncated | Self::TooLong)
    }
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
                 saved. Close other apps and try again, or pick a model that reads more at once \
                 in Settings › Ask and Enhance.",
            ),
            Self::TooLong => f.write_str(
                "your notes are too long for this model to read along with the meeting, so \
                 nothing was saved. Shorten the notes, or pick a model that reads more at once \
                 in Settings › Ask and Enhance.",
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

/// The words of a text that would render as a link, image, embed, HTML or
/// address, trimmed of the marks around them, as written.
fn link_words(markdown: &str) -> BTreeSet<String> {
    markdown
        .split_whitespace()
        .filter(|w| text::strip_links_and_emails(w).1 > 0)
        .map(|w| {
            w.trim_matches(|c: char| {
                matches!(
                    c,
                    '.' | ','
                        | ';'
                        | ':'
                        | '!'
                        | '?'
                        | '"'
                        | '\''
                        | '*'
                        | '_'
                        | '('
                        | ')'
                        | '<'
                        | '>'
                        | '['
                        | ']'
                )
            })
            .to_string()
        })
        .collect()
}

/// Words of a text, lowercase, letters and digits only ("Don't" is two).
fn norm_words(s: &str) -> Vec<String> {
    s.to_lowercase()
        .split(|c: char| !c.is_alphanumeric())
        .filter(|w| !w.is_empty())
        .map(str::to_string)
        .collect()
}

/// Words with their apostrophes kept ("didn't"), lowercase, one sentence
/// at a time (a negation does not reach into the next sentence).
fn sentence_tokens(s: &str) -> Vec<Vec<String>> {
    s.split(['.', '!', '?', ';', '\n'])
        .map(|sentence| {
            sentence
                .to_lowercase()
                .replace(['\u{2019}', '\u{2018}'], "'")
                .split(|c: char| !(c.is_alphanumeric() || c == '\''))
                .map(|w| w.trim_matches('\'').to_string())
                .filter(|w| !w.is_empty())
                .collect()
        })
        .filter(|t: &Vec<String>| !t.is_empty())
        .collect()
}

/// [`sentence_tokens`], and cut again at a comma, colon, dash or "but": a
/// negation reaches one clause ("You can't promise a title, but you'll
/// propose a bonus" says yes to the bonus).
fn clause_tokens(s: &str) -> Vec<Vec<String>> {
    sentence_tokens(
        &s.replace([',', ':', '\u{2014}', '\u{2013}', '('], ".")
            .replace(" but ", "."),
    )
}

const NEGATORS: &[&str] = &[
    "not", "never", "no", "nobody", "nothing", "nowhere", "without", "cannot", "neither", "nor",
    "none",
];

fn is_negator(tok: &str) -> bool {
    NEGATORS.contains(&tok) || tok.ends_with("n't")
}

/// Whether a text negates anything ("don't", "not", "never", "without").
fn has_negation(s: &str) -> bool {
    sentence_tokens(s).iter().flatten().any(|t| is_negator(t))
}

/// Same word, or one typo apart (a cleanup fixes "suplier"): five letters or
/// more, one letter changed, added or dropped.
fn close_word(a: &str, b: &str) -> bool {
    if same_word(a, b) {
        return true;
    }
    let (a, b): (Vec<char>, Vec<char>) = (a.chars().collect(), b.chars().collect());
    if a.len() < 5 || b.len() < 5 || a.len().abs_diff(b.len()) > 1 {
        return false;
    }
    let skip = a.iter().zip(&b).take_while(|(x, y)| x == y).count();
    let (ra, rb) = (&a[skip..], &b[skip..]);
    match ra.len().cmp(&rb.len()) {
        std::cmp::Ordering::Equal => ra[1..] == rb[1..],
        std::cmp::Ordering::Greater => ra[1..] == rb[..],
        std::cmp::Ordering::Less => ra[..] == rb[1..],
    }
}

/// One line of the user's notes, ready to compare a restatement with.
struct NoteLine {
    /// Every word, lowercase.
    words: Vec<String>,
    /// The words that say what it is about.
    content: Vec<String>,
    negated: bool,
}

fn note_lines(scratchpad: &str) -> Vec<NoteLine> {
    scratchpad
        .lines()
        .filter_map(|l| {
            let plain = text::split_prefix(l).1;
            let words = norm_words(plain);
            (!words.is_empty()).then(|| NoteLine {
                content: content_words(plain),
                negated: has_negation(plain),
                words,
            })
        })
        .collect()
}

/// `needle` appears in `hay` as a run of consecutive words.
fn contains_run(hay: &[String], needle: &[String]) -> bool {
    !needle.is_empty() && hay.windows(needle.len()).any(|w| w == needle)
}

/// Whether one line of a `user` block is the user's own: their words from
/// one line of their notes (most of that line, so a fragment can't flip
/// it: "do not delete the backups" is not "delete the backups"), or those
/// words lightly reworded: nearly all its content words in one note line
/// (a fifth may be new, none in a short line), covering most of that line,
/// and not an order to an assistant. A line with no words (a rule, a bare bullet)
/// says nothing.
fn line_is_theirs(line: &str, notes: &[NoteLine]) -> bool {
    let plain = text::split_prefix(line).1;
    let words = norm_words(plain);
    if words.is_empty() {
        return true;
    }
    // (and it says what they said: a run that drops their "do not" doesn't)
    let negated = has_negation(plain);
    if notes.iter().any(|n| {
        n.negated == negated
            && words.len() * 10 >= n.words.len() * 7
            && contains_run(&n.words, &words)
    }) {
        return true;
    }
    let mine = content_words(plain);
    if mine.is_empty() || aim_of(plain) != Aim::People {
        return false;
    }
    notes.iter().any(|n| {
        if n.content.is_empty() || n.negated != negated {
            return false;
        }
        let found = mine
            .iter()
            .filter(|w| n.content.iter().any(|c| close_word(w, c)))
            .count();
        let covered = n
            .content
            .iter()
            .filter(|c| mine.iter().any(|w| close_word(w, c)))
            .count();
        // New words: none in a short line, one in six to ten, and so on.
        let new_words = mine.len() - found;
        new_words <= (mine.len() - 1) / 5 && covered * 5 >= n.content.len() * 3
    })
}

/// How far a `user` block is cut for the flag that says what was not theirs.
const FOREIGN_CHARS: usize = 80;

fn short(text: &str) -> String {
    let one = text.split_whitespace().collect::<Vec<_>>().join(" ");
    if one.chars().count() <= FOREIGN_CHARS {
        return one;
    }
    let cut: String = one.chars().take(FOREIGN_CHARS).collect();
    format!("{}…", cut.trim_end())
}

/// What stops a `user` block from being the user's own, or `None` if nothing
/// does: it restates the user's notes (not the title, which an invite can
/// set). Every link, address and piece of HTML must be one of theirs as a
/// whole word as written, and every number, date, quote and name must be in
/// their notes. And every line of it must be theirs ([`line_is_theirs`]) on
/// its own: a block that repeats two of their lines and adds a third is not
/// theirs. The reason is the first thing that failed: the link, the figure or
/// name, or the line.
fn foreign_part(markdown: &str, p: &Prepared) -> Option<String> {
    let theirs = link_words(&p.scratchpad);
    if let Some(w) = link_words(markdown)
        .into_iter()
        .find(|w| !theirs.contains(w))
    {
        return Some(short(&w));
    }
    let scratchpad = settle(&p.scratchpad);
    let mine = Source {
        text: &scratchpad,
        speaker: "",
        // (an invite sets the title: it vouches for nothing a `user` block says)
        title: "",
    };
    if let Some(f) = claims::unsupported(&claim_text(markdown, p), &[mine]).first() {
        return Some(short(
            f.value.trim_end_matches(['.', ',', ';', ':', '!', '?']),
        ));
    }
    let notes = note_lines(&p.scratchpad);
    markdown
        .lines()
        .find(|l| !line_is_theirs(l, &notes))
        .map(|l| short(text::split_prefix(l).1))
}

/// Words that open a sentence, a bullet or a label in meeting notes without
/// naming anyone: structure words, connectives, and the verbs and nouns a
/// summary is made of. A capitalized word at the start of a line that is in
/// none of the meeting's words and not here is taken for a person's name
/// ("Marcus approved…", "Owner: Marcus."), so a name that no line holds
/// cannot pass as sourced.
const COMMON: &[&str] = &[
    // structure and labels
    "summary",
    "overview",
    "decision",
    "decisions",
    "action",
    "actions",
    "item",
    "items",
    "note",
    "notes",
    "next",
    "step",
    "steps",
    "follow",
    "followup",
    "follow-up",
    "followups",
    "risk",
    "risks",
    "question",
    "questions",
    "open",
    "topic",
    "topics",
    "discussion",
    "background",
    "context",
    "attendee",
    "attendees",
    "agenda",
    "key",
    "takeaway",
    "takeaways",
    "highlights",
    "status",
    "update",
    "updates",
    "timeline",
    "budget",
    "owner",
    "owners",
    "assignee",
    "due",
    "deadline",
    "date",
    "dates",
    "priority",
    "blocker",
    "blockers",
    "issue",
    "issues",
    "outcome",
    "outcomes",
    "recap",
    "details",
    "detail",
    "other",
    "misc",
    "miscellaneous",
    "todo",
    "tbd",
    "meeting",
    "call",
    "team",
    "project",
    "plan",
    "plans",
    "goal",
    "goals",
    "result",
    "results",
    "problem",
    "problems",
    "solution",
    "proposal",
    "approach",
    "requirement",
    "requirements",
    "impact",
    "performance",
    "rationale",
    "reasoning",
    "concern",
    "concerns",
    "benefit",
    "benefits",
    "challenge",
    "challenges",
    "implication",
    "implications",
    "recommendation",
    "recommendations",
    "observation",
    "observations",
    "insight",
    "insights",
    "learning",
    "learnings",
    "progress",
    "dependency",
    "dependencies",
    "assumption",
    "assumptions",
    "metric",
    "metrics",
    "objective",
    "objectives",
    "deliverable",
    "deliverables",
    "milestone",
    "milestones",
    "analysis",
    "evaluation",
    "assessment",
    "strategy",
    "opportunity",
    "opportunities",
    "mitigation",
    "resolution",
    "rollout",
    "purpose",
    "scope",
    "constraint",
    "constraints",
    "highlight",
    "win",
    "wins",
    "comment",
    "comments",
    "remarks",
    "up",
    "ideas",
    "idea",
    "reminder",
    "reminders",
    "agreement",
    "agreements",
    "delete",
    "email",
    "contact",
    "book",
    "fix",
    "build",
    "test",
    "write",
    "read",
    "find",
    "pay",
    "buy",
    "sign",
    "wire",
    "transfer",
    "refund",
    "hire",
    "hiring",
    "feedback",
    "number",
    "numbers",
    "total",
    "cost",
    "costs",
    "price",
    "pricing",
    "schedule",
    "review",
    "reviews",
    "release",
    "launch",
    "demo",
    "contract",
    "invoice",
    "payment",
    "order",
    "customer",
    "customers",
    "client",
    "clients",
    "vendor",
    "vendors",
    "supplier",
    "suppliers",
    "product",
    "products",
    "feature",
    "features",
    "version",
    "report",
    "reports",
    "task",
    "tasks",
    "everyone",
    "everybody",
    "someone",
    "anyone",
    "nobody",
    "somebody",
    "people",
    "group",
    "company",
    "management",
    "leadership",
    "board",
    "staff",
    "owner's",
    // articles, pronouns, determiners, numbers
    "the",
    "a",
    "an",
    "this",
    "that",
    "these",
    "those",
    "it",
    "its",
    "it's",
    "we",
    "we'll",
    "we're",
    "we've",
    "they",
    "they'll",
    "they're",
    "he",
    "she",
    "i",
    "i'll",
    "i'm",
    "i've",
    "you",
    "you'll",
    "your",
    "our",
    "their",
    "his",
    "her",
    "my",
    "there",
    "there's",
    "here",
    "that's",
    "let's",
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
    "both",
    "all",
    "some",
    "most",
    "several",
    "each",
    "every",
    "any",
    "many",
    "few",
    "no",
    "none",
    "not",
    "only",
    "such",
    "same",
    "another",
    "more",
    "less",
    "much",
    "other",
    "others",
    // prepositions, conjunctions, adverbs
    "in",
    "on",
    "at",
    "by",
    "for",
    "with",
    "from",
    "to",
    "of",
    "about",
    "after",
    "before",
    "during",
    "until",
    "while",
    "if",
    "when",
    "as",
    "since",
    "because",
    "so",
    "but",
    "and",
    "or",
    "yet",
    "though",
    "although",
    "however",
    "also",
    "then",
    "overall",
    "finally",
    "first",
    "second",
    "third",
    "lastly",
    "meanwhile",
    "additionally",
    "further",
    "furthermore",
    "still",
    "now",
    "today",
    "tomorrow",
    "yesterday",
    "later",
    "soon",
    "currently",
    "recently",
    "already",
    "per",
    "via",
    "within",
    "without",
    "across",
    "between",
    "regarding",
    "re",
    "ok",
    "okay",
    "yes",
    "please",
    "thanks",
    "thank",
    "what",
    "who",
    "why",
    "how",
    "where",
    "which",
    "whether",
    "should",
    "could",
    "would",
    "will",
    "can",
    "may",
    "might",
    "must",
    "need",
    "needs",
    "needed",
    "is",
    "are",
    "was",
    "were",
    "be",
    "been",
    "being",
    "do",
    "does",
    "did",
    "has",
    "have",
    "had",
    "get",
    "got",
    "make",
    "made",
    "take",
    "took",
    "give",
    "gave",
    "keep",
    "kept",
    "let",
    "see",
    "look",
    "want",
    "wants",
    "wanted",
    "like",
    "think",
    "know",
    "say",
    "said",
    "ask",
    "asked",
    "tell",
    "told",
    "go",
    "going",
    "come",
    "put",
    "run",
    "use",
    "used",
    "try",
    "tried",
    "work",
    "working",
    "worked",
    // what a summary says was done
    "approve",
    "approved",
    "approves",
    "reject",
    "rejected",
    "confirm",
    "confirmed",
    "confirms",
    "decide",
    "decided",
    "decides",
    "agree",
    "agreed",
    "agrees",
    "discuss",
    "discussed",
    "discusses",
    "review",
    "reviewed",
    "send",
    "sent",
    "share",
    "shared",
    "schedule",
    "scheduled",
    "check",
    "checked",
    "draft",
    "drafted",
    "finalize",
    "finalized",
    "finalise",
    "finalised",
    "finish",
    "finished",
    "complete",
    "completed",
    "start",
    "started",
    "stop",
    "stopped",
    "ship",
    "shipped",
    "deliver",
    "delivered",
    "prepare",
    "prepared",
    "create",
    "created",
    "add",
    "added",
    "remove",
    "removed",
    "move",
    "moved",
    "reach",
    "reached",
    "raise",
    "raised",
    "propose",
    "proposed",
    "suggest",
    "suggested",
    "request",
    "requested",
    "report",
    "reported",
    "mention",
    "mentioned",
    "explain",
    "explained",
    "note",
    "noted",
    "plan",
    "planned",
    "delay",
    "delayed",
    "postpone",
    "postponed",
    "cancel",
    "cancelled",
    "canceled",
    "defer",
    "deferred",
    "assign",
    "assigned",
    "owns",
    "handle",
    "handles",
    "handled",
    "provide",
    "provided",
    "coordinate",
    "coordinated",
    "arrange",
    "arranged",
    "organize",
    "organized",
    "update",
    "updated",
    "follow",
    "followed",
    "pending",
    "blocked",
    "waiting",
    "waits",
    "wait",
    "ready",
    "done",
    "good",
    "great",
    "new",
    "old",
    "big",
    "small",
    "long",
    "short",
    "high",
    "low",
    "early",
    "late",
    "important",
    "urgent",
    "possible",
    "likely",
    "unclear",
    "unknown",
    "total",
    "final",
    "initial",
    "current",
    "previous",
    "last",
    "upcoming",
    "weekly",
    "monthly",
    "daily",
    "annual",
    "quarterly",
];

/// Groups, departments and processes: what a summary line is about, and
/// who "agreed" or "completed" ("Finance agreed…", "Onboarding completed…",
/// "Migration delayed…"), not a person.
const GROUP_NOUNS: &[&str] = &[
    "participants",
    "stakeholders",
    "attendees",
    "everyone",
    "finance",
    "legal",
    "sales",
    "marketing",
    "operations",
    "engineering",
    "product",
    "design",
    "support",
    "security",
    "hr",
    "it",
    "ops",
    "devops",
    "data",
    "analytics",
    "research",
    "leadership",
    "executives",
    "board",
    "committee",
    "department",
    "departments",
    "group",
    "groups",
    "team",
    "teams",
    "customers",
    "partners",
    "vendors",
    "suppliers",
    "contractors",
    "reviewers",
    "managers",
    "onboarding",
    "offboarding",
    "migration",
    "migrations",
    "integration",
    "integrations",
    "implementation",
    "deployment",
    "installation",
    "rollout",
    "launch",
    "testing",
    "training",
    "hiring",
    "recruiting",
    "procurement",
    "budgeting",
    "forecasting",
    "planning",
    "reporting",
    "compliance",
    "audit",
    "billing",
    "invoicing",
    "renewal",
    "renewals",
    "pipeline",
    "roadmap",
    "backlog",
    "sprint",
    "release",
    "releases",
    "infrastructure",
    "development",
    "maintenance",
    "review",
    "reviews",
    "approval",
    "approvals",
    "payment",
    "payments",
    "shipping",
    "delivery",
    "deliveries",
    "production",
    "manufacturing",
    "logistics",
    "inventory",
    "pricing",
    "negotiation",
    "negotiations",
    "evaluation",
    "assessment",
    "analysis",
    "survey",
    "audit",
    "cleanup",
    "refactor",
    "redesign",
    "upgrade",
    "upgrades",
];

/// An ordinary word of a summary, by itself or as every part of a hyphenated
/// one.
fn is_common(w: &str) -> bool {
    let one = |x: &str| COMMON.contains(&x) || GROUP_NOUNS.contains(&x);
    one(w) || w.split('-').all(|part| part.is_empty() || one(part))
}

/// Whether a word, or an inflection of it ("budgets", "replacing"), is in
/// the meeting's data. Five letters in common and all but two of the
/// shorter word's: enough for an ending, not for "Marcus" and "March".
fn in_the_meeting(w: &str, p: &Prepared) -> bool {
    if p.words.contains(w) {
        return true;
    }
    let head: String = w.chars().take(5).collect();
    head.chars().count() == 5
        && p.words
            .range(head.clone()..)
            .take_while(|other| other.starts_with(&head))
            .any(|other| {
                let common = w
                    .chars()
                    .zip(other.chars())
                    .take_while(|(a, b)| a == b)
                    .count();
                common
                    >= w.chars()
                        .count()
                        .min(other.chars().count())
                        .saturating_sub(2)
            })
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

/// Labels that are followed by a person: "Owner: Marcus."
const ROLE_LABELS: &[&str] = &[
    "owner",
    "owners",
    "assignee",
    "assigned",
    "lead",
    "dri",
    "responsible",
    "contact",
    "who",
    "speaker",
    "attendees",
    "participants",
    "presenter",
    "facilitator",
    "approver",
    "requester",
    "from",
    "by",
    "cc",
];

/// Words after a comma that make the word before it a person: "Marcus, the
/// CFO".
const APPOSITIVE: &[&str] = &[
    "the", "a", "an", "our", "my", "who", "from", "of", "your", "their",
];

/// Whether a capitalized word that starts a sentence or label is taken to
/// name someone: it is a name in this meeting, or it is in none of the
/// meeting's words, is no ordinary word of a summary, and is placed like a
/// person: before a colon, a verb that someone does ("Marcus approved",
/// "Marcus will", "Marcus to confirm"), a comma and an appositive ("Marcus,
/// the CFO"), or after a label such as "Owner:". A topic noun that opens a
/// line ("Rollout is in Q1") is not.
fn names_someone(raw: &str, prev: Option<&str>, next: Option<&str>, p: &Prepared) -> bool {
    let w = bare_word(raw);
    if p.known_names.contains(&w) {
        return true;
    }
    if !w.starts_with(|c: char| c.is_alphabetic()) || in_the_meeting(&w, p) || is_common(&w) {
        return false;
    }
    let closed = raw.trim_end_matches(['"', '\'', ')', ']', '*', '_']);
    let after = next.map(bare_word).unwrap_or_default();
    let after_raw = next.unwrap_or_default();
    // The label before it, without bold or italic marks ("**Owner:**").
    let label = prev.map(|l| l.trim_end_matches(['*', '_']));
    // (a bold label, "**Impact:**", is a heading, not a speaker)
    raw.trim_end_matches(['"', '\'', ')', ']']).ends_with(':')
        || (closed.ends_with(',') && APPOSITIVE.contains(&after.as_str()))
        || AGENT_VERBS.contains(&after.as_str())
        || after == "to"
        || (after.chars().count() >= 5 && after.ends_with("ed"))
        // the template's owner-first shape: "Marcus — confirm the supplier — Friday"
        || matches!(after_raw, "—" | "–" | "―" | "-" | "--")
        // "Marcus confirm the supplier", "Marcus (CFO) handles…"
        || (OWNER_VERBS.contains(&after.as_str()) && !looks_plural(&w))
        || after_raw.starts_with('(')
        // "[Marcus] …", "Marcus's team will…"
        || (closed.starts_with(['[', '(']) && raw.trim_end_matches(['.', ',', ':', ';']).ends_with([']', ')']))
        || closed.trim_end_matches(['.', ',', ':', ';']).ends_with("'s")
        || closed.trim_end_matches(['.', ',', ':', ';']).ends_with("\u{2019}s")
        || label.is_some_and(|l| l.ends_with(':') && ROLE_LABELS.contains(&bare_word(l).as_str()))
}

/// A plural common noun ("Prices", "Developers", "Agents", "Stakeholders")
/// rather than a name: the endings that plurals take and names seldom do.
fn looks_plural(w: &str) -> bool {
    [
        "ers", "ors", "ents", "ants", "ions", "ings", "ices", "ies", "ets", "ards", "ands",
    ]
    .iter()
    .any(|suffix| w.len() > suffix.len() + 1 && w.ends_with(suffix))
}

/// Base-form verbs that follow an owner in an action line written without a
/// colon: "Marcus confirm the supplier".
const OWNER_VERBS: &[&str] = &[
    "confirm",
    "send",
    "follow",
    "check",
    "schedule",
    "finalize",
    "finalise",
    "share",
    "draft",
    "prepare",
    "approve",
    "handle",
    "book",
    "fix",
    "provide",
    "circulate",
    "sign",
    "contact",
    "arrange",
    "coordinate",
    "deliver",
    "submit",
    "resolve",
    "investigate",
    "pay",
    "buy",
    "email",
    "call",
    "ask",
    "review",
    "update",
];

/// Business initialisms a summary uses for what a speaker spelled out
/// ("annual recurring revenue" → ARR). They are not people, so a block that
/// says one the cited lines don't isn't claiming a name.
const INITIALISMS: &[&str] = &[
    "api", "arr", "aws", "b2b", "b2c", "ceo", "cfo", "cmo", "coo", "cpu", "crm", "cto", "dpa",
    "erp", "eta", "faq", "gpu", "gtm", "hr", "ios", "kpi", "mrr", "mvp", "nda", "okr", "pdf",
    "poc", "qa", "qbr", "rfp", "roi", "saas", "sdk", "sla", "sql", "sso", "ui", "ux", "vp",
];

/// An all-capitals token that names no person: a listed business initialism,
/// a quarter, half or fiscal year (Q1, H2, FY26), or a word this meeting's
/// own words, notes or template already use. Anything else in capitals
/// ("EVE") stays a name for the name check to find.
fn is_benign_initialism(core: &str, p: &Prepared) -> bool {
    let mut chars = core.chars();
    let letters_and_digits = core.chars().count() >= 2
        && chars.next().is_some_and(char::is_alphabetic)
        && core.chars().all(|c| c.is_uppercase() || c.is_ascii_digit());
    if !letters_and_digits || p.known_names.contains(&core.to_lowercase()) {
        return false;
    }
    let lower = core.to_lowercase();
    let period = {
        let digits = lower.trim_start_matches(|c: char| c.is_alphabetic());
        let tag = &lower[..lower.len() - digits.len()];
        !digits.is_empty()
            && ((matches!(tag, "q" | "h") && digits.len() == 1)
                || (tag == "fy" && matches!(digits.len(), 2 | 4)))
    };
    period || INITIALISMS.contains(&lower.as_str()) || p.vocabulary.contains(&stem4(&lower))
}

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

/// [`stop_before_closer`] over a whole text (line breaks kept): a source
/// that says "Nov 10)," would otherwise never hold the number 10.
fn settle(text: &str) -> String {
    text.lines()
        .map(|l| {
            l.split_whitespace()
                .map(stop_before_closer)
                .collect::<Vec<_>>()
                .join(" ")
        })
        .collect::<Vec<_>>()
        .join("\n")
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
/// start (of a line, a sentence or after a label's colon) is a name when it
/// is one of this meeting's people, or when no word of the meeting starts
/// like it and it is no ordinary word of a summary ([`names_someone`]):
/// "Marcus approved…", "Owner: Marcus." and "- Marcus: …" all claim him.
fn claim_text(markdown: &str, p: &Prepared) -> String {
    let mut out = Vec::new();
    for line in markdown.lines() {
        let toks: Vec<&str> = text::split_prefix(line).1.split_whitespace().collect();
        let mut starts = true;
        let mut words = Vec::new();
        for (k, raw) in toks.iter().enumerate() {
            let mut tok = raw.to_string();
            let core = raw.trim_matches(|c: char| !c.is_alphanumeric());
            if is_benign_initialism(core, p) || GROUP_NOUNS.contains(&core.to_lowercase().as_str())
            {
                // (a group or a process, "Sales, Marketing and Operations
                // agreed…", is not a person, even in capitals)
                tok = tok.to_lowercase();
            }
            if starts {
                let prev = k.checked_sub(1).map(|i| toks[i]);
                let next = toks.get(k + 1).copied();
                let name = names_someone(raw, prev, next, p);
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

/// Pairs of opposite verbs, by the start of the word: the first side says a
/// thing happened, the second that it did not or the reverse.
const OPPOSITES: &[(&[&str], &[&str])] = &[
    (&["approv"], &["reject", "veto", "denied", "denies", "deny"]),
    (&["accept"], &["declin", "refus"]),
    (&["agre"], &["disagre"]),
    (&["confirm"], &["cancel"]),
    (&["increas"], &["decreas", "reduc"]),
    (&["includ"], &["exclud"]),
    (&["support"], &["oppos"]),
];

/// What a text says about one pair: the word that says yes and the word that
/// says no, if any. A negation within three words before a verb turns it
/// round ("not approving" says no; "not rejected" says yes).
fn stance(text: &str, pair: &(&[&str], &[&str])) -> (Option<String>, Option<String>) {
    let (mut yes, mut no) = (None, None);
    for sentence in sentence_tokens(text) {
        for (i, w) in sentence.iter().enumerate() {
            let affirming = pair.0.iter().any(|s| w.starts_with(s));
            let reversing = pair.1.iter().any(|s| w.starts_with(s));
            if !(affirming || reversing) {
                continue;
            }
            let negated = sentence[i.saturating_sub(3)..i]
                .iter()
                .any(|t| is_negator(t));
            let slot = if affirming != negated {
                &mut yes
            } else {
                &mut no
            };
            slot.get_or_insert_with(|| w.clone());
        }
    }
    (yes, no)
}

/// The word a block uses to say the opposite of what every line it can lean
/// on says ("approved" against "not approving"). Cheap on purpose: a pair
/// of verbs and a negation, not a reading of the sentence.
fn reversed(block: &str, pool: &[String]) -> Option<String> {
    OPPOSITES.iter().find_map(|pair| {
        let (yes, no) = stance(block, pair);
        let (mut said_yes, mut said_no) = (false, false);
        for t in pool {
            let (y, n) = stance(t, pair);
            said_yes |= y.is_some();
            said_no |= n.is_some();
        }
        match (yes, no) {
            (Some(w), None) if said_no && !said_yes => Some(w),
            (None, Some(w)) if said_yes && !said_no => Some(w),
            _ => None,
        }
    })
}

/// What an AI block states that nothing it can lean on supports. `evidence`
/// are the cited lines that may back it (never a line aimed at an assistant).
fn judge(md: &str, evidence: &[&Sent], invalid: usize, p: &Prepared) -> Vec<BlockFlag> {
    let body = claim_text(md, p);
    let notes_text = settle(&p.scratchpad);
    let notes = Source {
        text: &notes_text,
        speaker: "",
        title: &p.vouching_title,
    };
    if evidence.is_empty() {
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
    let lines: Vec<String> = evidence.iter().map(|s| settle(&s.text)).collect();
    let mut sources: Vec<Source<'_>> = evidence
        .iter()
        .zip(&lines)
        .map(|(s, text)| Source {
            text,
            speaker: &s.speaker,
            title: &p.vouching_title,
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
    let mut pool: Vec<String> = evidence.iter().map(|s| s.text.clone()).collect();
    pool.push(p.scratchpad.clone());
    let body_text = plain(md);
    if let Some(word) = reversed(&without_open_items(&body_text, &pool), &pool) {
        flags.push(BlockFlag {
            kind: BlockFlagKind::Reversed,
            detail: Some(word),
        });
    } else if polarity_flips(&body_text, &pool) {
        flags.push(BlockFlag {
            kind: BlockFlagKind::Reversed,
            detail: None,
        });
    }
    dedup_flags(flags)
}

/// Negative verbs that turn a sentence round the way a negator does:
/// "declined to confirm", "refused", "rejected".
const NEGATIVE_VERBS: &[&str] = &["declin", "refus", "reject", "denied", "denies", "deny"];

fn is_negative(tok: &str) -> bool {
    is_negator(tok) || NEGATIVE_VERBS.iter().any(|v| tok.starts_with(v))
}

/// The content words of one sentence's tokens, negations left out.
fn sentence_content(tokens: &[String]) -> Vec<&str> {
    tokens
        .iter()
        .map(String::as_str)
        .filter(|w| {
            w.chars().count() >= 3
                && !STOP.contains(w)
                && !w.contains('\'')
                && !is_negative(w)
                // (where something stands, not what it is about)
                && !matches!(*w, "yet" | "still")
                && w.chars().all(char::is_alphabetic)
        })
        .collect()
}

/// Words that make a clause forward-looking: what is still to be done, or
/// planned ("will finalize", "let's confirm", "need to approve", "going to",
/// "arrives Friday").
const FUTURE_MARKERS: &[&str] = &[
    "will",
    "shall",
    "should",
    "lets",
    "let's",
    "need",
    "needs",
    "going",
    "gonna",
    "plan",
    "plans",
    "planning",
    "tomorrow",
    "soon",
    "later",
    "next",
    "monday",
    "tuesday",
    "wednesday",
    "thursday",
    "friday",
    "saturday",
    "sunday",
];

/// Modal verbs: "will not send" is about the future, not about a state.
const MODALS: &[&str] = &[
    "will", "would", "shall", "should", "can", "could", "must", "may", "might",
];

fn is_forward(tokens: &[String]) -> bool {
    tokens
        .iter()
        .any(|t| FUTURE_MARKERS.contains(&t.as_str()) || t.ends_with("'ll"))
}

/// A clause that reports where something stands: "yet", "still", "pending" or
/// "open" in it, or a plain "not" before a participle ("not finalized", "has
/// not arrived", "isn't updated"; "will not send" and "doesn't want" are
/// not).
fn has_open_word(tokens: &[String]) -> bool {
    tokens
        .iter()
        .any(|t| matches!(t.as_str(), "yet" | "still" | "pending" | "open"))
}

fn is_state_negation(tokens: &[String]) -> bool {
    tokens.iter().enumerate().any(|(i, t)| {
        let plain_not =
            t == "not" || (t.ends_with("n't") && !MODALS.iter().any(|m| t.starts_with(m)));
        let modal_before = i > 0 && MODALS.contains(&tokens[i - 1].as_str());
        plain_not
            && !modal_before
            && !matches!(t.as_str(), "cannot" | "can't" | "won't")
            && tokens
                .get(i + 1)
                .is_some_and(|n| n.chars().count() >= 4 && (n.ends_with("ed") || n.ends_with("en")))
    })
}

/// Whether `other` is a close clause for a block clause with these content
/// words: it holds all of them (two or more), or three or more and two thirds.
fn close_clause(words: &[&str], other: &[String]) -> bool {
    let shared = words
        .iter()
        .filter(|w| other.iter().any(|t| same_word(w, t)))
        .count();
    shared >= 2 && (shared == words.len() || (shared >= 3 && shared * 3 >= words.len() * 2))
}

/// A block clause that reports where something stands: "yet", "still",
/// "pending" or "open" in it, or a state negation ("not finalized").
fn is_open_item(tokens: &[String]) -> bool {
    has_open_word(tokens) || is_state_negation(tokens)
}

/// The block's sentences minus its open items that the source is still
/// planning: an open item is dropped from the verb-pair guard only when a
/// source clause that is close to it is forward-looking. Against a source
/// that states the thing as settled ("The supplier confirmed on the phone.")
/// an open item ("not confirmed yet") is still read, so "yet" cannot be
/// added to a flipped sentence to get it past the guard.
fn without_open_items(block: &str, pool: &[String]) -> String {
    let theirs: Vec<Vec<String>> = pool.iter().flat_map(|t| clause_tokens(t)).collect();
    block
        .split(['.', '!', '?', ';', '\n'])
        .filter(|sentence| {
            !clause_tokens(sentence).iter().any(|mine| {
                let words = sentence_content(mine);
                is_open_item(mine)
                    && theirs
                        .iter()
                        .any(|other| is_forward(other) && close_clause(&words, other))
            })
        })
        .collect::<Vec<_>>()
        .join(". ")
}

/// A sentence of the block that says yes where every close sentence of what
/// it can lean on says no, or the reverse: "will not send the draft" against
/// "Tomas will send the draft", "will renew" against "not going to renew".
/// A close sentence holds all of the block sentence's content words (two or
/// more), or three or more and two thirds of them. Cheap on purpose: it
/// compares whether a negation is there, not what it negates.
///
/// Where something stands is not a flip. "Budget not finalized." against
/// "Let's finalize the budget on Monday." is the open item a summary should
/// state: an open item ("yet", "still", "pending", "open", or a state report
/// such as "not finalized" or "has not arrived") is not judged against a close
/// source clause that is planning or scheduling it. It is judged against a
/// close clause that states the thing as settled, whatever words it has.
fn polarity_flips(block: &str, pool: &[String]) -> bool {
    let theirs: Vec<Vec<String>> = pool.iter().flat_map(|t| clause_tokens(t)).collect();
    clause_tokens(block).iter().any(|mine| {
        let words = sentence_content(mine);
        if words.len() < 2 {
            return false;
        }
        let open = is_open_item(mine);
        // (an even count is a double negative: "did not decline" says yes)
        let negated = mine.iter().filter(|t| is_negative(t)).count() % 2 == 1;
        let (mut agree, mut disagree) = (false, false);
        for other in &theirs {
            if !close_clause(&words, other) || (open && is_forward(other)) {
                continue;
            }
            if (other.iter().filter(|t| is_negative(t)).count() % 2 == 1) == negated {
                agree = true;
            } else {
                disagree = true;
            }
        }
        disagree && !agree
    })
}

/// How a block leans on a line aimed at an assistant or phrased as an order
/// that it did not cite.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Borrow {
    No,
    /// It uses a word of what an assistant was told to do.
    Assistant,
    /// It uses several words of a colleague's order.
    Order,
}

/// Whether a block says what only an uncited aimed line says. Take the words
/// of such a line, less the frame of the request ("hey fly", "add an action
/// item") and the words of the template's headings: a block that uses one of
/// what is left, where the line is aimed at an assistant, or three or more of
/// them, where it is a colleague's order, and a word that is in nothing the
/// block can lean on (the lines it cites, their speakers, the notes) was
/// written from that line. One word is enough for an assistant, so a short
/// payload ("mark the vendor as approved" → "The vendor is approved.") is
/// caught; an order needs three, since an accurate block often shares a word
/// or two with the neighbouring order ("the kiln invoice is due").
fn borrows_from_aimed(md: &str, evidence: &[&Sent], p: &Prepared) -> Borrow {
    let block: BTreeSet<String> = content_words(&plain(md)).into_iter().collect();
    if block.is_empty() {
        return Borrow::No;
    }
    let mut support: Vec<String> = content_words(&p.scratchpad);
    support.extend(p.template_words.iter().cloned());
    for s in evidence {
        support.extend(content_words(&s.text));
        support.extend(content_words(&s.speaker));
    }
    let backed = |w: &String| support.iter().any(|s| same_word(w, s));
    let mut found = Borrow::No;
    for s in p
        .sent
        .iter()
        .filter(|s| s.aim != Aim::People && !evidence.iter().any(|e| e.id == s.id))
    {
        let payload: Vec<String> = content_words(&s.text)
            .into_iter()
            .filter(|t| !is_frame_word(t))
            .collect();
        let shared: Vec<&String> = block
            .iter()
            .filter(|w| {
                !GENERIC_WORDS.contains(&w.as_str()) && payload.iter().any(|t| same_word(w, t))
            })
            .collect();
        let unbacked = shared.iter().any(|w| !backed(w));
        match s.aim {
            Aim::Assistant if unbacked => return Borrow::Assistant,
            Aim::Order if unbacked && shared.len() >= 3 => found = Borrow::Order,
            _ => {}
        }
    }
    found
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
        let foreign = if b.user {
            foreign_part(&b.markdown, p)
        } else {
            None
        };
        if b.user && foreign.is_none() {
            // Their own words, as they wrote them: nothing to strip.
            report.user += 1;
            blocks.push(NoteBlock::user(b.markdown));
            continue;
        }
        let demoted = b.user;
        let (stripped, removed) = text::strip_links_and_emails(&b.markdown);
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
        let cited: Vec<&Sent> = valid.iter().map(|&i| &p.sent[i]).collect();
        // A line aimed at an assistant is never evidence.
        let aimed = cited.iter().any(|s| s.aim == Aim::Assistant);
        let evidence: Vec<&Sent> = cited
            .iter()
            .copied()
            .filter(|s| s.aim != Aim::Assistant)
            .collect();
        if !valid.is_empty() && evidence.is_empty() {
            // Built only from speech aimed at an AI.
            report.dropped += 1;
            if report.dropped_text.len() < DROPPED_KEPT {
                report.dropped_text.push(excerpt(&md));
            }
            continue;
        }
        let mut flags = if p.has_transcript {
            judge(&md, &evidence, invalid, p)
        } else {
            Vec::new()
        };
        if p.has_transcript {
            let borrow = borrows_from_aimed(&md, &evidence, p);
            if aimed || borrow == Borrow::Assistant {
                // It cites an injected line next to an innocent one, or says
                // what only an injected line says: not a sourced claim.
                flags.insert(
                    0,
                    BlockFlag {
                        kind: BlockFlagKind::AimedAtAi,
                        detail: None,
                    },
                );
            } else if borrow == Borrow::Order || evidence.iter().any(|s| s.aim == Aim::Order) {
                // It rests on a line phrased as an order, alone or next to an
                // ordinary one: what colleagues say to each other, and what an
                // injected line can look like. Its numbers and names are
                // read from that line, so it is for the reader to check.
                flags.insert(
                    0,
                    BlockFlag {
                        kind: BlockFlagKind::Instruction,
                        detail: None,
                    },
                );
            }
        }
        if demoted {
            flags.insert(
                0,
                BlockFlag {
                    kind: BlockFlagKind::NotFromNotes,
                    detail: foreign.clone().filter(|d| !d.is_empty()),
                },
            );
        }
        let ids: Vec<String> = evidence.iter().map(|s| s.id.clone()).collect();
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
    if !p.is_whole() {
        // Saved with the note, so a partial read never looks complete.
        let read = PartialRead {
            shown: p.sent.len() as u32,
            total: p.segments_total as u32,
        };
        for b in &mut blocks {
            if matches!(b.origin, fly_core::BlockOrigin::Ai { .. }) {
                b.partial = Some(read);
            }
        }
    }
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
    let mut over = false;
    let mut last = None;
    for size in sizes {
        let prepared = prepare(req.note, req.transcript, req.template, profile, size);
        if prepared.prompt_chars() > size {
            // Selection can't cut the notes, the template or one huge line:
            // a prompt over what the model can read is one it would trim.
            over = true;
            continue;
        }
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
            let chars = prepared.prompt_chars();
            tracing::warn!(
                chars,
                "enhance: the model ran with a smaller context than the prompt needs"
            );
            retried = true;
            continue;
        }
        let mut done = check(&output, &prepared)?;
        done.report.retried_smaller = retried && size != req.prompt_chars;
        return Ok(done);
    }
    // Nothing was sent when every size was over budget; otherwise the model
    // ran with less than it was sent.
    Err(if over && !retried {
        EnhanceError::TooLong
    } else {
        EnhanceError::Truncated
    })
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
