# Plan 051: The supervisor recovers a local daemon whose socket dropped, and the restart loop finally has a test

> **Executor instructions**: Follow this plan step by step. Run every
> verification command and confirm the expected result before moving to the
> next step. If anything in the "STOP conditions" section occurs, stop and
> report — do not improvise. When done, update the status row for this plan
> in `plans/README.md`.
>
> **Drift check (run first)**:
> `git diff --stat 2bdeaba..HEAD -- crates/daku-client/src/process.rs crates/daku-daemon/tests/process.rs`
> If any in-scope file changed since this plan was written, compare the
> "Current state" excerpts against the live code before proceeding; on a
> mismatch, treat it as a STOP condition.

## Status

- **Priority**: P1
- **Effort**: S–M
- **Risk**: MED
- **Depends on**: none
- **Category**: bug + tests
- **Planned at**: commit `2bdeaba`, 2026-08-18

## Why this matters

The supervisor has two monitor loops. `monitor_remote` recovers when the
**socket** drops. `monitor_daemon` recovers only when the **process** exits.

So if the local daemon's connection breaks while the daemon itself keeps
running — which the daemon's own per-connection error path produces, since it
is thread-per-connection and a failed connection thread just returns — the
desktop sits on a permanently "Disconnected" banner over a frozen dashboard.
The daemon keeps polling ServiceNow into SQLite; the app never sees any of it.
`monitor_daemon` sees a live child and does nothing, forever. Only relaunching
the app recovers.

Plan 018 fixed exactly this for remote daemons. This is the surviving half of
the same bug on the default path.

The second half of this plan is why it survived: **the entire restart loop has
zero tests.** `monitor_daemon`, `monitor_remote` and `replace_local_daemon`
have none; the six tests in `process.rs` cover pure helpers (backoff
arithmetic, origin parsing, token minting), and the black-box test in
`crates/daku-daemon/tests/process.rs` asserts the child is reaped on drop but
never kills it and never asserts a replacement appears. `backoff_doubles_and_caps`
proves the arithmetic, not that anything ever calls it.

## Current state

`crates/daku-client/src/process.rs` (682 lines) — the daemon process
supervisor, owned by the desktop.

**`crates/daku-client/src/process.rs:469-473`** — the bug. Only process exit
counts:

```rust
        let process_exited = match &mut *inner.target.lock() {
            DaemonTarget::Local(process) => process.has_exited(),
            DaemonTarget::Restarting(_) => true,
            DaemonTarget::Remote(_) => return,
        };
```

**`crates/daku-client/src/process.rs:526-533`** — the remote loop, which gets
it right:

```rust
        let disconnected = match &*inner.target.lock() {
            DaemonTarget::Remote(client) => client.is_disconnected(),
            _ => return,
        };
        if !disconnected {
            backoff = RESTART_BACKOFF_MIN;
            continue;
        }
```

**`crates/daku-client/src/process.rs:455-509`** — the whole local loop, for
context. Note it already holds `inner.restart.lock()` before recovering,
already has `backoff` / `next_backoff`, and already calls `replace_local_daemon`:

```rust
fn monitor_daemon(
    weak_inner: std::sync::Weak<SupervisorInner>,
    mut active_stamp: ExecutableStamp,
    watch_for_rebuilds: bool,
) {
    let mut backoff = RESTART_BACKOFF_MIN;
    loop {
        std::thread::sleep(REBUILD_POLL_INTERVAL);
        ...
        let observed_stamp = ExecutableStamp::read(executable).ok();
        let executable_changed =
            watch_for_rebuilds && observed_stamp.is_some_and(|observed| observed != active_stamp);
        if !process_exited && !executable_changed {
            continue;
        }
        let _restart = inner.restart.lock();
        let Some(exposure) = inner.exposure.lock().clone() else {
            return;
        };
        match replace_local_daemon(&inner, executable, &exposure) {
            Ok(()) => { ... backoff = RESTART_BACKOFF_MIN; }
            Err(error) => { ... std::thread::sleep(backoff); backoff = next_backoff(backoff); continue; }
        }
        ...
    }
}
```

**`crates/daku-client/src/process.rs:558-592`** — `replace_local_daemon` swaps
`Local → Restarting → Local` and fans the new client out to every subscriber:

```rust
    let replacement = DaemonProcess::spawn_configured(executable, exposure.clone())?;
    let client = replacement.client();
    *inner.target.lock() = DaemonTarget::Local(replacement);
    inner
        .client_updates
        .lock()
        .retain(|subscriber| subscriber.send(client.clone()).is_ok());
    Ok(())
```

**`crates/daku-client/src/client.rs:90-96`** — the flag to consult:

```rust
    /// True once the reader thread has ended — the socket closed, the daemon
    /// shut down, or the connection broke. Supervisors poll this to reconnect.
    pub fn is_disconnected(&self) -> bool {
        self.inner.disconnected.load(Ordering::Acquire)
    }
```

**`src/app.rs:91-134`** — why nothing else recovers: the UI parks on
`clients.recv()`, and the only sender is `replace_local_daemon`'s
`client_updates` fan-out.

**`crates/daku-daemon/tests/process.rs:1-60`** — the black-box harness you will
extend. It already has `sandbox_home()`, `ensure_process_home()`,
`spawn_daemon()`, `read_ready()`, and (further down) `pid_alive` /
`wait_until` helpers. `const READY_TIMEOUT: Duration = Duration::from_secs(15);`
and `REBUILD_POLL_INTERVAL` in `process.rs` is 500 ms.

### Constraints you must honor

- **`CONTEXT.md`**: the **Operator** runs daku on their own machine. There is no
  multi-user story — a respawn affects nobody else.
- `docs/agents/git-workflow.md`: no CI. `bun run check` is the gate, and it runs
  `cargo test --workspace`, so any test you add runs on every future commit.
  **It must not be flaky.**
- `DaemonProcess::spawn_configured` is the only way a local daemon is created;
  reuse it via `replace_local_daemon` rather than adding a second spawn path.

## Commands you will need

| Purpose | Command | Expected on success |
|---------|---------|---------------------|
| Full gate | `bun run check` | exit 0 |
| Supervisor tests | `cargo test -p daku-client` | all pass |
| Black-box tests | `cargo test -p daku-daemon --test process` | all pass |

## Scope

**In scope**:
- `crates/daku-client/src/process.rs`
- `crates/daku-daemon/tests/process.rs`

**Out of scope** (do NOT touch):
- `monitor_remote` — plan 018 fixed it and it is the reference implementation.
- `crates/daku-core/src/server.rs` — the daemon closing one connection on error
  is correct behaviour for a thread-per-connection server; the fix belongs on
  the client.
- `src/app.rs` — `listen_dashboard` already re-subscribes on every client it
  receives; once the supervisor sends a replacement, the UI recovers with no
  change.
- `DaemonExposureSettings` and `spawn_configured` — `docs/research/hosted-daemon.md`
  proposes deleting that plumbing; plan 060 corrects that note. Do not start the
  deletion here.
- The `unsafe { set_var("HOME", …) }` in the test harness — that is plan 059.

## Git workflow

- Trunk-based on `main`; **no pull requests, no GitHub Actions**.
- Commit style: imperative, e.g.
  `Respawn the local daemon when its socket drops, not only when it exits (#76).`

## Steps

### Step 1: Treat a dropped socket as needing recovery

In `monitor_daemon`, replace the `process_exited` binding with one that also
consults the client, keeping the same `match` shape:

```rust
        let needs_restart = match &mut *inner.target.lock() {
            // A live child with a dead socket is unrecoverable from the UI's
            // side: `listen_dashboard` only ever gets a new client from
            // `replace_local_daemon`, so respawn rather than sit disconnected.
            DaemonTarget::Local(process) => process.has_exited() || process.client().is_disconnected(),
            DaemonTarget::Restarting(_) => true,
            DaemonTarget::Remote(_) => return,
        };
```

and update the guard below to `if !needs_restart && !executable_changed { continue; }`.

Check `DaemonProcess`'s API first: if `client()` takes `&self` you may need to
narrow the borrow (bind `has_exited()` and `is_disconnected()` in two steps
inside the match arm). Do **not** change `DaemonProcess`'s signatures.

**Verify**: `cargo test -p daku-client` → all pass.
`grep -n "is_disconnected" crates/daku-client/src/process.rs` → matches in both
`monitor_daemon` and `monitor_remote`.

### Step 2: Reset the backoff on a healthy poll

`monitor_remote` resets `backoff = RESTART_BACKOFF_MIN` on every healthy poll
(`process.rs:531`). `monitor_daemon` only resets it after a successful respawn.
Add the same reset in the `if !needs_restart && !executable_changed` branch
before `continue`, so a daemon that recovers and later fails again starts from
the minimum backoff rather than an inherited large one.

**Verify**: `cargo test -p daku-client` → all pass.

### Step 3: Characterize the restart loop

Add to `crates/daku-daemon/tests/process.rs`, reusing its existing helpers:

**Test A — `supervisor_replaces_a_killed_daemon`**: spawn a supervisor with
`DaemonSupervisor::spawn(executable, false)`, take
`supervisor.subscribe_clients()` and drain the initial client, find the child
pid, `kill` it, then `wait_until` (bounded, ≤ 10 s — the poll interval is
500 ms) that the subscriber yields a **new** client whose
`request(Command::Ping)` returns `ResponsePayload::Ack`, and that the old pid is
gone.

**Test B — `supervisor_records_an_error_when_the_daemon_cannot_be_respawned`**:
point the supervisor at a path that exists but is not executable, then assert
`supervisor.last_error()` becomes `Some(_)` within the timeout and that the
monitor thread is still alive (i.e. a second poll still updates it) rather than
having unwound.

Every wait must be a bounded poll loop with an explicit deadline — the file
already uses that pattern; **do not add bare `sleep`s as synchronisation**.

**Verify**: `cargo test -p daku-daemon --test process` → all pass.
Run it **five times in a row** and confirm five clean passes:
`for i in 1 2 3 4 5; do cargo test -p daku-daemon --test process -q || break; done`

### Step 4: Confirm the fix is what the test exercises

Temporarily revert Step 1 (`git stash` the change or comment the
`|| process.client().is_disconnected()`), run Test A, and confirm it **fails**.
Restore Step 1 and confirm it passes. A test that passes both ways is not
testing the fix.

**Verify**: documented in your report — "Test A fails without Step 1".

## Test plan

Covered by Step 3. Structural pattern to follow: the existing
`supervisor_spawns_and_reaps_the_daemon` test in
`crates/daku-daemon/tests/process.rs:161-196` — same `ensure_process_home()`
setup, same `pid_alive` / `wait_until` helpers, same bounded deadlines.

**Verification**: `cargo test -p daku-daemon --test process` → all pass, two
more tests than before; five consecutive clean runs.

## Done criteria

ALL must hold:

- [ ] `bun run check` exits 0
- [ ] `grep -c "is_disconnected" crates/daku-client/src/process.rs` → ≥ `2`
- [ ] `grep -n "process_exited" crates/daku-client/src/process.rs` → no matches
      (renamed to `needs_restart`)
- [ ] `cargo test -p daku-daemon --test process` → all pass, two more tests
      than before
- [ ] Five consecutive runs of `cargo test -p daku-daemon --test process` all
      pass
- [ ] Your report states that Test A fails when Step 1 is reverted
- [ ] `grep -n "std::thread::sleep" crates/daku-daemon/tests/process.rs` → no
      new occurrences used as synchronisation in your tests
- [ ] `git diff --name-only` lists only `crates/daku-client/src/process.rs`,
      `crates/daku-daemon/tests/process.rs` and `plans/README.md`
- [ ] `plans/README.md` status row for 051 updated to DONE

## STOP conditions

Stop and report back (do not improvise) if:

- The excerpts in "Current state" do not match the live code.
- Test A is flaky across the five runs — report the failure mode rather than
  widening the timeout until it passes. A flaky test in `bun run check` is worse
  than no test, because it trains everyone to re-run the gate.
- You find yourself needing to change `DaemonProcess`'s public API, or to add a
  second process-spawn path alongside `replace_local_daemon`.
- Step 4 shows Test A passing without the Step 1 change.
- The respawn loops (kill → respawn → immediately disconnected → respawn …) —
  that means `is_disconnected()` is true on a freshly spawned client and this
  approach needs rethinking. Report it; do not paper over it with a delay.

## Maintenance notes

- **Deliberately deferred**: re-dialling the *running* daemon instead of
  respawning it. A re-dial is gentler (it keeps the daemon's poll cycle and
  SQLite state warm) but needs the daemon's address plumbed to the supervisor
  and a new connect path. Respawn reuses `replace_local_daemon`'s locking and
  fan-out, which are already correct, and a local daemon restart costs one poll
  cycle. Revisit only if respawns turn out to be frequent.
- Watch in review: `replace_local_daemon` drops the old `DaemonProcess` outside
  the target lock (deliberate — teardown can block). Step 1 must not move that.
- If a third `DaemonTarget` variant is ever added, both monitor loops need an
  arm; they currently `return` on the variant they do not own.
