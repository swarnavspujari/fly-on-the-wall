//! What a question is about (spec §4.2 step 1): this meeting, a note,
//! a folder, a person, or everything — resolved against what the model may
//! see.
//!
//! Visibility comes from the caller's `on_device` (the app's
//! `build_provider` result), never from `provider.is_local()`: an Ollama
//! at a remote URL is local in kind but sends data off this machine. Under
//! `Visibility::ShareableOnly`, Keep-local meetings are excluded inside the
//! storage queries (fly-context) and by a fail-closed check here; a narrow
//! scope that is itself Keep local is refused. The number of excluded
//! meetings is computed for the user's trace only — it never goes into a
//! prompt or a tool result.

use std::collections::{HashMap, HashSet};

use chrono::{DateTime, Utc};
use fly_context::Visibility;
use fly_storage::{ItemFilter, MeetingFilter, Storage};
use serde::{Deserialize, Serialize};

use crate::router::Known;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", content = "id", rename_all = "snake_case")]
pub enum AskScope {
    /// Every meeting the model may see.
    All,
    /// One meeting.
    Meeting(String),
    /// A note: every recording it holds, plus the user's notes.
    Note(String),
    /// Notes filed directly in a folder.
    Folder(String),
    /// Meetings a person attended or spoke in (a name or speaker label).
    Person(String),
}

/// Typed scope kind (run log).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ScopeKind {
    All,
    Meeting,
    Note,
    Folder,
    Person,
}

/// A meeting the question may read.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MeetingRef {
    pub id: String,
    pub note_id: String,
    pub title: String,
    pub started_at: DateTime<Utc>,
}

#[derive(Debug, Clone)]
pub struct Resolved {
    pub kind: ScopeKind,
    pub visibility: Visibility,
    /// Meetings the search is limited to; `None` = every visible meeting.
    pub meeting_ids: Option<Vec<String>>,
    pub folder_id: Option<String>,
    /// The note whose own notes join the prefetch (meeting/note scope).
    pub note_id: Option<String>,
    /// More than one note's worth (router date/people filters apply).
    pub wide: bool,
    /// Keep-local meetings the scope would have covered but this view can't
    /// see. For the user's trace only.
    pub kept_local_excluded: usize,
    pub known: Known,
    /// Visible meetings in scope, newest first.
    pub meetings: Vec<MeetingRef>,
    /// Folder scope: meetings filed in its subfolders (searched by id;
    /// `folder_id` itself matches only notes filed directly in it).
    pub sub_meeting_ids: Vec<String>,
}

impl Resolved {
    pub fn meeting(&self, id: &str) -> Option<&MeetingRef> {
        self.meetings.iter().find(|m| m.id == id)
    }

    /// Whether a meeting is inside this scope (and visible).
    pub fn allows_meeting(&self, id: &str) -> bool {
        self.meeting(id).is_some()
    }
}

/// Why a scope can't be searched.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ScopeRefusal {
    /// The note/meeting is Keep local and the model isn't on this device.
    KeepLocal,
    /// The meeting/note/folder/person doesn't exist (or isn't visible).
    NotFound,
    /// Keep-local status couldn't be read, so nothing is shared.
    Unchecked,
}

struct Hidden {
    meetings: HashSet<String>,
    notes: HashSet<String>,
}

impl Hidden {
    fn hides(&self, m: &fly_core::Meeting) -> bool {
        self.meetings.contains(&m.id) || self.notes.contains(&m.note_id)
    }
}

fn hidden(s: &Storage, vis: Visibility) -> Result<Hidden, ScopeRefusal> {
    if vis == Visibility::All {
        return Ok(Hidden {
            meetings: HashSet::new(),
            notes: HashSet::new(),
        });
    }
    Ok(Hidden {
        meetings: s
            .keep_local_meeting_ids()
            .map_err(|_| ScopeRefusal::Unchecked)?,
        notes: s
            .keep_local_note_ids()
            .map_err(|_| ScopeRefusal::Unchecked)?,
    })
}

fn all_meetings(
    s: &Storage,
    folder: Option<&str>,
    exclude_keep_local: bool,
) -> Result<Vec<fly_core::Meeting>, ScopeRefusal> {
    s.list_meetings_filtered(&MeetingFilter {
        folder_id: folder.map(str::to_string),
        exclude_keep_local,
        limit: 1_000_000,
        ..Default::default()
    })
    .map_err(|_| ScopeRefusal::Unchecked)
}

fn mref(m: &fly_core::Meeting) -> MeetingRef {
    MeetingRef {
        id: m.id.clone(),
        note_id: m.note_id.clone(),
        title: m.title.clone(),
        started_at: m.started_at,
    }
}

/// Most transcripts read to learn speaker labels (narrow scopes, person).
const LABEL_SCAN: usize = 300;
/// Most meetings a person scope keeps (search_passages takes ≤ 50 ids).
const PERSON_MEETINGS: usize = 50;

fn labels(s: &Storage, meeting_id: &str) -> Vec<String> {
    s.get_transcript(meeting_id)
        .ok()
        .flatten()
        .map(|t| t.speakers.iter().map(|sp| sp.label.clone()).collect())
        .unwrap_or_default()
}

fn person_matches(name: &str, candidate: &str) -> bool {
    let (a, b) = (name.trim().to_lowercase(), candidate.trim().to_lowercase());
    !a.is_empty() && !b.is_empty() && (a == b || b.split_whitespace().next() == Some(a.as_str()))
}

pub fn resolve(s: &Storage, scope: &AskScope, vis: Visibility) -> Result<Resolved, ScopeRefusal> {
    let shareable = vis == Visibility::ShareableOnly;
    let hidden = hidden(s, vis)?;
    let visible: Vec<fly_core::Meeting> = all_meetings(s, None, shareable)?
        .into_iter()
        .filter(|m| !hidden.hides(m))
        .collect();
    let mut known = Known {
        earliest: visible.iter().map(|m| m.started_at).min(),
        folders: s
            .list_folders()
            .map(|fs| fs.into_iter().map(|f| (f.id, f.name)).collect())
            .unwrap_or_default(),
        people: Vec::new(),
    };
    let add_person = |known: &mut Known, p: &str| {
        let p = p.trim();
        if !p.is_empty() && !known.people.iter().any(|k| k.eq_ignore_ascii_case(p)) {
            known.people.push(p.to_string());
        }
    };

    let mut folder_tree: Vec<String> = Vec::new();
    let mut sub_meeting_ids: Vec<String> = Vec::new();
    let (kind, in_scope, folder_id, note_id): (ScopeKind, Vec<&fly_core::Meeting>, _, _) =
        match scope {
            AskScope::All => (ScopeKind::All, visible.iter().collect(), None, None),
            AskScope::Meeting(id) => {
                let m = s.get_meeting(id).map_err(|_| ScopeRefusal::NotFound)?;
                if shareable && (s.meeting_keep_local(&m.id) || s.note_keep_local(&m.note_id)) {
                    return Err(ScopeRefusal::KeepLocal);
                }
                let v: Vec<&fly_core::Meeting> = visible.iter().filter(|x| x.id == m.id).collect();
                if v.is_empty() {
                    return Err(ScopeRefusal::KeepLocal);
                }
                (ScopeKind::Meeting, v, None, Some(m.note_id.clone()))
            }
            AskScope::Note(id) => {
                s.get_note(id).map_err(|_| ScopeRefusal::NotFound)?;
                if shareable && s.note_keep_local(id) {
                    return Err(ScopeRefusal::KeepLocal);
                }
                let v: Vec<&fly_core::Meeting> =
                    visible.iter().filter(|m| m.note_id == *id).collect();
                (ScopeKind::Note, v, None, Some(id.clone()))
            }
            AskScope::Folder(id) => {
                if !known.folders.iter().any(|(f, _)| f == id) {
                    return Err(ScopeRefusal::NotFound);
                }
                // The folder and every folder under it.
                folder_tree = descendants(s, id);
                let mut direct: HashSet<String> = HashSet::new();
                let mut ids: HashSet<String> = HashSet::new();
                for (n, f) in folder_tree.iter().enumerate() {
                    for m in all_meetings(s, Some(f), shareable)? {
                        if n == 0 {
                            direct.insert(m.id.clone());
                        }
                        ids.insert(m.id);
                    }
                }
                let v: Vec<&fly_core::Meeting> =
                    visible.iter().filter(|m| ids.contains(&m.id)).collect();
                sub_meeting_ids = v
                    .iter()
                    .filter(|m| !direct.contains(&m.id))
                    .map(|m| m.id.clone())
                    .collect();
                (ScopeKind::Folder, v, Some(id.clone()), None)
            }
            AskScope::Person(name) => {
                let mut v: Vec<&fly_core::Meeting> = Vec::new();
                for (scanned, m) in visible.iter().enumerate() {
                    if v.len() >= PERSON_MEETINGS {
                        break;
                    }
                    let attended = m.attendees.iter().any(|a| {
                        person_matches(name, a.display_name()) || person_matches(name, &a.name)
                    });
                    let spoke = !attended
                        && scanned < LABEL_SCAN
                        && labels(s, &m.id).iter().any(|l| person_matches(name, l));
                    if attended || spoke {
                        v.push(m);
                    }
                }
                if v.is_empty() {
                    return Err(ScopeRefusal::NotFound);
                }
                (ScopeKind::Person, v, None, None)
            }
        };

    let wide = matches!(kind, ScopeKind::All | ScopeKind::Folder | ScopeKind::Person);
    for m in &in_scope {
        for a in &m.attendees {
            add_person(&mut known, a.display_name());
        }
    }
    if !wide {
        for m in in_scope.iter().take(LABEL_SCAN) {
            for l in labels(s, &m.id) {
                add_person(&mut known, &l);
            }
        }
    }
    let scope_ids: Option<Vec<String>> = match kind {
        ScopeKind::All => None,
        ScopeKind::Folder => None, // folder_id filters the search itself
        _ => Some(in_scope.iter().map(|m| m.id.clone()).collect()),
    };
    let item_ids: Option<Vec<String>> =
        (kind != ScopeKind::All).then(|| in_scope.iter().map(|m| m.id.clone()).collect());
    if let Ok(items) = s.query_items(&ItemFilter {
        meeting_ids: item_ids,
        exclude_keep_local: shareable,
        limit: 300,
        ..Default::default()
    }) {
        for i in items {
            if let Some(o) = &i.owner {
                add_person(&mut known, o);
            }
        }
    }
    if let AskScope::Person(name) = scope {
        add_person(&mut known, name);
    }

    // The user's "N kept-local meetings not searched" (wide scopes only;
    // a narrow keep-local scope was refused above).
    let kept_local_excluded = if shareable && wide {
        match kind {
            ScopeKind::Folder => {
                let mut n = 0;
                for f in &folder_tree {
                    n += all_meetings(s, Some(f), false)?
                        .iter()
                        .filter(|m| hidden.hides(m))
                        .count();
                }
                n
            }
            ScopeKind::All => all_meetings(s, None, false)?
                .iter()
                .filter(|m| hidden.hides(m))
                .count(),
            _ => 0,
        }
    } else {
        0
    };

    let meetings: Vec<MeetingRef> = in_scope.iter().map(|m| mref(m)).collect();
    Ok(Resolved {
        kind,
        visibility: vis,
        meeting_ids: scope_ids,
        folder_id,
        note_id,
        wide,
        kept_local_excluded,
        known,
        meetings,
        sub_meeting_ids,
    })
}

/// `root` and every folder below it, root first.
fn descendants(s: &Storage, root: &str) -> Vec<String> {
    let all = s.list_folders().unwrap_or_default();
    let mut out = vec![root.to_string()];
    let mut i = 0;
    while i < out.len() {
        let parent = out[i].clone();
        for f in &all {
            if f.parent_id.as_deref() == Some(parent.as_str()) && !out.contains(&f.id) {
                out.push(f.id.clone());
            }
        }
        i += 1;
    }
    out
}

/// Speaker labels and attendee names of one meeting (proposal owners).
pub fn meeting_people(s: &Storage, meeting_id: &str) -> Vec<String> {
    let mut out = labels(s, meeting_id);
    if let Ok(m) = s.get_meeting(meeting_id) {
        out.extend(m.attendees.iter().map(|a| a.display_name().to_string()));
    }
    out.retain(|p| !p.trim().is_empty());
    out
}

/// Meeting id → title for items rendering.
pub fn titles(r: &Resolved) -> HashMap<String, String> {
    r.meetings
        .iter()
        .map(|m| (m.id.clone(), m.title.clone()))
        .collect()
}
