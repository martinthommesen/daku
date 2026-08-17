# Signal drill-in: what detail exists, and how far to take it

Plan 038 landed `card_detail` — the daemon's persisted `error`/`detail`
string rendered under each Signal card. This note decides what (if
anything) comes next, i.e. whether cards become drillable into per-row
detail (issues #12/#13, "each Signal has its own state on drill-in").

## What detail exists but is not persisted

- **MID/ECC** — `crates/daku-core/src/mid_ecc.rs:18` already fetches
  `status,validated,version,host_name` for every `ecc_agent`, but
  `mid_ecc.rs:183-196` persists only `agents_total`, `agents_unhealthy`,
  `ecc_output_ready`, `ecc_error`. The per-MID rows are fetched and
  thrown away every tick — the cheapest possible drill-in.
- **Drift** — compares Environments and persists `mismatches` as a count
  (`crates/daku-core/src/drift.rs`); the mismatch list itself is plan 043.
- **Jobs / syslog / outbound** — aggregate-count probes only. The queries
  are `sysparm_count=true` stats calls
  (`jobs.rs:16-18`, `syslog.rs:18-21`, `outbound.rs:16`), so there are no
  rows in the response at all. Row-level detail means *new* Table API
  queries on `sys_trigger`, `syslog`, `sys_outbound_http_log` — more
  requests per tick, more payload, more scope.

## Options

**(a) Bounded lists in `payload_json`.** Persist the first N (say 10)
offending rows next to the counts and render them in an expandable card.
ADR-0007 ("latest snapshot, prune aggressively") holds as long as the
list is bounded and per-snapshot, not history. Cost: payload growth on
every tick, in proportion to N.

**(b) Card selection + a detail region.** A `selected_signal` on
`DashboardState` (the prototype already makes cards selectable,
`prototypes/environments-overview/index.html:420-421`) plus a region
under the card grid. Pure client work; useless on its own unless (a)
gives it something to show beyond `card_detail`.

**(c) Deep link "Open in ServiceNow".** Reuse the same encoded query as
the probe to build a list URL. Needs `instance_url` on
`EnvironmentSummary` (plan 039) and nothing else — no new requests, no
payload growth. It does break the "glance without opening ServiceNow"
promise, but only for the step where the Operator has decided to act
anyway.

## Recommendation

Do **(a)+(b) for MID/ECC only**, and **(c) for jobs, syslog, outbound**;
leave drift to plan 043, which already owns its mismatch list. MID/ECC is
the one Signal whose detail is already in hand — persisting ten unhealthy
agents costs one `json!` line and no extra HTTP, and "which MID is down"
is exactly the spec's dead-MID pain (`docs/spec/v1.md` §5). For the three
count-only Signals the same detail costs a second Table API request per
tick per Environment forever; a deep link costs nothing and lands the
Operator on a better list than we would render. `card_detail` stays the
first line of the detail region — the region is added around it, never
replacing it.

Follow-up plan stub: **"Persist unhealthy MID agents and add a Signal
detail region"** — in scope `crates/daku-core/src/mid_ecc.rs` (bounded
`agents_unhealthy_list` in the payload), `src/dashboard_state.rs`
(`selected_signal`, `card_rows`), `src/app.rs` (card `on_click`, detail
region). Estimate: M.

## Open questions

1. Is "which MID is unhealthy, by host name" enough, or does the Operator
   want the ECC queue error messages too?
2. Should a deep link open the ServiceNow list in the browser, or is
   leaving the app a non-goal for v1?
3. Ten rows — right bound, or is one row (the worst) all a glance needs?
