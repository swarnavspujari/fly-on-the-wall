---
name: flyonthewall-meetings
description: Use when answering questions about the user's meetings, projects, customers, action items, decisions, or "what happened / what changed" — drives the Fly on the Wall MCP server (local meeting notes + transcripts + extracted items) with the intended context-first workflow.
---

# Fly on the Wall — meeting context workflow

The `flyonthewall` MCP server exposes the user's local meeting notes,
diarized transcripts, and a structured layer of facts extracted from them
(decisions, action items, open questions, commitments, key figures). Every
extracted fact carries provenance: the meeting id and the transcript segment
ids it came from.

## The workflow

1. **Start with `get_context(query)`** for any question about a project,
   customer, or recurring meeting ("where are we with the Acme renewal?").
   It detects the recurring meeting series (normalized title + participant
   overlap) and returns a deterministic briefing: participants, timeline,
   decisions, action items, commitments, open questions, key figures —
   nothing generated, everything cited.
2. **Targeted lookups** go through the item tools:
   - `open_items(scope)` — what's still open, owner by owner.
   - `whats_changed(series, since)` — the delta since a date.
   - `query_items(type/owner/status/since)` — e.g. everything Dana owns,
     all figures since June.
3. **Drill into raw material only to verify or quote.** `get_transcript`
   (one meeting) / `get_transcripts` (a whole series, max 20) render
   diarized transcripts with a speaker legend; pass `with_segment_ids: true`
   to see the segment ids the item tools cite. `get_note` is the user's own
   notes; `search_notes` is full-text search with folder/date filters.
4. **Fix speaker names when you learn them.** If the user says "Speaker 1 is
   Dana", call `set_speaker_label(meeting_id, speaker_key, label)` — the
   server's only write, identical to the app's own relabel UI.

## Rules

- Extracted items are machine-generated. Before presenting a consequential
  claim as fact, verify it against the cited segment ids in the transcript.
- Item `status` is what was said IN that meeting. Before chasing someone
  about an old "open" item, check later meetings in the series
  (`whats_changed`) for it being resolved.
- Meetings may exist without transcripts (not yet transcribed) and
  transcripts without items (extraction not run) — `get_context`'s timeline
  marks both; fall back to reading transcripts when items are missing.
