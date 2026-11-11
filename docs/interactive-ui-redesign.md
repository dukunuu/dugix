# Interactive UI: the Bold redesign and what comes next

This records the original Bold restyle and its layout, live-interactivity,
component and motion additions. The follow-up below supersedes the original
presentation defaults; the component language and interaction model remain.

## Current presentation: useful tools, quieter defaults

The Bold pass made the widget vocabulary consistent, but still gave ordinary
calculators and quizzes the visual weight of dashboards. The follow-up:

- Sentence-case, unfilled panel headers; no block-glyph button caps.
- Slim progress tracks, explicit slider thumbs and sentence-case labels.
- Related actions remain together in source order, matching keyboard order.
- `Number` uses readable bold text with units, not three-row ASCII digits.
  Explicit legacy `Stats(..., true)` remains available, but isn't recommended
  for ordinary interactive tools.
- Model guidance favors one task, meaningful controls and an immediate result;
  no invented metrics, repeated headings or mandatory follow-up menus.
- Prompt examples for a planner, bill splitter and quiz are rendered in
  `crates/tui/src/interactive/reference_snapshots.txt` at 32 and 72 columns.
  Their tests cover dark/light/transparent themes, local calculation/reset,
  quiz feedback and stable transcript reservations. The component gallery
  remains the compatibility net for the larger vocabulary.

Blank rows reserved for quiz answers and taller tab branches are intentional:
revealing content must not push the following transcript around.

For the native component-library design lab, run
`scripts/preview-terminal-panels.sh`. Pick any of the 72 contracts and compare
Field Notes / Instrument / Playful options side by side (57 component families,
171 authored options, compact/wide and default/focused renders). Previews use
actual terminal cells, with signatures and copyable native source. Choices stay
in the page's memory and do not apply to Nexus. Full overview toggles the original
197×60-cell contact sheet; `--terminal` prints that ANSI frame in a sufficiently
large terminal. This is a design mockup, not a production renderer or live chat
mode.

## Original decisions (user, 2026-11-08)

- **The look is "Bold".** Panels get a filled title bar in the panel's accent
  color, big display numerals for headline numbers, filled pill controls, and
  tighter spacing. The rounded box goes away.
- **All four additions are in scope:** layout primitives, live interactivity,
  new components, and motion polish.
- **Canvas** (braille shapes plus `Anim` keyframes) landed the same day. The
  braille rasterizer and the per-panel clock it added are reused below.

## What is wrong today

These come from rendering every component at 72 columns
(`entry.canvas(72, …)`).

- **Three button styles.** `[ Create account ↵ ]` in forms, bare coloured text
  in `Buttons` (` Reset … Save plan ↵ `), and numbered rows (` 1  Lisbon … → `)
  for answer lists.
- **Four ways to label a control.** `Days  ◀ 5 ▶`, `Plan: < Pro >`,
  `Experience: ★★★○○`, `Volume: 40  ───●───`. They use different separators
  and have no shared label column.
- **Striped bars.** `Progress` and `Bars` repeat `▍`, so every bar reads as
  stripes, and they have no track.
- **A broken Meter.** The label is printed over the middle of a `█` bar:
  `█████CPU 0.72/1 ████`.
- **A ragged sparkline.** It mixes `▍` and `▎` instead of the `▁▂▃▄▅▆▇█` ramp.
- **Tabs with no active state.** Tab labels are spread evenly across the full
  width, nothing marks the active one except color, and the strip runs straight
  into the table's header row.
- **A plain table.** It has no header rule and no zebra or selection fill, and
  the columns stretch across the whole width.
- **A weak Hero.** The title is plain, and the badge floats at the far right
  edge.
- **Blank reserved rows.** Tabs and Quiz reserve their tallest branch, which
  shows up as empty space under short content. That reservation is correct:
  the transcript must not jump. But nothing fills the space or signals that it
  is reserved.
- **Bracketed badges.** `[BETA]` should be a filled pill.

## The Bold design language

Everything lives in `crates/tui/src/ui/style.rs`, next to the existing glyph
vocabulary, so the transcript, popups and panels can't drift apart. The color
roles do not change. The panel's own `accent` (default `accent2`, the
assistant) is the bold color. The user's `accent` still means "interactive /
waiting on you".

### Tokens

- **`Theme::on(fill)`** picks readable text for a filled background: `bg` or
  `fg`, whichever contrasts more. For RGB themes it compares relative
  luminance. For named ANSI colors it uses `bg`, which works for the default
  theme's black-on-cyan. Every filled surface uses it, so light themes keep
  working.
- **Pill caps.** `▐` on the left and `▌` on the right, drawn in the fill color
  over the panel background. The cap's inner half is the pill's fill, so a
  one-row pill looks rounded: `▐ Save plan ↵ ▌`.
- **The eighth ramp** `▏▎▍▌▋▊▉█` handles sub-cell bar ends. The track is
  `░` in `border_dim`.
- **The vertical ramp** `▁▂▃▄▅▆▇█` is for sparklines and column bars.
- **The display font** is a 3-row half-block digit set (`0-9 . , % + - $ k M`).
  Every glyph is 3 columns wide plus 1 of spacing. It is used by big stats and
  the hero number. It is a `const` table, with no font files.

### Panel chrome

```
▐█ TRIP PLANNER ████████████████████████████ ⤢ █▌    title bar: fill = panel accent,
                                                     text = Theme::on(fill), bold caps
  Days        ◀  5  ▶                                 content inset 2, no side borders
  ● Beach 2d  ○ Hiking 3d

  PLANNED     ████████████▋░░░░░░░░░░░░░░    2 / 5

  ▐ Reset ▌  ▐ SAVE PLAN ↵ ▌
──────────────────────────────── Tab next · ↵ press   bottom rule: border_dim + hint
```

- The title bar replaces the top border. The bottom border becomes a single
  `─` rule that carries the hint. Heights stay the same: one chrome row on top
  and one on the bottom, as today. So `card_height`, transcript reservations,
  and the viewport-pinning tests keep their arithmetic.
- `frame` keeps parsing. `rounded`, `double`, `heavy` and `dashed` all render
  the Bold bar and differ only in the bottom rule glyph (`─ ═ ━ ╌`). `plain`
  renders an unfilled title (accent text over a rule) for quiet layouts.
- The streaming preview (`streaming_buffer`), the error card (`card_buffer`),
  the pinned overlay, and the expanded popup all use the same chrome function.

### Components

| Component | Bold treatment |
|---|---|
| Hero | Bold title in the panel accent. The badge becomes a filled pill directly after the title. The subtitle is dim, with no blank row before it. |
| Stats | The compact form keeps 3 rows, in a label-over-value grid with a tone-colored delta. A new `big?` flag uses display numerals (5 rows: 3 digits, label, delta). |
| Progress, Meter, Bars | Shared label column (dim caps), an eighth-ramp bar in the tone color on a `░` track, and the value right-aligned in its own column. The label is never drawn over the bar. |
| Sparkline | The `▁…█` ramp, 1–3 rows tall, in the tone color. |
| Buttons, Form submit | One pill style. Primary: filled panel accent. Secondary: filled `control`. Answer lists: full-width rows with the number in a small pill and `→` on the right. Disabled: dim, unfilled. |
| Stepper, Select | `◀ value ▶`, with the value bold and the arrows dim until focused. |
| Toggle, Checklist | `●`/`○` stays. The label column lines up with the other controls. |
| Slider, Rating | Slider gets the eighth-ramp fill plus a `●` thumb. Rating keeps `★`/`☆` in warning. Both use the shared label column. |
| Tabs | Each label is sized to its text. The active tab is a filled pill, inactive tabs are dim, and a `─` rule runs under the strip. |
| Table | Bold header with a `─` rule. Columns are sized to content (capped). Zebra rows on `raised`; the selected row on `selection` with a `▸`. |
| Callout | A tone rail `▌` plus a tinted band (`raised`); the title is bold in the tone color. |
| Cards | Header row filled with `raised`, the badge as a pill, the body below. The border is dropped in favor of the fill. |
| Choice, Quiz | Options as rows. The picked option is filled; right and wrong turn success or error after the pick. The quiz header is a strip with `‹ 1 / 5 ›` and a score pill. |
| Badge | Filled pill in its tone. |
| Input, Textarea, Editor | The rounded field border stays (from the input research). The label sits above in dim caps; the focused border uses the panel accent. |
| Chart, Canvas | Braille, as landed. The legend gets tone-colored pills. |

### Focus

There is one focus treatment for everything, with no underlines. A filled
control that is focused gets bold text plus a `▸` in the gutter. An unfilled
control turns the user's `accent`. This keeps "you are here" in your color,
separate from the panel's own color.

## Additions

### Layout primitives

Today panels stack vertically. Only Columns, Cards and Stats put things side by
side, and only fixed item types.

- `Row(children, gap?, align?)` puts any components side by side. Widths are
  equal by default; `align` is `start | center | end | spread`.
- `Grid(children, cols?)` fills 2–4 columns, row by row.
- `Stack(children, gap?)` groups components so they can be nested and spaced.
- `Spacer(rows?)` adds space.

**Core.** These are new `Node` variants with child views, handled like `Tabs`
in `components::children`, `walk`, `max_visible`, and `tree_ok`. Their
`rows()` validation budget is the stacked worst case: the sum for `Row`, and
row-major sums for `Grid`. Then a narrow terminal that collapses a row into a
stack never exceeds what validation allowed. Nesting depth stays at 8 or less.

**TUI.** `branch_regions` learns horizontal splits. Below about 24 columns per
child, a `Row` stacks. The existing `reflow` keeps transcript reservations in
sync when the height changes.

Hit-testing and focus order follow reading order (left to right, then down).
`visible_regions` already returns regions in that order.

### Live interactivity

These turn panels into games, timers and simulations. Everything stays local
data: no new tool access, and no automatic sends.

- **Timers.** `@Every(seconds, action)` is a statement:
  `tick = @Every(0.5, @Set($t, $t + 1))`. Limits:
  - the interval is at least 0.1 s;
  - a panel has at most 4 timers;
  - timers pause off-screen and while the window is unfocused, like live-data
    refresh;
  - timers stop after 30 minutes idle;
  - timer actions may only use `set` and `reset` steps, never `to_assistant`
    or `answer`, so nothing can send on its own.
  The TUI drives timers from the same tick as Canvas.
- **Key bindings.** `Keys({left: action, right: action, space: action, …})`
  is active only while its panel is focused. The allowed keys are the arrows,
  space, a–z and 0–9. Tab, Esc and Enter are never bindable. Keys that a
  focused text field or editor uses are not overridden.
- **Clickable shapes.** Canvas shapes take an optional `action`. Hit areas are
  the cells a shape actually inked (taken from the rasterizer), checked
  topmost first.
- **State arrays.** State is scalar today. Snake, a board, or a kanban needs
  bounded arrays: at most 64 entries, scalars or flat objects. `@Set` gets
  `@Push`, `@Remove` and `@Update` helpers. This changes the data model, so it
  gets its own validation and persistence tests.

### New components

| Component | Notes |
|---|---|
| `Number(value, label, delta?, tone?)` | One big display numeral; shares the font with `Stats(big)`. |
| `Gauge(label, value, max, tone?)` | A braille half-ring. Needs a canvas `Arc` shape, which ships with it. |
| `Heatmap(values, labels?, tone?)` | A calendar or matrix grid of shade glyphs `░▒▓█` in the tone color. |
| `Tree(items)` | Nested `Node(label, children?)` with `▸`/`▾` expand state kept like the Accordion's. |
| `Board(columns)` | Kanban columns of cards. Display-only first; moving cards waits for state arrays. |
| `Diagram(source)` | Mermaid inside a panel, reusing the transcript's diagram renderer at a fixed height. |
| `Md(text, rows?)` | A Markdown block (bold, italic, code, lists, links) through `ui/markdown.rs`, clipped to its rows. |

### Motion

All of this rides on the per-panel clock and the frame tick that Canvas added.
The tick only runs while something visible is moving.

- **Value easing.** When a bound value changes, Progress, Meter, Bars, Slider
  and Stats glide to the new value over about 250 ms. View state keeps the
  last displayed value and when it changed.
- **Draw-in.** Charts, sparklines and canvases reveal left to right over about
  400 ms the first time they are shown. They never replay on scroll.
- **Pill press.** A clicked or Enter-pressed pill inverts for one frame.
- **`/motion on|off`** is a db setting, like `keymap`. With motion off, values
  snap, there is no draw-in, and canvas animations are frozen at time 0. This
  is the reduce-motion switch.

## Phases

Each phase lands on its own with a green `scripts/check.sh`. Model-facing text
(`interactive-ui.md`, `layouts.md`) changes only in the phase that ships the
feature, and the prompt snapshots are regenerated in that phase.

1. **Tokens and chrome.** `Theme::on`, pill/ramp/display-font helpers in
   `style.rs`, and the Bold title bar and bottom rule for every panel surface.
   Also a permanent **gallery snapshot test**: the 7-panel gallery used for the
   audit, written to `gallery_snapshots.txt` with the `NEXUS_UPDATE_SNAPSHOTS`
   switch. It is the visual regression net for everything after.
2. **Data viz.** Progress, Meter, Bars, Sparkline, Badge, Hero, Callout, Cards,
   the Chart legend, and `Stats(big)`.
3. **Controls.** The shared label column; pills for Buttons, Form submit and
   answer rows; Stepper, Select, Toggle, Slider and Rating; Tabs, Table,
   Choice and Quiz; one focus treatment.
4. **Layout primitives.** Row, Grid, Stack and Spacer, with responsive
   stacking.
5. **Motion.** Easing, draw-in, pill press, and `/motion`.
6. **Live interactivity.** Timers, then Keys, then clickable shapes, then
   state arrays.
7. **New components.** Number and Heatmap first, then Gauge (with Arc), Md,
   Tree, Diagram, and finally Board.

Phases 1–3 are the redesign and change no model-facing syntax except
`Stats(…, big?)`. Phases 4–7 each add syntax, and each adds about one
paragraph to the prompt at most.

## Constraints to keep

- **Heights are a contract.** `Node::rows()` is the validation budget the model
  is told about. A TUI height that differs from it must stay within it, and
  height changes go through `reflow`. The streaming viewport-pinning tests in
  `ui/history.rs` must keep passing.
- **No emoji.** All glyphs come from `style.rs`. The scan in `style.rs` bans
  U+1F000 and above under `src/ui`.
- **Transparent backgrounds.** Fills use `raised`, `control`, `selection` or a
  tone. Under a transparent background mode, `raised` is `Reset`, and the
  design must still read with that fill missing. Zebra rows and card headers
  are a bonus, never the only cue.
- **Core stays frontend-free.** Layout semantics, timers, keys, state arrays
  and limits live in `crates/core/src/interactive/`. Glyphs, fonts, fills and
  easing live in `crates/tui/src/interactive/`.

## Status

- **Phases 1–3 landed together** (2026-11-09) as one restyle commit: they
  rewrote the same component renders, so splitting them would have rewritten
  each test snapshot three times. A scratch PNG renderer, kept out of the
  tree, was used to review the look in dark and light themes.
- **Deviation:** table columns keep equal widths, not content sizing, so
  sorting clicks keep mapping columns the same way; the header fill and zebra
  rows give the structure instead.
- **Phase 4 landed** (2026-11-09): `Row(items, gap?)`, `Grid(items, cols?)`,
  `Stack(items, gap?)`, `Spacer(rows?)`. Rows stack below 20 columns per child,
  grids drop columns below 18. Containers and spacers don't count toward the
  16-node cap (their children do).
- **Phase 5 landed** (2026-11-09): glides (Progress, Meter, Bars, Slider;
  0.25 s ease-out), draw-in (Chart, Sparkline, Canvas; 0.4 s), a 140 ms press
  flash, and `/motion on|off` (db setting `motion`, on unless `off`). Motion
  state lives on `Session` (`interactive/motion.rs`) and is off unless a
  frontend turns it on, so previews, exports and tests see final frames.
  Stats numbers don't glide; the text always shows the true value.
- **Phase 6 landed** (2026-11-09): `@Every(seconds, action)` timers (≤4,
  ≥0.1 s, wall time, one catch-up run after a gap, live only while the window
  has focus and someone used it in the last 30 minutes), `@Keys({...})`
  bindings (focused panel, never while typing), and a trailing `action` on
  every Canvas shape (clickable, focusable, hit-tested on the cells it inks).
  Timer and key actions may only `@Set`/`@Reset` (directly or via called
  actions). Streaming previews drop timers and keys. The loop's panel save is
  throttled to once a second so a fast timer doesn't hammer the db; session
  switches and quitting still save immediately.
- **State arrays were already there:** state holds bounded data values and
  `@Each` re-renders from them, so phase 6 only added list editing:
  `@Append`, `@Remove`, `@Replace`, `@Slice` (negative indexes from the end).
- **Phase 7 landed** (2026-11-09): `Number`, `Gauge` (braille half-ring;
  canvas gained `Arc`), `Heatmap`, `Md`, `Tree` (`Branch` items; expands
  like Accordion sections, reserving its fully expanded height), `Board`
  (`Lane` items may be a `$state` list, so buttons move cards with
  `@Append`/`@Remove`), and `Diagram` (Mermaid, 12 rows, painted as an image
  by the transcript's diagram painter when the diagram starts at the panel's
  content edge; otherwise a text placeholder).
- **Fix found on the way:** `Widgets::clear` (every viewport reset, such as a
  session switch) reset `/vim` to its default; it now keeps vim and motion.
- **Deviation:** `Row` has no `align` argument. Children fill equal slots, so
  alignment would only matter with natural widths, which no component reports
  yet; add it if a real layout needs it.

## Resolved questions (decided 2026-11-09)

- **Static display layouts** get the Bold title bar too. `frame: "plain"`
  stays available for quiet answers, and the layouts prompt says when to use
  it.
- **Motion is on by default.** `/motion off` turns it off.
- **State arrays are in phase 6**, as its last step, after timers, Keys and
  clickable shapes. Board's card moving waits for them.
