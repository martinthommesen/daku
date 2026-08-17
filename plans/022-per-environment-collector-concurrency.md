# Plan 022: Poll Environments concurrently so one slow Environment cannot stall the others

> **Executor instructions**: Follow this plan step by step. Run every
> verification command and confirm the expected result before moving to the
> next step. If anything in the "STOP conditions" section occurs, stop and
> report — do not improvise. When done, update the status row for this plan
> in `plans/README.md` — unless a reviewer dispatched you and told you they
> maintain the index.
>
> **Drift check (run first)**: `git diff --stat f7fdbe7..HEAD -- crates/daku-core/src/collector.rs crates/daku-core/src/persistence.rs`
> If either file changed since this plan was written, compare the "Current
> state" excerpts against the live code before proceeding; on a mismatch,
> treat it as a STOP condition. (Plans 013 and 014 touch `collector.rs`
> — those diffs are expected; re-read `tick`/`run`/`build_default_loop`.)

## Status

- **Priority**: P2
- **Effort**: M
- **Risk**: MED
- **Depends on**: plans/011-green-baseline-check-gate.md (gate), plans/013-asleep-never-degrades.md (availability-first ordering assumption is preserved per Environment), plans/014-replay-dashboard-on-subscribe.md (touches `run`; land first to avoid a merge)
- **Category**: perf / bug
- **Planned at**: commit `f7fdbe7`, 2026-08-17
- **Issue**: https://github.com/martinthommesen/daku/issues/50

## Why this matters

`CollectorLoop::tick` runs the seven Signal collectors one after another, and each collector loops over every Environment issuing blocking HTTP calls (30 s timeout each, `crates/daku-core/src/servicenow.rs` `UreqTransport`). Per Environment per tick that is ~10 requests. A black-holed or very slow Environment (VPN down for one instance, a hibernating PDI that never answers) therefore adds up to ~5 minutes to **every** tick, during which no other Environment is refreshed and nothing is published — the "~120 s cadence" (spec §5) silently becomes minutes, and `observed_at` values misreport.

The lazy fix that changes no collector code: build one collector set **per Environment** and run those sets on scoped threads inside `tick`. The two cross-Environment Signals (drift, last-clone) run afterwards, sequentially, exactly as today. Collectors keep their `for environment in &self.environments` loops — each just gets a one-element list.

## Why not the "trait method per Environment" alternative

Adding `SignalCollector::collect_environment(&self, env)` and moving the loop into `collector.rs` is the shape the seven-collector consolidation (backlog DEBT-06, plan 031) wants. That refactor is larger, touches all seven files and their 60+ tests, and would make this perf fix wait on it. Per-Environment groups get the concurrency win now with `collector.rs`-only changes and are compatible with 031: when 031 lands, its shared loop simply keeps the group structure. So: **022 does not depend on 031**; 031 should preserve `register_group`.

## Current state

### `crates/daku-core/src/collector.rs` (at HEAD; plans 013/014 do not change these signatures)

```rust
// :39-46
pub trait SignalCollector: Send + Sync {
    fn collect(&self) -> anyhow::Result<()>;
}

pub struct CollectorLoop {
    interval: Duration,
    collectors: Vec<Box<dyn SignalCollector>>,
}

// :48-71
impl CollectorLoop {
    pub fn new(interval: Duration) -> Self { Self { interval, collectors: Vec::new() } }

    pub fn register(&mut self, collector: impl SignalCollector + 'static) {
        self.collectors.push(Box::new(collector));
    }

    pub fn tick(&self) -> anyhow::Result<()> {
        let mut first_error = None;
        for collector in &self.collectors {
            if let Err(error) = collector.collect() {
                first_error.get_or_insert(error);
            }
        }
        match first_error {
            Some(error) => Err(error),
            None => Ok(()),
        }
    }

// :73-84  run(): tick, after(), sleep(interval)  (plan 014 adds one after() before the loop)
```

```rust
// :100-152  build_default_loop registers, in order, with `environments.clone()` each:
//   AvailabilityCollector, JobsCollector, SyslogCollector, MidEccCollector,
//   OutboundCollector, DriftCollector (also takes `interval`), LastCloneCollector.
pub fn build_default_loop(
    environments: Vec<EnvironmentConfig>,
    credentials: Arc<dyn CredentialStore>,
    store: StateStore,
    interval: Duration,
    client: ServiceNowClient,
) -> CollectorLoop {
    let client = Arc::new(client);
    let mut loop_ = CollectorLoop::new(interval);
    loop_.register(AvailabilityCollector::new(environments.clone(), credentials.clone(), client.clone(), store.clone()));
    …
    loop_.register(DriftCollector::new(environments.clone(), credentials.clone(), client.clone(), store.clone(), interval));
    loop_.register(LastCloneCollector::new(environments, credentials, client, store));
    loop_
}
```

Callers of `build_default_loop`: `start_default_loop` (`:180-187`) and `probe_availability_once` (`:206-214`). Tests: `collector_loop_tick_writes_availability_snapshot` (`:253-295`, uses `register` + `tick` twice) and `collector_loop_run_invokes_after_tick` (`:297-315`).

Every collector holds `Arc<dyn CredentialStore>`, `Arc<ServiceNowClient>`, `StateStore` (a `Clone` path wrapper; `StateStore::open()` opens a **new** SQLite connection per call — `persistence.rs:115-126`) and its own `Vec<EnvironmentConfig>`.

Thread-safety facts (verified at HEAD): `ServiceNowClient` is `Send + Sync` (`transport: Box<dyn HttpTransport>` and `clock: Box<dyn Clock>` are both `Send + Sync` traits, token cache is `std::sync::Mutex`, `servicenow.rs:43-46, 76-80`); `CredentialStore: Send + Sync` (`config.rs:52-54`); `KeychainCredentialStore` is a unit struct calling `security_framework::passwords::get_generic_password` (thread-safe); `SignalCollector: Send + Sync`.

### `crates/daku-core/src/persistence.rs`

```rust
// :115-126
    pub fn open(&self) -> io::Result<Connection> {
        ensure_daku_dir(&self.path)?;
        let connection = Connection::open(&self.path).map_err(to_io_error)?;
        connection
            .execute_batch("PRAGMA journal_mode = WAL; PRAGMA synchronous = NORMAL;")
            .map_err(to_io_error)?;
        apply_migrations(&connection)?;
        // WAL may recreate sidecar modes; re-assert the main db file mode.
        #[cfg(unix)]
        fs::set_permissions(&self.path, fs::Permissions::from_mode(0o600))?;
        Ok(connection)
    }
```

No `busy_timeout` is set. With concurrent writers (one per Environment thread) SQLite returns `SQLITE_BUSY` immediately instead of waiting; rusqlite exposes `Connection::busy_timeout(Duration)`.

Conventions: `anyhow::Result` in collectors, `io::Result` in persistence; tests at file bottom in `mod tests` with temp DBs (`std::env::temp_dir().join(format!("daku-…-{}.db", uuid::Uuid::new_v4()))`).

## Commands you will need

| Purpose | Command | Expected on success |
|---------|---------|---------------------|
| Collector tests | `cargo test -p daku-core collector_loop` | all pass (incl. new) |
| All core tests | `cargo test -p daku-core` | all pass (65+ existing) |
| Check | `cargo check --workspace --all-targets` | exit 0 |
| Gate | `bun run check` | exit 0 |
| Manual (optional, needs Operator config) | `cargo run -p daku-daemon -- probe-availability` | completes; stderr shows no `tick took` warning unless an Environment is slow |

## Scope

**In scope**:
- `crates/daku-core/src/collector.rs` — `CollectorLoop` groups, parallel `tick`, tick-duration warning, `build_default_loop` per-Environment groups, tests
- `crates/daku-core/src/persistence.rs` — `busy_timeout` in `StateStore::open`
- `plans/README.md` (status row)

**Out of scope**:
- Any signal file (`availability.rs`, `jobs.rs`, `syslog.rs`, `mid_ecc.rs`, `outbound.rs`, `drift.rs`, `last_clone.rs`) — their loops stay; do not touch.
- `servicenow.rs` timeouts / retries (plan 012 owns caps).
- Publishing per Environment as it completes (would need `publish_dashboard` changes) — deferred.
- The consolidation refactor (plan 031).

## Git workflow

- Commit on `main`; do not push unless asked. Suggested summary: `Poll Environments on parallel collector groups; add SQLite busy timeout.`

## Steps

### Step 1: SQLite busy timeout

In `crates/daku-core/src/persistence.rs` `StateStore::open`, after the `PRAGMA` batch and before `apply_migrations`, add:

```rust
        connection
            .busy_timeout(Duration::from_secs(5))
            .map_err(to_io_error)?;
```

Add `use std::time::Duration;` (the file already imports `std::time::{SystemTime, UNIX_EPOCH}` — extend it).

**Verify**: `cargo test -p daku-core persistence` → all pass.

### Step 2: Groups in `CollectorLoop`

In `crates/daku-core/src/collector.rs` replace the struct and `impl` (keeping `run` as it is after plan 014) with:

```rust
pub struct CollectorLoop {
    interval: Duration,
    /// Run concurrently, one scoped thread per group (one group per Environment).
    groups: Vec<Vec<Box<dyn SignalCollector>>>,
    /// Run sequentially after every group has finished (cross-Environment Signals).
    shared: Vec<Box<dyn SignalCollector>>,
}

impl CollectorLoop {
    pub fn new(interval: Duration) -> Self {
        Self { interval, groups: Vec::new(), shared: Vec::new() }
    }

    /// Registers a collector that runs after all groups, on the calling thread.
    pub fn register(&mut self, collector: impl SignalCollector + 'static) {
        self.shared.push(Box::new(collector));
    }

    /// Registers a set of collectors that run in order on their own thread,
    /// concurrently with the other groups.
    pub fn register_group(&mut self, group: Vec<Box<dyn SignalCollector>>) {
        if !group.is_empty() {
            self.groups.push(group);
        }
    }

    pub fn tick(&self) -> anyhow::Result<()> {
        let started = Instant::now();
        let mut errors: Vec<anyhow::Error> = std::thread::scope(|scope| {
            let handles: Vec<_> = self
                .groups
                .iter()
                .map(|group| scope.spawn(move || run_sequential(group)))
                .collect();
            handles
                .into_iter()
                .filter_map(|handle| match handle.join() {
                    Ok(result) => result.err(),
                    Err(_) => Some(anyhow::anyhow!("collector group panicked")),
                })
                .collect()
        });
        if let Err(error) = run_sequential(&self.shared) {
            errors.push(error);
        }
        let elapsed = started.elapsed();
        if elapsed > self.interval {
            eprintln!(
                "daku collector tick took {:.0}s (poll interval {:.0}s)",
                elapsed.as_secs_f64(),
                self.interval.as_secs_f64()
            );
        }
        match errors.into_iter().next() {
            Some(error) => Err(error),
            None => Ok(()),
        }
    }
    // run(...) unchanged
}

fn run_sequential(collectors: &[Box<dyn SignalCollector>]) -> anyhow::Result<()> {
    let mut first_error = None;
    for collector in collectors {
        if let Err(error) = collector.collect() {
            first_error.get_or_insert(error);
        }
    }
    match first_error {
        Some(error) => Err(error),
        None => Ok(()),
    }
}
```

Add `Instant` to the `std::time` import. `first_error` semantics are preserved: every collector still runs even if an earlier one failed, and the first error (group order, then shared) is returned.

**Verify**: `cargo test -p daku-core collector_loop` → the two existing tests still pass (`register` still works; a loop with only `shared` collectors behaves exactly as before).

### Step 3: One group per Environment in `build_default_loop`

Replace the body so per-Environment Signals get a one-element `environments` list each, and the cross-Environment Signals stay shared:

```rust
pub fn build_default_loop(
    environments: Vec<EnvironmentConfig>,
    credentials: Arc<dyn CredentialStore>,
    store: StateStore,
    interval: Duration,
    client: ServiceNowClient,
) -> CollectorLoop {
    let client = Arc::new(client);
    let mut loop_ = CollectorLoop::new(interval);
    for environment in &environments {
        let one = vec![environment.clone()];
        loop_.register_group(vec![
            Box::new(AvailabilityCollector::new(one.clone(), credentials.clone(), client.clone(), store.clone())),
            Box::new(JobsCollector::new(one.clone(), credentials.clone(), client.clone(), store.clone())),
            Box::new(SyslogCollector::new(one.clone(), credentials.clone(), client.clone(), store.clone())),
            Box::new(MidEccCollector::new(one.clone(), credentials.clone(), client.clone(), store.clone())),
            Box::new(OutboundCollector::new(one, credentials.clone(), client.clone(), store.clone())),
        ]);
    }
    loop_.register(DriftCollector::new(environments.clone(), credentials.clone(), client.clone(), store.clone(), interval));
    loop_.register(LastCloneCollector::new(environments, credentials, client, store));
    loop_
}
```

Availability stays first **within each Environment's group**, so plan 013's same-tick `recent_reachability` gate keeps working. Drift runs after all groups, so its `reuse_availability_build` sees this tick's builds. `prune_signal_samples` (called inside jobs/syslog collectors) now runs once per Environment group — it is idempotent (`DELETE … WHERE observed_at < cutoff`), harmless.

**Verify**: `cargo check --workspace --all-targets` → exit 0. `cargo test -p daku-core` → all pass.

### Step 4: Tests

In `collector.rs` `mod tests` add:

```rust
    struct SleepingCollector(Duration, Arc<std::sync::atomic::AtomicUsize>);
    impl SignalCollector for SleepingCollector {
        fn collect(&self) -> anyhow::Result<()> {
            std::thread::sleep(self.0);
            self.1.fetch_add(1, Ordering::AcqRel);
            Ok(())
        }
    }

    #[test]
    fn collector_loop_tick_runs_groups_concurrently() {
        let calls = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let mut loop_ = CollectorLoop::new(Duration::from_secs(120));
        for _ in 0..3 {
            loop_.register_group(vec![Box::new(SleepingCollector(Duration::from_millis(200), calls.clone()))]);
        }
        let started = std::time::Instant::now();
        loop_.tick().unwrap();
        assert_eq!(calls.load(Ordering::Acquire), 3);
        assert!(started.elapsed() < Duration::from_millis(500), "groups must not run serially");
    }

    struct FailingCollector;
    impl SignalCollector for FailingCollector {
        fn collect(&self) -> anyhow::Result<()> { anyhow::bail!("boom") }
    }

    #[test]
    fn collector_loop_tick_isolates_failures_and_returns_first_error() {
        let calls = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let mut loop_ = CollectorLoop::new(Duration::from_secs(120));
        loop_.register_group(vec![Box::new(FailingCollector), Box::new(SleepingCollector(Duration::ZERO, calls.clone()))]);
        loop_.register(SleepingCollector(Duration::ZERO, calls.clone()));
        let error = loop_.tick().unwrap_err();
        assert!(error.to_string().contains("boom"));
        assert_eq!(calls.load(Ordering::Acquire), 2, "later collectors still run");
    }
```

Add a build-shape test:

```rust
    #[test]
    fn build_default_loop_groups_per_environment() {
        let credentials: Arc<dyn CredentialStore> = Arc::new(MemoryCredentialStore::default());
        let path = std::env::temp_dir().join(format!("daku-groups-{}.db", uuid::Uuid::new_v4()));
        let loop_ = build_default_loop(
            vec![prod_env("prod"), prod_env("test")],   // write a tiny helper or reuse the EnvironmentConfig literal from collector_loop_tick_writes_availability_snapshot
            credentials,
            StateStore::daemon(path.clone()),
            Duration::from_secs(120),
            ServiceNowClient::new(FixtureTransport, SystemClock),
        );
        assert_eq!(loop_.groups.len(), 2);
        assert_eq!(loop_.groups[0].len(), 5);
        assert_eq!(loop_.shared.len(), 2);
        let _ = std::fs::remove_file(path);
    }
```

(`groups`/`shared` are private fields; the test module is inside the file so it can read them.)

**Verify**: `cargo test -p daku-core collector_loop` and `cargo test -p daku-core build_default_loop` → all pass.

### Step 5: Gate

**Verify**: `bun run check` → exit 0.

## Test plan

- New in `collector.rs`: `collector_loop_tick_runs_groups_concurrently`, `collector_loop_tick_isolates_failures_and_returns_first_error`, `build_default_loop_groups_per_environment`.
- Existing: all `*_signal_*` tests unchanged (collectors untouched); `collector_loop_tick_writes_availability_snapshot` and `collector_loop_run_*` still pass.
- `cargo test --workspace --no-fail-fast` → 0 failed.

## Done criteria

- [ ] `grep -n 'busy_timeout' crates/daku-core/src/persistence.rs` → 1 match inside `open`
- [ ] `grep -n 'thread::scope\|register_group\|fn run_sequential' crates/daku-core/src/collector.rs` → all present
- [ ] `grep -n 'tick took' crates/daku-core/src/collector.rs` → 1 match
- [ ] `cargo test -p daku-core` passes with the 3 new tests; no signal file modified (`git status`)
- [ ] `bun run check` exits 0
- [ ] `plans/README.md` status row for 022 updated

## STOP conditions

- `CollectorLoop`/`build_default_loop` no longer match the excerpts (e.g. plan 031 already replaced the seven collectors with one shared loop) — then the concurrency belongs in that loop; report instead of layering groups on top.
- Any `*_signal_*` test fails after Step 3 (would mean a collector depends on seeing all Environments — e.g. drift/last_clone were accidentally put in a group).
- `busy_timeout` does not exist on `rusqlite::Connection` in the locked version (0.37 has it) — report.
- The concurrency test is flaky on the machine (timing) — loosen to `< 1 s`, never remove the assertion.

## Maintenance notes

- Publishing still happens once per tick after all groups join; a slow Environment now delays only *publish*, not other Environments' probing. If that matters, publish per group (needs `publish_dashboard` to accept an Environment subset).
- N Environments = N threads per tick; fine for the single-digit N daku targets. If N grows large, cap with a semaphore.
- Plan 031 (consolidation) must keep the group/shared split and the "availability first within a group" order.
- Reviewers: check that no collector file changed and that `first_error` ordering (groups, then shared) is what tests assert.
