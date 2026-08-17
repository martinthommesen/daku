# Plan 045: Restyle the Environment detail on gpui-component — header, status pills, Signal cards, compare strip

> **Executor instructions**: Follow this plan step by step. Run every
> verification command and confirm the expected result before moving to the
> next step. If anything in the "STOP conditions" section occurs, stop and
> report — do not improvise. When done, update the status row for this plan
> in `plans/README.md` — unless a reviewer dispatched you and told you they
> maintain the index.
>
> **Drift check (run first)**: `git diff --stat <sha of plan 044's last commit>..HEAD -- src/app.rs src/dashboard_state.rs`
> Plan 044 must be DONE (Root/TitleBar/Sidebar on gpui-component, no `src/theme.rs`). Any other in-scope change since is a STOP condition.

## Status

- **Priority**: P1
- **Effort**: M
- **Risk**: LOW (render-only; model contract unchanged)
- **Depends on**: plans/044-gpui-component-shell-and-pin.md
- **Category**: direction
- **Planned at**: commit `826a636`, 2026-08-17 (executor: re-check `src/app.rs` after 044)
- **Issue**: https://github.com/martinthommesen/daku/issues/69

## Why this matters

After plan 044 the shell is on gpui-component but the Environment detail is a token-for-token port of the old layout: cards are one number plus a grey line, the build string wraps over three lines, trends are barely visible, states like "Waiting"/"skipped"/errors are plain grey text, and the header has no hierarchy. This plan makes the detail pane read at a glance. Priorities from the design session: visible trends → card density → header hierarchy; wrapping and grey-error text fall out of a real card design.

## Current state (after 044)

- `src/app.rs`: `render_detail(&self, cx)` renders header (label; health + reachability pills as our own `div`s; `instance_url` host + `freshness` label), the seven `signal_card`s in a wrapping `flex_wrap` grid, and `compare_strip`. `signal_card(card: SignalCard, cx)`: title row with `status_dot` + `signal_label`, one summary line (`state.card_summary(id)`), optional detail line (`state.card_detail(id)`), optional `sparkline` (jobs/syslog), optional drift mismatch lines (`state.drift_mismatch_lines(5)`).
- `src/dashboard_state.rs` (model, unchanged by this plan): `SignalCard { signal_id, status, sparkline }`, `card_summary(signal_id) -> String` (e.g. `"7958 ms · glide-…zip"`, `"0 overdue · 0 error"`, `"3 plugins differ"`, `"12 days ago"`), `card_detail(signal_id) -> String` (error/detail/skipped phrase, ≤160 chars), `drift_mismatch_lines(limit)`, `compare_rows() -> Vec<CompareRow{id,label,build,drift,last_clone}>`, `compare_strip() -> CompareStrip{visible,has_mismatch}`, `freshness(last, now) -> Option<Freshness{label,stale}>`, `signal_label(id)`, `WAITING`.
- gpui-component pieces available (rev `972a3eb`): `cx.theme()` tokens (`background foreground border muted muted_foreground secondary secondary_foreground success warning danger accent radius radius_lg`), `gpui_component::{h_flex, v_flex}`, `label::Label`, `tag::Tag` (labelled chip — check `crates/ui/src/tag.rs` for variants/colours; use it for health/reachability pills if it fits, else keep our pill), `badge::Badge` (overlay dot/count), `separator::Separator`, `tooltip::Tooltip` (hover text on cards for the full build string / full error), `table::Table` (compare strip), `skeleton::Skeleton` (Waiting state), `chart::LineChart` (only if you drop the custom sparkline — not required). No sparkline primitive: keep `sparkline`/`paint_sparkline`.
- Vocabulary: **Signal card**, **Compare strip**, **Environment detail** (`CONTEXT.md` › Screen).

## Commands you will need

| Purpose | Command | Expected |
|---------|---------|----------|
| Build | `cargo build -p daku` | exit 0 |
| Fixture run | `HOME=$(mktemp -d) DAKU_UI_FIXTURE=1 DAKU_DAEMON_PATH=$PWD/target/debug/daku-daemon ./target/debug/daku` | window, no stderr |
| Gate | `bun run check` | exit 0 |

## Scope

**In scope**: `src/app.rs`; `src/dashboard_state.rs` **only** for a pure `card_value(signal_id) -> (String, String)`-style split if you need the summary as "value" + "unit/context" (add a test if you add it; keep `card_summary` for the compare strip); `plans/README.md`.

**Out of scope**: card selection, drill-in, deep links (plan 046); daemon/protocol; sidebar/TitleBar (044); fixture data changes beyond adding a value that exercises a new visual (add, don't reshape).

## Git workflow

As plan 044. One commit: `Restyle the Environment detail on gpui-component (#NN).`

## Steps

### Step 1: Header
`v_flex().gap(px(6))`: row 1 = Environment label (text_xl, semibold) + health pill + reachability pill (`Tag` if it supports a leading dot/colour, else our pill on `muted` bg with a `Badge::new().dot().color(..)` child); row 2 = `muted_foreground` line: host `·` freshness (stale → `warning` colour, as today). Header bottom border on `cx.theme().border`.
**Verify**: build.

### Step 2: Signal card
Fixed-width cards (e.g. `w(px(300))`, `min_h(px(120))`) in `flex_wrap` with `gap(px(12))`; `secondary` bg, `border`, `radius`. Layout: title row (`status_dot` + `signal_label`, `text_xs`, muted) → **value line** (text_2xl semibold, `foreground`; for availability show only the latency `NNN ms` here) → **context line** (text_sm, muted; the rest of the summary, e.g. the build string, `text_ellipsis()` + `overflow_hidden()` single line, full text in a `Tooltip` on hover) → detail line (`card_detail`, `danger` colour when `status == "down"`, else muted) → sparkline (jobs/syslog) with the status colour, `h(px(28))`, full card width → drift mismatch lines (max 5) as before. Status colours: healthy→success, degraded→warning, down→danger, skipped/Waiting→muted. `Waiting` renders the value line as a `Skeleton` bar instead of the word.
Split the summary in the model only if needed: `card_value(signal_id) -> Option<(String /*value*/, String /*context*/)>` next to `card_summary`, with a test for availability, jobs, drift, last_clone.
**Verify**: build; fixture launch shows two-line cards with no wrapping build string.

### Step 3: Compare strip
Render as a `Table` (or a bordered `v_flex` with `Separator`s if `Table` needs a stateful entity that is heavier than warranted — executor's call, note it): columns Environment · Build · Drift · Last clone; mismatch rows highlighted with `warning`. Only when `compare_strip().visible`.
**Verify**: fixture launch (two Environments) shows the strip; `cargo test -p daku` still 18+ green.

### Step 4: Gate + fixture launch
`bun run check` → 0; fixture launch alive 10 s, empty stderr; screenshot to `/tmp/claude-501/045.png` if possible.

## Test plan
Model tests unchanged; add `card_value_*` test only if Step 2 adds the helper. Manual acceptance by the Operator (fixture + PDI): cards readable at a glance, no wrapped build string, trends visible, header hierarchy.

## Done criteria
- [ ] `bun run check` exits 0
- [ ] fixture launch: no stderr; cards show value/context/detail; sparklines visible on jobs and syslog
- [ ] `grep -n 'text_ellipsis' src/app.rs` ≥ 1 (long context lines are clipped, not wrapped)
- [ ] `plans/README.md` status row updated

## STOP conditions
- Plan 044 not DONE (old `Theme` struct still present).
- A gpui-component component named here does not exist at rev `972a3eb` — substitute a plain `div` and note it; do not bump the rev.
