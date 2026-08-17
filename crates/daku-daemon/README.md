# daku-daemon

Standalone process that hosts `daku-core::serve`. Loopback-only by default;
clients authenticate with `DAKU_DAEMON_TOKEN`. Prints one JSON readiness record
to stdout (address, protocol version, pid).

```text
DAKU_DAEMON_TOKEN=<secret> daku-daemon --bind 127.0.0.1:0 [--parent-pid PID] [--allow-origin ORIGIN]...
daku-daemon probe-availability
```

`probe-availability` loads `~/.daku/environments.json`, resolves Credentials from the macOS Keychain (service `daku`), and writes an Availability snapshot. It does not need `DAKU_DAEMON_TOKEN`.

The desktop supervises this process. Debug builds use the feature-gated
`daku-debug-daemon` target at `target/debug/daku-debug-daemon`. Release
distributions place the signed `daku-daemon` beside the desktop executable.

A non-loopback bind is refused unless `--allow-non-loopback` is also present.
Browser handshakes need an exact `--allow-origin`; native clients send no Origin.
