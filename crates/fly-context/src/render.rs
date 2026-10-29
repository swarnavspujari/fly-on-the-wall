//! Compact text renderings of tool results for a model. Transcript lines
//! are always `[seg:ID] Speaker (m:ss): text` so the model can cite ids the
//! harness can validate; everything else is short labelled lines.

use std::fmt::Write;

use crate::fanout::Fanout;
use crate::memory::{Briefing, CitedSegment, MatchedBy, MemoryFact, OpenItems};
use crate::tools::{
    EventInfo, ItemList, MeetingList, NoteView, PassageResults, PassageSource, SegmentView,
    SegmentWindow,
};
use fly_core::ItemKind;

/// `m:ss` or `h:mm:ss`.
pub fn clock(ms: u64) -> String {
    let s = ms / 1000;
    let (h, m, s) = (s / 3600, (s % 3600) / 60, s % 60);
    if h > 0 {
        format!("{h}:{m:02}:{s:02}")
    } else {
        format!("{m}:{s:02}")
    }
}

/// One citable transcript line.
pub fn segment_line(s: &SegmentView) -> String {
    format!(
        "[seg:{}] {} ({}): {}",
        s.id,
        s.speaker,
        clock(s.start_ms),
        s.text
    )
}

pub fn passages(r: &PassageResults) -> String {
    let mut out = String::new();
    if r.passages.is_empty() {
        out.push_str("No matching passages found in the user's meetings.\n");
        return out;
    }
    let _ = writeln!(out, "{} passage(s):", r.passages.len());
    for p in &r.passages {
        out.push('\n');
        let date = p
            .started_at
            .map(|d| format!(" — {}", d.format("%Y-%m-%d")))
            .unwrap_or_default();
        match p.source {
            PassageSource::Transcript => {
                let _ = writeln!(
                    out,
                    "## {}{} (meeting {})",
                    p.title,
                    date,
                    p.meeting_id.as_deref().unwrap_or("?")
                );
                for s in &p.segments {
                    let _ = writeln!(out, "{}", segment_line(s));
                }
            }
            PassageSource::Note => {
                let _ = writeln!(out, "## Notes: {} (note {})", p.title, p.note_id);
                let _ = writeln!(out, "{}", p.text);
            }
        }
    }
    out
}

pub fn segments(w: &SegmentWindow) -> String {
    let mut out = format!(
        "{} (meeting {}){}:\n",
        w.title,
        w.meeting_id,
        if w.polished { ", cleaned-up text" } else { "" }
    );
    if w.more_before {
        out.push_str("…\n");
    }
    for s in &w.segments {
        let _ = writeln!(out, "{}", segment_line(s));
    }
    if w.more_after {
        out.push_str("…\n");
    }
    out
}

pub fn meetings(l: &MeetingList) -> String {
    let mut out = String::new();
    if l.meetings.is_empty() {
        out.push_str("No meetings match.\n");
    }
    for m in &l.meetings {
        let who = if m.attendees.is_empty() {
            String::new()
        } else {
            format!(" — with {}", m.attendees.join(", "))
        };
        let _ = writeln!(
            out,
            "- {} {} (meeting {}){}",
            m.started_at.format("%Y-%m-%d %H:%M"),
            m.title,
            m.meeting_id,
            who
        );
    }
    out
}

pub fn event(e: &EventInfo) -> String {
    let mut out = format!("{} (meeting {})\n", e.title, e.meeting_id);
    let _ = writeln!(
        out,
        "Started: {}",
        e.started_at.format("%Y-%m-%d %H:%M UTC")
    );
    if let Some(d) = e.duration_ms {
        let _ = writeln!(out, "Length: {}", clock(d));
    }
    if !e.attendees.is_empty() {
        let names: Vec<&str> = e.attendees.iter().map(|a| a.name.as_str()).collect();
        let _ = writeln!(
            out,
            "Invited: {}{}",
            names.join(", "),
            if e.attendees_confirmed {
                ""
            } else {
                " (from the calendar, unconfirmed)"
            }
        );
    }
    if !e.speakers.is_empty() {
        let labels: Vec<&str> = e.speakers.iter().map(|s| s.label.as_str()).collect();
        let _ = writeln!(out, "Speakers: {}", labels.join(", "));
    }
    let _ = writeln!(
        out,
        "Transcript: {} segments{}",
        e.segment_count,
        if e.has_polished_transcript {
            " (cleaned-up version available)"
        } else {
            ""
        }
    );
    let mut counts: Vec<(&String, &usize)> = e.item_counts.iter().collect();
    counts.sort();
    if !counts.is_empty() {
        let parts: Vec<String> = counts.iter().map(|(k, n)| format!("{n} {k}")).collect();
        let _ = writeln!(out, "Extracted: {}", parts.join(", "));
    }
    out
}

pub fn note(n: &NoteView) -> String {
    let mut out = format!("{} (note {})\n", n.title, n.note_id);
    if let Some(s) = &n.scratchpad {
        out.push_str("\n### The user's own notes\n");
        out.push_str(if s.trim().is_empty() {
            "(empty)"
        } else {
            s.trim()
        });
        out.push('\n');
    }
    if let Some(blocks) = &n.enhanced {
        out.push_str("\n### Enhanced notes\n");
        if blocks.is_empty() {
            out.push_str("(none yet)\n");
        }
        for b in blocks {
            let cites: String = b
                .source_segment_ids
                .iter()
                .map(|id| format!(" [seg:{id}]"))
                .collect();
            let _ = writeln!(out, "{}{}", b.markdown.trim(), cites);
        }
    }
    out
}

pub fn items(l: &ItemList) -> String {
    let mut out = String::new();
    if l.items.is_empty() {
        out.push_str("No items match.\n");
    }
    for i in &l.items {
        let owner = i
            .owner
            .as_deref()
            .map(|o| format!(" — owner: {o}"))
            .unwrap_or_default();
        let status = i
            .status
            .as_deref()
            .map(|s| format!(" [{s}]"))
            .unwrap_or_default();
        let cites: String = i
            .segment_ids
            .iter()
            .map(|id| format!(" [seg:{id}]"))
            .collect();
        let _ = writeln!(
            out,
            "- {}: {}{}{} (meeting {}){}",
            i.kind.as_str(),
            i.text,
            owner,
            status,
            i.meeting_id,
            cites
        );
    }
    out
}

// ---------------------------------------------------------------------------
// Memory and fan-out (meeting content goes through `safe`)
// ---------------------------------------------------------------------------

/// Meeting-derived text for a model: fence tags defused and no `[seg:` a
/// model could take for a citation (same rule as the Ask harness).
pub fn safe(text: &str) -> String {
    fly_core::enhance::fence_safe(text)
        .replace("[seg:", "[seg-")
        .replace("[SEG:", "[SEG-")
}

fn day(t: chrono::DateTime<chrono::Utc>) -> String {
    t.format("%Y-%m-%d").to_string()
}

const KIND_SECTIONS: [(ItemKind, &str); 5] = [
    (ItemKind::Decision, "Decisions"),
    (ItemKind::ActionItem, "Action items"),
    (ItemKind::Commitment, "Commitments"),
    (ItemKind::Question, "Open questions"),
    (ItemKind::Figure, "Key figures"),
];

/// Longest source quote shown under a fact, in chars (a transcript segment
/// is usually shorter; longer ones are cut at a word boundary).
const QUOTE_CHARS: usize = 400;
/// Source quotes (and so citations) per fact.
const QUOTES_PER_FACT: usize = 2;

/// Model-facing text plus exactly the cited segments it shows, each with
/// the text it shows (a clipped quote's visible part only). Register
/// `shown` as the evidence for `text`, not [`Briefing::evidence`]: a model
/// can cite only what it saw, and claims are checked against what it saw.
#[derive(Debug, Clone, Default, PartialEq, serde::Serialize)]
pub struct Rendered {
    pub text: String,
    pub shown: Vec<CitedSegment>,
}

impl Rendered {
    fn show(&mut self, c: CitedSegment) {
        if !self
            .shown
            .iter()
            .any(|s| s.meeting_id == c.meeting_id && s.segment.id == c.segment.id)
        {
            self.shown.push(c);
        }
    }
}

/// `text` cut to about `max` chars at a word boundary: (the visible part,
/// whether it was cut).
fn clip(text: &str, max: usize) -> (String, bool) {
    let text = text.trim();
    if text.chars().count() <= max {
        return (text.to_string(), false);
    }
    let cut: String = text.chars().take(max).collect();
    let cut = match cut.rfind(char::is_whitespace) {
        Some(i) if i > max / 2 => &cut[..i],
        _ => cut.as_str(),
    };
    (cut.trim_end().to_string(), true)
}

/// A fact and the words it came from: the fact line, then up to
/// [`QUOTES_PER_FACT`] of its cited segments verbatim. Only the quoted
/// segments get a `[seg:ID]` marker, and they're returned (with their
/// visible text) as the evidence this block shows. Facts are extraction
/// paraphrases, so an answer should take numbers, names and quotes from
/// the quoted lines (the Ask claim check compares against the segments).
/// `None` when no cited segment still exists: the fact can't be cited, so
/// it isn't shown.
fn fact_block<'b>(
    f: &MemoryFact,
    cited: impl Fn(&str, &str) -> Option<&'b CitedSegment>,
    with_kind: bool,
) -> Option<(String, Vec<CitedSegment>)> {
    let mut segs: Vec<&CitedSegment> = Vec::new();
    for id in &f.segment_ids {
        if let Some(c) = cited(&f.meeting_id, id) {
            if !segs.iter().any(|s| s.segment.id == c.segment.id) {
                segs.push(c);
            }
        }
    }
    segs.truncate(QUOTES_PER_FACT);
    if segs.is_empty() {
        return None;
    }
    let cites: String = segs
        .iter()
        .map(|c| format!(" [seg:{}]", c.segment.id))
        .collect();
    let mut block = fact_line(f, &cites, with_kind);
    let mut shown = Vec::with_capacity(segs.len());
    for c in segs {
        let (visible, cut) = clip(&c.segment.text, QUOTE_CHARS);
        let _ = write!(
            block,
            "\n  > {}: \"{}{}\"",
            safe(&c.segment.speaker),
            safe(&visible),
            if cut { " …" } else { "" }
        );
        let mut seen = c.clone();
        seen.segment.text = visible;
        shown.push(seen);
    }
    Some((block, shown))
}

fn fact_line(f: &MemoryFact, cites: &str, with_kind: bool) -> String {
    let mut line = String::from("- ");
    if with_kind {
        line.push_str(f.kind.as_str());
        line.push_str(": ");
    }
    line.push_str(&safe(f.text.trim()));
    if let Some(o) = &f.owner {
        let _ = write!(line, " — owner: {}", safe(o));
    }
    // Only the statuses extraction writes: a healed or imported row can't
    // put arbitrary text (or a marker) in brackets.
    if let Some(s) = f
        .status
        .as_deref()
        .filter(|s| matches!(*s, "open" | "done"))
    {
        let _ = write!(line, " [{s}]");
    }
    if let Some(by) = &f.said_by {
        if f.owner
            .as_deref()
            .is_none_or(|o| !o.eq_ignore_ascii_case(by))
        {
            let _ = write!(line, " — said by {}", safe(by));
        }
    }
    let _ = write!(
        line,
        " ({}, {}){}",
        day(f.started_at),
        safe(&f.meeting_title),
        cites
    );
    line
}

/// A briefing for a model, all of it.
pub fn briefing(b: &Briefing) -> String {
    briefing_rendered(b, usize::MAX).text
}

/// [`briefing_rendered`]'s text.
pub fn briefing_within(b: &Briefing, max_chars: usize) -> String {
    briefing_rendered(b, max_chars).text
}

/// A briefing for a model in about `max_chars`: the header, the newest
/// meetings, then as many facts as fit, newest first (shown oldest first,
/// grouped by kind), each with its quoted source lines. Facts whose cited
/// segments no longer exist are left out: they can't be cited. `shown` is
/// exactly the cited segments in the text.
pub fn briefing_rendered(b: &Briefing, max_chars: usize) -> Rendered {
    let mut r = Rendered::default();
    let out = &mut r.text;
    if b.meetings.is_empty() {
        let since = b
            .since
            .map(|s| format!(" since {}", day(s)))
            .unwrap_or_default();
        if !b.series.is_empty() {
            let _ = writeln!(
                out,
                "No meetings of \"{}\"{since}.",
                safe(&b.series.join(", "))
            );
        } else if let (MatchedBy::Person, Some(p)) = (b.matched_by, &b.person) {
            let _ = writeln!(out, "No meetings with {}{since}.", safe(p));
        } else {
            let _ = writeln!(
                out,
                "No recurring meeting, project or person in the user's meetings matches \"{}\".",
                safe(&b.query)
            );
        }
        return r;
    }
    let first = &b.meetings[0];
    let last = &b.meetings[b.meetings.len() - 1];
    let subject = match b.matched_by {
        MatchedBy::Person => b.person.as_deref().map(safe).unwrap_or_default(),
        _ => format!("\"{}\" (recurring meeting)", safe(&b.series.join("\", \""))),
    };
    let _ = writeln!(
        out,
        "Memory: {subject} — {} meeting(s), {} → {}{}",
        b.meetings.len(),
        day(first.started_at),
        day(last.started_at),
        b.since
            .map(|s| format!(", since {}", day(s)))
            .unwrap_or_default()
    );
    if b.matched_by != MatchedBy::Person {
        if let Some(p) = &b.person {
            let _ = writeln!(out, "Only what {} owns or said.", safe(p));
        }
    }
    if !b.participants.is_empty() {
        let names: Vec<String> = b.participants.iter().map(|p| safe(p)).collect();
        let _ = writeln!(out, "Participants: {}", names.join(", "));
    }
    out.push_str("Meetings:\n");
    let shown = b.meetings.len().min(8);
    if shown < b.meetings.len() {
        let _ = writeln!(out, "- … {} earlier meeting(s)", b.meetings.len() - shown);
    }
    for m in &b.meetings[b.meetings.len() - shown..] {
        let _ = writeln!(
            out,
            "- {} {} (meeting {}){}",
            day(m.started_at),
            safe(&m.title),
            m.meeting_id,
            if m.has_transcript {
                ""
            } else {
                ", no transcript yet"
            }
        );
    }

    // Facts that can be cited, newest first, while they fit.
    let lines: Vec<Option<(String, Vec<CitedSegment>)>> = b
        .facts
        .iter()
        .map(|f| fact_block(f, |m, id| b.cited(m, id), false))
        .collect();
    // (no literal marker here: a model could copy it as a citation)
    let footer = "Cite an item by the segment markers after it, and take numbers, names and \
                  quotes from the quoted source lines (items are paraphrased). Each item's \
                  status is as stated in its meeting.\n";
    // Section headings and the "not shown" line take a little room too.
    let mut budget = max_chars.saturating_sub(out.len() + footer.len() + 80);
    let mut keep = vec![false; lines.len()];
    let mut kept = 0;
    for i in (0..lines.len()).rev() {
        if let Some((l, _)) = &lines[i] {
            // The newest citable fact always shows: a briefing with facts
            // never reads like one without.
            if l.len() + 1 > budget && kept > 0 {
                break;
            }
            budget = budget.saturating_sub(l.len() + 1);
            keep[i] = true;
            kept += 1;
        }
    }
    let mut any = false;
    let mut shown: Vec<CitedSegment> = Vec::new();
    for (kind, heading) in KIND_SECTIONS {
        let mut section = String::new();
        for (i, f) in b.facts.iter().enumerate() {
            if let (true, Some((l, segs))) = (keep[i] && f.kind == kind, &lines[i]) {
                section.push_str(l);
                section.push('\n');
                shown.extend(segs.iter().cloned());
            }
        }
        if !section.is_empty() {
            let _ = write!(out, "{heading}:\n{section}");
            any = true;
        }
    }
    // Never claim "none" when items exist but weren't shown.
    let sourced = lines.iter().filter(|l| l.is_some()).count();
    let unsourced = lines.len() - sourced;
    let not_shown = b.facts_omitted + sourced - keep.iter().filter(|k| **k).count();
    if !any && unsourced == 0 && not_shown == 0 {
        out.push_str("No extracted decisions, action items or other facts yet.\n");
    }
    if unsourced > 0 {
        let _ = writeln!(
            out,
            "({unsourced} item(s) not shown: they have no transcript line left to cite.)"
        );
    }
    if not_shown > 0 {
        let _ = writeln!(out, "({not_shown} older item(s) not shown.)");
    }
    out.push_str(footer);
    for c in shown {
        r.show(c);
    }
    r
}

/// Open items for a model, all of them.
pub fn open_items(o: &OpenItems) -> String {
    open_items_rendered(o, usize::MAX).text
}

/// Open items for a model in about `max_chars`, newest first (the newest
/// citable one always shows), each with its quoted source lines. `shown`
/// is exactly the cited segments in the text.
pub fn open_items_rendered(o: &OpenItems, max_chars: usize) -> Rendered {
    let mut r = Rendered::default();
    let out = &mut r.text;
    if !o.scope_found {
        let _ = writeln!(
            out,
            "No recurring meeting matches \"{}\". Use \"all\" or a name from get_briefing.",
            safe(&o.scope)
        );
        return r;
    }
    let who = o
        .owner
        .as_deref()
        .map(|w| format!(" owned by {}", safe(w)))
        .unwrap_or_default();
    let scope = if o.scope == "all" {
        String::new()
    } else {
        format!(" in \"{}\"", safe(&o.scope))
    };
    let lines: Vec<(String, Vec<CitedSegment>)> = o
        .facts
        .iter()
        .filter_map(|f| fact_block(f, |m, id| o.cited(m, id), true))
        .collect();
    // Items whose transcript lines are gone can't be cited, but they are
    // still open: say so rather than claim there are none.
    let unsourced = o.facts.len() - lines.len();
    if o.facts.is_empty() {
        let _ = writeln!(out, "No open action items or commitments{who}{scope}.");
        return r;
    }
    let _ = writeln!(
        out,
        "{} open item(s){who}{scope}, newest meeting first (status as stated in that meeting; \
         a later meeting may have closed it):",
        o.facts.len()
    );
    let mut budget = max_chars.saturating_sub(out.len() + 120);
    let mut shown: Vec<CitedSegment> = Vec::new();
    let mut over = 0;
    for (i, (l, segs)) in lines.iter().enumerate() {
        if i > 0 && l.len() + 1 > budget {
            over = lines.len() - i;
            break;
        }
        budget = budget.saturating_sub(l.len() + 1);
        out.push_str(l);
        out.push('\n');
        shown.extend(segs.iter().cloned());
    }
    if over > 0 {
        let _ = writeln!(out, "({over} older open item(s) not shown.)");
    }
    if unsourced > 0 {
        let _ = writeln!(
            out,
            "({unsourced} more open item(s) not shown: they have no transcript line left to \
             cite.)"
        );
    }
    for c in shown {
        r.show(c);
    }
    r
}

/// Fan-out results for a model: the queries run, then the merged passages
/// (transcript lines with `[seg:ID]`), each saying which queries found it.
pub fn fanout(f: &Fanout) -> String {
    let mut out = String::new();
    if f.searches_run == 0 {
        out.push_str(
            "Nothing was searched: the search budget for this answer is used up. Answer from \
             what was already found.\n",
        );
        return out;
    }
    let _ = writeln!(
        out,
        "Searched {} {}:",
        f.queries.len(),
        if f.queries.len() == 1 {
            "query"
        } else {
            "queries"
        }
    );
    for (i, q) in f.queries.iter().enumerate() {
        let _ = writeln!(out, "{}. {}", i + 1, safe(q));
    }
    if f.passages.is_empty() {
        out.push_str("No matching passages found in the user's meetings.\n");
        return out;
    }
    let _ = writeln!(out, "{} passage(s):", f.passages.len());
    for fp in &f.passages {
        let p = &fp.passage;
        let found: Vec<String> = fp.found_by.iter().map(|i| (i + 1).to_string()).collect();
        out.push('\n');
        match p.source {
            PassageSource::Transcript => {
                let _ = writeln!(
                    out,
                    "## {}{} (meeting {}; found by query {})",
                    safe(&p.title),
                    p.started_at
                        .map(|d| format!(" — {}", day(d)))
                        .unwrap_or_default(),
                    p.meeting_id.as_deref().unwrap_or("?"),
                    found.join(", ")
                );
                for s in &p.segments {
                    let _ = writeln!(
                        out,
                        "[seg:{}] {} ({}): {}",
                        s.id,
                        safe(&s.speaker),
                        clock(s.start_ms),
                        safe(&s.text)
                    );
                }
            }
            PassageSource::Note => {
                let _ = writeln!(
                    out,
                    "## Notes: {} (note {}; found by query {})",
                    safe(&p.title),
                    p.note_id,
                    found.join(", ")
                );
                let _ = writeln!(out, "{}", safe(p.text.trim()));
            }
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn clock_formats() {
        assert_eq!(clock(0), "0:00");
        assert_eq!(clock(271_000), "4:31");
        assert_eq!(clock(3_723_000), "1:02:03");
    }
}
