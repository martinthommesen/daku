# Plan 042: Decide what "hostable later" means — document the existing remote-daemon path and write the hosted-daemon decision note

> **Executor instructions**: Follow this plan step by step. Run every
> verification command and confirm the expected result before moving to the
> next step. If anything in the "STOP conditions" section occurs, stop and
> report — do not improvise. When done, update the status row for this plan
> in `plans/README.md` — unless a reviewer dispatched you and told you they
> maintain the index.
>
> **Drift check (run first)**: `git diff --stat f7fdbe7..HEAD -- src/daemon.rs crates/daku-daemon/src/main.rs crates/daku-daemon/README.md crates/daku-client/src/process.rs crates/daku-client/src/persistence.rs crates/daku-core/src/config.rs crates/daku-core/src/collector.rs crates/daku-core/src/server.rs README.md`
> The Current-state excerpts are inventory, not edit targets — on drift,
> re-read the live code and correct the file:line references in the note.

## Status

- **Priority**: P3
- **Effort**: S (spike + one README paragraph; no product code)
- **Risk**: LOW
- **Depends on**: plans/011-green-baseline-check-gate.md (gate), plans/014-replay-dashboard-on-subscribe.md (closes one gap this note would otherwise list); soft: plans/020-settings-cleanup-typed-poll-interval.md (decides whether `DaemonExposureSettings` survives — coordinate)
- **Category**: direction
- **Planned at**: commit `f7fdbe7`, 2026-08-17
- **Issue**: https://github.com/martinthommesen/daku/issues/66

## Why this matters

ADR-0001 and ADR-0003 kept the daemon + versioned protocol + native client split explicitly so the collector can be **hosted later**; issue #6 resolved "(c) local first, shared later … a future shared host needs an explicit security pass" and closed with "shared host deferred". Today the seam is real but invisible: the desktop already connects to any daemon when `DAKU_DAEMON_ADDRESS` + `DAKU_DAEMON_TOKEN` are set, the daemon already has `--bind`/`--allow-non-loopback`/`--allow-origin`, and `~/.daku/app.json` persists a `daemon_exposure` block that no UI edits and no doc mentions. Half of that is inherited waku plumbing that may be dead weight; the other half is the smallest step towards "one Operator, daemon on another box of theirs". Without a written decision, every debt plan that touches this code has to guess. This plan writes that decision — options, gaps with file:line, security-pass checklist, recommendation — and documents the one path that already works. **No multi-user, no daku login, no second Platform** (spec §10).

## Current state (inventory — verify each line while writing the note)

Remote path that exists:

- `src/daemon.rs:7-29` — `start_process()`: if both `DAKU_DAEMON_ADDRESS` and `DAKU_DAEMON_TOKEN` are set → `daku_client::DaemonSupervisor::connect(address, auth)`; one set without the other → `bail!`; neither → local spawn with `app_settings.daemon_exposure` (`:30-36`).
- `crates/daku-client/src/process.rs:360-363` — `DaemonSupervisor::connect` → `DaemonTarget::Remote(client)`; `:494` `monitor_daemon` returns immediately for `Remote` (no reconnect — plan 018); `:411` `is_remote()`.
- `crates/daku-daemon/src/main.rs:20-23, 83-90` — `--bind`, `ensure_bind_allowed` (non-loopback refused without `--allow-non-loopback`), `--allow-origin` list; README `crates/daku-daemon/README.md:7-9,18-19` documents the flags but not the desktop side. Root `README.md` never mentions `DAKU_DAEMON_ADDRESS`.
- `crates/daku-core/src/server.rs:27` `MAX_CONNECTIONS = 64`; `:280-284, 446-460` Origin allowlist only when an `Origin` header is present (native clients send none); token check `:296` constant-time.
- `crates/daku-client/src/process.rs:34-40` — `DaemonExposureSettings { enabled, port, allowed_origins, token }` persisted in `AppSettings.daemon_exposure` (`crates/daku-client/src/persistence.rs:33-38`), default `enabled: false`, `allowed_origins: ["http://localhost:3001"]` (a waku web-client leftover), token minted once (`:54-63`); `bind_address()` = `0.0.0.0:<port>` when enabled (`:82-86`). No UI reads or writes it (`git grep daemon_exposure src/` → only `src/daemon.rs:35`). `src/daemon.rs:39-61` `local_hostname()` — comment says "Settings can then show a useful LAN URL"; no caller.

Gaps for a hosted daemon:

- Credentials: `crates/daku-core/src/config.rs:81-105` `KeychainCredentialStore` is macOS-only (`Err` elsewhere) and `collector.rs:190,212` hard-wire it in `start_default_loop`/`probe_availability_once`; the trait `CredentialStore` (`:52-54`) is the seam.
- Late subscribers see nothing until the next tick — **closed by plan 014** (cite it, don't re-list as open once 014 is DONE).
- Settings write access over the wire: `crates/daku-core/src/hollow_backend.rs:31-34` `UpdateSettings` writes `~/.daku/settings.json` for any authenticated client; `server.rs` spawns a thread per request with no cap and accepts up to `MAX_WIRE_MESSAGE_BYTES` (48 MiB) — fine on loopback, part of the security pass otherwise.
- Data sensitivity (issue #6): `instance_url` (plan 039 puts it on the wire), Signal payload error strings, builds — all cross the socket in clear text over `ws://`; `daku-client` has `rustls-tls-native-roots` for `wss://` but the daemon has no TLS listener.
- Empty token accepted server-side — **closed by plan 012**.

## Commands you will need

| Purpose | Command | Expected on success |
|---------|---------|---------------------|
| Issue context (read-only) | `gh issue view 6 --comments` | resolution text as quoted above |
| Verify remote path compiles/works locally | terminal 1: `DAKU_DAEMON_TOKEN=<any-non-empty> cargo run -p daku-daemon -- --bind 127.0.0.1:0` (prints a JSON ready line with `address`); terminal 2: `DAKU_DAEMON_ADDRESS=<that address> DAKU_DAEMON_TOKEN=<same> DAKU_UI_FIXTURE=1 bun run dev` | app connects to the external daemon (no `Disconnected` banner) |
| Gate | `bun run check` | exit 0 |

(Use a throwaway token; never paste a real one anywhere.)

## Scope

**In scope**:
- `docs/research/hosted-daemon.md` (new decision note)
- `README.md` (one paragraph: attaching the app to a hand-run daemon)
- `plans/README.md`

**Out of scope**:
- Any Rust change. Deleting or wiring `DaemonExposureSettings`, `local_hostname`, or the reconnect loop happens in plans 020/018/032 per this note's recommendation.
- Anything implying multiple Operators, daku accounts, or a hosted multi-tenant service.

## Git workflow

- Commit on `main`; do not push unless asked. Suggested: `Document the remote-daemon path and record the hosted-daemon decision note.`

## Steps

### Step 1: Prove the remote path

Run the two-terminal check from the Commands table. Note anything that does not work (e.g. the app shows `Disconnected` after the daemon restarts — expected until plan 018).

**Verify**: the app renders the fixture while attached to the external daemon; `cargo run -p daku-daemon -- --help` lists `--bind`, `--allow-non-loopback`, `--allow-origin`.

### Step 2: README paragraph

Under `## Build` (after the daemon-token sentence at `README.md:26`), add:

```markdown
**Attaching to a daemon you run yourself:** start `DAKU_DAEMON_TOKEN=<token> daku-daemon --bind 127.0.0.1:<port>` and launch the app with `DAKU_DAEMON_ADDRESS=127.0.0.1:<port> DAKU_DAEMON_TOKEN=<token>`. Both variables must be set together. Non-loopback binds need `--allow-non-loopback` and are outside the v1 support envelope — see `docs/research/hosted-daemon.md`.
```

**Verify**: `grep -n 'DAKU_DAEMON_ADDRESS' README.md` → 1 match.

### Step 3: Decision note

Write `docs/research/hosted-daemon.md` (≤ 2 pages) with these sections, every claim carrying a `file:line`:

1. **What exists** — the inventory above, re-verified.
2. **Options** — (A) *Local-only, delete the exposure plumbing*: remove `DaemonExposureSettings`/`daemon_exposure`/`local_hostname`/`--allow-origin`(?) — list what each deletion touches; keeps `DAKU_DAEMON_ADDRESS` env attach for debugging. (B) *One Operator, one remote box*: keep env attach + reconnect (plan 018), add a non-Keychain `CredentialStore` (file 0600 or env) behind a daemon flag, `wss://` via a reverse proxy or a TLS listener, keep loopback default. (C) *Defer entirely*: no change, note revisit trigger. For each: effort (S/M/L), what security-pass items it forces.
3. **Security-pass checklist** (from issue #6 "explicit security pass" + the gaps above): TLS/`wss://`, token provisioning and rotation, `UpdateSettings` authorisation, request-thread cap, message-size cap for non-loopback, Origin policy for native vs browser clients, data-at-rest on the host (SQLite 0600 semantics on Linux), log hygiene (`daemon.log` — plan 019), `instance_url` on the wire (plan 039).
4. **Recommendation** — one paragraph. Suggested default unless the maintainer overrides: **(A) now, keeping the env-attach path** (it is one match statement and useful for debugging), with (B) as the documented next step when a second machine is actually wanted; explicitly *not* (C) because plan 020/032 need the answer to delete safely.
5. **Follow-up plan stubs** — title + in-scope files + S/M for whichever option is chosen; and the exact list of symbols plan 020/032 may delete under (A).
6. **Explicit non-goals** — multi-user, daku login/roles, alerting, second Platform (spec §10).

**Verify**: `grep -n '^## ' docs/research/hosted-daemon.md` → the 6 headings; `grep -c 'rs:' docs/research/hosted-daemon.md` ≥ 10.

### Step 4: Gate

**Verify**: `bun run check` → exit 0 (docs-only; the gate proves nothing else moved).

## Test plan

- No code changes; the "test" is Step 1's manual attach run and the note's file:line references resolving (`git grep -n` each cited symbol once while writing).

## Done criteria

- [ ] `docs/research/hosted-daemon.md` exists with the six headings and a single `Recommendation`
- [ ] `grep -n 'DAKU_DAEMON_ADDRESS' README.md` → 1 match
- [ ] `git status` shows only `README.md`, `docs/research/hosted-daemon.md`, `plans/README.md`
- [ ] `bun run check` exits 0
- [ ] `plans/README.md` status row for 042 updated

## STOP conditions

- The remote attach path no longer exists in `src/daemon.rs` (someone deleted it under plan 020/032 before this note) — report; the note then documents (A) as already done.
- Step 1 fails to connect for a reason unrelated to a restart (handshake rejected with a correct token) — that is a bug; report with the daemon's stderr line, do not work around it.

## Maintenance notes

- This note is the input to plans 018/020/032 for the exposure plumbing; update it (or supersede with an ADR) when option (B) is actually built.
- If (B) is chosen later, promote the note to `docs/adr/0008-hosted-daemon.md` via `/domain-modeling`.
