# Plan 005: MID / ECC Signal

> **Executor instructions**: Follow this plan step by step. Run every verification command and confirm the expected result before moving to the next step. If anything in the "STOP conditions" section occurs, stop and report — do not improvise. When done, update the status row for this plan in `plans/README.md`.
>
> **Drift check (run first)**: confirm plan 003 collector pattern exists. Then `git diff --stat da67ae9..HEAD -- plans/005-mid-ecc-signal.md crates/daku-core`.

## Status

- **Priority**: P2
- **Effort**: M
- **Risk**: MED
- **Depends on**: plans/003-availability-signal.md
- **Category**: direction
- **Planned at**: commit `da67ae9`, 2026-08-17
- **Issue**: https://github.com/martinthommesen/daku/issues/21

## Why this matters

Dead MID Servers and ECC backlog are a named v1 pain (spec Signal #3). One Signal covers both `ecc_agent` health and `ecc_queue` backlog so the Environment detail shows a single MID/ECC card. Empty MID lists on a baseline PDI must be **healthy** (research), not an alarm.

## Current state

- Plan 003/004 collector + fixture patterns.
- Research — [servicenow-signals](https://github.com/martinthommesen/daku/blob/research/servicenow-signals/docs/research/servicenow-signals.md):

  **MID (`ecc_agent`)** Table API fields: `status` (Up/Down/Paused/Upgrading), `validated`, `version`, `host_name`. Unhealthy if any row has `status≠Up` or `validated≠true` (treat boolean/`true` string consistently in parser).

  Optional (v1 nice-to-have, do not block): `ecc_agent_issue` count — skip unless trivial.

  **ECC queue** Aggregate API on `ecc_queue`:

  - Backlog: `queue=output^state=ready` count (+ optional oldest age later)
  - Errors: `state=error` count
  - Constrain by date (default retention ~7 days) — e.g. `sys_created_on>javascript:gs.daysAgoStart(7)`

  Roles: `mid_server` or admin. Baseline PDI: zero MIDs → OK.

- Spec: point-in-time (no 24h ring required for this Signal).
- CONTEXT.md: one **Signal** named observation — use `signal_id = "mid_ecc"`.

## Commands you will need

| Purpose | Command | Expected on success |
|---------|---------|---------------------|
| Tests | `cargo test -p daku-core mid_ecc` | all pass |
| Check | `cargo check -p daku-core -p daku-daemon` | exit 0 |

## Suggested executor toolkit

- Mirror jobs/syslog collector module from 004 if present; else availability from 003.
- Research note rows for MID Server status + ECC queue backlog.

## Scope

**In scope**

- Collector producing `signal_id = "mid_ecc"`:
  - Fetch `ecc_agent` list (limit reasonable, e.g. 200) with fields above.
  - Aggregate counts for ready backlog + error on `ecc_queue`.
  - Payload JSON example shape (field names stable for UI later):

    ```json
    {
      "agents_total": 0,
      "agents_unhealthy": 0,
      "ecc_output_ready": 0,
      "ecc_error": 0
    }
    ```

  - State mapping (hard-coded):
    - `healthy`: agents_unhealthy=0 AND ecc_error=0 AND ecc_output_ready below soft ceiling (default **100** — constant `ECC_READY_DEGRADED_AT`; document in code)
    - `degraded`: any unhealthy agent, any ecc_error>0, or ready backlog ≥ ceiling
    - probe/auth failure → unreachable handling from 003 (do not mark “no MIDs” as failure)
  - Empty `ecc_agent` result → agents_total=0, healthy (unless probe failed).
- Fixtures: `crates/daku-core/tests/fixtures/mid_ecc/` — agents all Up; one Down; empty agents; ecc ready high; ecc errors; 403.
- Snapshot only (`signal_snapshots`); **do not** require `signal_samples` for this Signal.
- Daemon one-shot/poll wiring consistent with other collectors.

**Out of scope**

- Parsing `ecc_agent_status` CPU/mem (dashboard extras).
- Discovery Admin Workspace roles beyond what Table API already allows.
- GPUI (009).
- Inventing hostnames in fixtures — use `"mid-host-a.example.com"` style fakes only if a hostname field is needed.

## Git workflow

- Branch: `plan/005-mid-ecc-signal`
- Commit example: `Add MID/ECC Signal collector with fixtures`

## Steps

### Step 1: Parse agent list + classify unhealthy

**Verify**: `cargo test -p daku-core classify_mid_agents` → empty=ok; Down=unhealthy; validated false=unhealthy.

### Step 2: Parse ECC aggregate counts

**Verify**: `cargo test -p daku-core parse_ecc_queue_counts` → pass.

### Step 3: Combine → snapshot

**Verify**: `cargo test -p daku-core mid_ecc_signal` → state matrix covered.

### Step 4: HTTP + daemon wire

Injectable client; encoded queries copied from research comments.

**Verify**: `cargo test -p daku-core mid_ecc` all pass; `cargo check` exit 0.

## Test plan

| Case | Expected |
|------|----------|
| empty agents, zero queue | healthy |
| one Down agent | degraded |
| ecc_error > 0 | degraded |
| ready ≥ 100 | degraded |
| 403 | unreachable / probe failure path |

## Done criteria

- [ ] Fixture tests green, no network
- [ ] `signal_id` is exactly `mid_ecc`
- [ ] Empty MID list is healthy
- [ ] `plans/README.md` row 005 → `done`

## STOP conditions

- Table `ecc_agent` / `ecc_queue` 403 on Operator smoke with monitoring account that has `mid_server` — stop; do not scrape HTML dashboards as a workaround.
- Field names for `validated`/`status` differ on live instance — stop and update fixtures only after confirming against research + one Operator query (now-sdk), do not guess alternate tables.

## Maintenance notes

- Plan 008: unhealthy MID → degraded.
- Reviewers: ensure PDI-empty ≠ alarm; backlog ceiling is a named constant for later tuning.
