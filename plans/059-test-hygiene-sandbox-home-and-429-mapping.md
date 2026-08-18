# Plan 059: Tests clean up after themselves, stop racing on the environment, and pin what a rate-limited Environment looks like

> **Executor instructions**: Follow this plan step by step. Run every
> verification command and confirm the expected result before moving to the
> next step. If anything in the "STOP conditions" section occurs, stop and
> report — do not improvise. When done, update the status row for this plan
> in `plans/README.md`.
>
> **Drift check (run first)**:
> `git diff --stat 2bdeaba..HEAD -- crates/daku-daemon/tests/process.rs crates/daku-core/src/test_support.rs crates/daku-core/src/config.rs crates/daku-core/src/collector.rs crates/daku-core/src/servicenow.rs crates/daku-client/src/persistence.rs`
> If any in-scope file changed since this plan was written, compare the
> "Current state" excerpts against the live code before proceeding; on a
> mismatch, treat it as a STOP condition.

## Status

- **Priority**: P3
- **Effort**: S–M
- **Risk**: LOW
- **Depends on**: none (but if `plans/051-local-daemon-reconnect-and-supervisor-test.md`
  is in flight, land 051 first — it adds tests to the same file)
- **Category**: tests
- **Planned at**: commit `2bdeaba`, 2026-08-18

## Why this matters

Three things, all in the test layer that `bun run check` runs on every commit.

1. **An `unsafe` environment mutation in a parallel test binary.**
   `crates/daku-daemon/tests/process.rs` calls `std::env::set_var("HOME", …)`
   while sibling tests in the same binary read the environment
   (`std::env::var_os("PATH")` on every `spawn_daemon`). libtest runs those on
   parallel threads — this is exactly the data race `set_var` was made `unsafe`
   for in edition 2024. It surfaces as an unreproducible failure on a loaded
   machine, and it makes the whole file order-dependent.
2. **Cleanup only on the success path.** Every `remove_dir_all` in that file
   sits *after* the assertions, and the supervisor test never removes its
   `OnceLock` home at all. One failing test litters `$TMPDIR` with daemon homes,
   SQLite files and JSON fixtures — permanently. `TempDb`'s `Drop` guard exists
   precisely to prevent this; the daemon test file predates it and other files
   have drifted back:
   `grep -rn 'temp_dir()' crates/daku-core/src | grep -v 'test_support.rs\|daku-home\|settings.rs' | wc -l`
   is plan 028's own done criterion, specified as `0`. **It currently returns 5.**
3. **Nothing pins what a sustained 429 does.** Both existing 429 tests script
   one 429 then a success, so the exhaustion branch is never taken. Rate
   limiting is the most likely failure an Operator polling several Environments
   will actually hit, and its end-to-end mapping — Environment reads `down` /
   `unreachable`, every gated Signal reads `skipped: unreachable` — is
   unverified.

## Current state

**`crates/daku-daemon/tests/process.rs:20-42`**:

```rust
/// Fresh empty `HOME` so the daemon never sees the operator's `~/.daku`.
fn sandbox_home() -> PathBuf {
    static COUNTER: AtomicU32 = AtomicU32::new(0);
    let home = std::env::temp_dir().join(format!(
        "daku-home-{}-{}",
        std::process::id(),
        COUNTER.fetch_add(1, Ordering::Relaxed)
    ));
    std::fs::create_dir_all(&home).unwrap();
    home
}

/// The supervisor spawns its child with the *inherited* environment, so `HOME`
/// has to be set for this whole test binary. Only the supervisor test relies on
/// it; every other test spawns with `env_clear()`.
fn ensure_process_home() -> PathBuf {
    static HOME: std::sync::OnceLock<PathBuf> = std::sync::OnceLock::new();
    HOME.get_or_init(|| {
        let home = sandbox_home();
        unsafe { std::env::set_var("HOME", &home) };
        home
    })
    .clone()
}
```

**`crates/daku-core/src/test_support.rs:9-38`** — the pattern to copy:

```rust
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
    ...
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
```

**The five regressed sites** (each a temp JSON path with a trailing
`let _ = fs::remove_file(...)` that a panic skips):

- `crates/daku-core/src/config.rs:158`, `:221`
- `crates/daku-core/src/collector.rs:902`, `:967`, `:970`

and the same hand-rolled shape in `crates/daku-client/src/persistence.rs:106-111`.

**`crates/daku-core/src/servicenow.rs:13-18`** — the retry budget:

```rust
const MAX_429_RETRIES: u8 = 2;
const DEFAULT_RETRY_AFTER: Duration = Duration::from_secs(1);
const MAX_RETRY_AFTER: Duration = Duration::from_secs(30);
```

**The end-to-end mapping to pin** — `crates/daku-core/src/availability.rs:26-59`
classifies a non-200 as `Unreachable` / `Down`, and
`crates/daku-core/src/collector.rs:125-148` then makes every gated Signal write
`skipped: unreachable`.

### Constraints you must honor

- **`docs/agents/git-workflow.md`**: no CI. `bun run check` is the only gate, so
  a flaky test here is worse than no test — it trains everyone to re-run the
  gate and stop reading it.
- `plans/README.md` records "Bun test harness for `scripts/*.ts`" as considered
  and rejected. Do not add one.
- `plans/028`'s convention, still in force: *"New daku-core tests must use
  `TempDb::new("<label>")` and `test_support::prod()`; reviewers should reject
  fresh `temp_dir()` incantations in `crates/daku-core/src`."*
- `test_support` is `pub` but test-only in spirit. Anything you add there
  follows `TempDb`'s shape: unique path, `Drop` cleanup, no panic in `Drop`.

## Commands you will need

| Purpose | Command | Expected on success |
|---------|---------|---------------------|
| Full gate | `bun run check` | exit 0 |
| Daemon black-box | `cargo test -p daku-daemon --test process` | all pass |
| Core tests | `cargo test -p daku-core` | all pass |
| Plan 028 criterion | `grep -rn 'temp_dir()' crates/daku-core/src \| grep -v 'test_support.rs\|daku-home\|settings.rs' \| wc -l` | `0` |

## Scope

**In scope**:
- `crates/daku-daemon/tests/process.rs`
- `crates/daku-core/src/test_support.rs`
- `crates/daku-core/src/config.rs`, `collector.rs`, `servicenow.rs` (test
  modules only)
- `crates/daku-client/src/persistence.rs` (test module only)

**Out of scope** (do NOT touch):
- Any production code path. Every edit in this plan is inside a `#[cfg(test)]`
  module or a `tests/` file, except the new `TempFile` helper in
  `test_support.rs`.
- `crates/daku-client/tests/loopback.rs` — it is clean (binds `127.0.0.1:0`,
  bounded poll loops, no fixed ports, no blind sleeps).
- `MAX_429_RETRIES` / `MAX_RETRY_AFTER` values — plan 012 set them.
- The `TempDb` implementation itself.

## Git workflow

- Trunk-based on `main`; **no pull requests, no GitHub Actions**.
- Commit style: imperative. Three independent changes — three commits preferred.

## Steps

### Step 1: Remove the `unsafe` env mutation

`ensure_process_home` exists because `DaemonSupervisor::spawn_configured` spawns
its child with the **inherited** environment, so the test binary's own `HOME`
has to point at the sandbox.

Prefer, in order:

1. **Best**: check whether `DaemonProcess::spawn_configured` can be given the
   home explicitly. If a `HOME` can be passed through without changing the
   supervisor's public API, do that and delete `ensure_process_home` entirely.
2. **Otherwise**: keep the process-wide `HOME` but set it **once, before any
   test thread reads the environment**, and gate the file to a single thread.
   The mechanism that actually removes the race is running this binary
   single-threaded; document it at the top of the file and confirm it.

Whichever you choose, the `unsafe { set_var }` must not remain in a
multi-threaded test binary. If you cannot remove it, **STOP and report** — do
not leave it with a comment.

**Verify**: `grep -n "set_var" crates/daku-daemon/tests/process.rs` → no
matches, **or** your report explains precisely why option 1 was impossible and
how option 2 removes the race.

### Step 2: Clean up on failure too

Add a `SandboxHome` struct to `crates/daku-daemon/tests/process.rs` with a
`Drop` that `remove_dir_all`s its directory, mirroring `TempDb`. Replace every
`sandbox_home()` call and every trailing `remove_dir_all` with it.

**Verify**: `cargo test -p daku-daemon --test process` → all pass, and
`ls $TMPDIR | grep -c 'daku-home'` is the same before and after a run. Then
deliberately break one assertion, run again, confirm the directory is still
cleaned, and restore the assertion.

### Step 3: Restore plan 028's criterion

Add a `TempFile` (or `TempJson`) helper to `crates/daku-core/src/test_support.rs`
following `TempDb`'s shape — unique path from a label plus a UUID, `Drop`
removes it — and convert the five regressed sites plus
`crates/daku-client/src/persistence.rs:106-111` to use it.

`daku-client` does not depend on `daku-core` at build time but does as a
**dev-dependency** (`crates/daku-client/Cargo.toml`), so the helper is reachable
from its tests. Confirm that before relying on it; if it is not, give
`daku-client` its own small helper rather than adding a build dependency.

**Verify**:
`grep -rn 'temp_dir()' crates/daku-core/src | grep -v 'test_support.rs\|daku-home\|settings.rs' | wc -l`
→ `0`. `cargo test -p daku-core` → all pass.

### Step 4: Pin the sustained-429 mapping

Two tests, using the existing scripted-transport pattern:

1. In `crates/daku-core/src/servicenow.rs` `mod tests` — script
   `MAX_429_RETRIES + 1` consecutive 429 responses; assert the caller receives
   status 429 (not an `Err`, not a success) and that exactly `MAX_429_RETRIES`
   sleeps were recorded by the test clock.
2. In `crates/daku-core/src/collector.rs` `mod tests` — an Availability probe
   that keeps returning 429; assert the persisted availability snapshot is
   `down` with `reachability: "unreachable"`, and that a gated Signal in the
   same group then records `skipped` with reason `"unreachable"`.

**Verify**: `cargo test -p daku-core servicenow_http` and
`cargo test -p daku-core collector` → both all-pass. Run them separately —
`cargo test` takes one TESTNAME and rejects a second positional argument.

## Test plan

This plan **is** a test plan; Step 4 is the new coverage and Steps 1–3 are
hygiene. Additionally add one self-test for the new helper, mirroring
`temp_db_removes_db_and_sidecars_on_drop` in `test_support.rs`:
`temp_file_is_removed_on_drop`.

**Verification**: `bun run check` → exit 0. Then run
`cargo test -p daku-daemon --test process` **five times** and confirm five clean
passes: `for i in 1 2 3 4 5; do cargo test -p daku-daemon --test process -q || break; done`

## Done criteria

ALL must hold:

- [ ] `bun run check` exits 0
- [ ] `grep -n "set_var" crates/daku-daemon/tests/process.rs` → no matches (or a
      documented, reviewed exception per Step 1)
- [ ] `grep -rn 'temp_dir()' crates/daku-core/src | grep -v 'test_support.rs\|daku-home\|settings.rs' | wc -l`
      → `0`
- [ ] `grep -n "impl Drop for SandboxHome" crates/daku-daemon/tests/process.rs`
      → one match
- [ ] `cargo test -p daku-core servicenow_http` → all pass, one more test
- [ ] `cargo test -p daku-core collector` → all pass, one more test
- [ ] Five consecutive runs of `cargo test -p daku-daemon --test process` pass
- [ ] `$TMPDIR` gains no `daku-*` entries across a full `cargo test --workspace`
      run, including one with a deliberately failing assertion
- [ ] `plans/README.md` status row for 059 updated to DONE

## STOP conditions

Stop and report back (do not improvise) if:

- The excerpts in "Current state" do not match the live code.
- **You cannot remove the `unsafe { set_var }` without changing production
  code.** Report the options rather than editing `crates/daku-client/src/process.rs`
  — that file is plan 051's.
- Any test becomes flaky across the five runs.
- Step 4's collector test needs a real sleep to observe the retry sleeps. Use
  the `Clock` trait; a real sleep in `bun run check` is not acceptable.

## Maintenance notes

- The rule to enforce in review, restating plan 028: **no fresh `temp_dir()` in
  `crates/daku-core/src`** — use `TempDb` / `TempFile`. The grep in "Done
  criteria" is the check; it has regressed once already, which is why it is
  worth re-running rather than trusting.
- `crates/daku-client/tests/loopback.rs` is the model for integration-test
  hygiene here: ephemeral ports, bounded poll loops, explicit deadlines. Copy
  it, not the daemon file's older patterns.
- Step 4's two tests together pin the *only* end-to-end story for rate limiting.
  If `MAX_429_RETRIES` or the availability classification ever changes, these
  are the tests that should fail first.
