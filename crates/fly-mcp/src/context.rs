//! The MCP server's text for the context layer. The logic — series
//! detection, the threaded briefing, open items — lives in
//! `fly_context::memory`, shared with Ask; this module only renders its
//! results in the wording MCP clients already parse (pinned by
//! `tests/context_golden.rs`). No LLM runs at query time, and every claim
//! carries provenance (meeting_id + source segment ids).

use chrono::{DateTime, Utc};
use fly_context::{Briefing, MemoryFact, OpenItems};
use fly_core::ItemKind;

pub use fly_context::memory::{detect_series, normalize_title, series_matching, Series};

fn fmt_date(t: DateTime<Utc>) -> String {
    t.format("%Y-%m-%d").to_string()
}

fn item_line(f: &MemoryFact) -> String {
    let mut line = format!("- {} ", f.text);
    let mut meta = Vec::new();
    if let Some(owner) = &f.owner {
        meta.push(format!("owner: {owner}"));
    }
    if let Some(status) = &f.status {
        meta.push(format!("status: {status}"));
    }
    if let Some(by) = &f.said_by {
        meta.push(format!("said by: {by}"));
    }
    meta.push(fmt_date(f.started_at));
    meta.push(format!("meeting: {}", f.meeting_id));
    if !f.segment_ids.is_empty() {
        meta.push(format!("segments: {}", f.segment_ids.join(",")));
    }
    line.push_str(&format!("({})", meta.join(" · ")));
    line
}

fn section(out: &mut String, heading: &str, lines: Vec<String>) {
    if lines.is_empty() {
        return;
    }
    out.push_str(&format!("\n## {heading}\n"));
    for l in lines {
        out.push_str(&l);
        out.push('\n');
    }
}

/// The "no meetings" reply shared by `get_context` and `whats_changed`.
fn no_meetings(query: &str, since: Option<DateTime<Utc>>) -> String {
    format!(
        "No meetings matched \"{query}\"{}. Try search_notes for a broader text search.",
        since
            .map(|s| format!(" since {}", fmt_date(s)))
            .unwrap_or_default()
    )
}

/// The `get_context` / `whats_changed` briefing: participants, timeline,
/// then every extracted item grouped by kind, chronologically, each line
/// carrying provenance.
pub fn render_briefing(b: &Briefing, query: &str, since: Option<DateTime<Utc>>) -> String {
    if b.meetings.is_empty() {
        return no_meetings(query, since);
    }
    let mut out = String::new();
    out.push_str(&format!("# Context: {}\n", b.series.join(", ")));
    out.push_str(&format!(
        "{} meeting(s), {} → {}\n",
        b.meetings.len(),
        fmt_date(b.meetings[0].started_at),
        fmt_date(b.meetings[b.meetings.len() - 1].started_at)
    ));
    if !b.participants.is_empty() {
        out.push_str(&format!("Participants: {}\n", b.participants.join(", ")));
    }

    out.push_str("\n## Timeline\n");
    for m in &b.meetings {
        out.push_str(&format!(
            "- {} — {} (meeting: {}{}, {} extracted item(s))\n",
            fmt_date(m.started_at),
            m.title,
            m.meeting_id,
            if m.has_transcript {
                ", transcript ✓"
            } else {
                ", no transcript"
            },
            m.item_count,
        ));
    }

    let lines_of = |kind: ItemKind| -> Vec<String> {
        b.facts
            .iter()
            .filter(|f| f.kind == kind)
            .map(item_line)
            .collect()
    };
    section(&mut out, "Decisions", lines_of(ItemKind::Decision));
    section(&mut out, "Action items", lines_of(ItemKind::ActionItem));
    section(&mut out, "Commitments", lines_of(ItemKind::Commitment));
    section(&mut out, "Open questions", lines_of(ItemKind::Question));
    section(&mut out, "Key figures", lines_of(ItemKind::Figure));
    if b.facts.is_empty() {
        out.push_str(
            "\n(No extracted items yet for these meetings — run item extraction in the app, \
             or read the transcripts directly with get_transcripts.)\n",
        );
    }

    out.push_str(
        "\nEvery line above cites its meeting and transcript segment ids — verify any claim \
         with get_transcript(meeting_id) before relying on it.\n",
    );
    out
}

/// `whats_changed`: a series miss names the series; otherwise the briefing.
pub fn render_whats_changed(b: &Briefing, series: &str, since: DateTime<Utc>) -> String {
    if b.series.is_empty() {
        return format!("No series matches \"{series}\". get_context(query) lists series names.");
    }
    render_briefing(b, series, Some(since))
}

/// One item with its kind, owner, status, date and provenance (the
/// `query_items` / `open_items` line).
pub fn render_item_line(
    text: &str,
    kind: ItemKind,
    owner: Option<&str>,
    status: Option<&str>,
    date: String,
    meeting_id: &str,
    segment_ids: &[String],
) -> String {
    let mut meta = vec![format!("kind: {}", kind.as_str())];
    if let Some(o) = owner {
        meta.push(format!("owner: {o}"));
    }
    if let Some(s) = status {
        meta.push(format!("status: {s}"));
    }
    meta.push(date);
    meta.push(format!("meeting: {meeting_id}"));
    if !segment_ids.is_empty() {
        meta.push(format!("segments: {}", segment_ids.join(",")));
    }
    format!("- {text} ({})\n", meta.join(" · "))
}

/// `open_items`.
pub fn render_open_items(o: &OpenItems) -> String {
    let scope = &o.scope;
    if !o.scope_found {
        return format!(
            "No series matches \"{scope}\" — pass \"all\" or a series name from get_context."
        );
    }
    if o.facts.is_empty() {
        return format!("No open action items or commitments in scope \"{scope}\".");
    }
    let mut lines = String::new();
    for f in &o.facts {
        lines.push_str(&render_item_line(
            &f.text,
            f.kind,
            f.owner.as_deref(),
            f.status.as_deref(),
            fmt_date(f.started_at),
            &f.meeting_id,
            &f.segment_ids,
        ));
    }
    format!(
        "{} open item(s) in scope \"{scope}\" (newest meeting first; status is as stated \
         in that meeting — verify against later meetings before chasing anyone):\n{}",
        o.facts.len(),
        lines
    )
}
