# Plan 004: Scheduled jobs + syslog Signals with ~24h trends

> **Executor instructions**: Follow this plan step by step. Run every verification command and confirm the expected result before moving to the next step. If anything in the "STOP conditions" section occurs, stop and report — do not improvise. When done, update the status row for this plan in `plans/README.md`.
>
> **Drift check (run first)**: confirm plan 003 done (availability fixtures green). Then `git diff --stat da67ae9..HEAD -- plans/004-jobs-syslog-trends.md crates/daku-core`.

## Status

- **Priority**: P1
- **Effort**: M
- **Risk**: MED
- **Depends on**: plans/003-availability-signal.md
- **Category**: direction
- **Planned at**: commit `da67ae9`, 2026-08-17
- **Issue**: https://github.com/martinthommesen/daku/issues/21

## Why this matters

Stuck scheduled jobs and climbing syslog errors are two of the Operator’s core pains (spec §5). Spec requires **~24h trends** for both — the only Signals that need a sample ring in v1. Landing them together reuses the Aggregate/Table client pattern from plan 003 and proves `signal_samples` + prune before MID/outbound/drift work.

## Current state

- Collector layout from plan 003: injectable HTTP client, `signal_snapshots`, fixture classifiers, no network in `cargo test`.
- Plan 002 created `signal_samples` (environment_id, signal_id, observed_at, value_real, value_json) for this plan.
- Research (do **not** invent endpoints) — [servicenow-signals](https://github.com/martinthommesen/daku/blob/research/servicenow-signals/docs/research/servicenow-signals.md):

  **Jobs (`sys_trigger`)** via Aggregate or Table API:

  - Overdue Ready: `state=0^next_action<javascript:gs.minutesAgoStart(15)`
  - Error: `state=3`
  - Optional stuck Running: `state=1^claimed_by!=NULL^sys_updated_on<…` (document threshold; default 30 minutes if used)

  Prefer Aggregate `sysparm_count=true` per query over pulling rows.

  **Syslog** via Aggregate API:

  - `GET /api/now/stats/syslog?sysparm_count=true&sysparm_query=level=2^sys_created_on>javascript:gs.hoursAgoStart(1)`
  - Research notes level `2` = Error is **[unverified]** — fixtures + classifier must treat the numeric level as a config constant (`syslog_error_level`, default `2`); STOP if live PDI returns empty/wrong when Operator confirms Error rows exist with another value.
  - Always date-bound (`syslog` is rotated).

- Spec: poll ~2 min; store latest snapshot + ~24h ring; hard-coded health (e.g. overdue job → degraded).
- CONTEXT.md: Signal names on an Environment; vocabulary **Signal**, **Environment**, **Environment health**.

## Commands you will need

| Purpose | Command | Expected on success |
|---------|---------|---------------------|
| Tests | `cargo test -p daku-core jobs` and `cargo test -p daku-core syslog` | all pass |
| Check | `cargo check -p daku-core -p daku-daemon` | exit 0 |
| Sample prune | covered by unit test on tempfile DB | samples older than 24h removed |

## Suggested executor toolkit

- Copy module layout from availability collector (plan 003).
- Research note + spec §5–7; ADR-0007 for local store.

## Scope

**In scope**

- Collectors under `crates/daku-core` (or existing collector dir from 003):
  - `signal_id = "jobs"` — payload JSON with counts: `overdue_ready`, `error`, optional `stuck_running`; map state:
    - `healthy` if overdue=0 and error=0 (and stuck=0 if collected)
    - `degraded` if overdue>0 or stuck>0
    - `down` only if the Environment is unreachable / probe failed (auth/transport) — reuse 003 outcome types; do not invent a third “jobs down” meaning
  - `signal_id = "syslog"` — payload with `error_count_1h` (and optional top `source` groups if Aggregate `group_by` is easy; otherwise count-only is enough for v1)
    - `healthy` if count == 0
    - `degraded` if count > 0 (hard-coded; no Operator thresholds in v1)
    - probe failure → same unreachable handling as 003
- After each successful poll: upsert `signal_snapshots` **and** append `signal_samples` with `value_real` = primary count (jobs: overdue+error; syslog: error_count_1h).
- Prune `signal_samples` for these signal_ids where `observed_at` < now−24h (run after insert or on a timer).
- Fixtures under `crates/daku-core/tests/fixtures/jobs/` and `.../syslog/`:
  - Aggregate-style JSON bodies matching Table/Aggregate API `result` shape (fake counts only).
  - At least: zero counts, non-zero overdue, non-zero syslog errors, 403/auth failure body.
- Daemon: extend one-shot / poll loop to include jobs + syslog (or `probe-jobs` / `probe-syslog` subcommands mirroring availability).
- Constants module documenting encoded queries and `syslog_error_level = 2` with a comment pointing at the research unverified note.

**Out of scope**

- MID/ECC, outbound, drift/clone (005–007).
- GPUI charts (009) — DB samples are enough; UI reads them later.
- Live CI against a PDI.
- Semaphores / `stats.do` (explicitly skipped in research v1 set).

## Git workflow

- Branch: `plan/004-jobs-syslog-trends`
- Commit example: `Add jobs and syslog Signals with 24h samples`

## Steps

### Step 1: Fixture parsers for Aggregate counts

Pure functions parse Aggregate API JSON → counts. Do not call the network.

**Verify**: `cargo test -p daku-core parse_aggregate_count` → pass.

### Step 2: Jobs classifier + snapshot/sample write

Given overdue/error/(optional stuck) counts → Signal state + persist snapshot + one sample row.

**Verify**: `cargo test -p daku-core jobs_signal` → healthy/degraded cases + DB rows on tempfile.

### Step 3: Syslog classifier + snapshot/sample write

Same pattern with `error_count_1h`.

**Verify**: `cargo test -p daku-core syslog_signal` → pass.

### Step 4: HTTP collectors (injectable)

Build encoded queries exactly as in research (inline in code comments with research link). Inject client; production uses same HTTP stack as availability.

**Verify**: mock client tests write snapshots; `cargo test` does not open sockets.

### Step 5: 24h prune

Implement `prune_signal_samples(db, older_than)` and call it after samples insert (or daemon tick).

**Verify**: unit test inserts sample at now−25h and now; prune → only recent remains.

### Step 6: Wire daemon entrypoints

Document Operator-local smoke in `docs/examples/` without real hostnames (example.com only).

**Verify**: `rg -n 'dev[0-9]+\\.service-now' docs README.md` → no matches; `cargo check` exit 0.

## Test plan

| Case | Expected |
|------|----------|
| jobs all zero | healthy snapshot; sample value 0 |
| overdue > 0 | degraded |
| syslog errors > 0 | degraded |
| prune | drops >24h samples only for jobs/syslog (or all — document choice; prefer all signal_ids for simplicity) |
| no network in default tests | enforced |

## Done criteria

- [ ] `cargo test -p daku-core` jobs + syslog + prune tests pass with no network
- [ ] `signal_samples` populated and pruned
- [ ] Encoded queries match research note (no invented tables)
- [ ] `plans/README.md` row 004 → `done`

## STOP conditions

- Aggregate API response shape on fixtures cannot be made to match without guessing undocumented fields — stop and cite research; keep count parsing minimal (`result.stats` / documented Aggregate envelope — read research + one live Operator smoke before changing).
- Live PDI shows Error syslog rows but `level=2` returns 0 — stop; do not silently switch levels without updating research/comment and fixtures.
- Pressure to store secrets or real hostnames in fixtures.

## Maintenance notes

- Plan 008 rollup: overdue → degraded is defined here; keep thresholds in one constants file.
- Plan 009 may chart `signal_samples` for jobs/syslog only.
- Reviewers: confirm date bounds on every `syslog` query (rotation hazard).
