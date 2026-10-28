# Recorded model replays

One folder per model, one file per scenario:
`replays/<model>/<scenario id>.json`, in the `ReplayFile` format
(`evals/runner/src/ask_contract/mod.rs`):

```json
{
  "format": 1,
  "scenario": "auto-local-q02",
  "model": "llama3.1",
  "recorded_at": "2026-11-02T10:00:00Z",
  "responses": [ { "blocks": [ { "type": "text", "text": "…" } ], "stop_reason": "end_turn" } ]
}
```

Each replay re-runs its base scenario in CI with the recorded responses (as a
quality scenario named `replay:<model>:<scenario>`). None exist yet: they're
recorded once the Ask harness lands (README, "Recording replays"), on this
synthetic corpus only, and added by the user as a locked change.
