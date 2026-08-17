# daku-core

Daemon-side runtime: WebSocket hub (`serve`), SQLite snapshots, settings store,
ServiceNow HTTP/OAuth client (429 / `Retry-After`), and the shared `CollectorLoop`.
Depends on [`daku-protocol`](../daku-protocol); no desktop transport or UI.

`DaemonClient` lives in [`daku-client`](../daku-client). The `daku-daemon`
binary calls `serve` with `HollowBackend`.

Configuration ownership:

- desktop owns `~/.daku/app.json` in Release and checkout-local `temp/app.json` in Debug
- daemon owns `~/.daku/settings.json`
