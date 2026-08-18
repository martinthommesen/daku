# Plan 031: One per-Environment collector loop and a typed `SignalState` (end the seven-file lockstep)

> **Executor instructions**: Follow this plan step by step. Run every
> verification command and confirm the expected result before moving to the
> next step. If anything in the "STOP conditions" section occurs, stop and
> report — do not improvise. When done, update the status row for this plan
> in `plans/README.md` — unless a reviewer dispatched you and told you they
> maintain the index.
>
> **Drift check (run first)**: `git diff --stat f7fdbe7..HEAD -- crates/daku-protocol/src/protocol.rs crates/daku-core/src/collector.rs crates/daku-core/src/persistence.rs crates/daku-core/src/availability.rs crates/daku-core/src/jobs.rs crates/daku-core/src/syslog.rs crates/daku-core/src/mid_ecc.rs crates/daku-core/src/outbound.rs crates/daku-core/src/drift.rs crates/daku-core/src/last_clone.rs crates/daku-core/src/health.rs`
> Plans 013 (asleep gate) and 028 (temp-DB test helper) are prerequisites and
> WILL appear in this diff — expected. Read both plans and the live code before
> starting; the excerpts below are from `f7fdbe7` and note where 013 changes them.
> Any *other* mismatch → STOP.

## Status

- **Priority**: P2
- **Effort**: M–L
- **Risk**: MED (touches every Signal; mitigated by 65+ behaviour tests that must stay green unchanged in intent)
- **Depends on**: plans/011 (gate), plans/013-asleep-never-degrades.md (this plan absorbs its four gate copies), plans/028-temp-db-test-helper-and-collector-isolation.md (test scaffolding). Ordering vs plans/022 (per-Environment concurrency): the index lands 022 first (it is `collector.rs`-only and adds `register_group`); this plan must then keep the per-Environment group structure 022 introduces (its concurrency lives in `collector.rs`, not in the collectors) — re-read `collector.rs` at execution time. If 022 has NOT landed yet, this plan is free to shape the loop as written and 022 wraps it afterwards.
- **Category**: tech-debt
- **Planned at**: commit `f7fdbe7`, 2026-08-17
- **Issue**: https://github.com/martinthommesen/daku/issues/51

## Why this matters

Seven Signal modules copy-paste the same `collect()`: open the store, take `observed_at`, loop over Environments, probe, persist ok/down, aggregate `first_error`. The copies have already drifted:

- `last_clone` persists state `"healthy"` for an **unreachable** probe (`last_clone.rs:137-155`) while every other Signal writes `"down"` → the card renders green after a failed read.
- `health.rs:25` special-cases `state == "skipped"`, but drift persists its skipped case as `"healthy"` (`drift.rs:348-361`) — the branch was unreachable until plan 013 introduced a real `skipped` state.
- `prune_signal_samples` is called inside both `jobs.rs` and `syslog.rs` (twice per tick); any new sampled Signal must remember to add it.
- States are `&'static str` everywhere (`"healthy"`, `"degraded"`, `"down"`, `"skipped"`) and `availability.rs` carries its own `Reachability`/`SignalState` enums that mirror `daku_protocol::Reachability`/`EnvironmentHealth`; typos compile.
- Plan 013 adds the asleep/unreachable skip gate to four files — the fifth copy of a cross-cutting rule.

After this plan: `crates/daku-core/src/collector.rs` owns the loop once (`PerEnvironmentCollector<S: Signal>`), the five per-Environment Signals (availability, jobs, syslog, mid_ecc, outbound) are small `Signal` impls, `SignalState` is an enum in `daku-protocol`, `persist_signal_snapshot` takes it, `health_rollup` matches on it, and last-clone unreachable renders `down` (non-voting, as today). Drift and last-clone keep their own `SignalCollector` impls (source-vs-others shape) but use the shared helpers and typed states.

## Current state (at `f7fdbe7`; 013 adds a gate block at the top of four loops)

### `crates/daku-protocol/src/protocol.rs`

```rust
// :98-112
pub enum EnvironmentHealth { Healthy, Degraded, Down }        // serde camelCase
pub enum Reachability { Reachable, Unreachable, Asleep }        // serde camelCase
// :125-132
pub struct SignalSnapshotDto { pub signal_id: String, pub state: String, pub observed_at: i64, pub payload_json: String }
```

(`state` stays a `String` on the wire in this plan — the GPUI client compares it as text and uses a `"Waiting"` sentinel; changing the DTO would touch `src/dashboard_state.rs`/`src/app.rs` for no gain.)

### `crates/daku-core/src/collector.rs`

```rust
// :39-41
pub trait SignalCollector: Send + Sync {
    fn collect(&self) -> anyhow::Result<()>;
}
// :43-46 pub struct CollectorLoop { interval, collectors: Vec<Box<dyn SignalCollector>> }
// :60-71 tick(): for collector in &self.collectors { if let Err(e) = collector.collect() { first_error.get_or_insert(e) } } → first error
// :100-152 build_default_loop(environments, credentials, store, interval, client) registers, in order:
//   AvailabilityCollector::new(environments.clone(), credentials.clone(), client.clone(), store.clone())
//   JobsCollector::new(…), SyslogCollector::new(…), MidEccCollector::new(…), OutboundCollector::new(…),
//   DriftCollector::new(…, interval), LastCloneCollector::new(environments, credentials, client, store)
```

### `crates/daku-core/src/persistence.rs`

```rust
// :138-159
pub fn persist_signal_snapshot(connection: &Connection, environment_id: &str, signal_id: &str,
    observed_at: i64, state: &str, payload_json: &str) -> io::Result<()>   // upsert on (environment_id, signal_id)
// :219  pub fn persist_signal_sample(connection, environment_id, signal_id, observed_at, value_real: Option<f64>, value_text: Option<&str>?) — read the exact signature
// :275  pub fn prune_signal_samples(connection: &Connection, now: i64) -> io::Result<usize>
// (013 adds) pub const SKIPPED_STATE: &str = "skipped"; pub fn persist_signal_skipped(connection, environment_id, signal_id, observed_at, reason: &str)
```

### The five per-Environment Signals — identical skeleton

`availability.rs:150-221`, `jobs.rs:27-86`, `syslog.rs:31-77`, `outbound.rs:25-71`, `mid_ecc.rs:57-113`: a 4-field struct `{ environments: Vec<EnvironmentConfig>, credentials: Arc<dyn CredentialStore>, client: Arc<ServiceNowClient>, store: StateStore }` + `new(environments, credentials, client: impl Into<Arc<ServiceNowClient>>, store)`, and

```rust
impl SignalCollector for XCollector {
    fn collect(&self) -> anyhow::Result<()> {
        let connection = self.store.open()?;
        let observed_at = SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_secs() as i64).unwrap_or(0);
        let mut first_error = None;
        for environment in &self.environments {
            // (013 inserts the recent_reachability gate here in jobs/syslog/mid_ecc/outbound)
            … fetch … ; if let Err(error) = collect_x(&connection, environment, observed_at, …) { first_error.get_or_insert(error); }
        }
        // jobs.rs:79-81 and syslog.rs:70-72 additionally: prune_signal_samples(&connection, observed_at)
        match first_error { Some(error) => Err(error), None => Ok(()) }
    }
}
```

Per-Signal specifics the trait must preserve:

| Signal | id const | probe → payload | state fn | sample | down payload |
|---|---|---|---|---|---|
| availability | `AVAILABILITY_SIGNAL_ID` | `probe()` (`:171-195`) → `AvailabilityObservation { reachability, state, build, rtt_ms, error }`; `persist_availability_snapshot` (`:128-148`) payload `{reachability, rtt_ms, build, error}` | in observation | none | never Err — transport error becomes `Unreachable/Down` observation |
| jobs | `JOBS_SIGNAL_ID` | two `fetch_aggregate_count` (`JOBS_OVERDUE_PATH`, `JOBS_ERROR_PATH`) → `{overdue_ready, error}` | `jobs_state(overdue_ready)` (`:20-26`) | `Some((overdue+error) as f64)` (`persist_jobs_ok`) | `{"reachability":"unreachable","detail":msg}` state `down` (`:145-163`) |
| syslog | `SYSLOG_SIGNAL_ID` | one count (`syslog_error_path()`) → `{error_count_1h}` | `syslog_state` (`:23-29`) | `Some(count as f64)` (`:97-118`) | same shape (`:120-138`) |
| outbound | `OUTBOUND_SIGNAL_ID` | one count (`OUTBOUND_HTTP_PATH`) → `{outbound_http_4xx_5xx_1h}` | `outbound_state` (`:17-23`) | none | same shape (`:112-130`) |
| mid_ecc | `MID_ECC_SIGNAL_ID` | `fetch_mid_agents` + two counts → `{agents_total, agents_unhealthy, ecc_output_ready, ecc_error}` | `mid_ecc_state(unhealthy, error, ready)` (`:49-55`) | none | same shape (`:188-206`) |

`first_error` semantics: jobs/mid_ecc collapse partial failures into one message via `.err().or_else(...)` (`jobs.rs:104-112`, `mid_ecc.rs:141-152`); the resulting **snapshot** is `down` and `collect()` still returns `Ok` unless persistence failed. Keep exactly that.

### Drift and last-clone (different shapes — stay separate)

`drift.rs:64-125`: finds the `clone_source` Environment; `< 2` envs or no source → `persist_all_skipped` (`:331-345`, state `"healthy"`, payload `{"skipped":"need_two_environments"}`); fetches the source inventory once, then `collect_other` per non-source env (`:128-162`); down payloads `persist_drift_down` (`:363-381`); `DriftCollector` has a 5th field `poll_interval` (`:37-60`).

`last_clone.rs:71-118`: probes only the source; `persist_last_clone` (`:120-136`, state `"healthy"`, payload `{supported, completed}`) and `persist_last_clone_unreachable` (`:137-155`, **state `"healthy"`**, payload `{"reachability":"unreachable","detail":msg}`).

### `crates/daku-core/src/health.rs`

```rust
// :19-33 (013 rewrites the match; the vote loop stays)
pub fn health_rollup(reachability: Reachability, signals: &[(&str, &str)]) -> EnvironmentHealth {
    … for &(signal_id, state) in signals { if signal_id == LAST_CLONE_SIGNAL_ID || state == "skipped" { continue; }
      if state == "down" || state == "degraded" { health = EnvironmentHealth::Degraded; } } …
}
// :46-116 publish_dashboard builds votes from SQLite rows: (snapshot.signal_id.as_str(), snapshot.state.as_str())
```

Tests: `health.rs:120-260` call `health_rollup(Reachability::X, &[("jobs", "degraded"), …])` with string literals (≈12 tests).

### `crates/daku-core/src/availability.rs:17-49`

```rust
pub enum Reachability { Reachable, Unreachable, Asleep }   + as_str() → "reachable"|"unreachable"|"asleep"
pub enum SignalState { Healthy, Degraded, Down }           + as_str() → "healthy"|"degraded"|"down"
```

Used by `classify_availability_response`, `observation()`, `AvailabilityObservation`, `drift.rs:11` (imports `classify_availability_response`), 013's `recent_reachability` (returns this local `Reachability`), and the availability/collector/health tests.

Test scaffolding: after plan 028, `crates/daku-core/src/test_support.rs` (or the module 028 names) provides `TempDb` and a `prod()` fixture; the per-Signal tests each define a tiny `HttpTransport` returning fixture bodies (`jobs.rs:178-198` `JobsCountTransport`, etc.).

Conventions: `anyhow` for collector errors, `io::Result` for persistence; `pub const *_SIGNAL_ID`; tests at the bottom of each file; imperative commit summaries; CONTEXT.md vocabulary (Signal, Environment).

## Commands you will need

| Purpose | Command | Expected on success |
|---------|---------|---------------------|
| Compile | `cargo check --workspace --all-targets` | exit 0 |
| Core tests | `cargo test -p daku-core` | all pass (count ≥ HEAD's) |
| Protocol tests | `cargo test -p daku-protocol` | all pass |
| Gate | `bun run check` | exit 0 |
| Manual smoke | `DAKU_UI_FIXTURE=1 bun run dev` | cards render as before |

## Scope

**In scope**:
- `crates/daku-protocol/src/protocol.rs` (add `SignalState`, `as_str`/`FromStr` helpers on `SignalState` and `Reachability`), `crates/daku-protocol/src/lib.rs` (re-export)
- `crates/daku-core/src/collector.rs`, `persistence.rs`, `availability.rs`, `jobs.rs`, `syslog.rs`, `mid_ecc.rs`, `outbound.rs`, `drift.rs`, `last_clone.rs`, `health.rs`, `lib.rs` (re-exports)
- `plans/README.md` (status row)

**Out of scope**:
- `SignalSnapshotDto.state` type and anything in `src/` (GPUI client) — wire strings are unchanged.
- Concurrency across Environments (plan 022), drift inventory throttling / MID aggregate (plan 023), Retry-After (012).
- Changing any Signal's query paths, thresholds, payload keys, or sample semantics.
- `hollow_backend.rs`, `server.rs`.

## Git workflow

- Trunk-based on `main`; commit directly; do NOT push unless asked.
- Suggested commits: (1) `Add typed SignalState and shared snapshot helpers.` (2) `Route the five per-Environment Signals through one collector loop.` (3) `Type drift/last-clone states; last-clone unreachable is down.`

## Steps

### Step 1: `SignalState` in the protocol crate

In `crates/daku-protocol/src/protocol.rs`, next to `EnvironmentHealth`:

```rust
/// Per-Signal snapshot state. `Skipped` means the Signal deliberately did not
/// probe this tick (asleep/unreachable Environment, or not applicable); it
/// never votes in the Environment health rollup.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Deserialize, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum SignalState { Healthy, Degraded, Down, Skipped }

impl SignalState {
    pub fn as_str(self) -> &'static str {
        match self { Self::Healthy => "healthy", Self::Degraded => "degraded", Self::Down => "down", Self::Skipped => "skipped" }
    }
    pub fn parse(text: &str) -> Option<Self> {
        Some(match text { "healthy" => Self::Healthy, "degraded" => Self::Degraded, "down" => Self::Down, "skipped" => Self::Skipped, _ => return None })
    }
}

impl Reachability {
    pub fn as_str(self) -> &'static str {
        match self { Self::Reachable => "reachable", Self::Unreachable => "unreachable", Self::Asleep => "asleep" }
    }
    pub fn parse(text: &str) -> Option<Self> {
        Some(match text { "reachable" => Self::Reachable, "unreachable" => Self::Unreachable, "asleep" => Self::Asleep, _ => return None })
    }
}
```

Add `SignalState` to the `pub use protocol::{…}` list in `crates/daku-protocol/src/lib.rs` and to the re-export list in `crates/daku-core/src/lib.rs`. Test: `signal_state_round_trips_strings` — for each variant `SignalState::parse(v.as_str()) == Some(v)` and `serde_json::to_string(&v)` equals `"\"<as_str>\""`.

**Verify**: `cargo test -p daku-protocol signal_state` → 1 passed.

### Step 2: Typed persistence helpers

In `crates/daku-core/src/persistence.rs`:

- Change `persist_signal_snapshot(..., state: &str, ...)` to `state: SignalState` and write `state.as_str()`.
- Add:

```rust
/// Standard "probe failed" snapshot every Signal writes.
pub fn persist_signal_down(connection: &Connection, environment_id: &str, signal_id: &str, observed_at: i64, message: &str) -> io::Result<()> {
    let payload = serde_json::json!({ "reachability": "unreachable", "detail": message });
    persist_signal_snapshot(connection, environment_id, signal_id, observed_at, SignalState::Down, &payload.to_string())
}
```

- 013's `persist_signal_skipped` → use `SignalState::Skipped`; delete `SKIPPED_STATE` (replace its uses with `SignalState::Skipped`).
- Fix every caller to compile (`cargo check -p daku-core` will list them: availability, jobs, syslog, mid_ecc, outbound, drift, last_clone, health tests, collector tests). At this step just replace `"healthy"` → `SignalState::Healthy` etc.; the `*_state()` fns (`jobs_state`, `syslog_state`, `outbound_state`, `mid_ecc_state`, `drift_state`) now return `SignalState`.
- Delete `availability::{Reachability, SignalState}` (`availability.rs:17-49`) and `use daku_protocol::{Reachability, SignalState}` instead; `AvailabilityObservation` keeps its fields with the protocol types; `persist_availability_snapshot` writes `observation.reachability.as_str()`. 013's `recent_reachability` returns the protocol `Reachability` (use `Reachability::parse`).
- `health.rs`: `health_rollup(reachability: Reachability, signals: &[(&str, SignalState)])`; the loop becomes `if signal_id == LAST_CLONE_SIGNAL_ID || state == SignalState::Skipped { continue; } if matches!(state, SignalState::Down | SignalState::Degraded) { … }`. `publish_dashboard` builds votes with `SignalState::parse(&snapshot.state).unwrap_or(SignalState::Skipped)` (unknown text never votes). Update the ~12 rollup tests to enum literals — same expectations. `wire_reachability` (`:35-44`) → `Reachability::parse(..).unwrap_or(Reachability::Reachable)`.

**Verify**: `cargo test -p daku-core` → all pass, same test names as before (nothing deleted yet).

### Step 3: The shared loop

In `crates/daku-core/src/collector.rs` add:

```rust
use rusqlite::Connection;
use daku_protocol::{Reachability, SignalState};
use crate::availability::{AVAILABILITY_SIGNAL_ID, REACHABILITY_REUSE_SECS, recent_reachability};

/// What one probe of one Environment produced. `sample` is appended to the
/// 24 h ring for trend Signals (jobs, syslog).
pub struct Observation {
    pub state: SignalState,
    pub payload: serde_json::Value,
    pub sample: Option<f64>,
}

/// One Signal's per-Environment logic. The loop, `observed_at`, the asleep /
/// unreachable gate, the down snapshot, and sample pruning live in
/// `PerEnvironmentCollector`; implementations only probe.
pub trait Signal: Send + Sync {
    fn id(&self) -> &'static str;
    /// Probe one Environment. `Err` is persisted as a `down` snapshot with the
    /// error text as `detail`; return `Ok(Observation)` for every classified outcome.
    fn probe(&self, client: &ServiceNowClient, credentials: &dyn CredentialStore, environment: &EnvironmentConfig) -> anyhow::Result<Observation>;
    /// Whether to skip probing when Availability reported asleep/unreachable
    /// this tick. Availability itself returns false.
    fn gated_by_availability(&self) -> bool { true }
    /// Whether this Signal writes samples (and therefore prunes them).
    fn keeps_samples(&self) -> bool { false }
}

pub struct PerEnvironmentCollector<S: Signal> {
    environments: Vec<EnvironmentConfig>,
    credentials: Arc<dyn CredentialStore>,
    client: Arc<ServiceNowClient>,
    store: StateStore,
    signal: S,
}

impl<S: Signal> PerEnvironmentCollector<S> {
    pub fn new(signal: S, environments: Vec<EnvironmentConfig>, credentials: Arc<dyn CredentialStore>, client: impl Into<Arc<ServiceNowClient>>, store: StateStore) -> Self { … }
    pub fn signal(&self) -> &S { &self.signal }
}

impl<S: Signal + 'static> SignalCollector for PerEnvironmentCollector<S> {
    fn collect(&self) -> anyhow::Result<()> {
        let connection = self.store.open()?;
        let observed_at = unix_now();               // extract the SystemTime→i64 expression into a pub(crate) fn
        let mut first_error = None;
        for environment in &self.environments {
            if let Err(error) = self.collect_environment(&connection, environment, observed_at) {
                first_error.get_or_insert(error);
            }
        }
        if self.signal.keeps_samples() {
            if let Err(error) = persistence::prune_signal_samples(&connection, observed_at) {
                first_error.get_or_insert_with(|| anyhow::Error::from(error));
            }
        }
        match first_error { Some(error) => Err(error), None => Ok(()) }
    }
}
```

with `collect_environment` doing, in order: (a) if `gated_by_availability()` and `recent_reachability(connection, &environment.id, observed_at, REACHABILITY_REUSE_SECS)` is `Some(r @ (Asleep | Unreachable))` → `persist_signal_skipped(.., self.signal.id(), observed_at, r.as_str())` and return; (b) `match self.signal.probe(..)`: `Ok(obs)` → `persist_signal_snapshot(.., obs.state, &obs.payload.to_string())` then, if `obs.sample.is_some()`, `persist_signal_sample(.., obs.sample, None)`; `Err(e)` → `persist_signal_down(.., self.signal.id(), observed_at, &e.to_string())`. All persistence errors map through `anyhow::Error::from`.

Add tests in `collector.rs` (use 028's `TempDb`/`prod()`): `per_environment_collector_persists_ok_observation`, `per_environment_collector_persists_down_on_probe_error`, `per_environment_collector_skips_when_asleep` (persist an asleep availability snapshot first; a probe that panics), `per_environment_collector_prunes_only_when_keeps_samples`. Use a tiny `struct FakeSignal { result: … }` implementing `Signal`.

**Verify**: `cargo test -p daku-core per_environment_collector` → 4 passed.

### Step 4: Convert the five Signals

For each of jobs, syslog, outbound, mid_ecc, availability:

- Replace the 4-field struct + `SignalCollector` impl with a unit/small struct implementing `Signal` (e.g. `pub struct JobsSignal;`), moving the fetch + classification into `probe` and returning `Observation { state: jobs_state(overdue), payload: json!({"overdue_ready": .., "error": ..}), sample: Some((overdue+error) as f64) }`. Preserve the partial-failure message logic (`jobs.rs:104-112`, `mid_ecc.rs:141-152`) by returning `Err(anyhow!(message))` when any fetch failed — the loop writes the identical down payload.
- `keeps_samples()` → `true` for jobs and syslog only.
- Availability: `AvailabilitySignal` with `gated_by_availability() = false`, `probe` = today's `probe()` body returning `Ok(Observation { state: obs.state, payload: json!({reachability, rtt_ms, build, error}), sample: None })` (never `Err`). Keep `AvailabilityCollector::probe(&self, env)` available for plan 041's `doctor` by exposing `AvailabilitySignal::observe(&self, client, credentials, env) -> AvailabilityObservation` and having `probe` call it.
- Keep the public constructor names so `build_default_loop` and every existing test compile with a one-line change: `pub type JobsCollector = PerEnvironmentCollector<JobsSignal>;` and `impl JobsCollector { pub fn new(environments, credentials, client, store) -> Self { PerEnvironmentCollector::new(JobsSignal, environments, credentials, client, store) } }` (same for the other four; note an inherent `impl` on a type alias of a generic struct with a concrete parameter is allowed).
- Delete the per-file `persist_*_down` fns and 013's four gate blocks (now in the loop). Delete each file's now-unused imports (`SystemTime`, `UNIX_EPOCH`, `SignalCollector`, `Connection` where unused).
- Existing per-Signal tests must pass **unchanged in assertions** (they construct `XCollector::new(...)` and call `.collect()`); only imports/type paths may change. 013's `<signal>_signal_skips_when_availability_asleep` tests can stay (they now exercise the shared gate) or be reduced to one — keep them, they are cheap.

**Verify**: `cargo test -p daku-core` → all pass; `grep -n 'fn persist_.*_down' crates/daku-core/src/{jobs,syslog,mid_ecc,outbound}.rs` → 0 matches; `grep -c 'recent_reachability' crates/daku-core/src/{jobs,syslog,mid_ecc,outbound}.rs` → 0 each (only `collector.rs` and tests reference it).

### Step 5: Drift and last-clone use the shared helpers; last-clone unreachable is `down`

- `drift.rs`: `persist_drift_down` → `persistence::persist_signal_down(.., DRIFT_SIGNAL_ID, ..)`; `persist_drift_skipped` → `persistence::persist_signal_skipped(.., DRIFT_SIGNAL_ID, observed_at, "need_two_environments")` (state becomes `Skipped` — `health_rollup` already ignores it; the client summary for a `skipped` payload is `""` per 013 → card shows "skipped" instead of "healthy"; update `drift_signal_single_environment_skips` / `drift_signal_without_clone_source_skips` expectations from `"healthy"` to `"skipped"`); `drift_state` returns `SignalState`; keep the collector shape.
- `last_clone.rs`: `persist_last_clone_unreachable` → `persistence::persist_signal_down(.., LAST_CLONE_SIGNAL_ID, ..)`. Update the two tests that assert `"healthy"` for the unreachable case (`last_clone_signal_probe_failure_is_healthy_unreachable` → rename `…_is_down_unreachable`, expect `"down"`); the 403 "unsupported" case stays `Healthy` with `{supported:false}`. Add a `health_rollup` test `health_rollup_last_clone_down_does_not_vote` (already covered by `health_rollup_last_clone_never_votes_degraded` — extend it with a `Down` vote).
- Use `collector::unix_now()` in both instead of the inline `SystemTime` expression.

**Verify**: `cargo test -p daku-core drift_signal` → 9 passed;
`cargo test -p daku-core last_clone_signal` → 8 passed;
`cargo test -p daku-core health_rollup` → 11 passed.

### Step 6: Gate + proof

**Verify**: `bun run check` → exit 0; then the greps in Done criteria.

## Test plan

- New: `collector.rs` `per_environment_collector_*` ×4 (Step 3); `protocol.rs` `signal_state_round_trips_strings`.
- Adjusted expectations only where behaviour intentionally changed: drift skipped → `skipped`; last-clone unreachable → `down`. Everything else keeps its assertions.
- `cargo test --workspace --no-fail-fast` → 0 failed; daku-core test count ≥ HEAD + 5.

## Done criteria

- [ ] `grep -rn 'pub enum SignalState' crates` → exactly 1 match, in `crates/daku-protocol/src/protocol.rs`; `grep -n 'pub enum Reachability\|pub enum SignalState' crates/daku-core/src/availability.rs` → 0
- [ ] `grep -rn '"healthy"\|"degraded"\|"down"\|"skipped"' crates/daku-core/src --include='*.rs' | grep -v 'mod tests' | grep -v '=> "' | grep -v 'assert' | grep -v 'fixtures'` → 0 non-test matches outside `SignalState::as_str/parse` (inspect the remaining lines; the only literals allowed are inside `protocol.rs`)
- [ ] `grep -c 'impl SignalCollector for' crates/daku-core/src/*.rs` → `collector.rs` (1, generic), `drift.rs` (1), `last_clone.rs` (1); all others 0
- [ ] `grep -n 'fn persist_signal_down\|fn persist_signal_skipped' crates/daku-core/src/persistence.rs` → 2 matches; `grep -rn 'reachability": "unreachable' crates/daku-core/src` → 1 match (persistence.rs) plus availability's observation payload
- [ ] `grep -n 'prune_signal_samples' crates/daku-core/src/{jobs,syslog}.rs` → 0 (only `collector.rs`)
- [ ] `bun run check` exits 0
- [ ] `git status` shows only in-scope files modified
- [ ] `plans/README.md` status row for 031 updated

## STOP conditions

- Plans 013 or 028 are not DONE (no `recent_reachability` in `availability.rs`, no `TempDb` helper) — land them first.
- The five per-Environment collectors no longer share the loop skeleton excerpted above (someone already consolidated or restructured) — report.
- Converting a Signal requires changing a query path, threshold, or payload key to fit the trait — that is a design gap; STOP and report which one.
- Any existing test's assertion would need to change other than the two intentional changes in Step 5.

## Maintenance notes

- New per-Environment Signals: implement `Signal` (≈30 lines) and register `PerEnvironmentCollector::new(MySignal, …)` in `build_default_loop`; no loop, gate, down payload or pruning code to write.
- Plan 022 (concurrency) should parallelise inside `PerEnvironmentCollector::collect` (per-Environment threads around `collect_environment`) — one place.
- Reviewers: check that `Observation` payload keys are byte-identical to the previous `persist_*_ok` payloads (the GPUI `summarize_payload` reads them by name), and that `keeps_samples()` is `true` for exactly jobs and syslog.
- Deferred: making `SignalSnapshotDto.state` a `SignalState` on the wire (needs `src/dashboard_state.rs` `WAITING` handling); drift/last-clone onto the trait via a "needs all Environments" hook if a third cross-Environment Signal ever appears.
