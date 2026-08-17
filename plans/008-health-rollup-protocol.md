# Plan 008: Environment health rollup + protocol events for UI

> **Executor instructions**: Follow this plan step by step. Run every verification command and confirm the expected result before moving to the next step. If anything in the "STOP conditions" section occurs, stop and report — do not improvise. When done, update the status row for this plan in `plans/README.md`.
>
> **Drift check (run first)**: confirm plans 001–003 landed enough that `daku-protocol` / `daku-daemon` / `daku-client` compile. Then `git diff --stat 315f38d..HEAD -- plans/008-health-rollup-protocol.md crates/daku-protocol crates/daku-core crates/daku-daemon`.

## Status

- **Priority**: P1
- **Effort**: M
- **Risk**: MED
- **Depends on**: plans/003-availability-signal.md (stubs OK for Signals 004–007)
- **Category**: direction
- **Planned at**: commit `315f38d`, 2026-08-17
- **Issue**: https://github.com/martinthommesen/daku/issues/22

## Why this matters

The GPUI shell (plan 009) must not query SQLite itself. Spec keeps the **daemon + versioned protocol + native client** split (ADR-0001/0003). This plan defines Environment health rollup rules and the wire events that push snapshots/health to the client so UI work is not blocked on every Signal collector being finished — missing Signals may be stubbed as `unknown` / omitted.

## Current state

- After plan 001: crates `daku-protocol`, `daku-client`, `daku-daemon`, `daku-core` exist (renamed from waku); agent domain stripped; Hello handshake retained (research/waku-reuse: `ClientMessage::Hello` → `ServerMessage::Hello | Rejected`).
- After plan 002–003: SQLite has `signal_snapshots` (+ samples); availability (and later others) write rows.
- CONTEXT.md **Environment health**: rolled-up **healthy** / **degraded** / **down** from Signals; hard-coded defaults, not Operator alert rules.
- Spec §5: unreachable → down; overdue job → degraded; asleep is a collector outcome distinct from health (surface in availability payload / reachability, do not silently map asleep to healthy).
- Plans 004–007 define per-Signal state contributions (cite when present; if a Signal file is not implemented yet, rollup treats missing snapshot as `unknown` and **ignores** it for rollup — do not invent degraded).

## Commands you will need

| Purpose | Command | Expected on success |
|---------|---------|---------------------|
| Protocol tests | `cargo test -p daku-protocol` | all pass |
| Core rollup tests | `cargo test -p daku-core health_rollup` | all pass |
| Check | `cargo check -p daku-protocol -p daku-core -p daku-daemon -p daku-client` | exit 0 |

## Suggested executor toolkit

- Inventory / waku-reuse notes on protocol envelope (keep Hub/handshake/replay; replace agent Commands/Events).
- CONTEXT.md vocabulary for type names (`EnvironmentHealth`, `SignalSnapshot`, …).

## Scope

**In scope**

### 1. Rollup pure function (daku-core)

```text
rollup(reachability, signal_states[]) -> EnvironmentHealth
```

Hard-coded rules (document in `crates/daku-core/src/health.rs` or similar):

| Condition | Environment health |
|-----------|-------------------|
| reachability = unreachable | `down` |
| reachability = asleep | `degraded` (Environment not usable; distinct from Signal failures) **or** a dedicated UI flag — prefer health=`degraded` + availability payload still says asleep |
| any Signal state = `down` (if used) or availability implies unreachable already handled | `down` |
| any present Signal state = `degraded` | `degraded` |
| all present Signals `healthy` (and reachable) | `healthy` |
| no Signal snapshots yet | `unknown` (or `healthy` with `stale: true` — prefer explicit `unknown` in protocol so UI can show “—” / muted dot) |

`last_clone` informational degraded-never (plan 007): if snapshot state is always healthy, it never forces rollup alone.

### 2. Protocol types (daku-protocol)

Replace agent payloads with daku events/commands (names illustrative — match existing enum style in the crate):

- **Server → client events** (minimum):
  - `EnvironmentsUpdated { environments: Vec<EnvironmentSummary> }` — id, label, platform_id, health, last_observed_at
  - `SignalSnapshotsUpdated { environment_id, snapshots: Vec<SignalSnapshotDto> }` — signal_id, state, observed_at, payload_json (string or serde_json::Value)
  - Keep Hello / Rejected / ping machinery from waku envelope
- **Client → server commands** (minimum):
  - `SubscribeDashboard` or implicit subscribe after Hello
  - `SelectEnvironment { id }` (optional if UI filters locally)
  - `RefreshNow` (optional — triggers one poll cycle)

Bump `protocol_version` if the forked constant still says waku’s number — choose a daku starting version (e.g. `1`) and reject mismatches.

### 3. Daemon push path

On snapshot write or poll cycle end: recompute rollup per Environment; broadcast events to connected clients. Unit-test with an in-memory hub if waku left test helpers; otherwise test rollup + serialization round-trip only and a thin daemon integration test.

### 4. Stubs

If Signals 004–007 are missing, daemon may emit only availability (+ empty others). Rollup must still compile and tests must cover stubbed sets.

**Out of scope**

- GPUI layout (009).
- Sparkle (010).
- Alerting, non-loopback exposure, multi-user auth.
- Changing poll cadence semantics beyond exposing `RefreshNow`.

## Git workflow

- Branch: `plan/008-health-rollup-protocol`
- Commit example: `Add Environment health rollup and dashboard protocol events`

## Steps

### Step 1: Rollup unit tests first

Table-driven cases: unreachable→down; asleep→degraded; one degraded Signal→degraded; all healthy→healthy; empty→unknown.

**Verify**: `cargo test -p daku-core health_rollup` → pass.

### Step 2: Protocol DTOs + serde round-trip

Add types; remove leftover agent message variants if any remain after 001.

**Verify**: `cargo test -p daku-protocol` → pass; `rg -n 'Agent|SessionTool|waku' crates/daku-protocol/src` → no agent domain leftovers (allow historical comments sparingly).

### Step 3: Daemon broadcast hook

After availability (or full) poll, emit `EnvironmentsUpdated` + `SignalSnapshotsUpdated`.

**Verify**: integration or hub test with fake client receives both after a fixture poll; `cargo check -p daku-daemon` exit 0.

### Step 4: Client decode smoke

`daku-client` can deserialize the new events (compile-time + one decode test).

**Verify**: `cargo test -p daku-client` → pass (or `cargo check -p daku-client` if tests thin).

## Test plan

| Case | Expected |
|------|----------|
| unreachable | health down |
| jobs degraded only | health degraded |
| only last_clone healthy | health healthy |
| no snapshots | health unknown |
| serde Hello + EnvironmentsUpdated | round-trip |

## Done criteria

- [ ] Rollup rules implemented and tested
- [ ] Protocol events named for Environments/Signals (not agents)
- [ ] Daemon can push updates after a poll
- [ ] `plans/README.md` row 008 → `done`

## STOP conditions

- Plan 001 left protocol uncompilable — stop; do not redesign the envelope; fix compile or return to 001.
- Temptation to let GPUI open SQLite directly — refuse; keep daemon as source of truth.
- Changing Hello auth to remove local shared secret entirely — out of scope; keep loopback + env-based daemon auth from 002.

## Maintenance notes

- Plan 009 consumes these events only.
- When 004–007 land, ensure each collector calls the same “snapshots changed” hook.
- Reviewers: asleep vs unreachable vs degraded Signal must remain distinguishable in payloads even if rollup collapses some to degraded/down.
