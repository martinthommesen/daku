# Plan 009: GPUI shell — sidebar + Environment detail (variant C)

> **Executor instructions**: Follow this plan step by step. Run every verification command and confirm the expected result before moving to the next step. If anything in the "STOP conditions" section occurs, stop and report — do not improvise. When done, update the status row for this plan in `plans/README.md`.
>
> **Drift check (run first)**: confirm plan 008 protocol events exist and `cargo check -p daku` works. Then `git diff --stat 315f38d..HEAD -- plans/009-gpui-shell-variant-c.md src/ crates/daku-client`.

## Status

- **Priority**: P1
- **Effort**: L
- **Risk**: MED
- **Depends on**: plans/008-health-rollup-protocol.md
- **Category**: direction
- **Planned at**: commit `315f38d`, 2026-08-17
- **Issue**: https://github.com/martinthommesen/daku/issues/22

## Why this matters

v1’s Operator-facing product is a macOS GPUI app (ADR-0001) whose home screen is **sidebar + Environment detail** (ADR-0005), not the matrix. The HTML prototype validates layout only — implement in GPUI against protocol events from plan 008, using the prototype as a visual reference.

## Current state

- ADR-0005: waku-like sidebar (Platforms → Environments) + Environment detail (health, Signal cards, compare-vs-others strip). Matrix deferred.
- Spec §8 same structure.
- Visual reference (HTML, throwaway):  
  - Branch/path: [`prototype/environments-overview`](https://github.com/martinthommesen/daku/tree/prototype/environments-overview) (also `prototypes/environments-overview/` on main if present)  
  - Open with `?variant=C`  
  - Colours = waku tokens (canvas / text / accent) — research/waku-reuse cites canvas `#F6F5F6`, text `#242424`, accent `#C85F44` as published GPUI theme precedent; match the forked theme module rather than hard-coding if theme files survived plan 001.
- After 001: GPUI `src/` exists with agent UI stripped — shell chrome may remain; replace main content with daku dashboard.
- After 008: client receives `EnvironmentsUpdated` / `SignalSnapshotsUpdated`.

## Commands you will need

| Purpose | Command | Expected on success |
|---------|---------|---------------------|
| Check app | `cargo check -p daku` | exit 0 |
| Dev run (Operator) | existing `bun`/`cargo` dev script from plan 001 (document exact command found in README) | window opens, connects to local daemon |
| Tests | any UI unit tests you add; at least `cargo test -p daku-client` | pass |

## Suggested executor toolkit

- Prototype README: `prototypes/environments-overview/README.md`
- ADR-0005; ADR-0001
- Do **not** paste HTML/CSS into Rust; rebuild layout with GPUI primitives already used in the forked `src/ui/` / shell.

## Scope

**In scope**

### Shell structure

1. **Sidebar**
   - Section: Platform label (`ServiceNow` for v1 — single Platform).
   - List Environments from protocol (id, label).
   - Health dot per Environment: healthy / degraded / down / unknown (muted).
   - Keyboard: up/down to change selection (match prototype intent).

2. **Detail pane** (selected Environment)
   - Header: label + health + reachability hint if asleep/unreachable (from availability payload if present).
   - **Signal cards**: one card per known `signal_id` (`availability`, `jobs`, `syslog`, `mid_ecc`, `outbound`, `drift`, `last_clone`). Missing snapshot → card shows “Waiting” / unknown, not an error.
   - Show primary numbers from payload_json when present (e.g. overdue count, error_count_1h) — keep rendering defensive (serde ignore unknown fields).
   - **Compare strip**: compact “vs other Environments” for build string and/or drift summary — data from other Environments’ snapshots already in client memory; no extra daemon API required if 008 broadcasts all.

3. **Connection chrome**
   - Reuse waku daemon supervisor via `daku-client` (spawn local daemon, read ready JSON, connect Hello).
   - Surface disconnected/rejected states in UI (simple banner) — no settings megapage in v1 beyond what’s needed to run.

4. **Theming**
   - Stay on forked light theme tokens; do not invent a new purple/dark AI-default palette.
   - No emoji in chrome.

**Out of scope**

- Matrix view.
- Alert configuration UI.
- Web client / wry webview (stripped in 001).
- Packaging/notarisation (010).
- Editing `environments.json` inside the app (v1 may remain file + keychain Operator setup; optional “open config folder” link is OK).
- Pixel-perfect clone of the HTML prototype.

## Git workflow

- Branch: `plan/009-gpui-shell-variant-c`
- Commit example: `Add GPUI Environments sidebar and detail shell`

## Steps

### Step 1: Dashboard state model in the app

Hold `Vec<EnvironmentSummary>` + map of snapshots keyed by environment_id; apply protocol events.

**Verify**: unit test pure apply functions if extracted; `cargo test` for that module passes.

### Step 2: Sidebar GPUI view

Render list + selection; health dots.

**Verify**: `cargo check -p daku`; manual run shows list from fixture daemon or mock events (prefer a `DAKU_UI_FIXTURE=1` path that feeds fake events without ServiceNow — document it).

### Step 3: Detail header + Signal cards

Seven card slots; defensive JSON field reads.

**Verify**: with fixture events, cards populate; with missing Signals, cards show waiting.

### Step 4: Compare strip

Show build / drift mismatch counts vs source Environment when data exists.

**Verify**: fixture with two Environments differing in build → strip visible; single Environment → strip hidden or “n/a”.

### Step 5: Wire real daemon client

Default path: supervisor starts `daku-daemon`, Hello, subscribe, render live snapshots (even if only availability exists).

**Verify**: Operator-local run against example.com config shows unreachable/down without crashing; README documents the run command.

## Test plan

| Case | Expected |
|------|----------|
| apply EnvironmentsUpdated | sidebar order/labels update |
| unknown health | muted dot |
| missing signal snapshot | Waiting card |
| fixture two-env drift | compare strip shows mismatch |

Automated GPUI pixel tests are **not** required; prefer pure state tests + `cargo check`.

## Done criteria

- [ ] `cargo check -p daku` exit 0
- [ ] Sidebar + detail + compare strip exist in GPUI (not HTML)
- [ ] UI driven by plan 008 protocol events (no direct SQLite)
- [ ] Prototype cited in README as reference only
- [ ] `plans/README.md` row 009 → `done`

## STOP conditions

- Plan 008 events missing — stop; do not read DB from the UI process.
- GPUI pin/ Zed fork fails to build on the Operator’s macOS — stop with compiler errors; do not switch to web client (ADR-0001).
- Urge to reintroduce wry/webview for “faster UI” — out of scope.

## Maintenance notes

- Plan 010 wraps this `.app`.
- When Signal payloads gain fields, extend card renderers behind the same `signal_id` switch.
- Reviewers: confirm variant C structure (sidebar+detail), single Platform, no matrix home screen.
