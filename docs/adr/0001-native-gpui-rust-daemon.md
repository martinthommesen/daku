# Native GPUI client + Rust daemon

daku v1 inherits [waku](https://github.com/egoist/waku)'s **GPUI native desktop** and **Rust daemon/protocol** shape (local daemon + native client), not the web client. macOS-only. Chosen for native feel *and* look despite research showing the web shell is the smaller fork — the Operator wants a desktop app, and the daemon+client split keeps a path to host the collector later.

## Considered options

- **Web shell + TypeScript collector** (research recommendation) — smaller fork, same visual tokens; rejected because native feel matters.
- **Both clients** — double surface; deferred.
- **Single-process native app** — simpler, but drops the daemon seam [#6](https://github.com/martinthommesen/daku/issues/6) wanted for a future shared host.
