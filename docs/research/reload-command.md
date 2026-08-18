# Reload / poll-now — decision note

**Question**: the Operator edits `~/.daku/environments.json`, or fixes something
in ServiceNow, and wants daku to notice without relaunching the app. What
mechanism should daku grow — a `Command::Reload`, an in-place collector rebuild,
or a `Command::PollNow`?

Spike for plan 067 / issue #92. Verified against the tree at `cab980e`; claims
cite symbols, not line numbers (`docs/research/hosted-daemon.md` rotted the
other way). Every daku symbol named here is found by `git grep` at that commit.

**Recommendation up front**: **option A, and it needs no new protocol command at
all.** The desktop already owns a working "restart the local daemon" path —
`DaemonClient::shutdown` plus the supervisor's respawn in `monitor_daemon` — and
a measured round trip costs **~0.52 s** from the click to a fresh
`EnvironmentsUpdated` on the desktop's replacement client. A build plan is a UI
affordance and a guard, not a protocol change, not collector surgery.

## 1. Two different wants

| | **Reload config** | **Poll now** |
|---|---|---|
| Trigger | `environments.json` edited: Environment added, removed, relabelled, URL fixed; or a Keychain item corrected | Config is fine; the Operator fixed something *in ServiceNow* and wants the verdict now |
| What must change in the daemon | The `Vec<EnvironmentConfig>` baked into every collector by `build_default_loop`, and the `CollectorLoop` shape itself (one group per Environment) | Nothing. The next tick just has to start early |
| What the desktop sends today | Nothing exists | Nothing exists |
| What the Operator sees | Cards for the new Environment appear; a removed one stops updating | The same cards, restamped |

They are different in one way that matters: **poll-now is a subset of reload**.
A daemon that restarts re-reads config *and* ticks immediately —
`CollectorLoop::run` publishes last-known state and then ticks with no initial
sleep. So any mechanism that reloads also polls now; the reverse is not true.

### This is not the rejected "re-read `poll_interval_secs` every tick"

`plans/README.md` › *Findings considered and rejected* rejects *"Re-reading
`poll_interval_secs` every tick — restart is documented (011/020); not worth the
plumbing."* That rejection is about an **implicit, per-tick, unrequested**
re-read. Everything here is **explicit and Operator-initiated**.

More than that, the rejection's own premise — *restart is documented* — is the
recommendation below. Note the precedent: `poll_interval_secs` is read exactly
twice in the tree, by `start_default_loop` (baked into `CollectorLoop`'s
interval and into `DriftCollector`'s `poll_interval`) and by `run_doctor`. So
`Command::UpdateSettings` **already** is a live command whose effect requires a
restart. Option A does not introduce "change then restart" — it makes the
restart daku already depends on a one-click action instead of an app relaunch.

## 2. The three shapes

### A. Daemon restart — recommended

**What it is**: the desktop calls `DaemonClient::shutdown` on the current
client; `monitor_daemon` sees `has_exited()`, takes `inner.restart`, and
`replace_local_daemon` spawns a fresh `DaemonProcess`, fans the new
`DaemonClient` out to every `subscribe_clients` subscriber. The fresh daemon
runs `start_default_loop` against `default_environments_path()` — new config,
new settings, immediate tick.

**Cost — protocol**: *none*. `ClientMessage::Shutdown` already exists,
`DaemonClient::shutdown` is already public, and `ServerOptions::allow_shutdown`
is `arguments.parent_pid.is_some()` — which `DaemonProcess::spawn_configured`
always passes. `PROTOCOL_VERSION` stays at its current value. This also means
no desktop/daemon lockstep ship.

**Cost — code**: a menu item (or a button) that calls `shutdown` on the
supervisor's client, plus the guard in §4. Nothing in `daku-core` changes.

**Cost — cold caches**: two in-memory caches die with the process.

| Cache | Symbol | Survives restart? | Cold-tick cost |
|---|---|---|---|
| Drift plugin/store-app inventory | `DriftCollector::inventories` (`CachedInventory`, `INVENTORY_REFRESH_SECS` = 30 min) | **No** | 2 extra requests per Environment (`sys_plugins` + `sys_store_app` pages) on the first tick |
| OAuth access token | `CachedToken` in `servicenow.rs` (`MAX_TOKEN_TTL_SECS`, `MIN_TOKEN_TTL_SECS`) | **No** | 1 token POST + 1 Keychain read per `oauth_client_credentials` Environment |
| Instance build string | `fetch_build`, aged out of SQLite via `max_age_secs` | **Yes** | 0 |
| Every Signal snapshot / sample | SQLite (`StateStore`) | **Yes** | 0 |

For a 4-Environment setup that is ≤ 12 extra HTTP requests on one tick, once
per Operator-initiated reload. That is noise next to a normal tick's per-Signal
fan-out, and it is bounded by how often a human clicks.

**Cost — the UI is not blank during it**: `CollectorLoop::run` calls `publish`
*before* its first tick, so the fresh daemon emits `EnvironmentsUpdated` from
SQLite immediately, and `DaemonClient::subscribe_dashboard` replays the
`dashboard_cache` to a subscriber that connects later. The Operator sees the
last-known cards, not an empty window, while the first tick runs.

### B. Rebuild the collector graph in place — rejected

**What it is**: a `Command::Reload` re-reads `environments.json`, calls
`build_default_loop` again, and swaps the new `CollectorLoop` into the running
`spawn_collector_loop` thread.

**Is there a swap point that does not restructure `tick`?** Yes, technically:
`CollectorLoop::tick_timed`'s `std::thread::scope` blocks are joined inside the
call, so no collector thread outlives a tick; the gap between `publish` and
`clock.sleep` is a safe swap point. That is the good news and it is the end of
it.

**Cost**: `CollectorLoop::run` takes `&self`, and `groups`/`shared` are plain
`Vec<Box<dyn SignalCollector>>`. Swapping needs interior mutability (or an
`ArcSwap`-shaped indirection) around both fields, a new owner for the
config path and `CredentialStore`/`StateStore`/`ServiceNowClient` handles so the
loop can rebuild itself, and a reload flag threaded next to `shutdown` — i.e.
exactly the scoped-thread group structure that plans 022 and 031 own. It also
needs a `PROTOCOL_VERSION` bump and a lockstep desktop/daemon ship. Every test
that constructs a `CollectorLoop` (`collector_loop_tick_runs_groups_concurrently`,
`build_default_loop_groups_per_environment`, and the `Clock` fakes
`StopOnSleep` / `StopRecordingSleep` / `StopAfterTwo`) sits on the current
shape.

**What it buys over A**: it keeps the OAuth token and drift inventory warm, and
keeps one SQLite connection. Measured against A's ~0.52 s and ≤ 12 requests,
that is not worth a protocol bump plus surgery on two plans' structure.

### C. Poll-now only — subsumed by A

**What it is**: `Command::PollNow` interrupts the sleep so the next tick starts
immediately. No config reload; `environments.json` edits still need a relaunch.

**Cost**: smaller than B but *not* free. The sleep is `clock.sleep(...)` on the
`Clock` trait, whose only methods are `now` and `sleep`. Making it interruptible
means either widening `Clock` (three test fakes plus `SystemClock`, which also
backs the token TTL arithmetic in `servicenow.rs`) or replacing the flag-poll
loop with a channel-driven wait — and it still needs a new `Command`, a
`PROTOCOL_VERSION` bump, and a lockstep ship.

**How much friction does it remove?** Half, and the cheaper half. It answers
"check now"; it does nothing for the README's three *"relaunch daku after
creating or editing it"* sentences, which are the friction the Operator hits on
day one. And A already delivers poll-now for free, because a fresh daemon ticks
immediately.

**The lazy answer is still the winner — it just is not C.** A is cheaper than C
here (zero new protocol surface versus one command plus a `Clock` change) and
strictly more capable. C only becomes interesting if the cold-cache cost ever
stops being noise, at which point it is an *optimisation of* A, not an
alternative to it.

## 3. The measured number

Harness (throwaway, deleted; it lived as one `#[test]` in
`crates/daku-daemon/tests/`): a sandbox `HOME` with two Environments pointing at
`https://spike-a.invalid` / `https://spike-b.invalid` (RFC 2606 — no egress,
`instance_url_error` accepts them), a real `DaemonSupervisor::spawn` against the
built `daku-daemon`, then eight rounds of `client.shutdown()` → wait for the
replacement on `subscribe_clients` → wait for `EnvironmentsUpdated` on its
`subscribe_dashboard`.

| Leg | Measured |
|---|---|
| Cold `DaemonSupervisor::spawn` → first `EnvironmentsUpdated` | **44 ms** (spawn + `DaemonReady` line + connect + first publish) |
| `shutdown()` → replacement `DaemonClient` | **488–496 ms** in 7 of 8 rounds; one **1.53 s** outlier |
| `shutdown()` → first fresh `EnvironmentsUpdated` | **514–524 ms** in 7 of 8 rounds; **1.55 s** outlier |

Read the decomposition, not the total: the work itself is the 44 ms; the other
~470 ms is `monitor_daemon` sleeping `REBUILD_POLL_INTERVAL` (500 ms) before it
notices the daemon is gone. The ~1 s of extra delay in the outlier matches
`SHUTDOWN_TIMEOUT` (1 s), not a second poll cycle: when the monitor catches
`is_disconnected()` before the child has reaped, `DaemonProcess::stop` waits out
its shutdown deadline before `replace_local_daemon` spawns the replacement.
**Half a second for a click-initiated action is fine**, and the dominant term is
reducible later (§5) without touching the protocol. On a real instance the *first tick* is
slower than a fixture's, but the Operator sees last-known cards throughout —
freshness returns at tick speed, not at restart speed.

Two caveats on the 44 ms. The sandbox SQLite was near-empty, so the publish leg
serialized almost nothing; on a real machine `publish_dashboard` emits
`SignalSnapshotsUpdated` and `SignalSamplesUpdated` sized by what is stored
(≤ ~2 880 sample points per Signal), so that leg grows. And the machine is the
development Mac on `cargo test`'s debug profile. Order-of-magnitude, not a
benchmark.

## 4. Open questions

1. **The action must be gated to a local daemon, and no gate exists.**
   `replace_local_daemon` bails with *"the connected daemon is managed outside
   daku Desktop"* for `DaemonTarget::Remote`, and `monitor_daemon` returns
   outright on `Remote`. If the Operator triggers reload against a daemon
   reached via `DaemonSupervisor::connect`, the daemon shuts down (if it honours
   shutdown at all) and `monitor_remote` re-dials a dead address forever.
   `DaemonSupervisor`'s public surface today is `spawn`, `spawn_configured`,
   `connect`, `last_error`, `client`, `subscribe_clients` — **no local-vs-remote
   predicate**. A build plan must add one and hide or disable the affordance.
2. **Exposed browser clients get dropped.** With `--allow-non-loopback`, a
   restart closes every browser socket. They must re-dial; unlike the desktop
   they have no `subscribe_clients` fan-out. Unknown whether anything downstream
   relies on socket continuity.
3. **What does the Operator see while it happens?** ~0.5 s of stale-but-labelled
   cards. Nothing today distinguishes "reloading" from "connected", and
   `DaemonTarget::Restarting` is private. Does it need a spinner at all at half a
   second?
4. **A malformed `environments.json` turns reload into a silent no-op.**
   `start_default_loop` returns `None` on a parse error, an absent file, or an
   empty list, logging to `~/.daku/daemon.log` — the daemon comes back healthy
   and simply never polls. The reload affordance should probably surface
   `run_doctor`'s verdict rather than let the Operator stare at frozen cards.
   (Note the good half of the same behaviour: a daemon that started with **no**
   config picks one up on restart, which the current relaunch-only flow also
   requires.)
5. **Debounce.** Repeated clicks queue repeated restarts; `inner.restart`
   serialises them but nothing rate-limits the Operator.

## 5. Follow-up plan stubs

None of these has been written; nothing below has landed.

- **Reload affordance (build plan, from this note).** A menu item / button that
  calls `shutdown` on the supervisor's client, gated on a new local-vs-remote
  predicate on `DaemonSupervisor` (open question 1). Includes the README edit
  that replaces the three *"relaunch daku"* sentences. No protocol change, no
  `PROTOCOL_VERSION` bump. Small.
- **Cut the ~470 ms detection lag.** Have the desktop call
  `replace_local_daemon` directly (under `inner.restart`) instead of shutting
  down and waiting for `monitor_daemon`'s 500 ms poll to notice. Optional, and
  it does **not** get to 44 ms: the direct path still drops the old
  `DaemonProcess`, so it pays whatever `DaemonProcess::stop` waits (up to
  `SHUTDOWN_TIMEOUT`, 1 s) for the child to reap. Only worth doing if the half
  second reads as sluggish in practice.
- **Post-reload config feedback.** Surface `run_doctor`'s rows (already built,
  already used by `daku-daemon doctor`) after a reload so a malformed config
  says so (open question 4). Composes with `docs/research/operator-setup.md`,
  which recommends `doctor --fix`.
- **`Command::PollNow`** — only if the cold-cache cost of A ever stops being
  noise. Revisit §2C then, as an optimisation of A.

## 6. What was explicitly *not* done here

- No `Command::Reload`, no `Command::PollNow`, no `PROTOCOL_VERSION` bump — the
  recommendation removes the need for all three.
- No settings UI, no alerting surface (`docs/spec/v1.md` §10).
- No Environments table in SQLite: `~/.daku/environments.json` stays the config
  source of truth (`plans/README.md` › Ownership locks). Option A reads it the
  only way daku ever has — once, at daemon start.
