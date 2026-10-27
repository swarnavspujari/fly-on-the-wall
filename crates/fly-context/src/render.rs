//! Compact text renderings of tool results for a model. Transcript lines
//! are always `[seg:ID] Speaker (m:ss): text` so the model can cite ids the
//! harness can validate; everything else is short labelled lines.

use std::fmt::Write;

use crate::tools::{
    EventInfo, ItemList, MeetingList, NoteView, PassageResults, PassageSource, SegmentView,
    SegmentWindow,
};

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

fn excluded_note(out: &mut String, n: usize) {
    if n > 0 {
        let _ = writeln!(
            out,
            "({n} Keep-local meeting{} not searched.)",
            if n == 1 { " was" } else { "s were" }
        );
    }
}

pub fn passages(r: &PassageResults) -> String {
    let mut out = String::new();
    if r.passages.is_empty() {
        out.push_str("No matching passages found in the user's meetings.\n");
        excluded_note(&mut out, r.keep_local_excluded);
        return out;
    }
    let _ = writeln!(out, "{} passage(s):", r.passages.len());
    excluded_note(&mut out, r.keep_local_excluded);
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
    excluded_note(&mut out, l.keep_local_excluded);
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
