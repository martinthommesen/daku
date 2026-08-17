# Plan 003: Availability Signal (build/latency probe); fixtures + local PDI smoke

> **Executor instructions**: Follow this plan step by step. Run every verification command and confirm the expected result before moving to the next step. If anything in the "STOP conditions" section occurs, stop and report — do not improvise. When done, update the status row for this plan in `plans/README.md`.
>
> **Drift check (run first)**: confirm plans 001–002 done (`cargo check -p daku-daemon`; migrations test green). Then `git diff --stat b670982..HEAD -- plans/003-availability-signal.md`.

## Status

- **Priority**: P1
- **Effort**: M
- **Risk**: MED
- **Depends on**: plans/002-daemon-sqlite-skeleton.md
- **Category**: direction
- **Planned at**: commit `b670982`, 2026-08-17
- **Issue**: https://github.com/martinthommesen/daku/issues/20

## Why this matters

Availability is Signal #1 in the accepted spec: up/latency + build via Table API probe on `sys_properties` `name=glide.war`. Shipping this first proves the collector loop, credential read path, reachable/unreachable/asleep semantics (ADR-0004), and snapshot persistence — before investing in GPUI or the other six Signals.

## Current state

- Daemon + SQLite skeleton from plan 002 (`signal_snapshots` table).
- Research (do not invent endpoints):  
  `GET /api/now/table/sys_properties?sysparm_query=name=glide.war&sysparm_fields=value&sysparm_limit=1`  
  — see [servicenow-signals research](https://github.com/martinthommesen/daku/blob/research/servicenow-signals/docs/research/servicenow-signals.md) and spec §5.
- Auth: OAuth client credentials preferred for real Environments; **basic allowed for PDI stand-ins** (ADR-0004). Secrets from macOS Keychain (daku-owned service); never commit them.
- Collector outcomes: **reachable** / **unreachable** / **asleep** (PDI hibernate HTML) vs Signal state healthy/degraded/down.
- Poll cadence target ~2 minutes (spec) — implementation may use a shorter interval in dev via config.
- CONTEXT.md: this Signal is named observations on an **Environment**.

## Commands you will need

| Purpose | Command | Expected on success |
|---------|---------|---------------------|
| Unit/fixture tests | `cargo test -p daku-core availability` | all pass |
| Check | `cargo check -p daku-core -p daku-daemon` | exit 0 |
| Local smoke (Operator only) | documented curl/probe using **local** creds | 200 JSON with `result[0].value` — **not run in CI** |

## Suggested executor toolkit

- `.claude/skills/now-sdk/SKILL.md` for Operator-local `npx @servicenow/sdk query` smoke (optional).
- ADR-0004; signals research note; spec §5–6.

## Scope

**In scope**

- Module `daku-core` (or `daku-core/src/collector/`) implementing:
  - Config load from `~/.daku/environments.json` (example shape from plan 002).
  - Credential resolve stub interface + Keychain backend on macOS (service name e.g. `daku`, account = environment id) — **no secret values in tests**.
  - HTTP client probe for availability:
    - Measure RTT.
    - Parse JSON `result[0].value` as build string when `Content-Type` is JSON and status 2xx.
    - If body looks like hibernate HTML / non-JSON 200 → outcome `asleep`.
    - Transport/TLS/auth failures → `unreachable`.
  - Map to Signal state: reachable+ok → `healthy`; unreachable → treat Environment probe as `down` for this Signal; asleep → distinct state stored in payload (UI later).
  - Persist one row to `signal_snapshots` with `signal_id = "availability"`.
- Daemon timer or one-shot CLI subcommand `daku-daemon probe-availability` for easy testing.
- **Fixtures**: checked-in HTTP response bodies under `crates/daku-core/tests/fixtures/availability/` (`ok.json`, `hibernating.html`, `401.json`) — no real build strings required; fake `glide-australia-fake.zip` is fine.
- Unit tests that run the parser/classifier against fixtures (no network).

**Out of scope**

- Other Signals (004–007).
- GPUI display (009) — console/log or DB row is enough.
- CI calling a real PDI.
- Recording real hostnames in fixtures or source.

## Git workflow

- Branch: `plan/003-availability-signal`
- Commit example: `Add availability Signal probe with fixtures`

## Steps

### Step 1: Fixture classifier

Implement pure functions:

- `classify_availability_response(status, content_type, body, rtt_ms) -> AvailabilityObservation`

Cover fixtures: ok JSON → healthy + build value; hibernate HTML → asleep; 401/403 → unreachable; empty/connection error path unit-tested via `Result`.

**Verify**: `cargo test -p daku-core classify_availability` → pass.

### Step 2: Persist snapshot

Given an Environment id + observation, upsert/insert `signal_snapshots`.

**Verify**: `cargo test -p daku-core persist_availability_snapshot` using tempfile DB from plan 002 helpers.

### Step 3: HTTP probe (injectable client)

Trait or param for HTTP GET so tests inject fixtures; production uses `reqwest` (or existing HTTP stack from waku if present) with timeout.

**Verify**: test with mock/fixture client → snapshot written; **no** network in `cargo test`.

### Step 4: Wire daemon one-shot + optional interval

- CLI: probe all configured Environments once and exit (or log results).
- Config key for interval defaulting toward 120s (spec).

**Verify**: `cargo run -p daku-daemon -- probe-availability` with `DAKU_DB_PATH` + example env file using **example.com** URLs expects unreachable (network to example) **or** skip network by pointing at a local wiremock — prefer fixture-injected integration test over flaky DNS.

### Step 5: Operator-local smoke doc

In `README.md` or `docs/examples/availability-smoke.md`: steps to put PDI URL in `~/.daku/environments.json`, store basic auth in Keychain, run probe once. **Do not** write the Operator’s hostname into the repo.

**Verify**: doc exists; `rg -n 'dev[0-9]+\\.service-now' docs README.md` → no matches.

## Test plan

| Case | Expected |
|------|----------|
| ok.json fixture | healthy, payload contains build string |
| hibernating.html | asleep |
| 401 | unreachable |
| DB persist | one snapshot row for `availability` |
| No network in default `cargo test` | enforced |

## Done criteria

- [ ] Fixture tests green with no network
- [ ] Snapshots land in SQLite via override path
- [ ] Daemon exposes a one-shot probe entrypoint
- [ ] Operator smoke documented without real hostnames
- [ ] `plans/README.md` row 003 → `done`

## STOP conditions

- Plan 002 schema missing `signal_snapshots`.
- Pressure to commit real credentials or hostnames.
- Table API path differs on the Operator’s instance and fixtures cannot be updated without inventing undocumented APIs — stop and cite research note before changing the endpoint.

## Maintenance notes

- Plans 004–007 copy this collector module layout.
- Plan 008 reads `signal_snapshots` for Environment health rollup.
- Reviewers: ensure asleep ≠ unreachable ≠ down conflation stays explicit in the type.
