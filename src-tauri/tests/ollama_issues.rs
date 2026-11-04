//! Re-tests the Ollama issues the Ask provider code works around, against
//! the local Ollama and real models (ignored by default).
//!
//! * ollama#18944: `format` + `tools` together break tool calling. The
//!   provider refuses the combination; this shows whether the server still
//!   needs that.
//! * ollama#10976: omitting `think` on a thinking model with tools turns
//!   thinking on (empty content, no tool call). The provider always sends it.
//! * ollama#18861: gemma4 with `think: false` answers with nothing after a
//!   tool result. The tier gate's `tool_smoke` exists to catch it.
//!
//! Raw `/api/chat` calls (no provider), so the server's behavior is seen on
//! its own; the app's own `tool_smoke` runs through the provider as well.
//!
//! ```text
//! FOTW_OLLAMA_ISSUES_MODELS=qwen3.5:9b,gemma4:12b-it-qat FOTW_OLLAMA_ISSUES_RUNS=5 \
//!   cargo test -p fly-app --test ollama_issues -- --ignored --nocapture
//! ```
//! The result lands in `<FOTW_ASK_LIVE_OUT or temp>/ollama-issues.json`.

use serde_json::{json, Value};

const OLLAMA: &str = "http://127.0.0.1:11434";

fn tool() -> Value {
    json!({"type": "function", "function": {
        "name": "lookup_codename",
        "description": "Look up a project's codename by its number.",
        "parameters": {
            "type": "object",
            "properties": {"project": {"type": "integer", "description": "Project number."}},
            "required": ["project"]
        }
    }})
}

fn base_messages() -> Vec<Value> {
    vec![
        json!({"role": "system", "content": "Use the lookup_codename tool to answer. Reply with the codename only."}),
        json!({"role": "user", "content": "What is the codename of project 7?"}),
    ]
}

/// Output cap per call (`FOTW_OLLAMA_ISSUES_PREDICT`): enough for a tool
/// call or a one-line answer, short enough that a model that thinks until
/// the cap (the #10976 failure) doesn't take minutes.
fn predict() -> u32 {
    std::env::var("FOTW_OLLAMA_ISSUES_PREDICT")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(400)
}

/// One `/api/chat` call; `think` is `None` to omit the field.
async fn chat(
    http: &reqwest::Client,
    model: &str,
    messages: &[Value],
    tools: bool,
    think: Option<bool>,
    format: Option<Value>,
) -> Value {
    let mut body = json!({
        "model": model, "messages": messages, "stream": false,
        "options": {"temperature": 0, "num_predict": predict(), "num_ctx": 4096},
    });
    if tools {
        body["tools"] = json!([tool()]);
    }
    if let Some(t) = think {
        body["think"] = json!(t);
    }
    if let Some(f) = format {
        body["format"] = f;
    }
    let r = http
        .post(format!("{OLLAMA}/api/chat"))
        .json(&body)
        .send()
        .await
        .expect("ollama");
    let status = r.status().as_u16();
    let mut v: Value = r.json().await.unwrap_or(Value::Null);
    v["_status"] = json!(status);
    v
}

fn summary(v: &Value) -> Value {
    let m = &v["message"];
    json!({
        "status": v["_status"],
        "content_chars": m["content"].as_str().map_or(0, str::len),
        "thinking_chars": m["thinking"].as_str().map_or(0, str::len),
        "tool_calls": m["tool_calls"].as_array().map_or(0, Vec::len),
        "done_reason": v["done_reason"],
        "content_head": m["content"].as_str().unwrap_or("").chars().take(80).collect::<String>(),
        "error": v["error"],
    })
}

/// A two-hop tool dialog: returns (tool called with project 7, final reply).
async fn two_hop(
    http: &reqwest::Client,
    model: &str,
    think1: Option<bool>,
    think2: Option<bool>,
) -> Value {
    let mut msgs = base_messages();
    let first = chat(http, model, &msgs, true, think1, None).await;
    let calls = first["message"]["tool_calls"].clone();
    let call = calls.as_array().and_then(|c| c.first()).cloned();
    let args_ok = call
        .as_ref()
        .map(|c| {
            let a = &c["function"]["arguments"];
            a["project"] == 7 || a["project"] == "7"
        })
        .unwrap_or(false);
    if call.is_none() {
        return json!({"hop1": summary(&first), "tool_called": false});
    }
    msgs.push(
        json!({"role": "assistant", "content": first["message"]["content"], "tool_calls": calls}),
    );
    msgs.push(json!({"role": "tool", "tool_name": "lookup_codename", "content": "Project 7 codename: BLUE HERON"}));
    let second = chat(http, model, &msgs, true, think2, None).await;
    let text = second["message"]["content"].as_str().unwrap_or("");
    json!({
        "hop1": summary(&first),
        "tool_called": true,
        "args_ok": args_ok,
        "hop2": summary(&second),
        "final_empty": text.trim().is_empty(),
        "final_has_heron": text.to_lowercase().contains("heron"),
    })
}

#[tokio::test(flavor = "multi_thread")]
#[ignore = "needs a local Ollama with the models installed; run by hand"]
async fn ollama_issues() {
    let models: Vec<String> = std::env::var("FOTW_OLLAMA_ISSUES_MODELS")
        .unwrap_or_else(|_| "qwen3.5:9b".into())
        .split(',')
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
        .collect();
    let runs: usize = std::env::var("FOTW_OLLAMA_ISSUES_RUNS")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(5);
    let http = reqwest::Client::new();
    let version: Value = http
        .get(format!("{OLLAMA}/api/version"))
        .send()
        .await
        .expect("ollama is not running")
        .json()
        .await
        .unwrap();
    let schema = json!({"type": "object", "properties": {"codename": {"type": "string"}}, "required": ["codename"]});
    let mut report = json!({"ollama": version, "runs": runs, "models": {}});
    for model in &models {
        eprintln!("ollama_issues: {model}");
        // #18944: tools alone (control) vs tools + format, thinking off.
        let mut f18944 = Vec::new();
        for _ in 0..runs {
            let control = chat(&http, model, &base_messages(), true, Some(false), None).await;
            let both = chat(
                &http,
                model,
                &base_messages(),
                true,
                Some(false),
                Some(schema.clone()),
            )
            .await;
            f18944
                .push(json!({"tools_only": summary(&control), "tools_and_format": summary(&both)}));
        }
        // #10976: tools with `think` omitted vs explicitly false.
        let mut f10976 = Vec::new();
        for _ in 0..runs {
            let omitted = chat(&http, model, &base_messages(), true, None, None).await;
            let off = chat(&http, model, &base_messages(), true, Some(false), None).await;
            f10976.push(json!({"think_omitted": summary(&omitted), "think_false": summary(&off)}));
        }
        // #18861: the second hop after a tool result, thinking off (what the
        // app sends), with `think` omitted and with `think: true` for contrast.
        let mut f18861 = Vec::new();
        for _ in 0..runs {
            f18861.push(json!({
                "think_false": two_hop(&http, model, Some(false), Some(false)).await,
                "think_omitted": two_hop(&http, model, Some(false), None).await,
                "think_true_hop2": two_hop(&http, model, Some(false), Some(true)).await,
            }));
        }
        let count =
            |rows: &[Value], f: &dyn Fn(&Value) -> bool| rows.iter().filter(|r| f(r)).count();
        let digest = json!({
            "18944": {
                "tools_only_called_tool": count(&f18944, &|r| r["tools_only"]["tool_calls"].as_u64() > Some(0)),
                "tools_and_format_called_tool": count(&f18944, &|r| r["tools_and_format"]["tool_calls"].as_u64() > Some(0)),
                "tools_and_format_json_content": count(&f18944, &|r| r["tools_and_format"]["content_chars"].as_u64() > Some(0)),
            },
            "10976": {
                "think_omitted_called_tool": count(&f10976, &|r| r["think_omitted"]["tool_calls"].as_u64() > Some(0)),
                "think_omitted_thinking": count(&f10976, &|r| r["think_omitted"]["thinking_chars"].as_u64() > Some(0)),
                "think_omitted_empty": count(&f10976, &|r| r["think_omitted"]["content_chars"] == 0 && r["think_omitted"]["tool_calls"] == 0),
                "think_false_called_tool": count(&f10976, &|r| r["think_false"]["tool_calls"].as_u64() > Some(0)),
            },
            "18861": {
                "think_false_final_empty": count(&f18861, &|r| r["think_false"]["final_empty"] == true),
                "think_false_final_ok": count(&f18861, &|r| r["think_false"]["final_has_heron"] == true),
                "think_omitted_final_empty": count(&f18861, &|r| r["think_omitted"]["final_empty"] == true),
                "think_true_final_empty": count(&f18861, &|r| r["think_true_hop2"]["final_empty"] == true),
                "tool_not_called": count(&f18861, &|r| r["think_false"]["tool_called"] == false),
            },
        });
        eprintln!("ollama_issues: {model}: {digest}");
        report["models"][model] =
            json!({"digest": digest, "18944": f18944, "10976": f10976, "18861": f18861});
    }
    let out = std::env::var_os("FOTW_ASK_LIVE_OUT")
        .map(std::path::PathBuf::from)
        .unwrap_or_else(|| std::env::temp_dir().join("fotw-ask-live"));
    std::fs::create_dir_all(&out).ok();
    let path = out.join("ollama-issues.json");
    std::fs::write(&path, serde_json::to_string_pretty(&report).unwrap()).unwrap();
    eprintln!("ollama_issues: → {}", path.display());
}
