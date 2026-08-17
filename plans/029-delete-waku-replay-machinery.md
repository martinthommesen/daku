# Plan 029: Delete the inherited waku session/runtime replay machinery from protocol, hub and client

> **Executor instructions**: Follow this plan step by step. Run every
> verification command and confirm the expected result before moving to the
> next step. If anything in the "STOP conditions" section occurs, stop and
> report — do not improvise. When done, update the status row for this plan
> in `plans/README.md` — unless a reviewer dispatched you and told you they
> maintain the index.
>
> **Drift check (run first)**: `git diff --stat f7fdbe7..HEAD -- crates/daku-protocol/src/protocol.rs crates/daku-protocol/src/lib.rs crates/daku-core/src/server.rs crates/daku-core/src/hollow_backend.rs crates/daku-core/src/lib.rs crates/daku-client/src/client.rs crates/daku-client/src/process.rs crates/daku-client/README.md`
> Plan 014 (dashboard replay cache) is a **prerequisite** and will show up in this diff — that is expected. Any *other* change to these files since `f7fdbe7`: compare the "Current state" excerpts against the live code; on a mismatch, STOP.

## Status

- **Priority**: P2
- **Effort**: M
- **Risk**: LOW (only consumer of the wire is this repo's own client; the deleted paths are provably unreachable)
- **Depends on**: plans/011-green-baseline-check-gate.md (gate), plans/014-replay-dashboard-on-subscribe.md (adds the dashboard cache this plan must keep)
- **Category**: tech-debt
- **Planned at**: commit `f7fdbe7`, 2026-08-17
- **Issue**: https://github.com/martinthommesen/daku/issues/48

## Why this matters

daku is a partial fork of waku (ADR-0003). waku's agent domain had per-session "runtimes" that streamed sequenced driver events with epoch/sequence replay cursors and a per-connection response cache. daku stripped the agent domain but kept the plumbing: `Request` still carries `session_id`/`runtime_id` (always nil), the hub keeps a journal it can never fill (`HubState.active_runtimes` is **never inserted anywhere** — `emit()` returns early on its first line, so `EventSink::send*` are no-ops), the client keeps three maps of subscriptions/pending events/last sequences that no code outside `client.rs` reads, and `Command::LoadTaskState` answers with an empty waku workspace model. That is ~350 lines across the three most sensitive files, two mutex-guarded journals per connection, and vocabulary ("epoch", "runtime", "task state") that does not exist in the daku domain (CONTEXT.md: Platform, Environment, Signal, Credential, Operator). The test audit already ruled it "not worth testing — delete instead".

After this plan: `Request { request_id, command }`, `Hub` is subscribers + `broadcast` + the plan-014 dashboard cache, `DaemonClient` is `connect / request / subscribe_dashboard / shutdown`, `Backend::handle(&self, Command)`, and `PROTOCOL_VERSION` is bumped by one (plans 020 and 039 also bump it — always increment the live value, never set a fixed number).

## Current state

All excerpts at `f7fdbe7`; re-read after plan 014 landed (it adds `dashboard: BTreeMap<String, ServerMessage>` to `HubState`, `Hub::publish_dashboard`, a replay loop inside `Hub::subscribe`, `ClientInner.dashboard_cache`, and `ServerMessage::dashboard_cache_key` in the protocol crate — **all of those stay**).

### `crates/daku-protocol/src/protocol.rs`

```rust
// :8
pub const PROTOCOL_VERSION: u32 = 1;
// :28-38
pub enum ClientMessage {
    Hello { protocol_version: u32, token: String, client_id: Uuid,
            #[serde(default)] resume_from: Vec<ReplayCursor> },
    Request(Request),
    Shutdown,
}
// :40-47
pub struct Request { pub request_id: Uuid, pub session_id: Uuid, pub runtime_id: Uuid, pub command: Command }
// :49-56   pub struct ReplayCursor { session_id, runtime_id, epoch, sequence }
// :64-69
pub enum Command { Ping, GetSettings, UpdateSettings { settings: DaemonSettings }, LoadTaskState }
// :71-86   pub struct WireDriverEvent { kind, payload } + impl new
// :88-96   pub struct SequencedEvent { session_id, runtime_id, epoch, sequence, event }
// :147-176 pub enum ServerMessage { Hello{..}, Rejected{..}, Response{..},
//              Event(SequencedEvent), TaskStateChanged { revision: u64 },
//              EnvironmentsUpdated{..}, SignalSnapshotsUpdated{..}, SignalSamplesUpdated{..}, ShuttingDown }
// :195-206
pub enum ResponsePayload {
    Ack,
    Settings { settings: DaemonSettings },
    TaskState { projects: Vec<serde_json::Value>, sessions: Vec<serde_json::Value>,
                default_cwd: PathBuf, projectless_root: Option<PathBuf> },
}
// :1  use std::path::PathBuf;   ← only used by TaskState
```

Tests (`:221-335`): `handshake_field_names_are_stable` (`:226-243`, builds a `ReplayCursor` and asserts `json["resumeFrom"][0]["sequence"] == 9`), three dashboard round-trip tests (keep), `protocol_version_is_daku_domain` (`:332-334`, `assert_eq!(PROTOCOL_VERSION, 1)`).

`crates/daku-protocol/src/lib.rs:29-34` re-exports `ReplayCursor, … SequencedEvent, … WireDriverEvent`.

### `crates/daku-core/src/server.rs`

```rust
// :10-13 imports include ReplayCursor, Request, ResponseOutcome, ResponsePayload, RpcError, SequencedEvent, ServerMessage, WireDriverEvent
// :28-29
const MAX_REPLAY_EVENTS_PER_SESSION: usize = 4096;
const MAX_CACHED_RESPONSES: usize = 2048;
// :45-49
pub trait Backend: Send + Sync + 'static {
    fn handle(&self, request: Request, events: EventSink) -> anyhow::Result<ResponsePayload>;
    fn shutdown(&self) {}
}
// :51-69  pub struct EventSink { session_id, runtime_id, hub } + send / send_ephemeral → hub.emit(...)
// :71-80
struct HubState {
    next_subscriber_id: u64,
    task_state_revision: u64,
    subscribers: HashMap<u64, Sender<ServerMessage>>,
    active_runtimes: HashMap<Uuid, Uuid>,        // never inserted anywhere (grep: only read at :107)
    next_sequences: HashMap<(Uuid, Uuid), u64>,
    journal: HashMap<(Uuid, Uuid), VecDeque<SequencedEvent>>,
    responses: VecDeque<(Uuid, ResponseOutcome)>,
}
// :82-85  struct Hub { epoch: Uuid, state: ParkingMutex<HubState> }   (+ Default impl :87-94)
// :97-103  fn event_sink(..) -> EventSink
// :105-133 fn emit(..)  — first statement: `if state.active_runtimes.get(&session_id) != Some(&runtime_id) { return; }`
// :135-140 fn broadcast(&self, message)                        ← KEEP
// :142-162 fn subscribe(&self, resume_from: &[ReplayCursor], sender) -> u64
//          — loops over state.journal replaying events > cursor, then registers sender
// :164-166 fn unsubscribe                                       ← KEEP
// :168-177 fn task_state_changed(source_subscriber_id)
// :179-194 fn cached_response / cache_response
// :290-298 handle_connection: `let resume_from = match hello { ClientMessage::Hello { protocol_version, token, resume_from, .. } if … => { resume_from } …`
// :339     let subscriber_id = hub.subscribe(&resume_from, outgoing.clone());
// :350-356 dispatch_request(request, outgoing.clone(), subscriber_id, backend.clone(), hub.clone());
// :390-441 fn dispatch_request — reads request.session_id/runtime_id, `notification = request_id.is_nil()`,
//          cached_response / cache_response, hub.event_sink(..), TaskState → hub.task_state_changed(..)
// :522-540 tests: TestBackend implements handle(&self, request: Request, _: EventSink) matching request.command
// :543-547 test handshake_rejects_wrong_protocol_version — asserts only `PROTOCOL_VERSION == 1` + token_matches
// :550-563 test hub_broadcasts_environments_updated — calls hub.subscribe(&[], tx)   ← keep, adjust signature
```

`crates/daku-core/src/lib.rs:22-29` re-exports `ReplayCursor, … SequencedEvent, … WireDriverEvent` and `EventSink`.

### `crates/daku-core/src/hollow_backend.rs`

```rust
// :1   use std::path::PathBuf;
// :4   use daku_protocol::{Command, Request, ResponsePayload};
// :7   use crate::{Backend, EventSink};
// :26  fn handle(&self, request: Request, _: EventSink) -> anyhow::Result<ResponsePayload> {
// :27      match request.command {
// :36-42   Command::LoadTaskState => Ok(ResponsePayload::TaskState { projects: Vec::new(), sessions: Vec::new(),
//              default_cwd: std::env::current_dir().unwrap_or_else(|_| PathBuf::from(".")), projectless_root: None }),
```

### `crates/daku-client/src/client.rs`

```rust
// :1   use std::collections::{HashMap, VecDeque};
// :17-20 imports ReplayCursor, Request, …, SequencedEvent, ServerMessage, WireDriverEvent
// :24  const MAX_BUFFERED_EVENTS_PER_RUNTIME: usize = 4096;
// :31-40
struct ClientInner {
    outgoing: Sender<Outgoing>,
    pending: Mutex<HashMap<Uuid, Sender<Result<ResponsePayload, RpcError>>>>,
    sessions: Mutex<HashMap<(Uuid, Uuid), Sender<SequencedEvent>>>,
    pending_events: Mutex<HashMap<(Uuid, Uuid), VecDeque<SequencedEvent>>>,
    task_state_subscribers: Mutex<Vec<Sender<u64>>>,
    dashboard: Mutex<Vec<Sender<ServerMessage>>>,
    last_sequences: Mutex<HashMap<(Uuid, Uuid), LastSequence>>,
    disconnected: AtomicBool,
}
// :42-46 struct LastSequence { epoch, sequence }
// :54-56  pub fn connect(address, token) -> Self::connect_with_resume(address, token, Vec::new())
// :58-124 pub fn connect_with_resume(address, token, resume_from: Vec<ReplayCursor>) — builds last_sequences from cursors,
//         sends Hello { …, resume_from }, constructs ClientInner (:108-117), spawns run_client
// :126-140 pub fn subscribe(session_id, runtime_id) -> Receiver<SequencedEvent>
// :142-144 pub fn unsubscribe
// :146-150 pub fn subscribe_task_state -> Receiver<u64>
// :152-156 pub fn subscribe_dashboard                                        ← KEEP
// :158-193 pub fn request(&self, session_id: Uuid, runtime_id: Uuid, command) — builds Request { request_id, session_id, runtime_id, command }
// :195-216 pub fn notify(session_id, runtime_id, command)  — nil request id "fire-and-forget"
// :218-230 pub fn last_sequences(&self) -> Vec<ReplayCursor>
// :232-234 pub fn shutdown                                                  ← KEEP
// :288-321 run_client: `ServerMessage::Event(event) => { … last_sequences dedup … sessions / pending_events … }`
// :322-327 `ServerMessage::TaskStateChanged { revision } => task_state_subscribers.retain(...)`
// :328-335 dashboard arm                                                     ← KEEP
// :351-378 disconnect cleanup: fails `pending`, then synthesises a `processExited` WireDriverEvent per session
//          (:358-376), clears task_state_subscribers (:377) and dashboard (:378)
// :437-507 tests: daemon_endpoint_accepts_addresses_and_secure_urls, protocol_dashboard_decodes_* (keep all)
```

Callers outside `client.rs` (grep at `f7fdbe7`): `crates/daku-client/src/process.rs:568` `client.request(Uuid::nil(), Uuid::nil(), Command::GetSettings)` and `:594-600` `client.request(Uuid::nil(), Uuid::nil(), Command::UpdateSettings { .. })`; `src/app.rs:69` `client.subscribe_dashboard()`. **No** caller of `subscribe`, `unsubscribe`, `subscribe_task_state`, `notify`, `last_sequences`, `connect_with_resume` (other than `connect`), or `EventSink::send*`. `crates/daku-client/README.md:3-4` still advertises "subscriptions, replay cursors".

`crates/daku-client/src/lib.rs:12` is `pub use daku_protocol::*;` — removed protocol items vanish from the re-export automatically.

Conventions: `anyhow` errors; `parking_lot::Mutex` in client (no `.unwrap()`), `ParkingMutex` alias in server; tests at file bottom in `mod tests`; imperative commit summaries.

## Commands you will need

| Purpose | Command | Expected on success |
|---------|---------|---------------------|
| Compile everything incl. tests | `cargo check --workspace --all-targets` | exit 0 |
| Protocol tests | `cargo test -p daku-protocol` | all pass |
| Hub tests | `cargo test -p daku-core hub_` | all pass |
| Client tests | `cargo test -p daku-client` | all pass |
| Full gate | `bun run check` | exit 0 |
| Proof of deletion | see Done criteria greps | 0 matches |
| Manual smoke (optional) | `DAKU_UI_FIXTURE=1 bun run dev` | window shows fixture Environments |

## Scope

**In scope**:
- `crates/daku-protocol/src/protocol.rs`, `crates/daku-protocol/src/lib.rs`
- `crates/daku-core/src/server.rs`, `crates/daku-core/src/hollow_backend.rs`, `crates/daku-core/src/lib.rs`
- `crates/daku-client/src/client.rs`, `crates/daku-client/src/process.rs` (only the two `request(...)` call sites)
- `crates/daku-client/README.md` (one sentence)
- `plans/README.md` (status row)

**Out of scope**:
- The plan-014 dashboard cache (`HubState.dashboard`, `publish_dashboard`, `ClientInner.dashboard_cache`, `ServerMessage::dashboard_cache_key`) — keep verbatim.
- `HollowBackend` rename / `StateStore` construction / crate descriptions — plan 033.
- `src/app.rs`, `src/daemon.rs` — no change needed (they only use `subscribe_dashboard` and the supervisor).
- `crates/daku-client/src/persistence.rs`, settings shape — plan 020.
- Any change to `token_matches`, Origin validation, `MAX_WIRE_MESSAGE_BYTES`, `MAX_CONNECTIONS`.

## Git workflow

- Trunk-based on `main`; commit directly (or a disposable local branch merged locally). Do NOT push unless the operator asked.
- One commit is fine: `Delete waku session/runtime replay machinery; protocol v2.`

## Steps

Order the edits so the workspace compiles at the end of each step (protocol first, then its consumers).

### Step 1: Protocol crate

In `crates/daku-protocol/src/protocol.rs`:

1. `PROTOCOL_VERSION` → current value + 1 (read it first; `1` at f7fdbe7).
2. `ClientMessage::Hello`: delete the `resume_from` field (and its `#[serde(default)]`).
3. `Request` → `pub struct Request { pub request_id: Uuid, pub command: Command }`.
4. Delete `ReplayCursor`, `WireDriverEvent` (+ its `impl`), `SequencedEvent`.
5. `Command`: delete `LoadTaskState`.
6. `ServerMessage`: delete `Event(SequencedEvent)` and `TaskStateChanged { revision: u64 }`.
7. `ResponsePayload`: delete `TaskState { .. }`; delete `use std::path::PathBuf;` (`:1`, now unused).
8. Tests: rewrite `handshake_field_names_are_stable` without `resume_from` (assert `json["type"] == "hello"`, `json["protocolVersion"] == PROTOCOL_VERSION`, `json["clientId"]` present, and `json.get("resumeFrom").is_none()`); change `protocol_version_is_daku_domain` (and `crates/daku-core/src/server.rs:544` if it still asserts the constant) to the new value. Add one test `request_carries_only_id_and_command`: serialise `Request { request_id: Uuid::from_u128(7), command: Command::Ping }` and assert the JSON object has exactly the keys `requestId` and `command`.

In `crates/daku-protocol/src/lib.rs:29-34`: remove `ReplayCursor`, `SequencedEvent`, `WireDriverEvent` from the `pub use protocol::{…}` list.

**Verify**: `cargo test -p daku-protocol` → all pass (13 − 0 + 1 = 14 tests at least; the plan-014 `dashboard_cache_key` test also still passes). `cargo check -p daku-core -p daku-client` **fails** at this point — expected until Steps 2–3.

### Step 2: Hub / server

In `crates/daku-core/src/server.rs`:

1. Imports (`:10-13`): drop `ReplayCursor`, `SequencedEvent`, `WireDriverEvent`; keep `Request` only if still referenced (after this step `dispatch_request` still takes `Request` — keep it). Drop `VecDeque` from `std::collections` if unused after deleting `journal`/`responses` (keep `HashMap`, `HashSet`, and plan-014's `BTreeMap`).
2. Delete constants `MAX_REPLAY_EVENTS_PER_SESSION` and `MAX_CACHED_RESPONSES` (`:28-29`).
3. `Backend` trait → `fn handle(&self, command: Command) -> anyhow::Result<ResponsePayload>;` (keep `fn shutdown(&self) {}`). Import `Command` from `daku_protocol`.
4. Delete `EventSink` (struct + impl, `:51-69`).
5. `HubState`: delete `task_state_revision`, `active_runtimes`, `next_sequences`, `journal`, `responses`. Keep `next_subscriber_id`, `subscribers`, and plan-014 `dashboard`.
6. `Hub`: delete the `epoch` field; `impl Default for Hub` becomes derivable — replace `:87-94` with `#[derive(Default)]` on the struct (`state: ParkingMutex<HubState>` — `parking_lot::Mutex<T: Default>` implements `Default`; if the derive does not compile, keep a hand-written `Default` returning `Self { state: ParkingMutex::new(HubState::default()) }`).
7. Delete `event_sink`, `emit`, `task_state_changed`, `cached_response`, `cache_response`.
8. `subscribe(&self, sender: Sender<ServerMessage>) -> u64`: remove the `resume_from` parameter and the journal replay loop (`:144-157`); keep the plan-014 dashboard replay loop and the registration.
9. `handle_connection`: the Hello match no longer binds `resume_from` — replace `let resume_from = match hello { … => { resume_from } … };` with `match hello { ClientMessage::Hello { protocol_version, token, .. } if protocol_version == PROTOCOL_VERSION && token_matches(expected_token, &token) => {} … }` (the two reject arms and the `_ => bail!` arm stay). Then `let subscriber_id = hub.subscribe(outgoing.clone());`.
10. `dispatch_request(request, outgoing, backend)` — drop the `source_subscriber_id` and `hub` parameters and the call-site arguments (`:350-356`); body becomes:

```rust
fn dispatch_request(request: Request, outgoing: Sender<ServerMessage>, backend: Arc<dyn Backend>) {
    std::thread::Builder::new()
        .name("daku-daemon-request".into())
        .spawn(move || {
            let outcome = match backend.handle(request.command) {
                Ok(payload) => ResponseOutcome::Ok { payload },
                Err(error) => ResponseOutcome::Error { error: RpcError::from(error) },
            };
            let _ = outgoing.send(ServerMessage::Response { request_id: request.request_id, outcome });
        })
        .ok();
}
```

(The nil-request-id "notification" path and the response cache go away with it — no client ever sent a nil id after `notify` is deleted in Step 3.)

11. Tests: `TestBackend::handle(&self, command: Command)` matches on `command` directly; delete `handshake_rejects_wrong_protocol_version` (`:543-547` — it asserts a constant; plan 025 adds the real socket test); `hub_broadcasts_environments_updated` → `hub.subscribe(tx)`; adjust the plan-014 replay test the same way.

In `crates/daku-core/src/hollow_backend.rs`: `fn handle(&self, command: Command) -> anyhow::Result<ResponsePayload> { match command { … } }`, delete the `LoadTaskState` arm and the `PathBuf`/`Request`/`EventSink` imports.

In `crates/daku-core/src/lib.rs`: remove `ReplayCursor`, `SequencedEvent`, `WireDriverEvent` from the `pub use daku_protocol::{…}` list (`:22-27`) and `EventSink` from `pub use server::{…}` (`:29`).

**Verify**: `cargo test -p daku-core` → all pass (`hub_` tests included). `cargo check -p daku-daemon` → exit 0 (it only calls `serve` and `HollowBackend::new`, unchanged signatures).

### Step 3: Client

In `crates/daku-client/src/client.rs`:

1. Imports: `use std::collections::HashMap;` (drop `VecDeque`; keep plan-014 `BTreeMap`); drop `ReplayCursor`, `SequencedEvent`, `WireDriverEvent` from the `daku_protocol` import.
2. Delete `MAX_BUFFERED_EVENTS_PER_RUNTIME` and `LastSequence`.
3. `ClientInner`: delete `sessions`, `pending_events`, `task_state_subscribers`, `last_sequences` (keep `outgoing`, `pending`, `dashboard`, plan-014 `dashboard_cache`, `disconnected`); update the constructor at `:108-117`.
4. Fold `connect_with_resume` into `connect(address, token)` (delete `resume_from` handling and the `resume_from` Hello field). `connect` keeps its body otherwise.
5. Delete `subscribe`, `unsubscribe`, `subscribe_task_state`, `notify`, `last_sequences`.
6. `request(&self, command: Command)` — drop the two `Uuid` parameters; build `Request { request_id, command }`.
7. `run_client`: delete the `ServerMessage::Event(..)` and `ServerMessage::TaskStateChanged {..}` arms; in the disconnect cleanup delete the `sessions` loop with the synthetic `processExited` event (`:358-376`) and the `task_state_subscribers.clear()` line; keep failing `pending` and clearing `dashboard`/`dashboard_cache`.
8. Tests: unchanged (they don't touch the deleted API).

In `crates/daku-client/src/process.rs`: `client.request(Command::GetSettings)` (`:568`) and `client.request(Command::UpdateSettings { settings: settings.clone() })` (`:594-600`); drop the `Uuid` import only if nothing else in the file uses it (grep first — `Uuid` may be used elsewhere in process.rs; keep it then).

In `crates/daku-client/README.md:3-4`: change "authenticated WebSocket handshake, request correlation, subscriptions, replay cursors, and local-daemon supervision" to "authenticated WebSocket handshake, request correlation, dashboard subscription, and local-daemon supervision".

**Verify**: `cargo check --workspace --all-targets` → exit 0. `cargo test -p daku-client` → all pass.

### Step 4: Whole-workspace proof

**Verify**: `bun run check` → exit 0. Then the greps in Done criteria all return 0 matches. Optional: `DAKU_UI_FIXTURE=1 bun run dev` shows fixture Environments (the desktop's `GetSettings` request on start still round-trips).

## Test plan

- `daku-protocol`: `request_carries_only_id_and_command` (new), `handshake_field_names_are_stable` (rewritten), `protocol_version_is_daku_domain` (asserts 2).
- `daku-core`: `hub_broadcasts_environments_updated` + plan-014 `hub_replays_latest_dashboard_state_to_late_subscriber` adjusted to `subscribe(tx)`; `handshake_rejects_wrong_protocol_version` deleted (plan 025 replaces it with a real socket test).
- `daku-client`: existing 3+ tests unchanged.
- `cargo test --workspace --no-fail-fast` → 0 failed.

## Done criteria

- [ ] `grep -rn 'ReplayCursor\|SequencedEvent\|WireDriverEvent\|LoadTaskState\|TaskStateChanged\|EventSink\|active_runtimes\|last_sequences\|pending_events\|resume_from\|MAX_REPLAY_EVENTS\|MAX_CACHED_RESPONSES\|MAX_BUFFERED_EVENTS' src crates` → 0 matches
- [ ] `PROTOCOL_VERSION` is exactly one higher than before this plan (`git show HEAD~1:crates/daku-protocol/src/protocol.rs | grep PROTOCOL_VERSION`)
- [ ] `grep -n 'session_id\|runtime_id' crates/daku-protocol/src/protocol.rs crates/daku-client/src/client.rs crates/daku-core/src/server.rs` → 0 matches
- [ ] `grep -n 'fn handle(&self, command: Command)' crates/daku-core/src/server.rs crates/daku-core/src/hollow_backend.rs` → 2 matches
- [ ] `grep -n 'dashboard_cache\|publish_dashboard' crates/daku-core/src/server.rs crates/daku-client/src/client.rs` → still present (plan 014 preserved)
- [ ] `bun run check` exits 0
- [ ] `git status` shows only in-scope files modified
- [ ] `plans/README.md` status row for 029 updated

## STOP conditions

- Plan 014 is not DONE (no `dashboard_cache_key` in `protocol.rs`) — land 014 first; this plan's `subscribe` edits assume its replay loop exists.
- Any caller of `subscribe(`/`notify(`/`last_sequences(`/`subscribe_task_state(`/`EventSink` exists outside `client.rs`/`server.rs` (grep before Step 1) — something started using it; report.
- `Hub` derive `Default` fails and the hand-written fallback in Step 2.6 also fails to compile.
- After Step 3 the desktop no longer reaches `GetSettings` (`process.rs` compile error you cannot resolve by the two mechanical call-site edits).

## Maintenance notes

- Protocol is now v2; a v1 desktop against a v2 daemon (or vice versa) is rejected at Hello with a clear message — the desktop supervises its own daemon, so mixed versions only occur with `DAKU_DAEMON_ADDRESS`; the README does not need a compatibility note.
- Reviewers: check the diff is deletions plus mechanical signature changes; the only behavioural change is "nil request ids are no longer special" (no client sends them).
- Follow-ups: plan 033 renames `HollowBackend` and trims the protocol crate's manifest; plan 025 adds the loopback socket test that replaces the deleted constant-asserting test.
