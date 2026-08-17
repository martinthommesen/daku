# Plan 019: Daemon stderr goes to `~/.daku/daemon.log`, and an empty Environment list explains itself

> **Executor instructions**: Follow this plan step by step. Run every
> verification command and confirm the expected result before moving to the
> next step. If anything in the "STOP conditions" section occurs, stop and
> report — do not improvise. When done, update the status row for this plan
> in `plans/README.md` — unless a reviewer dispatched you and told you they
> maintain the index.
>
> **Drift check (run first)**: `git diff --stat f7fdbe7..HEAD -- crates/daku-client/src/process.rs src/app.rs src/dashboard_state.rs README.md crates/daku-daemon/README.md`
> If any of those files changed since this plan was written, compare the
> "Current state" excerpts against the live code before proceeding; on a
> mismatch, treat it as a STOP condition.

## Status

- **Priority**: P2
- **Effort**: S
- **Risk**: LOW
- **Depends on**: plans/011-green-baseline-check-gate.md (gate). Plan 014 is unrelated in code but improves the same first-run experience.
- **Category**: dx / bug
- **Planned at**: commit `f7fdbe7`, 2026-08-17
- **Issue**: https://github.com/martinthommesen/daku/issues/41

## Why this matters

Every daemon failure path is an `eprintln!` (`crates/daku-core/src/collector.rs` "daku collector idle: missing …", "daku collector not started: …", tick/publish failures; `server.rs` handshake errors). The desktop spawns the daemon with `.stderr(Stdio::inherit())`, and a packaged `Daku.app` (or `open`-launched Debug.app from `bun run dev`) has no terminal — so a malformed `~/.daku/environments.json`, a missing Keychain item, or an OAuth failure prints once to nowhere. The Operator sees "Waiting" cards or an empty sidebar and has no way to find out why short of running `daku-daemon` by hand.

Two small changes: (1) the supervisor opens `~/.daku/daemon.log` (append, 0600) and hands it to the child as stderr; (2) when connected and the Environment list is empty, the detail pane says what to do instead of "No Environment selected." — and the README says the daemon reads `environments.json` at start, so relaunch after creating it. Structured logging (`log`/`tracing`) is out of scope: a file is enough to make failures findable.

## Current state

### `crates/daku-client/src/process.rs:140-166` (`DaemonProcess::spawn_configured`)

```rust
        let mut child = command
            .env(DAEMON_TOKEN_ENV, &auth)
            .env(APP_EXECUTABLE_ENV, app_executable)
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::inherit())
            .spawn()
            .with_context(|| format!("could not launch {}", executable.display()))?;
```

Imports at the top: `use std::process::{Child, Command as ProcessCommand, Stdio};`, `use anyhow::{Context as _, bail};`. `crates/daku-client/src/persistence.rs:64-68` has `configuration_directory()` = `dirs::home_dir().unwrap_or_else(temp_dir).join(".daku")` (private fn) and the 0600 `OpenOptions` idiom at `:194-198`. `daku_protocol::identity::DATA_DIRECTORY_NAME` is `"daku"` (the dir is `.daku`).

Remote mode (`DAKU_DAEMON_ADDRESS`) never spawns, so no log is written there — correct.

### `src/app.rs:224-231` (`render_detail`, empty branch)

```rust
            .when(self.state.selected().is_none(), |element| {
                element.child(
                    div()
                        .p(px(22.0))
                        .text_color(theme.text_tertiary)
                        .child("No Environment selected."),
                )
            })
```

`DashboardState` (`src/dashboard_state.rs`) exposes `connected()`, `selected()`, `sidebar()` (Vec of rows — empty when there are no Environments); there is no `environments_len()`/`is_empty()` accessor. `SidebarRow` is `Clone` (`:44`). Fixture mode (`DAKU_UI_FIXTURE=1`) always has 3 Environments.

### Docs

`README.md:28`: "Copy [`environments.example.json`](environments.example.json) to `~/.daku/environments.json` and edit Environment URLs/labels." `README.md:33`: step 1 "Copy the example file to `~/.daku/environments.json`. Use your own Environment URLs locally — do not commit them." No mention that the daemon reads it only at start. `crates/daku-daemon/README.md` has no logging note.

Conventions: `parking_lot`-free std here (`process.rs` uses `std::sync`), `anyhow::Context`, `eprintln!` for supervisor-side diagnostics, imperative commit summaries. GPUI element style as in `disconnected_banner` (`src/app.rs:317-326`).

## Commands you will need

| Purpose | Command | Expected on success |
|---------|---------|---------------------|
| Client tests | `cargo test -p daku-client` | all pass |
| UI state tests | `cargo test -p daku dashboard_state` | all pass |
| Check | `cargo check --workspace` | exit 0 |
| Gate | `bun run check` | exit 0 |
| Manual | `bun run dev` with `~/.daku/environments.json` moved aside | log line + empty state (Step 4) |

## Scope

**In scope**:
- `crates/daku-client/src/process.rs` (stderr redirect + helper + test)
- `src/dashboard_state.rs` (one accessor + test)
- `src/app.rs` (empty-state text)
- `README.md`, `crates/daku-daemon/README.md` (one sentence each)
- `plans/README.md` (status row)

**Out of scope**:
- Replacing `eprintln!` with a logging crate; log rotation (see Maintenance notes).
- A protocol-level "collector status" message to render the actual error in the UI (backlog BUG-10/DIR-04).
- `crates/daku-core` — no daemon-side change; it keeps writing to stderr.
- `scripts/dev.ts` — the log path is the same in Debug and Release.

## Git workflow

- Commit on `main`; do not push unless asked. Suggested summary: `Log daemon stderr to ~/.daku/daemon.log and explain an empty Environment list.`

## Steps

### Step 1: Log file helper in `process.rs`

Add near `desktop_client_address`:

```rust
/// `~/.daku/daemon.log`, append-only, 0600. The daemon writes its diagnostics
/// to stderr; a packaged app has no terminal, so the supervisor points stderr
/// here. Falls back to inheriting stderr when the file cannot be opened.
fn daemon_log_stdio() -> Stdio {
    let path = dirs::home_dir()
        .unwrap_or_else(std::env::temp_dir)
        .join(".daku")
        .join("daemon.log");
    if let Some(parent) = path.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    let mut options = std::fs::OpenOptions::new();
    options.create(true).append(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt as _;
        options.mode(0o600);
    }
    match options.open(&path) {
        Ok(file) => Stdio::from(file),
        Err(error) => {
            eprintln!("could not open {} for daemon logs: {error}", path.display());
            Stdio::inherit()
        }
    }
}
```

(`dirs` is already a dependency of `daku-client`.) Change `.stderr(Stdio::inherit())` in `spawn_configured` to `.stderr(daemon_log_stdio())`.

Note: `~/.daku` may not exist yet on first run — `create_dir_all` handles it; the daemon later sets it 0700 (`persistence::ensure_daku_dir`).

Add a unit test that does not touch `~`: factor the path into `fn daemon_log_path(home: &Path) -> PathBuf` and `fn open_daemon_log(path: &Path) -> io::Result<File>`, keep `daemon_log_stdio()` as the thin composition, and test `open_daemon_log(&temp_dir().join(format!("daku-log-{}/daemon.log", Uuid::new_v4())))` → file exists, `metadata.permissions().mode() & 0o777 == 0o600` (unix), and a second open appends (write two lines via two opens, read back 2 lines).

**Verify**: `cargo test -p daku-client daemon_log` → 1 passed.

### Step 2: Empty-state accessor

In `src/dashboard_state.rs` add to `impl DashboardState`:

```rust
    pub fn has_environments(&self) -> bool {
        !self.environments.is_empty()
    }
```

Test in `mod tests`: `DashboardState::new().has_environments() == false`; after `loaded()` (existing fixture helper) → `true`.

**Verify**: `cargo test -p daku dashboard_state` → all pass (+1).

### Step 3: Empty-state text in the detail pane

In `src/app.rs` `render_detail`, replace the `"No Environment selected."` branch with:

```rust
            .when(self.state.selected().is_none(), |element| {
                let message = if self.state.connected() && !self.state.has_environments() {
                    "No Environments configured — copy environments.example.json to ~/.daku/environments.json, then relaunch daku. Daemon diagnostics: ~/.daku/daemon.log"
                } else {
                    "No Environment selected."
                };
                element.child(
                    div()
                        .p(px(22.0))
                        .text_color(theme.text_tertiary)
                        .child(message),
                )
            })
```

**Verify**: `cargo check -p daku` → exit 0.

### Step 4: Docs + manual check

- `README.md:33` step 1, append: "The daemon reads this file at start — relaunch daku after creating or editing it. Daemon diagnostics (missing config, Keychain misses, HTTP errors) are appended to `~/.daku/daemon.log`."
- `crates/daku-daemon/README.md`, after the "The desktop supervises this process." paragraph, add: "When launched by the desktop, stderr is redirected to `~/.daku/daemon.log` (append, 0600). Run the binary by hand to see diagnostics in the terminal."

Manual: move `~/.daku/environments.json` aside, `bun run dev` → the detail pane shows the "No Environments configured …" line and `tail -1 ~/.daku/daemon.log` contains `daku collector idle: missing`. Restore the file, relaunch → Environments appear.

**Verify**: `grep -n 'daemon.log' README.md crates/daku-daemon/README.md src/app.rs crates/daku-client/src/process.rs` → ≥1 match each.

### Step 5: Gate

**Verify**: `bun run check` → exit 0.

## Test plan

- `process.rs`: `daemon_log_opens_append_only_0600` (temp path; unix perms assertion under `#[cfg(unix)]`). Model on `desktop_uses_loopback_to_reach_an_unspecified_listener` for style.
- `dashboard_state.rs`: `has_environments_reflects_loaded_state`, model on `:504-514`.
- Manual: Step 4.
- `cargo test --workspace --no-fail-fast` → 0 failed.

## Done criteria

- [ ] `grep -n 'Stdio::inherit()' crates/daku-client/src/process.rs` → only inside `daemon_log_stdio`'s fallback
- [ ] `grep -n 'daemon.log' crates/daku-client/src/process.rs README.md crates/daku-daemon/README.md src/app.rs` → ≥1 each
- [ ] `grep -n 'pub fn has_environments' src/dashboard_state.rs` → 1 match
- [ ] `cargo test -p daku-client` and `cargo test -p daku` pass with the 2 new tests
- [ ] `bun run check` exits 0
- [ ] `git status` shows only in-scope files modified
- [ ] `plans/README.md` status row for 019 updated

## STOP conditions

- `spawn_configured` no longer contains `.stderr(Stdio::inherit())` (already redirected elsewhere) — report.
- `render_detail`'s empty branch differs from the excerpt (UI restructured) — place the message wherever "No Environment selected." now lives; if it no longer exists, report.
- Manual check shows nothing appended to `daemon.log` while the daemon clearly failed — verify the daemon still writes to stderr (`crates/daku-core/src/collector.rs` `eprintln!`), then report.

## Maintenance notes

- The log grows unbounded (one line per failure per tick at worst — ~120 s cadence, so KB/day). Add truncation-on-launch (`truncate(true)` when > N MB) if it ever matters; note it in the README when you do.
- If a `CollectorStatus` protocol message is added later (backlog), the empty-state text should show the daemon's actual reason instead of the generic hint.
- Reviewers: confirm the file is opened `append` (two daemons — debug + release — may share it) and 0600 (error strings can include instance hostnames).
