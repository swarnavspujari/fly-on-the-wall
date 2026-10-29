# Note editor (`ui-editor`)

> Layer: frontend feature. Described at main `82b7edf` (v2.4.0).
> In flight: PR #92 (draft, v2.5) adds `citation_locators` with re-anchoring on re-transcribe and per-segment storage. That changes when an AI line's cited segment ids still resolve, which is what `enhancedTrust.ts` calls "sourced" / "source not found" (inferred; no frontend file in #92). PR #86: none.

## Purpose
The open note: title (a new note's "Untitled" shows as the placeholder, so typing replaces it; an imported note's title follows its queue, see [ui-import](ui-import.md)), meta chips (date, template, folder, attendees, Keep local, recording, attachments), and one content region with three views. **Notes** is a WYSIWYG markdown editor. **Transcript** has the audio player, speaker relabel, line edits, VTT export and engine/download notices. **Enhanced** shows AI blocks colored by provenance, with Keep / Remove and source previews. The Ask composer docks in the same floating switcher (see [ui-ask](ui-ask.md)): Insert adds an AI block with its sources (`insert_ask_answer`), an Ask citation chip opens this note's transcript through `zoom` (another note's goes to `App` via `onOpenSourceElsewhere`, and comes back as `pendingZoom`), and `fotw:open-ask` opens the sheet. Untrusted content (AI output, imported notes) is rendered through `safeContent.ts` / `markdownSafe.tsx`, which never fetch remote images and never navigate the app window (THREAT_MODEL T1/T2).

## Files it owns
- `frontend/src/components/Editor.tsx` — container: autosave, Keep-local chip, view switcher, audio player, video strip/lightbox, screen-capture menu, Ask wiring.
- `frontend/src/components/NotesEditor.tsx` — contentEditable notes stored as markdown (marked → DOMPurify on load, turndown on edit).
- `frontend/src/components/EnhancedDoc.tsx` — provenance-colored blocks, legend, Keep / Remove with undo, source chip preview / zoom-in, block edit.
- `frontend/src/components/TranscriptPanel.tsx` — stage banner, engine / download notices, AI-cleaned ⇄ Raw, speaker menu, inline segment edit, VTT export, re-run.
- `frontend/src/enhancedTrust.ts` — pure provenance rules and copy.
- `frontend/src/safeContent.ts` — `isSafeMediaSrc`, `isSafeExternalHref`, `openExternal`, `installLinkGuard`, `sanitizeHtml` (DOMPurify + hook).
- `frontend/src/components/markdownSafe.tsx` — react-markdown overrides `SAFE_IMG`, `SafeLink`, `SAFE_A` (also used by Ask).
- `frontend/src/components/DateTimePicker.tsx` — meeting date/time editor (local time in, UTC ISO out).

## Surface area
### Public API
| Export | File | Notes |
|---|---|---|
| `Editor` (default) | `Editor.tsx:944` | Props `Editor.tsx:54-100`. Rendered by `App.tsx:1018`. Relabel, segment edits and (re)transcribe are callbacks that App turns into commands (`App.tsx:726`, `:820`, `:829`). |
| `NotesEditor` (default) | `NotesEditor.tsx:126` | Uncontrolled. Reloads only when `revision` changes. Emits markdown 400 ms after the last edit (`:155-161`). |
| `EnhancedDoc` (default), `REMOVE_UNDO_MS = 5000` | `EnhancedDoc.tsx:416`, `:42` | |
| `TranscriptPanel` (default), `isGenericLabel` | `TranscriptPanel.tsx:359`, `:47-49` | `isGenericLabel` mirrors the backend's generic-label rule. |
| `DateTimePicker` (default) | `DateTimePicker.tsx:27` | `onSave(isoUtc)`. Seconds are reset to 0 (`:75-79`). |
| `blockTrust`, `trustSummary`, `trustLine`, `trustSubline`, `needsReview`, `REVIEW_COPY`, `EARLY_COPY`, `enhancedBeforeTranscript`, `sourceQuotes`, `sourceTarget`, `templateSections`, `CitedTranscript` | `enhancedTrust.ts` | The cited set is the **raw** transcripts of all the note's meetings (`:7-24`). A source counts if any of them has the id (`:70`). "Missing" only when every meeting has a transcript (`:71`). Nothing is downgraded while loading (`:73`). |
| `isSafeMediaSrc`, `isSafeExternalHref`, `openExternal`, `installLinkGuard`, `sanitizeHtml`, `BLOCKED_IMG_ATTR` | `safeContent.ts:13-147` | See the invariants. |
| `SAFE_IMG`, `SafeLink`, `SAFE_A` | `markdownSafe.tsx:9`, `:18`, `:44` | |

Provenance looks (`EnhancedDoc.tsx:92-102`, legend `:191-255`):
| Trust | Card | Keep / Remove |
|---|---|---|
| `user` (yours, edited or kept) | plain ink, transparent rule | no |
| `sourced` (AI, a cited id resolves, or can't check yet) | violet-soft + citation chip | no |
| `unsourced` (AI, no ids, transcript exists) | warning-soft + "No source" | yes |
| `missing` (AI, all cited ids gone, every meeting transcribed) | warning-soft + "Source not found" | yes |
| `notes` (no transcript, or enhanced before one existed) | neutral | no |
| `pending` (no ids, still loading) | neutral | no |

Keep calls `keep_note_block` and the block becomes yours (`:496-509`). Remove hides the line behind "Removed · Undo" for 5 s, then calls `edit_note_block(…, "")`. Deletes are serialized. A note switch or unmount cancels the remove (`:446-454`, `:511-559`). The source chip previews up to 3 raw quotes on hover/focus. A click zooms to the meeting that holds most of the cited lines (`sourceTarget`) and opens that recording if it isn't the one shown (`Editor.tsx:1198-1205`).

### Tauri commands
- Calls (Editor): `get_app_setting` / `set_app_setting` (`ui.ask.pinned`), `get_meeting_privacy`, `set_note_keep_local`, `update_meeting_started_at`, `list_capture_windows`, `ensure_video_thumbnail`, `update_note_title`, `update_note_scratchpad`, `attach_file`, `remove_attachment`, `enhance_note`, `copy_note_markdown`, `export_note`, `update_meeting_attendees` ("Someone else…"), `open_attachment`, `reveal_attachment`, `reveal_data_dir`.
- Calls (EnhancedDoc): `get_meetings_for_note`, `get_transcript`, `keep_note_block`, `edit_note_block`.
- Calls (TranscriptPanel): `export_transcript_vtt`. Relabel, segment edit and re-run reach `relabel_speaker`, `edit_transcript_segment`, `transcribe_meeting` through `App.tsx` callbacks.
- Calls (safeContent): `open_external_url` (`safeContent.ts:39`).
- Backend owners: `app-notes-library`, `app-privacy`, `app-recording`, `app-pipeline`, `app-llm`, `app-screen-recording`, `app-shell`, `app-settings-consent`.
- devMock drift: `attach_file`, `remove_attachment`, `open_attachment`, `reveal_attachment` and `open_external_url` aren't mirrored. They fall through to `default: return null` (`devMock.ts:1507-1508`), so on port 1420 `removeAttachment` passes `null` to `onNoteChanged` (`Editor.tsx:1133-1136`) (inferred breakage). devMock's `convertFileSrc` returns the path unchanged (`devMock.ts:1615-1617`), so asset URLs (audio, video, thumbnails) are never exercised in jsdom.

### Events
| Event | Emits / listens | Payload | Counterpart |
|---|---|---|---|
| `fotw:privacy-changed` (window) | Emits after a Keep-local toggle (`Editor.tsx:1051`). Listens to re-read privacy (`:1038`). | none | Sidebar folder keep-local, Settings privacy (`privacy.ts:14`) |
| document `click` / `auxclick` (capture) | `installLinkGuard` (`safeContent.ts:46-76`, installed in `main.tsx:15`) | — | Every `<a href>` in the app. |

### Settings keys
| Key (or prefix) | Read / write | Webview-settable? (`is_webview_setting_key`) | Meaning |
|---|---|---|---|
| `ui.ask.pinned` | read + write (`Editor.tsx:995-1006`) | Yes (`ui.`) | Ask sheet pinned beside the note (wide windows). |

localStorage keys: none here. devMock toggles that change these views: `fotwMockNoTranscript`, `fotwMockEngineMissing`, `fotwMockEngineUnmanaged`, `fotwMockAudioDeleted`, `fotwMockImportFail`, `fotwMockImportPipelineFail` (`devMock.ts:11-62`).

### Database
None directly. Notes, blocks, transcripts and privacy rows are read and written through the commands above (`fly-storage`).

### Files and directories written
None from the webview. The backend writes `export_note` / `export_transcript_vtt` files to a path you pick in a native save dialog, attachments under `<data_dir>`, and video posters via `ensure_video_thumbnail`.

### Network egress
| Host | When | Gate (consent / keep-local / user action) |
|---|---|---|
| None from the webview | CSP `img-src`/`media-src` allow only `'self'`, `asset:`, `asset.localhost`, `data:`, `blob:` (`src-tauri/tauri.conf.json:36`). Links open in the system browser via `open_external_url`. | user click |
| Cloud LLM (indirect) | Enhance (`enhance_note`) | consent + Keep local in `build_provider` (backend) |

### External processes
None from the webview. The OS browser opens links (backend `open_external_url`); the native save dialog opens for export.

### Secrets and keychain entries
None.

## Dependencies
- **Inbound:** `App.tsx:1018` renders `Editor` (it owns note, meetings, transcripts, pipeline state and the relabel / edit / transcribe commands). `markdownSafe.tsx` is also used by `AskPanel.tsx:31`. `installLinkGuard` is installed by `main.tsx:15`.
- **Outbound:** `ui-ask` (`AskComposer`, `AskSheet`, `useAskChat`, `useWideWindow`), `ui-attendees` (`AttendeeEditor` in the meta row), `ui-import` (`ImportQueue`), `LivePane`, `RecordingBar` (`fmtElapsed`), `pipelineNotice.ts` (`selectPipelineNotice`, `briefError`), `privacy.ts`, `ui.tsx`, `confirm.tsx` (re-run), npm `marked`, `turndown` (+ gfm), `dompurify`, `react-markdown`, `remark-gfm`, `@tauri-apps/api` (`convertFileSrc`, `invoke`).

## Invariants
- **No remote image is ever fetched (THREAT_MODEL T1).** react-markdown: `SAFE_IMG` renders `<img>` only for `isSafeMediaSrc` sources (`data:image/(png|jpeg|gif|webp|avif|bmp)`, `asset:`, `http(s)://asset.localhost/`, `blob:`). Anything else becomes the text placeholder "[image: alt]" (`markdownSafe.tsx:9-16`; `safeContent.ts:13-23`). Notes HTML: the DOMPurify `afterSanitizeAttributes` hook strips unsafe `src`/`poster`/`href`/`xlink:href` (non-anchors) and swaps blocked `<img>` for a placeholder that keeps the URL, so turndown restores `![alt](url)` (`safeContent.ts:89-126`; `NotesEditor.tsx:38-47`). `style`, `srcset`, `ping`, `background`, `formaction` and `<style>/<svg>/<math>/<video>/<audio>/<iframe>/<form>/<object>/<embed>` are forbidden (`:128-146`). Backstop: CSP `img-src` (`tauri.conf.json:36`).
- **Links never navigate the app window (T1/T5).** `SafeLink` renders a link only for `http:`/`https:`/`mailto:` (`markdownSafe.tsx:18-42`; `safeContent.ts:26-34`). `installLinkGuard` cancels every anchor click and hands the raw `href` to `open_external_url`, which checks it again in Rust. In the notes editor a plain click places the caret; Ctrl/Cmd+click follows (`safeContent.ts:53-67`). Middle-click is cancelled (`:69-75`). `sanitizeHtml` drops unsafe anchor hrefs and `target` (`:119-125`).
- **No raw HTML from markdown (T2).** `react-markdown` runs without `rehype-raw` (`EnhancedDoc.tsx:112-118`, `AskPanel.tsx:694`). Notes HTML always goes through `sanitizeHtml` before `innerHTML` (`NotesEditor.tsx:49-56`, `:150`). Highlights move DOM nodes and never re-parse selected text as HTML (`NotesEditor.tsx:216-230`).
- **Sources are checked against the raw transcripts of every meeting of the note** (`enhancedTrust.ts:7-24`; `EnhancedDoc.tsx:120-188`). Editor passes the raw `transcript`, not the cleaned one (`Editor.tsx:1526`). While the meeting list or any transcript is loading, nothing is flagged (`EnhancedDoc.tsx:176-187`).
- **AI lines are never called "made up".** The copy says "No source" / "Source not found" (`enhancedTrust.ts:106-155`).
- **Remove deletes only after the full 5 s wait.** A note switch, unmount or Undo cancels it (`EnhancedDoc.tsx:431-454`).
- **Re-run transcription asks first** (danger tone) because it replaces line edits and renames (`TranscriptPanel.tsx:609-627`).
- **Keep local** is shown and toggled here but enforced in `build_provider` / the pipeline (spec §2 item 1). A folder-inherited Keep local can't be turned off from the chip (`Editor.tsx:744-755`, `:1044-1053`).
- Sentence case, "you", tokens and primitives from `index.css` / `ui.tsx` (spec §2 item 8).

## Tests
| Kind | File | Tests | Covers |
|---|---|---|---|
| vitest | `frontend/src/components/EnhancedDoc.test.tsx` | 26 | legend, unsourced/missing/notes/early states, Keep focus moves, Remove undo/serialize/cancel/failure, source preview + Esc, multi-meeting union, no false "missing" while loading |
| vitest | `frontend/src/enhancedTrust.test.ts` | 19 | trust rules, legend copy, `templateSections`, `sourceQuotes`, `sourceTarget`, re-recorded notes, enhanced-before-transcript |
| vitest | `frontend/src/components/Editor.enhanced.test.tsx` | 3 | last Remove returns to Notes; source chip opens the earlier recording and scrolls; a devMock self-check |
| vitest | `frontend/src/components/TranscriptPanel.test.tsx` | 2 | Re-run asks first; Cancel/yes |
| vitest | `frontend/src/safeContent.test.ts` | 4 | `isSafeMediaSrc` / `isSafeExternalHref` only (node env, no DOM) |
| vitest (Ask in Editor) | `frontend/src/components/Editor.ask.test.tsx` | 3 | counted under [ui-ask](ui-ask.md) |

**Known gaps:**
- **`sanitizeHtml` (the DOMPurify hook) has no test at all.** Neither does the blocked-image placeholder round trip through turndown, `installLinkGuard`, `SAFE_IMG` or `SafeLink`. `safeContent.test.ts` runs in the node environment, where DOMPurify can't run. No test feeds a real payload (remote `<img>`, `srcset`, `<svg><image href>`, `style="background:url()"`, `javascript:` link, `<img onerror>`, HTML inside markdown) to any renderer. T1/T2 rest on untested code (DOMPurify had hook-bypass advisories, `docs/SECURITY_AUDIT_2026-10.md` line 14).
- Link-text spoofing: `SafeLink`'s tooltip is the markdown title when there is one (`markdownSafe.tsx:33`, `title={title ?? href}`), so AI output can show a fake URL on hover. Untested.
- `TranscriptPanel`: no test for the speaker menu, "Someone else…", segment edit on blur, AI-cleaned ⇄ Raw, VTT export (the `cleaned` flag), the engine-missing / download-failed notices in the component, or the highlight scroll. (The notice selector is unit-tested in `pipelineNotice.test.ts`.)
- Editing a line while "AI-cleaned" is shown writes the polished text into the **raw** transcript too (`TranscriptPanel.tsx:691-695` → `fly-storage` `transcripts.rs:181-203`). That raw text is what Enhance sources quote. Untested.
- `NotesEditor` (autoformat, task checkboxes, highlight, link insert) and `DateTimePicker` (local→UTC, 12 AM/PM, DST) have no tests.
- Editor's Keep-local chip, autosave debounce, copy/export feedback and attachment removal (no confirm) have no tests.
- `window.prompt` is still used twice: "Add link" (`NotesEditor.tsx:205`) and "Region…" screen capture (`Editor.tsx:481`). tauri-plugin-dialog overrides only `alert` and `confirm` (checked in its init script, v2.8.3), so these fall to WebView2's own prompt. Whether that shows in the app is unverified. Lint doesn't ban `prompt`, and no test stubs it.

## Changing it safely
- Run `npm --prefix frontend test`, `npm --prefix frontend run lint`, and `tsc` (part of `npm --prefix frontend run build`).
- Any new place that renders AI or imported content must use `SAFE_IMG` / `SafeLink` (react-markdown) or `sanitizeHtml` (HTML). Never add `rehype-raw`, `dangerouslySetInnerHTML` or `innerHTML` without `sanitizeHtml`. Keep CSP `img-src` / `media-src` as they are (spec §2 item 3).
- If you change `isSafeMediaSrc` or the DOMPurify config, add jsdom tests first (`// @vitest-environment jsdom`). Today nothing would catch a regression.
- Enhance's cited ids must stay raw-transcript segment ids. If PR #92's re-anchoring changes ids on re-transcribe, update `enhancedTrust.ts` and the "missing" copy together.
- Keep `isGenericLabel` in step with the backend's generic-label rule.
- New commands called from here need an `api.ts` wrapper, a `lib.rs` registration and a devMock case (spec §2 item 4). devMock returns `null` silently for unknown commands.
- Replace the two `window.prompt` uses with an in-app input, then ban `prompt` in `eslint.config.js` like `confirm`/`alert`.
