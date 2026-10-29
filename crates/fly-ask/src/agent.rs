//! Tool execution for tiers M and C. The model may call fly-context's six
//! read tools and the two proposal tools; this module runs them under the
//! answer's visibility and scope:
//!
//! * every read goes through a `fly_context::Context` built with the
//!   answer's visibility (Keep local filtered in SQL, fail closed);
//! * model-supplied ids and filters are clamped to the question's scope
//!   (a meeting-scoped Ask can't read another meeting) — spec §4.2a #13;
//! * results are rendered by fly-context with meeting content's marker
//!   syntax defused, cut at a line boundary, and only the `[seg:ID]`
//!   markers that survive into the observation join the retrieved set;
//! * proposal tools go to the gate in `proposals.rs` and never write.

use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Mutex;

use chrono::{DateTime, Utc};
use fly_context::{
    render, Context, GetEventArgs, GetItemsArgs, GetNoteArgs, ListMeetingsArgs, PassageSource,
    QueryVector, ReadSegmentsArgs, SearchPassagesArgs, SegmentView,
};
use fly_llm::ToolSpec;
use fly_storage::Storage;
use serde::de::DeserializeOwned;
use serde_json::{json, Value};

use crate::events::{emit, AskEvent, EventSink, Stage};
use crate::evidence::{self, Evidence, SegmentInfo};
use crate::proposals::{self, Proposal, ProposalRefusal};
use crate::scope::Resolved;
use crate::store::StoreAccess;
use crate::text::{defuse_markers, take_markers};
use crate::QueryEmbedder;

const OUTSIDE_SCOPE: &str = "That is outside the meetings this question is about.";

/// The tools on offer (fly-context's read tools + proposals). Strict
/// schemas where the provider supports them for every tool.
pub fn tool_specs(caps: &fly_llm::Capabilities) -> Vec<ToolSpec> {
    let defs = fly_context::tool_defs();
    let n = defs.len() + 2;
    let strict = caps.strict_tools && (caps.max_strict_tools == 0 || caps.max_strict_tools >= n);
    let mut out: Vec<ToolSpec> = defs
        .into_iter()
        .map(|d| ToolSpec {
            name: d.name.into(),
            description: d.description.into(),
            input_schema: if strict {
                d.strict_input_schema
            } else {
                d.input_schema
            },
            strict,
        })
        .collect();
    out.extend(proposals::tool_specs());
    out
}

pub struct ToolState<'a, S, E> {
    pub store: &'a S,
    pub resolved: &'a Resolved,
    pub embedder: Option<&'a E>,
    pub sink: Option<EventSink<'a>>,
    pub observation_chars: usize,
    pub now: DateTime<Utc>,
    pub evidence: Mutex<Evidence>,
    pub proposals: Mutex<Vec<Proposal>>,
    pub refusals: Mutex<Vec<ProposalRefusal>>,
    /// The model ran a search itself (no deterministic re-query needed).
    pub searched: AtomicBool,
}

fn args<T: DeserializeOwned>(tool: &str, v: &Value) -> Result<T, String> {
    let v = if v.is_null() { json!({}) } else { v.clone() };
    serde_json::from_value(v).map_err(|e| format!("bad arguments for {tool}: {e}"))
}

/// Cut at a line boundary within `max` chars.
fn cut(s: &str, max: usize) -> String {
    if s.len() <= max {
        return s.to_string();
    }
    let mut end = 0;
    for (i, line) in s.split_inclusive('\n').scan(0usize, |pos, l| {
        let start = *pos;
        *pos += l.len();
        Some((start, l))
    }) {
        if i + line.len() > max {
            break;
        }
        end = i + line.len();
    }
    format!("{}… [more results cut]\n", &s[..end])
}

fn defuse_segments(segs: &mut [SegmentView]) {
    for s in segs {
        s.text = defuse_markers(&s.text);
        s.speaker = defuse_markers(&s.speaker);
    }
}

/// Segment views of a meeting by id.
fn meeting_segments(s: &Storage, meeting_id: &str) -> HashMap<String, SegmentView> {
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

impl<S: StoreAccess, E: QueryEmbedder> ToolState<'_, S, E> {
    pub fn new<'a>(
        store: &'a S,
        resolved: &'a Resolved,
        embedder: Option<&'a E>,
        sink: Option<EventSink<'a>>,
        observation_chars: usize,
        now: DateTime<Utc>,
        evidence: Evidence,
    ) -> ToolState<'a, S, E> {
        ToolState {
            store,
            resolved,
            embedder,
            sink,
            observation_chars,
            now,
            evidence: Mutex::new(evidence),
            proposals: Mutex::new(Vec::new()),
            refusals: Mutex::new(Vec::new()),
            searched: AtomicBool::new(false),
        }
    }

    fn ctx_scope_ids(&self, asked: Option<Vec<String>>) -> Option<Vec<String>> {
        match (&self.resolved.meeting_ids, asked) {
            (None, asked) => asked,
            (Some(scope), None) => Some(scope.clone()),
            (Some(scope), Some(asked)) => {
                let both: Vec<String> = asked.into_iter().filter(|a| scope.contains(a)).collect();
                Some(if both.is_empty() { scope.clone() } else { both })
            }
        }
    }

    /// Register the segments whose markers made it into `observation`.
    fn register(&self, observation: &str, found: &[SegmentInfo]) {
        let (_, ids) = take_markers(observation);
        let mut ev = self.evidence.lock().unwrap_or_else(|p| p.into_inner());
        for id in ids {
            if let Some(info) = found.iter().find(|f| f.id == id) {
                ev.insert(info.clone());
            }
        }
    }

    fn finish(&self, text: String, found: Vec<SegmentInfo>) -> Result<String, String> {
        let obs = cut(&text, self.observation_chars.max(500));
        self.register(&obs, &found);
        Ok(obs)
    }

    /// Run one tool call.
    pub async fn call(&self, name: String, input: Value) -> Result<String, String> {
        emit(
            self.sink,
            AskEvent::Stage {
                stage: Stage::Reading,
            },
        );
        // (The tool loop answers calls to tools that weren't offered
        // without executing anything.)
        if proposals::ProposalTool::parse(&name).is_some() {
            return self.propose(&name, &input);
        }
        if name == fly_context::tools::SEARCH_PASSAGES {
            let mut a: SearchPassagesArgs = args(&name, &input)?;
            a.meeting_ids = self.ctx_scope_ids(a.meeting_ids.take());
            if let Some(f) = &self.resolved.folder_id {
                a.folder_id = Some(f.clone());
            }
            self.searched.store(true, Ordering::Relaxed);
            let vector = match self.embedder {
                Some(e) => e.embed(a.query.trim()).await,
                None => None,
            };
            let model = self.embedder.map(|e| e.model().to_string());
            return self.store.with(|s| {
                let ctx = Context::new(s, self.resolved.visibility);
                let qv = vector
                    .as_ref()
                    .zip(model.as_deref())
                    .map(|(v, m)| QueryVector {
                        vector: v,
                        model: m,
                    });
                let mut r = ctx.search_passages(&a, qv).map_err(|e| e.to_string())?;
                let scope_notes: Vec<&str> = self
                    .resolved
                    .meetings
                    .iter()
                    .map(|m| m.note_id.as_str())
                    .collect();
                r.passages.retain(|p| match (&p.source, &p.meeting_id) {
                    (PassageSource::Transcript, Some(mid)) => self.resolved.allows_meeting(mid),
                    (PassageSource::Note, _) => {
                        self.resolved.meeting_ids.is_none()
                            || scope_notes.contains(&p.note_id.as_str())
                    }
                    _ => false,
                });
                let mut found = Vec::new();
                for p in &mut r.passages {
                    defuse_segments(&mut p.segments);
                    p.text = defuse_markers(&p.text);
                    p.title = defuse_markers(&p.title);
                    if let Some(mid) = &p.meeting_id {
                        for sv in &p.segments {
                            found.push(evidence::info(sv, mid, &p.title, p.started_at));
                        }
                    }
                }
                self.finish(render::passages(&r), found)
            });
        }
        self.store.with(|s| self.read(s, &name, &input))
    }

    fn read(&self, s: &Storage, name: &str, input: &Value) -> Result<String, String> {
        let ctx = Context::new(s, self.resolved.visibility);
        let e = |e: fly_context::ContextError| e.to_string();
        match name {
            fly_context::tools::READ_SEGMENTS => {
                let a: ReadSegmentsArgs = args(name, input)?;
                if !self.resolved.allows_meeting(a.meeting_id.trim()) {
                    return Err(OUTSIDE_SCOPE.into());
                }
                let mut w = ctx.read_segments(&a).map_err(e)?;
                defuse_segments(&mut w.segments);
                let started = self.resolved.meeting(&w.meeting_id).map(|m| m.started_at);
                let found = w
                    .segments
                    .iter()
                    .map(|sv| evidence::info(sv, &w.meeting_id, &w.title, started))
                    .collect();
                self.finish(render::segments(&w), found)
            }
            fly_context::tools::LIST_MEETINGS => {
                let mut a: ListMeetingsArgs = args(name, input)?;
                if let Some(f) = &self.resolved.folder_id {
                    a.folder_id = Some(f.clone());
                }
                let mut l = ctx.list_meetings(&a).map_err(e)?;
                l.meetings
                    .retain(|m| self.resolved.allows_meeting(&m.meeting_id));
                for m in &mut l.meetings {
                    m.title = defuse_markers(&m.title);
                }
                self.finish(render::meetings(&l), vec![])
            }
            fly_context::tools::GET_EVENT => {
                let a: GetEventArgs = args(name, input)?;
                if !self.resolved.allows_meeting(a.meeting_id.trim()) {
                    return Err(OUTSIDE_SCOPE.into());
                }
                let mut ev = ctx.get_event(&a).map_err(e)?;
                ev.title = defuse_markers(&ev.title);
                self.finish(render::event(&ev), vec![])
            }
            fly_context::tools::GET_NOTE => {
                let raw: GetNoteArgs = args(name, input)?;
                // Normalize exactly as fly-context does (trimmed, empty =
                // absent), check, and pass on only the normalized form.
                let norm = |s: &Option<String>| {
                    s.as_deref()
                        .map(str::trim)
                        .filter(|s| !s.is_empty())
                        .map(str::to_string)
                };
                let a = GetNoteArgs {
                    note_id: norm(&raw.note_id),
                    meeting_id: norm(&raw.meeting_id),
                    kind: raw.kind,
                };
                let note_ok = |nid: &str| {
                    self.resolved.note_id.as_deref() == Some(nid)
                        || self.resolved.meetings.iter().any(|m| m.note_id == nid)
                };
                let allowed = match (a.note_id.as_deref(), a.meeting_id.as_deref()) {
                    (Some(n), None) => note_ok(n),
                    (None, Some(m)) => self.resolved.allows_meeting(m),
                    _ => return Err("Pass exactly one of note_id or meeting_id.".into()),
                };
                if !allowed {
                    return Err(OUTSIDE_SCOPE.into());
                }
                let mut n = ctx.get_note(&a).map_err(e)?;
                n.scratchpad = n.scratchpad.map(|t| defuse_markers(&t));
                let mut found = Vec::new();
                if let Some(blocks) = &mut n.enhanced {
                    let mut segs: HashMap<String, HashMap<String, SegmentView>> = HashMap::new();
                    for b in blocks.iter_mut() {
                        b.markdown = defuse_markers(&b.markdown);
                        for id in &b.source_segment_ids {
                            for m in self
                                .resolved
                                .meetings
                                .iter()
                                .filter(|m| m.note_id == n.note_id)
                            {
                                let map = segs
                                    .entry(m.id.clone())
                                    .or_insert_with(|| meeting_segments(s, &m.id));
                                if let Some(sv) = map.get(id) {
                                    found.push(evidence::info(
                                        sv,
                                        &m.id,
                                        &m.title,
                                        Some(m.started_at),
                                    ));
                                }
                            }
                        }
                    }
                }
                self.finish(render::note(&n), found)
            }
            fly_context::tools::GET_ITEMS => {
                let mut a: GetItemsArgs = args(name, input)?;
                a.meeting_ids = self
                    .ctx_scope_ids(a.meeting_ids.take())
                    .filter(|ids| ids.len() <= fly_context::tools::MAX_MEETING_IDS);
                let mut l = ctx.get_items(&a).map_err(e)?;
                l.items
                    .retain(|i| self.resolved.allows_meeting(&i.meeting_id));
                let mut found = Vec::new();
                let mut segs: HashMap<String, HashMap<String, SegmentView>> = HashMap::new();
                for i in &mut l.items {
                    i.text = defuse_markers(&i.text);
                    let map = segs
                        .entry(i.meeting_id.clone())
                        .or_insert_with(|| meeting_segments(s, &i.meeting_id));
                    let m = self.resolved.meeting(&i.meeting_id);
                    for id in &i.segment_ids {
                        if let (Some(sv), Some(m)) = (map.get(id), m) {
                            found.push(evidence::info(sv, &m.id, &m.title, Some(m.started_at)));
                        }
                    }
                }
                self.finish(render::items(&l), found)
            }
            _ => Err(format!(
                "There is no tool named {:?}.",
                name.chars().take(40).collect::<String>()
            )),
        }
    }

    fn propose(&self, name: &str, input: &Value) -> Result<String, String> {
        let ev = self
            .evidence
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .clone();
        let existing = self
            .proposals
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .clone();
        let people = |mid: &str| self.store.with(|s| crate::scope::meeting_people(s, mid));
        let res = proposals::check(name, input, &ev, self.resolved, people, &existing, self.now);
        let ack = proposals::ack(&res);
        match res {
            Ok(p) => self
                .proposals
                .lock()
                .unwrap_or_else(|p| p.into_inner())
                .push(p),
            Err(r) => self
                .refusals
                .lock()
                .unwrap_or_else(|p| p.into_inner())
                .push(r),
        }
        ack
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cut_keeps_whole_lines() {
        let s = "[seg:a] one\n[seg:b] two two two\n[seg:c] three\n";
        let c = cut(s, 34);
        assert!(c.starts_with("[seg:a] one\n[seg:b] two two two\n"), "{c}");
        assert!(!c.contains("[seg:c]"));
        assert_eq!(cut("short", 30), "short");
    }
}
