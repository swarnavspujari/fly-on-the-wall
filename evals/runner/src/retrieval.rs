//! The retrieval suite: gold questions through
//! `fly_context::Context::search_passages` (limit 8) on the fixture
//! database, scored by segment id; plus the keep-local leak sweep over
//! every search and tool path under `Visibility::ShareableOnly`.

use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;

use anyhow::{ensure, Context as _, Result};
use fly_context::{call, Context, QueryVector, SearchPassagesArgs, Visibility};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

use crate::corpus::{Corpus, Db};
use crate::embedder::Embedder;
use crate::report::SuiteResult;

pub const K: usize = 8;

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct GoldSet {
    pub answerable: Vec<Answerable>,
    pub unanswerable: Vec<Unanswerable>,
}

/// One must-cite requirement: a segment id, or alternatives (any one).
#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(untagged)]
pub enum Requirement {
    One(String),
    Any(Vec<String>),
}

impl Requirement {
    pub fn ids(&self) -> Vec<&str> {
        match self {
            Requirement::One(s) => vec![s.as_str()],
            Requirement::Any(v) => v.iter().map(String::as_str).collect(),
        }
    }
    pub fn met_by(&self, segs: &BTreeSet<String>) -> bool {
        self.ids().iter().any(|id| segs.contains(*id))
    }
}

#[derive(Debug, Clone, Default, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct Scope {
    #[serde(default)]
    pub meeting: Option<String>,
    #[serde(default)]
    pub folder: Option<String>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Answerable {
    pub id: String,
    pub question: String,
    pub must_cite: Vec<Requirement>,
    pub evidence: Vec<String>,
    pub answer: String,
    pub category: Vec<String>,
    #[serde(default)]
    pub scope: Option<Scope>,
    #[serde(default)]
    pub keep_local: bool,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Unanswerable {
    pub id: String,
    pub question: String,
    pub why: String,
    /// "shareable": only the shareable view must find nothing.
    #[serde(default)]
    pub visibility: Option<String>,
    /// Off-corpus (the topic never comes up) or near-miss (the topic does,
    /// the asked-for fact doesn't): reported separately.
    pub kind: UnanswerableKind,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Deserialize, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum UnanswerableKind {
    OffCorpus,
    NearMiss,
}

impl UnanswerableKind {
    pub fn as_str(self) -> &'static str {
        match self {
            UnanswerableKind::OffCorpus => "off_corpus",
            UnanswerableKind::NearMiss => "near_miss",
        }
    }
}

impl Unanswerable {
    pub fn shareable_only(&self) -> bool {
        self.visibility.as_deref() == Some("shareable")
    }
}

impl GoldSet {
    pub fn load(path: &Path) -> Result<Self> {
        let text =
            std::fs::read_to_string(path).with_context(|| format!("read {}", path.display()))?;
        toml::from_str(&text).with_context(|| format!("parse {}", path.display()))
    }

    /// Every label checked against the fixture text, so a wrong id fails
    /// the suite instead of skewing it.
    pub fn validate(&self, corpus: &Corpus) -> Result<()> {
        let mut ids = BTreeSet::new();
        for q in &self.answerable {
            ensure!(
                ids.insert(q.id.clone()),
                "question id {} appears twice",
                q.id
            );
            ensure!(!q.must_cite.is_empty(), "{}: no must_cite", q.id);
            ensure!(!q.evidence.is_empty(), "{}: no evidence", q.id);
            let mut text = String::new();
            let mut all_keep_local = true;
            for r in &q.must_cite {
                let alts = r.ids();
                ensure!(!alts.is_empty(), "{}: empty alternative list", q.id);
                for id in alts {
                    let s = corpus
                        .segment(id)
                        .with_context(|| format!("{}: must_cite {id} is not a segment", q.id))?;
                    text.push_str(&s.text.to_lowercase());
                    text.push('\n');
                    all_keep_local &= corpus.is_keep_local_segment(id);
                    if let Some(scope) = &q.scope {
                        if let Some(m) = &scope.meeting {
                            ensure!(&s.meeting == m, "{}: {id} is outside scope {m}", q.id);
                        }
                        if let Some(f) = &scope.folder {
                            ensure!(
                                corpus.in_folder(&s.meeting, f),
                                "{}: {id} is outside folder {f}",
                                q.id
                            );
                        }
                    }
                }
            }
            for e in &q.evidence {
                ensure!(
                    text.contains(&e.to_lowercase()),
                    "{}: evidence {e:?} is not in its must-cite segments",
                    q.id
                );
            }
            // The gold answer must pass the claim check against the segments
            // the `auto` script cites for it (the first of each requirement).
            let cited: Vec<&crate::corpus::Segment> = q
                .must_cite
                .iter()
                .filter_map(|r| corpus.segment(r.ids()[0]))
                .collect();
            let sources: Vec<crate::claims::Source<'_>> = cited
                .iter()
                .map(|s| crate::claims::Source {
                    text: &s.text,
                    speaker: &s.speaker,
                })
                .collect();
            let missing = crate::claims::unsupported(&q.answer, &sources);
            ensure!(
                missing.is_empty(),
                "{}: the gold answer fails its own claim check ({})",
                q.id,
                missing.join(", ")
            );
            ensure!(
                q.keep_local == all_keep_local,
                "{}: keep_local must be {} (where its must-cite segments live)",
                q.id,
                all_keep_local
            );
            if let Some(scope) = &q.scope {
                ensure!(
                    scope.meeting.is_some() != scope.folder.is_some(),
                    "{}: scope needs exactly one of meeting or folder",
                    q.id
                );
                if let Some(m) = &scope.meeting {
                    ensure!(corpus.meeting(m).is_some(), "{}: unknown meeting {m}", q.id);
                }
                if let Some(f) = &scope.folder {
                    ensure!(
                        corpus.folder_paths().contains(f),
                        "{}: unknown folder {f}",
                        q.id
                    );
                }
            }
        }
        for u in &self.unanswerable {
            ensure!(
                ids.insert(u.id.clone()),
                "question id {} appears twice",
                u.id
            );
            if let Some(v) = &u.visibility {
                ensure!(
                    v == "shareable",
                    "{}: visibility must be \"shareable\"",
                    u.id
                );
            }
        }
        Ok(())
    }
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Probes {
    #[serde(default)]
    pub search: Vec<SearchProbe>,
    #[serde(default)]
    pub tool: Vec<ToolProbe>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SearchProbe {
    pub query: String,
    #[serde(default)]
    pub folder: Option<String>,
    #[serde(default)]
    pub meetings: Option<Vec<String>>,
    #[serde(default)]
    pub since: Option<String>,
    #[serde(default)]
    pub until: Option<String>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ToolProbe {
    pub name: String,
    pub args: toml::Value,
}

/// A fixture scope as `search_passages` arguments.
pub fn scoped_args(
    db: &Db,
    query: &str,
    scope: Option<&Scope>,
    limit: usize,
) -> SearchPassagesArgs {
    let mut args = SearchPassagesArgs {
        query: query.into(),
        limit: Some(limit as u32),
        ..Default::default()
    };
    if let Some(s) = scope {
        if let Some(m) = &s.meeting {
            args.meeting_ids = Some(vec![db.meeting_ids[m].clone()]);
        }
        if let Some(f) = &s.folder {
            args.folder_id = Some(db.folder_ids[f].clone());
        }
    }
    args
}

/// Everything that marks content as the keep-local meeting's.
pub struct LeakNeedles {
    needles: Vec<String>,
}

impl LeakNeedles {
    pub fn new(corpus: &Corpus, db: &Db) -> Self {
        let mut needles = corpus.canaries();
        for m in corpus.keep_local_meetings() {
            needles.push(m.title.to_lowercase());
            needles.push(db.meeting_ids[&m.id].to_lowercase());
            needles.push(db.note_ids[&m.id].to_lowercase());
            for (id, ..) in &m.segments {
                // Database segment ids are UUIDs: unique anywhere in text.
                needles.push(db.seg_ids[id].to_lowercase());
            }
        }
        Self { needles }
    }

    /// Also every keep-local segment's full text (for scanning requests,
    /// where content can arrive without its id).
    pub fn with_texts(mut self, corpus: &Corpus) -> Self {
        for s in corpus.segments() {
            if corpus.is_keep_local_segment(&s.id) && s.text.chars().count() >= 25 {
                self.needles.push(s.text.to_lowercase());
            }
        }
        self
    }

    /// Needles found in a serialized value or text.
    pub fn find(&self, haystack: &str) -> Vec<String> {
        let h = haystack.to_lowercase();
        self.needles
            .iter()
            .filter(|n| h.contains(n.as_str()))
            .cloned()
            .collect()
    }
}

pub struct Scored {
    pub recall: f64,
    pub rr: f64,
    pub covered: bool,
    pub rank: Option<usize>,
}

/// Gating score: only transcript segments count (note passages, which carry
/// no segment ids, never satisfy a must-cite requirement but still take one
/// of the 8 slots; crowding citable passages out is a real miss).
/// Database segment id → fixture segment id.
pub type IdMap<'a> = &'a dyn Fn(&str) -> Option<String>;

/// A passage's segments as fixture ids.
pub fn passage_segments(ids: IdMap<'_>, p: &fly_context::Passage) -> BTreeSet<String> {
    p.segments.iter().filter_map(|s| ids(&s.id)).collect()
}

pub fn score(ids: IdMap<'_>, q: &Answerable, passages: &[fly_context::Passage]) -> Scored {
    let top = &passages[..passages.len().min(K)];
    let mut all = BTreeSet::new();
    let mut rank = None;
    for (i, p) in top.iter().enumerate() {
        let segs = passage_segments(ids, p);
        if rank.is_none() && q.must_cite.iter().any(|r| r.met_by(&segs)) {
            rank = Some(i + 1);
        }
        all.extend(segs);
    }
    let met = q.must_cite.iter().filter(|r| r.met_by(&all)).count();
    Scored {
        recall: met as f64 / q.must_cite.len() as f64,
        rr: rank.map_or(0.0, |r| 1.0 / r as f64),
        covered: met == q.must_cite.len(),
        rank,
    }
}

/// Report-only evidence score: like [`score`], but a note passage of a
/// meeting that holds a requirement's segment also meets it (the user's own
/// notes are evidence an answer could use, though not citable lines).
/// `req_meetings[i]`: database ids of the meetings requirement `i` lives in.
fn score_evidence(
    ids: IdMap<'_>,
    q: &Answerable,
    passages: &[fly_context::Passage],
    req_meetings: &[BTreeSet<String>],
) -> Scored {
    let top = &passages[..passages.len().min(K)];
    let mut met_reqs = vec![false; q.must_cite.len()];
    let mut rank = None;
    for (i, p) in top.iter().enumerate() {
        let segs = passage_segments(ids, p);
        let note_meeting = (p.source == fly_context::PassageSource::Note)
            .then_some(p.meeting_id.as_ref())
            .flatten();
        for (k, r) in q.must_cite.iter().enumerate() {
            let hit = r.met_by(&segs) || note_meeting.is_some_and(|m| req_meetings[k].contains(m));
            if hit {
                met_reqs[k] = true;
                rank.get_or_insert(i + 1);
            }
        }
    }
    let met = met_reqs.iter().filter(|m| **m).count();
    Scored {
        recall: met as f64 / q.must_cite.len() as f64,
        rr: rank.map_or(0.0, |r| 1.0 / r as f64),
        covered: met == q.must_cite.len(),
        rank,
    }
}

fn mean(v: &[f64]) -> f64 {
    if v.is_empty() {
        0.0
    } else {
        v.iter().sum::<f64>() / v.len() as f64
    }
}

/// The retrieval suite under `emb`, reported as suite `name`
/// (`retrieval` for the CI embedder, `retrieval_real` for the app's).
pub fn run(evals_dir: &Path, emb: &Embedder, name: &str) -> SuiteResult {
    match run_inner(evals_dir, emb, name) {
        Ok(r) => r,
        Err(e) => SuiteResult::failed(name, e),
    }
}

/// Area under the ROC curve: the chance a random answerable question's
/// top-1 cosine beats a random unanswerable one's (ties count half).
fn auroc(pos: &[f64], neg: &[f64]) -> f64 {
    if pos.is_empty() || neg.is_empty() {
        return 0.0;
    }
    let mut wins = 0.0;
    for p in pos {
        for n in neg {
            wins += if p > n {
                1.0
            } else if p == n {
                0.5
            } else {
                0.0
            };
        }
    }
    wins / (pos.len() * neg.len()) as f64
}

/// The best cosine any chunk scores for `q` (the score floor's input; the
/// fused passage score is rank-only and can't serve as one).
fn top1_cosine(db: &Db, emb: &Embedder, q: &str, shareable_only: bool) -> Result<f64> {
    let v = emb.query(q)?;
    let filter = fly_storage::SearchFilter {
        folder_id: None,
        since: None,
        until: None,
        limit: 1,
        offset: 0,
        exclude_keep_local: shareable_only,
    };
    let hits = db.storage.vector_search(&v, emb.model(), &filter, 1)?;
    Ok(hits.first().map_or(0.0, |h| h.score as f64))
}

fn run_inner(evals_dir: &Path, emb: &Embedder, name: &str) -> Result<SuiteResult> {
    let corpus = Corpus::load(&evals_dir.join("fixtures"))?;
    let suite = evals_dir.join("suites").join("retrieval");
    let gold = GoldSet::load(&suite.join("questions.toml"))?;
    gold.validate(&corpus)?;
    let probes: Probes = toml::from_str(&std::fs::read_to_string(
        suite.join("keep_local_probes.toml"),
    )?)
    .context("parse keep_local_probes.toml")?;
    let db = Db::build(&corpus, false)?;
    let indexed = emb.index(&db.storage)?;
    let ids: IdMap<'_> = &|s| db.fixture_seg(s).map(str::to_string);
    let all = Context::new(&db.storage, Visibility::All);
    let shareable = Context::new(&db.storage, Visibility::ShareableOnly);

    let mut res = SuiteResult::new(name);
    let (mut recalls, mut rrs, mut covered) = (Vec::new(), Vec::new(), Vec::new());
    let (mut kw_recalls, mut kw_rrs) = (Vec::new(), Vec::new());
    let (mut kw_unscoped, mut fts_empty) = (Vec::new(), Vec::new());
    let mut answerable_top1 = Vec::new();
    let (mut ev_recalls, mut ev_rrs) = (Vec::new(), Vec::new());
    let mut by_category: BTreeMap<String, Vec<f64>> = BTreeMap::new();
    let mut rows = Vec::new();
    for q in &gold.answerable {
        let qv_vec = emb.query(&q.question)?;
        let qv = QueryVector {
            vector: &qv_vec,
            model: emb.model(),
        };
        let args = scoped_args(&db, &q.question, q.scope.as_ref(), K);
        let hybrid = all
            .search_passages(&args, Some(qv))
            .with_context(|| format!("{}: search_passages", q.id))?;
        let keyword = all
            .search_passages(&args, None)
            .with_context(|| format!("{}: keyword search", q.id))?;
        let s = score(ids, q, &hybrid.passages);
        let kw = score(ids, q, &keyword.passages);
        let req_meetings: Vec<BTreeSet<String>> = q
            .must_cite
            .iter()
            .map(|r| {
                r.ids()
                    .iter()
                    .filter_map(|id| corpus.segment(id))
                    .map(|seg| db.meeting_ids[&seg.meeting].clone())
                    .collect()
            })
            .collect();
        let ev = score_evidence(ids, q, &hybrid.passages, &req_meetings);
        ev_recalls.push(ev.recall);
        ev_rrs.push(ev.rr);
        recalls.push(s.recall);
        rrs.push(s.rr);
        covered.push(if s.covered { 1.0 } else { 0.0 });
        kw_recalls.push(kw.recall);
        kw_rrs.push(kw.rr);
        if q.scope.is_none() {
            kw_unscoped.push(kw.recall);
            fts_empty.push(if keyword.passages.is_empty() {
                1.0
            } else {
                0.0
            });
        }
        let top1 = top1_cosine(&db, emb, &q.question, false)?;
        answerable_top1.push(top1);
        for c in &q.category {
            by_category.entry(c.clone()).or_default().push(s.recall);
        }
        rows.push(json!({
            "id": q.id,
            "recall": s.recall,
            "rank": s.rank,
            "covered": s.covered,
            "keyword_only_rank": kw.rank,
            "top1_cosine": top1,
            "evidence_rank": ev.rank,
            "passages": hybrid.passages.len(),
        }));
    }
    // Unanswerables: how well a top-1 cosine floor would separate them
    // from answerable questions (spec §4.2a #1), per kind.
    let mut unanswerable_rows = Vec::new();
    let mut by_kind: BTreeMap<UnanswerableKind, Vec<f64>> = BTreeMap::new();
    for u in &gold.unanswerable {
        let top1 = top1_cosine(&db, emb, &u.question, u.shareable_only())?;
        by_kind.entry(u.kind).or_default().push(top1);
        unanswerable_rows.push(json!({ "id": u.id, "kind": u.kind.as_str(), "top1_cosine": top1 }));
    }
    let all_unanswerable: Vec<f64> = by_kind.values().flatten().copied().collect();
    let sweep: Vec<Value> = (2..=14)
        .map(|i| {
            let floor = i as f64 * 0.05;
            let kept = answerable_top1.iter().filter(|c| **c >= floor).count();
            let blocked = |v: &[f64]| v.iter().filter(|c| **c < floor).count();
            let mut row = json!({
                "floor": (floor * 100.0).round() / 100.0,
                "answerable_kept": kept,
                "answerable": answerable_top1.len(),
            });
            for (k, v) in &by_kind {
                row[format!("{}_blocked", k.as_str())] = json!(blocked(v));
                row[k.as_str()] = json!(v.len());
            }
            row
        })
        .collect();

    // Keep-local sweep (spec §4.2a #13).
    let needles = LeakNeedles::new(&corpus, &db);
    let mut leaks: Vec<Value> = Vec::new();
    let mut checks = 0usize;
    let mut check = |what: String, haystack: String| {
        checks += 1;
        let found = needles.find(&haystack);
        if !found.is_empty() {
            leaks.push(json!({ "probe": what, "found": found.len() }));
        }
    };
    let questions: Vec<(&str, &str, Option<&Scope>)> = gold
        .answerable
        .iter()
        .map(|q| (q.id.as_str(), q.question.as_str(), q.scope.as_ref()))
        .chain(
            gold.unanswerable
                .iter()
                .map(|u| (u.id.as_str(), u.question.as_str(), None)),
        )
        .collect();
    for (id, question, scope) in questions {
        let v = emb.query(question)?;
        let qv = QueryVector {
            vector: &v,
            model: emb.model(),
        };
        for (label, vector) in [("hybrid", Some(qv)), ("keyword", None)] {
            // Wide limit: a leak anywhere in the candidate list counts.
            let r = shareable.search_passages(&scoped_args(&db, question, scope, 20), vector)?;
            check(format!("{id}:{label}"), serde_json::to_string(&r)?);
        }
    }
    for (i, p) in probes.search.iter().enumerate() {
        let mut args = scoped_args(&db, &p.query, None, 20);
        if let Some(f) = &p.folder {
            args.folder_id = Some(
                db.folder_ids
                    .get(f)
                    .with_context(|| format!("probe folder {f}"))?
                    .clone(),
            );
        }
        if let Some(ms) = &p.meetings {
            args.meeting_ids = Some(
                ms.iter()
                    .map(|m| db.meeting_ids.get(m).cloned().context("probe meeting"))
                    .collect::<Result<_>>()?,
            );
        }
        args.since = p.since.clone();
        args.until = p.until.clone();
        let v = emb.query(&p.query)?;
        for vector in [
            Some(QueryVector {
                vector: &v,
                model: emb.model(),
            }),
            None,
        ] {
            let out = match shareable.search_passages(&args, vector) {
                Ok(r) => serde_json::to_string(&r)?,
                Err(e) => e.to_string(),
            };
            check(format!("search[{i}]"), out);
        }
    }
    for (i, p) in probes.tool.iter().enumerate() {
        let args = db.substitute(&serde_json::to_value(&p.args)?)?;
        let v = emb.query(args.get("query").and_then(Value::as_str).unwrap_or("x"))?;
        let qv = (p.name == "search_passages").then_some(QueryVector {
            vector: &v,
            model: emb.model(),
        });
        let out = match call(&shareable, &p.name, &args, qv) {
            Ok(o) => format!("{}\n{}", o.json, o.text),
            Err(e) => e.to_string(),
        };
        check(format!("tool[{i}]:{}", p.name), out);
    }
    // The same tool calls under Visibility::All must reach the keep-local
    // meeting, or the sweep above proves nothing.
    let mut reachable = 0usize;
    for p in &probes.tool {
        let args = db.substitute(&serde_json::to_value(&p.args)?)?;
        if let Ok(o) = call(&all, &p.name, &args, None) {
            if !needles.find(&format!("{}\n{}", o.json, o.text)).is_empty() {
                reachable += 1;
            }
        }
    }
    if reachable == 0 {
        res.errors.push(
            "keep-local probes never reach the keep-local meeting under Visibility::All; \
             the leak sweep is vacuous"
                .into(),
        );
    }

    res.metrics.insert("recall_at_8".into(), mean(&recalls));
    res.metrics.insert("mrr_at_8".into(), mean(&rrs));
    res.metrics
        .insert("must_cite_coverage".into(), mean(&covered));
    res.metrics
        .insert("keep_local_leaks".into(), leaks.len() as f64);
    res.metrics
        .insert("answerable_questions".into(), gold.answerable.len() as f64);
    res.metrics.insert(
        "unanswerable_questions".into(),
        gold.unanswerable.len() as f64,
    );
    res.metrics
        .insert("keyword_only_recall_at_8".into(), mean(&kw_recalls));
    res.metrics
        .insert("keyword_only_mrr_at_8".into(), mean(&kw_rrs));
    // Report-only (no gate in thresholds.toml, so they can't block): note
    // passages of a must-cite meeting count as hits.
    res.metrics
        .insert("evidence_recall_at_8".into(), mean(&ev_recalls));
    res.metrics
        .insert("evidence_mrr_at_8".into(), mean(&ev_rrs));
    // Report-only: the keyword channel on unscoped questions (where the
    // FTS candidates come from) and how often it finds nothing at all.
    res.metrics.insert(
        "keyword_only_recall_at_8_unscoped".into(),
        mean(&kw_unscoped),
    );
    res.metrics
        .insert("fts_empty_rate".into(), mean(&fts_empty));
    res.metrics.insert(
        "cosine_floor_auroc".into(),
        auroc(&answerable_top1, &all_unanswerable),
    );
    for (k, v) in &by_kind {
        res.metrics.insert(
            format!("cosine_floor_auroc_{}", k.as_str()),
            auroc(&answerable_top1, v),
        );
    }
    res.info.insert(
        "recall_by_category".into(),
        json!(by_category
            .iter()
            .map(|(k, v)| (
                k.clone(),
                json!({ "questions": v.len(), "recall_at_8": mean(v) })
            ))
            .collect::<BTreeMap<_, _>>()),
    );
    res.info.insert("questions".into(), json!(rows));
    res.info
        .insert("unanswerable".into(), json!(unanswerable_rows));
    res.info.insert("cosine_floor_sweep".into(), json!(sweep));
    res.info.insert(
        "keep_local_sweep".into(),
        json!({ "checks": checks, "leaks": leaks, "probes_reaching_keep_local_under_all": reachable }),
    );
    res.info.insert(
        "config".into(),
        json!({
            "k": K,
            "embedder": emb.describe(),
            "chunks_embedded": indexed,
            "meetings": corpus.meetings.len(),
            "segments": corpus.segments().count(),
        }),
    );
    Ok(res)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_committed_gold_set_matches_the_fixtures() {
        let corpus = Corpus::load(&crate::evals_dir().join("fixtures")).unwrap();
        let gold = GoldSet::load(
            &crate::evals_dir()
                .join("suites")
                .join("retrieval")
                .join("questions.toml"),
        )
        .unwrap();
        gold.validate(&corpus).unwrap();
        assert!(gold.answerable.len() >= 30 && gold.unanswerable.len() >= 8);
    }

    #[test]
    fn a_mislabeled_question_is_rejected() {
        let corpus = Corpus::load(&crate::evals_dir().join("fixtures")).unwrap();
        let bad: GoldSet = toml::from_str(
            r#"
            unanswerable = []
            [[answerable]]
            id = "x"
            question = "q"
            must_cite = ["m01-05"]
            evidence = ["ninety two hundred"]
            answer = "a"
            category = []
            "#,
        )
        .unwrap();
        assert!(bad.validate(&corpus).is_err());
    }

    #[test]
    fn scoring() {
        let q: Answerable = toml::from_str(
            r#"
            id = "x"
            question = "q"
            must_cite = ["a", ["b", "c"]]
            evidence = ["e"]
            answer = "a"
            category = []
            "#,
        )
        .unwrap();
        let same: IdMap<'_> = &|s| Some(s.to_string());
        let passage = |ids: &[&str]| fly_context::Passage {
            source: fly_context::PassageSource::Transcript,
            meeting_id: None,
            note_id: String::new(),
            title: String::new(),
            started_at: None,
            start_ms: None,
            end_ms: None,
            segments: ids
                .iter()
                .map(|id| fly_context::SegmentView {
                    id: id.to_string(),
                    speaker: String::new(),
                    speaker_key: String::new(),
                    start_ms: 0,
                    end_ms: 0,
                    text: String::new(),
                })
                .collect(),
            text: String::new(),
            score: 0.0,
            matched_by: vec![],
        };
        let s = score(
            same,
            &q,
            &[passage(&["z"]), passage(&["c"]), passage(&["a"])],
        );
        assert_eq!((s.recall, s.rank, s.covered), (1.0, Some(2), true));
        assert_eq!(s.rr, 0.5);
        let s = score(same, &q, &[passage(&["a"])]);
        assert_eq!((s.recall, s.rank, s.covered), (0.5, Some(1), false));
        let mut many: Vec<_> = (0..K).map(|_| passage(&["z"])).collect();
        many.push(passage(&["a"]));
        let s = score(same, &q, &many);
        assert_eq!((s.recall, s.rank), (0.0, None), "only the top 8 count");

        // Evidence score: a note passage of a must-cite meeting is a hit;
        // the gating score ignores it.
        let mut note = passage(&[]);
        note.source = fly_context::PassageSource::Note;
        note.meeting_id = Some("M".into());
        let reqs = vec![
            ["M".to_string()].into_iter().collect::<BTreeSet<_>>(),
            BTreeSet::new(),
        ];
        let gate = score(same, &q, &[note.clone(), passage(&["b"])]);
        assert_eq!((gate.recall, gate.rank), (0.5, Some(2)));
        let ev = score_evidence(same, &q, &[note, passage(&["b"])], &reqs);
        assert_eq!((ev.recall, ev.rank, ev.covered), (1.0, Some(1), true));
    }
}
