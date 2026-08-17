# Plan 009: GPUI shell — sidebar + Environment detail (variant C)

> **Executor instructions**: Follow this plan step by step. Run every verification command and confirm the expected result before moving to the next step. If anything in the "STOP conditions" section occurs, stop and report — do not improvise. When done, update the status row for this plan in `plans/README.md`.
>
> **Drift check (run first)**: `git diff --stat 567179a..HEAD -- src crates/daku-client crates/daku-protocol`
> Confirm 008 DONE (protocol events including `SignalSamplesUpdated`). On mismatch, STOP.

## Status

- **Priority**: P1
- **Effort**: L
- **Risk**: MED
- **Depends on**: plans/008-health-rollup-protocol.md
- **Category**: direction
- **Planned at**: commit `567179a`, 2026-08-17
- **Issue**: https://github.com/martinthommesen/daku/issues/22

## Why this matters

v1 Operator UI is macOS GPUI (ADR-0001) with **sidebar + Environment detail** (ADR-0005 / prototype variant C). Spec also requires **~24h trends** for jobs/syslog — show sparklines from `SignalSamplesUpdated`, not only scalar numbers.

## Current state

- Visual reference only: `prototypes/environments-overview/index.html?variant=C` (or branch `prototype/environments-overview`). Do not paste HTML into Rust.
- Theme: keep forked light tokens (research cites canvas `#F6F5F6`, text `#242424`, accent `#C85F44`) via surviving theme module.
- Protocol: `EnvironmentsUpdated`, `SignalSnapshotsUpdated`, `SignalSamplesUpdated`.
- Health: healthy | degraded | down. Reachability: reachable | unreachable | asleep (header badge, not a fourth health dot).

## Commands you will need

| Purpose | Command | Expected on success |
|---------|---------|---------------------|
| Check app | `cargo check -p daku` | exit 0 |
| State tests | `cargo test -p daku dashboard_state` | all pass |
| Client | `cargo test -p daku-client` | all pass |

## Scope

**In scope**

1. **Pure dashboard state** (testable without opening a window): apply protocol events → sidebar list, selection, cards, compare strip model, sparkline points for `jobs`/`syslog`.
2. **Sidebar GPUI**: Platform `ServiceNow`; Environments with health dots (3 colors + muted only if disconnected from daemon — connection chrome, not Environment health `unknown`).
3. **Detail**: header (label, health, **reachability badge**); seven Signal cards (`availability`, `jobs`, `syslog`, `mid_ecc`, `outbound`, `drift`, `last_clone`); missing snapshot → card label `Waiting` (string), still a card.
4. **Sparklines**: for `jobs` and `syslog` cards, render sample `points` (reuse chart helpers from `usage_page` if present). If `points` empty, hide sparkline (scalars still show).
5. **Compare strip**: build / drift vs clone-source when ≥2 Environments in state.
6. **DaemonSupervisor** connect via Hello; disconnected banner.

**Out of scope**

- Matrix home; alert UI; webview; packaging (010); in-app JSON editor; inventing `unknown` health.

## Git workflow

- Branch: `plan/009-gpui-shell-variant-c`
- Commit example: `Add GPUI Environments sidebar and detail shell`

## Steps

### Step 1: Dashboard state module + fixtures

Apply events in memory; include samples → sparkline series length.

**Verify**: `cargo test -p daku dashboard_state` → pass (cases: health dots mapping, asleep reachability does not change health enum, jobs samples length, Waiting when snapshot missing, compare strip mismatch flag).

### Step 2: Sidebar + detail GPUI wired to state

Render from state module. Fixture mode: `DAKU_UI_FIXTURE=1` feeds the same events as unit tests (no ServiceNow).

**Verify**: `cargo check -p daku` → exit 0; `rg -n 'DAKU_UI_FIXTURE|dashboard_state' src` → ≥1 hit; `rg -n 'SignalSamplesUpdated|sparkline|samples' src` → ≥1 hit.

### Step 3: Real daemon path

Default: supervisor starts daemon, Hello, render live events.

**Verify**: `rg -n 'DaemonSupervisor|Hello' src crates/daku-client` → ≥1 hit; `cargo check -p daku` → exit 0.

## Test plan

| Case (in `dashboard_state` tests) | Expected |
|----------------------------------|----------|
| EnvironmentsUpdated | ids/labels/order |
| health degraded | dot state degraded |
| reachability asleep | badge asleep; health unchanged by asleep alone |
| jobs samples [3 points] | sparkline series len 3 |
| missing signal | card status Waiting |
| two envs build mismatch | compare_strip.has_mismatch true |

No GPUI pixel tests required.

## Done criteria

- [ ] `cargo test -p daku dashboard_state` exit 0
- [ ] `cargo check -p daku` exit 0
- [ ] `rg -n 'Waiting' src` → ≥1 hit
- [ ] `rg -n 'sparkline|SignalSamples|samples' src` → ≥1 hit (trends surface)
- [ ] `rg -n 'Reachability|asleep|reachability' src` → ≥1 hit
- [ ] `rg -n 'Unknown|Health::Unknown' src crates/daku-protocol` → no Environment health unknown
- [ ] `plans/README.md` row 009 Status = `DONE`

## STOP conditions

- Plan 008 missing `SignalSamplesUpdated` — STOP (needed for spec trends).
- GPUI fails to build — STOP; do not switch to web client.
- Urge to read SQLite from UI — refuse.

## Maintenance notes

- Plan 010 packages this app.
- Reviewers: variant C; sparklines for jobs/syslog; asleep via reachability badge.
