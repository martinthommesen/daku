# Plan 018: Supervisor restarts with bounded backoff, and remote daemons reconnect

> **Executor instructions**: Follow this plan step by step. Run every
> verification command and confirm the expected result before moving to the
> next step. If anything in the "STOP conditions" section occurs, stop and
> report — do not improvise. When done, update the status row for this plan
> in `plans/README.md` — unless a reviewer dispatched you and told you they
> maintain the index.
>
> **Drift check (run first)**: `git diff --stat f7fdbe7..HEAD -- crates/daku-client/src/process.rs crates/daku-client/src/client.rs src/app.rs`
> If any of those files changed since this plan was written, compare the
> "Current state" excerpts against the live code before proceeding; on a
> mismatch, treat it as a STOP condition.

## Status

- **Priority**: P2
- **Effort**: M
- **Risk**: LOW
- **Depends on**: plans/011-green-baseline-check-gate.md (gate). Plan 014 (dashboard replay) makes a reconnect immediately useful — land 014 first if possible; not a hard dependency.
- **Category**: bug
- **Planned at**: commit `f7fdbe7`, 2026-08-17
- **Issue**: https://github.com/martinthommesen/daku/issues/42

## Why this matters

1. **Crash loop with no backoff.** `monitor_daemon` polls every 500 ms; when the local daemon has exited it calls `replace_local_daemon`, which spawns a new process and waits up to `START_TIMEOUT` (15 s) for the ready line. If the daemon dies at startup (port in use with exposure enabled, bad `--allow-origin`, unreadable `~/.daku`), the failure path is `eprintln!` + `continue` — an infinite spawn loop for the app's lifetime, stderr spam, and a UI that only says "Disconnected".
2. **Remote mode never reconnects.** For `DaemonTarget::Remote` (`DAKU_DAEMON_ADDRESS` + `DAKU_DAEMON_TOKEN`), `monitor_daemon` returns immediately. When the WebSocket drops (daemon upgrade, laptop sleep), `src/app.rs` flips to disconnected and waits on `subscribe_clients()` for a client that never arrives; the Operator must relaunch the app.

Fix both in the same supervisor thread: exponential backoff (500 ms → 30 s cap) for local respawns with the last error kept, and a reconnect loop for remote targets that re-runs `DaemonClient::connect` and pushes the new client through `client_updates`.

## Current state

### `crates/daku-client/src/process.rs`

```rust
// :22-24
const START_TIMEOUT: Duration = Duration::from_secs(15);
const SHUTDOWN_TIMEOUT: Duration = Duration::from_secs(1);
const REBUILD_POLL_INTERVAL: Duration = Duration::from_millis(500);
```

```rust
// :291-307
struct SupervisorInner {
    executable: Option<PathBuf>,
    target: Mutex<DaemonTarget>,
    exposure: Mutex<Option<DaemonExposureSettings>>,
    restart: Mutex<()>,
    settings: Mutex<DaemonSettings>,
    persisted_settings: Mutex<Option<DaemonSettings>>,
    settings_updates: Sender<DaemonSettings>,
    client_updates: Mutex<Vec<Sender<DaemonClient>>>,
    running: AtomicBool,
}

enum DaemonTarget {
    Local(DaemonProcess),
    Restarting(DaemonClient),
    Remote(DaemonClient),
}
```

`DaemonSupervisor::spawn_configured` (`:335-356`) spawns `monitor_daemon` on a thread named `daku-daemon-supervisor`. `DaemonSupervisor::connect` (`:358-364`) builds a `Remote` target via `from_target` and spawns **no** monitor thread. `from_target` (`:366-392`) stores `executable`/`exposure` (both `None` for remote).

```rust
// :478-523
fn monitor_daemon(
    weak_inner: std::sync::Weak<SupervisorInner>,
    mut active_stamp: ExecutableStamp,
    watch_for_rebuilds: bool,
) {
    loop {
        std::thread::sleep(REBUILD_POLL_INTERVAL);
        let Some(inner) = weak_inner.upgrade() else { return; };
        if !inner.running.load(Ordering::Acquire) { return; }
        let process_exited = match &mut *inner.target.lock() {
            DaemonTarget::Local(process) => process.has_exited(),
            DaemonTarget::Restarting(_) => true,
            DaemonTarget::Remote(_) => return,
        };
        let Some(executable) = inner.executable.as_ref() else { return; };
        let observed_stamp = ExecutableStamp::read(executable).ok();
        let executable_changed =
            watch_for_rebuilds && observed_stamp.is_some_and(|observed| observed != active_stamp);
        if !process_exited && !executable_changed {
            continue;
        }
        let _restart = inner.restart.lock();
        let Some(exposure) = inner.exposure.lock().clone() else { return; };
        match replace_local_daemon(&inner, executable, &exposure) {
            Ok(()) => {}
            Err(error) => {
                eprintln!("could not restart rebuilt daku daemon: {error:#}");
                continue;
            }
        }
        queue_settings_refresh(&inner);
        if let Some(observed_stamp) = observed_stamp {
            active_stamp = observed_stamp;
        }
        drop(_restart);
        drop(inner);
    }
}
```

`replace_local_daemon` (`:525-559`) swaps the target to `Restarting(old client)`, drops the old process (graceful stop), spawns a replacement, sets `Local(replacement)`, and fans the new client out to `client_updates` subscribers. `DaemonClient` has `connect(address, token)` (`crates/daku-client/src/client.rs:54`) and `is_disconnected()`-style state via `ClientInner.disconnected: AtomicBool` — check the exact public accessor name in `client.rs` (grep `disconnected`); if none is public, add `pub fn is_disconnected(&self) -> bool { self.inner.disconnected.load(Ordering::Acquire) }`.

Tests in `process.rs` (`:619-644`) cover only `parse_allowed_origins` and `desktop_client_address`. There is no process-spawning test in the repo (a black-box daemon test is a separate backlog item).

### `src/app.rs:48-98` (`listen_dashboard`)

Loops on `supervisor.subscribe_clients()`: for each client it sets connected, subscribes to the dashboard, and on channel close sets `connected=false` and waits for the **next client**. So delivering a fresh `DaemonClient` through `client_updates` is all the UI needs to recover — no UI change required.

Conventions: `parking_lot::Mutex` (no unwrap), `anyhow` errors, `eprintln!` for supervisor diagnostics (plan 019 redirects daemon stderr to a log file; the supervisor's own eprintln stays), constants at the top of the file, imperative commit summaries.

## Commands you will need

| Purpose | Command | Expected on success |
|---------|---------|---------------------|
| Client tests | `cargo test -p daku-client` | all pass |
| Check | `cargo check --workspace` | exit 0 |
| Gate | `bun run check` | exit 0 |
| Manual: crash loop | see Step 4 | backoff visible in stderr; app stays responsive |
| Manual: remote reconnect | see Step 4 | app reconnects after daemon restart |

## Scope

**In scope**:
- `crates/daku-client/src/process.rs`
- `crates/daku-client/src/client.rs` (only if a public `is_disconnected()` accessor is missing)
- `plans/README.md` (status row)

**Out of scope**:
- `src/app.rs` — no UI change; the existing `subscribe_clients` loop already handles a new client.
- Surfacing the last error in the UI (needs a new protocol/UI seam — see backlog BUG-10; here the last error is kept on `SupervisorInner` and printed).
- `persist_settings`/`queue_settings_refresh` (removed by plan 020) — leave the calls where they are; if plan 020 already landed and they are gone, skip them.
- `START_TIMEOUT`, `SHUTDOWN_TIMEOUT` values.

## Git workflow

- Commit on `main`; do not push unless asked. Suggested summary: `Back off daemon respawns and reconnect remote daemons.`

## Steps

### Step 1: Backoff constants and helper

At the top of `process.rs` add:

```rust
/// First delay after a failed respawn/reconnect; doubles per failure.
const RESTART_BACKOFF_MIN: Duration = Duration::from_millis(500);
/// Ceiling for the doubling — a dead daemon costs one spawn per 30 s, not two per second.
const RESTART_BACKOFF_MAX: Duration = Duration::from_secs(30);

fn next_backoff(current: Duration) -> Duration {
    (current * 2).min(RESTART_BACKOFF_MAX)
}
```

Add `last_error: Mutex<Option<String>>` to `SupervisorInner` (initialise `Mutex::new(None)` in `from_target`) and a public accessor on `DaemonSupervisor`:

```rust
    /// Why the last respawn/reconnect failed, if it did (cleared on success).
    pub fn last_error(&self) -> Option<String> {
        self.inner.last_error.lock().clone()
    }
```

Add a unit test `backoff_doubles_and_caps`: `next_backoff(500ms) == 1s`, `next_backoff(20s) == 30s`, `next_backoff(30s) == 30s`.

**Verify**: `cargo test -p daku-client backoff` → 1 passed.

### Step 2: Local respawn with backoff

Rewrite the failure path in `monitor_daemon`. Keep the loop shape; introduce `let mut backoff = RESTART_BACKOFF_MIN;` before the loop and replace the sleep + restart block:

```rust
    let mut backoff = RESTART_BACKOFF_MIN;
    loop {
        std::thread::sleep(REBUILD_POLL_INTERVAL);
        … (unchanged: upgrade, running check, process_exited, executable, stamps, `continue` when nothing to do)
        let _restart = inner.restart.lock();
        let Some(exposure) = inner.exposure.lock().clone() else { return; };
        match replace_local_daemon(&inner, executable, &exposure) {
            Ok(()) => {
                *inner.last_error.lock() = None;
                backoff = RESTART_BACKOFF_MIN;
            }
            Err(error) => {
                let message = format!("{error:#}");
                eprintln!("could not restart daku daemon (retry in {backoff:?}): {message}");
                *inner.last_error.lock() = Some(message);
                drop(_restart);
                drop(inner);
                std::thread::sleep(backoff);
                backoff = next_backoff(backoff);
                continue;
            }
        }
        queue_settings_refresh(&inner);   // (delete this line if plan 020 already removed the fn)
        if let Some(observed_stamp) = observed_stamp {
            active_stamp = observed_stamp;
        }
        drop(_restart);
        drop(inner);
    }
```

Note the failure branch drops the `restart` guard and the `Arc` **before** sleeping so `reconfigure()`/`Drop` are not blocked for up to 30 s. `running` is re-checked at the top of the next iteration, so app quit still ends the thread within one backoff.

**Verify**: `cargo check -p daku-client` → exit 0; `cargo test -p daku-client` → all pass.

### Step 3: Remote reconnect

`DaemonSupervisor::connect` needs to remember `address` and `token` for reconnects. Add to `SupervisorInner`: `remote: Option<(String, String)>` (address, token) — set `Some((address.to_owned(), token.clone()))` in `connect`, `None` in `spawn_configured`. (`from_target` gains a parameter; both call sites updated.)

In `connect`, after `from_target`, spawn a thread named `daku-daemon-reconnect` running `monitor_remote(weak_inner)`:

```rust
fn monitor_remote(weak_inner: std::sync::Weak<SupervisorInner>) {
    let mut backoff = RESTART_BACKOFF_MIN;
    loop {
        std::thread::sleep(REBUILD_POLL_INTERVAL);
        let Some(inner) = weak_inner.upgrade() else { return; };
        if !inner.running.load(Ordering::Acquire) { return; }
        let Some((address, token)) = inner.remote.clone() else { return; };
        let disconnected = match &*inner.target.lock() {
            DaemonTarget::Remote(client) => client.is_disconnected(),
            _ => return,
        };
        if !disconnected {
            backoff = RESTART_BACKOFF_MIN;
            continue;
        }
        match DaemonClient::connect(&address, token) {
            Ok(client) => {
                *inner.target.lock() = DaemonTarget::Remote(client.clone());
                inner.client_updates.lock().retain(|subscriber| subscriber.send(client.clone()).is_ok());
                *inner.last_error.lock() = None;
                backoff = RESTART_BACKOFF_MIN;
            }
            Err(error) => {
                let message = format!("{error:#}");
                eprintln!("could not reconnect to daku daemon at {address} (retry in {backoff:?}): {message}");
                *inner.last_error.lock() = Some(message);
                drop(inner);
                std::thread::sleep(backoff);
                backoff = next_backoff(backoff);
            }
        }
    }
}
```

`DaemonClient::is_disconnected`: in `crates/daku-client/src/client.rs`, `ClientInner.disconnected: AtomicBool` is set when the reader loop ends (grep `disconnected.store`). If no public accessor exists, add `pub fn is_disconnected(&self) -> bool { self.inner.disconnected.load(Ordering::Acquire) }` next to `subscribe_dashboard`.

**Verify**: `cargo check --workspace` → exit 0; `cargo test -p daku-client` → all pass.

### Step 4: Manual verification (no automated process test exists yet)

Crash loop: run the debug app with a daemon path that exits immediately, e.g. `DAKU_DAEMON_PATH=/usr/bin/false bun run dev` (or `cargo run` per README). Expected in the terminal: `could not restart daku daemon (retry in 500ms)`, then `1s`, `2s`, … capped at `30s`; the window opens and shows the disconnected banner; quitting the app returns promptly.

Remote reconnect: in one terminal `DAKU_DAEMON_TOKEN=<any-non-empty> cargo run -p daku-daemon -- --bind 127.0.0.1:34123` (note the printed address); in another `DAKU_DAEMON_ADDRESS=127.0.0.1:34123 DAKU_DAEMON_TOKEN=<same> cargo run` (root crate). Kill the daemon (Ctrl-C), observe the disconnected banner, restart the daemon with the same token → within ~1 s the banner disappears (and, with plan 014 landed, the dashboard repopulates).

Record both outcomes in the plan's status note.

### Step 5: Gate

**Verify**: `bun run check` → exit 0.

## Test plan

- `process.rs`: `backoff_doubles_and_caps` (pure). Model on `desktop_uses_loopback_to_reach_an_unspecified_listener` (`:636`).
- Manual: Step 4 (crash loop cadence; remote reconnect). The black-box daemon process test (backlog TEST-02) is the place to automate this later.
- `cargo test --workspace --no-fail-fast` → 0 failed.

## Done criteria

- [ ] `grep -n 'RESTART_BACKOFF_MIN\|RESTART_BACKOFF_MAX\|fn next_backoff' crates/daku-client/src/process.rs` → 3 definitions, used in both monitor fns
- [ ] `grep -n 'fn monitor_remote' crates/daku-client/src/process.rs` → 1 match; `DaemonSupervisor::connect` spawns it
- [ ] `grep -n 'DaemonTarget::Remote(_) => return' crates/daku-client/src/process.rs` → still present in `monitor_daemon` only (local monitor ignores remote; remote monitor is separate)
- [ ] `grep -n 'pub fn last_error' crates/daku-client/src/process.rs` → 1 match
- [ ] `cargo test -p daku-client` passes with the new test; `bun run check` exits 0
- [ ] Manual checks in Step 4 recorded (crash-loop cadence capped at 30 s; remote reconnect observed)
- [ ] `git status` shows only in-scope files modified
- [ ] `plans/README.md` status row for 018 updated

## STOP conditions

- `monitor_daemon`/`replace_local_daemon`/`from_target` no longer match the excerpts (e.g. plan 020 restructured `SupervisorInner`) — re-read and adapt only if mechanical; otherwise report.
- `ClientInner` has no `disconnected` flag (client rewritten) — report; do not invent a liveness probe.
- Manual crash-loop check shows the app UI blocked (a lock held across the sleep) — fix the drop order; if unclear, report.

## Maintenance notes

- `last_error()` is the seam for a future "why disconnected" banner (backlog BUG-10/DIR-04).
- If `Drop for DaemonSupervisor` ever needs a fast shutdown, add a `Condvar`/channel instead of the sleep; today quit waits ≤ one backoff step (≤ 30 s worst case, only while a respawn is already failing).
- Reviewers: check that neither monitor thread holds `inner.target` or `inner.restart` across a sleep or a `DaemonClient::connect` (which blocks up to the client's connect timeout).
