# Hosted daemon — decision note

**Question**: ADR-0001/ADR-0003 kept the daemon + versioned protocol + native
client split so the collector could be *hosted later*; issue
[#6](https://github.com/martinthommesen/daku/issues/6) closed with "local
first, shared later … a future shared host needs an explicit security pass"
and "shared host deferred". This note writes down what that seam actually is
today, what a hosted daemon would cost, and which option we take now.

Verified against the tree at plan 042's branch point (`e6f45b4`); every claim
carries a `file:line`.

## What exists

- `src/daemon.rs:7-37` — `start_process()`: with both `DAKU_DAEMON_ADDRESS`
  and `DAKU_DAEMON_TOKEN` set, the desktop attaches to an existing daemon
  instead of spawning one.
- `src/daemon.rs:18-27` — one variable without the other is a hard `bail!`;
  neither set falls through to the local spawn (`src/daemon.rs:30-36`).
- `crates/daku-client/src/process.rs:360-364` — `DaemonSupervisor::connect`
  builds a `DaemonTarget::Remote`; dropping the desktop never stops that
  daemon.
- `crates/daku-client/src/process.rs:494` — `monitor_daemon` returns
  immediately for `Remote`: a remote daemon is never reconnected (plan 018).
- `crates/daku-client/src/process.rs:411` — `is_remote()` has no caller
  outside that file; the UI cannot tell the two modes apart.
- `crates/daku-daemon/src/main.rs:93-96,113-135` — `--bind`,
  `--allow-non-loopback`, repeated `--allow-origin`.
- `crates/daku-daemon/src/main.rs:83-90` — `ensure_bind_allowed` refuses a
  non-loopback bind unless `--allow-non-loopback` is passed.
- `crates/daku-core/src/server.rs:445-450` — handshake path is `/v1`;
  `crates/daku-client/src/client.rs:244` is the client side of that.
- `crates/daku-core/src/server.rs:451-462` — the Origin allowlist only applies
  when an `Origin` header is present; native clients send none.
- `crates/daku-core/src/server.rs:296,473-475` — token comparison is
  constant-time (`ct_eq`), with no rejection of an empty expected token.
- `crates/daku-core/src/server.rs:27,232` — `MAX_CONNECTIONS = 64`, enforced
  by a `ConnectionPermit` on accept.
- `crates/daku-client/src/process.rs:35-49` — `DaemonExposureSettings
  { enabled, port, allowed_origins, token }`, default `enabled: false`,
  `allowed_origins: ["http://localhost:3001"]` (a waku web-client leftover),
  token minted once (`:54-65`); `bind_address()` becomes `0.0.0.0:<port>`
  when `enabled` (`:87-96`).
- `crates/daku-client/src/persistence.rs:39,48,128,138` — the block is
  persisted as `daemon_exposure` in `~/.daku/app.json`. Its only reader is
  `src/daemon.rs:35`; no UI writes it, and `reconfigure`
  (`crates/daku-client/src/process.rs:421`) has no caller.
- `src/daemon.rs:41-61` — `local_hostname()`, written to "show a useful LAN
  URL" in Settings; no caller anywhere.

Verified manually while writing this note: `daku-daemon --bind 127.0.0.1:0`
prints its JSON ready line, a `/v1` handshake with the right token receives
`{"type":"hello",…}`, and a wrong token receives
`{"type":"rejected","message":"authentication failed"}`.

## Options

**(A) Local-only — delete the desktop exposure plumbing, keep env attach.**
Delete `DaemonExposureSettings` and `AppSettings.daemon_exposure`
(`crates/daku-client/src/process.rs:35-126`,
`crates/daku-client/src/persistence.rs:39`), `spawn_configured`/`reconfigure`
/`is_remote` (`crates/daku-client/src/process.rs:334,411,421`), and
`local_hostname` (`src/daemon.rs:41-61`). Keep the daemon's `--bind`,
`--allow-non-loopback` and `--allow-origin` — that is the envelope any hosted
step needs, and it is already tested
(`crates/daku-daemon/src/main.rs:173-176`). Keep the `DAKU_DAEMON_ADDRESS`
attach: one match statement, useful for debugging. **Effort S.** Forces no
security-pass item; the shipped product stays loopback-only.

**(B) One Operator, one remote box.** Keep env attach, add reconnect for
`Remote` (`crates/daku-client/src/process.rs:494`, plan 018), add a
non-Keychain `CredentialStore` (`crates/daku-core/src/config.rs:52-54` is the
seam; `KeychainCredentialStore` is hard-wired at
`crates/daku-core/src/collector.rs:184,210` and returns `Err` off macOS,
`crates/daku-core/src/config.rs:102-105`) behind a daemon flag — a `0600` file
or env is enough for one Operator. Terminate `wss://` at a reverse proxy
(`daku-client` already carries `rustls-tls-native-roots`,
`crates/daku-client/Cargo.toml:17`; the daemon has no TLS listener). Loopback
stays the default. **Effort M–L.** Forces the whole checklist below.

**(C) Defer entirely.** No change. **Effort 0**, but plans 018/020/032 keep
guessing whether the exposure plumbing is dead weight or a foundation, and the
`http://localhost:3001` default keeps shipping.

## Security-pass checklist

Required before any non-loopback bind is supported (i.e. before (B)):

- TLS: `wss://` via reverse proxy or a daemon TLS listener; today everything
  crosses `ws://` in clear text.
- Token provisioning and rotation: today the token is an env var read once
  (`crates/daku-daemon/src/main.rs` via `DAEMON_TOKEN_ENV`,
  `crates/daku-protocol/src/protocol.rs:10`), and an empty token is accepted
  (`crates/daku-core/src/server.rs:473-475`) — closes when plan 012 lands.
- `UpdateSettings` authorisation: any authenticated client rewrites
  `~/.daku/settings.json` (`crates/daku-core/src/hollow_backend.rs:32-35`).
- Request-thread cap: connections are capped at 64
  (`crates/daku-core/src/server.rs:27,232`) but `dispatch_request` spawns an
  unbounded thread per request (`crates/daku-core/src/server.rs:390-399`).
- Message-size cap: `MAX_WIRE_MESSAGE_BYTES` is 48 MiB
  (`crates/daku-protocol/src/protocol.rs:9`, applied at
  `crates/daku-core/src/server.rs:331-332`) — fine on loopback, too generous
  from a network.
- Origin policy: native clients send no Origin and skip the allowlist
  (`crates/daku-core/src/server.rs:451-462`); a hosted daemon needs an
  explicit browser-vs-native decision.
- Data at rest on the host: `~/.daku/` `0700` / `app.db` `0600` semantics are
  a macOS assumption (`README.md:26`); a Linux host needs its own check.
- Log hygiene: daemon stderr carries Environment paths and error strings
  (plan 019) — must not leak `instance_url`, tokens or Operator hostnames.
- `instance_url` on the wire: plan 039 puts it on `EnvironmentSummary`; over a
  network that is Operator-identifying data.
- Late subscribers: dashboard messages are broadcast only
  (`crates/daku-core/src/collector.rs:194`,
  `crates/daku-core/src/server.rs:214-222`); `Hub::subscribe` replays the
  inherited waku journal, not dashboard state
  (`crates/daku-core/src/server.rs:142-158`), so a reconnecting remote client
  sees nothing until the next tick — closes when plan 014 lands.

## Recommendation

Take **(A) now, keeping the `DAKU_DAEMON_ADDRESS` attach path**. The desktop
exposure block is unreachable configuration — no UI reads or writes it, its
default origin is waku residue, and `local_hostname` has no caller — so it is
cost with no user. The daemon-side `--bind`/`--allow-non-loopback`
/`--allow-origin` flags stay: they are the tested envelope (B) would build on,
and they cost nothing while `enabled` is unreachable. (B) is the documented
next step, to be planned only when a second machine is actually wanted, and it
must clear the checklist above first. Explicitly not (C): plans 020 and 032
need this answer to delete safely.

## Follow-up plan stubs

- **Under (A) — "Delete the desktop daemon-exposure plumbing" (S)**, folded
  into plan 020's settings cleanup. In scope:
  `crates/daku-client/src/process.rs` (`DaemonExposureSettings`,
  `parse_allowed_origins`, `allowed_origins_text`,
  `with_allowed_origins_text`, `ensure_token`, `bind_address`,
  `DaemonProcess::spawn_configured`, `DaemonSupervisor::spawn_configured`,
  `reconfigure`, `is_remote`, the `exposure` field on `SupervisorInner`),
  `crates/daku-client/src/persistence.rs` (`AppSettings.daemon_exposure` and
  its migration read at `:128,138`), `src/daemon.rs` (`local_hostname`, the
  `spawn_configured` call site → `DaemonSupervisor::spawn`). Out of scope:
  `crates/daku-daemon/src/main.rs` flags and `crates/daku-core/src/server.rs`.
- **Under (B), when wanted — "Remote daemon support" (L)**: plan 018
  (reconnect) first, then a `CredentialStore` implementation behind a daemon
  flag, then the security-pass checklist as its done criteria.

## Non-goals

No multiple Operators, no daku accounts, login or roles, no multi-tenant
hosted service, no alerting, no second Platform (spec §10). This note covers
one Operator optionally running their own daemon on their own machine.
