# daku-core

Daemon-side runtime: WebSocket hub (`serve`), SQLite migration helpers, settings
store, and a hollow `HollowBackend` until Signal collectors land (plan 002+).
Depends on [`daku-protocol`](../daku-protocol); no desktop transport or UI.

`DaemonClient` lives in [`daku-client`](../daku-client). The `daku-daemon`
binary calls `serve` with `HollowBackend`.

Configuration ownership:

- desktop owns `~/.daku/app.json` in Release and checkout-local `temp/app.json` in Debug
- daemon owns `~/.daku/settings.json`
