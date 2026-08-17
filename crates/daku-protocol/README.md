# daku-protocol

Versioned, transport-neutral wire contract shared by the daku client and daemon:
serde envelopes and identity constants only — no filesystem, OS, or socket I/O
(dependencies: serde, serde_json, uuid, anyhow for `RpcError`).
