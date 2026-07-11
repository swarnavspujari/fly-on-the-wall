# Fly on the Wall design system

`Looma.dc.html` is the Claude design export this UI is built from (open it in a browser —
`support.js` is its tiny runtime). It is the visual source of truth: a warm cream/coral
language with Bricolage Grotesque display type and Spline Sans body type.

## Tokens

The tokens live in `frontend/src/index.css` under `@theme` and are the only place colors
are defined; components use Tailwind classes derived from them (`bg-coral`, `text-ink`,
`border-line`, …).

| Token       | Value                | Role                                  |
| ----------- | -------------------- | ------------------------------------- |
| `cream`     | `#FAF6F1`            | app background                        |
| `surface`   | `#FFFFFF`            | cards, editor, panels                 |
| `shell`     | `#F2EAE0`            | window chrome (sidebar, bars, footer) |
| `peach`     | `#FBEADF`            | selection/hover fill, AI tint         |
| `peach-2`   | `#FDF3EC`            | subtle fill                           |
| `coral`     | `#E86A4A`            | accent (buttons, active states)       |
| `clay`      | `#A64A2E`            | accent text on peach                  |
| `ink`       | `#2A2320`            | primary text                          |
| `ink-2`     | `#5C524B`            | secondary text                        |
| `mute`      | `#94897F`            | tertiary text                         |
| `line`      | `rgba(60,40,30,.10)` | borders                               |
| `line-2`    | `rgba(60,40,30,.05)` | hairlines                             |
| `rec`       | `#E23B3B`            | live-recording red                    |
| `spk-*`     | teal/violet/blue/…   | speaker rotation (self is coral)      |

Fonts are bundled via `@fontsource/*` (no CDN — the app is offline-first).

## Conventions

- Display type (`font-display`) for view titles and the wordmark only; everything else is
  Spline Sans.
- Radii: 8–12px for controls, 14–18px for cards, `rounded-2xl` for modals.
- Provenance: the user's own words render as plain ink; AI-written blocks are peach-tinted
  with clay accents and numbered citation chips that jump to their transcript segments.
- Shared primitives (`Btn`, `SectionLabel`, `ModalShell`, speaker colors) are in
  `frontend/src/components/ui.tsx`.
