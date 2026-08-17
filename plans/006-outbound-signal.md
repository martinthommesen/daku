# Plan 006: Outbound / integration failures Signal

> **Executor instructions**: Follow this plan step by step. Run every verification command and confirm the expected result before moving to the next step. If anything in the "STOP conditions" section occurs, stop and report — do not improvise. When done, update the status row for this plan in `plans/README.md`.
>
> **Drift check (run first)**: confirm plan 003 collector pattern exists. Then `git diff --stat da67ae9..HEAD -- plans/006-outbound-signal.md crates/daku-core`.

## Status

- **Priority**: P2
- **Effort**: M
- **Risk**: MED
- **Depends on**: plans/003-availability-signal.md
- **Category**: direction
- **Planned at**: commit `da67ae9`, 2026-08-17
- **Issue**: https://github.com/martinthommesen/daku/issues/21

## Why this matters

Silent integration failure is an explicit v1 pain (spec Signal #7). Research points at Aggregate counts on outbound HTTP logs plus email send-failures — enough for a glance without parsing Flow Designer deeply in v1.

## Current state

- Collector patterns from 003/004.
- Research — [servicenow-signals](https://github.com/martinthommesen/daku/blob/research/servicenow-signals/docs/research/servicenow-signals.md) row “Integration / outbound errors”:

  **Primary:** Aggregate on `sys_outbound_http_log` for recent non-success HTTP statuses, e.g. count where `http_status>=400` and `sys_created_on>javascript:gs.hoursAgoStart(1)` (table name from developer blog / research — do not invent another log table).

  **Secondary (include if Aggregate works on fixtures + optional live smoke):** `sys_email` with `type=send-failed` (and state Error) in the same 1h window — count only.

  **Deferred in this plan (do not implement unless free):** `sys_flow_context` state=ERROR — note in maintenance as follow-up; v1 card can ship with HTTP log + email only.

- Point-in-time Signal (no required 24h ring; optional sample append is OK but not required).
- CONTEXT.md: `signal_id = "outbound"`.

## Commands you will need

| Purpose | Command | Expected on success |
|---------|---------|---------------------|
| Tests | `cargo test -p daku-core outbound` | all pass |
| Check | `cargo check -p daku-core -p daku-daemon` | exit 0 |

## Suggested executor toolkit

- Same HTTP Aggregate helper as jobs/syslog (004) if already landed; otherwise implement a shared `aggregate_count(table, query)` helper here and leave a note for 004 to reuse (prefer not duplicating forever — if 004 not merged yet, put helper in `daku-core` shared module).

## Scope

**In scope**

- Collector `signal_id = "outbound"`:
  - Payload:

    ```json
    {
      "outbound_http_4xx_5xx_1h": 0,
      "email_send_failed_1h": 0
    }
    ```

  - State:
    - `healthy` if both counts are 0
    - `degraded` if either count > 0
    - probe failure → unreachable path from 003
  - If `sys_email` Aggregate returns 403 but HTTP log works: store email count as `null` in payload and still produce healthy/degraded from HTTP only; log once at warn. Do **not** fail the whole Signal solely because email ACL is missing.
- Fixtures under `crates/daku-core/tests/fixtures/outbound/`.
- Snapshot persistence + daemon wire.
- Code comment: outbound DB logging properties exist (`glide.outbound_http.db.log` etc.) — Operator may see zeros if logging disabled; document in Operator smoke doc.

**Out of scope**

- Scraping IntegrationHub UI.
- Alerting / webhooks.
- ECC errors (covered by 005).
- Real hostnames or request URLs that look like customer systems in fixtures — use `https://api.example.com/...` only if a URL field appears in fixtures.

## Git workflow

- Branch: `plan/006-outbound-signal`
- Commit example: `Add outbound integration failures Signal`

## Steps

### Step 1: Fixture Aggregate parsers for outbound + email

**Verify**: `cargo test -p daku-core parse_outbound_counts` → pass.

### Step 2: Classifier + snapshot

**Verify**: `cargo test -p daku-core outbound_signal` → zero=healthy; any>0=degraded.

### Step 3: HTTP collectors + soft email ACL

**Verify**: test where email call returns 403 → snapshot still written from HTTP counts.

### Step 4: Daemon + Operator smoke doc

No real instance hostnames in repo docs.

**Verify**: `rg -n 'dev[0-9]+\\.service-now' docs README.md` → no matches; `cargo check` exit 0.

## Test plan

| Case | Expected |
|------|----------|
| both zero | healthy |
| http failures > 0 | degraded |
| email failures > 0 | degraded |
| email 403, http ok | snapshot with email null/omitted; state from http |

## Done criteria

- [ ] Fixture tests green, no network
- [ ] Encoded queries match research tables `sys_outbound_http_log` and `sys_email`
- [ ] `plans/README.md` row 006 → `done`

## STOP conditions

- `sys_outbound_http_log` does not exist / always 404 on Operator’s family — stop; do not invent alternate table names; cite research and ask for a research refresh.
- Pressure to commit request bodies that contain live credentials from outbound logs — never persist raw log rows in daku DB; counts only.

## Maintenance notes

- Follow-up: `sys_flow_context` ERROR counts.
- Plan 008: any outbound degraded → Environment degraded.
- Reviewers: counts only, no log body retention.
