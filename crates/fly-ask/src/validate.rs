//! Validation (spec §4.2 step 5): what the user sees is decided here, by
//! deterministic checks, never by the model's own account.
//!
//! * A `[seg:ID]` counts only if the id is in the retrieved set (what the
//!   model was shown). Anything else is dropped from the citations.
//! * A sentence with no valid citation is **unsourced** — or **flagged**
//!   when it states a number, date or quote (an uncited claim).
//! * A cited sentence whose numbers, dates, names or quotes aren't in the
//!   lines it cites is **flagged** (the claim check).
//! * Links, images and HTML are removed from what's displayed.

use serde::{Deserialize, Serialize};

use crate::claims::{self, ClaimKind, Source};
use crate::evidence::Evidence;
use crate::text;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SentenceStatus {
    /// Every claim checked against valid citations.
    Sourced,
    /// No valid citation.
    Unsourced,
    /// Cited but a claim failed, or an uncited claim.
    Flagged,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum FlagKind {
    /// Cited only ids it wasn't shown (or none at all) and states a claim.
    UncitedClaim,
    /// Cited an id that wasn't in the retrieved set.
    InvalidCitation,
    Number,
    Date,
    Name,
    Quote,
}

impl From<ClaimKind> for FlagKind {
    fn from(k: ClaimKind) -> Self {
        match k {
            ClaimKind::Number => FlagKind::Number,
            ClaimKind::Date => FlagKind::Date,
            ClaimKind::Name => FlagKind::Name,
            ClaimKind::Quote => FlagKind::Quote,
        }
    }
}

/// A reason a sentence isn't shown as sourced. `detail` is answer text
/// for the UI ("84000", "marcus"); it never enters the run log.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Flag {
    pub kind: FlagKind,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub detail: Option<String>,
}

/// One sentence as the user sees it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AnswerSentence {
    /// Display Markdown, markers and links removed.
    pub text: String,
    /// Valid citations (retrieved segment ids), in order.
    pub citations: Vec<String>,
    pub status: SentenceStatus,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub flags: Vec<Flag>,
}

/// Counts for the run log.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct ValidationStats {
    pub sentences: u32,
    pub sourced: u32,
    pub unsourced: u32,
    pub flagged: u32,
    pub valid_citations: u32,
    pub invalid_citations: u32,
    pub links_removed: u32,
    pub claim_numbers: u32,
    pub claim_dates: u32,
    pub claim_names: u32,
    pub claim_quotes: u32,
}

#[derive(Debug, Clone, Default)]
pub struct Validated {
    pub sentences: Vec<AnswerSentence>,
    pub stats: ValidationStats,
    /// The reply was empty after removing reasoning tags.
    pub empty: bool,
    /// A "not found" reply: some sentence refuses and none is sourced.
    pub refusal: bool,
}

impl Validated {
    pub fn any_valid_citation(&self) -> bool {
        self.sentences.iter().any(|s| !s.citations.is_empty())
    }

    pub fn sourced(&self) -> usize {
        self.sentences
            .iter()
            .filter(|s| s.status == SentenceStatus::Sourced)
            .count()
    }
}

pub fn validate(reply: &str, ev: &Evidence) -> Validated {
    let clean = text::strip_reasoning(reply);
    if clean.trim().is_empty() {
        return Validated {
            empty: true,
            ..Default::default()
        };
    }
    let mut out = Validated::default();
    for raw in text::sentences(&clean) {
        let (display, removed) = text::strip_links(&raw.display);
        out.stats.links_removed += removed as u32;
        // A bare bracketed id the model was shown (`[m07-14]`) cites too.
        let (display, bare) = text::take_bare_ids(&display, |id| ev.contains(id));
        let display = display.trim().to_string();
        if display.chars().all(|c| !c.is_alphanumeric()) {
            continue;
        }
        let mut markers = raw.markers.clone();
        for id in bare {
            if !markers.contains(&id) {
                markers.push(id);
            }
        }
        let (valid, invalid): (Vec<String>, Vec<String>) =
            markers.into_iter().partition(|id| ev.contains(id));
        out.stats.invalid_citations += invalid.len() as u32;
        let mut flags = Vec::new();
        if !invalid.is_empty() {
            flags.push(Flag {
                kind: FlagKind::InvalidCitation,
                detail: None,
            });
        }
        let status = if valid.is_empty() {
            if claims::has_claims(&display) {
                flags.push(Flag {
                    kind: FlagKind::UncitedClaim,
                    detail: None,
                });
                SentenceStatus::Flagged
            } else {
                SentenceStatus::Unsourced
            }
        } else {
            let sources: Vec<Source<'_>> = valid
                .iter()
                .filter_map(|id| ev.get(id))
                .map(|s| Source {
                    text: &s.text,
                    speaker: &s.speaker,
                })
                .collect();
            let failures = claims::unsupported(&display, &sources);
            for f in &failures {
                match f.kind {
                    ClaimKind::Number => out.stats.claim_numbers += 1,
                    ClaimKind::Date => out.stats.claim_dates += 1,
                    ClaimKind::Name => out.stats.claim_names += 1,
                    ClaimKind::Quote => out.stats.claim_quotes += 1,
                }
                flags.push(Flag {
                    kind: f.kind.into(),
                    detail: Some(f.value.clone()),
                });
            }
            if failures.is_empty() {
                SentenceStatus::Sourced
            } else {
                SentenceStatus::Flagged
            }
        };
        match status {
            SentenceStatus::Sourced => out.stats.sourced += 1,
            SentenceStatus::Unsourced => out.stats.unsourced += 1,
            SentenceStatus::Flagged => out.stats.flagged += 1,
        }
        out.stats.valid_citations += valid.len() as u32;
        out.sentences.push(AnswerSentence {
            text: display,
            citations: valid,
            status,
            flags,
        });
    }
    out.stats.sentences = out.sentences.len() as u32;
    out.empty = out.sentences.is_empty();
    out.refusal = out.sourced() == 0 && out.sentences.iter().any(|s| text::is_refusal(&s.text));
    out
}

/// Every sentence marked flagged (an answer that still cites nothing after
/// its retry is shown fully flagged).
pub fn flag_all(v: &mut Validated) {
    for s in &mut v.sentences {
        if s.status != SentenceStatus::Flagged {
            s.status = SentenceStatus::Flagged;
            if !s.flags.iter().any(|f| f.kind == FlagKind::UncitedClaim) {
                s.flags.push(Flag {
                    kind: FlagKind::UncitedClaim,
                    detail: None,
                });
            }
        }
    }
    let n = v.sentences.len() as u32;
    v.stats.flagged = n;
    v.stats.sourced = 0;
    v.stats.unsourced = 0;
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::evidence::SegmentInfo;

    fn ev() -> Evidence {
        let mut e = Evidence::default();
        e.insert(SegmentInfo {
            id: "m01-05".into(),
            meeting_id: "m1".into(),
            meeting_title: "Launch".into(),
            started_at: None,
            speaker: "Ines Carvalho".into(),
            speaker_key: "spk_0".into(),
            start_ms: 0,
            end_ms: 1,
            text: "All in, I'm asking for eighty four hundred dollars for the first month.".into(),
        });
        e
    }

    #[test]
    fn valid_cites_pass_the_claim_check() {
        let v = validate("Ines asked for $8,400 [seg:m01-05].", &ev());
        assert_eq!(v.sentences[0].status, SentenceStatus::Sourced);
        assert_eq!(v.sentences[0].citations, vec!["m01-05"]);
        assert_eq!(v.sentences[0].text, "Ines asked for $8,400.");
    }

    #[test]
    fn corrupted_numbers_unknown_ids_and_links_are_caught() {
        let v = validate("Ines asked for $84k [seg:m01-05].", &ev());
        assert_eq!(v.sentences[0].status, SentenceStatus::Flagged);
        assert_eq!(v.sentences[0].flags[0].kind, FlagKind::Number);

        let v = validate("Harrowgate pays forty two thousand [seg:m04-99].", &ev());
        assert_eq!(
            v.sentences[0].status,
            SentenceStatus::Flagged,
            "uncited claim"
        );
        assert!(v.sentences[0].citations.is_empty());
        assert_eq!(v.stats.invalid_citations, 1);

        let v = validate(
            "They want offline mode [seg:x]. See ![l](https://e.example/p.png).",
            &ev(),
        );
        assert_eq!(v.sentences[0].status, SentenceStatus::Unsourced);
        assert!(v
            .sentences
            .iter()
            .all(|s| crate::text::strip_links(&s.text).1 == 0));
        assert_eq!(v.stats.links_removed, 1);
    }

    #[test]
    fn refusals_and_empty_replies_differ() {
        let v = validate("I couldn't find that in your meetings.", &ev());
        assert!(v.refusal && !v.empty);
        let v = validate("<think>hmm</think>  ", &ev());
        assert!(v.empty && !v.refusal);
        // A sourced sentence beside a partial refusal is an answer.
        let v = validate(
            "Ines asked for eighty four hundred dollars [seg:m01-05]. I couldn't find the cap.",
            &ev(),
        );
        assert!(!v.refusal);
    }

    #[test]
    fn flag_all_marks_everything() {
        let mut v = validate("Something happened. Then more.", &ev());
        flag_all(&mut v);
        assert!(v
            .sentences
            .iter()
            .all(|s| s.status == SentenceStatus::Flagged));
    }
}
