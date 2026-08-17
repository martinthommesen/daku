# Plan 007: Version / plugin drift + last-clone Signals

> **Executor instructions**: Follow this plan step by step. Run every verification command and confirm the expected result before moving to the next step. If anything in the "STOP conditions" section occurs, stop and report — do not improvise. When done, update the status row for this plan in `plans/README.md`.
>
> **Drift check (run first)**: confirm plan 003 writes availability snapshots that include build/`glide.war` when reachable. Then `git diff --stat da67ae9..HEAD -- plans/007-drift-last-clone.md crates/daku-core`.

## Status

- **Priority**: P2
- **Effort**: L
- **Risk**: MED
- **Depends on**: plans/003-availability-signal.md
- **Category**: direction
- **Planned at**: commit `da67ae9`, 2026-08-17
- **Issue**: https://github.com/martinthommesen/daku/issues/21

## Why this matters

Post-clone drift and “when did we last clone?” are first-class for prod/test/dev (spec §5 Signals #5–#6). Drift is **cross-Environment** (compare plugin/app versions and builds); last-clone is read from the **clone source** Environment (usually prod), not from the target. These are two `signal_id`s so the UI can show two cards, authored in one plan because they share inventory fetch helpers.

## Current state

- Availability already probes `glide.war` (plan 003) — reuse stored build string when present; do not require a second `sys_properties` call if the latest availability snapshot is fresh (< poll interval × 2). Otherwise fetch `glide.war` the same way as 003.
- Research — [servicenow-signals](https://github.com/martinthommesen/daku/blob/research/servicenow-signals/docs/research/servicenow-signals.md):

  **Build / family:** `sys_properties` `name=glide.war` (same as availability).

  **Plugins / apps:** Table API on `sys_plugins` (id, version, active) and `sys_store_app` (version, latest_version, active); optionally `sys_app` for custom scoped apps. Diff by `id`/`scope` across Environments.

  **Last clone:** Table API on `clone_instance` on the **source** Environment. States include Completed, etc. PDI: **no** clone source/target — Signal should be `healthy` with payload `{"supported": false}` or state `healthy` + `status: "unavailable_on_environment"` without alarming.

- Config: Environments in `~/.daku/environments.json` need a way to know which id is the clone **source** (default: Environment with label/role `prod` or explicit `clone_source: true`). Document the field in the example JSON from plan 002 (extend example only; no real URLs).
- CONTEXT.md: two Signals — `drift` and `last_clone`.

## Commands you will need

| Purpose | Command | Expected on success |
|---------|---------|---------------------|
| Tests | `cargo test -p daku-core drift` and `cargo test -p daku-core last_clone` | all pass |
| Check | `cargo check -p daku-core -p daku-daemon` | exit 0 |

## Suggested executor toolkit

- Research rows 8–10 (upgrade/build, plugins, clone history).
- Spec §5 default prod/test/dev as clones of prod.

## Scope

**In scope**

### A. Inventory fetch (per Environment)

- Fetch plugin/store-app version maps (fixtures: small JSON arrays). Cap pages (`sysparm_limit` + offset or `sysparm_query` pagination) — v1 may cap at first 1000 rows and record `truncated: true` in payload if more remain; STOP if pagination API usage is unclear — prefer documented Table API offset pattern only.
- Fake plugin ids in fixtures: `com.example.plugin_a`, never customer-looking names that imply a real tenant.

### B. `signal_id = "drift"`

- After inventories exist for ≥2 configured Environments (or compare each non-source to source):
  - Diff build string (`glide.war`) source vs other.
  - Diff plugin/app version maps: missing / extra / version mismatch counts.
- Payload example:

  ```json
  {
    "compared_to": "prod",
    "build_matches": false,
    "plugin_mismatches": 3,
    "plugin_only_here": 1,
    "plugin_only_there": 0,
    "truncated": false
  }
  ```

- State:
  - Environments that are the designated source: `healthy` with payload `{"role":"source"}` (no self-diff), **or** skip writing drift on source — pick one and document; prefer writing a snapshot so UI always finds a row.
  - Other Environments: `healthy` if build matches and mismatch counts are 0; `degraded` otherwise.
  - Single-Environment config: `healthy` + `{"skipped":"need_two_environments"}`.

### C. `signal_id = "last_clone"`

- Query **only** the clone-source Environment: recent `clone_instance` rows with state Completed (field names from research: requested/completed timestamps — use whatever fields appear in fixtures shaped like Table API `result` arrays; name them in parser to match research “State, source/target, requested/completed”).
- Payload: `last_completed_at` (ISO), `target_label` if present, `supported: true`.
- State: always `healthy` for v1 (informational). If table missing/403: `healthy` + `supported: false` (PDI / ACL) — **not** degraded.
- Do not invent Now Support APIs.

### D. Fixtures + daemon

- `crates/daku-core/tests/fixtures/drift/` and `.../last_clone/`.
- Unit tests for pure diff function (no HTTP).
- Wire collectors into daemon poll after per-Environment inventory fetch.

**Out of scope**

- Clone Admin Console–only Australia UI scraping.
- Auto-triggering clones.
- Matrix UI (spec: deferred secondary view) — payloads must still support a future compare strip.
- GPUI (009).

## Git workflow

- Branch: `plan/007-drift-last-clone`
- Commit example: `Add drift and last-clone Signals`

## Steps

### Step 1: Pure diff helper

Given two `HashMap<id, version>` + two build strings → mismatch counts.

**Verify**: `cargo test -p daku-core diff_plugin_inventory` → pass.

### Step 2: Drift snapshot writer

Multi-env fixture config (example.com URLs) drives classifier without network.

**Verify**: `cargo test -p daku-core drift_signal` → match=healthy; mismatch=degraded.

### Step 3: Last-clone parser

Completed row → timestamp; empty/403 → supported false.

**Verify**: `cargo test -p daku-core last_clone_signal` → pass.

### Step 4: HTTP inventory + clone fetch (injectable)

Reuse availability property GET for build when needed.

**Verify**: mock tests; no network in `cargo test`.

### Step 5: Config field + example JSON

Document `clone_source` (bool) on Environments in `environments.example.json`.

**Verify**: example file validates mentally against plan 002 shape; `rg -n 'service-now\\.com' environments.example.json` → only `example.service-now.com` fakes.

## Test plan

| Case | Expected |
|------|----------|
| identical inventories | drift healthy |
| version mismatch | drift degraded |
| source Environment | drift non-alarming snapshot |
| PDI / 403 clone table | last_clone healthy, supported false |
| completed clone row | last_clone has timestamp |

## Done criteria

- [ ] Diff tests + collectors green, no network
- [ ] Two signal_ids: `drift`, `last_clone`
- [ ] Clone queried only on source Environment
- [ ] `plans/README.md` row 007 → `done`

## STOP conditions

- `clone_instance` field names cannot be confirmed from research + Operator smoke — stop rather than guessing Now Support.
- Plugin tables return empty for admin monitoring user on a known-active instance — stop; do not switch to undocumented virtual tables without research update (`v_plugin` is mentioned as richer but unverified — only use if Operator smoke confirms).

## Maintenance notes

- Plan 008: drift degraded → Environment degraded; last_clone informational (does not force degraded).
- Plan 009 compare strip reads drift payloads.
- Reviewers: public hygiene on fixture plugin ids; no real clone target hostnames.
