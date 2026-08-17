# Plan 006: Outbound / integration failures Signal

> **Executor instructions**: Follow this plan step by step. Run every verification command and confirm the expected result before moving to the next step. If anything in the "STOP conditions" section occurs, stop and report — do not improvise. When done, update the status row for this plan in `plans/README.md`.
>
> **Drift check (run first)**: `git diff --stat 567179a..HEAD -- crates/daku-core crates/daku-daemon`
> Confirm 003 DONE. Reuse `parse_aggregate_count` from 004 (or 005 note); do not invent a parallel helper.

## Status

- **Priority**: P2
- **Effort**: M
- **Risk**: MED
- **Depends on**: plans/003-availability-signal.md (soft: 004)
- **Category**: direction
- **Planned at**: commit `567179a`, 2026-08-17
- **Issue**: https://github.com/martinthommesen/daku/issues/21

## Why this matters

Silent integration failure is v1 Signal #7. v1 uses Aggregate counts on outbound HTTP logs only (single table, no optional email fork).

## Current state

- Research: Aggregate on `sys_outbound_http_log` for `http_status>=400` with `sys_created_on>javascript:gs.hoursAgoStart(1)`.
- `signal_id = "outbound"`. Point-in-time. Register on 003 loop.

## Commands you will need

| Purpose | Command | Expected on success |
|---------|---------|---------------------|
| Tests | `cargo test -p daku-core outbound` | all pass |
| Check | `cargo check -p daku-core -p daku-daemon` | exit 0 |

## Scope

**In scope**

- Collector payload: `{ "outbound_http_4xx_5xx_1h": N }`.
- State: `healthy` if N=0; `degraded` if N>0; probe failure → 003 path.
- Fixtures under `tests/fixtures/outbound/`.
- Snapshot only.

**Out of scope**

- `sys_email`, `sys_flow_context`, IntegrationHub UI, ECC errors (005), private timers, GPUI, persisting raw log bodies.

## Git workflow

- Work on `main` (trunk-based). No PRs; no GitHub Actions — see `docs/agents/git-workflow.md`.
- Optional disposable local branch for isolation; merge to `main` locally and delete the branch. Do not push topic branches to `origin`.
- Commit example: keep the imperative message named in each plan's Steps.


## Steps

### Step 1: Parse + classify

**Verify**: `cargo test -p daku-core outbound_signal` → zero=healthy; N>0=degraded.

### Step 2: HTTP collector + register

**Verify**: `cargo test -p daku-core outbound` → pass; `cargo check -p daku-core -p daku-daemon` → exit 0; no private `interval` in outbound module.

## Test plan

| Case | Expected |
|------|----------|
| count 0 | healthy |
| count > 0 | degraded |

## Done criteria

- [ ] `cargo test -p daku-core outbound` exit 0
- [ ] `rg -n 'sys_outbound_http_log' crates/daku-core` → ≥1 hit
- [ ] `plans/README.md` row 006 Status = `DONE`

## STOP conditions

- Table missing/404 on Operator family — STOP; no alternate table names without research refresh.
- Pressure to store outbound response bodies in SQLite — refuse (counts only).

## Maintenance notes

- Deferred (not this plan): email send-failed, flow ERROR counts.
- Plan 008: outbound degraded → Environment degraded.
