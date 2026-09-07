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

/**
 * Bounded health-transition + build-change log (v1.1, ADR-0009).
 * Written by `publish_dashboard`, never by collectors directly.
 */
export const healthEvents = sqliteTable(
  "health_events",
  {
    environmentId: text("environment_id").notNull(),
    /** Event time, unix seconds. */
    observedAt: integer("observed_at").notNull(),
    /** `health` or `build`. */
    kind: text("kind").notNull(),
    /** Previous rollup; null for the bootstrap build event. */
    fromHealth: text("from_health"),
    toHealth: text("to_health").notNull(),
    /** Build string after the change; null for pure health transitions. */
    build: text("build"),
  },
  (table) => [
    primaryKey({
      columns: [table.environmentId, table.observedAt, table.kind],
    }),
    index("health_events_by_env_time").on(
      table.environmentId,
      table.observedAt,
    ),
  ],
);

/**
 * Hourly aggregates over raw samples for 30-day trends (v1.1, ADR-0009).
 * One idempotent recompute of the current hour per publish; the 24 h raw
 * ring stays untouched. `avg_real` draws the line, `max_real` the ticks.
 */
export const signalRollupsHourly = sqliteTable(
  "signal_rollups_hourly",
  {
    environmentId: text("environment_id").notNull(),
    signalId: text("signal_id").notNull(),
    /** Hour bucket start, unix seconds. */
    hourStart: integer("hour_start").notNull(),
    avgReal: real("avg_real"),
    maxReal: real("max_real"),
    sampleCount: integer("sample_count").notNull(),
  },
  (table) => [
    primaryKey({
      columns: [table.environmentId, table.signalId, table.hourStart],
    }),
    index("signal_rollups_by_env_signal_hour").on(
      table.environmentId,
      table.signalId,
      table.hourStart,
    ),
  ],
);

/**
 * Last-published rollup per Environment so `publish_dashboard` can fire a
 * health event only after two consecutive publishes agree (flap suppression)
 * without keeping in-memory daemon state. One row per Environment, bounded.
 */
export const dashboardPublishState = sqliteTable(
  "dashboard_publish_state",
  {
    environmentId: text("environment_id").notNull(),
    lastHealth: text("last_health").notNull(),
    /** Consecutive publishes agreeing with `lastHealth`. */
    consecutive: integer("consecutive").notNull(),
    /** Value before the current streak; the `from_health` of the next event. */
    previousHealth: text("previous_health"),
    /** Last published build string, if any. */
    lastBuild: text("last_build"),
  },
  (table) => [primaryKey({ columns: [table.environmentId] })],
);
