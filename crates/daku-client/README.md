# daku-client

Rust client for `daku-daemon`: authenticated WebSocket handshake, request
correlation, dashboard subscription, and local-daemon supervision
(`DaemonSupervisor`). Depends on [`daku-protocol`](../daku-protocol), never on
`daku-core`.

Bare socket addresses and `ws://` / `wss://` URLs are accepted. Dropping a
connection to an externally managed daemon does not stop it.
