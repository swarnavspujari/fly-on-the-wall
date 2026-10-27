//! Streaming plumbing shared by the providers: byte → line framing (SSE and
//! NDJSON), an adapter from an HTTP body to a [`ChatStream`], a replay of a
//! finished response as events, and [`collect`] to fold events back into a
//! [`ChatResponse`].
//!
//! Decoders are pure (`&str` line in, events out) so every provider's wire
//! format is unit-tested from recorded fixtures without a network.

use std::collections::{HashMap, VecDeque};
use std::fmt::Display;

use futures_util::{Stream, StreamExt};

use crate::conversation::{ChatResponse, ContentBlock, StopReason, StreamEvent, Usage};
use crate::{ChatStream, LlmError, Result};

/// Splits a byte stream into complete lines. Bytes are decoded only once a
/// whole line has arrived, so a UTF-8 character split across network chunks
/// survives.
#[derive(Debug, Default)]
pub struct LineBuffer {
    buf: Vec<u8>,
}

impl LineBuffer {
    /// Feed bytes; returns every line completed by them (without `\r\n`).
    pub fn push(&mut self, bytes: &[u8]) -> Vec<String> {
        self.buf.extend_from_slice(bytes);
        let mut lines = Vec::new();
        while let Some(pos) = self.buf.iter().position(|b| *b == b'\n') {
            let mut line: Vec<u8> = self.buf.drain(..=pos).collect();
            line.pop();
            if line.last() == Some(&b'\r') {
                line.pop();
            }
            lines.push(String::from_utf8_lossy(&line).into_owned());
        }
        lines
    }

    /// The unterminated tail, if any (a body that doesn't end in `\n`).
    pub fn finish(&mut self) -> Option<String> {
        if self.buf.is_empty() {
            return None;
        }
        let tail = String::from_utf8_lossy(&self.buf).trim_end().to_string();
        self.buf.clear();
        (!tail.is_empty()).then_some(tail)
    }
}

/// The payload of an SSE `data:` line (`event:`, comments and blanks are
/// `None` — both SSE APIs here repeat the event type inside the JSON).
pub fn sse_data(line: &str) -> Option<&str> {
    line.strip_prefix("data:")
        .map(|d| d.strip_prefix(' ').unwrap_or(d))
}

/// A wire-format decoder: fed complete lines, yields events.
pub(crate) trait LineDecoder: Send + 'static {
    fn line(&mut self, line: &str) -> Result<Vec<StreamEvent>>;
    /// End of body. Must have produced `Done` by now, else it's an error
    /// (a cut connection must not look like a finished answer).
    fn finish(&mut self) -> Result<Vec<StreamEvent>>;
}

/// Adapt an HTTP body (`reqwest::Response::bytes_stream()`) into events.
/// The stream ends after `Done` or after the first error.
pub(crate) fn decode_body<S, B, E, D>(body: S, decoder: D) -> ChatStream
where
    S: Stream<Item = std::result::Result<B, E>> + Send + 'static,
    B: AsRef<[u8]>,
    E: Display,
    D: LineDecoder,
{
    struct State<S, D> {
        body: std::pin::Pin<Box<S>>,
        lines: LineBuffer,
        decoder: D,
        pending: VecDeque<Result<StreamEvent>>,
        ended: bool,
    }
    fn push_all(pending: &mut VecDeque<Result<StreamEvent>>, r: Result<Vec<StreamEvent>>) {
        match r {
            Ok(evs) => pending.extend(evs.into_iter().map(Ok)),
            Err(e) => pending.push_back(Err(e)),
        }
    }
    let state = State {
        body: Box::pin(body),
        lines: LineBuffer::default(),
        decoder,
        pending: VecDeque::new(),
        ended: false,
    };
    Box::pin(futures_util::stream::unfold(state, |mut st| async move {
        loop {
            if let Some(item) = st.pending.pop_front() {
                let terminal = match &item {
                    Err(_) => true,
                    Ok(StreamEvent::Done { .. }) => true,
                    Ok(_) => false,
                };
                if terminal {
                    st.ended = true;
                    st.pending.clear();
                }
                return Some((item, st));
            }
            if st.ended {
                return None;
            }
            match st.body.next().await {
                Some(Ok(chunk)) => {
                    for line in st.lines.push(chunk.as_ref()) {
                        let r = st.decoder.line(&line);
                        push_all(&mut st.pending, r);
                    }
                }
                Some(Err(e)) => st
                    .pending
                    .push_back(Err(LlmError::Network(format!("stream interrupted: {e}")))),
                None => {
                    if let Some(tail) = st.lines.finish() {
                        let r = st.decoder.line(&tail);
                        push_all(&mut st.pending, r);
                    }
                    let r = st.decoder.finish();
                    push_all(&mut st.pending, r);
                    if st.pending.is_empty() {
                        st.ended = true;
                    }
                }
            }
        }
    }))
}

/// Replay a finished response as a stream (providers/mocks without
/// incremental output).
pub fn replay(resp: ChatResponse) -> ChatStream {
    Box::pin(futures_util::stream::iter(
        response_events(resp).into_iter().map(Ok),
    ))
}

/// The events a finished response corresponds to.
pub fn response_events(resp: ChatResponse) -> Vec<StreamEvent> {
    let mut out = Vec::new();
    for b in resp.blocks {
        match b {
            ContentBlock::Text { text } => out.push(StreamEvent::TextDelta { text }),
            ContentBlock::ToolUse { id, name, input } => {
                out.push(StreamEvent::ToolUseStart {
                    id: id.clone(),
                    name,
                });
                out.push(StreamEvent::ToolInputDelta {
                    id,
                    partial_json: input.to_string(),
                });
            }
            ContentBlock::Opaque { provider, raw } => {
                out.push(StreamEvent::Opaque { provider, raw })
            }
            // Results are never in a model reply.
            ContentBlock::ToolResult { .. } => {}
        }
    }
    out.push(StreamEvent::Done {
        stop_reason: resp.stop_reason,
        usage: resp.usage,
    });
    out
}

/// Fold a stream back into one response. Tool arguments that aren't valid
/// JSON are kept as a JSON string (the tool loop treats a non-object input
/// as a malformed call); a stream without `Done` is an error.
pub async fn collect(mut stream: ChatStream) -> Result<ChatResponse> {
    let mut acc = Accumulator::default();
    while let Some(ev) = stream.next().await {
        acc.push(ev?);
    }
    acc.finish()
}

/// Incremental fold used by [`collect`] (and usable by a UI that both
/// renders deltas and needs the final response).
#[derive(Debug, Default)]
pub struct Accumulator {
    blocks: Vec<ContentBlock>,
    args: HashMap<String, String>,
    done: Option<(StopReason, Usage)>,
}

impl Accumulator {
    pub fn push(&mut self, ev: StreamEvent) {
        match ev {
            StreamEvent::TextDelta { text } => match self.blocks.last_mut() {
                Some(ContentBlock::Text { text: t }) => t.push_str(&text),
                _ => self.blocks.push(ContentBlock::Text { text }),
            },
            StreamEvent::ToolUseStart { id, name } => {
                self.args.insert(id.clone(), String::new());
                self.blocks.push(ContentBlock::ToolUse {
                    id,
                    name,
                    input: serde_json::Value::Null,
                });
            }
            StreamEvent::ToolInputDelta { id, partial_json } => {
                self.args.entry(id).or_default().push_str(&partial_json);
            }
            StreamEvent::Opaque { provider, raw } => {
                self.blocks.push(ContentBlock::Opaque { provider, raw })
            }
            StreamEvent::Done { stop_reason, usage } => self.done = Some((stop_reason, usage)),
        }
    }

    pub fn finish(mut self) -> Result<ChatResponse> {
        let (stop_reason, usage) = self
            .done
            .ok_or_else(|| LlmError::Network("the reply stream ended before it finished".into()))?;
        for b in &mut self.blocks {
            if let ContentBlock::ToolUse { id, input, .. } = b {
                let raw = self.args.remove(id.as_str()).unwrap_or_default();
                *input = parse_tool_args(&raw);
            }
        }
        Ok(ChatResponse {
            blocks: self.blocks,
            stop_reason,
            usage,
        })
    }
}

/// Tool arguments from their JSON text: empty → `{}`; invalid → the raw
/// text as a JSON string (flagged malformed downstream).
pub fn parse_tool_args(raw: &str) -> serde_json::Value {
    if raw.trim().is_empty() {
        return serde_json::json!({});
    }
    serde_json::from_str(raw).unwrap_or_else(|_| serde_json::Value::String(raw.to_string()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn line_buffer_handles_split_utf8_and_crlf() {
        let mut lb = LineBuffer::default();
        let bytes = "data: héllo\r\nnext".as_bytes();
        // split inside the two-byte é
        let (a, b) = bytes.split_at(8);
        assert!(lb.push(a).is_empty());
        assert_eq!(lb.push(b), vec!["data: héllo".to_string()]);
        assert_eq!(lb.finish().as_deref(), Some("next"));
        assert_eq!(lb.finish(), None);
    }

    #[test]
    fn sse_data_strips_prefix() {
        assert_eq!(sse_data("data: {\"a\":1}"), Some("{\"a\":1}"));
        assert_eq!(sse_data("data:[DONE]"), Some("[DONE]"));
        assert_eq!(sse_data("event: ping"), None);
        assert_eq!(sse_data(": comment"), None);
    }

    #[tokio::test]
    async fn replay_then_collect_round_trips() {
        let resp = ChatResponse {
            blocks: vec![
                ContentBlock::text("let me look"),
                ContentBlock::ToolUse {
                    id: "c1".into(),
                    name: "search_passages".into(),
                    input: json!({"query": "budget"}),
                },
            ],
            stop_reason: StopReason::ToolUse,
            usage: Usage {
                input_tokens: Some(10),
                ..Default::default()
            },
        };
        assert_eq!(collect(replay(resp.clone())).await.unwrap(), resp);
    }

    #[test]
    fn accumulator_merges_deltas_and_keeps_bad_args_as_string() {
        let mut acc = Accumulator::default();
        for ev in [
            StreamEvent::TextDelta { text: "Hel".into() },
            StreamEvent::TextDelta { text: "lo".into() },
            StreamEvent::ToolUseStart {
                id: "a".into(),
                name: "t".into(),
            },
            StreamEvent::ToolInputDelta {
                id: "a".into(),
                partial_json: "{\"q\":".into(),
            },
            StreamEvent::ToolInputDelta {
                id: "a".into(),
                partial_json: "\"x\"}".into(),
            },
            StreamEvent::ToolUseStart {
                id: "b".into(),
                name: "t".into(),
            },
            StreamEvent::ToolInputDelta {
                id: "b".into(),
                partial_json: "{not json".into(),
            },
            StreamEvent::Done {
                stop_reason: StopReason::ToolUse,
                usage: Usage::default(),
            },
        ] {
            acc.push(ev);
        }
        let r = acc.finish().unwrap();
        assert_eq!(r.text(), "Hello");
        let uses = r.tool_uses();
        assert_eq!(uses[0].2, &json!({"q": "x"}));
        assert_eq!(uses[1].2, &json!("{not json"));
    }

    #[test]
    fn missing_done_is_an_error() {
        let mut acc = Accumulator::default();
        acc.push(StreamEvent::TextDelta { text: "x".into() });
        assert!(acc.finish().is_err());
    }

    struct Echo {
        done: bool,
    }
    impl LineDecoder for Echo {
        fn line(&mut self, line: &str) -> Result<Vec<StreamEvent>> {
            if line == "END" {
                self.done = true;
                return Ok(vec![StreamEvent::Done {
                    stop_reason: StopReason::EndTurn,
                    usage: Usage::default(),
                }]);
            }
            Ok(vec![StreamEvent::TextDelta { text: line.into() }])
        }
        fn finish(&mut self) -> Result<Vec<StreamEvent>> {
            if self.done {
                Ok(vec![])
            } else {
                Err(LlmError::Network("cut".into()))
            }
        }
    }

    #[tokio::test]
    async fn decode_body_frames_lines_across_chunks() {
        let chunks: Vec<std::result::Result<Vec<u8>, std::convert::Infallible>> = vec![
            Ok(b"ab".to_vec()),
            Ok(b"c\nde".to_vec()),
            Ok(b"f\nEND\nignored\n".to_vec()),
        ];
        let s = decode_body(futures_util::stream::iter(chunks), Echo { done: false });
        let r = collect(s).await.unwrap();
        assert_eq!(r.text(), "abcdef");
    }

    #[tokio::test]
    async fn decode_body_reports_a_cut_stream() {
        let chunks: Vec<std::result::Result<Vec<u8>, std::convert::Infallible>> =
            vec![Ok(b"partial\n".to_vec())];
        let s = decode_body(futures_util::stream::iter(chunks), Echo { done: false });
        assert!(collect(s).await.is_err());
    }
}
