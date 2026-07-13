//! Deterministic context assembly: recurring-series detection over meeting
//! titles + participants, and the chronologically threaded briefing that
//! `get_context` / `whats_changed` / `open_items` return. No LLM runs at
//! query time — everything is assembled from stored meetings, transcripts,
//! and the extraction table, and every claim carries provenance
//! (meeting_id + source segment ids).

use std::collections::BTreeMap;

use chrono::{DateTime, Utc};
use fly_core::{ItemKind, Meeting, MeetingItem};
use fly_storage::{ItemFilter, Storage};

/// Normalize a meeting title into its series key: lowercase, digits and
/// punctuation stripped (dates, "#12", "(3/4)" all collapse), whitespace
/// folded. "Weekly Sync 2026-07-01" and "weekly sync #2" → "weekly sync".
pub fn normalize_title(title: &str) -> String {
    let mut out = String::with_capacity(title.len());
    for c in title.to_lowercase().chars() {
        if c.is_alphabetic() {
            out.push(c);
        } else if c.is_whitespace() || c.is_numeric() || c.is_ascii_punctuation() {
            out.push(' ');
        }
    }
    out.split_whitespace().collect::<Vec<_>>().join(" ")
}

/// Attendee-overlap (Jaccard) between two meetings; 1.0 when either side has
/// no attendee data (manual recordings) so the title match alone decides.
fn participant_overlap(a: &Meeting, b: &Meeting) -> f64 {
    if a.attendees.is_empty() || b.attendees.is_empty() {
        return 1.0;
    }
    let sa: std::collections::HashSet<String> =
        a.attendees.iter().map(|s| s.to_lowercase()).collect();
    let sb: std::collections::HashSet<String> =
        b.attendees.iter().map(|s| s.to_lowercase()).collect();
    let inter = sa.intersection(&sb).count() as f64;
    let union = sa.union(&sb).count() as f64;
    if union == 0.0 {
        1.0
    } else {
        inter / union
    }
}

/// A detected recurring series: same normalized title AND sufficient
/// participant overlap with the rest of the group.
pub struct Series {
    pub key: String,
    /// Chronological (oldest → newest).
    pub meetings: Vec<Meeting>,
}

/// Group meetings into series. Deterministic: meetings sort chronologically,
/// grouping is by normalized title, then a meeting only joins the group when
/// it overlaps (≥ 0.34 Jaccard) with the group's first member — enough that
/// "Weekly sync" with a disjoint set of people forms its own thread.
pub fn detect_series(mut meetings: Vec<Meeting>) -> Vec<Series> {
    meetings.sort_by_key(|m| m.started_at);
    let mut groups: BTreeMap<String, Vec<Vec<Meeting>>> = BTreeMap::new();
    for m in meetings {
        let key = normalize_title(&m.title);
        let buckets = groups.entry(key).or_default();
        match buckets
            .iter_mut()
            .find(|b| participant_overlap(&b[0], &m) >= 0.34)
        {
            Some(bucket) => bucket.push(m),
            None => buckets.push(vec![m]),
        }
    }
    let mut out = Vec::new();
    for (key, buckets) in groups {
        for (i, meetings) in buckets.into_iter().enumerate() {
            let key = if i == 0 {
                key.clone()
            } else {
                format!("{key} ({})", i + 1)
            };
            out.push(Series { key, meetings });
        }
    }
    out
}

/// Series whose key or member titles match the query tokens (all tokens must
/// appear in the key). Used to route `get_context`/`whats_changed`.
pub fn series_matching<'a>(series: &'a [Series], query: &str) -> Vec<&'a Series> {
    let q = normalize_title(query);
    let tokens: Vec<&str> = q.split_whitespace().collect();
    if tokens.is_empty() {
        return Vec::new();
    }
    series
        .iter()
        .filter(|s| tokens.iter().all(|t| s.key.contains(t)))
        .collect()
}

fn fmt_date(t: DateTime<Utc>) -> String {
    t.format("%Y-%m-%d").to_string()
}

fn item_line(storage: &Storage, item: &MeetingItem, date: DateTime<Utc>) -> String {
    let mut line = format!("- {} ", item.text);
    let mut meta = Vec::new();
    if let Some(owner) = &item.owner {
        meta.push(format!("owner: {owner}"));
    }
    if let Some(status) = &item.status {
        meta.push(format!("status: {status}"));
    }
    if let Some(key) = &item.speaker_key {
        // resolve the display label for the speaker at render time
        if let Ok(Some(t)) = storage.get_transcript(&item.meeting_id) {
            meta.push(format!("said by: {}", t.label_for(key)));
        }
    }
    meta.push(fmt_date(date));
    meta.push(format!("meeting: {}", item.meeting_id));
    if !item.segment_ids.is_empty() {
        meta.push(format!("segments: {}", item.segment_ids.join(",")));
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

/// The `get_context` briefing: participants, timeline, then every extracted
/// item grouped by kind, chronologically, each line carrying provenance.
pub fn build_briefing(
    storage: &Storage,
    series_list: &[&Series],
    since: Option<DateTime<Utc>>,
    query: &str,
) -> Result<String, String> {
    let mut meetings: Vec<&Meeting> = series_list
        .iter()
        .flat_map(|s| s.meetings.iter())
        .filter(|m| since.map(|s| m.started_at >= s).unwrap_or(true))
        .collect();
    meetings.sort_by_key(|m| m.started_at);
    if meetings.is_empty() {
        return Ok(format!(
            "No meetings matched \"{query}\"{}. Try search_notes for a broader text search.",
            since
                .map(|s| format!(" since {}", fmt_date(s)))
                .unwrap_or_default()
        ));
    }

    let mut out = String::new();
    out.push_str(&format!(
        "# Context: {}\n",
        series_list
            .iter()
            .map(|s| s.key.as_str())
            .collect::<Vec<_>>()
            .join(", ")
    ));
    out.push_str(&format!(
        "{} meeting(s), {} → {}\n",
        meetings.len(),
        fmt_date(meetings[0].started_at),
        fmt_date(meetings.last().unwrap().started_at)
    ));

    // participants: attendee union + speaker labels seen in transcripts
    let mut participants: Vec<String> = Vec::new();
    for m in &meetings {
        for a in &m.attendees {
            if !participants.iter().any(|p| p.eq_ignore_ascii_case(a)) {
                participants.push(a.clone());
            }
        }
        if let Ok(Some(t)) = storage.get_transcript(&m.id) {
            for s in &t.speakers {
                if !s.label.starts_with("Speaker ")
                    && !participants
                        .iter()
                        .any(|p| p.eq_ignore_ascii_case(&s.label))
                {
                    participants.push(s.label.clone());
                }
            }
        }
    }
    if !participants.is_empty() {
        out.push_str(&format!("Participants: {}\n", participants.join(", ")));
    }

    // timeline
    out.push_str("\n## Timeline\n");
    for m in &meetings {
        let has_transcript = storage.get_transcript(&m.id).ok().flatten().is_some();
        let n_items = storage
            .get_meeting_items(&m.id)
            .map(|i| i.len())
            .unwrap_or(0);
        out.push_str(&format!(
            "- {} — {} (meeting: {}{}, {} extracted item(s))\n",
            fmt_date(m.started_at),
            m.title,
            m.id,
            if has_transcript {
                ", transcript ✓"
            } else {
                ", no transcript"
            },
            n_items,
        ));
    }

    // items by kind, chronological within kind
    let meeting_ids: Vec<String> = meetings.iter().map(|m| m.id.clone()).collect();
    let date_of: std::collections::HashMap<&str, DateTime<Utc>> = meetings
        .iter()
        .map(|m| (m.id.as_str(), m.started_at))
        .collect();
    let mut items = storage
        .query_items(&ItemFilter {
            meeting_ids: Some(meeting_ids),
            limit: 500,
            ..Default::default()
        })
        .map_err(|e| e.to_string())?;
    items.sort_by_key(|i| date_of.get(i.meeting_id.as_str()).copied());

    let lines_of = |kind: ItemKind, items: &[MeetingItem]| -> Vec<String> {
        items
            .iter()
            .filter(|i| i.kind == kind)
            .map(|i| {
                item_line(
                    storage,
                    i,
                    date_of
                        .get(i.meeting_id.as_str())
                        .copied()
                        .unwrap_or_default(),
                )
            })
            .collect()
    };
    section(&mut out, "Decisions", lines_of(ItemKind::Decision, &items));
    section(
        &mut out,
        "Action items",
        lines_of(ItemKind::ActionItem, &items),
    );
    section(
        &mut out,
        "Commitments",
        lines_of(ItemKind::Commitment, &items),
    );
    section(
        &mut out,
        "Open questions",
        lines_of(ItemKind::Question, &items),
    );
    section(&mut out, "Key figures", lines_of(ItemKind::Figure, &items));
    if items.is_empty() {
        out.push_str(
            "\n(No extracted items yet for these meetings — run item extraction in the app, \
             or read the transcripts directly with get_transcripts.)\n",
        );
    }

    out.push_str(
        "\nEvery line above cites its meeting and transcript segment ids — verify any claim \
         with get_transcript(meeting_id) before relying on it.\n",
    );
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn meeting(title: &str, attendees: &[&str], days_ago: i64) -> Meeting {
        Meeting {
            id: format!("m-{title}-{days_ago}"),
            title: title.into(),
            note_id: "n".into(),
            attendees: attendees.iter().map(|s| s.to_string()).collect(),
            started_at: Utc::now() - chrono::Duration::days(days_ago),
            ended_at: None,
            recording: None,
        }
    }

    #[test]
    fn titles_normalize_dates_numbers_and_case_away() {
        assert_eq!(normalize_title("Weekly Sync 2026-07-01"), "weekly sync");
        assert_eq!(normalize_title("weekly sync #2"), "weekly sync");
        assert_eq!(
            normalize_title("Acme <> Us: Renewal (3/4)"),
            "acme us renewal"
        );
    }

    #[test]
    fn series_groups_by_title_and_participant_overlap() {
        let series = detect_series(vec![
            meeting("Weekly Sync 2026-06-01", &["a@x.com", "b@x.com"], 30),
            meeting("Weekly Sync 2026-06-08", &["a@x.com", "b@x.com"], 23),
            // same title, disjoint people → its own thread
            meeting("Weekly Sync", &["p@y.com", "q@y.com"], 20),
            meeting("Board prep", &["a@x.com"], 10),
        ]);
        let keys: Vec<&str> = series.iter().map(|s| s.key.as_str()).collect();
        assert_eq!(keys, vec!["board prep", "weekly sync", "weekly sync (2)"]);
        let weekly = series.iter().find(|s| s.key == "weekly sync").unwrap();
        assert_eq!(weekly.meetings.len(), 2);
        // chronological inside the series
        assert!(weekly.meetings[0].started_at < weekly.meetings[1].started_at);
    }

    #[test]
    fn empty_attendees_join_on_title_alone() {
        let series = detect_series(vec![
            meeting("1:1 Dana", &[], 14),
            meeting("1:1 Dana", &["dana@x.com"], 7),
        ]);
        assert_eq!(series.len(), 1);
        assert_eq!(series[0].meetings.len(), 2);
    }

    #[test]
    fn matching_requires_all_query_tokens() {
        let series = detect_series(vec![
            meeting("Acme renewal sync", &[], 7),
            meeting("Beta kickoff", &[], 6),
        ]);
        let hits = series_matching(&series, "acme renewal");
        assert_eq!(hits.len(), 1);
        assert_eq!(hits[0].key, "acme renewal sync");
        assert!(series_matching(&series, "gamma").is_empty());
    }
}
