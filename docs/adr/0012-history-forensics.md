# History and forensics: flap log, annotations, export, comparison

Wave 2 derived intelligence from existing history; this ADR admits the
storage and surfaces that make history itself useful:

* **Per-Signal flap log** (`signal_events` + `signal_publish_state`,
  migration `0003`): every non-skipped Signal confirms transitions over two
  consecutive publishes, exactly like health events. Same bounds (90 d /
  500 per Environment), same wire pattern (`SignalEventsUpdated` with its
  own replay key), merged into the Recent timeline beside health events.
  Skipped ticks leave streaks untouched.
* **Annotations** (`health_events.note`, same migration): one Operator note
  per health event, written over the authenticated loopback RPC
  (`AddHealthEventNote`, capped at 500 chars, empty clears) and rendered in
  the timeline. Re-publishes never overwrite notes; unknown targets report
  zero rows instead of creating history.
* **Retention**: roll-ups 30 d → 90 d. Raw samples stay 24 h; the anomaly
  baseline window grows with the roll-ups for free.
* **Export** (`⌘⇧E`, menu): `snapshots.json` + `trends.csv` +
  `summary.md` into `~/.daku/exports/<id>-<unix>/`, no dialogs — the
  footer flashes the directory.
* **Timeline search**: substring filter over merged timeline text in the
  Recent header; click a health row to annotate it.
* **Week comparison**: trailing-7d vs prior-7d roll-up means beside the
  trend switch ("4.0 avg · +100% vs prior 7d"), each side needing a day of
  hourly buckets. A text delta, not a chart overlay — the sparkline stays
  single-series.

Still out: cross-Environment history queries, retention tuning knobs,
chart overlays, note editing history. Protocol 8 → 9 (new message,
command, and two DTO fields — all additive).
