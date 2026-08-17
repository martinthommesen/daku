# Plan 028: One temp-DB test helper for daku-core (with sidecar cleanup) and a collector-isolation test

> **Executor instructions**: Follow this plan step by step. Run every
> verification command and confirm the expected result before moving to the
> next step. If anything in the "STOP conditions" section occurs, stop and
> report — do not improvise. When done, update the status row for this plan
> in `plans/README.md` — unless a reviewer dispatched you and told you they
> maintain the index.
>
> **Drift check (run first)**: `git diff --stat f7fdbe7..HEAD -- crates/daku-core/src/lib.rs crates/daku-core/src/collector.rs crates/daku-core/src/persistence.rs crates/daku-core/src/availability.rs crates/daku-core/src/jobs.rs crates/daku-core/src/syslog.rs crates/daku-core/src/mid_ecc.rs crates/daku-core/src/outbound.rs crates/daku-core/src/drift.rs crates/daku-core/src/last_clone.rs crates/daku-core/src/health.rs`
> Test-module drift from plans 013/022/023/031 is expected — re-grep the
> site list in Step 2 instead of trusting the line numbers here. If a
> consolidation (plan 031) already introduced a shared test module, STOP and
> report — merge into it rather than adding a second one.

## Status

- **Priority**: P3
- **Effort**: S
- **Risk**: LOW (test-only; behaviour-preserving migration)
- **Depends on**: plans/011-green-baseline-check-gate.md. **Ordering**: land before plan 031 (collector consolidation) and ideally before 013/022/023 so their new tests use the helper — but it is safe in any order.
- **Category**: tests / dx
- **Planned at**: commit `f7fdbe7`, 2026-08-17
- **Issue**: https://github.com/martinthommesen/daku/issues/45

## Why this matters

Nineteen daku-core tests build a temp SQLite path with the same six-line incantation (`std::env::temp_dir().join(format!("daku-…-{}.db", uuid::Uuid::new_v4()))`, `let _ = remove_file(&path)` before, `let _ = remove_file(path)` after). Because `StateStore::open` enables WAL (`persistence.rs:119`), every run leaves `<name>.db-wal` / `<name>.db-shm` sidecars in `$TMPDIR`, and a panicking test leaves the `.db` too. Four files also define an identical `prod()` `EnvironmentConfig` fixture. Every new collector test (plans 013, 022, 023) copies the pattern again.

Separately, `CollectorLoop::tick` continues past a failing collector and returns the first error (`collector.rs:60-71`) — the property that keeps one misbehaving Signal from blanking the dashboard — but only single-collector tests exist, so nothing pins it.

## Current state

### `crates/daku-core/src/lib.rs` (verified, 30 lines)

Module list `pub mod availability; … pub mod syslog; mod server;` and re-exports. No test-support module exists.

### `crates/daku-core/Cargo.toml:31-32`

```toml
[dev-dependencies]
uuid = { version = "1.18", features = ["v4", "serde"] }
```

### `crates/daku-core/src/persistence.rs`

```rust
// :107-126
    pub fn daemon(path: PathBuf) -> Self { Self { path } }
    pub fn path(&self) -> &Path { &self.path }
    pub fn open(&self) -> io::Result<Connection> {
        ensure_daku_dir(&self.path)?;
        let connection = Connection::open(&self.path).map_err(to_io_error)?;
        connection.execute_batch("PRAGMA journal_mode = WAL; PRAGMA synchronous = NORMAL;").map_err(to_io_error)?;
        …
// :288-291 (tests)
    fn temp_db_path(label: &str) -> PathBuf {
        std::env::temp_dir().join(format!("daku-{label}-{}.db", uuid::Uuid::new_v4()))
    }
```

`StateStore` is `#[derive(Clone)]` with a single `path: PathBuf` field.

### The copy-pasted sites (grep `temp_dir()` in `crates/daku-core/src`, verified at HEAD — 21 hits, 19 are DB paths)

| File | Lines | Label |
|---|---|---|
| `availability.rs` | 293 | `daku-avail-persist` |
| `collector.rs` | 254 | `daku-collector` |
| `drift.rs` | 507, 582, 616, 687, 739 | `daku-drift`, `-skip`, `-nosrc`, `-trunc`, `-reuse` |
| `health.rs` | 231 | `daku-health-publish` |
| `jobs.rs` | 216, 253, 296 | `daku-jobs`, `-overdue`, `-fail` |
| `last_clone.rs` | 246 | `daku-last-clone` |
| `mid_ecc.rs` | 279, 413 | `daku-mid-ecc`, `-fail` |
| `outbound.rs` | 197, 259 | `daku-outbound`, `-fail` |
| `persistence.rs` | 290 (`temp_db_path` helper used by 2 tests) | `daku-{label}` |
| `syslog.rs` | 198, 233 | `daku-syslog`, `-err` |

Not DB paths (leave alone): `persistence.rs:320` (`daku-home-…` directory for the permissions test), `settings.rs:110` (`.json`).

Each site follows: `let path = …; let _ = std::fs::remove_file(&path); let store = StateStore::daemon(path.clone()); … let connection = StateStore::daemon(path.clone()).open().unwrap(); … let _ = std::fs::remove_file(path);` (see `jobs.rs:214-250` for a full example). `let _ = std::fs::remove_file` appears 43 times across the crate.

Fixtures: `fn prod() -> EnvironmentConfig` identical in `jobs.rs:203`, `syslog.rs:185`, `mid_ecc.rs:263`, `outbound.rs:185` (id `prod`, label `Production`, `https://acme-prod.example.service-now.com`, `AuthMethod::Basic`, `sort_order 0`, `clone_source false`); `servicenow.rs:414/425` `basic_env()`/`oauth_env()`; `drift.rs:492`/`last_clone.rs:233` `env(id, host, clone_source)`; inline structs at `collector.rs:260`, `health.rs:293`.

### `crates/daku-core/src/collector.rs`

```rust
// :39-41
pub trait SignalCollector: Send + Sync { fn collect(&self) -> anyhow::Result<()>; }
// :60-71
    pub fn tick(&self) -> anyhow::Result<()> {
        let mut first_error = None;
        for collector in &self.collectors {
            if let Err(error) = collector.collect() { first_error.get_or_insert(error); }
        }
        match first_error { Some(error) => Err(error), None => Ok(()) }
    }
```

Tests (`:230-316`): `collector_loop_tick_writes_availability_snapshot`, `collector_loop_run_invokes_after_tick` (plan 014 renames/extends the latter). `use std::sync::atomic::{AtomicBool, Ordering}` and `Arc` are already imported at the top of the file.

Conventions: `#[cfg(test)] mod tests { use super::*; … }` per file; edition 2024; `uuid` only as a dev-dep. Ponytail: no `tempfile` crate — 20 lines of `Drop` cover it.

## Commands you will need

| Purpose | Command | Expected on success |
|---------|---------|---------------------|
| Core tests | `cargo test -p daku-core` | all pass |
| Sidecar check | `ls "${TMPDIR:-/tmp}" \| grep -c 'daku-.*\.db'` before/after a run | count does not grow after the migration |
| Site count | `grep -rn 'temp_dir()' crates/daku-core/src \| grep -v 'test_support\|daku-home\|settings.rs' \| wc -l` | `0` after Step 2 |
| Gate | `bun run check` | exit 0 |

## Scope

**In scope**:
- `crates/daku-core/src/test_support.rs` (create; `#[cfg(test)]`)
- `crates/daku-core/src/lib.rs` (one `#[cfg(test)] pub(crate) mod test_support;` line)
- Test modules only in: `availability.rs`, `collector.rs`, `drift.rs`, `health.rs`, `jobs.rs`, `last_clone.rs`, `mid_ecc.rs`, `outbound.rs`, `persistence.rs`, `syslog.rs`
- `plans/README.md` (status row)

**Out of scope**:
- Any non-test code. Any change to assertions or fixture *values*.
- `settings.rs` test (JSON temp file, not a DB), `persistence.rs:320` permissions test (needs a directory).
- `servicenow.rs` fixtures (`basic_env`/`oauth_env` have different ids/auth — leave them).
- Adding `tempfile` or any new dependency.

## Git workflow

- Commit on `main`; do not push unless asked. Suggested summary: `Add TempDb test helper for daku-core and a collector isolation test.`

## Steps

### Step 1: `test_support` module

Create `crates/daku-core/src/test_support.rs`:

```rust
//! Test-only helpers shared by daku-core unit tests.

use std::path::{Path, PathBuf};

use crate::config::{AuthMethod, EnvironmentConfig};
use crate::persistence::StateStore;

/// Unique SQLite path under the OS temp dir; removes the db and its WAL/SHM
/// sidecars on drop (also on panic).
pub struct TempDb {
    path: PathBuf,
}

impl TempDb {
    pub fn new(label: &str) -> Self {
        let path = std::env::temp_dir().join(format!("daku-{label}-{}.db", uuid::Uuid::new_v4()));
        let _ = std::fs::remove_file(&path);
        Self { path }
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    pub fn store(&self) -> StateStore {
        StateStore::daemon(self.path.clone())
    }
}

impl Drop for TempDb {
    fn drop(&mut self) {
        for suffix in ["", "-wal", "-shm"] {
            let mut sidecar = self.path.clone().into_os_string();
            sidecar.push(suffix);
            let _ = std::fs::remove_file(sidecar);
        }
    }
}

/// The Basic-auth `prod` Environment used across collector tests.
pub fn prod() -> EnvironmentConfig {
    EnvironmentConfig {
        id: "prod".into(),
        label: "Production".into(),
        instance_url: "https://acme-prod.example.service-now.com".into(),
        auth_method: AuthMethod::Basic,
        sort_order: 0,
        clone_source: false,
    }
}
```

In `crates/daku-core/src/lib.rs` add after `mod server;`:

```rust
#[cfg(test)]
pub(crate) mod test_support;
```

Add a self-test at the bottom of `test_support.rs`:

```rust
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn temp_db_removes_db_and_sidecars_on_drop() {
        let path;
        {
            let db = TempDb::new("self");
            path = db.path().to_path_buf();
            let _connection = db.store().open().unwrap(); // creates .db (+ -wal/-shm under WAL)
        }
        assert!(!path.exists());
        let mut wal = path.clone().into_os_string();
        wal.push("-wal");
        assert!(!Path::new(&wal).exists());
    }
}
```

**Verify**: `cargo test -p daku-core temp_db_removes` → 1 passed.

### Step 2: Migrate the 19 sites

For every row in the site table (re-grep first: `grep -rn 'temp_dir()' crates/daku-core/src`), replace the path setup with the helper — pattern (from `jobs.rs:214-250`):

```rust
// before
let path = std::env::temp_dir().join(format!("daku-jobs-{}.db", uuid::Uuid::new_v4()));
let _ = std::fs::remove_file(&path);
let store = StateStore::daemon(path.clone());
…
let connection = StateStore::daemon(path.clone()).open().unwrap();
…
let _ = std::fs::remove_file(path);

// after
let db = TempDb::new("jobs");
let store = db.store();
…
let connection = db.store().open().unwrap();
…
// (no trailing remove_file — Drop handles it; keep `db` alive to the end of the test)
```

Add `use crate::test_support::TempDb;` (and `prod` where Step 3 applies) inside each file's `mod tests`. In `persistence.rs` tests, replace the private `temp_db_path` helper with `TempDb` for its two DB tests; leave the `daku-home` permissions test alone. Delete the now-unused trailing `let _ = std::fs::remove_file(path);` lines (and any now-unused `use std::path::PathBuf` in test modules).

**Verify**: `grep -rn 'temp_dir()' crates/daku-core/src | grep -v 'test_support.rs\|daku-home\|settings.rs' | wc -l` → `0`. `cargo test -p daku-core` → all pass (same count as before + 1). Sidecar check: `before=$(ls "${TMPDIR:-/tmp}" | grep -c 'daku-.*\.db'); cargo test -p daku-core -q >/dev/null; after=$(ls "${TMPDIR:-/tmp}" | grep -c 'daku-.*\.db'); echo "$before $after"` → equal.

### Step 3: Deduplicate `prod()`

Delete the four identical `fn prod()` in `jobs.rs`, `syslog.rs`, `mid_ecc.rs`, `outbound.rs` test modules and import `crate::test_support::prod` instead. Leave `drift.rs`/`last_clone.rs` `env(...)`, `servicenow.rs` `basic_env/oauth_env`, and the inline structs in `collector.rs`/`health.rs` unless they are byte-identical to `prod()` (`collector.rs:260-267` is — replace it too).

**Verify**: `grep -rn 'fn prod()' crates/daku-core/src` → only `test_support.rs`. `cargo test -p daku-core` → all pass.

### Step 4: Collector isolation test

In `crates/daku-core/src/collector.rs` `mod tests` add:

```rust
    #[test]
    fn collector_loop_tick_isolates_failures() {
        struct Failing;
        impl SignalCollector for Failing {
            fn collect(&self) -> anyhow::Result<()> {
                anyhow::bail!("first collector failed")
            }
        }
        struct Recording(Arc<AtomicBool>);
        impl SignalCollector for Recording {
            fn collect(&self) -> anyhow::Result<()> {
                self.0.store(true, Ordering::Release);
                Ok(())
            }
        }
        let ran = Arc::new(AtomicBool::new(false));
        let mut loop_ = CollectorLoop::new(Duration::from_secs(1));
        loop_.register(Failing);
        loop_.register(Recording(ran.clone()));
        let error = loop_.tick().unwrap_err();
        assert!(error.to_string().contains("first collector failed"));
        assert!(ran.load(Ordering::Acquire), "later collectors must still run");
    }
```

(If plan 022 has already turned `tick` into a per-Environment fan-out, adapt the test to whatever `tick` still guarantees — "one failing collector does not prevent the others" — and keep the name.)

**Verify**: `cargo test -p daku-core collector_loop_tick_isolates_failures` → 1 passed.

### Step 5: Gate

**Verify**: `bun run check` → exit 0.

## Test plan

- New: `test_support::tests::temp_db_removes_db_and_sidecars_on_drop`, `collector::tests::collector_loop_tick_isolates_failures`.
- Migrated (no assertion changes): the 19 sites listed above.
- `cargo test --workspace --no-fail-fast` → 0 failed; test count for daku-core = previous + 2.

## Done criteria

- [ ] `crates/daku-core/src/test_support.rs` exists; `grep -n 'mod test_support' crates/daku-core/src/lib.rs` → 1 match under `#[cfg(test)]`
- [ ] `grep -rn 'temp_dir()' crates/daku-core/src | grep -v 'test_support.rs\|daku-home\|settings.rs' | wc -l` → `0`
- [ ] `grep -rn 'fn prod()' crates/daku-core/src` → only `test_support.rs`
- [ ] `cargo test -p daku-core` passes; temp-file count in `$TMPDIR` does not grow across a run
- [ ] `bun run check` exits 0
- [ ] `git status` shows only in-scope files modified
- [ ] `plans/README.md` status row for 028 updated

## STOP conditions

- A shared test-support module already exists in daku-core (plan 031 or someone else landed one) — merge into it, do not create a second; if the shapes conflict, report.
- Any migrated test changes outcome (a test relied on the `.db` surviving across two `StateStore` handles — `TempDb` keeps the file until drop, so this should not happen; if it does, report the test name).
- `cargo test -p daku-core` count decreases (a test was accidentally deleted).

## Maintenance notes

- New daku-core tests must use `TempDb::new("<label>")` and `test_support::prod()`; reviewers should reject fresh `temp_dir()` incantations in `crates/daku-core/src`.
- Plan 031 (consolidation) can add shared `HttpTransport` stubs (`StaticTransport`, `PanicTransport`) to this module — do not add them here.
- Deferred: the `servicenow.rs` fixtures and `drift`/`last_clone` `env()` helpers (different shapes; not worth forcing).
