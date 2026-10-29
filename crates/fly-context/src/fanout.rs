//! Fan-out for broad questions (spec §4.2a #9, Ask tier C): split a
//! question into 2–4 sub-queries (the original is always kept), run a
//! passage search for each in parallel, and merge the results — every
//! passage keeps its `[seg:ID]` lines and says which queries found it.
//!
//! * **Budgeted.** [`FanoutBudget`] caps the searches (each sub-query is
//!   one; the harness passes what is left of the tier's tool calls), the
//!   passages per search, the merged passages, passages per meeting and the
//!   merged text size.
//! * **Parallel.** Searches run on scoped threads, each through a
//!   [`Readers`] connection. `Mutex<Storage>` (the app's handle) serializes
//!   them; [`OpenPerWorker`] gives each worker its own read connection, so
//!   SQLite's WAL lets them run at once.
//! * **Visibility** is enforced exactly as for one search: every worker
//!   builds its own [`Context`] with the caller's [`Visibility`], and the
//!   merged set is re-checked against the Keep-local sets (fail closed).
//! * **Merging** is reciprocal-rank fusion, after a coverage pass that
//!   gives each query its best passage first — so one sub-query can't crowd
//!   out the others in a comparison.

use std::collections::{HashMap, HashSet};
use std::path::PathBuf;
use std::sync::Mutex;

use fly_storage::Storage;
use serde::Serialize;

use crate::tools::{Passage, PassageResults, PassageSource, SearchPassagesArgs};
use crate::{Context, ContextError, QueryVector, Result, Visibility};

/// RRF constant (as in [`crate::passages::fuse`]).
const RRF_K: f32 = 60.0;
/// Most sub-queries besides the original (spec: 2–4).
pub const MAX_SUB_QUERIES: usize = 4;

/// Where fan-out workers read storage.
pub trait Readers: Sync {
    /// Run `f` against a storage connection.
    fn read<R>(&self, f: impl FnOnce(&Storage) -> Result<R>) -> Result<R>;
}

/// The app's shared handle: workers take turns. Don't call [`fan_out`]
/// while holding this lock yourself: every worker waits on it (deadlock).
impl Readers for Mutex<Storage> {
    fn read<R>(&self, f: impl FnOnce(&Storage) -> Result<R>) -> Result<R> {
        // A poisoned lock means another command panicked mid-write; reads
        // still see a consistent SQLite database.
        let guard = self.lock().unwrap_or_else(|p| p.into_inner());
        f(&guard)
    }
}

/// A connection per search (`Storage::open_existing` on the data dir), so
/// searches run in parallel under WAL. It never migrates or creates
/// anything, and fan-out only reads through it; fly-storage has no
/// read-only open yet, so the connection itself is read-write.
#[derive(Debug, Clone)]
pub struct OpenPerWorker {
    data_dir: PathBuf,
}

impl OpenPerWorker {
    pub fn new(data_dir: impl Into<PathBuf>) -> Self {
        Self {
            data_dir: data_dir.into(),
        }
    }
}

impl Readers for OpenPerWorker {
    fn read<R>(&self, f: impl FnOnce(&Storage) -> Result<R>) -> Result<R> {
        let storage = Storage::open_existing(&self.data_dir)?;
        f(&storage)
    }
}

/// Limits for one fan-out.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub struct FanoutBudget {
    /// Searches allowed; queries beyond it are skipped (the original runs
    /// first).
    pub max_searches: u32,
    /// Passages each search returns (1–20).
    pub per_search_limit: u32,
    /// Passages in the merged result.
    pub max_passages: usize,
    /// Passages per meeting (or note) in the merged result.
    pub per_meeting: usize,
    /// Characters of passage text in the merged result.
    pub max_chars: usize,
    /// Searches running at once.
    pub max_parallel: usize,
}

impl FanoutBudget {
    /// Ask tier C (≤ 8 tool calls, ≤ 40k input tokens per answer): up to 5
    /// searches and ~24k chars (~8k tokens) of passages.
    pub const TIER_C: FanoutBudget = FanoutBudget {
        max_searches: 5,
        per_search_limit: 8,
        max_passages: 16,
        per_meeting: 3,
        max_chars: 24_000,
        max_parallel: 4,
    };
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct FannedPassage {
    pub passage: Passage,
    /// Indices into [`Fanout::queries`] whose search returned it.
    pub found_by: Vec<usize>,
    /// Fused RRF score across the searches.
    pub score: f32,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Fanout {
    /// The queries searched, original first.
    pub queries: Vec<String>,
    /// Merged, best first.
    pub passages: Vec<FannedPassage>,
    pub searches_run: u32,
    /// Queries not searched because of `max_searches`.
    pub skipped_queries: u32,
    /// At least one search was semantic (had a query vector).
    pub semantic: bool,
}

// ---------------------------------------------------------------------------
// Decomposition
// ---------------------------------------------------------------------------

/// Words that start an independent clause ("…and who owns the launch").
#[rustfmt::skip]
const CLAUSE_STARTS: &[&str] = &[
    "who", "what", "when", "where", "why", "how", "which", "did", "does", "do", "is", "are", "was",
    "were", "will", "would", "can", "could", "should", "has", "have", "had", "we", "i", "they",
    "he", "she", "it", "you", "there", "anything", "any",
];

#[rustfmt::skip]
const DETERMINERS: &[&str] = &[
    "the", "a", "an", "our", "their", "my", "your", "his", "her", "its",
];

const SEPARATORS: &[&str] = &[
    " compared with ",
    " compared to ",
    " as well as ",
    " versus ",
    " vs. ",
    " vs ",
    " and ",
    " or ",
    ", ",
];

fn first_word(s: &str) -> String {
    s.split_whitespace()
        .next()
        .unwrap_or("")
        .trim_matches(|c: char| !c.is_alphanumeric())
        .to_lowercase()
}

/// Split at coordinating separators (case-insensitive, outside words).
fn split_coordination(clause: &str) -> Vec<String> {
    // ASCII-only lowering keeps byte offsets aligned with `clause`.
    let lower = clause.to_ascii_lowercase();
    let mut cuts: Vec<(usize, usize)> = Vec::new();
    let mut i = 0;
    while i < lower.len() {
        let hit = SEPARATORS
            .iter()
            .find(|sep| lower[i..].starts_with(*sep))
            .map(|sep| sep.len());
        match hit {
            Some(len) => {
                cuts.push((i, i + len));
                i += len;
            }
            None => {
                i += lower[i..].chars().next().map_or(1, char::len_utf8);
            }
        }
    }
    let mut parts = Vec::new();
    let mut start = 0;
    for (a, b) in cuts {
        parts.push(clause[start..a].trim().to_string());
        start = b;
    }
    parts.push(clause[start..].trim().to_string());
    // "pricing, hiring, and the launch": the ", " cut leaves "and …".
    for p in &mut parts {
        for lead in ["and ", "or "] {
            // (`get` so a multibyte first char can't split a slice)
            if p.len() > lead.len()
                && p.get(..lead.len())
                    .is_some_and(|h| h.eq_ignore_ascii_case(lead))
            {
                *p = p[lead.len()..].trim().to_string();
            }
        }
    }
    parts.retain(|p| !p.is_empty());
    parts
}

/// One clause → sub-queries: independent clauses stay whole; a noun list
/// ("pricing, hiring and the launch") shares the words before it, and a
/// shared last word ("the Acme and Beta renewals").
fn expand_clause(clause: &str) -> Vec<String> {
    let parts = split_coordination(clause);
    if parts.len() < 2 {
        return vec![clause.trim().to_string()];
    }
    let mut out: Vec<String> = Vec::new();
    // Groups: a clause-starting part opens a new group; noun parts join
    // the current one.
    let mut groups: Vec<Vec<String>> = Vec::new();
    for (i, p) in parts.into_iter().enumerate() {
        if i == 0 || CLAUSE_STARTS.contains(&first_word(&p).as_str()) {
            groups.push(vec![p]);
        } else {
            groups.last_mut().expect("first part opens a group").push(p);
        }
    }
    for g in groups {
        if g.len() == 1 {
            out.push(g[0].clone());
            continue;
        }
        let mut head: Vec<&str> = g[0].split_whitespace().collect();
        let first_item = head.pop().unwrap_or("").to_string();
        let mut items: Vec<String> = vec![first_item];
        items.extend(g[1..].iter().cloned());
        // "the Acme and Beta renewals": a 2-word last item after 1-word
        // names shares its last word. Only after names (capitalized or
        // codes like "Q3"): "pricing and travel spend" shares nothing.
        let last: Vec<&str> = items[items.len() - 1].split_whitespace().collect();
        let tail = (last.len() == 2
            && items[..items.len() - 1].iter().all(|it| {
                it.split_whitespace().count() == 1
                    && it
                        .chars()
                        .next()
                        .is_some_and(|c| c.is_uppercase() || c.is_ascii_digit())
            })
            && !DETERMINERS.contains(&last[0].to_lowercase().as_str()))
        .then(|| last[1].to_string());
        let n = items.len();
        for (k, item) in items.into_iter().enumerate() {
            let item = match &tail {
                Some(t) if k + 1 < n => format!("{item} {t}"),
                _ => item,
            };
            let mut h = head.clone();
            if DETERMINERS.contains(&first_word(&item).as_str()) {
                while h
                    .last()
                    .is_some_and(|w| DETERMINERS.contains(&w.to_lowercase().as_str()))
                {
                    h.pop();
                }
            }
            let q = if h.is_empty() {
                item
            } else {
                format!("{} {item}", h.join(" "))
            };
            out.push(q);
        }
    }
    out
}

/// The queries for one question: the original first, then up to
/// `max_sub_queries` (≤ [`MAX_SUB_QUERIES`]) sub-queries from its
/// sentences and coordinations. Just the original when it doesn't split.
/// Deterministic; a cloud model may propose its own sub-queries instead.
pub fn decompose(question: &str, max_sub_queries: usize) -> Vec<String> {
    let q = question.trim();
    if q.is_empty() {
        return Vec::new();
    }
    let mut subs: Vec<String> = Vec::new();
    let sentences = q
        .split(['?', ';', '\n', '!'])
        .flat_map(|s| s.split(". "))
        .map(|s| s.trim().trim_end_matches('.').trim())
        .filter(|s| !s.is_empty());
    for s in sentences {
        subs.extend(expand_clause(s));
    }
    let mut out = vec![q.to_string()];
    if subs.len() < 2 {
        return out;
    }
    let norm = |s: &str| {
        s.split(|c: char| !c.is_alphanumeric())
            .filter(|w| !w.is_empty())
            .map(str::to_lowercase)
            .collect::<Vec<_>>()
            .join(" ")
    };
    let mut seen: HashSet<String> = HashSet::from([norm(q)]);
    for s in subs {
        let n = norm(&s);
        if n.is_empty() || !seen.insert(n) {
            continue;
        }
        if out.len() > max_sub_queries.min(MAX_SUB_QUERIES) {
            break;
        }
        out.push(s);
    }
    out
}

// ---------------------------------------------------------------------------
// Fan-out
// ---------------------------------------------------------------------------

/// A merged passage's identity: the same speaker-turn window (or note
/// paragraph) found by two searches is one passage.
fn passage_key(p: &Passage) -> String {
    match p.source {
        PassageSource::Transcript => format!(
            "t\u{1f}{}\u{1f}{}",
            p.meeting_id.as_deref().unwrap_or(""),
            p.segments.first().map(|s| s.id.as_str()).unwrap_or("")
        ),
        PassageSource::Note => format!("n\u{1f}{}\u{1f}{}", p.note_id, p.text),
    }
}

fn owner_of(p: &Passage) -> String {
    match (&p.source, &p.meeting_id) {
        (PassageSource::Transcript, Some(m)) => format!("m\u{1f}{m}"),
        _ => format!("n\u{1f}{}", p.note_id),
    }
}

/// Search every query (in parallel, within `budget`) and merge.
///
/// * `budget`: never more than [`FanoutBudget::TIER_C`] — `max_searches`
///   (at most 1 + [`MAX_SUB_QUERIES`]), `max_parallel`, `max_passages` and
///   `max_chars` are clamped to it whatever the caller passes. With no
///   searches left (`max_searches: 0`) nothing runs and
///   [`Fanout::searches_run`] is 0.
/// * `queries`: original first (see [`decompose`]); empty and duplicate
///   queries are dropped, and only the first `budget.max_searches` run.
/// * `base`: scope filters applied to every search (`meeting_ids`,
///   `folder_id`, dates); its `query` and `limit` are replaced.
/// * `vectors`: one optional query embedding per query, same order (may be
///   shorter or empty: keyword-only for the rest).
///
/// Any failed search fails the whole fan-out (a Keep-local lookup error is
/// never answered with partial, unfiltered results).
pub fn fan_out<R: Readers>(
    readers: &R,
    visibility: Visibility,
    queries: &[String],
    base: &SearchPassagesArgs,
    vectors: &[Option<QueryVector<'_>>],
    budget: &FanoutBudget,
) -> Result<Fanout> {
    // Which queries run, with their vector.
    let mut seen: HashSet<String> = HashSet::new();
    let mut wanted: Vec<(String, Option<QueryVector<'_>>)> = Vec::new();
    for (i, q) in queries.iter().enumerate() {
        let q = q.trim();
        if q.is_empty() || !seen.insert(q.to_lowercase()) {
            continue;
        }
        wanted.push((q.to_string(), vectors.get(i).copied().flatten()));
    }
    if wanted.is_empty() {
        return Err(ContextError::InvalidArgs("no query to search".into()));
    }
    // The caller's budget never exceeds tier C's.
    let cap = FanoutBudget::TIER_C;
    let budget = FanoutBudget {
        max_searches: budget.max_searches.min(1 + MAX_SUB_QUERIES as u32),
        per_search_limit: budget.per_search_limit.clamp(1, 20),
        max_passages: budget.max_passages.min(cap.max_passages),
        per_meeting: budget.per_meeting.max(1),
        max_chars: budget.max_chars.min(cap.max_chars),
        max_parallel: budget.max_parallel.clamp(1, cap.max_parallel),
    };
    let run_n = wanted.len().min(budget.max_searches as usize);
    let skipped = (wanted.len() - run_n) as u32;
    wanted.truncate(run_n);
    let per_search = budget.per_search_limit;
    let semantic = wanted.iter().any(|(_, v)| v.is_some());

    // Search, `max_parallel` at a time.
    let mut lists: Vec<PassageResults> = Vec::with_capacity(wanted.len());
    for wave in wanted.chunks(budget.max_parallel) {
        let results: Vec<Result<PassageResults>> = std::thread::scope(|scope| {
            let handles: Vec<_> = wave
                .iter()
                .map(|(q, qv)| {
                    let args = SearchPassagesArgs {
                        query: q.clone(),
                        limit: Some(per_search),
                        ..base.clone()
                    };
                    let qv = *qv;
                    scope.spawn(move || {
                        readers.read(|s| Context::new(s, visibility).search_passages(&args, qv))
                    })
                })
                .collect();
            handles
                .into_iter()
                .map(|h| h.join().unwrap_or_else(|p| std::panic::resume_unwind(p)))
                .collect()
        });
        for r in results {
            lists.push(r?);
        }
    }

    // Fuse.
    struct Acc {
        passage: Passage,
        found_by: Vec<usize>,
        score: f32,
        best_rank: usize,
    }
    let mut acc: HashMap<String, Acc> = HashMap::new();
    let mut order: Vec<Vec<String>> = Vec::with_capacity(lists.len());
    for (qi, list) in lists.into_iter().enumerate() {
        let mut keys = Vec::new();
        for (rank, p) in list.passages.into_iter().enumerate() {
            let key = passage_key(&p);
            let e = acc.entry(key.clone()).or_insert_with(|| Acc {
                passage: p,
                found_by: Vec::new(),
                score: 0.0,
                best_rank: rank,
            });
            if !e.found_by.contains(&qi) {
                e.found_by.push(qi);
                e.score += 1.0 / (RRF_K + rank as f32 + 1.0);
                e.best_rank = e.best_rank.min(rank);
                keys.push(key);
            }
        }
        order.push(keys);
    }

    // Defense in depth: drop anything the Keep-local sets hide (fail closed).
    if visibility == Visibility::ShareableOnly {
        let hidden = readers.read(|s| Context::new(s, visibility).hidden())?;
        acc.retain(|_, a| match (&a.passage.source, &a.passage.meeting_id) {
            (PassageSource::Transcript, Some(m)) => !hidden.meeting(m, &a.passage.note_id),
            (PassageSource::Transcript, None) => false,
            (PassageSource::Note, _) => !hidden.note(&a.passage.note_id),
        });
    }

    // Select: each query's best first (coverage), then by fused score.
    let mut by_score: Vec<&String> = acc.keys().collect();
    by_score.sort_by(|a, b| {
        let (x, y) = (&acc[*a], &acc[*b]);
        y.score
            .total_cmp(&x.score)
            .then(x.best_rank.cmp(&y.best_rank))
            .then(a.cmp(b))
    });
    let mut picked: Vec<String> = Vec::new();
    let mut per_owner: HashMap<String, usize> = HashMap::new();
    let mut chars = 0usize;
    let mut try_take = |key: &String, picked: &mut Vec<String>| -> bool {
        if picked.len() >= budget.max_passages || picked.contains(key) {
            return false;
        }
        let a = &acc[key];
        let owner = owner_of(&a.passage);
        if per_owner.get(&owner).copied().unwrap_or(0) >= budget.per_meeting {
            return false;
        }
        let len = a.passage.text.len();
        if !picked.is_empty() && chars + len > budget.max_chars {
            return false;
        }
        chars += len;
        *per_owner.entry(owner).or_default() += 1;
        picked.push(key.clone());
        true
    };
    for keys in &order {
        for k in keys {
            // Covered already (another query's pick was this one's best):
            // don't spend a second slot on it.
            if picked.contains(k) {
                break;
            }
            if acc.contains_key(k) && try_take(k, &mut picked) {
                break;
            }
        }
    }
    for k in by_score {
        try_take(k, &mut picked);
    }

    let mut passages: Vec<FannedPassage> = picked
        .into_iter()
        .filter_map(|k| acc.remove(&k))
        .map(|a| {
            let mut found_by = a.found_by;
            found_by.sort_unstable();
            FannedPassage {
                passage: a.passage,
                found_by,
                score: a.score,
            }
        })
        .collect();
    passages.sort_by(|a, b| b.score.total_cmp(&a.score));
    Ok(Fanout {
        queries: wanted.into_iter().map(|(q, _)| q).collect(),
        passages,
        searches_run: run_n as u32,
        skipped_queries: skipped,
        semantic,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn single_topic_questions_do_not_split() {
        assert_eq!(
            decompose("What did we decide about pricing?", 4),
            vec!["What did we decide about pricing?"]
        );
        assert!(decompose("   ", 4).is_empty());
    }

    #[test]
    fn independent_clauses_split_and_the_original_is_kept() {
        let q = "What did Dana say about pricing and who owns the launch checklist?";
        assert_eq!(
            decompose(q, 4),
            vec![
                q,
                "What did Dana say about pricing",
                "who owns the launch checklist"
            ]
        );
        let two = "When is the beta? What is the budget?";
        assert_eq!(
            decompose(two, 4),
            vec![two, "When is the beta", "What is the budget"]
        );
    }

    #[test]
    fn noun_lists_share_their_head_and_shared_last_word() {
        assert_eq!(
            decompose("Compare the Acme and Beta renewals", 4),
            vec![
                "Compare the Acme and Beta renewals",
                "Compare the Acme renewals",
                "Compare the Beta renewals"
            ]
        );
        assert_eq!(
            decompose(
                "What did we decide about pricing, hiring and the launch?",
                4
            ),
            vec![
                "What did we decide about pricing, hiring and the launch?",
                "What did we decide about pricing",
                "What did we decide about hiring",
                "What did we decide about the launch"
            ]
        );
        assert_eq!(
            decompose("Acme vs Beta", 4),
            vec!["Acme vs Beta", "Acme", "Beta"]
        );
        // lowercase words are topics, not names sharing a noun
        assert_eq!(
            decompose("pricing and travel spend", 4),
            vec!["pricing and travel spend", "pricing", "travel spend"]
        );
    }

    #[test]
    fn an_oxford_comma_leaves_no_stray_and() {
        assert_eq!(
            decompose(
                "What did we decide about pricing, hiring, and the launch?",
                4
            ),
            vec![
                "What did we decide about pricing, hiring, and the launch?",
                "What did we decide about pricing",
                "What did we decide about hiring",
                "What did we decide about the launch"
            ]
        );
    }

    #[test]
    fn sub_queries_are_capped_and_deduplicated() {
        let q = "Status of alpha, beta, gamma, delta, epsilon and zeta";
        let d = decompose(q, 4);
        assert_eq!(d.len(), 5, "{d:?}");
        assert_eq!(d[0], q);
        assert_eq!(decompose(q, 2).len(), 3);
        assert_eq!(decompose(q, 9).len(), 1 + MAX_SUB_QUERIES);
        // duplicates collapse
        let d = decompose("pricing and pricing", 4);
        assert_eq!(d, vec!["pricing and pricing", "pricing"]);
    }
}
