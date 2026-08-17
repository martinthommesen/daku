# Plan 008: Environment health rollup + protocol events for UI

> **Executor instructions**: Follow this plan step by step. Run every verification command and confirm the expected result before moving to the next step. If anything in the "STOP conditions" section occurs, stop and report — do not improvise. When done, update the status row for this plan in `plans/README.md`.
>
> **Drift check (run first)**: `git diff --stat 567179a..HEAD -- crates/daku-protocol crates/daku-core crates/daku-daemon crates/daku-client`
> Confirm 003 DONE (snapshots + loop). Missing 004–007 OK (stubs).

## Status

- **Priority**: P1
- **Effort**: M
- **Risk**: MED
- **Depends on**: plans/003-availability-signal.md (stubs OK for 004–007)
- **Category**: direction
- **Planned at**: commit `567179a`, 2026-08-17
- **Issue**: https://github.com/martinthommesen/daku/issues/22

## Why this matters

GPUI (009) must not open SQLite. Spec keeps daemon + versioned protocol + native client (ADR-0001/0003). This plan defines Environment health (exactly three values) and wire events, including **trend samples** for jobs/syslog so 009 can render ~24h sparklines.

## Current state

- Hello handshake retained; replace agent payloads.
- CONTEXT.md **Environment health**: **healthy** | **degraded** | **down** only — no `unknown`.
- Spec §6 **reachability** outcomes: **reachable** | **unreachable** | **asleep** — **separate fields** from health. Asleep must **not** be folded into health=`degraded`.
- Spec §5: unreachable → health `down`; overdue job → health `degraded` (via Signal state).
- `last_clone` is informational (never forces degraded).

## Commands you will need

| Purpose | Command | Expected on success |
|---------|---------|---------------------|
| Rollup tests | `cargo test -p daku-core health_rollup` | all pass |
| Protocol tests | `cargo test -p daku-protocol` | all pass |
| Check | `cargo check -p daku-protocol -p daku-core -p daku-daemon -p daku-client` | exit 0 |
| Client decode | `cargo test -p daku-client protocol_dashboard` | all pass (or module name you choose — must exist) |

## Scope

**In scope**

### 1. Reachability vs health (required types)

```text
Reachability = reachable | unreachable | asleep
EnvironmentHealth = healthy | degraded | down   // exactly these three
```

Rollup rules (pure function + table tests):

| Inputs | health | reachability |
|--------|--------|--------------|
| probe unreachable | `down` | `unreachable` |
| probe asleep | rollup(**Signal** states only; if none, `healthy`) — **do not** set health from asleep itself | `asleep` |
| reachable + any Signal `degraded` | `degraded` | `reachable` |
| reachable + all present Signals `healthy` | `healthy` | `reachable` |
| reachable + no snapshots yet | `healthy` | `reachable` |

Missing Signals are omitted (not degraded). `last_clone` never votes degraded.

### 2. Protocol (minimum — no optional commands)

**Server → client**

- `EnvironmentsUpdated { environments: Vec<EnvironmentSummary> }`  
  `EnvironmentSummary { id, label, platform_id, health, reachability, last_observed_at }`
- `SignalSnapshotsUpdated { environment_id, snapshots: Vec<SignalSnapshotDto> }`  
  `SignalSnapshotDto { signal_id, state, observed_at, payload_json }`
- `SignalSamplesUpdated { environment_id, signal_id, points: Vec<{ observed_at, value_real }> }`  
  Emitted for `jobs` and `syslog` only (≤24h window). Empty `points` allowed.

**Client → server:** Hello only for v1 dashboard subscribe (implicit after Hello). **Do not** add `RefreshNow` or `SelectEnvironment` in v1.

Bump/reset `PROTOCOL_VERSION` for daku domain (e.g. start at `1`).

### 3. Daemon push

After each collector tick: recompute rollup; broadcast the three event types as needed.

**Out of scope**

- Fourth health value; RefreshNow/SelectEnvironment; GPUI layout; Sparkle; SQLite from UI.

## Git workflow

- Branch: `plan/008-health-rollup-protocol`
- Commit example: `Add Environment health rollup and dashboard protocol events`

## Steps

### Step 1: Rollup unit tests

Include explicit case: **asleep + no degraded Signals → health healthy (or last signal rollup), reachability asleep** — not health degraded.

**Verify**: `cargo test -p daku-core health_rollup` → pass; `rg -n 'asleep' crates/daku-core` → tests assert health ≠ degraded solely due to asleep.

### Step 2: Protocol DTOs + round-trip

**Verify**: `cargo test -p daku-protocol` → pass; `rg -n 'EnvironmentHealth|Reachability|SignalSamplesUpdated' crates/daku-protocol` → ≥1 hit each; `rg -n 'RefreshNow|SelectEnvironment' crates/daku-protocol` → no matches.

### Step 3: Daemon broadcast after tick

**Verify**: integration or hub test receives `EnvironmentsUpdated` after fixture tick; `cargo check -p daku-daemon` exit 0.

### Step 4: Client decode

**Verify**: `cargo test -p daku-client protocol_dashboard` → pass (deserialize EnvironmentsUpdated + SignalSamplesUpdated).

## Test plan

| Case | Expected |
|------|----------|
| unreachable | health down, reachability unreachable |
| asleep, signals healthy | health healthy, reachability asleep |
| jobs degraded | health degraded, reachability reachable |
| samples event | points length matches DB window in fixture |

## Done criteria

- [ ] `cargo test -p daku-core health_rollup` exit 0
- [ ] `cargo test -p daku-protocol` exit 0
- [ ] `cargo check -p daku-protocol -p daku-core -p daku-daemon -p daku-client` exit 0
- [ ] `rg -n 'enum EnvironmentHealth|EnvironmentHealth::' crates/daku-core crates/daku-protocol` → only healthy/degraded/down variants (no `Unknown`)
- [ ] `rg -n 'asleep' crates/daku-core` → rollup test proves asleep ↛ degraded
- [ ] `rg -n 'SignalSamplesUpdated' crates/daku-protocol` → ≥1 hit
- [ ] `plans/README.md` row 008 Status = `DONE`

## STOP conditions

- Plan 001 protocol uncompilable — STOP.
- Urge to open SQLite from the UI process — refuse.
- Urge to map asleep → health degraded — refuse (spec §6).

## Maintenance notes

- Plan 009: health dots use `health`; header badge uses `reachability`; sparklines use `SignalSamplesUpdated`.
- Reviewers: three health values only; asleep distinct.
