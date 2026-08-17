# daku

macOS operator console for ServiceNow Environments. Native GPUI client + Rust daemon (GPL-3.0-only).

Product spec: [`docs/spec/v1.md`](docs/spec/v1.md). Domain vocabulary: [`CONTEXT.md`](CONTEXT.md).

## One-time upstream pin

This tree is a **partial fork** of [egoist/waku](https://github.com/egoist/waku) at SHA **`4c483bc282faf4ce9296390887f09b44abb34f27`** (agent/browser/web domain stripped; crates renamed to `daku-*`). GPUI comes from upstream [zed-industries/zed](https://github.com/zed-industries/zed) after the WKWebView strip. Do not track waku after this import (ADR-0003).

## Toolchain

| Tool | Version |
|------|---------|
| Rust | **≥ 1.96** (edition 2024) |
| Bun | current (schema / `scripts/dev.ts`) |
| Xcode / clang | macOS builds |

## Build

```sh
bun install
cargo check -p daku -p daku-core -p daku-daemon -p daku-protocol -p daku-client
```

Daemon Hello auth uses env **`DAKU_DAEMON_TOKEN`**. Operator data/config lives under **`~/.daku/`** (directory `0700`, SQLite `app.db` `0600`). Override the DB path with **`DAKU_DB_PATH`**.

Copy [`environments.example.json`](environments.example.json) to `~/.daku/environments.json` and edit Environment URLs/labels. **Secrets stay in the macOS Keychain** (daku-owned service) — never in that JSON file or in SQLite.

Optional `poll_interval_secs` in `~/.daku/settings.json` `extra` (default **120**). One shared `CollectorLoop` polls every Environment; Availability, jobs, syslog, MID/ECC, outbound, drift, and last-clone register onto it. After each tick the daemon broadcasts `EnvironmentsUpdated`, `SignalSnapshotsUpdated`, and `SignalSamplesUpdated` (jobs/syslog ≤24h) so the GPUI client never opens SQLite. The GPUI shell is sidebar + Environment detail; `DAKU_UI_FIXTURE=1` loads the same events as the dashboard_state tests (no ServiceNow).

### Operator smoke (local)

1. Copy the example file to `~/.daku/environments.json`. Use your own Environment URLs locally — do not commit them.
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

```sh
cargo test -p daku-daemon
```

Dev watcher (macOS Debug.app):

```sh
bun run dev
```

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
