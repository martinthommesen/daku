# Plan 046: Signal card selection opens a drill-in region; every card links into ServiceNow

> **Executor instructions**: Follow this plan step by step. Run every
> verification command and confirm the expected result before moving to the
> next step. If anything in the "STOP conditions" section occurs, stop and
> report — do not improvise. When done, update the status row for this plan
> in `plans/README.md` — unless a reviewer dispatched you and told you they
> maintain the index.
>
> **Drift check (run first)**: `git diff --stat <sha of plan 045's commit>..HEAD -- src/app.rs src/dashboard_state.rs crates/daku-core/src/lib.rs`
> Plans 044 and 045 must be DONE. Any other in-scope change since is a STOP condition.

## Status

- **Priority**: P2
- **Effort**: M
- **Risk**: LOW (client-only; payload keys already exist)
- **Depends on**: plans/045-environment-detail-restyle.md
- **Category**: direction
- **Planned at**: commit `826a636`, 2026-08-17
- **Issue**: https://github.com/martinthommesen/daku/issues/70

## Why this matters

Cards are dead ends: the daemon already persists rows the Operator wants to see (drift `mismatch_list`, last-clone rows, error text, ~24h samples) and the header carries `instance_url`, but nothing opens. `docs/research/signal-drill-in.md` decided: card selection + a bounded per-row region, deep links for the count-only Signals; the MID/ECC agents list needs a collector change and is a later plan. This plan builds the **Drill-in** (`CONTEXT.md` › Screen) from what the wire already carries, plus an "Open in ServiceNow" link per card.

## Current state (after 045)

- `src/dashboard_state.rs`: `DashboardState { environments, snapshots: HashMap<env_id, HashMap<signal_id, SignalSnapshotDto>>, samples: HashMap<(env_id, signal_id), Vec<SignalSampleDto>>, selected_id, connected, … }`; `selected() -> Option<&EnvironmentSummary>` (has `instance_url`); `card_detail`, `drift_mismatch_lines(limit)` (from payload `mismatch_list` [{id, this_version, other_version}], `mismatches`, `mismatch_list_truncated`), `summarize_payload("last_clone")` reads `{completed, age_days, source_id}` / `{role:"source"}` / `{supported, completed:null}`; jobs/syslog samples are `SignalSampleDto { observed_at, value_real }`. Selection of a **card** does not exist yet.
- `src/app.rs`: `signal_card(card, cx)` is a plain `div` (no click); no region under the cards.
- ServiceNow list paths (for deep links; base = `instance_url`, open with `cx.open_url(&url)`): jobs → `/sys_trigger_list.do?sysparm_query=state=0^next_action<javascript:gs.minutesAgo(5)`; syslog → `/syslog_list.do?sysparm_query=level=2^sys_created_on>javascript:gs.hoursAgoStart(1)`; MID/ECC → `/ecc_agent_list.do`; outbound → `/sys_outbound_http_log_list.do?sysparm_query=http_status>=400^sys_created_on>javascript:gs.hoursAgoStart(1)`; drift → `/v_plugin_list.do`; last clone → `/clone_instance_list.do`; availability → `/sys_properties_list.do?sysparm_query=name=glide.war`. (Mirror the collectors' encoded queries in `crates/daku-core/src/{jobs,syslog,mid_ecc,outbound,drift,last_clone,availability}.rs`; percent-encode `^ < > space` in the URL you open.)
- gpui-component: `link::Link` / `button::Button` (ghost, small) for the link; `table::Table` or a `v_flex` of rows for the region; `chart::LineChart` for the larger trend (optional; the custom `sparkline` scaled to `h(px(80))` is acceptable).
- Vocabulary: **Drill-in**, **Signal card**.

## Commands you will need

Same as plan 045.

## Scope

**In scope**: `src/dashboard_state.rs` (`selected_card: Option<&'static str>`, `select_card(id)` toggle, `drill_in_rows(signal_id) -> Vec<Vec<String>>` or a small enum of drill-in content, `signal_url(signal_id) -> Option<String>` + tests), `src/app.rs` (clickable cards, selected style, region, link), `plans/README.md`.

**Out of scope**: daemon/protocol/payload changes (MID agents list is a follow-up plan); persisting selection; a second window.

## Git workflow

As plan 044. One commit: `Add Signal card selection, drill-in region, and ServiceNow deep links (#NN).`

## Steps

### Step 1: Model
`select_card(signal_id)` toggles `selected_card` (clicking the selected card closes the region); `select(env)` keeps `selected_card`. `signal_url(signal_id) -> Option<String>` builds `instance_url` + path (percent-encoded), `None` without a selected Environment. `drill_in(signal_id) -> DrillIn` where `enum DrillIn { Rows { headers: Vec<&'static str>, rows: Vec<Vec<String>>, truncated: bool }, Trend(Vec<f64>), Text(String), Empty }`: drift → Rows(id / this / other) from `mismatch_list` (all rows, ≤50; `truncated` from `mismatch_list_truncated`); last_clone → Rows(completed / age / source) or Text("clone source"); jobs/syslog → Trend(samples); availability/mid_ecc/outbound → Text(card_detail or summary). Tests: toggle semantics, `signal_url` encoding, drift rows from the fixture, trend from fixture samples.
**Verify**: `cargo test -p daku drill_in signal_url select_card` → all pass.

### Step 2: Render
Card: `.id(signal_id).cursor_pointer().on_click(cx.listener(..select_card..))`, selected → `border_color(accent)`; add a small "Open ↗" ghost `Button`/`Link` in the card title row (stops propagation, `cx.open_url`). Under the card grid, when `selected_card.is_some()`: a bordered region titled `signal_label` with the `DrillIn` content (Rows → `Table` or striped `v_flex`; Trend → tall sparkline; Text → paragraph; truncated → "… and N more" from `mismatches`).
**Verify**: build; fixture launch: clicking "Version / plugins" on Test opens 3 rows; clicking again closes.

### Step 3: Gate + fixture launch
`bun run check` → 0; screenshot to `/tmp/claude-501/046.png` if possible.

## Test plan
New model tests as in Step 1 (≥4). Manual: Operator clicks each card on the PDI; links open the right ServiceNow list.

## Done criteria
- [ ] `bun run check` exits 0
- [ ] `cargo test -p daku` includes ≥4 new tests (`select_card_*`, `signal_url_*`, `drill_in_*`)
- [ ] fixture: drift card opens a 3-row region; jobs card opens a trend; clicking the open card closes it
- [ ] `plans/README.md` status row updated

## STOP conditions
- Plans 044/045 not DONE.
- `cx.open_url` (or equivalent) is unavailable on the pinned gpui — report; do not shell out to `open` silently.
