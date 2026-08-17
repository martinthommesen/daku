# Plan 014: Late subscribers get the current dashboard immediately (no blank UI until the next tick)

> **Executor instructions**: Follow this plan step by step. Run every
> verification command and confirm the expected result before moving to the
> next step. If anything in the "STOP conditions" section occurs, stop and
> report — do not improvise. When done, update the status row for this plan
> in `plans/README.md` — unless a reviewer dispatched you and told you they
> maintain the index.
>
> **Drift check (run first)**: `git diff --stat f7fdbe7..HEAD -- crates/daku-protocol/src/protocol.rs crates/daku-core/src/server.rs crates/daku-core/src/collector.rs crates/daku-client/src/client.rs`
> If any of those files changed since this plan was written, compare the
> "Current state" excerpts against the live code before proceeding; on a
> mismatch, treat it as a STOP condition.

## Status

- **Priority**: P1
- **Effort**: S
- **Risk**: LOW
- **Depends on**: plans/011-green-baseline-check-gate.md (gate only)
- **Category**: bug / perf
- **Planned at**: commit `f7fdbe7`, 2026-08-17
- **Issue**: https://github.com/martinthommesen/daku/issues/36

## Why this matters

The daemon only **broadcasts** dashboard state (`EnvironmentsUpdated`, `SignalSnapshotsUpdated`, `SignalSamplesUpdated`) to subscribers present at the moment the collector tick finishes; nothing is cached or replayed. Three consequences:

1. On app start the collector thread starts (and can finish its first tick in milliseconds when it fails fast: no Keychain item, offline, DNS error) **before** `serve` accepts the desktop's connection (`crates/daku-daemon/src/main.rs:55-61`) — that broadcast is lost and the sidebar stays empty for up to `poll_interval_secs` (120 s default).
2. Any client that connects to an already-running daemon (`DAKU_DAEMON_ADDRESS` remote mode, a restarted desktop) sees "No Environment selected." until the next tick.
3. On the client side, `DaemonClient` also drops dashboard messages that arrive before `subscribe_dashboard()` is called — which happens in `src/app.rs:69` only after the window is created — so even a server-side replay on Hello would be lost.

Fix: cache the latest dashboard message per key on both hub and client, replay on subscribe, and publish once from SQLite before the first tick so a relaunch shows last-known state instantly.

## Current state

### `crates/daku-protocol/src/protocol.rs`

`ServerMessage` variants relevant here (names exact; fields as used below): `EnvironmentsUpdated { environments: Vec<EnvironmentSummary> }`, `SignalSnapshotsUpdated { environment_id: String, snapshots: Vec<SignalSnapshotDto> }`, `SignalSamplesUpdated { environment_id: String, signal_id: String, points: Vec<SamplePoint> }`. The crate is "serde envelopes only" — the helper added below is a pure function, no I/O.

### `crates/daku-core/src/server.rs`

```rust
// :70-80
#[derive(Default)]
struct HubState {
    next_subscriber_id: u64,
    task_state_revision: u64,
    subscribers: HashMap<u64, Sender<ServerMessage>>,
    active_runtimes: HashMap<Uuid, Uuid>,
    next_sequences: HashMap<(Uuid, Uuid), u64>,
    journal: HashMap<(Uuid, Uuid), VecDeque<SequencedEvent>>,
    responses: VecDeque<(Uuid, ResponseOutcome)>,
}
```

```rust
// :135-140
    fn broadcast(&self, message: ServerMessage) {
        let mut state = self.state.lock();
        state
            .subscribers
            .retain(|_, subscriber| subscriber.send(message.clone()).is_ok());
    }

// :142-162  (subscribe: replays only the per-session `journal`, then registers the sender)
    fn subscribe(&self, resume_from: &[ReplayCursor], sender: Sender<ServerMessage>) -> u64 {
        let mut state = self.state.lock();
        for (&(session_id, runtime_id), events) in &state.journal { … let _ = sender.send(ServerMessage::Event(event.clone())); … }
        let id = state.next_subscriber_id;
        state.next_subscriber_id = state.next_subscriber_id.saturating_add(1);
        state.subscribers.insert(id, sender);
        id
    }
```

The dashboard thread in `serve` (`:209-223`) does `Ok(message) => hub.broadcast(message)` for every message from the collector's channel. Per-connection code calls `hub.subscribe(&resume_from, outgoing.clone())` right after Hello (`:338`). Existing hub test: `hub_broadcasts_environments_updated` (`:550-563`) — model the new test on it. `std::collections::{HashMap, HashSet, VecDeque}` are imported at `:1`.

### `crates/daku-core/src/collector.rs`

```rust
// :73-84
    pub fn run(&self, shutdown: &AtomicBool, clock: &dyn Clock, after: &dyn Fn()) {
        while !shutdown.load(Ordering::Acquire) {
            if let Err(error) = self.tick() {
                eprintln!("daku collector tick failed: {error}");
            }
            after();
            if shutdown.load(Ordering::Acquire) {
                break;
            }
            clock.sleep(self.interval);
        }
    }
```

`after` is the closure that calls `publish_dashboard` (`:189-201`), which reads everything from SQLite — so calling it before the first tick publishes last-known state from the previous run (or nothing on a fresh DB, which is harmless). Test `collector_loop_run_invokes_after_tick` (`:297-315`) uses a `StopOnSleep` clock that flips `shutdown` inside `sleep`, and asserts `after` was called.

### `crates/daku-client/src/client.rs`

```rust
// :30-39
struct ClientInner {
    outgoing: Sender<Outgoing>,
    pending: …,
    sessions: …,
    pending_events: …,
    task_state_subscribers: Mutex<Vec<Sender<u64>>>,
    dashboard: Mutex<Vec<Sender<ServerMessage>>>,
    last_sequences: …,
    disconnected: AtomicBool,
}
// :112-116  ClientInner is constructed with `dashboard: Mutex::new(Vec::new()),`
// :152-156
    pub fn subscribe_dashboard(&self) -> Receiver<ServerMessage> {
        let (events, receiver) = unbounded();
        self.inner.dashboard.lock().push(events);
        receiver
    }
// :328-335 (reader loop)
                    ServerMessage::EnvironmentsUpdated { .. }
                    | ServerMessage::SignalSnapshotsUpdated { .. }
                    | ServerMessage::SignalSamplesUpdated { .. } => {
                        inner
                            .dashboard
                            .lock()
                            .retain(|subscriber| subscriber.send(message.clone()).is_ok());
                    }
// :378  on disconnect: inner.dashboard.lock().clear();
```

`Mutex` here is `parking_lot::Mutex` (no `.unwrap()`). Client tests (`:440-507`) are serde decode tests; there is no in-process socket test — the new client behaviour is covered by a unit test on `ClientInner` construction only if cheap; otherwise rely on the hub test + manual run.

### `src/dashboard_state.rs`

`DashboardState::apply` (`:83-121`) treats the three messages independently (maps keyed by env / (env, signal)); `EnvironmentsUpdated` sets the selection. Replay order: `EnvironmentsUpdated` first, then the rest — the key scheme below sorts that way.

## Commands you will need

| Purpose | Command | Expected on success |
|---------|---------|---------------------|
| Protocol tests | `cargo test -p daku-protocol dashboard_cache_key` | pass |
| Hub tests | `cargo test -p daku-core hub_` | all pass |
| Collector tests | `cargo test -p daku-core collector_loop_run` | all pass |
| Client build | `cargo test -p daku-client` | all pass |
| Gate | `bun run check` | exit 0 |
| Manual smoke (optional) | `DAKU_UI_FIXTURE=1 bun run dev` | app shows fixture Environments |

## Scope

**In scope**:
- `crates/daku-protocol/src/protocol.rs` — add `pub fn dashboard_cache_key(&ServerMessage) -> Option<String>` + test
- `crates/daku-core/src/server.rs` — `HubState.dashboard` cache, `publish_dashboard`, replay in `subscribe`, test
- `crates/daku-core/src/collector.rs` — call `after()` once before the first tick + test tweak
- `crates/daku-client/src/client.rs` — `dashboard_cache`, replay in `subscribe_dashboard`
- `plans/README.md` (status row)

**Out of scope**:
- `PROTOCOL_VERSION` — no wire change (same messages, just replayed).
- Removing the dead `journal`/`active_runtimes` replay machinery (separate tech-debt plan). Leave it in place.
- `src/app.rs` — no UI change needed.
- Reconnect logic for remote daemons.

## Git workflow

- Commit on `main`; do not push unless asked. Suggested summary: `Replay the latest dashboard state to new subscribers and publish before the first tick.`

## Steps

### Step 1: Shared cache key in the protocol crate

In `crates/daku-protocol/src/protocol.rs` add (below `ServerMessage`):

```rust
impl ServerMessage {
    /// Cache key for "latest dashboard state" replay. `EnvironmentsUpdated`
    /// sorts first so a replaying client sets its selection before snapshots
    /// and samples arrive. `None` for non-dashboard messages.
    pub fn dashboard_cache_key(&self) -> Option<String> {
        match self {
            Self::EnvironmentsUpdated { .. } => Some("0:environments".to_owned()),
            Self::SignalSnapshotsUpdated { environment_id, .. } => {
                Some(format!("1:snapshots:{environment_id}"))
            }
            Self::SignalSamplesUpdated { environment_id, signal_id, .. } => {
                Some(format!("2:samples:{environment_id}:{signal_id}"))
            }
            _ => None,
        }
    }
}
```

Add a test in the file's existing `mod tests`: keys for the three variants are `Some(..)` and distinct, `Some("0:environments")` sorts before a snapshots key; `ServerMessage::ShuttingDown.dashboard_cache_key()` is `None`.

**Verify**: `cargo test -p daku-protocol dashboard_cache_key` → 1 passed.

### Step 2: Hub caches and replays

In `crates/daku-core/src/server.rs`:

- Add `use std::collections::BTreeMap;` (extend the existing `std::collections` import).
- Add to `HubState`: `dashboard: BTreeMap<String, ServerMessage>,`.
- Add to `impl Hub`:

```rust
    /// Broadcasts a dashboard message and remembers it for late subscribers.
    fn publish_dashboard(&self, message: ServerMessage) {
        let mut state = self.state.lock();
        if let Some(key) = message.dashboard_cache_key() {
            state.dashboard.insert(key, message.clone());
        }
        state
            .subscribers
            .retain(|_, subscriber| subscriber.send(message.clone()).is_ok());
    }
```

- In `subscribe`, immediately before `let id = state.next_subscriber_id;`, add:

```rust
        for message in state.dashboard.values() {
            let _ = sender.send(message.clone());
        }
```

- In `serve`'s dashboard thread (`:217`), change `Ok(message) => hub.broadcast(message),` to `Ok(message) => hub.publish_dashboard(message),`. Leave `broadcast` for any other callers (grep `hub.broadcast` — if it then has no callers, keep it anyway; it is used by the existing test).

Add test after `hub_broadcasts_environments_updated`:

```rust
    #[test]
    fn hub_replays_latest_dashboard_state_to_late_subscriber() {
        let hub = Hub::default();
        hub.publish_dashboard(ServerMessage::SignalSnapshotsUpdated {
            environment_id: "prod".into(),
            snapshots: vec![],
        });
        hub.publish_dashboard(ServerMessage::EnvironmentsUpdated { environments: vec![] });
        hub.publish_dashboard(ServerMessage::EnvironmentsUpdated { environments: vec![] }); // newer replaces older
        let (tx, rx) = unbounded();
        hub.subscribe(&[], tx);
        let replayed: Vec<ServerMessage> = rx.try_iter().collect();
        assert_eq!(replayed.len(), 2);
        assert!(matches!(replayed[0], ServerMessage::EnvironmentsUpdated { .. }));
        assert!(matches!(replayed[1], ServerMessage::SignalSnapshotsUpdated { .. }));
    }
```

**Verify**: `cargo test -p daku-core hub_` → 2 passed.

### Step 3: Publish before the first tick

In `crates/daku-core/src/collector.rs` `run`, add `after();` as the first statement of the function (before the `while`), with the comment `// Publish last-known state from SQLite so a fresh subscriber is not blank until the first tick completes.`

Update `collector_loop_run_invokes_after_tick` to count calls: replace the `called: AtomicBool` with an `AtomicUsize` counter and assert `== 2` (one initial publish + one after the single tick before `StopOnSleep` stops the loop). Keep the test name or rename to `collector_loop_run_publishes_before_and_after_tick`.

**Verify**: `cargo test -p daku-core collector_loop_run` → pass.

### Step 4: Client caches and replays

In `crates/daku-client/src/client.rs`:

- Add `use std::collections::BTreeMap;` if not present.
- Add to `ClientInner`: `dashboard_cache: Mutex<BTreeMap<String, ServerMessage>>,` and initialise with `Mutex::new(BTreeMap::new())` where `ClientInner` is constructed (`:112-116`).
- In the reader loop's dashboard arm (`:328-335`), before the `retain`, insert:

```rust
                        if let Some(key) = message.dashboard_cache_key() {
                            inner.dashboard_cache.lock().insert(key, message.clone());
                        }
```

- Change `subscribe_dashboard` to replay first:

```rust
    pub fn subscribe_dashboard(&self) -> Receiver<ServerMessage> {
        let (events, receiver) = unbounded();
        for message in self.inner.dashboard_cache.lock().values() {
            let _ = events.send(message.clone());
        }
        self.inner.dashboard.lock().push(events);
        receiver
    }
```

- In the disconnect cleanup (`:378`, next to `inner.dashboard.lock().clear();`) add `inner.dashboard_cache.lock().clear();`.

Take the lock order into account: never hold `dashboard_cache` and `dashboard` locks simultaneously (the code above does not).

**Verify**: `cargo test -p daku-client` → all pass; `cargo check --workspace` → exit 0.

### Step 5: Gate + optional smoke

**Verify**: `bun run check` → exit 0. Optional: `DAKU_UI_FIXTURE=1 bun run dev` still shows the fixture Environments; with a real `~/.daku/environments.json`, relaunching the app shows last-known state within a second instead of after ~2 minutes.

## Test plan

- `daku-protocol`: `dashboard_cache_key_orders_environments_first` (Step 1).
- `daku-core`: `hub_replays_latest_dashboard_state_to_late_subscriber` (Step 2), `collector_loop_run_publishes_before_and_after_tick` (Step 3).
- `daku-client`: no new socket test (none exists yet — a loopback integration test is a separate backlog item); the change is compile-checked and covered by the manual smoke.
- `cargo test --workspace --no-fail-fast` → 0 failed.

## Done criteria

- [ ] `grep -n 'dashboard_cache_key' crates/daku-protocol/src/protocol.rs crates/daku-core/src/server.rs crates/daku-client/src/client.rs` → definition + 2 call sites (+ tests)
- [ ] `grep -n 'hub.publish_dashboard' crates/daku-core/src/server.rs` → 1 match in `serve`
- [ ] `cargo test -p daku-core hub_` → 2 passed; `collector_loop_run` test asserts 2 calls
- [ ] `bun run check` exits 0
- [ ] `git status` shows only in-scope files modified
- [ ] `plans/README.md` status row for 014 updated

## STOP conditions

- `HubState`/`subscribe`/`run`/`subscribe_dashboard` no longer match the excerpts (e.g. the dead replay machinery was already removed and `subscribe` has a different signature — adapt only if the change is mechanical; otherwise report).
- `ServerMessage` variant field names differ from those used in `dashboard_cache_key`.
- Any existing test fails after Step 3 for a reason other than the `after` call count.

## Maintenance notes

- If snapshots ever become deltas instead of full per-Environment lists, the cache must merge instead of replace — revisit `publish_dashboard`.
- If an Environment is removed from `environments.json`, its stale `1:snapshots:<id>` / `2:samples:*` entries remain in the cache until daemon restart (the config is only read at start anyway). Clear the cache on `EnvironmentsUpdated` if config reload is added later.
- Reviewers: check that replay happens **before** the subscriber is registered (no duplicate/lost message window) and that no lock is held across a blocking send (crossbeam `unbounded` sends never block).
