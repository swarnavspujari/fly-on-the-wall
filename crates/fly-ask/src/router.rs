//! Deterministic self-query router (spec §4.2a #4), run before the
//! prefetch: pull dates, people, a folder and a list intent ("open action
//! items for Dana") out of the question, and validate each against known
//! values — anything not in the user's meetings is dropped, never invented.
//! The prefetch applies these as soft filters: when a filter leaves fewer
//! than 3 passages it searches again without it.

use chrono::{DateTime, Datelike, Duration, NaiveDate, Utc, Weekday};
use fly_core::ItemKind;
use serde::{Deserialize, Serialize};

/// Values the router may match against.
#[derive(Debug, Clone, Default)]
pub struct Known {
    /// People: attendee names, speaker labels and item owners.
    pub people: Vec<String>,
    /// Folders: (id, display name).
    pub folders: Vec<(String, String)>,
    /// The earliest visible meeting, to drop impossible date ranges.
    pub earliest: Option<DateTime<Utc>>,
}

/// Which date phrase matched (typed, for the run log).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DateRule {
    Today,
    Yesterday,
    ThisWeek,
    LastWeek,
    ThisMonth,
    LastMonth,
    PastDays,
    Weekday,
    Month,
    Since,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ItemIntent {
    pub kind: Option<ItemKind>,
    /// "open" / "outstanding" / "still": open action items only.
    pub open_only: bool,
}

#[derive(Debug, Clone, Default, PartialEq)]
pub struct Hints {
    pub since: Option<DateTime<Utc>>,
    pub until: Option<DateTime<Utc>>,
    pub date_rule: Option<DateRule>,
    /// Canonical names from [`Known::people`].
    pub people: Vec<String>,
    pub folder_id: Option<String>,
    pub items: Option<ItemIntent>,
}

impl Hints {
    pub fn has_filters(&self) -> bool {
        self.since.is_some() || self.until.is_some() || !self.people.is_empty()
    }
}

fn day_start(d: NaiveDate) -> DateTime<Utc> {
    d.and_hms_opt(0, 0, 0).expect("midnight").and_utc()
}

fn day_end(d: NaiveDate) -> DateTime<Utc> {
    d.and_hms_milli_opt(23, 59, 59, 999)
        .expect("end of day")
        .and_utc()
}

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

const WEEKDAYS: &[(&str, Weekday)] = &[
    ("monday", Weekday::Mon),
    ("tuesday", Weekday::Tue),
    ("wednesday", Weekday::Wed),
    ("thursday", Weekday::Thu),
    ("friday", Weekday::Fri),
    ("saturday", Weekday::Sat),
    ("sunday", Weekday::Sun),
];

const MEETING_WORDS: &[&str] = &["meeting", "meetings", "call", "calls", "sync", "standup"];

fn words(q: &str) -> Vec<String> {
    q.split(|c: char| !(c.is_alphanumeric() || c == '\''))
        .filter(|w| !w.is_empty())
        .map(|w| w.to_lowercase())
        .collect()
}

/// Month range: the most recent such month at or before `now`.
fn month_range(m: u32, now: DateTime<Utc>) -> (DateTime<Utc>, DateTime<Utc>) {
    let today = now.date_naive();
    let year = if m <= today.month() {
        today.year()
    } else {
        today.year() - 1
    };
    let start = NaiveDate::from_ymd_opt(year, m, 1).expect("valid month");
    let next = if m == 12 {
        NaiveDate::from_ymd_opt(year + 1, 1, 1)
    } else {
        NaiveDate::from_ymd_opt(year, m + 1, 1)
    }
    .expect("valid month");
    (day_start(start), day_end(next - Duration::days(1)))
}

/// Date phrases that clearly scope WHICH MEETINGS (not dates mentioned in
/// them: "is the launch in October?" talks about October, it doesn't ask
/// about October's meetings).
fn dates(q: &str, now: DateTime<Utc>) -> Option<(DateRule, DateTime<Utc>, DateTime<Utc>)> {
    let ws = words(q);
    let joined = format!(" {} ", ws.join(" "));
    let has = |p: &str| joined.contains(&format!(" {p} "));
    let today = now.date_naive();
    let monday = today - Duration::days(today.weekday().num_days_from_monday() as i64);
    if has("yesterday") {
        let d = today - Duration::days(1);
        return Some((DateRule::Yesterday, day_start(d), day_end(d)));
    }
    if has("today") || has("this morning") || has("this afternoon") {
        return Some((DateRule::Today, day_start(today), now));
    }
    if has("last week") {
        let start = monday - Duration::days(7);
        return Some((
            DateRule::LastWeek,
            day_start(start),
            day_end(monday - Duration::days(1)),
        ));
    }
    if has("this week") {
        return Some((DateRule::ThisWeek, day_start(monday), now));
    }
    let first = today.with_day(1).expect("day 1");
    if has("last month") {
        let prev_end = first - Duration::days(1);
        let prev_start = prev_end.with_day(1).expect("day 1");
        return Some((
            DateRule::LastMonth,
            day_start(prev_start),
            day_end(prev_end),
        ));
    }
    if has("this month") {
        return Some((DateRule::ThisMonth, day_start(first), now));
    }
    // "past/last N days|weeks", "past week", "past few days"
    for (i, w) in ws.iter().enumerate() {
        if w != "past" && w != "last" {
            continue;
        }
        let (n, unit) = match (ws.get(i + 1).map(String::as_str), ws.get(i + 2)) {
            (Some("week"), _) if w == "past" => (1, "week"),
            (Some("few"), Some(u)) => (3, u.as_str()),
            (Some(n), Some(u)) => match n.parse::<i64>().ok().or_else(|| small_number(n)) {
                Some(n) => (n, u.as_str()),
                None => continue,
            },
            _ => continue,
        };
        let days = match unit {
            "day" | "days" => n,
            "week" | "weeks" => n * 7,
            _ => continue,
        };
        if (1..=366).contains(&days) {
            return Some((
                DateRule::PastDays,
                day_start(today - Duration::days(days)),
                now,
            ));
        }
    }
    // "since <month>" / "since 2026-09-01"
    if let Some(i) = ws.iter().position(|w| w == "since") {
        if let Some(next) = ws.get(i + 1) {
            if let Some(m) = MONTHS.iter().position(|m| m == next) {
                let (start, _) = month_range(m as u32 + 1, now);
                return Some((DateRule::Since, start, now));
            }
        }
        if let Some(d) = q
            .split_whitespace()
            .skip_while(|w| !w.eq_ignore_ascii_case("since"))
            .nth(1)
            .and_then(|w| {
                NaiveDate::parse_from_str(
                    w.trim_matches(|c: char| !c.is_ascii_digit() && c != '-'),
                    "%Y-%m-%d",
                )
                .ok()
            })
        {
            return Some((DateRule::Since, day_start(d), now));
        }
    }
    // "Friday's meeting", "on Monday's call", "the Tuesday sync"
    for (name, wd) in WEEKDAYS {
        let poss = format!("{name}'s");
        let pos = ws.iter().position(|w| w == name || *w == poss);
        if let Some(i) = pos {
            let next = ws.get(i + 1).map(String::as_str).unwrap_or("");
            if MEETING_WORDS.contains(&next) {
                // The most recent such day, today included.
                let back = (today.weekday().num_days_from_monday() as i64
                    - wd.num_days_from_monday() as i64)
                    .rem_euclid(7);
                let d = today - Duration::days(back);
                return Some((DateRule::Weekday, day_start(d), day_end(d)));
            }
        }
    }
    // "in the September meetings", "during October", "October's calls"
    for (i, w) in ws.iter().enumerate() {
        let base = w.strip_suffix("'s").unwrap_or(w);
        let Some(m) = MONTHS.iter().position(|x| *x == base) else {
            continue;
        };
        let prev = if i > 0 { ws[i - 1].as_str() } else { "" };
        let next = ws.get(i + 1).map(String::as_str).unwrap_or("");
        if prev == "during" || MEETING_WORDS.contains(&next) {
            let (s, e) = month_range(m as u32 + 1, now);
            return Some((DateRule::Month, s, e.min(now)));
        }
    }
    None
}

fn small_number(w: &str) -> Option<i64> {
    [
        "one", "two", "three", "four", "five", "six", "seven", "eight", "nine", "ten",
    ]
    .iter()
    .position(|x| *x == w)
    .map(|i| i as i64 + 1)
}

/// People named in the question: a full name or label anywhere
/// (case-insensitive), or a first name written capitalized ("Dana", not
/// "will" for Will).
fn people(q: &str, known: &[String]) -> Vec<String> {
    let plain: Vec<String> = words(q)
        .into_iter()
        .map(|w| w.strip_suffix("'s").map(str::to_string).unwrap_or(w))
        .collect();
    let lower = format!(" {} ", plain.join(" "));
    let caps: Vec<&str> = q
        .split(|c: char| !(c.is_alphanumeric() || c == '\''))
        .filter(|w| w.starts_with(|c: char| c.is_uppercase()))
        .map(|w| w.strip_suffix("'s").unwrap_or(w))
        .collect();
    let mut out: Vec<String> = Vec::new();
    for name in known {
        let n = name.trim();
        if n.is_empty() || n.eq_ignore_ascii_case("you") {
            continue;
        }
        let full = format!(" {} ", words(n).join(" "));
        let full_hit = full.trim().len() >= 3 && lower.contains(&full);
        let first = n.split_whitespace().next().unwrap_or("");
        let first_hit = first.chars().count() >= 3
            && n.split_whitespace().count() > 1
            && caps.iter().any(|c| c.eq_ignore_ascii_case(first));
        if (full_hit || first_hit) && !out.iter().any(|o| o.eq_ignore_ascii_case(n)) {
            out.push(n.to_string());
        }
    }
    out
}

fn folder(q: &str, folders: &[(String, String)]) -> Option<String> {
    let lower = q.to_lowercase();
    folders
        .iter()
        .filter(|(_, name)| name.trim().chars().count() >= 3)
        .find(|(_, name)| {
            let n = name.trim().to_lowercase();
            lower.contains(&format!("@{n}"))
                || lower.contains(&format!("{n} folder"))
                || lower.contains(&format!("folder {n}"))
        })
        .map(|(id, _)| id.clone())
}

fn items(q: &str) -> Option<ItemIntent> {
    let l = format!(" {} ", words(q).join(" "));
    let any = |ps: &[&str]| ps.iter().any(|p| l.contains(&format!(" {p}")));
    let kind = if any(&[
        "action item",
        "to do",
        "todo",
        "to-do",
        "next step",
        "follow up",
        "follow-up",
        "task",
        "assigned",
        "owes",
    ]) {
        Some(ItemKind::ActionItem)
    } else if any(&["decision", "decide", "agreed", "agree on"]) {
        Some(ItemKind::Decision)
    } else if any(&["open question", "unresolved", "unanswered"]) {
        Some(ItemKind::Question)
    } else if any(&["commitment", "committed", "promised"]) {
        Some(ItemKind::Commitment)
    } else {
        return None;
    };
    let open_only = kind == Some(ItemKind::ActionItem)
        && any(&[
            "open",
            "outstanding",
            "pending",
            "still",
            "remaining",
            "not done",
        ]);
    Some(ItemIntent { kind, open_only })
}

/// Route a question. `wide` = the scope spans more than one note (dates
/// and people only filter wide scopes).
pub fn route(question: &str, now: DateTime<Utc>, known: &Known, wide: bool) -> Hints {
    let mut h = Hints {
        items: items(question),
        people: people(question, &known.people),
        folder_id: if wide {
            folder(question, &known.folders)
        } else {
            None
        },
        ..Default::default()
    };
    if wide {
        if let Some((rule, since, until)) = dates(question, now) {
            // Validate: the range must overlap the meetings there are.
            let plausible = until >= known.earliest.unwrap_or(since) && since <= now;
            if plausible {
                h.date_rule = Some(rule);
                h.since = Some(since);
                h.until = Some(until);
            }
        }
    }
    h
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::TimeZone;

    fn now() -> DateTime<Utc> {
        // A Saturday.
        Utc.with_ymd_and_hms(2026, 10, 10, 17, 0, 0).unwrap()
    }

    fn known() -> Known {
        Known {
            people: vec![
                "Ines Carvalho".into(),
                "Dana".into(),
                "Will Park".into(),
                "Speaker 2".into(),
                "You".into(),
            ],
            folders: vec![("f1".into(), "Customers".into())],
            earliest: Some(Utc.with_ymd_and_hms(2026, 9, 1, 0, 0, 0).unwrap()),
        }
    }

    #[test]
    fn date_phrases_scope_meetings_not_mentions() {
        let h = route("What did we decide last week?", now(), &known(), true);
        assert_eq!(h.date_rule, Some(DateRule::LastWeek));
        assert_eq!(h.since.unwrap().date_naive().to_string(), "2026-09-28");
        assert_eq!(h.until.unwrap().date_naive().to_string(), "2026-10-04");
        let h = route(
            "What did the board decide at Friday's meeting?",
            now(),
            &known(),
            true,
        );
        assert_eq!(h.date_rule, Some(DateRule::Weekday));
        assert_eq!(h.since.unwrap().date_naive().to_string(), "2026-10-09");
        // A date the meetings talk about is not a filter.
        let h = route("Is the launch in October?", now(), &known(), true);
        assert_eq!(h.date_rule, None);
        let h = route(
            "What came up in the September meetings?",
            now(),
            &known(),
            true,
        );
        assert_eq!(h.date_rule, Some(DateRule::Month));
        let h = route("Anything new in the past 3 days?", now(), &known(), true);
        assert_eq!(h.date_rule, Some(DateRule::PastDays));
        // Narrow scopes ignore dates.
        assert_eq!(
            route("What did we decide last week?", now(), &known(), false).since,
            None
        );
    }

    #[test]
    fn impossible_dates_are_dropped() {
        let k = Known {
            earliest: Some(Utc.with_ymd_and_hms(2026, 10, 9, 0, 0, 0).unwrap()),
            ..known()
        };
        assert_eq!(
            route("What did we decide last month?", now(), &k, true).since,
            None
        );
    }

    #[test]
    fn people_are_validated_against_known_names() {
        let h = route("What did Ines ask for?", now(), &known(), true);
        assert_eq!(h.people, vec!["Ines Carvalho"]);
        assert!(route("Who will own it?", now(), &known(), true)
            .people
            .is_empty());
        assert_eq!(
            route("What did dana say?", now(), &known(), true).people,
            vec!["Dana"]
        );
        assert_eq!(
            route("What did speaker 2 propose?", now(), &known(), true).people,
            vec!["Speaker 2"]
        );
        assert!(route("What did Zed say?", now(), &known(), true)
            .people
            .is_empty());
    }

    #[test]
    fn list_intents_and_folders() {
        let h = route("What are Dana's open action items?", now(), &known(), true);
        assert_eq!(
            h.items,
            Some(ItemIntent {
                kind: Some(ItemKind::ActionItem),
                open_only: true
            })
        );
        assert_eq!(h.people, vec!["Dana"]);
        assert_eq!(
            route("What did we decide about pricing?", now(), &known(), true)
                .items
                .unwrap()
                .kind,
            Some(ItemKind::Decision)
        );
        assert!(route("Why did the budget go up?", now(), &known(), true)
            .items
            .is_none());
        assert_eq!(
            route("Summarize @customers", now(), &known(), true)
                .folder_id
                .as_deref(),
            Some("f1")
        );
    }
}
