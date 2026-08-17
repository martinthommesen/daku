# Plan 020: One typed daemon setting (`poll_interval_secs`), no desktop settings mirror, no dead preference plumbing

> **Executor instructions**: Follow this plan step by step. Run every
> verification command and confirm the expected result before moving to the
> next step. If anything in the "STOP conditions" section occurs, stop and
> report — do not improvise. When done, update the status row for this plan
> in `plans/README.md` — unless a reviewer dispatched you and told you they
> maintain the index.
>
> **Drift check (run first)**: `git diff --stat f7fdbe7..HEAD -- crates/daku-protocol/src/settings.rs crates/daku-protocol/src/protocol.rs crates/daku-core/src/settings.rs crates/daku-core/src/collector.rs crates/daku-core/src/hollow_backend.rs crates/daku-daemon/src/main.rs crates/daku-client/src/process.rs crates/daku-client/src/persistence.rs src/lib.rs src/daemon.rs README.md crates/daku-core/README.md`
> Plan 011 intentionally touched `settings.rs`/`collector.rs`/`README.md` (flatten + tests + README line) — that diff is expected and this plan supersedes it. Any other mismatch with the "Current state" excerpts is a STOP condition.

## Status

- **Priority**: P2
- **Effort**: M
- **Risk**: LOW–MED (on-disk `settings.json`/`app.json` shapes change; loads stay tolerant)
- **Depends on**: plans/011-green-baseline-check-gate.md (must land first — this plan replaces its `extra`-based tests and README line)
- **Category**: tech-debt / bug
- **Planned at**: commit `f7fdbe7`, 2026-08-17
- **Issue**: https://github.com/martinthommesen/daku/issues/43

## Why this matters

The settings code is a waku leftover with two overlapping preference documents, neither honoured, plus one live bug:

- **BUG-04**: the desktop reads `DaemonSettings` once at spawn (`process.rs:342`), keeps it in `SupervisorInner.settings`, and a `persist_settings` thread re-sends that boot-time snapshot to the daemon (`UpdateSettings` → `DaemonSettingsStore::replace` rewrites the whole file) after **every** daemon restart (`queue_settings_refresh` at `process.rs:516`, dev rebuild swaps and crash restarts). Any hand edit to `~/.daku/settings.json` while the app runs is silently overwritten.
- `DaemonSettings.theme/language` are loaded, written back, and never applied (`apply_theme_preference` and `i18n::set_language` have no callers). Same for `AppSettings.theme/language/analytics_enabled` on the desktop side.
- The only real daemon setting, `poll_interval_secs`, lives untyped in `extra`.
- Client `StateStore`/`save_window_state` have no callers, so `load_window_state`/`restored_window_placement` (`src/lib.rs:50-95`) can only ever read a stale waku `state.json` — window placement is never persisted (BUG-12). `analytics_id` is regenerated on every (never-happening) save.
- `DaemonSupervisor::settings/update_settings/reconfigure/is_remote` (`process.rs:411-467`) have no callers outside the crate.

End state (minimal, honest): `DaemonSettings { poll_interval_secs: u64 }` on the wire and on disk; the daemon owns `~/.daku/settings.json`; the desktop never writes it; the desktop's `app.json` holds only `daemon_exposure` (kept — daemon exposure flags are a decided tradeoff and are what `spawn_configured` needs); window state code deleted (nothing writes it; add it back with a writer when a UI needs it). Poll interval is still read at daemon start (relaunch after edit — README already says so after plan 011); re-reading per tick is not worth the plumbing today.

## Current state

### `crates/daku-protocol/src/settings.rs` (whole file after plan 011; `:15` gains `flatten`)

```rust
use std::collections::BTreeMap;
use std::path::PathBuf;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use crate::i18n::AppLanguage;
use crate::theme::ThemePreference;

#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
#[serde(default)]
pub struct DaemonSettings {
    pub theme: ThemePreference,
    pub language: AppLanguage,
    #[serde(flatten, skip_serializing_if = "BTreeMap::is_empty")]   // `flatten` added by plan 011
    pub extra: BTreeMap<String, Value>,
}
impl Default for DaemonSettings { … theme/language defaults, empty extra … }
impl DaemonSettings {
    pub fn default_path() -> PathBuf { dirs::home_dir().unwrap_or_else(std::env::temp_dir).join(".daku").join("settings.json") }
    pub fn discard_legacy_app_keys(&mut self) { for key in ["analytics_enabled", "favorite_models"] { self.extra.remove(key); } }
}
```

`crates/daku-protocol/src/protocol.rs:8` `pub const PROTOCOL_VERSION: u32 = 1;` — used in `Command::UpdateSettings { settings: DaemonSettings }` (`:67`) and `ResponsePayload::Settings { settings }` (`:197-199`); test `protocol_version_is_daku_domain` (`:332-334`) asserts `PROTOCOL_VERSION == 1`. `crates/daku-protocol/src/lib.rs:35` `pub use settings::DaemonSettings;`. `crates/daku-protocol/src/theme.rs` (`ThemePreference`, still used by `src/theme.rs`) and `i18n.rs` (`AppLanguage`) stay.

### `crates/daku-core/src/settings.rs`

`DaemonSettingsStore::open(path)` → `open_with_legacy(path, empty)` (`:17-19`); `open_with_legacy` (`:21-67`) parses the file, quarantines corrupt files (`quarantine_corrupt_settings`, `:82-87`), scans legacy paths on NotFound, calls `settings.discard_legacy_app_keys()` (`:59`) and on `replace` (`:74`); `write_atomic` (`:89-97`). Test `legacy_combined_settings_keep_only_daemon_fields` (`:108-127`) asserts `extra["future"] == 42` and `analytics_enabled` scrubbed.

### `crates/daku-core/src/collector.rs:27-37`

```rust
pub const DEFAULT_POLL_INTERVAL_SECS: u64 = 120;
pub const POLL_INTERVAL_SECS_KEY: &str = "poll_interval_secs";
pub fn poll_interval_secs(settings: &DaemonSettings) -> u64 {
    settings.extra.get(POLL_INTERVAL_SECS_KEY).and_then(|value| value.as_u64()).filter(|value| *value > 0).unwrap_or(DEFAULT_POLL_INTERVAL_SECS)
}
```

Callers: `start_default_loop` (`:187`), test at `:272`, plus plan 011's two tests `poll_interval_secs_reads_top_level_json_key` / `poll_interval_secs_falls_back_to_default_for_zero_or_non_number`.

### `crates/daku-core/src/hollow_backend.rs:25-35` — `GetSettings` → `self.settings.get()`, `UpdateSettings { settings }` → `self.settings.replace(settings)?`. `crates/daku-core/src/server.rs:528-540` `TestBackend` answers `GetSettings` with `Default::default()`.

### `crates/daku-daemon/src/main.rs:48-53`

```rust
    let task_path = daku_core::persistence::StateStore::default_path();
    let settings = daku_core::DaemonSettingsStore::open_with_legacy(
        daku_core::DaemonSettings::default_path(),
        [task_path.with_file_name("settings.json")],
    )
    .context("could not load daemon settings")?;
```

### `crates/daku-client/src/process.rs`

Settings mirror to delete: `SupervisorInner.settings / persisted_settings / settings_updates` (`:296-298`); `spawn_configured` `let settings = read_settings(&process.client())?;` (`:342`) and `connect` (`:362`); `from_target` takes `settings`, spawns the `daku-daemon-settings` thread running `persist_settings` (`:372-390`); `DaemonSupervisor::is_remote/settings/reconfigure/update_settings` (`:411-467`); `queue_settings_refresh` (`:516`, `:439`, `:445`, `:561-565`); `read_settings` (`:567-572`); `persist_settings` (`:574-617`). Imports at `:17-21` include `Command, DaemonSettings, ResponsePayload`. Keep: `DaemonExposureSettings` (`:33-94`), `parse_allowed_origins`, `DaemonProcess`, `DaemonTarget`, `monitor_daemon`, `replace_local_daemon`, `subscribe_clients`, `client`. Note `reconfigure` is the only other caller of `replace_local_daemon` besides `monitor_daemon`.

### `crates/daku-client/src/persistence.rs` (242 lines, read it in full)

`AppSettings { analytics_enabled, theme, language, daemon_exposure }` (`:33-51`); `PersistedWindowState`, `AppState`, `APP_STATE_VERSION`, `DEFAULT_SIDEBAR_WIDTH/RIGHT_PANEL_WIDTH` (`:18-62`); `default_app_settings_path()` (`:70-76`: Debug → `StateStore::default_path().with_file_name("app.json")` i.e. `<repo>/temp/app.json`; Release → `~/.daku/app.json`); `default_app_state_path`, `default_legacy_settings_paths` (`:78-88`); `read_app_state_file`, `load_window_state` (`:90-98`); `read_app_settings_source` (`:100-117`, primary then legacy `settings.json`); `load_or_create_app_settings` (`:119-143`); client `StateStore { path, app_state_path }` with `default_path()` (Debug `<repo>/temp/app.db`, Release `dirs::data_local_dir()/daku/app.db`), `remote()`, `path()`, `save_window_state` (`:145-186`); `write_json_atomically` (`:188-205`, the 0600 idiom — keep); test `desktop_settings_paths_are_build_specific` (`:215-241`, tautological).

Callers: `src/daemon.rs:30-36` uses `load_or_create_app_settings()?.daemon_exposure`; `src/lib.rs:28` `pub use daku_client::{i18n, identity, persistence};` and `:61` `crate::persistence::load_window_state()` inside `restored_window_placement` (`:50-95`, with `TITLEBAR_GRAB_WIDTH/HEIGHT` `:47-48` used only there); `src/lib.rs:151` `let (window_bounds, display_id) = restored_window_placement(cx);` feeding `WindowOptions { window_bounds: Some(..), display_id, .. }` (`:169-170`). Nothing calls `StateStore::remote/path/save_window_state`.

### Docs

`README.md:30` (after plan 011): "Optional poll cadence: put a top-level `"poll_interval_secs"` in `~/.daku/settings.json` … relaunch after editing" — still true after this plan; no change. `crates/daku-core/README.md:10-13`: "Configuration ownership: desktop owns `~/.daku/app.json` in Release and checkout-local `temp/app.json` in Debug; daemon owns `~/.daku/settings.json`" — still true; add what each holds.

Conventions: `parking_lot::Mutex` in client/core, `std::sync::Mutex` nowhere new; `#[serde(default)]` structs; imperative commit summaries; tests at file bottom.

## Commands you will need

| Purpose | Command | Expected on success |
|---------|---------|---------------------|
| Protocol tests | `cargo test -p daku-protocol` | all pass |
| Core settings/collector tests | `cargo test -p daku-core settings:: poll_interval` (run twice with each filter) | all pass |
| Client tests | `cargo test -p daku-client` | all pass |
| Root build | `cargo check -p daku` | exit 0 |
| Dead code check | `cargo clippy -p daku-client -p daku-protocol -p daku-core 2>&1 \| grep -c 'never used'` | not higher than before this plan |
| Gate | `bun run check` | exit 0 |

## Scope

**In scope**:
- `crates/daku-protocol/src/settings.rs`, `crates/daku-protocol/src/protocol.rs` (`PROTOCOL_VERSION` + its test)
- `crates/daku-core/src/settings.rs`, `crates/daku-core/src/collector.rs` (`poll_interval_secs` + tests), `crates/daku-daemon/src/main.rs` (`open` call)
- `crates/daku-client/src/process.rs`, `crates/daku-client/src/persistence.rs`
- `src/lib.rs` (window placement), `src/daemon.rs` only if a signature changes (it should not)
- `crates/daku-core/README.md`
- `plans/README.md` (status row)

**Out of scope**:
- `crates/daku-protocol/src/theme.rs`, `i18n.rs`, `src/theme.rs` (`apply_theme_preference`, `native_override` stay as dead code for the dead-code plan DEBT-08 / crate-layering plan DEBT-09).
- `Command::LoadTaskState` / `HollowBackend` naming (DEBT-10).
- Re-reading `poll_interval_secs` per tick; PERF-07 floor.
- Anything under `src/app.rs`, `src/updater.rs`.
- The `temp/app.json` Debug location — keep behaviour, only the code that computes it changes.

## Git workflow

- Commit on `main`; do not push unless asked. Two commits: `Type DaemonSettings as { poll_interval_secs }; bump protocol to 2.` then `Delete the desktop settings mirror and dead window/preference plumbing.`

## Steps

### Step 1: Typed `DaemonSettings`

Replace `crates/daku-protocol/src/settings.rs` body with:

```rust
use std::path::PathBuf;

use serde::{Deserialize, Serialize};

pub const DEFAULT_POLL_INTERVAL_SECS: u64 = 120;

/// Daemon-owned settings (`~/.daku/settings.json`). Unknown keys are ignored
/// on load and dropped on write.
#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
#[serde(default)]
pub struct DaemonSettings {
    /// Shared collector cadence in seconds; `0` means the default.
    pub poll_interval_secs: u64,
}

impl Default for DaemonSettings {
    fn default() -> Self {
        Self { poll_interval_secs: DEFAULT_POLL_INTERVAL_SECS }
    }
}

impl DaemonSettings {
    pub fn default_path() -> PathBuf {
        dirs::home_dir().unwrap_or_else(std::env::temp_dir).join(".daku").join("settings.json")
    }
}
```

In `protocol.rs`: increment `PROTOCOL_VERSION` by one from its current value (it is `1` at f7fdbe7; plans 029 and 039 also bump it — read the live value first, set `N+1`, and update `protocol_version_is_daku_domain` and `crates/daku-core/src/server.rs:544` to assert the new value). (Desktop and daemon ship together; a stale daemon is rejected at Hello with a clear "protocol … does not match" error — that is the point of the bump.)

**Verify**: `cargo test -p daku-protocol` → all pass. `cargo check --workspace` will now fail in `daku-core`/`daku-client` — expected until Steps 2–4.

### Step 2: `daku-core` — collector + store

`collector.rs`: delete `POLL_INTERVAL_SECS_KEY`; make `DEFAULT_POLL_INTERVAL_SECS` a re-export (`pub use daku_protocol::settings::DEFAULT_POLL_INTERVAL_SECS;`) and:

```rust
pub fn poll_interval_secs(settings: &DaemonSettings) -> u64 {
    if settings.poll_interval_secs == 0 { DEFAULT_POLL_INTERVAL_SECS } else { settings.poll_interval_secs }
}
```

Tests: `poll_interval_secs_reads_top_level_json_key` (from plan 011) still passes unchanged. Replace `poll_interval_secs_falls_back_to_default_for_zero_or_non_number` with `poll_interval_secs_zero_means_default` (`{"poll_interval_secs":0}` → 120; default → 120) and `poll_interval_secs_rejects_non_number` (`serde_json::from_str::<DaemonSettings>(r#"{"poll_interval_secs":"fast"}"#).is_err()` — a typed field now fails to parse, and `DaemonSettingsStore` quarantines such a file).

`settings.rs`: delete `open_with_legacy` (fold the parse/quarantine/NotFound→default logic into `open`), delete both `discard_legacy_app_keys()` calls. Replace the test with `settings_file_with_unknown_keys_loads_and_rewrites_typed`: write `{"theme":"dark","poll_interval_secs":45,"future":42}`; `open` → `get().poll_interval_secs == 45`; `replace(get())`; file JSON has `poll_interval_secs == 45` and **no** `theme`/`future` keys. Add `corrupt_settings_are_quarantined`: write `not json`; `open` → `Ok`, defaults, a sibling `settings.json.corrupt-*` exists, the primary now parses. `daku-daemon/src/main.rs:49-53` → `daku_core::DaemonSettingsStore::open(daku_core::DaemonSettings::default_path())`. Delete `task_path.with_file_name("settings.json")` usage (keep `task_path` for the store).

**Verify**: `cargo test -p daku-core settings::` and `cargo test -p daku-core poll_interval` → all pass; `cargo test -p daku-daemon` → all pass.

### Step 3: `daku-client/src/process.rs` — delete the settings mirror

Remove: `settings`, `persisted_settings`, `settings_updates` fields; the `settings` parameter of `from_target` and the `daku-daemon-settings` thread; `read_settings(&…)` calls in `spawn_configured` and `connect`; `is_remote`, `settings`, `reconfigure`, `update_settings`; `queue_settings_refresh` (and its call in `monitor_daemon`); `read_settings`; `persist_settings`; now-unused imports (`Command`, `DaemonSettings`, `ResponsePayload`, `Receiver`/`Sender` if unused, `Uuid` if unused). `replace_local_daemon` keeps its one remaining caller (`monitor_daemon`).

**Verify**: `cargo check -p daku-client` → exit 0 with no `unused import` warnings for this file; `cargo test -p daku-client` → all pass.

### Step 4: `daku-client/src/persistence.rs` — `AppSettings { daemon_exposure }`, no window state

- `AppSettings` → single field `daemon_exposure: DaemonExposureSettings` (keep `#[serde(default)]`; `Default` derives). Delete the `AppLanguage`/`ThemePreference` imports.
- Delete `PersistedWindowState`, `AppState`, `APP_STATE_VERSION`, `DEFAULT_SIDEBAR_WIDTH`, `DEFAULT_RIGHT_PANEL_WIDTH`, `default_app_state_path`, `read_app_state_file`, `load_window_state`, `StateStore` (whole impl), `default_legacy_settings_paths`, and the legacy branch of `read_app_settings_source` (rename to `read_app_settings(path) -> io::Result<Option<Vec<u8>>>`).
- Keep the Debug/Release location rule with a small helper replacing the deleted `StateStore::default_path()` usage:

```rust
fn default_app_settings_path() -> PathBuf {
    if cfg!(debug_assertions) {
        // Checkout-local so a dev build never shares app.json with an installed Daku.app.
        Path::new(env!("CARGO_MANIFEST_DIR")).parent().and_then(Path::parent)
            .unwrap_or_else(|| Path::new(env!("CARGO_MANIFEST_DIR"))).join("temp").join("app.json")
    } else {
        configuration_directory().join("app.json")
    }
}
```

- `load_or_create_app_settings`: read primary only; `token_was_persisted` check stays (an `app.json` without a `daemon_exposure.token` must be rewritten so the token is stable across restarts); write when the file was absent, the token was not persisted, or `ensure_token()` minted one.
- Extract `pub fn load_or_create_app_settings_at(path: &Path) -> io::Result<AppSettings>` (the current body parameterised by path) and make `load_or_create_app_settings()` call it with the default path. Replace the tautological test with three on `_at`: no file → written, 0600 (unix), non-empty token; file with `{"daemon_exposure":{"token":""}}` → token minted and rewritten; file with a token and extra legacy keys (`"theme":"dark","analytics_enabled":false`) → token unchanged, file **not** rewritten (compare mtime or bytes before/after).

**Verify**: `cargo test -p daku-client` → all pass (+3).

### Step 5: `src/lib.rs` — centred window, no restore

Delete `restored_window_placement`, `TITLEBAR_GRAB_WIDTH`, `TITLEBAR_GRAB_HEIGHT`; at `:151` replace with:

```rust
            let window_bounds = WindowBounds::Windowed(Bounds::centered(
                None,
                size(px(DEFAULT_WINDOW_WIDTH), px(DEFAULT_WINDOW_HEIGHT)),
                cx,
            ));
```

and in `WindowOptions` set `window_bounds: Some(window_bounds), display_id: None,`. Remove `point`/`Bounds` from the `gpui` import only if now unused (`point` is still used at `:159`). `pub use daku_client::{i18n, identity, persistence};` stays (`persistence` is still used by `src/daemon.rs`).

**Verify**: `cargo check -p daku` → exit 0; `cargo test -p daku` → all pass.

### Step 6: Docs and gate

`crates/daku-core/README.md:12-13` → "- desktop owns `~/.daku/app.json` (Release) / checkout-local `temp/app.json` (Debug): `daemon_exposure` only\n- daemon owns `~/.daku/settings.json`: `poll_interval_secs` (read at start)". Confirm `README.md:30` still describes `{"poll_interval_secs": 60}` — unchanged.

**Verify**: `bun run check` → exit 0. Manual: with an old `~/.daku/settings.json` containing `theme`/`language`, launch the daemon once → file is rewritten to `{ "poll_interval_secs": … }` only (or left as-is if never `replace`d — either is fine); a hand-edited `poll_interval_secs` survives a dev-rebuild daemon restart (`bun run dev`, touch the daemon binary, check the file).

## Test plan

- `daku-protocol`: `protocol_version_is_daku_domain` updated to 2.
- `daku-core`: `poll_interval_secs_zero_means_default`, `poll_interval_secs_rejects_non_number`, `settings_file_with_unknown_keys_loads_and_rewrites_typed`, `corrupt_settings_are_quarantined`.
- `daku-client`: three `load_or_create_app_settings_at` tests (Step 4).
- `cargo test --workspace --no-fail-fast` → 0 failed.

## Done criteria

- [ ] `grep -rn 'extra\|discard_legacy_app_keys\|open_with_legacy' crates/daku-protocol/src/settings.rs crates/daku-core/src/settings.rs crates/daku-daemon/src/main.rs crates/daku-core/src/collector.rs` → no matches
- [ ] `PROTOCOL_VERSION` in `crates/daku-protocol/src/protocol.rs` is exactly one higher than at the commit before this plan (`git show HEAD~1:crates/daku-protocol/src/protocol.rs | grep PROTOCOL_VERSION`)
- [ ] `grep -n 'persist_settings\|queue_settings_refresh\|read_settings\|fn reconfigure\|fn update_settings\|fn is_remote' crates/daku-client/src/process.rs` → no matches
- [ ] `grep -n 'analytics\|ThemePreference\|AppLanguage\|StateStore\|window_state\|PersistedWindowState' crates/daku-client/src/persistence.rs` → no matches
- [ ] `grep -n 'restored_window_placement\|load_window_state' src/lib.rs` → no matches
- [ ] `grep -n 'daemon_exposure' crates/daku-client/src/persistence.rs src/daemon.rs` → still present (kept)
- [ ] `cargo test --workspace --no-fail-fast` → 0 failed; `bun run check` exits 0
- [ ] `git status` shows only in-scope files modified
- [ ] `plans/README.md` status row for 020 updated

## STOP conditions

- Plan 011 has not landed (the flatten test is still failing) — land 011 first.
- Any caller of `DaemonSupervisor::settings/update_settings/reconfigure`, `persistence::StateStore`, or `load_window_state` exists outside the files listed (`git grep -n` each before deleting) — report; something grew a UI.
- `src/lib.rs` window setup no longer matches the excerpt (GPUI API drift) — adapt only if it is the same two fields; otherwise report.
- `cargo test -p daku-core` shows any collector test depending on `DaemonSettings.extra`.

## Maintenance notes

- New daemon settings: add a typed field with a default to `DaemonSettings`; bump `PROTOCOL_VERSION` only when an existing field changes meaning or type (adding a defaulted field is wire-compatible, but keep desktop and daemon in the same build anyway).
- Window placement persistence: when a UI wants it, add a writer on window move/close (GPUI `on_window_bounds_changed`-style hook) together with the reader — never one side alone.
- Reviewers: `daemon_exposure.token` stability is what keeps a configured browser client working across restarts — check the "token unchanged, file not rewritten" test.
- Superseded from plan 011: its `poll_interval_secs_falls_back_to_default_for_zero_or_non_number` test (replaced here); its README wording stays.
