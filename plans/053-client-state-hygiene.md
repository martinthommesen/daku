# Plan 053: Three client-side honesty fixes — prune removed Environments, close the subscribe gap, mute the detail when disconnected

> **Executor instructions**: Follow this plan step by step. Run every
> verification command and confirm the expected result before moving to the
> next step. If anything in the "STOP conditions" section occurs, stop and
> report — do not improvise. When done, update the status row for this plan
> in `plans/README.md`.
>
> **Drift check (run first)**:
> `git diff --stat 2bdeaba..HEAD -- src/dashboard_state.rs src/app.rs crates/daku-client/src/client.rs`
> If any in-scope file changed since this plan was written, compare the
> "Current state" excerpts against the live code before proceeding; on a
> mismatch, treat it as a STOP condition.

## Status

- **Priority**: P2
- **Effort**: S
- **Risk**: LOW
- **Depends on**: `plans/047-unobserved-environment-is-not-healthy.md`
  (047 also edits `sidebar()`'s `muted`; land 047 first and this is additive)
- **Category**: bug
- **Planned at**: commit `2bdeaba`, 2026-08-18

## Why this matters

Three small, independent defects in the client, all of the same family: the app
presenting something it should not, or losing something it should have kept.

1. **Removed Environments leave their data behind.** `DashboardState::apply`
   replaces `self.environments` on `EnvironmentsUpdated` but only ever *inserts*
   into `snapshots` and `samples`. Remove an Environment from
   `~/.daku/environments.json`, restart the daemon (the app survives that — the
   supervisor respawns it), and add the same id back later: its Signal cards and
   Compare strip immediately render **last session's** snapshots — a green
   "healthy · 142 ms" for an Environment that has not been polled yet — until
   the next tick overwrites them.
2. **`subscribe_dashboard` can drop a message.** It drains the replay cache
   under one lock, releases it, then takes a second lock to register the new
   subscriber. A message the reader thread processes inside that gap lands in
   the cache *after* the drain and fans out to a list that does not yet contain
   the new sender. That is precisely what plan 014's replay exists to prevent,
   and the window is widest exactly when it matters: right after connect or
   reconnect. Cost is up to a full poll interval on a stale or empty sidebar.
   The daemon-side hub does this correctly under one lock; only the client half
   is racy.
3. **Disconnect greys the sidebar and nothing else.** `sidebar()` sets
   `muted: !self.connected` and the dot greys out — but the pane the Operator is
   actually reading keeps rendering the health tag, the reachability tag and
   every Signal card at full saturation from stale state. Two half-signals is
   worse than one: the freshness label is honest ("polled 3 min ago", counting
   up), and the colours contradict it.

## Current state

**`src/dashboard_state.rs:159-195`** — `apply`, with no pruning:

```rust
    pub fn apply(&mut self, message: &ServerMessage) {
        match message {
            ServerMessage::EnvironmentsUpdated { environments } => {
                self.environments = environments.clone();
                if self.selected_id.as_ref().is_none_or(|id| {
                    !self
                        .environments
                        .iter()
                        .any(|environment| &environment.id == id)
                }) {
                    self.selected_id = self
                        .environments
                        .first()
                        .map(|environment| environment.id.clone());
                }
            }
            ServerMessage::SignalSnapshotsUpdated { environment_id, snapshots } => {
                self.snapshots.insert(
                    environment_id.clone(),
                    snapshots.iter().map(|s| (s.signal_id.clone(), s.clone())).collect(),
                );
            }
            ServerMessage::SignalSamplesUpdated { environment_id, signal_id, points } => {
                self.samples
                    .insert((environment_id.clone(), signal_id.clone()), points.clone());
            }
            _ => {}
        }
    }
```

The fields (`src/dashboard_state.rs:81-90`):

```rust
    snapshots: HashMap<String, HashMap<String, SignalSnapshotDto>>,
    samples: HashMap<(String, String), Vec<SamplePoint>>,
```

**`crates/daku-client/src/client.rs:98-112`** — the subscriber side. The
`dashboard_cache` guard is a temporary of the `for` expression and lives for the
loop; it is released **before** `dashboard.lock()` is taken:

```rust
    pub fn subscribe_dashboard(&self) -> Receiver<ServerMessage> {
        let (events, receiver) = unbounded();
        for message in self.inner.dashboard_cache.lock().values() {
            let _ = events.send(message.clone());
        }
        let mut dashboard = self.inner.dashboard.lock();
        // The reader thread flips `disconnected` and then clears this list
        // exactly once; a sender registered after that would never be dropped
        // and `recv()` would block forever instead of letting the caller move
        // to the next client. Checking under the lock closes both orderings.
        if !self.inner.disconnected.load(Ordering::Acquire) {
            dashboard.push(events);
        }
        receiver
    }
```

**`crates/daku-client/src/client.rs:203-211`** — the reader side. **Note the
order: cache first, then dashboard.** Your fix must use the same order.

```rust
                    ServerMessage::EnvironmentsUpdated { .. }
                    | ServerMessage::SignalSnapshotsUpdated { .. }
                    | ServerMessage::SignalSamplesUpdated { .. } => {
                        if let Some(key) = message.dashboard_cache_key() {
                            inner.dashboard_cache.lock().insert(key, message.clone());
                        }
                        inner
                            .dashboard
                            .lock()
                            .retain(|subscriber| subscriber.send(message.clone()).is_ok());
                    }
```

**`src/dashboard_state.rs:350-360`** — the existing muting mechanism:

```rust
                muted: !self.connected,
```

**`src/app.rs:264-266`** — the detail header, which ignores `connected`:

```rust
                                    .child(health_tag(environment.health))
                                    .child(reachability_tag(environment.reachability)),
```

**`src/app.rs:476-524`** — the colour mappings the cards use:

```rust
fn status_color(status: &str, cx: &App) -> gpui::Hsla {
    match status {
        "healthy" => cx.theme().success,
        "degraded" => cx.theme().warning,
        "down" => cx.theme().danger,
        _ => cx.theme().muted_foreground,
    }
}
```

### Constraints you must honor

- **Lock order is cache → dashboard**, set by the reader thread. Taking
  `dashboard` first would invert it and risk deadlock. Do **not** follow any
  advice to "take `dashboard` first".
- **`CONTEXT.md`** › Screen: **Environment detail**, **Signal card**,
  **Compare strip**, **Drill-in**. Use those names.
- `src/app.rs` maps decisions to theme tokens; `src/dashboard_state.rs` makes
  decisions. `SidebarRow.muted` is the existing example — extend that shape.
- `parking_lot::Mutex` is what `daku-client` uses; its guards have no
  poisoning, so holding two across a short critical section is safe.

## Commands you will need

| Purpose | Command | Expected on success |
|---------|---------|---------------------|
| Full gate | `bun run check` | exit 0 |
| Client fold tests | `cargo test -p daku dashboard_state` | all pass |
| Daemon client tests | `cargo test -p daku-client` | all pass |
| Loopback integration | `cargo test -p daku-client --test loopback` | all pass |

## Scope

**In scope**:
- `src/dashboard_state.rs`
- `src/app.rs`
- `crates/daku-client/src/client.rs`

**Out of scope** (do NOT touch):
- `crates/daku-core/src/server.rs` `Hub::subscribe` — the daemon side is already
  correct under one lock.
- `ServerMessage::dashboard_cache_key` in `crates/daku-protocol` — the ordering
  prefixes (`0:` / `1:` / `2:`) are load-bearing for replay order.
- `crates/daku-client/src/process.rs` — plan 051 owns it.
- The 24 h sample retention / pruning in the daemon — that is ADR-0007's and is
  unrelated to the client's in-memory maps.

## Git workflow

- Trunk-based on `main`; **no pull requests, no GitHub Actions**.
- Commit style: imperative. Three independent fixes — three commits preferred,
  e.g. `Drop snapshots for Environments that left the config (#78).`

## Steps

### Step 1: Prune snapshots and samples for Environments that left

In `src/dashboard_state.rs`, in the `EnvironmentsUpdated` arm, after assigning
`self.environments`:

```rust
                let known: HashSet<&str> = self
                    .environments
                    .iter()
                    .map(|environment| environment.id.as_str())
                    .collect();
                self.snapshots.retain(|id, _| known.contains(id.as_str()));
                self.samples.retain(|(id, _), _| known.contains(id.as_str()));
```

Note the borrow: build `known` as owned `String`s if the borrow checker
complains about holding `&str` into `self.environments` while mutating
`self.snapshots`. Correctness first, cleverness never.

**Verify**: `cargo test -p daku dashboard_state` → all pass.

### Step 2: Close the subscribe gap with a single critical section

In `crates/daku-client/src/client.rs`, rewrite `subscribe_dashboard` to hold
**both** guards, acquired in the reader thread's order (cache, then dashboard),
across both the drain and the push:

```rust
    pub fn subscribe_dashboard(&self) -> Receiver<ServerMessage> {
        let (events, receiver) = unbounded();
        // Same lock order as the reader thread (cache, then dashboard) so the
        // two can never deadlock, and held across both the replay drain and the
        // registration so a message processed in between cannot slip past a
        // subscriber that is about to exist.
        let cache = self.inner.dashboard_cache.lock();
        let mut dashboard = self.inner.dashboard.lock();
        for message in cache.values() {
            let _ = events.send(message.clone());
        }
        // The reader thread flips `disconnected` and then clears this list
        // exactly once; a sender registered after that would never be dropped
        // and `recv()` would block forever instead of letting the caller move
        // to the next client. Checking under the lock closes both orderings.
        if !self.inner.disconnected.load(Ordering::Acquire) {
            dashboard.push(events);
        }
        receiver
    }
```

Keep the existing `disconnected` check and its comment — that is plan 014's fix
and is still needed.

**Verify**: `cargo test -p daku-client` → all pass.
`cargo test -p daku-client --test loopback` → all pass (this is the test that
would hang on a deadlock; if it hangs, you have the lock order wrong — see STOP
conditions).

### Step 3: Mute the Environment detail when disconnected

1. In `src/dashboard_state.rs`, add `pub muted: bool` to `SignalCard` and set it
   from `!self.connected` in the card builder, mirroring `SidebarRow`.
2. Add a public accessor for the detail header if one does not exist —
   `pub fn connected(&self) -> bool { self.connected }`.
3. In `src/app.rs`: skip both `health_tag` and `reachability_tag` when
   `!self.state.connected()` (the header keeps the label, the URL and the
   freshness line, which stays honest); and in the Signal card, use
   `cx.theme().muted_foreground` instead of `status_color(...)` when
   `card.muted`.

**Verify**: `cargo test -p daku` → all pass. `bun run check` → exit 0.

## Test plan

New tests in `src/dashboard_state.rs` `mod tests` (model on the existing
`dashboard_state_*` tests and the `summary(...)` / `snap(...)` helpers around
`src/dashboard_state.rs:697-830`):

1. `removing_an_environment_drops_its_snapshots` — apply
   `EnvironmentsUpdated` with prod+test, then `SignalSnapshotsUpdated` for
   `test`, then `EnvironmentsUpdated` with prod only, then
   `EnvironmentsUpdated` with prod+test again. Assert `test`'s Signal cards are
   all `WAITING` — the old data must not resurrect.
2. `removing_an_environment_drops_its_samples` — same shape for
   `SignalSamplesUpdated`, asserting the sparkline is empty.
3. `signal_cards_are_muted_while_disconnected` — connected state with snapshots
   → `!card.muted`; `set_connected(false)` → every card `muted`.

One test in `crates/daku-client/src/client.rs` `mod tests`:

4. `subscribe_dashboard_replays_the_cache_and_registers_atomically` — a
   behavioural smoke test is enough here (the race itself is not
   deterministically reproducible without instrumentation): assert that a
   subscriber taken after two cached messages receives both, in
   `dashboard_cache_key` order, and that a subscriber taken after
   `disconnected` is set receives a channel that closes rather than blocking.
   **State honestly in your report that this test pins the invariant, not the
   race window.**

**Verification**: `cargo test -p daku dashboard_state` → all pass, +3 tests.
`cargo test -p daku-client` → all pass, +1 test.

## Done criteria

ALL must hold:

- [ ] `bun run check` exits 0
- [ ] `grep -n "retain" src/dashboard_state.rs` shows retains on both
      `self.snapshots` and `self.samples` inside the `EnvironmentsUpdated` arm
- [ ] In `crates/daku-client/src/client.rs`, `subscribe_dashboard` binds
      `dashboard_cache.lock()` **before** `dashboard.lock()` and both live to
      the end of the function
- [ ] `grep -n "pub muted" src/dashboard_state.rs` → two matches
      (`SidebarRow`, `SignalCard`)
- [ ] `cargo test -p daku-client --test loopback` completes (does not hang)
- [ ] `cargo test -p daku dashboard_state` → all pass, three more tests
- [ ] `git diff --name-only` lists only the three in-scope files and
      `plans/README.md`
- [ ] `plans/README.md` status row for 053 updated to DONE

## STOP conditions

Stop and report back (do not improvise) if:

- The excerpts in "Current state" do not match the live code.
- **Any test hangs after Step 2.** That is a deadlock, not a slow test. Revert
  Step 2 immediately and report — do not "fix" it by dropping a guard early or
  by reordering to dashboard-first.
- You find a third site that takes both `dashboard_cache` and `dashboard`, with
  a different order than the reader thread's.
- Step 3's header change breaks the layout in a way you cannot fix inside
  `src/app.rs`.

## Maintenance notes

- **The lock-order rule is now load-bearing**: everything that takes both
  `dashboard_cache` and `dashboard` takes cache first. Any new site must follow
  it; that is the thing to check in review.
- `muted` now means "do not trust these colours" in two places. **Plan 047 has
  already made `SidebarRow.muted` two-valued (`!connected || never observed`);
  do NOT refactor it to an enum in this plan.** `SignalCard.muted` is
  `!connected` only. If a third reason to mute ever appears, that is the point
  to replace the bool with a small enum — in its own change, not here.
- Test 4 documents a limitation rather than hiding it. If the race is ever
  observed in practice, the honest next step is instrumentation, not a wider
  timeout.
