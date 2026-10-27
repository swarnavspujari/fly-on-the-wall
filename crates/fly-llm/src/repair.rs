//! Small-model tool-call repair.
//!
//! Local models often write a tool call as text instead of using the native
//! tool channel: llama3.1 emits `<|python_tag|>{"name": ..., "parameters":
//! {...}}`, qwen emits `<tool_call>{"name": ..., "arguments": {...}}
//! </tool_call>`, others wrap bare JSON in a code fence. [`classify`] reads
//! one reply once and says whether it is a plain answer, a recoverable call
//! of a declared tool, or something that looks like a call but can't be
//! used. The tool loop turns the last case into one corrective turn; a
//! second failure ends the loop with an error (never a silent guess).

use serde_json::Value;

use crate::conversation::ToolSpec;

/// What a text reply turned out to be.
#[derive(Debug, Clone, PartialEq)]
pub enum TextToolCall {
    /// An ordinary answer.
    NotACall,
    /// A usable call of a declared tool.
    Call { name: String, input: Value },
    /// Shaped like a tool call but unusable (bad JSON, unknown tool,
    /// arguments not an object). Carries a short, fixed-vocabulary reason
    /// for the corrective message — never any of the model's text (not even
    /// an invented tool name).
    Malformed { reason: String },
}

const WRAPPERS: &[(&str, &str)] = &[
    ("<tool_call>", "</tool_call>"),
    ("<|python_tag|>", "<|eom_id|>"),
    ("<function_call>", "</function_call>"),
];

/// Classify a text-only reply given the tools that were offered.
pub fn classify(text: &str, tools: &[ToolSpec]) -> TextToolCall {
    let trimmed = text.trim();
    if trimmed.is_empty() || tools.is_empty() {
        return TextToolCall::NotACall;
    }
    let (candidate, wrapped) = unwrap_candidate(trimmed);
    let looks_like_call = wrapped
        || candidate.starts_with("{\"name\"")
        || candidate.starts_with("{ \"name\"")
        || (candidate.starts_with('{')
            && tools
                .iter()
                .any(|t| candidate.contains(&format!("\"{}\"", t.name))));
    if !looks_like_call {
        return TextToolCall::NotACall;
    }
    let Some(json_text) = first_json_object(candidate) else {
        return TextToolCall::Malformed {
            reason: "the tool call was not valid JSON".into(),
        };
    };
    let Ok(v) = serde_json::from_str::<Value>(json_text) else {
        return TextToolCall::Malformed {
            reason: "the tool call was not valid JSON".into(),
        };
    };
    // {"name": ..., "arguments"|"parameters"|"input": {...}} or the OpenAI
    // shape {"function": {"name": ..., "arguments": ...}}.
    let v = v.get("function").cloned().unwrap_or(v);
    let Some(name) = v.get("name").and_then(|n| n.as_str()) else {
        return TextToolCall::Malformed {
            reason: "the tool call had no \"name\"".into(),
        };
    };
    if !tools.iter().any(|t| t.name == name) {
        return TextToolCall::Malformed {
            // Never echo the model's invented name: the reason ends up in
            // `LlmError::ToolProtocol`, which may reach logs.
            reason: format!(
                "that tool name isn't one of the available tools ({})",
                tools
                    .iter()
                    .map(|t| t.name.as_str())
                    .collect::<Vec<_>>()
                    .join(", ")
            ),
        };
    }
    let args = ["arguments", "parameters", "input"]
        .iter()
        .find_map(|k| v.get(*k))
        .cloned()
        .unwrap_or_else(|| serde_json::json!({}));
    // Some models double-encode the arguments as a JSON string.
    let args = match args {
        Value::String(s) => match serde_json::from_str::<Value>(&s) {
            Ok(inner) => inner,
            Err(_) => {
                return TextToolCall::Malformed {
                    reason: "the tool arguments were not valid JSON".into(),
                }
            }
        },
        other => other,
    };
    if !args.is_object() {
        return TextToolCall::Malformed {
            reason: "the tool arguments must be a JSON object".into(),
        };
    }
    TextToolCall::Call {
        name: name.to_string(),
        input: args,
    }
}

/// Strip a known wrapper or a code fence; anything after the closing marker
/// is dropped (models keep talking after a call — that text is discarded).
fn unwrap_candidate(text: &str) -> (&str, bool) {
    for (open, close) in WRAPPERS {
        if let Some(rest) = text.strip_prefix(open) {
            let inner = rest.split(close).next().unwrap_or(rest);
            return (inner.trim(), true);
        }
    }
    if let Some(rest) = text.strip_prefix("```") {
        let rest = rest
            .strip_prefix("json")
            .or_else(|| rest.strip_prefix("JSON"))
            .unwrap_or(rest);
        let inner = rest.split("```").next().unwrap_or(rest);
        return (inner.trim(), false);
    }
    (text, false)
}

/// The first balanced `{...}` in `s` (string-aware), so trailing prose after
/// the object doesn't break parsing.
fn first_json_object(s: &str) -> Option<&str> {
    let start = s.find('{')?;
    let mut depth = 0usize;
    let mut in_str = false;
    let mut escaped = false;
    for (i, c) in s[start..].char_indices() {
        if in_str {
            match (escaped, c) {
                (true, _) => escaped = false,
                (false, '\\') => escaped = true,
                (false, '"') => in_str = false,
                _ => {}
            }
            continue;
        }
        match c {
            '"' => in_str = true,
            '{' => depth += 1,
            '}' => {
                depth -= 1;
                if depth == 0 {
                    return Some(&s[start..start + i + 1]);
                }
            }
            _ => {}
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn tools() -> Vec<ToolSpec> {
        vec![ToolSpec {
            name: "search_passages".into(),
            description: "search".into(),
            input_schema: json!({"type": "object"}),
            strict: false,
        }]
    }

    fn call(name: &str, input: Value) -> TextToolCall {
        TextToolCall::Call {
            name: name.into(),
            input,
        }
    }

    #[test]
    fn plain_answers_are_not_calls() {
        for t in [
            "The budget was $8,400 [seg:s1].",
            "",
            "{ this is prose in braces }",
        ] {
            assert_eq!(classify(t, &tools()), TextToolCall::NotACall, "{t}");
        }
        // No tools offered → never a call.
        assert_eq!(
            classify(r#"{"name":"search_passages"}"#, &[]),
            TextToolCall::NotACall
        );
    }

    #[test]
    fn llama_python_tag_with_parameters() {
        let t = r#"<|python_tag|>{"name": "search_passages", "parameters": {"query": "budget"}}<|eom_id|>"#;
        assert_eq!(
            classify(t, &tools()),
            call("search_passages", json!({"query": "budget"}))
        );
    }

    #[test]
    fn qwen_tool_call_tag_and_trailing_prose_is_dropped() {
        let t = "<tool_call>\n{\"name\": \"search_passages\", \"arguments\": {\"query\": \"q3 {plan}\"}}\n</tool_call>\nI will now search.";
        assert_eq!(
            classify(t, &tools()),
            call("search_passages", json!({"query": "q3 {plan}"}))
        );
    }

    #[test]
    fn fenced_json_and_string_encoded_arguments() {
        let t = "```json\n{\"name\":\"search_passages\",\"arguments\":\"{\\\"query\\\":\\\"x\\\"}\"}\n```";
        assert_eq!(
            classify(t, &tools()),
            call("search_passages", json!({"query": "x"}))
        );
    }

    #[test]
    fn openai_function_shape() {
        let t = r#"{"function": {"name": "search_passages", "arguments": {"query": "x"}}}"#;
        assert_eq!(
            classify(t, &tools()),
            call("search_passages", json!({"query": "x"}))
        );
    }

    #[test]
    fn malformed_shapes_are_flagged_not_guessed() {
        let cases = [
            r#"{"name": "search_passages", "arguments": {"query": "x"}"#, // unbalanced
            r#"<tool_call>{"name": "delete_everything", "arguments": {}}</tool_call>"#,
            r#"{"name": "search_passages", "arguments": ["x"]}"#,
            r#"<tool_call>{"arguments": {}}</tool_call>"#,
        ];
        for t in cases {
            assert!(
                matches!(classify(t, &tools()), TextToolCall::Malformed { .. }),
                "{t}"
            );
        }
        // The reason names the real tools, never the model's text.
        if let TextToolCall::Malformed { reason } = classify(cases[1], &tools()) {
            assert!(reason.contains("search_passages"), "{reason}");
            assert!(!reason.contains("delete_everything"), "{reason}");
        }
    }
}
