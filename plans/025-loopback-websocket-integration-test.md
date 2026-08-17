# Plan 025: Cover the daemon's trust boundary and the desktop↔daemon path with a loopback WebSocket integration test

> **Executor instructions**: Follow this plan step by step. Run every
> verification command and confirm the expected result before moving to the
> next step. If anything in the "STOP conditions" section occurs, stop and
> report — do not improvise. When done, update the status row for this plan
> in `plans/README.md` — unless a reviewer dispatched you and told you they
> maintain the index.
>
> **Drift check (run first)**: `git diff --stat f7fdbe7..HEAD -- crates/daku-core/src/server.rs crates/daku-core/src/hollow_backend.rs crates/daku-client/src/client.rs crates/daku-client/Cargo.toml crates/daku-protocol/src/protocol.rs`
> If any of those files changed since this plan was written, compare the
> "Current state" excerpts against the live code before proceeding; on a
> mismatch, treat it as a STOP condition. (Plan 014 legitimately changes
> `server.rs` `subscribe` and `client.rs` `subscribe_dashboard` — see
> "Ordering" below; that drift is expected and the assertions here hold either
> way.)

## Status

- **Priority**: P2
- **Effort**: M
- **Risk**: LOW (additive tests; no production code changes except one renamed test)
- **Depends on**: plans/011-green-baseline-check-gate.md
- **Category**: tests
- **Planned at**: commit `f7fdbe7`, 2026-08-17
- **Issue**: https://github.com/martinthommesen/daku/issues/46

## Why this matters

The daemon's only trust boundary — bearer-token Hello, protocol-version check, `Origin` allowlist, `/v1` path — and the entire desktop↔daemon path (`DaemonClient::connect` → `request` → dashboard fan-out → disconnect) have **zero executable coverage**. The one test named `handshake_rejects_wrong_protocol_version` (`crates/daku-core/src/server.rs:543-547`) asserts `PROTOCOL_VERSION == 1` and `token_matches` on two literals; it never opens a socket. `crates/daku-client/src/client.rs` tests (`:440-507`) are three serde/URL unit tests. With no CI and a red-until-011 baseline, a regression in auth or in the reader loop would ship silently.

`serve()` takes a caller-bound `TcpListener` and an `Arc<AtomicBool>` shutdown, so it runs in-process on `127.0.0.1:0` with no external dependency. One integration test file covers the whole boundary.

## Current state

### `crates/daku-core/src/server.rs` (public surface, verified at HEAD)

```rust
// :195-202
pub fn serve(
    listener: TcpListener,
    auth: String,
    backend: Arc<dyn Backend>,
    shutdown: Arc<AtomicBool>,
    options: ServerOptions,
    dashboard_events: Option<Receiver<ServerMessage>>,
) -> anyhow::Result<()> {
// :30-34
pub struct ServerOptions {
    pub allowed_origins: HashSet<String>,
    pub allow_shutdown: bool,
}
// :45-49
pub trait Backend: Send + Sync + 'static {
    fn handle(&self, request: Request, events: EventSink) -> anyhow::Result<ResponsePayload>;
    fn shutdown(&self) {}
}
```

Behaviour the tests pin (all read at HEAD):

- `serve` sets the listener non-blocking, spawns a `daku-dashboard` thread that forwards every message from `dashboard_events` to `hub.broadcast` (`:209-223`), then loops `listener.accept()` until `shutdown` is set, calls `backend.shutdown()` and returns `Ok(())` (`:226-259`).
- `validate_handshake` (`:443-467`): path must be exactly `/v1` else HTTP **404** `unknown daemon endpoint`; if an `Origin` header is present it must be in `allowed_origins` else **403** `WebSocket origin is not allowed`; no `Origin` header → accepted (native clients).
- After the WebSocket upgrade the first message must be `ClientMessage::Hello`. Wrong `protocol_version` → `ServerMessage::Rejected { message: "protocol N is unsupported; expected 1" }` (`:299-311`); wrong token → `Rejected { message: "authentication failed" }` (`:312-319`); token compare is `subtle::ct_eq` (`:476-478`). Success → `ServerMessage::Hello { protocol_version, daemon_version }` (`:322-328`).
- Connection loop (`:337-386`): `ClientMessage::Request(request)` → `dispatch_request` on a thread → `ServerMessage::Response { request_id, outcome }`; `ClientMessage::Shutdown` → if `options.allow_shutdown` writes `ServerMessage::ShuttingDown`, sets `shutdown` and breaks, else writes `Rejected { message: "daemon shutdown is managed by its service owner" }`.
- The `hub.subscribe(&resume_from, outgoing.clone())` call at `:338` registers the connection for broadcasts (plan 014 adds a replay of cached dashboard messages here — harmless for these tests).
- Existing test module (`:528-564`) has a `TestBackend` answering `Ping → Ack`, `GetSettings → Settings { settings: Default::default() }`, and the two tests `handshake_rejects_wrong_protocol_version` (vacuous) and `hub_broadcasts_environments_updated`.

`daku-core` re-exports what the test needs from `crates/daku-core/src/lib.rs:20-30`: `serve`, `Backend`, `EventSink`, `ServerOptions`, `HollowBackend`, `DaemonSettingsStore`, and the protocol types (`ClientMessage`, `Command`, `PROTOCOL_VERSION`, `Request`, `ResponsePayload`, `ServerMessage`, …).

### `crates/daku-core/src/hollow_backend.rs`

`HollowBackend::new(settings: DaemonSettingsStore, task_store: StateStore) -> anyhow::Result<Self>` opens the store once and answers `Ping → Ack`, `GetSettings → Settings { settings }`, `UpdateSettings → replace + Ack`, `LoadTaskState → TaskState {…}`. Using it in the test needs a temp `DaemonSettingsStore::open(path)` (`crates/daku-core/src/settings.rs:17`) and a temp `StateStore::daemon(path)` (`persistence.rs:107`). Simpler: copy the 12-line `TestBackend` from `server.rs` into the test file — recommended below.

### `crates/daku-client/src/client.rs`

- `DaemonClient::connect(address: &str, token: String) -> anyhow::Result<Self>` (`:54-56`) → `connect_with_resume` (`:58-124`): normalises the address with `daemon_url` (bare `host:port` becomes `ws://host:port/v1`, `:238-249`), performs the WebSocket handshake, sends `Hello`, and maps the reply: `Rejected { message }` → `bail!("daemon rejected connection: {message}")` (`:99`).
- `request(&self, session_id: Uuid, runtime_id: Uuid, command: Command) -> anyhow::Result<ResponsePayload>` (`:158-193`): bails `"daku daemon is disconnected"` once the reader loop has set `disconnected` (`:164-166`, set at `:351`); `REQUEST_TIMEOUT` is 120 s (`:23`).
- `subscribe_dashboard(&self) -> Receiver<ServerMessage>` (`:152-156`) — receives `EnvironmentsUpdated | SignalSnapshotsUpdated | SignalSamplesUpdated` (`:328-335`); on disconnect all dashboard senders are dropped (`:378`) so `recv()` errors.
- `shutdown(&self)` (`:233-235`) sends `ClientMessage::Shutdown` and closes the socket.
- `ServerMessage::ShuttingDown` breaks the reader loop (`:336`) → `disconnected = true`.

`crates/daku-client/Cargo.toml` has **no** `[dev-dependencies]`; its deps are `daku-protocol`, `tungstenite 0.30` (`handshake`, `rustls-tls-native-roots`), `url`, `uuid`, `crossbeam-channel`, `parking_lot`, `serde_json`, `anyhow`. `daku-core` depends only on `daku-protocol` (plus rusqlite/ureq/etc.), so `daku-core` as a dev-dependency of `daku-client` creates **no cycle**.

### `crates/daku-protocol/src/protocol.rs` (names used below, verified)

`PROTOCOL_VERSION: u32 = 1` (`:8`); `ClientMessage::{Hello { protocol_version, token, client_id, resume_from }, Request(Request), Shutdown}` (`:22-38`); `Request { request_id, session_id, runtime_id, command }` (`:40-47`); `Command::{Ping, GetSettings, UpdateSettings { settings }, LoadTaskState}` (`:64-70`); `ServerMessage::{Hello {..}, Rejected { message }, Response {..}, Event(..), TaskStateChanged {..}, EnvironmentsUpdated { environments }, SignalSnapshotsUpdated {..}, SignalSamplesUpdated {..}, ShuttingDown}` (`:151-181`); `ResponsePayload::{Ack, Settings { settings }, TaskState {..}}` (`:198-211`).

### tungstenite 0.30 (already a dep of daku-client)

For the raw-handshake cases use `tungstenite::client::ClientRequestBuilder::new(uri: http::Uri).with_header("Origin", "http://evil.test")` (verified in the registry source, `client.rs:293-335`) with `tungstenite::connect(builder)`. A rejected upgrade surfaces as `Err(tungstenite::Error::Http(response))` where `response.status()` is the HTTP status the server sent. `tungstenite::http` re-exports the `http` crate (`Uri`, `StatusCode`).

Conventions: integration tests live in `crates/<crate>/tests/*.rs` (none exist yet); temp paths via `std::env::temp_dir().join(format!("daku-…-{}", uuid::Uuid::new_v4()))`; imperative commit summaries. If plan 028 has landed, prefer its `TempDb` helper only inside `daku-core` — this file is in `daku-client` and needs no DB.

### Ordering

- Plan 014 changes `Hub::subscribe` (replay of cached dashboard messages) and `DaemonClient::subscribe_dashboard` (replay). Test 6 below pushes **one** dashboard message and asserts it is the **first** message received on `subscribe_dashboard()` — true before and after 014 (before: live broadcast; after: replay or live). Do not assert "exactly one message".
- Plan 029 (delete replay machinery) may remove `session_id`/`runtime_id` from `Request` and `request(...)`'s signature; if it has landed, adapt the `Uuid::nil(), Uuid::nil()` arguments (STOP if the change is not mechanical).

## Commands you will need

| Purpose | Command | Expected on success |
|---------|---------|---------------------|
| New integration test | `cargo test -p daku-client --test loopback` | all pass |
| Server unit tests | `cargo test -p daku-core hub_` and `cargo test -p daku-core token_matches` | pass |
| Whole client crate | `cargo test -p daku-client` | all pass |
| Gate | `bun run check` | exit 0 |

## Scope

**In scope**:
- `crates/daku-client/tests/loopback.rs` (create)
- `crates/daku-client/Cargo.toml` (add `[dev-dependencies] daku-core = { path = "../daku-core" }`)
- `crates/daku-core/src/server.rs` — only the test module: rename/replace `handshake_rejects_wrong_protocol_version` with `token_matches_is_exact` (keep the two `token_matches` asserts, drop the `PROTOCOL_VERSION == 1` line)
- `plans/README.md` (status row)

**Out of scope**:
- Any production code in `server.rs`, `client.rs`, `hollow_backend.rs`, `protocol.rs`. If a test cannot be written without changing them, STOP and report which behaviour is untestable.
- The daemon binary / process supervisor (plan 026).
- Removing the dead journal/replay code (plan 029).

## Git workflow

- Commit on `main`; do not push unless asked. Suggested summary: `Add loopback WebSocket integration test for daemon auth, requests, and dashboard delivery.`

## Steps

### Step 1: Dev-dependency

Append to `crates/daku-client/Cargo.toml`:

```toml
[dev-dependencies]
daku-core = { path = "../daku-core" }
```

**Verify**: `cargo check -p daku-client --tests` → exit 0 (no cycle error).

### Step 2: Test harness in `crates/daku-client/tests/loopback.rs`

Create the file with this shape (fill in exactly; adjust only if compile errors point at a renamed symbol):

```rust
use std::collections::HashSet;
use std::net::TcpListener;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::thread::JoinHandle;
use std::time::Duration;

use anyhow::bail;
use crossbeam_channel::{Receiver, unbounded};
use daku_client::DaemonClient;
use daku_core::{Backend, EventSink, ServerOptions, serve};
use daku_protocol::{Command, Request, ResponsePayload, ServerMessage, PROTOCOL_VERSION};
use uuid::Uuid;

const TOKEN: &str = "loopback-test-token";

struct TestBackend;

impl Backend for TestBackend {
    fn handle(&self, request: Request, _: EventSink) -> anyhow::Result<ResponsePayload> {
        match request.command {
            Command::Ping => Ok(ResponsePayload::Ack),
            Command::GetSettings => Ok(ResponsePayload::Settings { settings: Default::default() }),
            _ => bail!("unexpected command"),
        }
    }
}

struct Daemon {
    address: String,
    shutdown: Arc<AtomicBool>,
    dashboard: crossbeam_channel::Sender<ServerMessage>,
    thread: Option<JoinHandle<anyhow::Result<()>>>,
}

impl Daemon {
    fn start(allow_shutdown: bool, allowed_origins: &[&str]) -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let address = listener.local_addr().unwrap().to_string();
        let shutdown = Arc::new(AtomicBool::new(false));
        let (dashboard, dashboard_rx) = unbounded();
        let options = ServerOptions {
            allowed_origins: allowed_origins.iter().map(|s| s.to_string()).collect::<HashSet<_>>(),
            allow_shutdown,
        };
        let thread = {
            let shutdown = shutdown.clone();
            std::thread::spawn(move || {
                serve(listener, TOKEN.to_owned(), Arc::new(TestBackend), shutdown, options, Some(dashboard_rx))
            })
        };
        Self { address, shutdown, dashboard, thread: Some(thread) }
    }

    fn stop(mut self) -> anyhow::Result<()> {
        self.shutdown.store(true, Ordering::Release);
        self.thread.take().unwrap().join().unwrap()
    }
}

impl Drop for Daemon {
    fn drop(&mut self) {
        self.shutdown.store(true, Ordering::Release);
        if let Some(thread) = self.thread.take() { let _ = thread.join(); }
    }
}

fn recv_within(receiver: &Receiver<ServerMessage>, timeout: Duration) -> Option<ServerMessage> {
    receiver.recv_timeout(timeout).ok()
}
```

(`crossbeam_channel` and `uuid` are regular deps of `daku-client`, so they are available to its integration tests; `anyhow` too.)

**Verify**: `cargo test -p daku-client --test loopback` → compiles, `0 tests`.

### Step 3: Tests — write all eight

Add these `#[test]` functions to `loopback.rs`. Each starts its own `Daemon` (its own port), so they may run in parallel.

1. `wrong_token_is_rejected_at_hello`
   `let daemon = Daemon::start(false, &[]);` `let err = DaemonClient::connect(&daemon.address, "nope".into()).unwrap_err();` assert `err.to_string().contains("authentication failed")`.

2. `correct_token_gets_ack_and_settings`
   Connect with `TOKEN.to_owned()`; `client.request(Uuid::nil(), Uuid::nil(), Command::Ping)` → `matches!(…, ResponsePayload::Ack)`; `Command::GetSettings` → `matches!(…, ResponsePayload::Settings { .. })`.

3. `unknown_path_is_404`
   `tungstenite::connect(format!("ws://{}/nope", daemon.address))` → `Err(tungstenite::Error::Http(response))` with `response.status() == tungstenite::http::StatusCode::NOT_FOUND`. (Match on the error variant; `panic!` on anything else with the debug output.)

4. `disallowed_origin_is_403_and_allowlisted_origin_upgrades`
   `let daemon = Daemon::start(false, &["http://allowed.test"]);`
   - Builder: `tungstenite::client::ClientRequestBuilder::new(format!("ws://{}/v1", daemon.address).parse().unwrap()).with_header("Origin", "http://evil.test")` → `tungstenite::connect(builder)` is `Err(Http(r))` with status `FORBIDDEN`.
   - Same with `"http://allowed.test"` → `Ok((socket, response))` and `response.status() == SWITCHING_PROTOCOLS`; then `socket.close(None)` (ignore the result).

5. `shutdown_is_rejected_unless_allowed`
   - `Daemon::start(false, &[])`, connect, call `client.shutdown()`. The daemon replies `Rejected` (the client ignores it, `client.rs:337`) and **keeps serving**: assert a subsequent `client.request(.., Command::Ping)` from a *fresh* `DaemonClient::connect` still returns `Ack` (the first client's socket was closed by `shutdown()`).
   - `Daemon::start(true, &[])`, connect, `client.shutdown()`; then `daemon.stop()` must return `Ok(())` promptly (`serve` exits because the connection set the shared flag) — wrap in a `std::thread::spawn` + `join` with a 5 s timeout loop if you want a hard bound: simplest is `assert!(daemon.stop().is_ok())`.

6. `dashboard_events_reach_subscribers`
   Connect; `let rx = client.subscribe_dashboard();` then `daemon.dashboard.send(ServerMessage::EnvironmentsUpdated { environments: vec![] }).unwrap();` assert `matches!(recv_within(&rx, Duration::from_secs(5)), Some(ServerMessage::EnvironmentsUpdated { .. }))`. (Holds before and after plan 014.)

7. `daemon_shutdown_disconnects_client`
   Connect; `let rx = client.subscribe_dashboard();` `daemon.stop().unwrap();` — the daemon's `serve` returned, but the per-connection thread only exits when it notices the flag on its next 25 ms poll and drops the socket; so poll: loop up to 5 s until `client.request(Uuid::nil(), Uuid::nil(), Command::Ping)` returns `Err` whose message contains `"disconnected"` **or** `"closed"` (`client.rs:164-166,178-181`); then assert `rx.recv_timeout(Duration::from_secs(5)).is_err()` (all dashboard senders were dropped at `client.rs:378`).

8. `wrong_protocol_version_is_rejected` (replaces the vacuous unit test)
   Raw socket: `let (mut socket, _) = tungstenite::connect(format!("ws://{}/v1", daemon.address)).unwrap();` send `Message::Text(serde_json::to_string(&daku_protocol::ClientMessage::Hello { protocol_version: PROTOCOL_VERSION + 1, token: TOKEN.into(), client_id: Uuid::new_v4(), resume_from: vec![] }).unwrap().into())`; read one `Message::Text` and deserialize to `ServerMessage`; assert `matches!(msg, ServerMessage::Rejected { ref message } if message.contains("unsupported"))`. Add `serde_json` use (regular dep).

**Verify**: `cargo test -p daku-client --test loopback` → `8 passed`. Run it three times in a row (`for i in 1 2 3; do cargo test -p daku-client --test loopback -q || break; done`) → no flakes.

### Step 4: Retire the vacuous unit test

In `crates/daku-core/src/server.rs` test module, rename `handshake_rejects_wrong_protocol_version` to `token_matches_is_exact` and delete the `assert_eq!(PROTOCOL_VERSION, 1);` line (keep both `token_matches` asserts). The real protocol-version check now lives in test 8.

**Verify**: `cargo test -p daku-core token_matches_is_exact` → 1 passed; `grep -n handshake_rejects_wrong_protocol_version crates/daku-core/src/server.rs` → no matches.

### Step 5: Gate

**Verify**: `bun run check` → exit 0.

## Test plan

- `crates/daku-client/tests/loopback.rs`: the eight tests in Step 3 (model the harness on `server.rs` `hub_broadcasts_environments_updated` for channel handling and on `TestBackend` for the backend).
- `crates/daku-core/src/server.rs`: `token_matches_is_exact` (renamed).
- `cargo test --workspace --no-fail-fast` → 0 failed.

## Done criteria

- [ ] `crates/daku-client/tests/loopback.rs` exists with 8 `#[test]` fns; `cargo test -p daku-client --test loopback` → `8 passed`
- [ ] `grep -n 'daku-core' crates/daku-client/Cargo.toml` → 1 match under `[dev-dependencies]` only
- [ ] `grep -n handshake_rejects_wrong_protocol_version crates/daku-core/src/server.rs` → no matches
- [ ] Three consecutive runs of the loopback test pass
- [ ] `bun run check` exits 0
- [ ] `git status` shows only in-scope files modified
- [ ] `plans/README.md` status row for 025 updated

## STOP conditions

- `serve`'s signature, `ServerOptions` fields, or the `Backend` trait differ from the excerpts (plan 029 landing first may change `Request`/`EventSink`; adapt only if the change is a pure field removal — otherwise report).
- `daku-core` cannot be a dev-dependency of `daku-client` (cyclic dependency error) — report; the fallback is placing the test under `crates/daku-core/tests/` with `daku-client` as **its** dev-dependency (also acyclic).
- Test 5 or 7 needs a sleep longer than 5 s to pass, or any test flakes across three runs — report rather than adding sleeps.
- `tungstenite::client::ClientRequestBuilder` is missing (version drift) — report; do not add another WebSocket crate.

## Maintenance notes

- Any change to the Hello/handshake contract (`PROTOCOL_VERSION` bump, new rejection reason, dropping `session_id`/`runtime_id` from `Request`) must update this file first — it is now the executable spec of the boundary.
- Reviewers: check that no test asserts an exact message count on `subscribe_dashboard()` (plan 014's replay would break it) and that every `Daemon` is stopped/dropped so ports are released.
- Deferred: `UpdateSettings` round-trip against a real `HollowBackend` with a temp `DaemonSettingsStore` (add once plan 020 settles the settings shape).
