# Local SQLite under ~/.daku

The daemon stores Signal snapshots and short trends in **SQLite** (waku’s drizzle → SQL → rusqlite/WAL pipeline, schema replaced for daku). On disk under **`~/.daku/`** (e.g. `app.db`), directory mode `0700`, db `0600`. Persist the **latest snapshot** for every Signal × Environment and a **~24h ring** (small buffer OK) for syslog error rate and scheduled-job backlog only; prune aggressively. No alert-history store in v1.
