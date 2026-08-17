# Plan 005: MID / ECC Signal

> **Executor instructions**: Follow this plan step by step. Run every verification command and confirm the expected result before moving to the next step. If anything in the "STOP conditions" section occurs, stop and report — do not improvise. When done, update the status row for this plan in `plans/README.md`.
>
> **Drift check (run first)**: `git diff --stat 567179a..HEAD -- crates/daku-core crates/daku-daemon`
> Confirm 003 DONE (HTTP client + loop). Prefer 004 DONE for `parse_aggregate_count`; if 004 not merged, copy the same Aggregate parse tests into this crate module and STOP to note duplication for reconcile — do **not** invent a second helper API shape.

## Status

- **Priority**: P2
- **Effort**: M
- **Risk**: MED
- **Depends on**: plans/003-availability-signal.md (soft: 004 for `parse_aggregate_count`)
- **Category**: direction
- **Planned at**: commit `567179a`, 2026-08-17
- **Issue**: https://github.com/martinthommesen/daku/issues/21

## Why this matters

Dead MID Servers and ECC backlog are v1 Signal #3. Empty MID lists on a baseline PDI are **healthy** (research).

## Current state

- Research ([docs/research/servicenow-signals.md](../docs/research/servicenow-signals.md)):
  - **MID:** Table API `ecc_agent` fields `status`, `validated`, `version`, `host_name`. Unhealthy if `status≠Up` or `validated` is not true.
  - **ECC:** Aggregate `ecc_queue`: `queue=output^state=ready` count; `state=error` count; date-bound (~7 days).
- Point-in-time Signal (no samples required).
- `signal_id = "mid_ecc"`. Soft ceiling: `ECC_READY_DEGRADED_AT = 100` (named constant).

## Commands you will need

| Purpose | Command | Expected on success |
|---------|---------|---------------------|
| Tests | `cargo test -p daku-core mid_ecc` | all pass |
| Check | `cargo check -p daku-core -p daku-daemon` | exit 0 |

## Scope

**In scope**

- Collector registered on 003 loop; payload:

  ```json
  { "agents_total": 0, "agents_unhealthy": 0, "ecc_output_ready": 0, "ecc_error": 0 }
  ```

- State: `healthy` if unhealthy=0, ecc_error=0, ready < 100; else `degraded`. Probe failure → 003 unreachable path. Empty agents → healthy.
- Fixtures under `tests/fixtures/mid_ecc/`.
- Snapshot only (no `signal_samples`).

**Out of scope**

- `ecc_agent_issue`, `ecc_agent_status` CPU/mem, Discovery workspace roles, private poll timers, GPUI.

## Git workflow

- Work on `main` (trunk-based). No PRs; no GitHub Actions — see `docs/agents/git-workflow.md`.
- Optional disposable local branch for isolation; merge to `main` locally and delete the branch. Do not push topic branches to `origin`.
- Commit example: keep the imperative message named in each plan's Steps.


## Steps

### Step 1: Classify agents

**Verify**: `cargo test -p daku-core classify_mid_agents` → empty=ok; Down=unhealthy; validated false=unhealthy.

### Step 2: ECC counts via Aggregate helper

**Verify**: `cargo test -p daku-core mid_ecc` (includes queue parse) → pass.

### Step 3: Snapshot + register on loop

**Verify**: `cargo test -p daku-core mid_ecc_signal` → state matrix; `rg -n 'interval|tokio::time' crates/daku-core/src/*mid*` → no private timer; `cargo check -p daku-core -p daku-daemon` → exit 0.

## Test plan

| Case | Expected |
|------|----------|
| empty agents, zero queue | healthy |
| one Down | degraded |
| ecc_error > 0 | degraded |
| ready ≥ 100 | degraded |

## Done criteria

- [ ] `cargo test -p daku-core mid_ecc` exit 0
- [ ] `rg -n 'mid_ecc' crates/daku-core` → ≥1 hit
- [ ] No private poll timer in MID module
- [ ] `plans/README.md` row 005 Status = `DONE`

## STOP conditions

- `ecc_agent` / `ecc_queue` 403 with `mid_server` account — STOP; no HTML scraping.
- Field names differ on live instance — STOP; confirm via Operator query + research before changing.

## Maintenance notes

- Plan 008: unhealthy MID → Environment degraded.
- Reviewers: PDI-empty ≠ alarm.
