# Plan 003: Availability Signal (build/latency probe); fixtures + local PDI smoke

> **Executor instructions**: Follow this plan step by step. Run every verification command and confirm the expected result before moving to the next step. If anything in the "STOP conditions" section occurs, stop and report — do not improvise. When done, update the status row for this plan in `plans/README.md`.
>
> **Drift check (run first)**: `git diff --stat 567179a..HEAD -- crates/daku-core crates/daku-daemon docs/examples README.md`
> Confirm 002 DONE (`signal_snapshots` migration exists). On mismatch, STOP.

## Status

- **Priority**: P1
- **Effort**: M
- **Risk**: MED
- **Depends on**: plans/002-daemon-sqlite-skeleton.md
- **Category**: direction
- **Planned at**: commit `567179a`, 2026-08-17
- **Issue**: https://github.com/martinthommesen/daku/issues/20

## Why this matters

Availability is Signal #1: up/latency + build via Table API on `sys_properties` `name=glide.war`. This plan also owns the **shared collector infrastructure** every later Signal reuses: config load, Keychain credentials, OAuth client-credentials, HTTP client with **429 / `Retry-After`**, and the **single ~2 minute poll loop**.

## Current state

- Daemon + SQLite: `signal_snapshots` / `signal_samples`; config SoT = `~/.daku/environments.json`.
- Probe (do not invent):  
  `GET /api/now/table/sys_properties?sysparm_query=name=glide.war&sysparm_fields=value&sysparm_limit=1`  
  — [docs/research/servicenow-signals.md](../docs/research/servicenow-signals.md), spec §5–6.
- Auth (ADR-0004 / spec §6): **OAuth 2.0 client credentials** for real Environments; **basic** only for PDI stand-ins. Secrets in macOS Keychain (service `daku`, account = environment `id`).
- Outcomes: **reachable** / **unreachable** / **asleep** — distinct from Environment health and from Signal state (003 stores reachability in availability payload; plan 008 must not map asleep → degraded).
- Poll cadence: **one shared loop**, default **120s**, all Environments, all registered collectors (004–007 only register; they do not start their own timers).

## Commands you will need

| Purpose | Command | Expected on success |
|---------|---------|---------------------|
| Classifier tests | `cargo test -p daku-core classify_availability` | all pass |
| HTTP/oauth/429 tests | `cargo test -p daku-core servicenow_http` | all pass |
| Persist tests | `cargo test -p daku-core persist_availability_snapshot` | all pass |
| Check | `cargo check -p daku-core -p daku-daemon` | exit 0 |
| Poll loop test | `cargo test -p daku-core collector_loop` | all pass |

## Scope

**In scope**

1. **Config load** from `~/.daku/environments.json` (example from 002).
2. **Credential resolve** trait + macOS Keychain backend (service `daku`, account = env id). Tests use a fake backend — never real secrets.
3. **OAuth client-credentials** (required for `auth_method: oauth_client_credentials`):
   - Token URL: `{instance_url}/oauth_token.do` (standard ServiceNow; if Operator smoke proves a different path, STOP and report — do not invent).
   - Body: `grant_type=client_credentials` + client id/secret from Keychain (store two Keychain accounts or one JSON blob — document the choice in code comments; tests use fake backend).
   - Cache access token in memory until `expires_in`; on 401 once, refresh and retry once.
   - Basic auth path for `auth_method: basic` only.
4. **Shared HTTP client** used by all Signals:
   - Timeouts; injectability for fixtures.
   - On **429**: honor `Retry-After` (seconds or HTTP-date); retry ≤2 times; then fail probe as unreachable/transient error recorded in payload — unit-test with stub headers (no network).
5. **Availability classifier** + probe → `signal_id = "availability"` snapshot (RTT, build string, reachability).
6. **`CollectorLoop`**: interval default 120s (config key `poll_interval_secs`); runs all registered `SignalCollector`s; 004–007 add registrations only.
7. Fixtures under `crates/daku-core/tests/fixtures/availability/` (`ok.json`, `hibernating.html`, `401.json`).
8. One-shot CLI: `daku-daemon probe-availability` (or equivalent) for smoke.
9. Operator smoke doc without real hostnames.

**Out of scope**

- Other Signal collectors’ business logic (004–007).
- GPUI (009); CI against live PDI; inventing hostnames.

## Git workflow

- Work on `main` (trunk-based). No PRs; no GitHub Actions — see `docs/agents/git-workflow.md`.
- Optional disposable local branch for isolation; merge to `main` locally and delete the branch. Do not push topic branches to `origin`.
- Commit example: keep the imperative message named in each plan's Steps.


## Steps

### Step 1: Fixture classifier

`classify_availability_response(status, content_type, body, rtt_ms) -> AvailabilityObservation` (reachability + build + Signal state).

**Verify**: `cargo test -p daku-core classify_availability` → pass (ok→reachable/healthy; hibernate HTML→asleep; 401→unreachable).

### Step 2: Persist snapshot

**Verify**: `cargo test -p daku-core persist_availability_snapshot` → one row `signal_id=availability` on tempfile DB.

### Step 3: Shared HTTP + 429 + OAuth (fake clock/backend)

Implement `ServiceNowClient` with injectable transport. Tests: 429 with `Retry-After: 1` retries; OAuth token cache + single refresh on 401; basic auth header path.

**Verify**: `cargo test -p daku-core servicenow_http` → pass; **no** sockets in these tests.

### Step 4: Availability collector + CollectorLoop

Register availability; loop interval from config (default 120). Test loop invokes collector twice with fake instant advance **or** with interval=0 / manual `tick()` — prefer explicit `tick()` API for tests.

**Verify**: `cargo test -p daku-core collector_loop` → pass.

### Step 5: Daemon one-shot + docs

**Verify**: `rg -n 'dev[0-9]+\\.service-now' docs README.md` → no matches; `cargo check -p daku-core -p daku-daemon` → exit 0.

## Test plan

| Case | Expected |
|------|----------|
| ok.json | reachable, healthy, build present |
| hibernating.html | asleep (not conflated with unreachable) |
| 401 | unreachable |
| 429 + Retry-After | retries then ok/fail per stub |
| OAuth cache | second call skips token endpoint |
| collector tick | snapshot written without network |

## Done criteria

- [x] `cargo test -p daku-core classify_availability servicenow_http persist_availability_snapshot collector_loop` exit 0
- [x] `cargo check -p daku-core -p daku-daemon` exit 0
- [x] `rg -n 'Retry-After|retry_after' crates/daku-core` → ≥1 hit
- [x] `rg -n 'client_credentials|oauth_token' crates/daku-core` → ≥1 hit
- [x] `rg -n 'poll_interval|CollectorLoop|collector_loop' crates/daku-core` → ≥1 hit
- [x] `plans/README.md` row 003 Status = `DONE`

## STOP conditions

- Plan 002 missing `signal_snapshots`.
- Token URL differs on Operator instance — STOP; do not invent alternate OAuth paths.
- Table API path for `glide.war` differs and cannot be confirmed from research — STOP.
- Request to put Credentials in git or SQLite — refuse.

## Maintenance notes

- **004–007 must not spawn their own timers** — implement `SignalCollector` and register on the loop from 003.
- Plan 008: read availability payload reachability separately from Environment health; **never** map asleep → health degraded.
- Reviewers: asleep ≠ unreachable; 429 tests present; OAuth not “basic-only”.
