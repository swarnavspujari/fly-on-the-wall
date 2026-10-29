# Attendees and speaker roster (`ui-attendees`)

> Layer: frontend feature. Described at main `82b7edf` (v2.4.0).
> In flight: None. (PR #92 changes transcript storage under re-diarize and undo, but no frontend file here.)

## Purpose
Says who was on a meeting, so diarization can use the head count. There are three entry points. The **attendee pill** in the note's meta row opens a popover editor: "Just you", named rows, and calendar-seeded rows that keep their email. The **speaker card** appears while this meeting records with an unconfirmed roster. And `calendarMatch.ts` seeds a plain Record press from the calendar event in progress. Saving marks the roster user-confirmed, and only a confirmed roster sets `num_speakers` (spec §2 item 7). On a transcribed meeting, Save offers "Re-analyze speakers" (re-diarize only), with a 10-minute undo.

## Files it owns
- `frontend/src/components/AttendeeEditor.tsx` — the pill, the popover editor, re-analyze + undo (popover footer, toast, ⌘Z / Ctrl+Z), and `SpeakerCard`.
- `frontend/src/calendarMatch.ts` — `pickInProgressEvent`, `seedFromEvent` (used by `App.tsx:692-693`).

## Surface area
### Public API
| Export | File | Notes |
|---|---|---|
| `AttendeeEditor` (default) | `AttendeeEditor.tsx:65` | Props `meeting`, `hasTranscript`, `onMeetingChanged`, `onTranscriptRestored` (`:56-63`). Rendered in `Editor.tsx:733-740`. It stays mounted across notes; only `meeting` changes. |
| `pickInProgressEvent(events, now)` | `calendarMatch.ts:11-26` | The event whose window (start − 5 min … end) contains `now`. On overlap, the latest start wins. |
| `seedFromEvent(ev, forNewNote)` | `calendarMatch.ts:29-31` | `{title: forNewNote ? ev.title : null, attendees}`. This title rule is duplicated in the backend (`src-tauri/src/recording.rs:86`). |

Behaviour:
- **Popover Save** (`AttendeeEditor.tsx:254-276`): "Just you" sends `[]`. Otherwise it sends the non-empty rows, and a name falls back to the email. `update_meeting_attendees` marks the list confirmed (`crates/fly-storage/src/meetings.rs:82`). With a transcript, the popover stays open and offers Re-analyze. Without one, it closes.
- **Re-analyze** (`:278-292`): `re_diarize_meeting` reports changed segments, and the undo opens for 10 minutes (`UNDO_WINDOW_MS`, `:25`). Undo is offered in the popover footer (`:624-649`), in a toast **anchored under the pill** (`:685-725`; hidden while the popover is open, because the bottom corners belong to the notice stack), and as ⌘Z / Ctrl+Z through a **document `keydown` listener** that skips inputs, textareas and contentEditable (`:231-243`). The undo state is reloaded from `speaker_undo_state` on each meeting change (`:104-123`). The 10-minute window is UI-only: the backend reverts whenever a snapshot exists (`src-tauri/src/asr_commands.rs:136-153`).
- **Esc:** the popover closes through the shared layer stack, `useEscapeLayer` (`AttendeeEditor.tsx:151`). The speaker card handles Esc itself, with a React `onKeyDown` on its region (`:814-818`), so only an Esc from inside the card hides it, and only until the note opens again. Clicking the backdrop also closes the popover (`:449`).
- **Speaker card** (`:159-199`, `:751-911`): shown when `recording_status` says this meeting is recording, the roster is unconfirmed, and you haven't dismissed it. "Looks right" confirms the list as is. "Edit list" opens the editor and focuses the first name. "Not now" hides it for the session (module-level `dismissedCards` Set, `:30`). Typed names are saved only on Done, and Done is disabled for an empty list (which would mean "just you") (`:889-897`). The card never takes focus.

### Tauri commands
- Calls: `update_meeting_attendees`, `re_diarize_meeting`, `revert_speaker_assignment`, `speaker_undo_state`, `recording_status`. `calendarMatch` feeds `start_recording(noteId, seed)` in `App.tsx` (events come from `upcoming_meetings`).
- Backend owners: `app-pipeline` (attendees, re-diarize, undo), `app-recording` (`recording_status`, `start_recording`), `app-calendar` (`upcoming_meetings`).
- devMock mirrors all of them (`devMock.ts:1182-1195`, `recording_status`). It is simplified: `re_diarize_meeting` always reports 3 changed lines, and `update_meeting_attendees` always returns note n1's meeting.

### Events
| Event | Emits / listens | Payload | Counterpart |
|---|---|---|---|
| document `keydown` (⌘Z / Ctrl+Z) | listens while the undo window is open (`AttendeeEditor.tsx:231-243`) | — | — |
| Esc layer | registers one while the popover is open (`:151`) | — | `ui.tsx` layer stack |

### Settings keys
| Key (or prefix) | Read / write | Webview-settable? (`is_webview_setting_key`) | Meaning |
|---|---|---|---|
| None | — | — | Attendees live on the meeting row. |

localStorage keys: none. devMock toggles: `fotwMockRecording="1"` (an active capture, so the card shows), `fotwMockNoAttendees="1"` (no calendar roster).

### Database
None directly. Through commands: `meetings.attendees_json` / `attendees_confirmed` (`fly-storage` `meetings.rs:82`) and `transcripts.speaker_undo_json` (the snapshot, `fly-storage` `transcripts.rs:206-219`).

### Files and directories written
None from the webview (the backend re-mirrors the meeting manifest).

### Network egress
| Host | When | Gate (consent / keep-local / user action) |
|---|---|---|
| Saved LLM provider (indirect) | after Re-analyze and after Undo, the backend re-extracts action items in the background (`asr_commands.rs:104-153`, `spawn_extraction`) | backend consent + Keep local (`build_provider`) |

### External processes
None from the webview. Re-diarize runs the local diarizer in the backend.

### Secrets and keychain entries
None.

## Dependencies
- **Inbound:** `Editor.tsx` (`MetaRow`, `:733-740`). `App.tsx:5`, `:692-693` (calendar seed on Record).
- **Outbound:** `api.ts`, `ui.tsx` (`Avatar`, `Button`, `useEscapeLayer`).
- **Sibling writer:** the transcript speaker menu's "Someone else…" (`Editor.tsx:1212-1229`, [ui-editor](ui-editor.md)) also calls `update_meeting_attendees`, which confirms the **whole** roster, calendar-seeded names included, when you add one person.

## Invariants
- **Calendar-seeded attendees stay unconfirmed until you confirm them. Only a confirmed roster sets `num_speakers`** (spec §2 item 7; backend `speaker_hint`, `src-tauri/src/pipeline.rs:43-51`). The card and the popover write only on an explicit click: "Looks right", Done or Save (`AttendeeEditor.tsx:201-213`, `:254-276`). Showing or re-rendering writes nothing.
- An empty roster is never confirmed from the card (it would mean "just you") (`:892-893`).
- "Just you" = confirmed with `[]` (the pill reads "Just you", `:296-302`).
- Re-analyze never changes transcript text. It re-runs diarize → align → save and snapshots the prior speaker assignment (`api.ts:149-152`; `asr_commands.rs:104-135`). Undo restores the speakers and keeps later text edits (`asr_commands.rs:136-138`).
- The card never steals focus. Closing it moves focus to the pill (`:185-195`).

## Tests
| Kind | File | Tests | Covers |
|---|---|---|---|
| vitest (devMock + spies) | `frontend/src/components/AttendeeEditor.test.tsx` | 12 | the card shows only while this meeting records and the roster is unconfirmed; it never writes by itself; "Looks right" sends the roster unchanged; "Edit list" opens the editor with focus; "Not now" lasts the session; stray Esc ignored, Esc inside hides until reopen; typed names never saved without Done; no duplicate names; Done disabled when empty; popover Esc via the layer stack |

**Known gaps:**
- **Re-analyze + undo has no test:** Save → Re-analyze → toast / footer → Undo, ⌘Z / Ctrl+Z (and that it's ignored inside inputs), the 10-minute expiry, `speaker_undo_state` on reopen, and `onTranscriptRestored`.
- Popover Save has no test: "Just you" sends `[]`, empty rows are dropped, a seeded row keeps its email after a rename, and the dialog stays open when there's a transcript.
- `calendarMatch.ts` has no test (the 5-minute lead, overlap tie-break, invalid dates, title only for a new note).
- "Someone else…" confirming the whole seeded roster is untested and undocumented in the UI (see Dependencies).
- `dismissedCards` is module state shared by every test in the file. The tests stay independent only because each makes a fresh meeting id (`seq`).

## Changing it safely
- Run `npm --prefix frontend test` (vitest + jsdom).
- Any new path that calls `update_meeting_attendees` confirms the roster. Make it an explicit user action, and say so in the UI (spec §2 item 7).
- Keep the card's Esc local to the card (the `onKeyDown` on the region). A global Esc would let a stray Esc from the notes editor dismiss it.
- If the backend ever enforces the undo window, align `UNDO_WINDOW_MS` with it. Today the UI hides an undo the backend would still perform.
- Keep `seedFromEvent`'s title rule in step with `recording.rs:86`.
