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

```sh
cargo test -p daku-daemon
```

Dev watcher (macOS Debug.app):

```sh
bun run dev
```

## Licence

GPL-3.0-only — see [`LICENSE`](LICENSE).
