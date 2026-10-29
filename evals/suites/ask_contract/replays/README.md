# Recorded model replays

One folder per model, one file per scenario:
`replays/<model>/<scenario id>.json`, in the `ReplayFile` format 2
(`evals/runner/src/ask_contract/provider.rs`):

```json
{
  "format": 2,
  "scenario": "auto-s-local-q02",
  "model": "llama3.1",
  "provider": "ollama",
  "capabilities": { "tools": true, "streaming": true, "probed": true, "...": "..." },
  "profile": { "thinking": "default", "effort": null },
  "tier": "S",
  "requests_fingerprint": "<sha-256 of the requests, database ids normalized>",
  "recorded_at": "2026-11-02T10:00:00Z",
  "responses": [ { "blocks": [ { "type": "text", "text": "…" } ], "stop_reason": "end_turn" } ]
}
```

Scenario ids look like `auto-<tier>-<provider>-<gold id>` (`auto-s-local-q02`,
`auto-c-cloud-q46`) or a scripted scenario's id. Each replay re-runs its base
scenario in CI with the recorded replies, as `replay:<model>:<scenario>`; a
replay of a quality scenario scores in the group `replay:<model>@<tier>`.
When the harness's requests no longer match `requests_fingerprint`, the
replay is stale: it isn't scored, and `stale_replays` (gated at 0) fails the
suite until it's re-recorded.

None exist yet. They're recorded with
`RecordingProvider::wrap(provider, on_device)` and `to_replay(...)` once the
Ask harness lands (README, "Recording replays"), on this synthetic corpus
only, and added with the user's approval.
