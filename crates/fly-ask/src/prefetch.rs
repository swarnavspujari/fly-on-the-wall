//! Deterministic prefetch (spec §4.2 step 2, §4.2a #8 and #10): the harness
//! — not the model — retrieves transcript passages, extracted items and the
//! user's own notes, and presents them as `[seg:ID]`-marked lines sized to
//! the tier's prompt budget.
//!
//! * **Search** goes through `fly_context::Context::search_passages` only
//!   (hybrid keyword + semantic, Keep-local filtered in SQL), asking for
//!   its deepest result (20 passages) and taking the best that fit.
//! * **One meeting / one note**: the whole transcript when it fits the
//!   budget (nothing to miss); otherwise the question's passages plus
//!   single-term sub-searches, because search caps passages per meeting.
//! * **Notes demoted**: note paragraphs can't be cited by segment, so at
//!   most `max_note_passages` of them, after every transcript excerpt.
//! * **Passages clipped** around their best-matching line.
//! * **Router filters are soft**: dates that leave fewer than 3 passages
//!   are dropped and the search runs again.
//!
//! What the model was shown is exactly what lands in [`Evidence`]: after
//! rendering, only ids whose markers appear in the prompt text count.

use std::collections::{HashMap, HashSet};

use chrono::{DateTime, Utc};
use fly_context::passages::{lexical_score, query_terms};
use fly_context::{
    Context, GetItemsArgs, GetNoteArgs, MemoryPrefetch, NoteKind, Passage, PassageSource,
    QueryVector, SearchPassagesArgs, SegmentView,
};
use fly_core::{ItemKind, MeetingItem};
use fly_storage::Storage;
use serde::{Deserialize, Serialize};

use crate::evidence::{self, Evidence, SegmentInfo};
use crate::router::Hints;
use crate::scope::Resolved;
use crate::tier::Budget;

/// Deepest search fly-context offers.
const SEARCH_LIMIT: u32 = 20;
/// Router filters are dropped when they leave fewer passages than this.
const SOFT_FILTER_MIN: usize = 3;
/// Single-term sub-searches for a long single meeting.
const MAX_SUB_QUERIES: usize = 3;
/// Most meetings a narrow scope reads whole.
const FULL_READ_MEETINGS: usize = 3;
/// The plain search's top transcript passages shown whole (retrieval
/// alone shows the top 8 passages).
const MUST_INCLUDE: usize = 8;
/// Most items fetched before the char budget applies.
const ITEM_FETCH: u32 = 40;

/// Question words that say nothing about the topic (for the floor and
/// sub-queries).
const GENERIC: &[&str] = &[
    "decide",
    "decided",
    "decision",
    "current",
    "currently",
    "now",
    "meeting",
    "meetings",
    "discuss",
    "discussed",
    "talk",
    "talked",
    "think",
    "happen",
    "happened",
    "plan",
    "planned",
    "update",
    "latest",
    "status",
    "much",
    "many",
    "long",
    "going",
    "get",
    "got",
    "make",
    "made",
    "need",
    "new",
    "has",
    "have",
    "had",
    "will",
    "would",
    "should",
    "could",
    "can",
    "any",
    "anything",
    "everything",
    "all",
    "tell",
    "know",
    "mention",
    "mentioned",
    "agree",
    "agreed",
    "ask",
    "asked",
    "summarize",
    "summary",
    "please",
    "give",
    "list",
    "show",
    "explain",
    "happening",
    "next",
    "last",
    "first",
    "today",
    "yesterday",
    "week",
    "month",
    "time",
    "again",
    "still",
    "yet",
    "so",
    "far",
    "ever",
    "also",
    "just",
    "some",
    "there",
    "here",
    "its",
    "it's",
    "what's",
    "whats",
    "who's",
    "how's",
    "we're",
    "they",
    "them",
    "these",
    "those",
    "one",
    "ones",
    "thing",
    "things",
];

/// Topic terms of a question.
pub fn salient_terms(q: &str) -> Vec<String> {
    query_terms(q)
        .into_iter()
        .filter(|t| !GENERIC.contains(&t.as_str()) && t.chars().count() >= 2)
        .collect()
}

/// A crude English stem: "capped" → "cap", "budgets" → "budget",
/// "planning" → "plan".
fn stem(w: &str) -> String {
    let mut s = w.to_lowercase();
    for suf in ["ing", "ed", "es", "s"] {
        if s.len() >= suf.len() + 3 && s.ends_with(suf) {
            s.truncate(s.len() - suf.len());
            break;
        }
    }
    let b = s.as_bytes();
    if b.len() >= 4 && b[b.len() - 1] == b[b.len() - 2] && !b"aeiou".contains(&b[b.len() - 1]) {
        s.pop();
    }
    s
}

/// Share of `terms` found in `text`: same stem, or (4+ chars) a word
/// starting with the term, as fly-context's lexical score.
pub fn coverage(terms: &[String], text: &str) -> f64 {
    if terms.is_empty() {
        return 1.0;
    }
    let words: HashSet<String> = text
        .split(|c: char| !c.is_alphanumeric())
        .filter(|w| !w.is_empty())
        .map(stem)
        .collect();
    let hits = terms
        .iter()
        .filter(|t| {
            let st = stem(t);
            words.contains(&st) || lexical_score(std::slice::from_ref(*t), text) > 0
        })
        .count();
    hits as f64 / terms.len() as f64
}

/// A precomputed query embedding.
#[derive(Debug, Clone)]
pub struct QueryEmbedding {
    pub query: String,
    pub vector: Vec<f32>,
}

pub struct PrefetchInput<'a> {
    pub question: &'a str,
    /// The question joined with the previous one, for a short follow-up.
    pub follow_up_query: Option<&'a str>,
    pub resolved: &'a Resolved,
    pub hints: &'a Hints,
    pub budget: Budget,
    pub embeddings: &'a [QueryEmbedding],
    pub embed_model: Option<&'a str>,
}

/// Counts for the run log (no content).
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct PrefetchStats {
    pub searches: u32,
    pub semantic: bool,
    pub passages_found: u32,
    pub transcript_passages: u32,
    pub note_passages: u32,
    pub full_transcripts: u32,
    pub items: u32,
    pub notes_included: bool,
    pub segments: u32,
    pub context_chars: u32,
    pub router_fallback: bool,
    pub items_fallback: bool,
    /// Facts in a memory briefing (a person or a recurring meeting).
    pub memory_facts: u32,
}

#[derive(Debug, Clone, Default)]
pub struct Prefetch {
    /// The fenced context for the prompt.
    pub context: String,
    pub evidence: Evidence,
    pub stats: PrefetchStats,
    /// Share of the question's topic terms present in the context.
    pub coverage: f64,
    pub terms: usize,
}

/// A transcript excerpt about to be rendered.
struct Excerpt {
    meeting_id: String,
    title: String,
    started_at: Option<DateTime<Utc>>,
    segments: Vec<SegmentView>,
    lines: Vec<usize>,
    /// One of the plain search's top passages: shown whole, always kept.
    must: bool,
}

fn line_chars(s: &SegmentView) -> usize {
    s.text.len() + s.speaker.len() + s.id.len() + 16
}

/// Lines of a passage within `max` chars, centered on the line that best
/// matches the question (always at least that line).
pub fn clip(segs: &[SegmentView], terms: &[String], max: usize) -> Vec<usize> {
    if segs.is_empty() {
        return vec![];
    }
    let lens: Vec<usize> = segs.iter().map(line_chars).collect();
    if lens.iter().sum::<usize>() <= max {
        return (0..segs.len()).collect();
    }
    let best = (0..segs.len())
        .max_by(|&a, &b| {
            lexical_score(terms, &segs[a].text)
                .cmp(&lexical_score(terms, &segs[b].text))
                .then(b.cmp(&a))
        })
        .unwrap_or(0);
    let (mut lo, mut hi, mut total) = (best, best, lens[best]);
    loop {
        let mut grew = false;
        if hi + 1 < segs.len() && total + lens[hi + 1] <= max {
            hi += 1;
            total += lens[hi];
            grew = true;
        }
        if lo > 0 && total + lens[lo - 1] <= max {
            lo -= 1;
            total += lens[lo];
            grew = true;
        }
        if !grew {
            break;
        }
    }
    (lo..=hi).collect()
}

fn excerpt_chars(e: &Excerpt) -> usize {
    e.lines
        .iter()
        .map(|&i| line_chars(&e.segments[i]))
        .sum::<usize>()
        + e.title.len()
        + 24
}

fn search(
    ctx: &Context<'_>,
    args: &SearchPassagesArgs,
    input: &PrefetchInput<'_>,
    stats: &mut PrefetchStats,
) -> Result<Vec<Passage>, fly_context::ContextError> {
    let qv = input
        .embeddings
        .iter()
        .find(|e| e.query == args.query)
        .zip(input.embed_model)
        .map(|(e, model)| QueryVector {
            vector: &e.vector,
            model,
        });
    stats.searches += 1;
    let r = ctx.search_passages(args, qv)?;
    stats.semantic |= r.semantic;
    Ok(r.passages)
}

fn key(p: &Passage) -> (String, Option<u64>, String) {
    (
        p.meeting_id.clone().unwrap_or_default(),
        p.start_ms,
        p.note_id.clone(),
    )
}

/// Append `more` to `into`, skipping passages already there.
fn merge(into: &mut Vec<Passage>, more: Vec<Passage>) {
    let have: HashSet<_> = into.iter().map(key).collect();
    into.extend(more.into_iter().filter(|p| !have.contains(&key(p))));
}

/// Whole transcripts of a narrow scope, if they fit.
fn full_read(s: &Storage, r: &Resolved, budget: &Budget) -> Option<Vec<Excerpt>> {
    let ids = r.meeting_ids.as_ref()?;
    if r.wide || ids.is_empty() || ids.len() > FULL_READ_MEETINGS {
        return None;
    }
    let mut out = Vec::new();
    for id in ids {
        let m = r.meeting(id)?;
        let Ok(Some(t)) = s.get_transcript(id) else {
            continue;
        };
        let segments: Vec<SegmentView> = t
            .segments
            .iter()
            .filter(|seg| !seg.text.trim().is_empty())
            .map(|seg| SegmentView {
                id: seg.id.clone(),
                speaker: t.label_for(&seg.speaker_key),
                speaker_key: seg.speaker_key.clone(),
                start_ms: seg.start_ms,
                end_ms: seg.end_ms,
                text: seg.text.trim().to_string(),
            })
            .collect();
        let lines = (0..segments.len()).collect();
        out.push(Excerpt {
            meeting_id: id.clone(),
            title: m.title.clone(),
            started_at: Some(m.started_at),
            segments,
            lines,
            must: true,
        });
    }
    let total: usize = out.iter().map(excerpt_chars).sum();
    (total <= budget.context_chars && !out.is_empty()).then_some(out)
}

/// Segment views of one meeting by id (for items' citations).
fn segments_of(s: &Storage, meeting_id: &str) -> HashMap<String, SegmentView> {
    let Ok(Some(t)) = s.get_transcript(meeting_id) else {
        return HashMap::new();
    };
    t.segments
        .iter()
        .map(|seg| {
            (
                seg.id.clone(),
                SegmentView {
                    id: seg.id.clone(),
                    speaker: t.label_for(&seg.speaker_key),
                    speaker_key: seg.speaker_key.clone(),
                    start_ms: seg.start_ms,
                    end_ms: seg.end_ms,
                    text: seg.text.trim().to_string(),
                },
            )
        })
        .collect()
}

fn item_kind_str(k: ItemKind) -> String {
    k.as_str().to_string()
}

/// Run the prefetch. Storage reads only; the caller embedded the queries.
pub fn prefetch(
    s: &Storage,
    input: &PrefetchInput<'_>,
) -> Result<Prefetch, fly_context::ContextError> {
    let r = input.resolved;
    let b = input.budget;
    let ctx = Context::new(s, r.visibility);
    let terms = query_terms(input.question);
    let salient = salient_terms(input.question);
    // A short follow-up ("and who capped it?") is about the earlier
    // question too.
    let floor_terms = match input.follow_up_query {
        Some(fq) => salient_terms(fq),
        None => salient.clone(),
    };
    let mut stats = PrefetchStats::default();

    // --- transcript excerpts -------------------------------------------
    let mut excerpts: Vec<Excerpt> = Vec::new();
    let mut note_passages: Vec<Passage> = Vec::new();
    if let Some(full) = full_read(s, r, &b) {
        stats.full_transcripts = full.len() as u32;
        excerpts = full;
    } else if r.meeting_ids.as_ref().is_some_and(|ids| ids.is_empty()) {
        // A note with no recordings: nothing to search.
    } else {
        // A scope's meetings, in batches the search accepts (`None` = the
        // whole visible corpus, or the folder via `folder_id`).
        let batches: Vec<Option<Vec<String>>> = match &r.meeting_ids {
            None => vec![None],
            Some(ids) => ids
                .chunks(fly_context::tools::MAX_MEETING_IDS)
                .map(|c| Some(c.to_vec()))
                .collect(),
        };
        let base = |meeting_ids: Option<Vec<String>>, query: &str| SearchPassagesArgs {
            query: query.to_string(),
            meeting_ids,
            folder_id: r
                .folder_id
                .clone()
                .or_else(|| input.hints.folder_id.clone()),
            since: None,
            until: None,
            limit: Some(SEARCH_LIMIT),
        };
        let dated = |a: SearchPassagesArgs| SearchPassagesArgs {
            since: input.hints.since.map(|d| d.to_rfc3339()),
            until: input.hints.until.map(|d| d.to_rfc3339()),
            ..a
        };
        let has_dates = input.hints.since.is_some() || input.hints.until.is_some();

        // 1. The plain search (exactly what retrieval alone would show): its
        //    top transcript passages always go in whole.
        let mut plain: Vec<Passage> = Vec::new();
        for batch in &batches {
            merge(
                &mut plain,
                search(
                    &ctx,
                    &base(batch.clone(), input.question),
                    input,
                    &mut stats,
                )?,
            );
        }
        // 2. Extras: the router's date filter, the follow-up, subfolders,
        //    and single-term searches for one long meeting.
        let mut extra: Vec<Passage> = Vec::new();
        if has_dates {
            let mut dated_hits = Vec::new();
            for batch in &batches {
                merge(
                    &mut dated_hits,
                    search(
                        &ctx,
                        &dated(base(batch.clone(), input.question)),
                        input,
                        &mut stats,
                    )?,
                );
            }
            if dated_hits.len() < SOFT_FILTER_MIN {
                stats.router_fallback = true;
            }
            merge(&mut extra, dated_hits);
        }
        if let Some(fq) = input.follow_up_query {
            for batch in &batches {
                let a = base(batch.clone(), fq);
                let a = if has_dates { dated(a) } else { a };
                merge(&mut extra, search(&ctx, &a, input, &mut stats)?);
            }
        }
        for chunk in r
            .sub_meeting_ids
            .chunks(fly_context::tools::MAX_MEETING_IDS)
        {
            let a = SearchPassagesArgs {
                folder_id: None,
                ..base(Some(chunk.to_vec()), input.question)
            };
            merge(&mut extra, search(&ctx, &a, input, &mut stats)?);
        }
        if !r.wide && r.meeting_ids.is_some() {
            let mut subs: Vec<&String> = salient.iter().collect();
            subs.sort_by_key(|t| std::cmp::Reverse(t.chars().count()));
            for t in subs.into_iter().take(MAX_SUB_QUERIES) {
                for batch in &batches {
                    merge(
                        &mut extra,
                        search(&ctx, &base(batch.clone(), t), input, &mut stats)?,
                    );
                }
            }
        }

        // The scope's own notes only (a narrow scope never shows another
        // note's paragraphs, whatever the search returned).
        let scope_notes: HashSet<&str> = r
            .meetings
            .iter()
            .map(|m| m.note_id.as_str())
            .chain(r.note_id.as_deref())
            .collect();
        let note_allowed =
            |p: &Passage| r.meeting_ids.is_none() || scope_notes.contains(p.note_id.as_str());

        let mut whole = 0usize;
        let mut ordered: Vec<(Passage, bool)> = Vec::new();
        for p in plain {
            let must = p.source == PassageSource::Transcript && whole < MUST_INCLUDE;
            if must {
                whole += 1;
            }
            ordered.push((p, must));
        }
        let have: HashSet<_> = ordered.iter().map(|(p, _)| key(p)).collect();
        // Extras rank after the plain top passages, before its tail.
        let tail_at = ordered
            .iter()
            .rposition(|(_, must)| *must)
            .map_or(0, |i| i + 1);
        let extras: Vec<(Passage, bool)> = extra
            .into_iter()
            .filter(|p| !have.contains(&key(p)))
            .map(|p| (p, false))
            .collect();
        ordered.splice(tail_at..tail_at, extras);
        stats.passages_found = ordered.len() as u32;

        for (p, must) in ordered {
            match p.source {
                PassageSource::Transcript => {
                    let Some(mid) = p.meeting_id.clone() else {
                        continue;
                    };
                    // Defense in depth: never show a meeting outside the
                    // resolved (visibility-checked) scope.
                    if !r.allows_meeting(&mid) {
                        continue;
                    }
                    let lines = if must {
                        (0..p.segments.len()).collect()
                    } else {
                        clip(&p.segments, &terms, b.passage_clip_chars)
                    };
                    excerpts.push(Excerpt {
                        meeting_id: mid,
                        title: p.title.clone(),
                        started_at: p.started_at,
                        segments: p.segments,
                        lines,
                        must,
                    });
                }
                PassageSource::Note if note_allowed(&p) => note_passages.push(p),
                PassageSource::Note => {}
            }
        }
    }

    // Fit to the budget in rank order (the plain top passages always fit:
    // they're what retrieval alone would have shown).
    let mut used = 0usize;
    let mut kept: Vec<Excerpt> = Vec::new();
    for e in excerpts {
        let c = excerpt_chars(&e);
        if !e.must && used + c > b.context_chars && !kept.is_empty() {
            continue;
        }
        used += c;
        kept.push(e);
    }
    stats.transcript_passages = kept.len() as u32;

    // Same-meeting excerpts read in transcript order.
    let mut by_meeting: Vec<(String, Vec<Excerpt>)> = Vec::new();
    for e in kept {
        match by_meeting.iter_mut().find(|(m, _)| *m == e.meeting_id) {
            Some((_, v)) => v.push(e),
            None => by_meeting.push((e.meeting_id.clone(), vec![e])),
        }
    }

    // Best-ranked meeting last, next to the question (and furthest from a
    // server that trims an oversized prompt from the front).
    by_meeting.reverse();
    let mut transcript_text = String::new();
    let mut shown: Vec<SegmentInfo> = Vec::new();
    for (_, mut group) in by_meeting {
        group.sort_by_key(|e| {
            e.lines
                .first()
                .map(|&i| e.segments[i].start_ms)
                .unwrap_or(0)
        });
        let first = &group[0];
        transcript_text.push_str(&format!(
            "## {}{}\n",
            evidence::safe(&first.title),
            first
                .started_at
                .map(|d| format!(" — {}", d.format("%Y-%m-%d")))
                .unwrap_or_default()
        ));
        let mut last_end: Option<u64> = None;
        for e in &group {
            for &i in &e.lines {
                let seg = &e.segments[i];
                if shown.iter().any(|x| x.id == seg.id) {
                    continue;
                }
                if last_end.is_some_and(|end| seg.start_ms > end + 30_000) {
                    transcript_text.push_str("…\n");
                }
                transcript_text.push_str(&evidence::segment_line(seg));
                transcript_text.push('\n');
                last_end = Some(seg.end_ms);
                shown.push(evidence::info(seg, &e.meeting_id, &e.title, e.started_at));
            }
        }
        transcript_text.push('\n');
    }

    // --- memory: a person or a recurring meeting (deterministic) ----------
    let mut memory_text = String::new();
    if b.memory_chars > 0 {
        let opts = MemoryPrefetch {
            meetings: (r.kind != crate::scope::ScopeKind::All)
                .then(|| r.meetings.iter().map(|m| m.id.clone()).collect()),
            people: input.hints.people.clone(),
            since: input.hints.since,
            until: input.hints.until,
            max_facts: 0,
            max_meetings: 0,
        };
        if let Some(mut brief) = ctx.memory_prefetch(input.question, &opts)? {
            brief.retain_meetings(|m| r.allows_meeting(m));
            if !brief.facts.is_empty() {
                let rendered = fly_context::render::briefing_rendered(&brief, b.memory_chars);
                for c in rendered.shown {
                    if !shown.iter().any(|x| x.id == c.segment.id) {
                        shown.push(evidence::info(
                            &c.segment,
                            &c.meeting_id,
                            &c.meeting_title,
                            Some(c.started_at),
                        ));
                    }
                }
                memory_text = rendered.text;
                stats.memory_facts = brief.facts.len() as u32;
            }
        }
    }

    // --- extracted items -------------------------------------------------
    let mut items_text = String::new();
    let narrow = !r.wide;
    // A briefing already lists the facts (with their source lines).
    if memory_text.is_empty() && (input.hints.items.is_some() || narrow) {
        let intent = input.hints.items;
        // Items of the scope's meetings (a folder too), when that fits one
        // query; otherwise everything visible, filtered to the scope below.
        let scope_meetings: Vec<String> = r.meetings.iter().map(|m| m.id.clone()).collect();
        let item_scope = (r.kind != crate::scope::ScopeKind::All)
            .then_some(scope_meetings)
            .filter(|ids| !ids.is_empty() && ids.len() <= fly_context::tools::MAX_MEETING_IDS);
        let owner = intent
            .and(input.hints.people.first().cloned())
            .filter(|_| intent.is_some());
        let args = GetItemsArgs {
            meeting_ids: item_scope.clone(),
            kind: intent.and_then(|i| i.kind).map(item_kind_str),
            owner: owner.clone(),
            status: intent.filter(|i| i.open_only).map(|_| "open".to_string()),
            since: input.hints.since.map(|d| d.to_rfc3339()),
            limit: Some(ITEM_FETCH),
        };
        let mut list = if r.meeting_ids.as_ref().is_some_and(|v| v.is_empty()) {
            Vec::new()
        } else {
            ctx.get_items(&args)?.items
        };
        if list.is_empty() && (owner.is_some() || args.since.is_some()) {
            stats.items_fallback = true;
            list = ctx
                .get_items(&GetItemsArgs {
                    owner: None,
                    since: None,
                    ..args.clone()
                })?
                .items;
        }
        let titles = crate::scope::titles(r);
        let mut seg_cache: HashMap<String, HashMap<String, SegmentView>> = HashMap::new();
        let mut used_items = 0usize;
        for item in list {
            if !r.allows_meeting(&item.meeting_id) {
                continue;
            }
            let title = titles.get(&item.meeting_id).cloned().unwrap_or_default();
            let segs = seg_cache
                .entry(item.meeting_id.clone())
                .or_insert_with(|| segments_of(s, &item.meeting_id));
            // An item is a paraphrase: its source lines go under it (unless
            // already shown above), so a citation of them is checked against
            // words the model actually read.
            let mut block = evidence::render_item(&item, &title);
            block.push('\n');
            let mut sources = Vec::new();
            for id in &item.segment_ids {
                if let Some(seg) = segs.get(id) {
                    if !shown.iter().any(|x| x.id == *id)
                        && !sources.iter().any(|x: &&SegmentView| x.id == *id)
                    {
                        block.push_str("  ");
                        block.push_str(&evidence::segment_line(seg));
                        block.push('\n');
                        sources.push(seg);
                    }
                }
            }
            if used_items + block.len() > b.item_chars {
                break;
            }
            used_items += block.len();
            register_item(&item, segs, &title, r, &mut shown);
            items_text.push_str(&block);
            stats.items += 1;
        }
    }

    // --- the user's own notes -------------------------------------------
    let mut notes_text = String::new();
    if let Some(note_id) = &r.note_id {
        if let Ok(n) = ctx.get_note(&GetNoteArgs {
            note_id: Some(note_id.clone()),
            meeting_id: None,
            kind: Some(NoteKind::Scratchpad),
        }) {
            let pad = n.scratchpad.unwrap_or_default();
            let pad = pad.trim();
            if !pad.is_empty() {
                notes_text.push_str(&evidence::safe(&truncate(pad, b.note_chars)));
                notes_text.push('\n');
                stats.notes_included = true;
            }
        }
    }
    let mut note_used = 0usize;
    for p in note_passages.into_iter().take(b.max_note_passages) {
        if Some(&p.note_id) == r.note_id.as_ref() && stats.notes_included {
            continue;
        }
        let t = truncate(p.text.trim(), b.passage_clip_chars);
        if used + note_used + t.len() > b.context_chars + b.note_chars {
            break;
        }
        note_used += t.len();
        notes_text.push_str(&format!(
            "## {}\n{}\n",
            evidence::safe(&p.title),
            evidence::safe(&t)
        ));
        stats.note_passages += 1;
    }

    // --- assemble ----------------------------------------------------------
    let mut context = String::new();
    if !transcript_text.trim().is_empty() {
        context.push_str("<meeting_transcript>\n");
        context.push_str(&transcript_text);
        context.push_str("</meeting_transcript>\n");
    }
    if !memory_text.is_empty() {
        context.push_str("\n<meeting_metadata>\n");
        context.push_str(&memory_text);
        context.push_str("</meeting_metadata>\n");
    }
    if !items_text.is_empty() {
        context.push_str(
            "\n<meeting_metadata>\nExtracted from these meetings (each cites its source lines):\n",
        );
        context.push_str(&items_text);
        context.push_str("</meeting_metadata>\n");
    }
    if !notes_text.is_empty() {
        context.push_str("\n<user_notes>\nThe user's own notes (no line ids; not citable):\n");
        context.push_str(&notes_text);
        context.push_str("</user_notes>\n");
    }

    // Exactly what was shown counts as retrieved: marker and whole text.
    let mut ev = Evidence::default();
    ev.add_shown(&context, shown);
    stats.segments = ev.len() as u32;
    stats.context_chars = context.len() as u32;
    Ok(Prefetch {
        coverage: coverage(&floor_terms, &context.to_lowercase()),
        terms: floor_terms.len(),
        context,
        evidence: ev,
        stats,
    })
}

fn register_item(
    item: &MeetingItem,
    segs: &HashMap<String, SegmentView>,
    title: &str,
    r: &Resolved,
    shown: &mut Vec<SegmentInfo>,
) {
    let started = r.meeting(&item.meeting_id).map(|m| m.started_at);
    for id in &item.segment_ids {
        if let Some(seg) = segs.get(id) {
            if !shown.iter().any(|x| x.id == *id) {
                shown.push(evidence::info(seg, &item.meeting_id, title, started));
            }
        }
    }
}

/// At most `max` chars on a char boundary, marked when cut.
pub fn truncate(s: &str, max: usize) -> String {
    if s.len() <= max {
        return s.to_string();
    }
    let mut end = max;
    while !s.is_char_boundary(end) {
        end -= 1;
    }
    format!("{} …", s[..end].trim_end())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn seg(i: usize, text: &str) -> SegmentView {
        SegmentView {
            id: format!("s{i}"),
            speaker: "Dana".into(),
            speaker_key: "spk_0".into(),
            start_ms: i as u64 * 1000,
            end_ms: i as u64 * 1000 + 900,
            text: text.into(),
        }
    }

    #[test]
    fn clip_centers_on_the_best_line() {
        let segs: Vec<SegmentView> = (0..10)
            .map(|i| {
                seg(
                    i,
                    if i == 6 {
                        "the launch budget is ninety two hundred dollars"
                    } else {
                        "we talked about several other things for a while here"
                    },
                )
            })
            .collect();
        let lines = clip(&segs, &query_terms("launch budget"), 250);
        assert!(lines.contains(&6), "{lines:?}");
        assert!(lines.len() < 10);
        assert!(lines.windows(2).all(|w| w[1] == w[0] + 1), "contiguous");
        assert_eq!(clip(&segs, &[], 100_000).len(), 10, "fits whole");
    }

    #[test]
    fn salient_terms_drop_question_filler() {
        assert_eq!(
            salient_terms("What did we decide about the Berlin office lease?"),
            vec!["berlin", "office", "lease"]
        );
        assert!(
            (coverage(&salient_terms("Berlin office lease"), "the office moved") - 1.0 / 3.0).abs()
                < 1e-9
        );
        assert_eq!(coverage(&[], "x"), 1.0);
        assert_eq!(coverage(&["capped".to_string()], "let's cap it"), 1.0);
        assert_eq!(coverage(&["budgets".to_string()], "the budget"), 1.0);
        assert_eq!(coverage(&["lease".to_string()], "the release"), 0.0);
    }
}
