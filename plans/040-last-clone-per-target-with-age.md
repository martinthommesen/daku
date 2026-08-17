# Plan 040: Last-clone is answered per clone target ("test: 12 days ago"), not as one timestamp on the source

> **Executor instructions**: Follow this plan step by step. Run every
> verification command and confirm the expected result before moving to the
> next step. If anything in the "STOP conditions" section occurs, stop and
> report — do not improvise. When done, update the status row for this plan
> in `plans/README.md` — unless a reviewer dispatched you and told you they
> maintain the index.
>
> **Drift check (run first)**: `git diff --stat f7fdbe7..HEAD -- crates/daku-core/src/last_clone.rs crates/daku-core/tests/fixtures/last_clone src/dashboard_state.rs docs/research/servicenow-signals.md`
> If any of those files changed since this plan was written, compare the
> "Current state" excerpts against the live code before proceeding; on a
> mismatch, treat it as a STOP condition.

## Status

- **Priority**: P2
- **Effort**: M (Step 1 is an Operator-local spike; Steps 2–4 build)
- **Risk**: MED — mapping `clone_instance.target` to an Environment is the unverified part; STOP conditions guard it
- **Depends on**: plans/011-green-baseline-check-gate.md (gate), plans/013-asleep-never-degrades.md (soft), plans/031-collector-consolidation-typed-signal-state.md (soft — if 031 lands first, implement inside its shared loop; if not, edit `last_clone.rs` as below)
- **Category**: direction
- **Planned at**: commit `f7fdbe7`, 2026-08-17
- **Issue**: https://github.com/martinthommesen/daku/issues/65

## Why this matters

Spec §5: "test and dev treated as **clones of prod** (drift and last-clone first-class)"; Signal #6 "Last-clone date (from prod / clone source)". Issue #9's resolution names "days-since-clone" as first-class. The prototype shows per-Environment ages (`prototypes/environments-overview/index.html:278,291`: `"12 days ago"` / `"41 days ago"`). Today the collector queries the clone source once, drops the `target` field it requests, and persists **one** `last_clone` snapshot under the **source's** id — so for test/dev (the Environments the question is about) the card is `Waiting` forever, and prod shows the most recent clone to *any* target as a raw timestamp. "Is dev stale?" cannot be answered from daku.

Note the research finding: PDIs cannot be clone sources/targets (`docs/research/servicenow-signals.md:17,34`), so this Signal only lights up on real Environments; keep it informational and non-voting (spec: hard-coded rollup; the prototype's "41 days = degraded" is **not** adopted).

## Current state

### `crates/daku-core/src/last_clone.rs`

```rust
// :14-15
pub const LAST_CLONE_SIGNAL_ID: &str = "last_clone";
pub const CLONE_INSTANCE_PATH: &str = "/api/now/table/clone_instance?sysparm_query=state=Completed^ORDERBYDESCcompleted&sysparm_fields=state,completed,target&sysparm_limit=1";

// :17-45  LastCloneObservation { supported: bool, completed: Option<String> } and
//         parse_last_clone(status, body) — takes rows.first().completed only; `target` is ignored.

// :70-118  collect(): finds the `clone_source` Environment (else Ok(())), one request to the source,
//          200/403 → persist_last_clone(&source.id, …) state "healthy" payload {supported, completed};
//          other status / Err → persist_last_clone_unreachable(&source.id, …) state "healthy" payload {reachability:"unreachable", detail}.
```

Fixture `crates/daku-core/tests/fixtures/last_clone/completed.json`:

```json
{ "result": [ { "state": "Completed", "completed": "2026-01-15 12:00:00", "target": "acme-test" } ] }
```

Tests (`last_clone.rs:158-316`): `parse_last_clone` cases; `LastCloneTransport` asserts the URL contains `clone_instance`, `state=Completed`, `sysparm_limit=1`, and the source host `acme-prod`; `env(id, host, clone_source)` builds `instance_url = format!("https://{host}.example.service-now.com")`; `collect_last_clone` runs the collector with `prod` (source) and `test`; `last_clone_signal_completed_writes_healthy_snapshot` asserts a `prod` snapshot and **no** `test` snapshot (`:271-274`) — this plan inverts that.

### Health / UI

- `crates/daku-core/src/health.rs:25` — `LAST_CLONE_SIGNAL_ID` never votes (test `health_rollup_last_clone_never_votes_degraded` at `:179`). Keep.
- `src/dashboard_state.rs:363-367` — `summarize_payload("last_clone")` prints the raw `completed` string; fixture `test` snapshot at `:434-437` is `{"supported":true,"completed":"2026-08-05 09:00:00"}`.
- `crates/daku-core/src/config.rs:22-31` — `EnvironmentConfig { id, label, instance_url, auth_method, sort_order, clone_source }`.

### Research

`docs/research/servicenow-signals.md:34`: `clone_instance` "stores records for all previously and currently scheduled clones; State, source/target, requested/completed. Record lives on the **source**". The exact **format of `target`** (bare instance name like `acme-test`, hostname, or `sys_id` reference) is not recorded — that is Step 1.

## Commands you will need

| Purpose | Command | Expected on success |
|---------|---------|---------------------|
| Signal tests | `cargo test -p daku-core last_clone` | all pass |
| Model tests | `cargo test -p daku dashboard_state` | all pass |
| Operator-local probe (Step 1; needs a real source Environment + Credential) | `cargo run -p daku-daemon -- probe-availability` then inspect `~/.daku/app.db` — or use the now-sdk query per `docs/research/servicenow-signals.md:61` | one `clone_instance` row per target |
| Gate | `bun run check` | exit 0 |

## Scope

**In scope**:
- `docs/research/servicenow-signals.md` (Step 1: record `target` format — **no hostnames**; describe the shape, e.g. "bare instance name = first DNS label")
- `crates/daku-core/src/last_clone.rs`
- `crates/daku-core/tests/fixtures/last_clone/*.json` (add a multi-target fixture)
- `src/dashboard_state.rs` (`summarize_payload("last_clone")` + fixture + test)
- `plans/README.md`

**Out of scope**:
- `health.rs` — last_clone stays non-voting; do not add an age threshold.
- Any protocol/DTO change (`payload_json` is free-form).
- Drift (043) and the collector consolidation (031); if 031 has landed, follow its `Signal` trait instead of editing `collect()` directly.

## Git workflow

- Commit on `main`; do not push unless asked. Suggested: `Record clone_instance.target format.` then `Persist last-clone per target Environment with age.`

## Steps

### Step 1: Spike — what is `clone_instance.target`? (Operator-local)

Against a real clone-source Environment (never a PDI — see research), fetch a few rows with `sysparm_fields=state,completed,target,source,sys_id&sysparm_limit=5` (add `sysparm_display_value=all` in a second call to compare display vs raw). Determine whether `target` (raw) is a bare instance name (`acme-test`), a full hostname, or a `sys_id`, and whether `sysparm_display_value=true` yields the name. Append a short paragraph to `docs/research/servicenow-signals.md` under item 10 stating the format and the query that returns a matchable name — describe the shape only, no real hostnames.

If no real source Environment is available, write the paragraph as "unverified; fixture assumes bare instance name" and continue with that assumption — the STOP condition below covers the mismatch case in production.

**Verify**: `grep -n 'target' docs/research/servicenow-signals.md` → the new paragraph is present.

### Step 2: Parse all rows and group by target

In `last_clone.rs`:

- Change `CLONE_INSTANCE_PATH` to `sysparm_limit=10` (keep `state=Completed^ORDERBYDESCcompleted`, fields `state,completed,target`) and update `LastCloneTransport`'s `sysparm_limit=1` assertion to `sysparm_limit=10`.
- Replace `LastCloneObservation` with a per-target shape:

```rust
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CloneRow { pub target: String, pub completed: String }

/// Newest Completed clone per target, in response order (already newest-first).
pub fn parse_last_clones(status: u16, body: &str) -> Option<Vec<CloneRow>>  // None = unsupported (non-200)
```

keeping first-per-target semantics (a `HashSet<String>` of seen targets). Empty `result` → `Some(vec![])`.

- Matching: `fn target_matches(target: &str, environment: &EnvironmentConfig) -> bool` — case-insensitive compare of `target` against the Environment's host (`instance_url` stripped of scheme/path) **and** against its first DNS label; also `environment.id`. Keep it a pure function with a test.
- Persist: for the source Environment `{ "role": "source", "supported": <bool> }` state `healthy`; for every non-source Environment that matches a row: `{ "completed": <string>, "age_days": <i64>, "source_id": <source.id> }` state `healthy` — `age_days` = `(observed_at - parse(completed)) / 86400`, where `completed` is `YYYY-MM-DD HH:MM:SS` UTC (Table API without display values); parse with a small hand-rolled parser or `httpdate`? No — neither fits; use a 10-line manual parse of that fixed format (year/month/day/hour/min/sec → days since epoch via a civil-from-days routine, or simply compare dates: `age_days` may be computed from the date part only — state the choice). Non-source Environments with **no** matching row: `{ "supported": true, "completed": null }` state `healthy` (renders "never / unknown"). Unreachable/HTTP error keeps the current `persist_last_clone_unreachable` on the **source** only.

Update tests: `last_clone_signal_completed_writes_healthy_snapshot` now expects a `test` snapshot with `completed == "2026-01-15 12:00:00"`, `source_id == "prod"`, `age_days` ≥ 0, and a `prod` snapshot with `role == "source"`. Add fixture `crates/daku-core/tests/fixtures/last_clone/two_targets.json` (rows for `acme-test` newest, older `acme-test`, and `acme-dev`) and a test that `dev` gets its own row and `test` gets the newest. Add `target_matches_host_label_and_id` unit test.

**Verify**: `cargo test -p daku-core last_clone` → all pass.

### Step 3: UI shows age

In `src/dashboard_state.rs` `summarize_payload`, `"last_clone"` arm: if `role == "source"` → `"clone source"`; else if `age_days` present → `format!("{n} days ago")` (`"today"` for 0); else if `completed` null and `supported == true` → `"no clone found"`; else `""`. Update the fixture `test` last_clone snapshot to `{"completed":"2026-08-05 09:00:00","age_days":12,"source_id":"prod"}` and add `prod` `{"role":"source","supported":true}`; add a test asserting `card_summary("last_clone") == "12 days ago"` after `select("test")`. Plan 039's compare strip picks this up automatically.

**Verify**: `cargo test -p daku dashboard_state` → pass.

### Step 4: Gate

**Verify**: `bun run check` → exit 0.

## Test plan

- `last_clone.rs`: `parse_last_clones_groups_newest_per_target`, `target_matches_host_label_and_id`, updated `last_clone_signal_completed_writes_healthy_snapshot`, `last_clone_signal_two_targets_each_get_a_row`; existing 403/500 tests keep passing (source-only behaviour).
- `dashboard_state.rs`: `last_clone_summary_shows_age`.
- `cargo test --workspace --no-fail-fast` → 0 failed.

## Done criteria

- [ ] `grep -n 'sysparm_limit=10' crates/daku-core/src/last_clone.rs` → 1 match; `grep -n 'age_days\|source_id' crates/daku-core/src/last_clone.rs` → ≥2 matches
- [ ] `grep -n 'target' docs/research/servicenow-signals.md` → new paragraph present, no real hostnames (`git diff docs/research | grep -i 'service-now.com'` shows only `example` hosts or nothing)
- [ ] `cargo test -p daku-core last_clone` and `cargo test -p daku dashboard_state` pass with the new tests
- [ ] `health.rs` unchanged (`git diff --stat -- crates/daku-core/src/health.rs` empty)
- [ ] `bun run check` exits 0
- [ ] `plans/README.md` status row for 040 updated

## STOP conditions

- Step 1 shows `target` is a `sys_id`/reference with no name available even with display values — report; the mapping needs a second query (`instance` table) and a design decision.
- Plan 031 landed and `last_clone.rs` no longer has `collect()` as excerpted — implement inside 031's shape; if that shape can't express "one probe on the source writes N snapshots", report.
- Any `health.rs` change appears necessary.

## Maintenance notes

- Ages come from the source's clock and the daemon's clock; a skew of hours is irrelevant at day granularity — do not add timezone handling.
- If an Environment's `instance_url` host does not match `target` for a real customer (e.g. custom domains), the row falls through to "no clone found"; the fix is a per-Environment `clone_target_name` in `environments.json` — add it only when a real Operator hits it.
- Reviewers: last_clone must remain non-voting; the prototype's degraded-at-41-days idea contradicts the spec's hard-coded rollup and was deliberately not adopted.
