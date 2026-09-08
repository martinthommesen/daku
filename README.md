# daku

macOS operator console for ServiceNow Environments — plus HTTP and GitHub probes. Native GPUI client + Rust daemon (GPL-3.0-only).

Product spec: [`docs/spec/v1.md`](docs/spec/v1.md) (v1.1 scope included). Domain vocabulary: [`CONTEXT.md`](CONTEXT.md). Signal reference: [`docs/signals.md`](docs/signals.md). Platforms: [`docs/platforms.md`](docs/platforms.md).

## One-time upstream pin

This tree is a **partial fork** of [egoist/waku](https://github.com/egoist/waku) at SHA **`4c483bc282faf4ce9296390887f09b44abb34f27`** (agent/browser/web domain stripped; crates renamed to `daku-*`). GPUI comes from upstream [zed-industries/zed](https://github.com/zed-industries/zed) after the WKWebView strip. Do not track waku after this import (ADR-0003).

## Toolchain

| Tool | Version / note |
|------|----------------|
| Rust | **≥ 1.96** (edition 2024; `rust-version` in `Cargo.toml`) |
| Xcode + Command Line Tools | macOS builds |
| Metal toolchain | GPUI compiles shaders with `xcrun metal`. On Xcode 26+ install it once: `xcodebuild -downloadComponent MetalToolchain`; check with `xcrun -f metal`. |
| Bun | only for `scripts/dev.ts`, `bun run check`, `bun run release`, `bun run lint`, `bun run db:generate` — **not** needed for `cargo` builds |

Release builds keep line-table debuginfo in a separate `.dSYM`
(`split-debuginfo = "packed"`); the shipped binaries are stripped.

## Build

```sh
cargo check --workspace
bun install  # optional: Bun scripts / lint
```

The first `cargo` build clones the pinned zed repository for GPUI (~0.5 GB) and
compiles GPUI + gpui-component — expect several minutes; later builds reuse it.
`bun install` is only needed for the Bun scripts.

The shell is built on [gpui-component](https://github.com/longbridge/gpui-component)
(ADR-0008), which depends on `gpui = { git = zed }` with no `rev`. Cargo treats
`git+zed?rev=X` and `git+zed` as different sources, so **`gpui`/`gpui_platform`
carry no `rev`** — the zed commit is pinned in `Cargo.lock` only, while
`gpui-component`/`gpui-component-assets` are pinned by `rev` in `Cargo.toml`.
Bump both together:

```sh
cargo update -p gpui-component --precise <rev>
cargo update -p gpui-component-assets --precise <rev>
cargo update -p gpui --precise <zed sha from gpui-component's Cargo.lock at that rev>
```

Then run `bun run check` and launch the fixture. Do not run `cargo update`
casually — it re-resolves every zed crate. Feature unification through
gpui-component enables `profiler` on `gpui` and `runtime_shaders` on
`gpui_platform`.

Daemon Hello auth uses env **`DAKU_DAEMON_TOKEN`**. Operator data/config lives under **`~/.daku/`** (directory `0700`, SQLite `app.db` `0600`). Override the DB path with **`DAKU_DB_PATH`**.

**Attaching to a daemon you run yourself:** start `DAKU_DAEMON_TOKEN=<token> daku-daemon --bind 127.0.0.1:<port>` and launch the app with `DAKU_DAEMON_ADDRESS=127.0.0.1:<port> DAKU_DAEMON_TOKEN=<token>`. Both variables must be set together. Non-loopback binds need `--allow-non-loopback` and are outside the v1 support envelope — see [`docs/research/hosted-daemon.md`](docs/research/hosted-daemon.md).

Copy [`environments.example.json`](environments.example.json) to `~/.daku/environments.json` (`chmod 600` it — the daemon only enforces `0700` on the directory and `0600` on files it writes) and edit Environment URLs/labels. URLs must be `https://` with no user:password part. **Secrets stay in the macOS Keychain** (daku-owned service) — never in that JSON file or in SQLite.

Per-Environment tuning lives in the same file (or in the Environment sheet, which edits the same values): a defaulted `thresholds` object overrides any degrade threshold for that Environment only (`jobs_overdue_degraded_at`, `jobs_error_degraded_at`, `syslog_error_degraded_at`, `outbound_failures_degraded_at`, `flow_error_degraded_at`, `email_failure_degraded_at`, `upgrade_failed_degraded_at`, `transaction_avg_degraded_ms` in ms or `null` to disable, `update_sets_open_degraded_at`, `scan_p1_degraded_at`, `mid_unhealthy_degraded_at`, `ecc_error_degraded_at`, `ecc_output_ready_degraded_at`, `drift_mismatches_degraded_at`, `availability_rtt_degraded_ms` in ms or `null` to disable) — unknown keys are rejected so typos fail fast. `expected_drift` lists plugin ids / store-app scopes that are planned differences; drift shows "N differ · M expected" and only unexpected drift degrades. `daku-daemon doctor` prints the effective values per Environment plus a database census line; `daku-daemon doctor --fix` repairs the directory, a missing file, and lax modes (never Credentials or URLs).

Optional poll cadence: put a top-level `"poll_interval_secs"` in `~/.daku/settings.json`, e.g. `{"poll_interval_secs": 60}` (default **120**, values below 30 are raised to 30; the daemon reads it at start — relaunch after editing `settings.json`). Inventory and history Signals (drift, last-clone, scan) ride the slow cadence `"slow_poll_interval_secs"` (default **1800**, never faster than the shared one). Optional fan-out: `"webhook_url"` POSTs every health/build event as JSON after each tick — `http` only to loopback, `https` anywhere, anything else refused (also enforced on `UpdateSettings`). One shared `CollectorLoop` polls every Environment; Availability, jobs, syslog, MID/ECC, outbound, flow, email, upgrades, sessions, table growth, slow transactions, update sets, Instance Scan, drift, and last-clone register onto it. After each tick the daemon broadcasts `EnvironmentsUpdated`, `SignalSnapshotsUpdated`, `SignalSamplesUpdated` (jobs/syslog ≤24h), `HealthEventsUpdated` (transitions + builds) and `SignalRollupsUpdated` (90 d hourly) so the GPUI client never opens SQLite. The GPUI shell is sidebar + Environment detail with health notifications, menu-bar dot, Dock badge, per-Environment mutes (header), ⌘R reload, ⌘1–9 switching, ⌘⇧C copy, ⌘⇧E export, and a ⌘K command palette; `DAKU_UI_FIXTURE=1` loads the same events as the dashboard_state tests (no ServiceNow). The Notifications menu holds the master switch, one switch per Signal, quiet-hours presets, and the Monday-09:00 weekly digest toggle; banners group every voting Signal instead of just the worst line. The detail header explains degraded health ("Label: summary" per voting Signal), flags likely clone/upgrade fallout after a recent build change, and trend drill-ins note same-weekday-hour anomalies at 2× baseline. `daku-daemon digest --env <id> [--days 7]` prints a Markdown week-in-review from local history.

### Operator smoke (local)

1. Script the setup instead of hand-editing JSON (same validation, probe,
   and save path as the sheet — the secret arrives via file, never argv):

    ```sh
    daku-daemon setup --id dev --url https://acme-dev.example.service-now.com \
      --label Dev --auth basic --secret-file /path/to/blob.json
    ```

    A failed probe aborts before writing (`--no-probe` skips it for
    known-asleep Environments). Rotate a secret the same safe way —
    shape-check, dry-run probe, then replace, with the old item surviving
    any failure:

    ```sh
    daku-daemon rotate-credential --env dev --secret-file /path/to/new-blob.json
    ```

    Ask what each table grants the monitoring account before tightening
    roles (`doctor --check-roles` reads one row per table and reports
    granted/denied with the minimal role). `daku-daemon diagnostics [--out DIR]`
    writes a redacted bundle (config without secrets, scrubbed log tail,
    database census) for tickets and debugging — offline by design.
2. Add an Environment from the app menu (daku → Add Environment…): label, `https://` URL, auth method, Credential, thresholds (empty means default), and expected-drift ids — Test dry-runs the probe, Save writes `~/.daku/environments.json` (0600) and the Keychain item, then reloads. Editing works from the Environment header (Edit); deleting asks twice. The sheet refuses malformed URLs, bad threshold numbers, and mismatched Credential shapes before anything is written.
2. Prefer OAuth (`{"client_id":"…","client_secret":"…"}`), basic only for PDI stand-ins (`{"username":"…","password":"…"}`). Hand-editing stays supported: copy [`environments.example.json`](environments.example.json) to `~/.daku/environments.json` (`chmod 600` it) and store Credentials with `security add-generic-password -U -s daku -a <id> -w` (with `-w` last so the shell prompts — the secret never lands in history). Press ⌘R after hand-editing. Daemon diagnostics (missing config, Keychain misses, HTTP errors) are appended to `~/.daku/daemon.log`. Do not commit URLs or secrets.

3. One-shot Availability probe (no daemon token):

   ```sh
   cargo run -p daku-daemon -- probe-availability
   ```

   Writes `signal_id=availability` into `~/.daku/app.db` (or `DAKU_DB_PATH`).

4. Diagnose the setup (writes nothing):

   ```sh
   cargo run -p daku-daemon -- doctor
   ```

   One line per Environment; fix anything flagged before launching the app.

```sh
cargo test -p daku-daemon
```

Dev watcher (macOS Debug.app):

```sh
bun run dev
```

`DAKU_UI_FIXTURE=1 bun run dev` renders fixture data without ServiceNow;
`DAKU_DB_PATH=/tmp/daku-dev.db` keeps a dev daemon's SQLite away from
`~/.daku/app.db` (the dev Debug.app otherwise polls the same Environments as an
installed Daku.app).

## Environment variables

Runtime and dev variables. See [`docs/packaging.md`](docs/packaging.md) for
release-time variables.

| Variable | Read by | Effect |
|----------|---------|--------|
| `DAKU_DAEMON_TOKEN` | daemon (`crates/daku-daemon`), app when attaching | Hello bearer token. The daemon refuses to start with an empty value. |
| `DAKU_DAEMON_ADDRESS` | app (`src/daemon.rs`) | `host:port` or `ws://` URL of a daemon you run yourself; attach instead of spawning. Must be set together with `DAKU_DAEMON_TOKEN`. |
| `DAKU_DAEMON_PATH` | app (`src/daemon.rs`), set by `scripts/dev.ts` | Path to the `daku-daemon` binary the app spawns. |
| `DAKU_APP_EXECUTABLE` | set by the app for its daemon child (`crates/daku-client`) | Internal — not for Operators. |
| `DAKU_DB_PATH` | `crates/daku-core` persistence | SQLite path override (default `~/.daku/app.db`, or `DAKU_HOME/app.db`). |
| `DAKU_HOME` | `crates/daku-core` config/persistence/settings | Home override for automation: every default daemon path (`environments.json`, `credentials.json`, `app.db`, `settings.json`) resolves under this dir instead of `~/.daku/`. |
| `DAKU_CREDENTIAL_STORE` | daemon (`crates/daku-daemon`), `crates/daku-core` config | `file` selects the file store (`~/.daku/credentials.json`, `0600`); anything else selects the macOS Keychain (default). Test/automation and non-macOS hosts. |
| `DAKU_CREDENTIAL_FILE` | `crates/daku-core` config | Path override for the file credential store (implies file-store use with `--credential-store file`; also honoured via `--credential-file`). |
| `DAKU_UI_FIXTURE` | app (`src/dashboard_state.rs`) | `=1` loads fixture dashboard events; no ServiceNow calls. |
| `DAKU_CHANNEL` | app (`src/updater.rs`) | `homebrew` disables Sparkle at runtime. |
| `DAKU_FORCE_UPDATER` | app (`src/updater.rs`, debug builds) | `=1` runs the real Sparkle flow from a debug bundle. |
| `CARGO_TARGET_DIR` | `scripts/dev.ts`, `scripts/delete-debug-app.ts` | Cargo's target directory, when it is not `target/`. |

Daemon stderr: `~/.daku/daemon.log`.

Secrets never go in env files — Credentials live in the macOS Keychain (service
`daku`); there is deliberately no `.env.example`.

## Packaging

Unsigned `Daku.app` (no Developer ID):

```sh
./scripts/bundle.sh --unsigned
```

Writes `dist/Daku.app`. Sparkle is the primary updater. Homebrew cask
(`homebrew/daku.rb`) installs `Daku-x.y.z-homebrew.dmg` built with
`DAKU_CHANNEL=homebrew` so Sparkle is a compile-time no-op. Human
notarisation checklist: [`docs/packaging.md`](docs/packaging.md).

## Licence

GPL-3.0-only — see [`LICENSE`](LICENSE).
