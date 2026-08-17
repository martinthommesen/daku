# daku

macOS operator console for ServiceNow Environments. Native GPUI client + Rust daemon (GPL-3.0-only).

Product spec: [`docs/spec/v1.md`](docs/spec/v1.md). Domain vocabulary: [`CONTEXT.md`](CONTEXT.md).

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

Optional poll cadence: put a top-level `"poll_interval_secs"` in `~/.daku/settings.json`, e.g. `{"poll_interval_secs": 60}` (default **120**, values below 30 are raised to 30; the daemon reads it at start — relaunch after editing). One shared `CollectorLoop` polls every Environment; Availability, jobs, syslog, MID/ECC, outbound, drift, and last-clone register onto it. After each tick the daemon broadcasts `EnvironmentsUpdated`, `SignalSnapshotsUpdated`, and `SignalSamplesUpdated` (jobs/syslog ≤24h) so the GPUI client never opens SQLite. The GPUI shell is sidebar + Environment detail; `DAKU_UI_FIXTURE=1` loads the same events as the dashboard_state tests (no ServiceNow).

### Operator smoke (local)

1. Copy the example file to `~/.daku/environments.json`. Use your own Environment URLs locally — do not commit them. The daemon reads this file at start — relaunch daku after creating or editing it. Daemon diagnostics (missing config, Keychain misses, HTTP errors) are appended to `~/.daku/daemon.log`.
2. Store Credentials in Keychain, service `daku`, account = Environment `id`:
   - OAuth: `{"client_id":"…","client_secret":"…"}`
   - Basic (PDI stand-in only): `{"username":"…","password":"…"}`

   ```sh
   security add-generic-password -s daku -a prod -w '{"client_id":"…","client_secret":"…"}'
   ```

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
| `DAKU_DB_PATH` | `crates/daku-core` persistence | SQLite path override (default `~/.daku/app.db`). |
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
