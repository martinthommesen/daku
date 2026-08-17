# Plan 026: Black-box test of the daemon process — spawn, ready line, connect, shutdown/reap — sandboxed away from `~/.daku`

> **Executor instructions**: Follow this plan step by step. Run every
> verification command and confirm the expected result before moving to the
> next step. If anything in the "STOP conditions" section occurs, stop and
> report — do not improvise. When done, update the status row for this plan
> in `plans/README.md` — unless a reviewer dispatched you and told you they
> maintain the index.
>
> **Drift check (run first)**: `git diff --stat f7fdbe7..HEAD -- crates/daku-daemon/src/main.rs crates/daku-daemon/Cargo.toml crates/daku-client/src/process.rs crates/daku-core/src/config.rs crates/daku-core/src/persistence.rs crates/daku-protocol/src/settings.rs`
> If any of those files changed since this plan was written, compare the
> "Current state" excerpts against the live code before proceeding; on a
> mismatch, treat it as a STOP condition. (Plan 018 changes `monitor_daemon`
> in `process.rs`; that is expected and does not affect these tests.)

## Status

- **Priority**: P2
- **Effort**: M
- **Risk**: MED (spawns real processes; must stay hermetic — the child must never see the operator's real `~/.daku` or Keychain)
- **Depends on**: plans/011-green-baseline-check-gate.md; plans/012 (empty-token refusal — test 1c below assumes it; skip 1c if 012 has not landed)
- **Category**: tests
- **Planned at**: commit `f7fdbe7`, 2026-08-17
- **Issue**: https://github.com/martinthommesen/daku/issues/47

## Why this matters

The desktop's most visible failure mode is a daemon that does not launch, launches with a changed ready line, or is left orphaned when the app quits. None of that is tested: `crates/daku-client/src/process.rs` `DaemonProcess::spawn_configured` (token via env, argv, ready-line parse, protocol check), `stop()`/`Drop` (graceful-then-kill + reap), and `crates/daku-daemon/src/main.rs` (ready JSON, `remove_var(DAKU_DAEMON_TOKEN)`, parent-pid watchdog) have only pure-helper unit tests (`process.rs:618-643`, `main.rs:168-209`).

Cargo builds the daemon binary for integration tests and exposes it as `env!("CARGO_BIN_EXE_daku-daemon")`, so a black-box test costs one file. The only hazard is hermeticity: the daemon resolves everything under `dirs::home_dir()` — which on macOS/Linux is `$HOME` (verified in `dirs-sys` 0.5: `env::var_os("HOME")` first) — so pointing `HOME` at a temp dir sandboxes settings, DB, and `environments.json`. With no `environments.json` there, `start_default_loop` returns `None` and no collector (hence no Keychain access, no network) runs.

## Current state

### `crates/daku-daemon/src/main.rs` (verified at HEAD)

```rust
// :10-30
fn main() -> anyhow::Result<()> {
    let arguments = Arguments::parse(std::env::args().skip(1))?;
    if arguments.probe_availability { return run_probe_availability(); }
    let auth = std::env::var(DAEMON_TOKEN_ENV).context("DAKU_DAEMON_TOKEN is missing")?;
    unsafe { std::env::remove_var(DAEMON_TOKEN_ENV) };
    let listener = TcpListener::bind(&arguments.bind)…?;
    let address = listener.local_addr()?;
    ensure_bind_allowed(address, arguments.allow_non_loopback)?;
    let ready = DaemonReady { address: address.to_string(), protocol_version: PROTOCOL_VERSION, pid: std::process::id() };
    println!("{}", serde_json::to_string(&ready)?);
    std::io::stdout().flush()?;
// :33-46  optional --parent-pid watchdog thread (kill(pid,0) every 500 ms → sets shutdown)
// :48-60
    let task_path = daku_core::persistence::StateStore::default_path();          // DAKU_DB_PATH or ~/.daku/app.db
    let settings = daku_core::DaemonSettingsStore::open_with_legacy(
        daku_core::DaemonSettings::default_path(),                              // ~/.daku/settings.json
        [task_path.with_file_name("settings.json")],
    )…?;
    let task_store = daku_core::persistence::StateStore::daemon(task_path.clone());
    let dashboard_events = daku_core::start_default_loop(
        &daku_core::default_environments_path(),                                // ~/.daku/environments.json
        …);
// :61-72  daku_core::serve(listener, auth, Arc::new(HollowBackend::new(settings, task_store)?), shutdown, ServerOptions{ allowed_origins, allow_shutdown: arguments.parent_pid.is_some() }, dashboard_events)
```

Arguments (`:100-155`): `probe-availability`, `--bind ADDR` (default `127.0.0.1:0`), `--parent-pid PID`, `--allow-origin ORIGIN`…, `--allow-non-loopback`, `--help`. `allow_shutdown` is true **only** when `--parent-pid` was given.

Paths (all `dirs::home_dir()`-relative, verified): `crates/daku-protocol/src/settings.rs:30-35` `DaemonSettings::default_path()` = `~/.daku/settings.json`; `crates/daku-core/src/config.rs:33-38` `default_environments_path()` = `~/.daku/environments.json`; `crates/daku-core/src/persistence.rs:95-105` `StateStore::default_path()` = `$DAKU_DB_PATH` if non-empty else `~/.daku/app.db`. `dirs::home_dir()` falls back to `std::env::temp_dir()` only when `HOME` is unset/empty.

`crates/daku-daemon/Cargo.toml`: `[[bin]] daku-daemon` (`src/main.rs`) + feature-gated `daku-debug-daemon`; deps `anyhow`, `serde_json`, `daku-core`, `daku-protocol`, unix `libc`. No dev-dependencies.

### `crates/daku-client/src/process.rs` (verified at HEAD)

```rust
// :35-40  pub struct DaemonExposureSettings { pub enabled: bool, pub port: u16, pub allowed_origins: Vec<String>, pub token: String }   (#[serde(default)], Default: enabled=false, port=34123, origins ["http://localhost:3001"], token=new uuid)
// :57-64
    pub fn ensure_token(&mut self) -> bool {          // fills an empty/whitespace token; returns whether it minted
// :74-83
    pub fn validate(mut self) -> anyhow::Result<Self> { // port 0 → Err "daemon port must be between 1 and 65535"; empty token → Err "daemon authentication token is empty"; re-parses origins
// :85-91  fn bind_address(&self) -> String  // enabled → "0.0.0.0:{port}", else "127.0.0.1:0"
// :129-136
pub struct DaemonProcess { client: DaemonClient, child: Child }
impl DaemonProcess {
    pub fn spawn(executable: &Path) -> anyhow::Result<Self> { Self::spawn_configured(executable, DaemonExposureSettings::default()) }
    fn spawn_configured(executable: &Path, settings: DaemonExposureSettings) -> anyhow::Result<Self> {   // PRIVATE
// :146-165  argv: --bind <addr> --parent-pid <own pid> [--allow-non-loopback] [--allow-origin o]*; env DAKU_DAEMON_TOKEN=<token>, DAKU_APP_EXECUTABLE=<current_exe>; stdin null, stdout piped, stderr inherit
// :171-192  reads ONE stdout line on a thread, parses DaemonReady, START_TIMEOUT 15 s; on failure kill+wait
// :193-201  ready.protocol_version != PROTOCOL_VERSION → kill + bail "daemon protocol … does not match"
// :211-218  DaemonClient::connect(desktop_client_address(&ready.address), auth)
// :227-245  has_exited(); stop(): client.shutdown(), poll try_wait up to SHUTDOWN_TIMEOUT (1 s), then kill+wait
// :247-251  impl Drop for DaemonProcess { fn drop(&mut self) { self.stop(); } }
// :328-356  DaemonSupervisor::spawn(executable, watch_for_rebuilds) / spawn_configured(executable, watch_for_rebuilds, exposure) → spawns DaemonProcess, read_settings, monitor thread
// :390-402  pub fn subscribe_clients(&self) -> Receiver<DaemonClient>; pub fn client(&self) -> DaemonClient
```

`DaemonSupervisor` has no `Drop`; dropping the last clone drops `SupervisorInner` → `DaemonTarget::Local(DaemonProcess)` → `DaemonProcess::drop` → `stop()`. The env the child inherits is the **test process's** env (only `DAKU_DAEMON_TOKEN`/`DAKU_APP_EXECUTABLE` are added), so `HOME` must be set in the test process before spawning via the supervisor.

Existing tests to model after: `process.rs:622-643` (`browser_origins_are_exact_and_deduplicated`, `desktop_uses_loopback_to_reach_an_unspecified_listener`); `main.rs:172-208`.

`daku-client` re-exports `DaemonSupervisor`, `DaemonProcess`, `DaemonExposureSettings`? — check `crates/daku-client/src/lib.rs` (12 lines) before writing imports; at HEAD it is `pub use daku_protocol::*;` plus module re-exports — read it and adjust the `use` lines in Step 3.

## Commands you will need

| Purpose | Command | Expected on success |
|---------|---------|---------------------|
| Unit tests (process helpers) | `cargo test -p daku-client validate_ ensure_token` | pass |
| Black-box test | `cargo test -p daku-daemon --test process` | all pass |
| Confirm hermetic | `ls -la <tempdir>/.daku` printed by the test | contains `settings.json`, `app.db`; **no** `environments.json` |
| Gate | `bun run check` | exit 0 |

## Scope

**In scope**:
- `crates/daku-daemon/tests/process.rs` (create)
- `crates/daku-daemon/Cargo.toml` (`[dev-dependencies] daku-client = { path = "../daku-client" }` and, if not already transitively usable, `libc = "0.2"` under `[dev-dependencies]` — `libc` is already a unix dependency, so it is available)
- `crates/daku-client/src/process.rs` — **tests only**: add `exposure_validate_rejects_port_zero_and_empty_token` and `ensure_token_mints_only_when_empty` to the existing `mod tests`
- `plans/README.md` (status row)

**Out of scope**:
- Any production change in `process.rs`/`main.rs`. In particular do **not** add a `DAKU_HOME` env override — `HOME` is a clean seam (verified above); note in the report if it turned out not to be.
- Making `DaemonProcess::spawn_configured` public.
- Restart/backoff behaviour (plan 018).

## Git workflow

- Commit on `main`; do not push unless asked. Suggested summary: `Add black-box daemon process test and exposure-settings unit tests.`

## Steps

### Step 1: Unit tests for the pure helpers

In `crates/daku-client/src/process.rs` `mod tests` add:

```rust
    #[test]
    fn exposure_validate_rejects_port_zero_and_empty_token() {
        let mut settings = DaemonExposureSettings::default();
        settings.port = 0;
        assert!(settings.clone().validate().unwrap_err().to_string().contains("port"));
        let mut settings = DaemonExposureSettings::default();
        settings.token = "   ".into();
        assert!(settings.validate().unwrap_err().to_string().contains("token"));
        assert!(DaemonExposureSettings::default().validate().is_ok());
    }

    #[test]
    fn ensure_token_mints_only_when_empty() {
        let mut settings = DaemonExposureSettings::default();
        let before = settings.token.clone();
        assert!(!settings.ensure_token());
        assert_eq!(settings.token, before);
        settings.token.clear();
        assert!(settings.ensure_token());
        assert!(!settings.token.trim().is_empty());
    }
```

**Verify**: `cargo test -p daku-client exposure_validate ensure_token` → 2 passed.

### Step 2: Dev-dependency

Append to `crates/daku-daemon/Cargo.toml`:

```toml
[dev-dependencies]
daku-client = { path = "../daku-client" }
```

(`daku-client` depends only on `daku-protocol` → no cycle with `daku-daemon`.)

**Verify**: `cargo check -p daku-daemon --tests` → exit 0.

### Step 3: `crates/daku-daemon/tests/process.rs`

Create the file. Structure:

```rust
use std::io::{BufRead as _, BufReader};
use std::path::PathBuf;
use std::process::{Command, Stdio};
use std::sync::Once;
use std::time::{Duration, Instant};

use daku_protocol::{Command as DaemonCommand, DaemonReady, ResponsePayload, PROTOCOL_VERSION};

const DAEMON: &str = env!("CARGO_BIN_EXE_daku-daemon");
static SANDBOX: Once = Once::new();

/// Fresh empty HOME so the daemon never sees the operator's ~/.daku or Keychain.
fn sandbox_home() -> PathBuf {
    let home = std::env::temp_dir().join(format!("daku-home-{}", uuid::Uuid::new_v4()));
    std::fs::create_dir_all(&home).unwrap();
    home
}

/// Process-wide: the supervisor spawns children with the *inherited* env, so HOME
/// must be set once for this test binary. Every test in this file tolerates that.
fn ensure_process_home() -> PathBuf {
    static HOME: std::sync::OnceLock<PathBuf> = std::sync::OnceLock::new();
    HOME.get_or_init(|| {
        let home = sandbox_home();
        SANDBOX.call_once(|| unsafe { std::env::set_var("HOME", &home) });
        home
    }).clone()
}

fn pid_alive(pid: u32) -> bool {
    unsafe { libc::kill(pid as i32, 0) == 0 }
}
```

(`uuid` is not a dep of `daku-daemon`; use `std::process::id()` + a counter or `std::time::SystemTime` nanos for uniqueness instead if you prefer not to add `uuid` as a dev-dependency — either is fine; if you add it: `uuid = { version = "1.18", features = ["v4"] }` under `[dev-dependencies]`.)

Tests:

1. `daemon_prints_one_ready_line_and_serves`
   - `let home = sandbox_home();` spawn `Command::new(DAEMON).env_clear().env("HOME", &home).env("PATH", std::env::var_os("PATH").unwrap_or_default()).env("DAKU_DAEMON_TOKEN", "test-token").arg("--bind").arg("127.0.0.1:0").stdout(Stdio::piped()).stderr(Stdio::piped())`.
   - Read one line from stdout with a 15 s bound (spawn a reader thread + `mpsc::sync_channel(1)` `recv_timeout`, exactly like `process.rs:171-192`); `serde_json::from_str::<DaemonReady>` → assert `protocol_version == PROTOCOL_VERSION`, `pid == child.id()`, `address` parses as `SocketAddr` with loopback ip and non-zero port.
   - Connect with `daku_client::DaemonClient::connect(&ready.address, "test-token".into())` → `request(Uuid::nil(), Uuid::nil(), DaemonCommand::Ping)` → `Ack`. Then `child.kill()`, `child.wait()`.
   - Assert hermeticity: `home.join(".daku/settings.json").exists()`, `home.join(".daku/app.db").exists()`, `!home.join(".daku/environments.json").exists()`; and stderr (read after wait) contains `daku collector idle: missing` (`collector.rs:166-169` prints that when `environments.json` is absent — proves no collector ran).
   - 1b. `daemon_refuses_to_start_without_token`: same command with **no** `DAKU_DAEMON_TOKEN` → `child.wait()` exits non-zero and stderr contains `DAKU_DAEMON_TOKEN is missing`.
   - 1c (only if plan 012 landed): `DAKU_DAEMON_TOKEN=""` → non-zero, stderr contains `is empty`.

2. `supervisor_spawns_and_reaps_the_daemon`
   - `let _home = ensure_process_home();` (sets `HOME` process-wide for the child to inherit).
   - `let supervisor = daku_client::DaemonSupervisor::spawn(std::path::Path::new(DAEMON), false).unwrap();` (check the re-export path in `crates/daku-client/src/lib.rs`; if `DaemonSupervisor` is under `daku_client::process::DaemonSupervisor`, use that).
   - `let client = supervisor.client(); assert!(matches!(client.request(Uuid::nil(), Uuid::nil(), DaemonCommand::Ping).unwrap(), ResponsePayload::Ack));`
   - Get the child pid: the supervisor does not expose it — read it from a second `GetSettings`? No. Instead: the daemon prints its pid only on stdout which the supervisor consumed. Use `pgrep -P <own pid> -f daku-daemon` via `Command::new("pgrep").args(["-P", &std::process::id().to_string(), "-f", "daku-daemon"])` and parse the first pid; assert `pid_alive(pid)`.
   - `drop(client); drop(supervisor);` then poll for up to 5 s until `!pid_alive(pid)`; assert it died (graceful `Shutdown` is allowed because the supervisor passes `--parent-pid`, so this exercises the `ShuttingDown` path, with kill as fallback after 1 s).

3. `daemon_exits_when_parent_pid_dies`
   - Spawn a throwaway parent: `Command::new("sleep").arg("30").spawn()` → its pid is the "parent". Spawn the daemon (as in test 1, sandboxed `HOME`) with `--parent-pid <sleep pid>`; read the ready line. `kill(sleep)` + wait; then poll ≤ 3 s until `child.try_wait()` is `Ok(Some(_))` (watchdog checks every 500 ms, `main.rs:38-44`). Assert it exited.

**Verify**: `cargo test -p daku-daemon --test process` → all pass; run three times in a row without flakes. Also `ls -la "$(ls -dt /tmp/daku-home-* 2>/dev/null | head -1)/.daku"` (or the macOS `$TMPDIR` equivalent) shows only `settings.json`, `app.db` (+ `-wal/-shm`), never `environments.json`.

### Step 4: Gate

**Verify**: `bun run check` → exit 0.

## Test plan

- `crates/daku-client/src/process.rs`: `exposure_validate_rejects_port_zero_and_empty_token`, `ensure_token_mints_only_when_empty` (model on `desktop_uses_loopback_to_reach_an_unspecified_listener`, `process.rs:634`).
- `crates/daku-daemon/tests/process.rs`: `daemon_prints_one_ready_line_and_serves` (+1b, +1c conditional), `supervisor_spawns_and_reaps_the_daemon`, `daemon_exits_when_parent_pid_dies`.
- `cargo test --workspace --no-fail-fast` → 0 failed.

## Done criteria

- [ ] `cargo test -p daku-daemon --test process` → ≥3 passed, three consecutive runs green
- [ ] `grep -n 'CARGO_BIN_EXE_daku-daemon' crates/daku-daemon/tests/process.rs` → 1 match
- [ ] `grep -n 'HOME' crates/daku-daemon/tests/process.rs` → every daemon spawn sets/inherits a sandbox `HOME`; `grep -n '\.daku' crates/daku-daemon/tests/process.rs` refers only to paths under the sandbox
- [ ] `cargo test -p daku-client exposure_validate ensure_token` → 2 passed
- [ ] `bun run check` exits 0
- [ ] `git status` shows only in-scope files modified
- [ ] `plans/README.md` status row for 026 updated

## STOP conditions

- The daemon touches anything outside the sandbox `HOME` during the test (check with `ls -la ~/.daku` mtimes before/after) — report immediately; do not proceed.
- `DaemonSupervisor::spawn` is no longer public or its signature changed (plan 018/020 may touch it) — adapt only if mechanical.
- Any test needs more than 15 s or is flaky across three runs — report rather than adding sleeps.
- `pgrep` is unavailable — replace with reading `/proc` is not possible on macOS; report and drop the pid assertion in test 2 (keep the rest).

## Maintenance notes

- Because `HOME` is process-wide, keep every daemon-spawning test in this one file and never read `HOME` in it after `ensure_process_home()`; adding a second integration file that also spawns the daemon must repeat the sandbox pattern.
- Reviewers: verify test 1's `env_clear()` keeps `PATH` only, and that stderr assertions do not depend on plan 019 (which redirects the child's stderr to a log file **only** when spawned by the supervisor — test 1 spawns directly, so `stderr(Stdio::piped())` still works).
- Deferred: a restart-on-crash test belongs to plan 018 (needs backoff to be observable).
