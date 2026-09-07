# Semaphore / transaction Signal — spike

**Question**: should daku grow a semaphore/transaction Signal for ServiceNow
nodes? Spike for issue #97. No code changed.

**Status**: research-only. No live Environment was available in this session,
so no `stats.do` / `xmlstats.do` / `sys_cluster_state` probe was run here.
Everything below marked `[unverified-live]` needs the Operator-run probes in
§3. The recommendation in §4 is conditional on those results.

## 1. What v1 skipped and why

From `docs/research/servicenow-signals.md` row 2 + §Skipped:

* Data lives in `stats.do` (HTML) / `xmlstats.do` (XML) per node: semaphore
  sets (Default, API_INT, AMB_RECEIVE, …), available/max/queue depth, active
  transactions, JVM memory, uptime, build tag. Not the Table API — HTML/XML
  scraping with per-node fan-out.
* Gate: `glide.security.diag_txns_acl=true` makes them admin-only (or
  allow-listed IPs via `glide.custom.ip.authenticate.allow`). Docs disagree
  on the default (Security Center says true/recommended, system-properties
  reference says false) — check the instance. `[unverified-live]`.
* Node list candidate: Table API on `sys_cluster_state` **[table name
  unverified in docs]**.
* PDI is one node on shared hardware — numbers are not meaningful there.
* Alternatives already documented: Stats Tools tables `sys_query_pattern`,
  `sys_script_pattern`, `sys_transaction_pattern` (Table-API readable, admin);
  `syslog_transaction` fields (response_time, sql_count) `[table verified by
  name only]`; Instance Observer (off-instance, paid Impact, no public
  customer REST API documented).

None of the objections is a code problem. All four need a live read-only
probe to answer.

## 2. What this spike verified without a live instance

* Doc citations re-checked: performance-monitoring ACL page, Available system
  properties page, deprecated semaphore-queue-efficiency page, Stats Tools
  page, Instance Observer overview/roles pages (see servicenow-signals.md
  links). No doc was found that publishes an `xmlstats.do` schema or a
  customer REST equivalent for semaphore depth.
* daku-side cost if built: new collector in `crates/daku-core/src/` reusing
  `ServiceNowClient` auth/429 handling, but with an HTML/XML parser daku does
  not have (new dependency or hand-rolled), per-node fan-out (N requests per
  tick, not 1), and a node-identity scheme (`sys_cluster_state` shape
  unknown). Thresholds would be per-semaphore-set, per-node-count dependent —
  a poor fit for the single-threshold-per-Signal shape `073` introduces.
* Privacy: node hostnames are Environment internals — payload must store
  counts/depths, never raw hostnames, same rule as outbound URL hosts.

## 3. Operator-run probes (open)

Run with a read-only OAuth user (`snc_read_only` + custom read role, the same
account daku polls with). Redact hostnames before pasting anything into the
tracker.

1. `curl -u oauth-token 'https://<env>/xmlstats.do' | head -c 4000` — record:
   root element, per-node element, semaphore field names for available / max /
   queue depth, active transactions, JVM, uptime, build tag. **[unverified-live]**
2. Same URL with the read-only user vs an admin — 200 vs 403? Decides whether
   the Signal is even pollable without widening the daku role.
   **[unverified-live]**
3. `sys_cluster_state` Table API list (`sysparm_limit=5`, fields
   `sys_id,node,state` or whatever the describe returns) — reachable? field
   names? ACL-filtered short page? **[unverified-live]**
4. `glide.security.diag_txns_acl` value on the instance + whether an
   allow-listed IP bypass exists that daku could use. **[unverified-live]**

Expected outputs: exact XML shape (§1 guess becomes fact), yes/no on
read-only visibility, node-list table shape, ACL posture.

## 4. Conditional recommendation

* **If probes 1–4 show read-only XML + stable node list**: build plan sketch —
  Signal id `semaphores`, per-node queue-depth max + available-min payload
  (counts only, hostnames dropped), thresholds
  `semaphore_queue_degraded_at` / `semaphore_available_min` in the `073`
  thresholds object, health `degraded` when any tracked set queues
  (never `down` — exhaustion degrades before it pages), point-in-time (no
  samples until `078` roll-ups prove useful), drill-in lists worst sets with
  deep-link to `stats.do`. Cost: XML parser + N-request fan-out + node
  matching. Revisit only if prod nodes matter and Instance Observer is not
  licensed.
* **If any probe fails (403 for read-only, no stable XML, no node list)**:
  documented **no**. Keep the Stats Tools / `syslog_transaction` /
  Instance Observer alternatives in servicenow-signals.md as the answer, and
  do not add an admin-only scraper to an Operator console that promises
  read-only polling.

My judgement on today's evidence: lean **no** unless the Operator's probes
contradict the admin-only reading — the role widening alone breaks the
ADR-0004 promise, and the per-node fan-out breaks the polite-aggregate
posture, for a failure mode Instance Observer already covers where licensed.
The probes above are cheap and settle it either way.
