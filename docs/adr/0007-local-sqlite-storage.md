# Local SQLite under ~/.daku

The daemon stores Signal snapshots and short trends in **SQLite** (waku’s drizzle → SQL → rusqlite/WAL pipeline, schema replaced for daku). On disk under **`~/.daku/`** (e.g. `app.db`), directory mode `0700`, db `0600`. Persist the **latest snapshot** for every Signal × Environment and a **~24h ring** (small buffer OK) for syslog error rate and scheduled-job backlog only; prune aggressively. No alert-history store in v1.

**Amendment (v1.1, ADR-0009):** exactly two history tables are sanctioned, nothing else:

1. `health_events` (plan `072`) — bounded health-transition + build-change log written by `publish_dashboard`, pruned on write.
2. `signal_rollups_hourly` (plan `078`) — idempotent hourly aggregates (avg for latency, max for backlog/error counts), 30 d retention, pruned on write.
> Superseded by ADR-0012: rollups and events keep 90 days (24h raw samples unchanged, 500 per-Environment event cap). This ADR stays as history; do not implement 30d from this file.

The 24 h raw ring stays as-is. No other history, no alert rules store.
