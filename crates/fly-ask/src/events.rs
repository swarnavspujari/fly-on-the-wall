//! Events streamed to the UI while an answer is built (spec §4.2a #10:
//! trace → citations → answer). These carry content and go only to this
//! machine's UI — never to the run log.

use serde::Serialize;

use crate::evidence::SegmentInfo;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Stage {
    Scoping,
    Searching,
    Answering,
    /// The model is using a tool (tiers M/C).
    Reading,
    Checking,
    Retrying,
}

#[derive(Debug, Clone, Serialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum AskEvent {
    Stage {
        stage: Stage,
    },
    /// The prefetched lines, the moment retrieval finishes.
    Sources {
        sources: Vec<SegmentInfo>,
        /// Keep-local meetings this provider can't see ("N kept-local
        /// meetings not searched").
        kept_local_not_searched: usize,
    },
    /// A model turn started; text from an earlier turn that ended in a
    /// tool call is superseded.
    TurnStart {
        turn: u32,
    },
    TextDelta {
        turn: u32,
        text: String,
    },
}

/// Where events go (the app forwards them to the webview).
pub type EventSink<'a> = &'a (dyn Fn(AskEvent) + Send + Sync);

pub(crate) fn emit(sink: Option<EventSink<'_>>, ev: AskEvent) {
    if let Some(s) = sink {
        s(ev);
    }
}
