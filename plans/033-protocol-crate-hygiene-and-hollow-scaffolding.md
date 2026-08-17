# Plan 033: Make `daku-protocol` a pure wire crate and retire the "hollow" scaffolding names

> **Executor instructions**: Follow this plan step by step. Run every
> verification command and confirm the expected result before moving to the
> next step. If anything in the "STOP conditions" section occurs, stop and
> report — do not improvise. When done, update the status row for this plan
> in `plans/README.md` — unless a reviewer dispatched you and told you they
> maintain the index.
>
> **Drift check (run first)**: `git diff --stat f7fdbe7..HEAD -- crates/daku-protocol crates/daku-core/src/hollow_backend.rs crates/daku-core/src/lib.rs crates/daku-core/src/settings.rs crates/daku-core/README.md crates/daku-core/Cargo.toml crates/daku-daemon/src/main.rs crates/daku-daemon/Cargo.toml crates/daku-client/src/lib.rs crates/daku-client/src/process.rs scripts/delete-debug-app.ts`
> Plans 020, 029, 030 are prerequisites and WILL appear here. Read the live
> files; the excerpts below are from `f7fdbe7` and each step says what those
> plans already changed. Any *other* mismatch → STOP.

## Status

- **Priority**: P3
- **Effort**: M
- **Risk**: LOW
- **Depends on**: plans/011 (gate), plans/020 (settings shape), plans/029 (replay machinery gone), plans/030 (i18n gone from the protocol crate)
- **Category**: tech-debt
- **Planned at**: commit `f7fdbe7`, 2026-08-17
- **Issue**: https://github.com/martinthommesen/daku/issues/56

## Why this matters

`crates/daku-protocol/README.md` promises "Serde envelopes only — no database, filesystem, or socket I/O", yet the crate depends on `dirs` (computes `~/.daku/settings.json`), `rust-i18n` + macOS `objc2-foundation` (system locale), and hosts UI preferences (`theme.rs`, `i18n.rs`). A future non-macOS or web client cannot consume the wire contract without dragging ObjC bindings. `crates/daku-client/src/lib.rs` re-exports the whole crate with a glob while the root crate also depends on `daku-protocol` directly, so the same types reach the app by two paths.

Meanwhile the daemon still calls itself a placeholder: `HollowBackend` (the only backend — it serves settings), a `Command::LoadTaskState`/`TaskState` waku workspace model (deleted by plan 029), a stub `export_types` binary that prints "disabled in the hollow stack", crate descriptions "Headless provider, persistence, and workspace runtime" / "Headless daku provider daemon", `main.rs` constructing three `StateStore`s for one path, `HollowBackend::new` taking a `StateStore` only to open and drop it, and `DaemonProcess::spawn`/`DaemonSupervisor::spawn` constructors nobody calls. None of it breaks anything; all of it tells a reader the daemon is unfinished and hides what the crates actually are.

## Current state (at `f7fdbe7`; see per-step notes for what 020/029/030 already changed)

### `crates/daku-protocol/Cargo.toml`

```toml
[dependencies]
anyhow = "1.0"          # used: protocol.rs:213 impl From<anyhow::Error> for RpcError  → KEEP
dirs = "6.0"            # used only by settings.rs:31 DaemonSettings::default_path()  → move to daku-core
rust-i18n = "4"         # i18n.rs/theme.rs/lib.rs                                    → plan 030 removes
serde = { version = "1.0", features = ["derive"] }
serde_json = "1.0"
uuid = { version = "1.18", features = ["v4", "serde"] }   # protocol.rs Uuid (request ids)  → KEEP
[target.'cfg(target_os = "macos")'.dependencies]
objc2-foundation = { … "NSArray", "NSLocale", "NSString" }  # i18n.rs system_locale()   → plan 030 removes
```

`crates/daku-protocol/src/lib.rs` (after 029/030): `pub mod identity; pub mod settings; pub mod theme; mod protocol; pub use protocol::{…}; pub use settings::DaemonSettings;` (`i18n` gone; `theme` gone or reduced per 030's outcome).

`crates/daku-protocol/src/settings.rs:29-35` (shape per plan 020 — typed `poll_interval_secs`):

```rust
impl DaemonSettings {
    pub fn default_path() -> PathBuf {
        dirs::home_dir().unwrap_or_else(std::env::temp_dir).join(".daku").join("settings.json")
    }
    …
}
```

Callers of `DaemonSettings::default_path()`: `crates/daku-daemon/src/main.rs:50` only (grep). `crates/daku-core/src/settings.rs` re-exports `pub use daku_protocol::settings::DaemonSettings;` (`:9`) and owns `DaemonSettingsStore`; `crates/daku-core/Cargo.toml` already depends on `dirs = "6.0"`.

`crates/daku-protocol/README.md:1-7`: claims no I/O; mentions the `export_types` stub.

### `crates/daku-client/src/lib.rs`

```rust
// :1-12
//! Rust transport and lifecycle support for clients of `daku-daemon`.
mod client;
pub mod persistence;
mod process;
pub use client::DaemonClient;
pub use process::{DEFAULT_EXPOSED_DAEMON_PORT, DaemonExposureSettings, DaemonProcess, DaemonSupervisor, parse_allowed_origins};
pub use daku_protocol::*;
```

Root crate uses through the client: `src/lib.rs:28` `pub use daku_client::{i18n, identity, persistence};` (030 drops `i18n`), `src/theme.rs:3` `pub use daku_client::theme::ThemePreference;` (030 moves/deletes), `src/daemon.rs` `daku_client::DaemonSupervisor::{connect, spawn_configured}`, `src/app.rs:4` imports from `daku_protocol` directly (`Cargo.toml:37` root depends on `daku-protocol`).

### `crates/daku-client/src/process.rs`

```rust
// :130-139
pub struct DaemonProcess { client: DaemonClient, child: Child }
impl DaemonProcess {
    pub fn spawn(executable: &Path) -> anyhow::Result<Self> { Self::spawn_configured(executable, DaemonExposureSettings::default()) }
    fn spawn_configured(…)
// :322-333
pub struct DaemonSupervisor { inner: Arc<SupervisorInner> }
impl DaemonSupervisor {
    pub fn spawn(executable: &Path, watch_for_rebuilds: bool) -> anyhow::Result<Self> { Self::spawn_configured(executable, watch_for_rebuilds, DaemonExposureSettings::default()) }
    pub fn spawn_configured(…)
```

Callers (grep `f7fdbe7`): `DaemonSupervisor::spawn_configured` from `src/daemon.rs:32`, `DaemonSupervisor::connect` from `src/daemon.rs:16`; **no** caller of `DaemonProcess::spawn` or `DaemonSupervisor::spawn`; `DaemonProcess` itself is only re-exported. (Plan 018 edits `process.rs` supervision code — coordinate; this plan touches only the two `spawn` fns and, if unused after 018, the `DaemonProcess` re-export.)

### `crates/daku-core/src/hollow_backend.rs` (after plan 029: `handle(&self, command: Command)`, no `LoadTaskState` arm)

```rust
pub struct HollowBackend { settings: Arc<DaemonSettingsStore> }
impl HollowBackend {
    pub fn new(settings: DaemonSettingsStore, task_store: crate::persistence::StateStore) -> anyhow::Result<Self> {
        let _ = task_store.open()?;            // opens + migrates the DB, then drops the connection
        Ok(Self { settings: Arc::new(settings) })
    }
}
impl Backend for HollowBackend { fn handle(&self, command: Command) -> anyhow::Result<ResponsePayload> { Ping → Ack; GetSettings → Settings; UpdateSettings → replace + Ack } }
```

`crates/daku-core/src/lib.rs:8,28`: `pub mod hollow_backend;` / `pub use hollow_backend::HollowBackend;`. `crates/daku-core/README.md:7-8`: "The `daku-daemon` binary calls `serve` with `HollowBackend`." `crates/daku-core/Cargo.toml:5`: `description = "Headless provider, persistence, and workspace runtime for daku"`. `crates/daku-daemon/Cargo.toml:5`: `description = "Headless daku provider daemon"`.

### `crates/daku-daemon/src/main.rs:48-64`

```rust
    let task_path = daku_core::persistence::StateStore::default_path();
    let settings = daku_core::DaemonSettingsStore::open_with_legacy(
        daku_core::DaemonSettings::default_path(),
        [task_path.with_file_name("settings.json")],
    ).context("could not load daemon settings")?;
    let task_store = daku_core::persistence::StateStore::daemon(task_path.clone());
    let dashboard_events = daku_core::start_default_loop(
        &daku_core::default_environments_path(),
        daku_core::persistence::StateStore::daemon(task_path),
        &settings.get(), shutdown.clone(),
    );
    daku_core::serve(listener, auth, Arc::new(daku_core::HollowBackend::new(settings, task_store)?), shutdown, ServerOptions { … }, dashboard_events)
```

(`StateStore` is `#[derive(Clone)]` — `persistence.rs:88-91`; and `:75-77` in `run_probe_availability` builds a third one.)

### `crates/daku-protocol/src/bin/export_types.rs` + `Cargo.toml`

```rust
//! Placeholder for future TypeScript binding export.
fn main() { eprintln!("daku-protocol export_types is disabled in the hollow stack"); }
```

There is **no** `[[bin]]` entry in `crates/daku-protocol/Cargo.toml` — cargo auto-discovers `src/bin/export_types.rs`. Deleting the file is enough. `.cargo/config.toml:7` sets `TS_RS_LARGE_INT` for a `ts-rs` crate that is not a dependency (plan 035/DEP-07 owns `.cargo/config.toml`; leave it here).

### `scripts/delete-debug-app.ts:11-16`

```ts
// Current + legacy fork IDs so a cleanup still removes pre-rename debug data.
const debugBundleIdentifiers = [
  "app.daku.dev",
  "sh.waku.dev",
  "codes.waku.dev",
];
```

Deliberate: the script also removes waku-era Debug.app data on a developer machine that had the fork's predecessor installed. **Keep** — it is documented in the comment; not a leftover.

Conventions: imperative commit summaries; `bun run check` gate; keep CONTEXT.md vocabulary (the backend serves the Operator's daemon settings — name it for what it does).

## Commands you will need

| Purpose | Command | Expected on success |
|---------|---------|---------------------|
| Compile | `cargo check --workspace --all-targets` | exit 0 |
| Protocol deps | `cargo tree -p daku-protocol -e normal --depth 1` | lists only `anyhow`, `serde`, `serde_json`, `uuid` |
| Tests | `cargo test --workspace --no-fail-fast` | 0 failed |
| Gate | `bun run check` | exit 0 |
| Daemon still starts | `DAKU_DAEMON_TOKEN=x cargo run -p daku-daemon 2>&1 \| head -1` (Ctrl-C after) | one JSON `{"address":…,"protocolVersion":…,"pid":…}` line |

## Scope

**In scope**:
- `crates/daku-protocol/{Cargo.toml,README.md}`, `crates/daku-protocol/src/{lib.rs,settings.rs}`, `crates/daku-protocol/src/bin/export_types.rs` (delete)
- `crates/daku-core/src/{hollow_backend.rs → settings_backend.rs, lib.rs, settings.rs}`, `crates/daku-core/{README.md,Cargo.toml}`
- `crates/daku-daemon/{Cargo.toml,src/main.rs}`
- `crates/daku-client/src/{lib.rs,process.rs}` (only `spawn` fns + re-export list)
- `plans/README.md` (status row)

**Out of scope**:
- Any wire type or `PROTOCOL_VERSION` (029 already bumped it).
- `scripts/delete-debug-app.ts` (waku IDs are intentional — see Current state).
- `.cargo/config.toml` (`TS_RS_LARGE_INT` — plan 035).
- `src/` root crate beyond fixing an import path if the glob re-export change breaks one.
- `DaemonExposureSettings`, `--allow-origin`, exposure flags — decided tradeoff, stay.

## Git workflow

- Trunk-based on `main`; commit directly; do NOT push unless asked.
- Suggested commits: (1) `Keep daku-protocol free of filesystem/OS deps; explicit client re-exports.` (2) `Rename HollowBackend to SettingsBackend; drop export_types stub and stale descriptions.`

## Steps

### Step 1: Move `DaemonSettings::default_path` to daku-core; drop `dirs` from the protocol crate

1. Delete `impl DaemonSettings { pub fn default_path() … }` (and `use std::path::PathBuf;` if now unused) from `crates/daku-protocol/src/settings.rs`.
2. In `crates/daku-core/src/settings.rs` add:

```rust
impl DaemonSettingsStore {
    /// `~/.daku/settings.json` (ADR-0007 data directory).
    pub fn default_path() -> PathBuf {
        dirs::home_dir()
            .unwrap_or_else(std::env::temp_dir)
            .join(format!(".{}", daku_protocol::identity::DATA_DIRECTORY_NAME))
            .join("settings.json")
    }
    …
}
```

3. `crates/daku-daemon/src/main.rs:50`: `daku_core::DaemonSettings::default_path()` → `daku_core::DaemonSettingsStore::default_path()`.
4. `crates/daku-protocol/Cargo.toml`: remove `dirs = "6.0"`. (`rust-i18n` and the `objc2-foundation` block are removed by plan 030 — verify they are gone; if not, STOP: 030 is a prerequisite.)

**Verify**: `cargo tree -p daku-protocol -e normal --depth 1` → only `anyhow`, `serde`, `serde_json`, `uuid`; `cargo check --workspace` → exit 0.

### Step 2: Explicit re-exports in the client crate

Replace `pub use daku_protocol::*;` in `crates/daku-client/src/lib.rs:12` with the explicit list the root crate actually needs. Find it: `cargo check -p daku` after the change lists every unresolved `daku_client::X` import; at `f7fdbe7` the root uses `daku_client::{identity, persistence, DaemonSupervisor}` and imports protocol types via `daku_protocol::` directly (`src/app.rs:4`, `src/dashboard_state.rs`, `src/daemon.rs`). Expected result:

```rust
pub use client::DaemonClient;
pub use process::{DEFAULT_EXPOSED_DAEMON_PORT, DaemonExposureSettings, DaemonSupervisor, parse_allowed_origins};
pub use daku_protocol::identity;
```

(`persistence` is already `pub mod persistence;` in this crate — `daku_client::persistence` keeps working.) Drop `DaemonProcess` from the re-export if Step 3 leaves it crate-private.

**Verify**: `cargo check --workspace --all-targets` → exit 0 (fix any root import by switching it to `daku_protocol::…`, which the root already depends on).

### Step 3: Delete the uncalled `spawn` constructors

In `crates/daku-client/src/process.rs` delete `DaemonProcess::spawn` (`:136-138`) and `DaemonSupervisor::spawn` (`:327-333`). Then: is `DaemonProcess` referenced outside `process.rs`? (`grep -rn 'DaemonProcess' src crates | grep -v process.rs` → at `f7fdbe7` only the re-export.) If only the re-export, remove it from `lib.rs`; the struct can stay `pub(crate)`.

**Verify**: `cargo check --workspace --all-targets` → exit 0; `grep -rn 'fn spawn(' crates/daku-client/src/process.rs` → 0.

### Step 4: `HollowBackend` → `SettingsBackend`; one `StateStore` in `main.rs`

1. `git mv crates/daku-core/src/hollow_backend.rs crates/daku-core/src/settings_backend.rs`; rename the struct to `SettingsBackend`, doc comment `//! Serves the daemon's own settings over the wire; dashboard state is pushed by the collector, not requested.`; `new(settings: DaemonSettingsStore) -> Self` (drop the `StateStore` parameter and the `task_store.open()?` — the collector opens/migrates the DB on its first tick, and `probe-availability` opens it itself; if you want the migration to run even without `environments.json`, call `store.open()?` **once** in `main.rs` instead — see step 4.3).
2. `crates/daku-core/src/lib.rs`: `pub mod settings_backend;` / `pub use settings_backend::SettingsBackend;`.
3. `crates/daku-daemon/src/main.rs:48-64` becomes:

```rust
    let store = daku_core::persistence::StateStore::daemon(daku_core::persistence::StateStore::default_path());
    store.open().context("could not open ~/.daku/app.db")?;   // migrate once at startup, fail fast
    let settings = daku_core::DaemonSettingsStore::open_with_legacy(
        daku_core::DaemonSettingsStore::default_path(),
        [store.path().with_file_name("settings.json")],
    ).context("could not load daemon settings")?;
    let dashboard_events = daku_core::start_default_loop(&daku_core::default_environments_path(), store.clone(), &settings.get(), shutdown.clone());
    daku_core::serve(listener, auth, Arc::new(daku_core::SettingsBackend::new(settings)), shutdown, daku_core::ServerOptions { … }, dashboard_events)
```

(`StateStore::path()` exists at `persistence.rs:111-113`; `open_with_legacy`'s legacy path stays as today.)
4. `crates/daku-core/README.md:7-8` → "The `daku-daemon` binary calls `serve` with `SettingsBackend` (settings RPC); Signal data is broadcast by the `CollectorLoop`."
5. Descriptions: `crates/daku-core/Cargo.toml:5` → `"Daemon runtime for daku: WebSocket hub, SQLite snapshots, ServiceNow collectors"`; `crates/daku-daemon/Cargo.toml:5` → `"daku daemon: polls ServiceNow Environments and serves the desktop client"`.

**Verify**: `grep -rn 'Hollow\|hollow' crates src README.md docs/*.md` → 0 matches (docs/research may keep historical mentions — exclude `docs/research`); `cargo test -p daku-core -p daku-daemon` → all pass; `DAKU_DAEMON_TOKEN=x cargo run -p daku-daemon 2>&1 | head -1` prints the readiness JSON.

### Step 5: Delete the `export_types` stub and fix the protocol README

1. `git rm crates/daku-protocol/src/bin/export_types.rs` (no `[[bin]]` entry exists; verify with `grep -n bin crates/daku-protocol/Cargo.toml` → 0).
2. `crates/daku-protocol/README.md` → 

```markdown
# daku-protocol

Versioned, transport-neutral wire contract shared by the daku client and daemon:
serde envelopes and identity constants only — no filesystem, OS, or socket I/O
(dependencies: serde, serde_json, uuid, anyhow for `RpcError`).
```

**Verify**: `ls crates/daku-protocol/src/bin 2>&1` → no such directory; `cargo build -p daku-protocol --bins 2>&1 | tail -1` → no bin targets (or "no bin target" note).

### Step 6: Gate

**Verify**: `bun run check` → exit 0.

## Test plan

- No behaviour change; existing tests must pass unchanged (`cargo test --workspace --no-fail-fast` → 0 failed). The daemon readiness smoke in Step 4 is the only manual check.
- If Step 4.1 removed the eager `task_store.open()`, the `main.rs` `store.open()` keeps "DB unwritable → daemon fails at startup" behaviour; confirm with `DAKU_DB_PATH=/dev/null/x DAKU_DAEMON_TOKEN=x cargo run -p daku-daemon; echo $?` → non-zero with "could not open".

## Done criteria

- [ ] `cargo tree -p daku-protocol -e normal --depth 1` shows only `anyhow`, `serde`, `serde_json`, `uuid`
- [ ] `grep -rn 'dirs::\|rust_i18n\|objc2' crates/daku-protocol` → 0 matches
- [ ] `grep -n 'pub use daku_protocol::\*' crates/daku-client/src/lib.rs` → 0 matches
- [ ] `grep -rn 'HollowBackend\|hollow_backend' src crates` → 0 matches; `grep -n 'SettingsBackend' crates/daku-daemon/src/main.rs` → 1 match
- [ ] `grep -c 'StateStore::daemon(' crates/daku-daemon/src/main.rs` → `2` (main + `run_probe_availability`)
- [ ] `test ! -e crates/daku-protocol/src/bin/export_types.rs`
- [ ] `grep -n 'workspace runtime\|Headless' crates/*/Cargo.toml` → 0 matches
- [ ] `grep -rn 'fn spawn(' crates/daku-client/src/process.rs` → 0 matches
- [ ] `bun run check` exits 0
- [ ] `git status` shows only in-scope files modified
- [ ] `plans/README.md` status row for 033 updated

## STOP conditions

- Plan 030 has not removed `rust-i18n`/`objc2-foundation` from `crates/daku-protocol/Cargo.toml` (Step 1.4) — land 030 first.
- Plan 029 has not landed (`hollow_backend.rs` still has `LoadTaskState`, `handle` still takes `Request` + `EventSink`).
- A caller of `DaemonProcess::spawn`/`DaemonSupervisor::spawn` appeared (plan 018/026 may add one for tests) — keep that fn and note it.
- The explicit re-export list in Step 2 needs more than five items to make the root crate compile — the root is importing protocol types via the client crate; switch those imports to `daku_protocol::` (root already depends on it) rather than growing the list.

## Maintenance notes

- Adding a dependency to `daku-protocol` should be a deliberate act — the README now states the rule; reviewers should reject `dirs`/OS crates there.
- `SettingsBackend` is the place for future daemon RPCs (e.g. a `doctor` command from plan 041); dashboard data stays push-only.
- Deferred: `.cargo/config.toml` `TS_RS_LARGE_INT` (plan 035); if a web client ever returns, TypeScript export is a new plan, not this stub.
