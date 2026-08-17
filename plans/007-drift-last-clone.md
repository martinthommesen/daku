# Plan 007: Version / plugin drift + last-clone Signals

> **Executor instructions**: Follow this plan step by step. Run every verification command and confirm the expected result before moving to the next step. If anything in the "STOP conditions" section occurs, stop and report — do not improvise. When done, update the status row for this plan in `plans/README.md`.
>
> **Drift check (run first)**: `git diff --stat 567179a..HEAD -- crates/daku-core crates/daku-daemon docs/examples environments.example.json`
> Confirm 003 DONE (availability build + loop). On mismatch, STOP.

## Status

- **Priority**: P2
- **Effort**: L
- **Risk**: MED
- **Depends on**: plans/003-availability-signal.md
- **Category**: direction
- **Planned at**: commit `567179a`, 2026-08-17
- **Issue**: https://github.com/martinthommesen/daku/issues/21

## Why this matters

Post-clone drift and last-clone date are Signals #5–#6. Drift compares Environments; last-clone is read from the **clone source** only.

## Current state

- Reuse `glide.war` from latest availability snapshot when `observed_at` is within `2 * poll_interval_secs`; else same property GET as 003.
- Research: plugins via Table API `sys_plugins` (id, version, active) and `sys_store_app` (version, latest_version, active). Diff by id. (**Not** `sys_app` / `v_plugin` in v1.)
- Last clone: Table API `clone_instance` on **source** Environment; PDI unsupported → informational, not degraded.
- Config: add boolean `clone_source` to **this plan’s** update of `environments.example.json` (file owned for this field here; 002 created the base shape).

## Commands you will need

| Purpose | Command | Expected on success |
|---------|---------|---------------------|
| Diff tests | `cargo test -p daku-core diff_plugin_inventory` | all pass |
| Drift tests | `cargo test -p daku-core drift_signal` | all pass |
| Clone tests | `cargo test -p daku-core last_clone_signal` | all pass |
| Check | `cargo check -p daku-core -p daku-daemon` | exit 0 |
| Example JSON | `rg -n 'clone_source' environments.example.json docs/examples/environments.example.json` | ≥1 hit |

## Scope

**In scope**

- `signal_id = "drift"` and `"last_clone"`; register on 003 loop.
- Diff builds + `sys_plugins` / `sys_store_app` maps only; cap `sysparm_limit=1000`; if more rows exist, set `truncated: true` in payload (no extra pagination inventiveness — single page only).
- Drift state: source Environment snapshot `{ "role": "source" }` + `healthy`; others `healthy` if build matches and mismatches=0 else `degraded`; single-env config → `healthy` + `{ "skipped": "need_two_environments" }`.
- Last-clone: query source only; always Signal state `healthy`; `supported: false` on 403/empty/PDI.
- Fixtures under `tests/fixtures/drift/` and `tests/fixtures/last_clone/`.
- Fake plugin ids like `com.example.plugin_a` only.

**Out of scope**

- `sys_app`, `v_plugin`, Clone Admin Console scraping, Now Support APIs, matrix UI, private timers.

## Git workflow

- Work on `main` (trunk-based). No PRs; no GitHub Actions — see `docs/agents/git-workflow.md`.
- Optional disposable local branch for isolation; merge to `main` locally and delete the branch. Do not push topic branches to `origin`.
- Commit example: keep the imperative message named in each plan's Steps.


## Steps

### Step 1: Pure diff helper

**Verify**: `cargo test -p daku-core diff_plugin_inventory` → pass.

### Step 2: Drift snapshot

**Verify**: `cargo test -p daku-core drift_signal` → match=healthy; mismatch=degraded.

### Step 3: Last-clone parser

**Verify**: `cargo test -p daku-core last_clone_signal` → completed timestamp; 403→supported false + healthy.

### Step 4: HTTP + register on loop

**Verify**: `cargo test -p daku-core drift last_clone` → pass; `cargo check` exit 0; no private timers.

### Step 5: Example JSON field

Add `"clone_source": true` on the prod-like example entry in the existing example file from 002.

**Verify**: `rg -n 'clone_source' environments.example.json docs/examples/environments.example.json` → ≥1 hit; `rg -n 'service-now\\.com' environments.example.json docs/examples/environments.example.json` → only `example.service-now.com`.

## Test plan

| Case | Expected |
|------|----------|
| identical inventories | drift healthy |
| version mismatch | drift degraded |
| source env | healthy role=source |
| clone 403 | last_clone healthy, supported false |

## Done criteria

- [x] Diff/drift/clone `cargo test` filters exit 0
- [x] `rg -n '"drift"|"last_clone"|last_clone' crates/daku-core` → both signal ids present
- [x] `clone_source` present in example JSON (`rg` above)
- [x] `plans/README.md` row 007 Status = `DONE`

## STOP conditions

- `clone_instance` fields unconfirmed — STOP; no Now Support guesses.
- Plugin tables empty for admin on a known-active instance — STOP; do not switch to `v_plugin` without research update.

## Maintenance notes

- Plan 008: drift degraded → Environment degraded; last_clone never forces degraded.
- Plan 009 compare strip reads drift payloads.
