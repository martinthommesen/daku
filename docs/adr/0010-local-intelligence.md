# Local intelligence: explainer, anomaly strip, correlation, digest

daku collects fifteen Signals per Environment; the Operator's bottleneck is
interpretation, not data. This ADR admits four read-only intelligence
surfaces and fixes where each computes:

* **Health explainer**: one "Label: summary" line per voting Signal that is
  degraded or down, under the Environment header and in `⌘⇧C` copy text.
  Computed client-side from published snapshots so the daemon sends nothing
  new. The non-voting list is shared (`NON_VOTING_SIGNALS` in
  `daku-protocol`): the daemon rollup and the desktop explainer can never
  disagree about what voted.
* **Anomaly strip**: latest raw sample over the same-weekday-hour baseline
  mean from the 30-day hourly roll-ups, shown in the drill-in at 2× and
  above ("4.0× normal for a Monday 09:00"). Client-side, trend Signals
  only, four-bucket minimum, zero baselines never divide. Threshold
  crossings still own alerting; this is context.
* **Correlation note**: a build change in the last 48 h plus a currently
  voting error Signal (jobs, syslog, outbound, flow, MID/ECC) on a degraded
  Environment names the build as likely clone/upgrade fallout, in the
  header and the copy text. One deterministic rule, no model.
* **Digest**: `daku-daemon digest --env <id> [--days 7]` prints Markdown
  over local SQLite (transitions, builds, current states). Read-only, no
  token, no ServiceNow calls — paste it into a status note or an agent
  chat.

Explicitly still out: per-Signal flap history (that is Wave 5 history
storage, not Wave 2 derivation), notification rules (Wave 3), any daemon-to-
network fan-out, any model or heuristic beyond the three rules above. No
protocol change: every surface derives from already-published messages.
