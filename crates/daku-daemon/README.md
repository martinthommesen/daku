# daku-daemon

Standalone process that hosts `daku-core::serve`. Loopback-only by default;
clients authenticate with `DAKU_DAEMON_TOKEN`. An empty token is refused at
startup. Prints one JSON readiness record to stdout (address, protocol
version, pid).

```text
DAKU_DAEMON_TOKEN=<secret> daku-daemon --bind 127.0.0.1:0 [--parent-pid PID] [--allow-origin ORIGIN]... [--credential-store keychain|file] [--credential-file PATH]
daku-daemon probe-availability [--credential-store file --credential-file PATH]
daku-daemon doctor [--credential-store file --credential-file PATH]
```

`probe-availability` loads `~/.daku/environments.json` (or `DAKU_HOME` when set), resolves Credentials from the macOS Keychain (service `daku`) or — with `--credential-store file` / `DAKU_CREDENTIAL_STORE=file` — from the file store (`~/.daku/credentials.json`, `0600`; override with `--credential-file` / `DAKU_CREDENTIAL_FILE`), and writes an Availability snapshot. It does not need `DAKU_DAEMON_TOKEN`.

`doctor` prints one line per Environment (config, Credential presence — never the value —, reachability, build) and exits 1 if any Environment lacks a Credential or is unreachable. It writes nothing.

The desktop supervises this process. Debug builds use the feature-gated
`daku-debug-daemon` target at `target/debug/daku-debug-daemon`. Release
distributions place the signed `daku-daemon` beside the desktop executable.

When launched by the desktop, stderr is redirected to `~/.daku/daemon.log`
(append, 0600). Run the binary by hand to see diagnostics in the terminal.

A non-loopback bind is refused unless `--allow-non-loopback` is also present.
Browser handshakes need an exact `--allow-origin`; native clients send no Origin.
