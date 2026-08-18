# Plan 055: A panic in a shared collector cannot silently end polling

> **Executor instructions**: Follow this plan step by step. Run every
> verification command and confirm the expected result before moving to the
> next step. If anything in the "STOP conditions" section occurs, stop and
> report — do not improvise. When done, update the status row for this plan
> in `plans/README.md`.
>
> **Drift check (run first)**:
> `git diff --stat 2bdeaba..HEAD -- crates/daku-core/src/collector.rs crates/daku-daemon/src/main.rs`
> If any in-scope file changed since this plan was written, compare the
> "Current state" excerpts against the live code before proceeding; on a
> mismatch, treat it as a STOP condition.

## Status

- **Priority**: P2
- **Effort**: S
- **Risk**: LOW
- **Depends on**: none
- **Category**: tech-debt
- **Planned at**: commit `2bdeaba`, 2026-08-18

## Why this matters

`CollectorLoop::tick` protects half of itself and not the other half.

The per-Environment groups run under `thread::scope`, every handle is joined,
and a panic is converted into a tick error — good. Then `run_sequential(&self.shared)`
runs `DriftCollector` and `LastCloneCollector` **bare on the collector thread**,
and `run` calls `after()` (which publishes the dashboard) bare too. A panic in
either unwinds straight out of `tick()` and out of `run()`, killing the
collector thread. `spawn_collector_loop` drops the `JoinHandle`, so nothing can
observe that it died.

The daemon then stays up and keeps serving WebSocket clients, and `Hub`'s
dashboard cache keeps replaying the last snapshot to every new subscriber. No
Signal turns red. Polling never resumes until the Operator restarts the daemon.

**Be clear about the impact, because it is smaller than it first looks:** I
found **no reachable panic in the shared collectors today.** The two `.expect`
sites in `drift.rs` fire only on mutex poisoning, which itself requires a prior
panic; the `age_days` era arithmetic needs a year value ServiceNow will not
return. And the Operator is not blind if it happens — the Environment detail
header's "polled … ago" label keeps counting up and tints stale after 300 s.

So this is not a live bug. It is a structural asymmetry: the parallel half of
the tick is already hardened against exactly this, the fix is small, and the
failure mode it prevents is the worst one a monitoring daemon has — appearing
to work while having stopped.

## Current state

**`crates/daku-core/src/collector.rs:238-269`** — protected, then not:

```rust
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
```

**`crates/daku-core/src/collector.rs:271-285`** — `after()` is unprotected too:

```rust
    pub fn run(&self, shutdown: &AtomicBool, clock: &dyn Clock, after: &dyn Fn()) {
        after();
        while !shutdown.load(Ordering::Acquire) {
            if let Err(error) = self.tick() {
                eprintln!("daku collector tick failed: {error}");
            }
            after();
```

**`crates/daku-core/src/collector.rs:301-312`** — the handle is dropped:

```rust
pub fn spawn_collector_loop(
    loop_: CollectorLoop,
    shutdown: Arc<AtomicBool>,
    after_tick: impl Fn() + Send + 'static,
) {
    std::thread::Builder::new()
        .name("daku-collector".into())
        .spawn(move || {
            loop_.run(&shutdown, &SystemClock, &after_tick);
        })
        .expect("spawn collector loop");
}
```

**`crates/daku-core/src/collector.rs:355-370`** — how the shared collectors are
registered (there is already a `register_group` API taking a `Vec`):

```rust
    loop_.register(DriftCollector::new(
        environments.clone(),
        credentials.clone(),
        client.clone(),
        store.clone(),
        interval,
    ));
    loop_.register(LastCloneCollector::new(
        environments,
        credentials,
        client,
        store,
    ));
```

**`crates/daku-core/src/collector.rs:230-236`** — the group API to reuse:

```rust
    /// Registers a set of collectors that run in order on their own thread,
    /// concurrently with the other groups.
    pub fn register_group(&mut self, group: Vec<Box<dyn SignalCollector>>) {
        if !group.is_empty() {
            self.groups.push(group);
        }
    }
```

### Constraints you must honor

- **`plans/README.md` › Ownership locks**: the poll loop belongs to
  `build_default_loop`. Registering the shared collectors differently is inside
  that ownership; restructuring the loop is not.
- **Plan 031's shape must survive**: `groups` are per-Environment. If drift and
  last-clone become a group, that group is not per-Environment — say so in a
  comment so the next reader is not confused. Plan 022's note ("031 must
  preserve the per-Environment group structure") is about the *five* gated
  Signals, not these two.
- **Plan 049 adds an availability gate** to drift and last-clone. Both plans
  touch these two collectors' scheduling; they are independent (this one changes
  *where they run*, 049 changes *whether they probe*), but land one at a time.
- The daemon logs to `~/.daku/daemon.log` via stderr redirection (plan 019). A
  new `eprintln!` reaches that file — that is the right place for a diagnostic.

## Commands you will need

| Purpose | Command | Expected on success |
|---------|---------|---------------------|
| Full gate | `bun run check` | exit 0 |
| Collector tests | `cargo test -p daku-core collector` | all pass |
| Black-box daemon | `cargo test -p daku-daemon --test process` | all pass |

## Scope

**In scope**:
- `crates/daku-core/src/collector.rs`

**Out of scope** (do NOT touch):
- `crates/daku-core/src/drift.rs` and `last_clone.rs` — do **not** go hunting
  for the `.expect` sites to replace. Removing one `.expect` does not fix the
  asymmetry, and plan 049 owns those files.
- `crates/daku-core/src/health.rs` `publish_dashboard` — its `let _ = sink.send(...)`
  is correct (an unbounded channel with a dropped receiver means shutdown).
- `crates/daku-core/src/server.rs` `Hub` and its replay cache — replaying the
  last known state is plan 014's fix and is right.
- Adding a watchdog that restarts the collector thread. That is a bigger
  design decision; this plan makes the death *visible*, not self-healing.

## Git workflow

- Trunk-based on `main`; **no pull requests, no GitHub Actions**.
- Commit style: imperative, e.g.
  `Run drift and last-clone inside the tick's panic-capturing scope (#80).`

## Steps

### Step 1: Move the shared collectors inside the scope

The cleanest fix reuses machinery that already exists rather than adding
`catch_unwind`. In `build_default_loop`, replace the two `loop_.register(...)`
calls with one `loop_.register_group(vec![Box::new(DriftCollector::new(...)), Box::new(LastCloneCollector::new(...))])`,
with a comment saying why this group is not per-Environment:

```rust
    // Drift and last-clone read across every Environment rather than one each,
    // so they form a single group — registered as a group (not as `shared`) so
    // they inherit `tick`'s join-based panic capture like every other group.
```

Then check whether `shared` and `register` still have any callers. If they do
not, delete both — dead code is what `clippy -D warnings` and plan 032 exist to
keep out. If something else still uses them, leave them and say so in your
report.

**Verify**: `cargo test -p daku-core collector` → all pass.
`bun run check` → exit 0 (clippy will flag `shared`/`register` if they became
dead, which is the signal to delete them).

### Step 2: Name the collector in the panic message

`"collector group panicked"` does not say which one. Change the `Err(_)` arm to
include the group index, and — since groups are now a mix of per-Environment and
cross-Environment — give `register_group` an accompanying label so the message
can name it. The smallest version: store `Vec<(String, Vec<Box<dyn SignalCollector>>)>`
and use the label in the error. If that ripples further than a few lines, keep
the index-only version and note it.

**Verify**: `cargo test -p daku-core collector` → all pass.

### Step 3: Make a dead collector thread observable

Keep the `JoinHandle` from `spawn_collector_loop` rather than dropping it.
`spawn_collector_loop` returns `()` today; change it to return the
`JoinHandle<()>` and have `crates/daku-daemon/src/main.rs`'s caller hold it.
Do not join it on the happy path — the daemon must keep serving — but holding it
means a future health check can ask `handle.is_finished()`.

Additionally, wrap the body so an unwind is logged before the thread ends:

```rust
        .spawn(move || {
            loop_.run(&shutdown, &SystemClock, &after_tick);
            // Reached only on shutdown or an unwind out of `run`. Either way the
            // daemon stops polling, so say so in ~/.daku/daemon.log rather than
            // leaving the last snapshot to look current forever.
            eprintln!("daku collector loop ended");
        })
```

**Verify**: `bun run check` → exit 0.

### Step 4: Protect the publish call

In `run`, `after()` is called twice outside any capture. Wrap both in
`std::panic::catch_unwind(std::panic::AssertUnwindSafe(after))` and log an
error on `Err`, so a panic in `publish_dashboard` costs one tick's publish
rather than the whole loop.

**Verify**: `cargo test -p daku-core collector` → all pass.

## Test plan

New tests in `crates/daku-core/src/collector.rs` `mod tests`. The existing tests
already construct a `CollectorLoop` with stub collectors — follow that shape,
and use `TempDb` from `crates/daku-core/src/test_support.rs` for anything
needing a store.

1. `tick_reports_a_panicking_shared_collector_as_an_error` — register a
   collector whose `collect` panics, in the drift/last-clone group; assert
   `tick()` returns `Err` and **does not unwind**. Set a panic hook that
   suppresses output for the duration if the test log gets noisy
   (`std::panic::set_hook` / `take_hook`), and restore it.
2. `tick_still_runs_the_other_collectors_when_one_panics` — a panicking
   collector plus a counting one in a *different* group; assert the counter
   advanced.
3. `run_survives_a_panicking_publish` — an `after` closure that panics on its
   first call and counts on later ones; drive `run` with the existing test clock
   and assert it ticked more than once.

**Verification**: `cargo test -p daku-core collector` → all pass, +3 tests.

## Done criteria

ALL must hold:

- [ ] `bun run check` exits 0
- [ ] `grep -n "run_sequential(&self.shared)" crates/daku-core/src/collector.rs`
      → no matches
- [ ] `grep -n "catch_unwind" crates/daku-core/src/collector.rs` → at least one
      match, around `after()`
- [ ] `grep -n "fn spawn_collector_loop" crates/daku-core/src/collector.rs`
      shows a `JoinHandle` return type
- [ ] `cargo test -p daku-core collector` → all pass, three more tests
- [ ] `cargo test -p daku-daemon --test process` → all pass (the daemon still
      starts and prints its ready line)
- [ ] `git diff --name-only` lists only `crates/daku-core/src/collector.rs`,
      possibly `crates/daku-daemon/src/main.rs`, and `plans/README.md`
- [ ] `plans/README.md` status row for 055 updated to DONE

## STOP conditions

Stop and report back (do not improvise) if:

- The excerpts in "Current state" do not match the live code.
- Moving drift and last-clone into a group changes their **ordering relative to
  the per-Environment groups**. They currently run *after* every group joins,
  which plan 049 relies on (this tick's availability snapshot must already be
  committed when they run). If `register_group` would run them concurrently with
  Availability, **stop** — the ordering guarantee matters more than the panic
  capture, and this plan needs redesigning to keep both.
- Step 2 ripples beyond `collector.rs`.
- Deleting `shared`/`register` breaks a caller outside `collector.rs`.

## Maintenance notes

- The invariant to protect: **every collector runs inside `tick`'s
  `thread::scope`.** If a future Signal is registered any other way, it is
  outside the panic capture again.
- The ordering invariant is now load-bearing for plan 049: drift and last-clone
  must observe this tick's availability snapshot. Whoever changes group
  scheduling must preserve it.
- Deliberately **not** done: restarting the collector thread automatically, and
  surfacing "polling has stopped" on the wire. `handle.is_finished()` is now
  available for whoever builds that; the freshness label is the Operator's
  current signal.
