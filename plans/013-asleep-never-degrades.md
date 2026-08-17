# Plan 013: An asleep Environment is never rolled up as degraded, and secondary Signals skip probing it

> **Executor instructions**: Follow this plan step by step. Run every
> verification command and confirm the expected result before moving to the
> next step. If anything in the "STOP conditions" section occurs, stop and
> report — do not improvise. When done, update the status row for this plan
> in `plans/README.md` — unless a reviewer dispatched you and told you they
> maintain the index.
>
> **Drift check (run first)**: `git diff --stat f7fdbe7..HEAD -- crates/daku-core/src/health.rs crates/daku-core/src/availability.rs crates/daku-core/src/persistence.rs crates/daku-core/src/jobs.rs crates/daku-core/src/syslog.rs crates/daku-core/src/mid_ecc.rs crates/daku-core/src/outbound.rs src/dashboard_state.rs`
> If any of those files changed since this plan was written, compare the
> "Current state" excerpts against the live code before proceeding; on a
> mismatch, treat it as a STOP condition.

## Status

- **Priority**: P1
- **Effort**: M (S for Step 1 alone)
- **Risk**: LOW
- **Depends on**: plans/011-green-baseline-check-gate.md (gate only)
- **Category**: bug (decision drift) + perf
- **Planned at**: commit `f7fdbe7`, 2026-08-17
- **Issue**: https://github.com/martinthommesen/daku/issues/37

## Why this matters

The spec and the plan index lock the rule: reachability (`reachable` | `unreachable` | `asleep`) is separate from Environment health, and **asleep must not become health=`degraded`** (`plans/README.md` "Ownership locks"; `docs/spec/v1.md` §6 "distinct from up but unhealthy"; ADR-0004). A hibernating PDI is the normal state of a dev stand-in.

Today the code violates it in two places:

1. `health_rollup` only special-cases `Unreachable`; for `Asleep` any `"down"`/`"degraded"` vote yields `Degraded`. A test even pins that (`health_rollup_asleep_signal_down_is_degraded_not_down`).
2. Only the Availability probe recognises the hibernation HTML splash. Jobs, syslog, MID/ECC and outbound then send their Aggregate-API requests to the same sleeping instance, get the same HTML, fail JSON parsing, and persist `"down"`. Result: a sleeping PDI shows an amber dot and four red "down" cards, and burns 7+ HTTP round-trips per tick for nothing.

After this plan: an asleep Environment rolls up **healthy** (dot green, reachability badge says asleep), the four per-Environment secondary Signals persist a `skipped` snapshot instead of probing when Availability in the same tick reported asleep or unreachable, and the cards render "skipped" with a neutral dot.

## Current state

### `crates/daku-core/src/health.rs`

```rust
// :19-33
pub fn health_rollup(reachability: Reachability, signals: &[(&str, &str)]) -> EnvironmentHealth {
    if reachability == Reachability::Unreachable {
        return EnvironmentHealth::Down;
    }
    let mut health = EnvironmentHealth::Healthy;
    for &(signal_id, state) in signals {
        if signal_id == LAST_CLONE_SIGNAL_ID || state == "skipped" {
            continue;
        }
        if state == "down" || state == "degraded" {
            health = EnvironmentHealth::Degraded;
        }
    }
    health
}
```

Tests to change (`health.rs:197-218`):

```rust
    #[test]
    fn health_rollup_asleep_with_degraded_signal_is_degraded() {
        assert_eq!(
            health_rollup(Reachability::Asleep, &[("jobs", "degraded")]),
            EnvironmentHealth::Degraded
        );
    }
    …
    #[test]
    fn health_rollup_asleep_signal_down_is_degraded_not_down() {
        let health = health_rollup(Reachability::Asleep, &[("jobs", "down")]);
        assert_eq!(health, EnvironmentHealth::Degraded);
        assert_ne!(health, EnvironmentHealth::Down);
    }
```

Keep-as-is: `health_rollup_asleep_without_degraded_signals_is_healthy`, `health_rollup_asleep_with_no_signals_is_healthy`, `health_rollup_skips_missing_and_skipped_signals`.

`Reachability` here is `daku_protocol::Reachability` (`crates/daku-protocol/src/protocol.rs`, variants `Reachable | Unreachable | Asleep`). `publish_dashboard` (`health.rs:46-116`) derives an Environment's reachability from the availability snapshot's payload `"reachability"` string via `wire_reachability` (`:35-44`) — unchanged by this plan.

### `crates/daku-core/src/availability.rs`

Has its own `pub enum Reachability { Reachable, Unreachable, Asleep }` with `as_str()` → `"reachable" | "unreachable" | "asleep"` (`:17-32`). `persist_availability_snapshot` (`:128-148`) writes payload `{"reachability": <as_str>, "rtt_ms", "build", "error"}` under `AVAILABILITY_SIGNAL_ID = "availability"`. The Availability collector is registered **first** in `build_default_loop` (`crates/daku-core/src/collector.rs:109-114`) and `CollectorLoop::tick` runs collectors sequentially (`collector.rs:60-71`), so by the time jobs/syslog/mid_ecc/outbound run, the same tick's availability snapshot is already committed in SQLite. `drift.rs:236-255` (`reuse_availability_build`) already reads that snapshot back with a `max_age_secs` freshness check — reuse that shape.

### `crates/daku-core/src/persistence.rs`

`persist_signal_snapshot(connection, environment_id, signal_id, observed_at, state: &str, payload_json: &str)` (`:138-159`) upserts on `(environment_id, signal_id)`. `load_signal_snapshot(connection, environment_id, signal_id) -> io::Result<Option<SignalSnapshot>>` (`:183-`) — `SignalSnapshot { environment_id, signal_id, observed_at, state, payload_json }`.

### The four per-Environment collectors

Each has the same loop shape; e.g. `crates/daku-core/src/outbound.rs:49-70`:

```rust
    fn collect(&self) -> anyhow::Result<()> {
        let connection = self.store.open()?;
        let observed_at = SystemTime::now() … as i64 …;
        let mut first_error = None;
        for environment in &self.environments {
            let count = fetch_aggregate_count(&self.client, environment, self.credentials.as_ref(), OUTBOUND_HTTP_PATH);
            if let Err(error) = collect_outbound(&connection, environment, observed_at, count) {
                first_error.get_or_insert(error);
            }
        }
        match first_error { Some(error) => Err(error), None => Ok(()) }
    }
```

Same in `jobs.rs:51-86` (two `fetch_aggregate_count` calls, then `prune_signal_samples`), `syslog.rs:55-76` (one call + prune), `mid_ecc.rs:81-113` (`fetch_mid_agents` + two counts). Signal id constants: `JOBS_SIGNAL_ID = "jobs"`, `SYSLOG_SIGNAL_ID = "syslog"`, `MID_ECC_SIGNAL_ID` (check the exact name in `mid_ecc.rs`), `OUTBOUND_SIGNAL_ID` (check `outbound.rs`).

Test scaffolding in each of those files' `mod tests` (see `jobs.rs:166-262`): a `prod()` `EnvironmentConfig` (id `"prod"`, Basic auth), a temp DB via `std::env::temp_dir().join(format!("daku-jobs-{}.db", uuid::Uuid::new_v4()))`, `MemoryCredentialStore` seeded with `("prod", r#"{"username":"reader","password":"secret"}"#)`, and a small `HttpTransport` struct returning fixture bodies. `jobs_signal_probe_failure_is_down_without_sample` shows the "down" path.

### `src/dashboard_state.rs`

`summarize_payload(signal_id, payload_json)` (`:295-370`) formats a card's summary; for `"jobs"` it prints `"{overdue} overdue · {error} error"` with `unwrap_or(0)`, so a skipped payload would print `0 overdue · 0 error` (misleading). `signal_card` in `src/app.rs:246-286` shows `card.status` when the summary is empty and colours the dot via `status_dot` (`app.rs:357-365`: `"healthy"|"degraded"|"down"`, anything else → `theme.text_ghost`). Tests: `src/dashboard_state.rs` `mod tests` (`:497+`) with a `loaded()` fixture; `card_summary` is asserted for jobs at `:549`.

Vocabulary (CONTEXT.md): Environment, Signal, Environment health, Operator. Use `skipped` as the snapshot **state** and put the reason in the payload.

## Commands you will need

| Purpose | Command | Expected on success |
|---------|---------|---------------------|
| Health tests | `cargo test -p daku-core health_rollup` | all pass |
| Availability tests | `cargo test -p daku-core recent_reachability` | all pass |
| Collector tests | `cargo test -p daku-core skips_when` | all pass |
| UI state tests | `cargo test -p daku dashboard_state` | all pass |
| Gate | `bun run check` | exit 0 |

## Scope

**In scope**:
- `crates/daku-core/src/health.rs` (rollup + 2 tests + use the new const)
- `crates/daku-core/src/persistence.rs` (`SKIPPED_STATE` const + `persist_signal_skipped`)
- `crates/daku-core/src/availability.rs` (`recent_reachability` helper + tests)
- `crates/daku-core/src/jobs.rs`, `syslog.rs`, `mid_ecc.rs`, `outbound.rs` (gate call in the loop + one test each)
- `src/dashboard_state.rs` (`summarize_payload` early return + one test)
- `plans/README.md` (status row)

**Out of scope**:
- `crates/daku-core/src/drift.rs` and `last_clone.rs` — different loop shapes (source vs others); the rollup fix already covers their health effect. Deferred (see Maintenance notes).
- `crates/daku-core/src/collector.rs` — do not reorder collectors or change `tick`.
- Any change to protocol types (`daku-protocol`) — `state` stays a string on the wire.
- Consolidating the seven collectors into one generic loop (separate tech-debt plan). Yes, this plan adds the same 3 lines to four files; that is intentional for now.

## Git workflow

- Commit on `main`; do not push unless asked. Two commits suggested: `Asleep Environments roll up healthy, never degraded.` then `Skip secondary Signals when Availability reports asleep or unreachable.`

## Steps

### Step 1: Rollup — asleep ignores Signal votes

In `crates/daku-core/src/persistence.rs` add (near the top, after `DAKU_DB_PATH_ENV`):

```rust
/// Snapshot state for a Signal that deliberately did not probe this tick.
pub const SKIPPED_STATE: &str = "skipped";
```

In `crates/daku-core/src/health.rs`, replace `health_rollup` with:

```rust
pub fn health_rollup(reachability: Reachability, signals: &[(&str, &str)]) -> EnvironmentHealth {
    match reachability {
        // Reachability is reported separately; a sleeping Environment cannot
        // be observed, so its Signals must not vote.
        Reachability::Unreachable => return EnvironmentHealth::Down,
        Reachability::Asleep => return EnvironmentHealth::Healthy,
        Reachability::Reachable => {}
    }
    let mut health = EnvironmentHealth::Healthy;
    for &(signal_id, state) in signals {
        if signal_id == LAST_CLONE_SIGNAL_ID || state == persistence::SKIPPED_STATE {
            continue;
        }
        if state == "down" || state == "degraded" {
            health = EnvironmentHealth::Degraded;
        }
    }
    health
}
```

(`persistence` is already imported in `health.rs`: `use crate::persistence::{self, SAMPLE_RETENTION_SECS, StateStore};`.)

Replace the two tests quoted above with:

```rust
    #[test]
    fn health_rollup_asleep_ignores_signal_votes() {
        assert_eq!(
            health_rollup(Reachability::Asleep, &[("jobs", "degraded")]),
            EnvironmentHealth::Healthy
        );
        assert_eq!(
            health_rollup(Reachability::Asleep, &[("jobs", "down"), ("syslog", "down")]),
            EnvironmentHealth::Healthy
        );
    }
```

**Verify**: `cargo test -p daku-core health_rollup` → all pass (one fewer test than before, none failing).

### Step 2: `recent_reachability` helper

In `crates/daku-core/src/availability.rs` add:

```rust
/// Freshness window for reusing this tick's Availability result in later
/// Signals. Availability runs first in the same tick, so seconds usually
/// separate the two; the window only tolerates a slow tick.
pub const REACHABILITY_REUSE_SECS: i64 = 300;

/// Reachability the Availability Signal recorded for `environment_id` within
/// `max_age_secs` of `observed_at`, if any.
pub fn recent_reachability(
    connection: &Connection,
    environment_id: &str,
    observed_at: i64,
    max_age_secs: i64,
) -> Option<Reachability> {
    let snapshot =
        persistence::load_signal_snapshot(connection, environment_id, AVAILABILITY_SIGNAL_ID)
            .ok()
            .flatten()?;
    if observed_at.saturating_sub(snapshot.observed_at) > max_age_secs {
        return None;
    }
    let payload: serde_json::Value = serde_json::from_str(&snapshot.payload_json).ok()?;
    match payload.get("reachability").and_then(|value| value.as_str())? {
        "reachable" => Some(Reachability::Reachable),
        "unreachable" => Some(Reachability::Unreachable),
        "asleep" => Some(Reachability::Asleep),
        _ => None,
    }
}
```

In `crates/daku-core/src/persistence.rs` add:

```rust
/// Records that `signal_id` deliberately skipped probing (`reason` is
/// `"asleep"` or `"unreachable"` — the Availability outcome it deferred to).
pub fn persist_signal_skipped(
    connection: &Connection,
    environment_id: &str,
    signal_id: &str,
    observed_at: i64,
    reason: &str,
) -> io::Result<()> {
    let payload = serde_json::json!({ "skipped": reason });
    persist_signal_snapshot(connection, environment_id, signal_id, observed_at, SKIPPED_STATE, &payload.to_string())
}
```

(`serde_json` is a dependency of `daku-core`; add `use serde_json` only if the file does not already reference it — it currently does not; `serde_json::json!` fully-qualified needs no import.)

Tests in `availability.rs` `mod tests` (temp DB pattern as in `persist_availability_snapshot_writes_one_row` in the same module):

- `recent_reachability_reads_fresh_asleep_snapshot`: persist an availability observation with `Reachability::Asleep` at `observed_at = 1_700_000_000`, call `recent_reachability(&conn, "prod", 1_700_000_010, REACHABILITY_REUSE_SECS)` → `Some(Reachability::Asleep)`.
- `recent_reachability_ignores_stale_snapshot`: same snapshot, `observed_at + 301` → `None`; missing snapshot → `None`.

**Verify**: `cargo test -p daku-core recent_reachability` → 2 passed.

### Step 3: Gate the four collectors

In each of `jobs.rs`, `syslog.rs`, `mid_ecc.rs`, `outbound.rs`, at the top of the `for environment in &self.environments {` loop body (before any `fetch_*` call), insert:

```rust
            if let Some(reachability @ (Reachability::Asleep | Reachability::Unreachable)) =
                recent_reachability(&connection, &environment.id, observed_at, REACHABILITY_REUSE_SECS)
            {
                if let Err(error) = persistence::persist_signal_skipped(
                    &connection,
                    &environment.id,
                    <THIS_SIGNAL_ID>,
                    observed_at,
                    reachability.as_str(),
                ) {
                    first_error.get_or_insert_with(|| anyhow::Error::from(error));
                }
                continue;
            }
```

with `<THIS_SIGNAL_ID>` = the file's own signal-id constant, and add the imports `use crate::availability::{REACHABILITY_REUSE_SECS, Reachability, recent_reachability};` (adjust if `persistence` is not yet imported as a module in that file — `jobs.rs` already has `use crate::persistence::{self, StateStore};`).

Note for `jobs.rs`/`syslog.rs`: keep the `prune_signal_samples` call after the loop as-is. `mid_ecc.rs`: `fetch_mid_agents` and both counts are arguments to `collect_mid_ecc(...)`; the gate goes before that whole `if let Err(error) = collect_mid_ecc(` statement.

Add one test per file (model on that file's existing "probe failure is down" test; name `<signal>_signal_skips_when_availability_asleep`):

1. Open the temp store, persist an availability snapshot for `prod` with `Reachability::Asleep` and `observed_at = now` (`SystemTime::now()` secs) using `crate::availability::persist_availability_snapshot` and an `AvailabilityObservation { reachability: Reachability::Asleep, state: SignalState::Healthy, build: None, rtt_ms: 0, error: None }`.
2. Build the collector with a transport whose `execute` does `panic!("must not probe an asleep Environment")`.
3. `collector.collect().unwrap()`; load the signal's snapshot → `state == "skipped"`, payload `["skipped"] == "asleep"`; for jobs/syslog also assert `load_signal_samples(...).len() == 0`.

**Verify**: `cargo test -p daku-core skips_when` → 4 passed; `cargo test -p daku-core` → all pass (no existing collector test regresses — they persist no availability snapshot, so the gate is a no-op for them).

### Step 4: Card summary for skipped snapshots

In `src/dashboard_state.rs` `summarize_payload`, right after the `let Ok(value) = … else { return String::new(); };` line, add:

```rust
    if value.get("skipped").is_some() {
        return String::new();
    }
```

(so the card falls back to showing its status text, `skipped`, with the neutral dot). Add a test in `dashboard_state.rs` `mod tests`: `summarize_payload("jobs", r#"{"skipped":"asleep"}"#)` → `""` (call the private fn directly; the tests module is inside the file). Confirm the drift `need_two_environments` behaviour is unchanged (`summarize_payload("drift", r#"{"skipped":"need_two_environments"}"#)` → `""`).

**Verify**: `cargo test -p daku dashboard_state` → all pass.

### Step 5: Gate

**Verify**: `bun run check` → exit 0.

## Test plan

- `health.rs`: `health_rollup_asleep_ignores_signal_votes` (replaces two tests).
- `availability.rs`: `recent_reachability_reads_fresh_asleep_snapshot`, `recent_reachability_ignores_stale_snapshot`.
- `jobs.rs`, `syslog.rs`, `mid_ecc.rs`, `outbound.rs`: `<signal>_signal_skips_when_availability_asleep` (panic-on-request transport).
- `src/dashboard_state.rs`: `summarize_payload_is_empty_for_skipped`.
- Verification: `cargo test --workspace --no-fail-fast` → 0 failed.

## Done criteria

- [ ] `grep -n 'Reachability::Asleep => return EnvironmentHealth::Healthy' crates/daku-core/src/health.rs` → 1 match
- [ ] `grep -c 'recent_reachability' crates/daku-core/src/{jobs,syslog,mid_ecc,outbound}.rs` → ≥1 each
- [ ] `grep -n 'pub fn persist_signal_skipped\|pub const SKIPPED_STATE' crates/daku-core/src/persistence.rs` → 2 matches
- [ ] `cargo test -p daku-core` passes with the 7 new tests; `cargo test -p daku` passes with 1 new test
- [ ] `bun run check` exits 0
- [ ] `git status` shows only in-scope files modified
- [ ] `plans/README.md` status row for 013 updated

## STOP conditions

- `health_rollup`'s signature or the two named tests do not match the excerpts.
- The four collectors no longer share the `for environment in &self.environments` loop shape (a consolidation landed first) — report; the gate then belongs in the shared loop.
- Availability is no longer registered first in `build_default_loop` (`collector.rs`) — the same-tick assumption breaks; report.
- Any existing `*_signal_*` test fails after Step 3.

## Maintenance notes

- Drift and last-clone still probe asleep/unreachable Environments and persist `"down"` (drift) — health is unaffected after Step 1, but their cards stay red. Add the same gate to `drift.rs::collect_other` when touching drift next.
- When the collectors are consolidated (tech-debt backlog), move the gate into the shared loop and delete the four copies.
- Reviewers: check that `skipped` never reaches `health_rollup` as a vote (it is filtered by `SKIPPED_STATE`) and that no collector persists `"down"` for a hibernation HTML body any more in the gated paths.
