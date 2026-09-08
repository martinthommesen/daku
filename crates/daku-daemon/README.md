# daku-daemon

Standalone process that hosts `daku-core::serve`. Loopback-only by default;
clients authenticate with `DAKU_DAEMON_TOKEN`. An empty token is refused at
startup. Prints one JSON readiness record to stdout (address, protocol
version, pid).

```text
DAKU_DAEMON_TOKEN=<secret> daku-daemon --bind 127.0.0.1:0 [--parent-pid PID] [--allow-origin ORIGIN]... [--credential-store keychain|file] [--credential-file PATH]
daku-daemon probe-availability [--credential-store file --credential-file PATH]
daku-daemon doctor [--fix] [--check-roles] [--credential-store file --credential-file PATH]
daku-daemon digest --env <id> [--days 7]
daku-daemon diagnostics [--out DIR]
daku-daemon mcp
daku-daemon setup --id <id> --url <https-url> [--label L] [--platform servicenow|http|github] [--auth oauth|basic] [--clone-source] [--secret-file PATH] [--no-probe]
daku-daemon rotate-credential --env <id> --secret-file <path> [--no-probe]
```

`probe-availability` loads `~/.daku/environments.json` (or `DAKU_HOME` when set), resolves Credentials from the macOS Keychain (service `daku`) or — with `--credential-store file` / `DAKU_CREDENTIAL_STORE=file` — from the file store (`~/.daku/credentials.json`, `0600`; override with `--credential-file` / `DAKU_CREDENTIAL_FILE`), and writes an Availability snapshot. It does not need `DAKU_DAEMON_TOKEN`.

`doctor` prints one line per Environment (config, Credential presence — never the value —, reachability, build) and exits 1 if any Environment lacks a Credential or is unreachable. It writes nothing — unless `--fix`, which repairs the directory, a missing file, and lax modes (never Credentials or URLs). `--check-roles` reads one row per watched table and reports granted/denied with the minimal role.

`digest --env <id> [--days 7]` (1–90, clamped) prints a Markdown week-in-review from local history. `diagnostics [--out DIR]` writes a redacted bundle (config without secrets, scrubbed log tail, database census) for tickets and debugging — offline by design. `mcp` serves the five read-only agent tools over stdio (see `docs/agents/daku-mcp.md`). `setup` scripts Environment creation end to end (same validation, probe, and save path as the sheet — the secret arrives via file, never argv; `--no-probe` skips the probe for known-asleep Environments). `rotate-credential` replaces a Credential the same safe way: shape-check, dry-run probe, then replace, with the old item surviving any failure. Operator flows are in the root `README.md`.

The desktop supervises this process. Debug builds use the feature-gated
`daku-debug-daemon` target at `target/debug/daku-debug-daemon`. Release
distributions place the signed `daku-daemon` beside the desktop executable.

When launched by the desktop, stderr is redirected to `~/.daku/daemon.log`
(append, 0600). Run the binary by hand to see diagnostics in the terminal.

A non-loopback bind is refused unless `--allow-non-loopback` is also present.
Browser handshakes need an exact `--allow-origin`; native clients send no Origin.
