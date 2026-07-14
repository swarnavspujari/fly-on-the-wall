//! Per-model prompt profiles: small, code-owned adjustments to the shared
//! prompts for models that need them. The four LLM call sites (Enhance,
//! Polish, Extraction, Ask) consult the profile of the *resolved model*
//! (`LLMProvider::model()`) at prompt-build time. The default profile is a
//! no-op — every model behaves exactly as before until an entry is added to
//! `PROFILES`, so shipping this registry is behavior-neutral.
//!
//! Profiles are code, not settings: they encode what we measured about a
//! model (see `docs/BENCHMARKS.md`), and users shouldn't have to tune them.
//!
//! # How to add a profile for a new model
//!
//! This is the checklist a "switch the default local model" change follows:
//!
//! 1. Benchmark the model first (`src-tauri/tests/llm_bench.rs`,
//!    `docs/BENCHMARKS.md` documents how). Only add a profile for a
//!    *measured* failure — contract violations, truncation, chattiness —
//!    never speculatively.
//! 2. Add one entry to `PROFILES` below, keyed by the model's base name
//!    (the part before any `:tag` — `"qwen3.5"` matches `qwen3.5:4b` and
//!    `qwen3.5:latest`). Prefer the narrowest fix:
//!    - `system_preamble` for behavioral nudges ("answer directly, no
//!      preamble"), prepended to every task's system prompt;
//!    - `simplified_contract: true` when the model fails the strict JSON
//!      block contracts — Enhance and Extraction switch to a terser
//!      phrasing with an inline example (Polish never simplifies: its
//!      no-loss contract is load-bearing for the retention guard);
//!    - `max_tokens` overrides per task when the model truncates output
//!      (or wastes budget) at the shared defaults (4096 enhance / 8192
//!      polish / 8192 extract / 2048 ask);
//!    - `disable_thinking: true` for local thinking models whose reasoning
//!      trace eats the output budget (empty/truncated answers on the
//!      `ThinkingMode::Default` call sites — Enhance and Ask).
//! 3. Re-run the benchmark with the profile applied and record the delta
//!    in `docs/BENCHMARKS.md` before changing any default-model constant.
//! 4. If the model becomes a provider default, update the `PROVIDERS`
//!    table in `src-tauri/src/llm_commands.rs` — that's the only other
//!    place a default model name lives.

/// Per-task `max_tokens` overrides. `None` keeps the call site's shared
/// default (documented at each site in `llm_commands.rs` / `extraction.rs`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct TaskMaxTokens {
    pub enhance: Option<u32>,
    pub polish: Option<u32>,
    pub extract: Option<u32>,
    pub ask: Option<u32>,
}

/// Prompt adjustments for one model family. Deliberately minimal — four
/// knobs, no templating. If a model needs more than this, the shared prompt
/// probably needs fixing for everyone instead.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PromptProfile {
    /// Prepended (with a blank line) to every task's system prompt.
    pub system_preamble: Option<&'static str>,
    /// Use the terser JSON output-contract phrasing in Enhance/Extraction.
    pub simplified_contract: bool,
    /// Per-task output token budget overrides.
    pub max_tokens: TaskMaxTokens,
    /// Turn model "thinking" off on EVERY task, including the two call sites
    /// that ship `ThinkingMode::Default` (Enhance, Ask). For local thinking
    /// models (qwen3.5, deepseek-r1) the reasoning trace otherwise consumes
    /// the whole output budget and the answer comes back empty/truncated —
    /// measured in docs/BENCHMARKS.md §3.0/§6. Leave false for cloud models:
    /// Anthropic's adaptive thinking helps Enhance/Ask quality there.
    pub disable_thinking: bool,
}

impl PromptProfile {
    /// Prepend this profile's preamble to a system prompt (no-op when unset).
    pub fn apply_preamble(&self, system: String) -> String {
        match self.system_preamble {
            Some(p) => format!("{p}\n\n{system}"),
            None => system,
        }
    }

    /// Whether a call site that would ship `thinking_default` should disable
    /// thinking instead. Returns `true` when the profile turns thinking off;
    /// call sites map that to `ThinkingMode::Disabled`. (Kept as a bool so
    /// fly-core needs no dependency on fly-llm's `ThinkingMode`.)
    pub fn thinking_disabled(&self) -> bool {
        self.disable_thinking
    }
}

/// The no-op profile: current behavior for every model without an entry.
pub const DEFAULT_PROFILE: PromptProfile = PromptProfile {
    system_preamble: None,
    simplified_contract: false,
    max_tokens: TaskMaxTokens {
        enhance: None,
        polish: None,
        extract: None,
        ask: None,
    },
    disable_thinking: false,
};

/// The registry: `(model base name, profile)`. Keys match the model string
/// with any `:tag` suffix stripped. See the module docs for the checklist
/// before adding an entry. (llm_bench also measured llama3.1 contract
/// failures, but the candidate fix — the simplified contract — leaked its
/// inline example into llama3.1's output and scored WORSE on quality, so no
/// llama3.1 profile earned its way in; docs/BENCHMARKS.md §5.)
const PROFILES: &[(&str, PromptProfile)] = &[
    // qwen3.5 reasons unconditionally; with thinking on, Enhance came back
    // EMPTY (0/3 contract) and 3/15 Ask answers were empty because the trace
    // consumed the output budget. With thinking off it posts the best local
    // scores in the benchmark (docs/BENCHMARKS.md §6). Measured 2026-07-14.
    (
        "qwen3.5",
        PromptProfile {
            disable_thinking: true,
            ..DEFAULT_PROFILE
        },
    ),
];

/// Look up the profile for a resolved model string (`"llama3.1:latest"`,
/// `"claude-sonnet-5"`). Unknown models get `DEFAULT_PROFILE`.
pub fn profile_for(model: &str) -> &'static PromptProfile {
    let base = model.split(':').next().unwrap_or(model).trim();
    PROFILES
        .iter()
        .find(|(key, _)| *key == base)
        .map(|(_, p)| p)
        .unwrap_or(&DEFAULT_PROFILE)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn unknown_models_get_the_default_profile() {
        for m in ["llama3.1", "llama3.1:latest", "claude-sonnet-5", "", "mock"] {
            assert_eq!(profile_for(m), &DEFAULT_PROFILE, "model {m:?}");
        }
    }

    #[test]
    fn qwen35_profile_disables_thinking_and_nothing_else() {
        for m in ["qwen3.5", "qwen3.5:4b", "qwen3.5:latest"] {
            let p = profile_for(m);
            assert!(p.disable_thinking, "model {m:?}");
            // Prompts stay byte-identical to the default profile.
            assert_eq!(p.system_preamble, None);
            assert!(!p.simplified_contract);
            assert_eq!(p.max_tokens, DEFAULT_PROFILE.max_tokens);
        }
        // The qwen3 (non-3.5) family is NOT matched by the qwen3.5 key.
        assert_eq!(profile_for("qwen3:4b"), &DEFAULT_PROFILE);
    }

    #[test]
    fn tag_suffix_is_stripped_before_lookup() {
        // Same profile object for tagged and untagged spellings.
        assert!(std::ptr::eq(
            profile_for("qwen3.5:4b"),
            profile_for("qwen3.5")
        ));
    }

    #[test]
    fn default_profile_preamble_is_a_noop() {
        let s = "SYSTEM".to_string();
        assert_eq!(DEFAULT_PROFILE.apply_preamble(s.clone()), s);
    }

    #[test]
    fn preamble_prepends_with_blank_line() {
        let p = PromptProfile {
            system_preamble: Some("Answer directly."),
            ..DEFAULT_PROFILE
        };
        assert_eq!(
            p.apply_preamble("SYSTEM".into()),
            "Answer directly.\n\nSYSTEM"
        );
    }
}
