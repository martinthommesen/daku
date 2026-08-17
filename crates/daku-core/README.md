# daku-core

Daemon-side runtime: WebSocket hub (`serve`), SQLite snapshots, settings store,
ServiceNow HTTP/OAuth client (429 / `Retry-After`), and the shared `CollectorLoop`.
Depends on [`daku-protocol`](../daku-protocol); no desktop transport or UI.

`DaemonClient` lives in [`daku-client`](../daku-client). The `daku-daemon`
binary calls `serve` with `HollowBackend`.

Configuration ownership:

- desktop owns `~/.daku/app.json` in Release and checkout-local `temp/app.json` in Debug
- daemon owns `~/.daku/settings.json`

## Migrations

SQL under `db/migrations` is embedded by `build.rs` and applied by
`persistence::apply_migrations`, keyed on the numeric prefix. Append new files
(`bun run db:generate`); never regenerate a shipped one.
