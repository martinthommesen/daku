/**
 * daku local SQLite schema — Signal snapshots and ~24h samples only.
 *
 * Drizzle is a build-time tool: `bun run db:generate` diffs this file and
 * writes plain SQL into `db/migrations`, which the Rust app applies at startup
 * (see `apply_migrations` in `crates/daku-core`). drizzle-orm never ships in
 * the binary — Rust owns every query.
 *
 * Never edit or regenerate a migration that has shipped — only append a new
 * `NNNN_*.sql`; the Rust runner identifies applied migrations by the numeric
 * prefix.
 *
 * Environments live in `~/.daku/environments.json` (ADR-0004), not here.
 * Credentials stay in the macOS Keychain — never SQLite.
 */

import {
  index,
  integer,
  primaryKey,
  real,
  sqliteTable,
  text,
} from "drizzle-orm/sqlite-core";

/** Latest observation per Environment × Signal. */
export const signalSnapshots = sqliteTable(
  "signal_snapshots",
  {
    environmentId: text("environment_id").notNull(),
    signalId: text("signal_id").notNull(),
    /** Observation time, unix seconds. */
    observedAt: integer("observed_at").notNull(),
    state: text("state").notNull(),
    payloadJson: text("payload_json").notNull(),
  },
  (table) => [
    primaryKey({ columns: [table.environmentId, table.signalId] }),
  ],
);

/** Short trend ring (~24h) for Signals that need samples (jobs, syslog). */
export const signalSamples = sqliteTable(
  "signal_samples",
  {
    environmentId: text("environment_id").notNull(),
    signalId: text("signal_id").notNull(),
    /** Sample time, unix seconds. */
    observedAt: integer("observed_at").notNull(),
    valueReal: real("value_real"),
    valueJson: text("value_json"),
  },
  (table) => [
    index("signal_samples_by_env_signal_time").on(
      table.environmentId,
      table.signalId,
      table.observedAt,
    ),
  ],
);
