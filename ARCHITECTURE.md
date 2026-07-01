# Looma architecture

## The golden rule

**UI talks to `looma-core` types via Tauri commands only. `looma-core` depends on trait crates,
never on OS-specific impls. Impl selection happens once, in `src-tauri`.** That composition-root
discipline is what makes the macOS/iOS/Android ports "add one impl crate," not a rewrite.

```
frontend/  (React + TS + Tailwind — thin; no business logic)
    │  invoke() / events
    ▼
src-tauri/ (composition root: picks platform impls, owns app state, exposes commands)
    │
    ▼
crates/
  looma-core/           domain: notes, folders, meetings, templates, provenance,
                        transcript model, word↔speaker aligner. No OS, no UI, no network.
  looma-audio/          AudioCapture trait (+ Windows WASAPI impl from M2)
  looma-asr/            TranscriptionEngine trait (whisper.cpp / parakeet / groq from M3)
  looma-diarize/        DiarizationEngine trait (sherpa-onnx from M3)
  looma-llm/            LLMProvider trait (nim / openai / anthropic / ollama from M4) + mock
  looma-calendar/       CalendarProvider trait (google / msgraph from M5)
  looma-capture-screen/ ScreenRecorder trait (ffmpeg sidecar from M7)
  looma-storage/        SQLite+FTS5 index + markdown/JSON on disk + media/attachments
  looma-secrets/        SecretStore trait + OS-keychain impl (all keys/tokens live here)
  looma-mcp/            stdio MCP server binary over looma-storage (from M6)
```

## Key design points

### Provenance (looma-core::model)

A note is a list of **blocks**; each block is `origin: user | ai{source_segment_ids}`. AI blocks
carry the transcript segment ids they were derived from — that mapping powers "zoom in" (click an
AI line → see the exact source). Editing an AI block flips it to `user` (the text becomes yours).
Plain-markdown export flattens colors, optionally keeping sources as comments.

### Transcript model & aligner (looma-core::align)

ASR produces **words with timestamps**; diarization produces **speaker turns**. The pure,
unit-tested aligner assigns every word the speaker whose turn overlaps it most (nearest-turn
fallback within 1 s), then groups consecutive same-speaker words into segments, splitting on >2 s
pauses. Speaker keys (`mic`, `spk_0`, …) are stable; display labels are relabelable.

### Storage (looma-storage)

SQLite (bundled, WAL) is the **index**: folders, note metadata, FTS5 tables for note bodies and
transcripts. The **source of truth the user owns** is markdown/JSON on disk under a visible data
dir (`%APPDATA%/Looma` by default): `notes/*.md`, `transcripts/*.{md,json}`, `attachments/`,
`recordings/`. Everything is portable; nothing is locked in a database blob.

### Secrets (looma-secrets)

Every API key and OAuth token goes through `SecretStore` → OS keychain (Windows Credential
Manager). No plaintext secrets on disk, no secrets in logs, `.env` only as a dev convenience.

### Privacy boundaries

Local by default. Exactly two things can touch the network, both user-triggered and clearly
labeled in the UI: (1) LLM enhancement/chat via the chosen provider (Ollama = fully local), and
(2) the optional Groq cloud-ASR fallback. Diarization is **always** local (spec §6.3), even when
ASR is offloaded — Groq's word timestamps are merged with locally computed speaker turns.

### Deliberately empty seams

`looma-core::seams::{SharingProvider, Integration}` exist so hosted sharing and CRM/app
integrations have a place to plug in later. They are intentionally unimplemented (out of scope).

## Runtime shape

- Tauri 2 (stable). `src-tauri/src/lib.rs` is the composition root; `main.rs` is a thin shim so
  the same lib can back mobile later (`crate-type = ["staticlib", "cdylib", "rlib"]`).
- Heavy work (transcription, diarization, enhancement) runs off the UI thread with progress
  events streamed to the frontend.
- Sidecar binaries (whisper-cli, sherpa-onnx, ffmpeg) are managed per-machine in the data dir —
  downloaded with checksum verification, never committed.
