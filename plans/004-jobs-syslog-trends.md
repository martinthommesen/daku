# Plan 004: Scheduled jobs + syslog Signals with ~24h trends

> **Executor instructions**: Follow this plan step by step. Run every verification command and confirm the expected result before moving to the next step. If anything in the "STOP conditions" section occurs, stop and report — do not improvise. When done, update the status row for this plan in `plans/README.md`.
>
> **Drift check (run first)**: `git diff --stat 567179a..HEAD -- crates/daku-core crates/daku-daemon`
> Confirm plan 003 DONE (`CollectorLoop` + `ServiceNowClient` exist). On mismatch, STOP.

## Status

- **Priority**: P1
- **Effort**: M
- **Risk**: MED
- **Depends on**: plans/003-availability-signal.md
- **Category**: direction
- **Planned at**: commit `567179a`, 2026-08-17
- **Issue**: https://github.com/martinthommesen/daku/issues/21

## Why this matters

Stuck jobs and syslog errors are core Operator pains (spec §5). Spec requires **~24h trends** for both — the only Signals that need `signal_samples`. This plan registers collectors on the **existing** 003 poll loop (no second timer).

## Current state

- 003: `ServiceNowClient` (OAuth/basic, 429), `CollectorLoop` (~120s), `signal_snapshots`.
- 002: `signal_samples` table ready.
- Research ([docs/research/servicenow-signals.md](../docs/research/servicenow-signals.md)):

  **Jobs** — Aggregate/Table on `sys_trigger`:
  - Overdue Ready: `state=0^next_action<javascript:gs.minutesAgoStart(15)`
  - Error: `state=3`
  Prefer Aggregate `sysparm_count=true`.

  **Syslog** — Aggregate on `syslog`:
  - `sysparm_count=true&sysparm_query=level=2^sys_created_on>javascript:gs.hoursAgoStart(1)`
  - `level=2` = Error is research-**[unverified]** — constant `SYSLOG_ERROR_LEVEL=2`; STOP if Operator smoke shows Error rows with another value.
  - Always date-bound (rotated table).

- Hard-coded: overdue>0 → Signal `degraded`; syslog count>0 → `degraded`; probe failure → unreachable handling from 003 (do not invent Signal `down` for count>0).

## Commands you will need

| Purpose | Command | Expected on success |
|---------|---------|---------------------|
| Jobs tests | `cargo test -p daku-core jobs_signal` | all pass |
| Syslog tests | `cargo test -p daku-core syslog_signal` | all pass |
| Prune tests | `cargo test -p daku-core prune_signal_samples` | all pass |
| Aggregate helper | `cargo test -p daku-core aggregate_count` | all pass |
| Check | `cargo check -p daku-core -p daku-daemon` | exit 0 |

## Scope

**In scope**

- Shared pure helper `parse_aggregate_count(body: &[u8]) -> Result<u64>` (and optional query builder) in `daku-core` — **this plan owns it**; 005–006 reuse it.
- Collectors `signal_id = "jobs"` and `"syslog"` registered on 003’s loop.
- Jobs payload: `{ "overdue_ready": N, "error": N }` only (no stuck-running in v1).
- Syslog payload: `{ "error_count_1h": N }`.
- Each successful poll: upsert snapshot **and** append `signal_samples` (`value_real` = overdue+error for jobs; error_count_1h for syslog).
- Prune samples with `observed_at` older than 24h (all signal_ids or these two — prefer **all** for one function).
- Fixtures under `tests/fixtures/jobs/` and `tests/fixtures/syslog/`.

**Out of scope**

- Second poll timer; stuck-running jobs; `group_by=source` (defer); GPUI sparklines (009 reads samples via protocol from 008); live CI.

## Git workflow

- Work on `main` (trunk-based). No PRs; no GitHub Actions — see `docs/agents/git-workflow.md`.
- Optional disposable local branch for isolation; merge to `main` locally and delete the branch. Do not push topic branches to `origin`.
- Commit example: keep the imperative message named in each plan's Steps.


## Steps

### Step 1: `parse_aggregate_count`

**Verify**: `cargo test -p daku-core aggregate_count` → pass.

### Step 2: Jobs collector + snapshot/sample

**Verify**: `cargo test -p daku-core jobs_signal` → zero→healthy; overdue>0→degraded; sample row written.

### Step 3: Syslog collector + snapshot/sample

**Verify**: `cargo test -p daku-core syslog_signal` → pass.

### Step 4: Prune

**Verify**: `cargo test -p daku-core prune_signal_samples` → row at now−25h removed; now kept.

### Step 5: Register on CollectorLoop

**Verify**: `rg -n 'register.*jobs|JobsCollector|signal_id.*jobs' crates/daku-core crates/daku-daemon` → ≥1 hit; `cargo check -p daku-core -p daku-daemon` → exit 0; `rg -n 'tokio::time::interval|poll_interval' crates/daku-core/src/jobs.rs crates/daku-core/src/syslog.rs 2>/dev/null` → no matches (no private timers).

## Test plan

| Case | Expected |
|------|----------|
| jobs zeros | healthy, sample 0 |
| overdue > 0 | degraded |
| syslog > 0 | degraded |
| prune | drops >24h |
| no network in default tests | enforced |

## Done criteria

- [ ] Listed `cargo test` filters exit 0
- [ ] `cargo check -p daku-core -p daku-daemon` exit 0
- [ ] `rg -n 'signal_samples' crates/daku-core` → write + prune paths exist
- [ ] No private interval in jobs/syslog modules (Step 5 rg)
- [ ] `plans/README.md` row 004 Status = `DONE`

## STOP conditions

- Aggregate envelope cannot be parsed without guessing undocumented fields — STOP; cite research.
- Live PDI: Error rows exist but `level=2` counts 0 — STOP; do not silently change level.
- Plan 003 loop missing — STOP; do not add a new timer.

## Maintenance notes

- Plan 008/009: expose sample series for jobs/syslog sparklines.
- Plan 008 rollup: jobs/syslog `degraded` → Environment `degraded`.
- Reviewers: date bounds on every syslog query.
