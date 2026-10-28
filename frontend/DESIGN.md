# Fly on the Wall — frontend design foundation

This session wired the **Fly on the Wall design system** (violet + yellow + ink) into the
frontend as the single source of truth. It is a **restyle foundation**: tokens, fonts, brand
assets, theme wiring, and the shared primitives. Individual screens are **not** restyled yet —
they keep rendering via temporary compatibility aliases (see below). This doc is the map the
screen sessions follow.

Source of truth for the design: `…/Fly on the Wall — Design System-handoff/fly-on-the-wall-design-system/project/`
(`readme.md`, `tokens/*.css`, `components/**`, `ui_kits/desktop-app/`).

---

## Architecture

```
tokens/*.css  ──ported verbatim──►  src/index.css  :root (light) + [data-theme="dark"]
                                          │  (raw CSS custom properties = the truth)
                                          ▼
                          @theme { --color-*: var(--…) }   ← Tailwind utilities are token-mapped
                                          │
                    ┌─────────────────────┴─────────────────────┐
                    ▼                                            ▼
        src/components/ui.tsx                         screens (App.tsx, components/*)
   12 primitives, token-driven injected CSS      compose Tailwind utilities (bg-surface,
   (.fly-* classes read var(--…))                text-text-2, border-line, …) → all var()-backed
```

- **`src/index.css`** is the single source of truth. The design tokens live as raw CSS variables
  in `:root` (light) and `[data-theme="dark"]` (dark), ported verbatim from `tokens/*.css`. The
  `@theme` block maps Tailwind color/font names onto those variables, so **every utility is
  theme-aware** (switches with `data-theme`).
- **`src/components/ui.tsx`** holds the 12 primitives. They carry their own token-driven CSS
  (injected once as `<style id="fly-ds-css">`) — the same approach the design ships, because
  hover/focus-visible/checked/placeholder/keyframe states can't be inline styles.
- **Fonts** are self-hosted via `@fontsource/*` (no CDN) so the app stays offline: Bricolage
  Grotesque (display), Spline Sans (text), Spline Sans Mono (mono).

---

## Theme (default = system, follows the OS)

`src/theme.ts` `useTheme()` mirrors the design's `app-main.jsx`: defaults to `"system"`, follows
`prefers-color-scheme`, persists to `localStorage["fotw-theme"]`, sets `data-theme` on `<html>`.
It is **not** hard-pinned. A pre-paint script in `index.html` sets `data-theme` before React
mounts (no flash). `App.tsx` consumes `resolved` and passes it to `Sidebar`, which swaps the
wordmark to `-logo-dark.svg` on the ink shell.

**Next session:** wire a Settings › Appearance segmented control (System / Light / Dark) to
`setTheme` from `useTheme()` — the plumbing is already there; Settings just needs the control.

---

## Token → Tailwind utility reference (the vocabulary to adopt)

All are `var()`-backed and theme-aware. Use these semantic names in new/restyled screens.

| Role | Utility (e.g.) | Light | Dark |
|---|---|---|---|
| App canvas | `bg-bg` | cream `#F2F2E8` | ink `#0D0D12` |
| Card/panel | `bg-surface` | `#FFFFFF` | `#17161F` |
| Subtle fill | `bg-surface-2` / `bg-surface-3` | `#FBFAF4` / `#F4F2E9` | `#201F2A` / `#2B2B3C` |
| Window chrome | `bg-shell` | `#EAE9DC` | `#121118` |
| Primary text | `text-text` | `#16151C` | `#F2F2E8` |
| Secondary / muted | `text-text-2` / `text-text-3` | `#4C4B57` / `#66646F` | `#C3C1CE` / `#A29FAE` |
| Primary (fills) | `bg-primary text-on-primary` | `#6A4AE0` + white | `#A896FF` + ink |
| Violet as text/link | `text-primary-text` | `#5B3FD9` | `#B7A8FF` |
| Soft violet chip | `bg-primary-soft text-primary-soft-text` | `#EEEBFE` / `#5133C2` | `#201B33` / `#C3B4FF` |
| Yellow accent (fills only) | `bg-accent text-on-accent` | `#FFD94A` + ink | same |
| Highlight (marks) | `bg-highlight text-on-highlight` | `#FFE9A6` | translucent yellow |
| Hairlines | `border-line` / `border-line-2` / `border-line-strong` | ink @12/6/26% | cream @14/7/30% |
| Recording / error | `bg-rec text-on-rec` | `#D6342E` + white | same |
| Status | `text-success-text bg-success-soft`, `…-warning-…`, `…-error-…`, `text-info` | see `index.css` | see `index.css` |
| Speakers | `bg-spk-self/-teal/-blue/-amber/-pink/-green/-indigo text-on-speaker` | self = violet | rotation + white |
| Brand (fixed) | `bg-brand-ink/-violet/-yellow/-cream/-slate` | literal brand hexes | same |
| Fonts | `font-sans` / `font-display` / `font-mono` | Spline / Bricolage / Spline Mono | same |

Raw tokens also available as CSS vars for inline styles: `--radius-{xs,sm,md,lg,xl,2xl,3xl,pill}`,
`--space-*`, `--shadow-{xs,sm,md,lg,pop}`, `--focus-ring`, `--dur-*`, `--ease-*`, layout widths
`--sidebar-w 240 / --notelist-w 288 / --askpanel-w 320 / --content-max 760`.

### Legacy aliases — TEMPORARY (remove when screens migrate)

Old cream/coral class names still used by not-yet-restyled screens now resolve to the new
theme-aware tokens (defined in the `@theme` "LEGACY ALIASES" block of `index.css`):

| Old class | Now resolves to | Old class | Now resolves to |
|---|---|---|---|
| `bg-cream` | `--bg` | `text-ink` | `--text` |
| `bg-coral` / `border-coral` | `--primary` | `text-ink-2` | `--text-2` |
| `text-clay` | `--primary-soft-text` | `text-mute` | `--text-3` |
| `bg-peach` | `--primary-soft` | `bg-peach-2` | `--surface-3` |
| `shadow-warm` | `--shadow-lg` | `shadow-card` | `--shadow-sm` |
| `spk-violet` | `--spk-indigo` | (surface/shell/line/rec keep their names) | |

Legacy keyframes `pulse-dot` / `wave` / `fade-up` / `shimmer` are also kept (the primitives use
`fly-*` versions). **When a screen is restyled**, replace its legacy classes with the semantic
names above and drop the animation-name references; once no screen uses them, delete the
`LEGACY ALIASES` block and the unprefixed keyframes from `index.css`.

> ⚠️ Known transitional wart: the old `ink` name was overloaded — `text-ink` (themeable text)
> and `bg-ink` (an always-dark surface: the recording-bar warning strip, old modal scrim). The
> alias maps `ink → --text`, so `bg-ink` is correct in light but wrong in dark (cream strip).
> It only shows while recording and is fixed when `RecordingBar`/scrims are restyled — use an
> always-dark token there (`bg-brand-ink` or `var(--text)`-independent) rather than `bg-ink`.

---

## Primitives (`src/components/ui.tsx`) — 12 components + shims

Import from `./ui` (or `../components/ui`). All are token-driven and theme-aware.

| Component | Key props |
|---|---|
| `Button` | `variant` primary·soft·outline·ghost·contrast·record · `size` xs·sm·md·lg · `startIcon`/`endIcon` |
| `SectionLabel` | uppercase micro-label · `as` (default `p`) |
| `Card` | `tone` surface·muted·invert · `pad` none·sm·md·lg · `radius` md·lg·xl · `interactive` |
| `Input` | `label` `hint` `error` `invalid` + native input props |
| `Select` | `options=[{value,label}]` or `<option>` children · custom chevron |
| `Checkbox` | `type` checkbox/radio · `label` `description` |
| `Badge` | `tone` neutral·primary·success·warning·danger·accent·**live** · `size` sm·md · `uppercase` `dot` |
| `ProgressBar` | `value` 0–100 (omit = indeterminate) · `size` sm·md |
| `RecordingIndicator` | `state` recording·paused · `elapsedMs` · `showWave` `showTimer` · `size` |
| `Avatar` | `name` `self` `index`/`colorKey`/`color` · `shape` circle·square · `size` xs–lg |
| `CitationChip` | `count` or `label`/children · `onClick` (button when handler given) |
| `Modal` | `open` `onClose` `title` `footer` `width` `closeOnOverlay` (Esc/click-out close) |

**Backward-compat shims (deprecated — remove after screens migrate):** `Btn` (wraps `Button`,
maps `variant="dark"` → `contrast`), `ModalShell` (overlay+card, theme-correct scrim, width
still from `className`), `speakerColor(key,index)` (mic/you/self = violet, else rotation),
`speakerInitials(label)`.

**Icons:** `lucide-react` is installed and is the intended icon set (24px, 1.75 stroke). Screens
currently draw CSS shapes/emoji; swap to Lucide during restyle. The primitives keep the design's
inline SVGs for chevron/check/close (matches the design exactly).

---

## Brand assets & app icon

- SVGs copied to `frontend/src/assets/brand/`: `fly-on-the-wall-logo{,-dark,-violet}.svg`,
  `fly-on-the-wall-mascot.svg`, `fly-icon-{cream,ink,violet}.svg`. Import as URLs
  (`import logo from "../assets/brand/…svg"`). **Use `-logo-dark.svg` on the ink shell**
  (the base logo's black "FLY" disappears on dark). Never redraw the mark.
- App icons regenerated from the mascot tile (`fly-icon-ink.svg`, the default): rasterized to
  `assets/icon-source.png` (1024²) and run through `tauri icon`, replacing everything under
  `src-tauri/icons/`. The stale `assets/looma-icon.png` was removed.

---

## Golden rules (brand guide)

Sentence case · address the user as **you** · violet is primary · **yellow is highlight-only,
always with ink text** · near-flat hairline cards, big lift only on modals/popovers · **3px
violet focus ring on every control** · **recording state is never ambiguous**.

---

## Screen restyle status (session 2B — done)

All screens are restyled to the design's desktop-app recreation, using the 2A primitives + the
composed-pattern recipes: `Sidebar` (Lucide nav, Up-next, folder **color+emoji picker** + **drag-drop
filing**), `NoteList` (styled search, Avatar rows, drag source), `App`/`RecordingBar` (chrome + pinned
recording bar with the ink "system output muted" strip), the `Editor` cluster (`Editor` +
`TranscriptPanel` + `EnhancedDoc` + `AskPanel` + `LivePane`: the Granola single content region + floating
Notes⇄Transcript⇄Enhanced switcher, transcript waveform + inline speaker rename, Enhanced blocks with a
`CitationChip`, ephemeral Ask), `FirstRunNotice` (consent Modal), `SettingsModal` (Simple/Technical +
Appearance + tiers + providers + models + calendars + MCP). `UpdateBanner` moved onto `Notice` in the
app `NoticeStack` (v2.4 PR 1.3), so no component uses the legacy aliases now.

- Settings › Appearance is wired to `useTheme().setTheme`; `theme.ts` is now a **shared store**
  (`useSyncExternalStore`) so every consumer stays in sync (the sidebar wordmark variant updates live).
- Restyled screens migrated off the legacy aliases; the alias/keyframe/`Btn`/`ModalShell` shims remain
  only for the few holdouts — trim them once nothing references them.

### Dev-only mock backend (`src/devMock.ts`)

`main.tsx` installs a Tauri IPC mock **only in a plain-browser dev context** (`import.meta.env.DEV` +
no `__TAURI_INTERNALS__`); it is a first, self-guarding side-effect import so it binds the IPC before
`@tauri-apps/api` loads, and is dead-code-eliminated from production. It returns fixtures mirroring the
design's `data.js` so the whole UI renders (with a clean console) for `vite`-served visual QA via a
browser. Toggles: `localStorage.fotwMockRecording="1"` (active capture) / `fotwMockFirstRun="1"` (consent
modal). It never runs in the native app. Keep it for frontend dev, or drop it if undesired.
