# Signal reference

What the fifteen Signal cards measure, what makes each amber, what the
Operator can tune, and where each link lands. Code truth lives in
`crates/daku-core/src/` (`availability.rs`, `jobs.rs`, `syslog.rs`,
`mid_ecc.rs`, `outbound.rs`, `flow.rs`, `email.rs`, `upgrade.rs`, `sessions.rs`, `table_growth.rs`, `transaction.rs`, `update_sets.rs`, `scan.rs`, `drift.rs`, `last_clone.rs`); rendering truth
in `src/dashboard_state.rs`. Research hedges live in
[`docs/research/servicenow-signals.md`](./research/servicenow-signals.md) —
this page describes what shipped.

Shared semantics:

- Poll cadence: one shared loop, default 120 s (`poll_interval_secs` in
  `~/.daku/settings.json`, floor 30 s, read at daemon start).
- Thresholds: per-Environment `thresholds` object in
  `~/.daku/environments.json` (or the Environment sheet, which edits the
  same values); missing keys fall back to the defaults below,
  unknown keys are rejected. `daku-daemon doctor` prints effective values.
- Outcomes `reachable` · `unreachable` · `asleep` (hibernating PDI) are
  distinct from health. Asleep Environments are skipped, never probed, never
  degraded. Unreachable rolls the Environment `down`.
- Health rollup: unreachable → `down`; asleep → `healthy`; otherwise any
  `down`/`degraded` vote (except `last_clone` and `skipped`, which never
  vote) → `degraded`, else `healthy`.
- Freshness: "polled … ago" tints stale after 5 min, critical after 1 h.
- Mutes (1 h / 4 h / 24 h in the Environment header, `app.json`) silence
  notifications, the menu-bar dot and the Dock badge. The daemon keeps
  collecting.
- Drill-ins open under the cards on selection; every card title links its
  source list in ServiceNow.

| Signal | Source | Default degrade rule | Trend |
|--------|--------|----------------------|-------|
| Availability | `sys_properties` (`glide.war`) | unreachable, or RTT over ceiling | RTT sparkline |
| Scheduled jobs | `sys_trigger` aggregates | ≥1 overdue (error count off) | backlog, 24 h raw + 30 d hourly |
| Syslog errors | `syslog` aggregate, level 2, 1 h | ≥1 error | 24 h raw + 30 d hourly |
| MID / ECC | `ecc_agent` table + `ecc_queue` aggregates | any unhealthy/error, queue ≥100 | point-in-time |
| Outbound | `sys_outbound_http_log` aggregate, 4xx+, 1 h | ≥1 failure | point-in-time |
| Flow errors | `sys_flow_context` aggregate, state ERROR, 1 h | ≥1 error | point-in-time |
| Email failures | `sys_email` aggregate, send-failed, 1 h | opt-in (`off` by default) | point-in-time |
| Upgrades | `sys_upgrade_history` newest rows | ≥1 failed in 7 d | point-in-time |
| Sessions | `v_user_session` row count, cap 100 | never votes (informational) | point-in-time |
| Table growth | whole-table Aggregate counts, 5 tables | never votes (informational) | point-in-time |
| Slow transactions | `syslog_transaction` avg response time, 1 h | opt-in (`off` by default) | point-in-time |
| Update sets | `sys_update_set` newest rows, open states | opt-in (`off` by default) | point-in-time |
| Instance Scan | `scan_finding` newest rows, open P1/P2 | ≥1 P1 open | point-in-time |
| Version / plugins | `sys_plugins` + `sys_store_app` vs clone source | any unexpected mismatch / build differs | point-in-time |
| Last clone | `clone_instance` on the clone source | never votes (informational) | point-in-time |

## Availability — up/latency + build probe

`GET sys_properties?name=glide.war`. HTML containing "hibernat" →
`asleep`/`healthy`. 200 with a `result` array → `reachable`/`healthy` plus
the build string and round-trip ms. Anything else → `unreachable`/`down`
(429 records `HTTP 429`). Transport errors probe as `down` with the error.

Thresholds: `availability_rtt_degraded_ms` (default off). A reachable
Environment slower than the ceiling degrades with `rtt X ms > Y ms` as the
detail. Samples keep RTT only when reachable (a timeout is not latency).

Drill-in: RTT trend (2+ points) else text. Link: `sys_properties` filtered
to `glide.war`. The build string also feeds drift comparison, the compare
strip, health-event build tracking, and "on this build since …".

## Scheduled jobs — overdue / error

Aggregates on `sys_trigger`: overdue Ready
(`state=0`, `next_action` older than 15 min) and state-3 errors.

Thresholds: `jobs_overdue_degraded_at` (1), `jobs_error_degraded_at` (off —
error counts never voted historically). Either count at its ceiling →
`degraded`. Sample: overdue + error.

Rows (fetched only while a count is non-zero, 10 each): overdue oldest
first (waiting longest), errors newest first. Each row links its
`sys_trigger` record. Drill-in prefers rows over the trend. Link: overdue
list. Empty name falls back to `sys_id`.

## Syslog errors — error rate

Aggregate on `syslog`: `level=2` in the last hour.

Threshold: `syslog_error_degraded_at` (1). Sample: the count.

Rows (only while non-zero, 10, newest first): time, source, message
(truncated to 160 chars in storage). Drill-in prefers rows over the trend.
Link: filtered `syslog` list.

## MID / ECC — health + queue backlog

`ecc_agent` table (status, validated, version, host name; 10k cap) plus
`ecc_queue` aggregates: output-ready (7 d window) and error.

Thresholds: `mid_unhealthy_degraded_at` (1), `ecc_error_degraded_at` (1),
`ecc_output_ready_degraded_at` (100). Healthy iff unhealthy below its
ceiling **and** errors below theirs **and** output-ready below 100. An
agent is healthy when status is `Up` and validated is true. Zero MID
servers renders "unknown", not healthy.

Rows: unhealthy agents (10) with status/version. Drill-in lists them, else
text. Link: `ecc_agent` list.

## Outbound — integration failures

Aggregate on `sys_outbound_http_log`: `http_status >= 400` in the last hour.

Threshold: `outbound_failures_degraded_at` (1). No samples.

Rows (only while non-zero, 10, newest first): time, URL, status. Stored
URLs drop query and fragment — paths identify the integration, query
strings can carry third-party secrets. Drill-in rows or text. Link:
filtered log list.

## Flow errors — IntegrationHub / Flow Designer failures

Aggregate on `sys_flow_context`: `state=ERROR` updated in the last hour.
Flow failures are silent by default (no email, no incident), so this is
often the Operator's first notice before a downstream ticket.

Threshold: `flow_error_degraded_at` (1). No samples.

Rows (only while non-zero, 10, newest first): flow name, last update.
Each row links its `sys_flow_context` record. Drill-in rows or text. Link:
filtered flow-context list.

## Email failures — mail that never arrived

Aggregate on `sys_email`: `type=send-failed` created in the last hour.
Off by default — dev Environments fail mail noisily — so the Operator opts
in per Environment with `email_failure_degraded_at` (the sheet accepts
`off` to return to silent). No samples.

Rows (only while non-zero, 10, newest first): subject, recipients, time.
The error string stays out of the snapshot (it can carry addresses and
message bodies). Each row links its `sys_email` record. Drill-in rows or
text. Link: filtered mail list.

## Upgrades — patch history with failure detection

Newest `sys_upgrade_history` rows (10): from/to versions, state,
start/finish. An unnoticed upgrade explains drift and new errors; a failed
one needs action.

Threshold: `upgrade_failed_degraded_at` (1) counts upgrades in the last 7
days whose state reads as failed (`fail`/`error`/`cancel`/`abort`
substring — exact choice values vary by release). Unknown states never
vote; unreadable timestamps count fail-loud. No samples.

Summary names the last target build with its age ("Zurich P1 · 3 days
ago"), or "1 failed · last 7d", or "no upgrades found". Drill-in lists the
rows with links into the history table. Link: upgrade-history list.

## Sessions — who is logged in

Row count on `v_user_session` (the Logged-in-users list), capped at 100 —
"100+ active sessions" past the cap. Capacity context for every other
Signal, and nothing else: this card **never votes** in the health rollup
and has no threshold to tune. A failed read still lands as `down` with the
error, so a lost read ACL is visible instead of silent. No samples, no
rows, no drill-in beyond the count. Link: logged-in-users list.

## Table growth — row counts on the watched tables

Whole-table Aggregate counts on `syslog`, `sys_email`, `ecc_queue`,
`sys_attachment`, and `task` — five cheap reads per tick. Absolute counts
turn "the instance feels slow" into something checkable, and a jump after a
clone or integration change points at the table to look at first.

Like sessions this card **never votes** and has no threshold. A table the
monitoring account cannot read lands as `null` (em dash in the drill-in)
rather than failing the probe; only when every table fails does the Signal
read `down`. Summary totals the readable tables ("5 tables · 211.3K
rows"). No samples today — per-table trends join the roll-ups in a later
wave. Drill-in lists one row per table with links into each list. Link:
table-definition list.

## Slow transactions — mean response time that Operators feel

The availability probe measures one synthetic read; this Signal measures
the mean server-side response time across real transactions in the last
hour (`avg_fields=response_time` on `syslog_transaction`). While over the
ceiling it fetches the 10 slowest transactions for the drill-in.

Threshold: `transaction_avg_degraded_ms` (off by default — slowness varies
wildly per instance, so the Operator opts in; the sheet accepts `off` to
return to silent). No samples. Every query is date-bounded:
`syslog_transaction` is a rotated table.

Summary reads the hourly mean ("842 ms avg · last hour"). Drill-in rows
show time, host, and milliseconds with redacted URLs. Link: filtered
transaction list.

## Update sets — open work per Environment

The 10 newest `sys_update_set` rows with the open ones counted. Stale open
sets collide at commit time and hide unreviewed customizations.

Threshold: `update_sets_open_degraded_at` (off by default — developers keep
work-in-progress sets open while building; the sheet accepts `off` to
return to silent). Open means the state reads as `open`, `build`, or
`progress` (choice values vary by release between "Open" and "In
Progress"); `complete`, `ignore`, and friends never match. No samples.

Summary counts the open sets ("2 open update sets", "no open update
sets"). Drill-in lists them with links into each record. Conflict
detection via the CI/CD preview API is future work — previews are
expensive, role-gated reads that do not belong in a 2-minute poll. Link:
update-set list.

## Instance Scan — outstanding P1/P2 findings

The 50 newest `scan_finding` rows with the open ones counted by priority
(P1 outstanding at its ceiling degrades, default 1; P2 is counted and
listed but never votes alone). "Open" excludes closed/fixed/resolved/
dismissed/ignored states; an empty state counts as open. Priority takes
the leading digit (`1` and `1 - Critical` both read P1).

Instances without the plugin read `skipped` ("Instance Scan unavailable"),
not `down`: there is nothing to observe, and a permanent red card with no
opt-out would be noise. A 403 still reads `down` — the table exists but
the monitoring account cannot read it. No samples. Link: findings list.

## Version / plugins — drift across Environments

Shared collector (runs after the per-Environment groups). Reuses each
Environment's availability build (fresh within two poll intervals) else
probes `glide.war` directly. Compares `sys_plugins` (id, version, active)
and `sys_store_app` (scope/id fallback, version) against the Environment
marked `clone_source`, inventories cached 30 min, pages capped at 1000
(`truncated` flags a partial inventory — counts are a floor then).

Threshold: `drift_mismatches_degraded_at` (1). `expected_drift` lists
plugin ids / store scopes that are planned differences: the card reads "N
differ · M expected" and only unexpected entries vote. `build_matches` is
tri-state — `false` degrades, unknown never does.

Payload: `mismatches` (unexpected), `expected_mismatches`, `build_matches`,
`mismatch_list` (50, full sorted list including expected entries).
The clone source renders "source of truth" and never votes against itself.
Skipped while any side sleeps or the source is unreachable, with the reason
on the card. Drill-in: mismatch table. Link: plugin list.

## Last clone — date from the clone source

Shared collector. Reads `clone_instance` (completed, newest per target, 10
rows) once from the clone source and fans the answer out per target
(host / first-label / id match, case-insensitive). Reports completion time
plus `age_days` ("today", "3 days ago", "not in the last 10 clones", "no
clone found"). **Never votes** in health. Drill-in: one row
(Completed/Age/Source). Link: clone-instance list.

## History the console keeps

- Latest snapshot per Signal × Environment (always).
- Raw samples 24 h: availability RTT, jobs backlog, syslog errors.
- Hourly roll-ups 30 d (avg line, max ticks, sample counts) for the same
  three; drill-in switch 24 h / 7 d / 30 d.
- Bounded health-event log (90 d / 500 per Environment): rollup changes
  confirmed twice (single flaps are not events) and build changes including
  bootstrap. Rendered as the Recent timeline; feeds notifications.
