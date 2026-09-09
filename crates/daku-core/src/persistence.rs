//! Daemon-owned SQLite storage helpers.

use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

#[cfg(unix)]
use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};

use rusqlite::{Connection, TransactionBehavior, params};

use daku_protocol::SignalState;

include!(concat!(env!("OUT_DIR"), "/migrations.rs"));

const MIGRATIONS_TABLE: &str = "CREATE TABLE IF NOT EXISTS migrations (
         tag        TEXT PRIMARY KEY,
         applied_at INTEGER NOT NULL
     )";

const DAKU_DB_PATH_ENV: &str = "DAKU_DB_PATH";

fn unix_time() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_secs())
        .unwrap_or(0)
}

fn to_io_error(error: impl std::error::Error + Send + Sync + 'static) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, error)
}

/// Brings a database up to the latest schema.
pub fn apply_migrations(connection: &Connection) -> io::Result<usize> {
    connection
        .execute_batch(MIGRATIONS_TABLE)
        .map_err(to_io_error)?;
    let mut applied = 0;
    for (tag, sql) in MIGRATIONS {
        // Identity is the numeric prefix build.rs enforces (`0000_…`), not
        // drizzle's random suffix, so regenerating a migration's name never
        // re-applies it on an existing database.
        let prefix = tag.split('_').next().unwrap_or(tag);
        let already_applied: bool = connection
            .query_row(
                "SELECT EXISTS(SELECT 1 FROM migrations WHERE substr(tag, 1, ?1) = ?2)",
                params![prefix.len() as i64, prefix],
                |row| row.get(0),
            )
            .map_err(to_io_error)?;
        if already_applied {
            continue;
        }
        // Collector threads each open their own connection; IMMEDIATE takes
        // the write lock before the re-check, so a concurrent opener either
        // applies the migration or sees it applied — never both.
        let transaction =
            rusqlite::Transaction::new_unchecked(connection, TransactionBehavior::Immediate)
                .map_err(to_io_error)?;
        let already_applied: bool = transaction
            .query_row(
                "SELECT EXISTS(SELECT 1 FROM migrations WHERE substr(tag, 1, ?1) = ?2)",
                params![prefix.len() as i64, prefix],
                |row| row.get(0),
            )
            .map_err(to_io_error)?;
        if already_applied {
            continue;
        }
        transaction
            .execute_batch(sql)
            .map_err(|error| io::Error::other(format!("migration {tag} failed: {error}")))?;
        transaction
            .execute(
                "INSERT INTO migrations(tag, applied_at) VALUES(?1, ?2)",
                params![tag, unix_time() as i64],
            )
            .map_err(to_io_error)?;
        transaction.commit().map_err(to_io_error)?;
        applied += 1;
    }
    Ok(applied)
}

/// Ensures the db file exists as `0o600`. Sets the parent `0o700` when daku
/// created it, or when it is literally named `.daku` — never a pre-existing
/// directory the Operator pointed `DAKU_DB_PATH` at.
pub fn ensure_daku_dir(db_path: &Path) -> io::Result<()> {
    if let Some(parent) = db_path
        .parent()
        .filter(|parent| !parent.as_os_str().is_empty())
    {
        #[cfg(unix)]
        let existed = parent.exists();
        fs::create_dir_all(parent)?;
        #[cfg(unix)]
        if !existed
            || parent
                .file_name()
                .is_some_and(|name| name == std::ffi::OsStr::new(".daku"))
        {
            fs::set_permissions(parent, fs::Permissions::from_mode(0o700))?;
        }
    }
    if !db_path.exists() {
        let mut options = fs::OpenOptions::new();
        options.write(true).create_new(true);
        #[cfg(unix)]
        options.mode(0o600);
        match options.open(db_path) {
            Ok(_) => {}
            Err(error) if error.kind() == io::ErrorKind::AlreadyExists => {}
            Err(error) => return Err(error),
        }
    }
    #[cfg(unix)]
    fs::set_permissions(db_path, fs::Permissions::from_mode(0o600))?;
    Ok(())
}

#[derive(Clone)]
pub struct StateStore {
    path: PathBuf,
}

impl StateStore {
    /// Default DB path: `DAKU_DB_PATH`, else `<home>/app.db` where `<home>`
    /// is `DAKU_HOME` when set, else `~/.daku/`.
    pub fn default_path() -> PathBuf {
        if let Ok(path) = std::env::var(DAKU_DB_PATH_ENV)
            && !path.is_empty()
        {
            return PathBuf::from(path);
        }
        crate::config::daku_home_dir().join("app.db")
    }

    pub fn daemon(path: PathBuf) -> Self {
        Self { path }
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    pub fn open(&self) -> io::Result<Connection> {
        // Collector threads open concurrently (plan 022); `PRAGMA journal_mode
        // = WAL` on a fresh file returns SQLITE_BUSY under contention even with
        // a busy handler, so serialise opens within the process.
        static OPEN_LOCK: parking_lot::Mutex<()> = parking_lot::Mutex::new(());
        let _guard = OPEN_LOCK.lock();
        ensure_daku_dir(&self.path)?;
        let connection = Connection::open(&self.path).map_err(to_io_error)?;
        // One connection per collector thread: wait for a writer instead of
        // failing straight away with SQLITE_BUSY.
        connection
            .busy_timeout(Duration::from_secs(5))
            .map_err(to_io_error)?;
        connection
            .execute_batch("PRAGMA journal_mode = WAL; PRAGMA synchronous = NORMAL;")
            .map_err(to_io_error)?;
        apply_migrations(&connection)?;
        // Bound the WAL sidecar: checkpoint every 1000 pages and cap the
        // journal, so a long-lived daemon cannot grow -wal without bound
        // between checkpoints.
        connection
            .execute_batch(
                "PRAGMA wal_autocheckpoint = 1000; PRAGMA journal_size_limit = 67108864;",
            )
            .map_err(to_io_error)?;
        // WAL may recreate sidecar modes; re-assert the main db file mode.
        #[cfg(unix)]
        fs::set_permissions(&self.path, fs::Permissions::from_mode(0o600))?;
        Ok(connection)
    }

    /// Enforces retention once at daemon startup (not on every open, so
    /// tests with fake timestamps stay deterministic): a stalled daemon
    /// prunes on restart instead of retaining beyond bounds until the next
    /// tick. Best-effort; failures are ignored.
    pub fn prune_all_for_startup(&self) {
        use std::time::{SystemTime, UNIX_EPOCH};
        let Ok(connection) = self.open() else {
            return;
        };
        let now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|d| d.as_secs() as i64)
            .unwrap_or(0);
        let environments: Vec<String> = connection
            .prepare(
                "SELECT environment_id FROM health_events UNION SELECT environment_id FROM signal_events",
            )
            .and_then(|mut stmt| {
                stmt.query_map([], |row| row.get(0))?
                    .collect::<Result<_, _>>()
            })
            .unwrap_or_default();
        for environment_id in &environments {
            let _ = prune_health_events(&connection, environment_id, now);
            let _ = prune_signal_events(&connection, environment_id, now);
        }
        let _ = prune_signal_samples(&connection, now);
        let _ = prune_signal_rollups(&connection, now);
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SignalSnapshot {
    pub environment_id: String,
    pub signal_id: String,
    pub observed_at: i64,
    pub state: String,
    pub payload_json: String,
}

/// Read-only census of the daemon database for `doctor`: file size plus row
/// counts per history table. A missing file — or a table from a migration
/// the file predates — reads as zero. Never creates, migrates, or writes:
/// opening read-only is what keeps `doctor` a diagnosis.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct DbStats {
    pub bytes: u64,
    pub snapshots: i64,
    pub samples: i64,
    pub rollups: i64,
    pub health_events: i64,
}

pub fn db_stats(path: &Path) -> DbStats {
    let bytes = fs::metadata(path).map(|meta| meta.len()).unwrap_or(0);
    let Ok(connection) =
        Connection::open_with_flags(path, rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY)
    else {
        return DbStats {
            bytes,
            ..Default::default()
        };
    };
    // Table names are internal constants, never Operator input.
    let count = |table: &str| -> i64 {
        connection
            .query_row(&format!("SELECT COUNT(*) FROM {table}"), [], |row| {
                row.get(0)
            })
            .unwrap_or(0)
    };
    DbStats {
        bytes,
        snapshots: count("signal_snapshots"),
        samples: count("signal_samples"),
        rollups: count("signal_rollups_hourly"),
        health_events: count("health_events"),
    }
}

/// `doctor` warns past this database size: retention is automatic, so a
/// bigger file means more Environments or Signals than the census covers.
pub const DB_SIZE_WARNING_BYTES: u64 = 200 * 1024 * 1024;

/// One-line `doctor` rendering of a census: `db <name> · 41 KB ·
/// snapshots 21 · samples 1440 · rollups 2160 · health events 3`.
pub fn format_db_stats(path: &Path, stats: &DbStats) -> String {
    let name = path
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or("app.db");
    let size = if stats.bytes < 1024 {
        format!("{} B", stats.bytes)
    } else if stats.bytes < 1024 * 1024 {
        format!("{} KB", stats.bytes / 1024)
    } else {
        format!("{} MB", stats.bytes / (1024 * 1024))
    };
    let line = format!(
        "db {name} · {size} · snapshots {} · samples {} · rollups {} · health events {}",
        stats.snapshots, stats.samples, stats.rollups, stats.health_events
    );
    if stats.bytes > DB_SIZE_WARNING_BYTES {
        format!("{line} · WARNING over 200 MB — reduce Environments or Signals")
    } else {
        line
    }
}

/// Records that `signal_id` deliberately skipped probing (`reason` is
/// `"asleep"` or `"unreachable"` — the Availability outcome it deferred to).
pub fn persist_signal_skipped(
    connection: &Connection,
    environment_id: &str,
    signal_id: &str,
    observed_at: i64,
    reason: &str,
) -> io::Result<()> {
    let payload = serde_json::json!({ "skipped": reason });
    persist_signal_snapshot(
        connection,
        environment_id,
        signal_id,
        observed_at,
        SignalState::Skipped,
        &payload.to_string(),
    )
}

/// The standard "probe failed" snapshot every Signal writes.
pub fn persist_signal_down(
    connection: &Connection,
    environment_id: &str,
    signal_id: &str,
    observed_at: i64,
    message: &str,
) -> io::Result<()> {
    let payload = serde_json::json!({
        "reachability": "unreachable",
        "detail": message,
    });
    persist_signal_snapshot(
        connection,
        environment_id,
        signal_id,
        observed_at,
        SignalState::Down,
        &payload.to_string(),
    )
}

pub fn persist_signal_snapshot(
    connection: &Connection,
    environment_id: &str,
    signal_id: &str,
    observed_at: i64,
    state: SignalState,
    payload_json: &str,
) -> io::Result<()> {
    connection
        .execute(
            "INSERT INTO signal_snapshots (
                environment_id, signal_id, observed_at, state, payload_json
             ) VALUES (?1, ?2, ?3, ?4, ?5)
             ON CONFLICT(environment_id, signal_id) DO UPDATE SET
                observed_at = excluded.observed_at,
                state = excluded.state,
                payload_json = excluded.payload_json",
            params![
                environment_id,
                signal_id,
                observed_at,
                state.as_str(),
                payload_json
            ],
        )
        .map_err(to_io_error)?;
    Ok(())
}

pub fn load_all_signal_snapshots(connection: &Connection) -> io::Result<Vec<SignalSnapshot>> {
    let mut statement = connection
        .prepare(
            "SELECT environment_id, signal_id, observed_at, state, payload_json
             FROM signal_snapshots
             ORDER BY environment_id, signal_id",
        )
        .map_err(to_io_error)?;
    let mut rows = statement.query([]).map_err(to_io_error)?;
    let mut snapshots = Vec::new();
    while let Some(row) = rows.next().map_err(to_io_error)? {
        snapshots.push(SignalSnapshot {
            environment_id: row.get(0).map_err(to_io_error)?,
            signal_id: row.get(1).map_err(to_io_error)?,
            observed_at: row.get(2).map_err(to_io_error)?,
            state: row.get(3).map_err(to_io_error)?,
            payload_json: row.get(4).map_err(to_io_error)?,
        });
    }
    Ok(snapshots)
}

pub fn load_signal_snapshot(
    connection: &Connection,
    environment_id: &str,
    signal_id: &str,
) -> io::Result<Option<SignalSnapshot>> {
    let mut statement = connection
        .prepare(
            "SELECT environment_id, signal_id, observed_at, state, payload_json
             FROM signal_snapshots
             WHERE environment_id = ?1 AND signal_id = ?2",
        )
        .map_err(to_io_error)?;
    let mut rows = statement
        .query(params![environment_id, signal_id])
        .map_err(to_io_error)?;
    let Some(row) = rows.next().map_err(to_io_error)? else {
        return Ok(None);
    };
    Ok(Some(SignalSnapshot {
        environment_id: row.get(0).map_err(to_io_error)?,
        signal_id: row.get(1).map_err(to_io_error)?,
        observed_at: row.get(2).map_err(to_io_error)?,
        state: row.get(3).map_err(to_io_error)?,
        payload_json: row.get(4).map_err(to_io_error)?,
    }))
}

#[derive(Debug, Clone, PartialEq)]
pub struct SignalSample {
    pub environment_id: String,
    pub signal_id: String,
    pub observed_at: i64,
    pub value_real: Option<f64>,
    pub value_json: Option<String>,
}

pub fn persist_signal_sample(
    connection: &Connection,
    environment_id: &str,
    signal_id: &str,
    observed_at: i64,
    value_real: Option<f64>,
    value_json: Option<&str>,
) -> io::Result<()> {
    connection
        .execute(
            "INSERT INTO signal_samples (
                environment_id, signal_id, observed_at, value_real, value_json
             ) VALUES (?1, ?2, ?3, ?4, ?5)",
            params![
                environment_id,
                signal_id,
                observed_at,
                value_real,
                value_json
            ],
        )
        .map_err(to_io_error)?;
    Ok(())
}

pub fn load_signal_samples(
    connection: &Connection,
    environment_id: &str,
    signal_id: &str,
) -> io::Result<Vec<SignalSample>> {
    let mut statement = connection
        .prepare(
            "SELECT environment_id, signal_id, observed_at, value_real, value_json
             FROM signal_samples
             WHERE environment_id = ?1 AND signal_id = ?2
             ORDER BY observed_at ASC",
        )
        .map_err(to_io_error)?;
    let mut rows = statement
        .query(params![environment_id, signal_id])
        .map_err(to_io_error)?;
    let mut samples = Vec::new();
    while let Some(row) = rows.next().map_err(to_io_error)? {
        samples.push(SignalSample {
            environment_id: row.get(0).map_err(to_io_error)?,
            signal_id: row.get(1).map_err(to_io_error)?,
            observed_at: row.get(2).map_err(to_io_error)?,
            value_real: row.get(3).map_err(to_io_error)?,
            value_json: row.get(4).map_err(to_io_error)?,
        });
    }
    Ok(samples)
}

pub const SAMPLE_RETENTION_SECS: i64 = 24 * 60 * 60;

/// Hourly roll-up retention: 90 days of hour buckets per Environment × Signal.
pub const ROLLUP_RETENTION_SECS: i64 = 90 * 24 * 60 * 60;
pub const ROLLUP_BUCKET_SECS: i64 = 60 * 60;

#[derive(Debug, Clone, PartialEq)]
pub struct SignalRollup {
    pub environment_id: String,
    pub signal_id: String,
    pub hour_start: i64,
    pub avg_real: Option<f64>,
    pub max_real: Option<f64>,
    pub sample_count: i64,
}

/// Recomputes one hour bucket from the raw samples. Idempotent: re-publishing
/// the same tick replaces the row, so restarts never duplicate history.
/// Hours without samples leave no row.
pub fn record_hour_rollup(
    connection: &Connection,
    environment_id: &str,
    signal_id: &str,
    hour_start: i64,
) -> io::Result<()> {
    connection
        .execute(
            "INSERT INTO signal_rollups_hourly (
                environment_id, signal_id, hour_start, avg_real, max_real, sample_count
             ) SELECT ?1, ?2, ?3, AVG(value_real), MAX(value_real), COUNT(*)
               FROM signal_samples
               WHERE environment_id = ?1 AND signal_id = ?2
                 AND value_real IS NOT NULL
                 AND observed_at >= ?3 AND observed_at < ?3 + 3600
               HAVING COUNT(*) > 0
             ON CONFLICT(environment_id, signal_id, hour_start) DO UPDATE SET
                avg_real = excluded.avg_real,
                max_real = excluded.max_real,
                sample_count = excluded.sample_count",
            params![environment_id, signal_id, hour_start],
        )
        .map_err(to_io_error)?;
    Ok(())
}

pub fn load_signal_rollups(
    connection: &Connection,
    environment_id: &str,
    signal_id: &str,
    since: i64,
) -> io::Result<Vec<SignalRollup>> {
    let mut statement = connection
        .prepare(
            "SELECT environment_id, signal_id, hour_start, avg_real, max_real, sample_count
             FROM signal_rollups_hourly
             WHERE environment_id = ?1 AND signal_id = ?2 AND hour_start >= ?3
             ORDER BY hour_start ASC",
        )
        .map_err(to_io_error)?;
    let mut rows = statement
        .query(params![environment_id, signal_id, since])
        .map_err(to_io_error)?;
    let mut rollups = Vec::new();
    while let Some(row) = rows.next().map_err(to_io_error)? {
        rollups.push(SignalRollup {
            environment_id: row.get(0).map_err(to_io_error)?,
            signal_id: row.get(1).map_err(to_io_error)?,
            hour_start: row.get(2).map_err(to_io_error)?,
            avg_real: row.get(3).map_err(to_io_error)?,
            max_real: row.get(4).map_err(to_io_error)?,
            sample_count: row.get(5).map_err(to_io_error)?,
        });
    }
    Ok(rollups)
}

pub fn prune_signal_rollups(connection: &Connection, now: i64) -> io::Result<usize> {
    let cutoff = now.saturating_sub(ROLLUP_RETENTION_SECS);
    connection
        .execute(
            "DELETE FROM signal_rollups_hourly WHERE hour_start < ?1",
            params![cutoff],
        )
        .map_err(to_io_error)
}

/// Health-event log bounds: 90 days and 500 events per Environment, enforced
/// on every publish. Attention history, not an audit trail.
pub const HEALTH_EVENTS_RETENTION_SECS: i64 = 90 * 24 * 60 * 60;
pub const HEALTH_EVENTS_MAX_PER_ENV: i64 = 500;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HealthEvent {
    pub environment_id: String,
    pub observed_at: i64,
    /// `"health"` or `"build"`.
    pub kind: String,
    pub from_health: Option<String>,
    pub to_health: String,
    pub build: Option<String>,
    /// Operator annotation; `None` when unannotated.
    pub note: Option<String>,
}

/// Idempotent: re-publishing the same tick (same env/observed_at/kind) is a
/// no-op, so a restarted daemon cannot duplicate events. Annotations ride
/// alongside, never as identity: re-recording never overwrites a note.
pub fn record_health_event(connection: &Connection, event: &HealthEvent) -> io::Result<()> {
    connection
        .execute(
            "INSERT INTO health_events (
                environment_id, observed_at, kind, from_health, to_health, build
             ) VALUES (?1, ?2, ?3, ?4, ?5, ?6)
             ON CONFLICT(environment_id, observed_at, kind) DO NOTHING",
            params![
                event.environment_id,
                event.observed_at,
                event.kind,
                event.from_health,
                event.to_health,
                event.build
            ],
        )
        .map_err(to_io_error)?;
    Ok(())
}

/// Attaches (or clears, with an empty note) the Operator annotation on one
/// health event. Returns rows updated: 0 names an unknown event.
pub fn annotate_health_event(
    connection: &Connection,
    environment_id: &str,
    observed_at: i64,
    kind: &str,
    note: &str,
) -> io::Result<usize> {
    let note = note.trim();
    let note: Option<&str> = if note.is_empty() { None } else { Some(note) };
    connection
        .execute(
            "UPDATE health_events SET note = ?1
              WHERE environment_id = ?2 AND observed_at = ?3 AND kind = ?4",
            params![note, environment_id, observed_at, kind],
        )
        .map_err(to_io_error)
}

pub fn load_health_events(
    connection: &Connection,
    environment_id: &str,
    limit: i64,
) -> io::Result<Vec<HealthEvent>> {
    let mut statement = connection
        .prepare(
            "SELECT environment_id, observed_at, kind, from_health, to_health, build, note
              FROM (
                SELECT environment_id, observed_at, kind, from_health, to_health, build, note
                FROM health_events
                WHERE environment_id = ?1
                ORDER BY observed_at DESC
                LIMIT ?2
              )
              ORDER BY observed_at ASC",
        )
        .map_err(to_io_error)?;
    let mut rows = statement
        .query(params![environment_id, limit])
        .map_err(to_io_error)?;
    let mut events = Vec::new();
    while let Some(row) = rows.next().map_err(to_io_error)? {
        events.push(HealthEvent {
            environment_id: row.get(0).map_err(to_io_error)?,
            observed_at: row.get(1).map_err(to_io_error)?,
            kind: row.get(2).map_err(to_io_error)?,
            from_health: row.get(3).map_err(to_io_error)?,
            to_health: row.get(4).map_err(to_io_error)?,
            build: row.get(5).map_err(to_io_error)?,
            note: row.get(6).map_err(to_io_error)?,
        });
    }
    Ok(events)
}

/// Enforces both bounds: drops events older than 90 days, then keeps only the
/// newest 500 per Environment. Returns rows deleted.
pub fn prune_health_events(
    connection: &Connection,
    environment_id: &str,
    now: i64,
) -> io::Result<usize> {
    let cutoff = now.saturating_sub(HEALTH_EVENTS_RETENTION_SECS);
    let mut deleted = connection
        .execute(
            "DELETE FROM health_events WHERE environment_id = ?1 AND observed_at < ?2",
            params![environment_id, cutoff],
        )
        .map_err(to_io_error)?;
    deleted += connection
        .execute(
            "DELETE FROM health_events WHERE environment_id = ?1 AND rowid NOT IN (
                 SELECT rowid FROM health_events
                 WHERE environment_id = ?1
                 ORDER BY observed_at DESC
                 LIMIT ?2
             )",
            params![environment_id, HEALTH_EVENTS_MAX_PER_ENV],
        )
        .map_err(to_io_error)?;
    Ok(deleted)
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PublishState {
    pub last_health: String,
    pub consecutive: i64,
    pub previous_health: Option<String>,
    pub last_build: Option<String>,
}

/// One per-Signal state transition, confirmed over two consecutive
/// publishes. Bounds mirror `health_events`: 90 days, 500 per Environment.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SignalEvent {
    pub environment_id: String,
    pub signal_id: String,
    pub observed_at: i64,
    pub from_state: Option<String>,
    pub to_state: String,
}

/// Idempotent like `record_health_event`: same tick re-published is a no-op.
pub fn record_signal_event(connection: &Connection, event: &SignalEvent) -> io::Result<()> {
    connection
        .execute(
            "INSERT INTO signal_events (
                environment_id, signal_id, observed_at, from_state, to_state
              ) VALUES (?1, ?2, ?3, ?4, ?5)
              ON CONFLICT(environment_id, signal_id, observed_at) DO NOTHING",
            params![
                event.environment_id,
                event.signal_id,
                event.observed_at,
                event.from_state,
                event.to_state
            ],
        )
        .map_err(to_io_error)?;
    Ok(())
}

pub fn load_signal_events(
    connection: &Connection,
    environment_id: &str,
    limit: i64,
) -> io::Result<Vec<SignalEvent>> {
    let mut statement = connection
        .prepare(
            "SELECT environment_id, signal_id, observed_at, from_state, to_state
              FROM (
                SELECT environment_id, signal_id, observed_at, from_state, to_state
                FROM signal_events
                WHERE environment_id = ?1
                ORDER BY observed_at DESC
                LIMIT ?2
              )
              ORDER BY observed_at ASC",
        )
        .map_err(to_io_error)?;
    let mut rows = statement
        .query(params![environment_id, limit])
        .map_err(to_io_error)?;
    let mut events = Vec::new();
    while let Some(row) = rows.next().map_err(to_io_error)? {
        events.push(SignalEvent {
            environment_id: row.get(0).map_err(to_io_error)?,
            signal_id: row.get(1).map_err(to_io_error)?,
            observed_at: row.get(2).map_err(to_io_error)?,
            from_state: row.get(3).map_err(to_io_error)?,
            to_state: row.get(4).map_err(to_io_error)?,
        });
    }
    Ok(events)
}

/// Enforces both bounds per Environment. Returns rows deleted.
pub fn prune_signal_events(
    connection: &Connection,
    environment_id: &str,
    now: i64,
) -> io::Result<usize> {
    let cutoff = now.saturating_sub(HEALTH_EVENTS_RETENTION_SECS);
    let mut deleted = connection
        .execute(
            "DELETE FROM signal_events WHERE environment_id = ?1 AND observed_at < ?2",
            params![environment_id, cutoff],
        )
        .map_err(to_io_error)?;
    deleted += connection
        .execute(
            "DELETE FROM signal_events WHERE environment_id = ?1 AND rowid NOT IN (
                  SELECT rowid FROM signal_events
                  WHERE environment_id = ?1
                  ORDER BY observed_at DESC
                  LIMIT ?2
              )",
            params![environment_id, HEALTH_EVENTS_MAX_PER_ENV],
        )
        .map_err(to_io_error)?;
    Ok(deleted)
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SignalPublishState {
    pub last_state: String,
    pub consecutive: i64,
    pub previous_state: Option<String>,
}

pub fn load_signal_publish_state(
    connection: &Connection,
    environment_id: &str,
    signal_id: &str,
) -> io::Result<Option<SignalPublishState>> {
    let mut statement = connection
        .prepare(
            "SELECT last_state, consecutive, previous_state
              FROM signal_publish_state
              WHERE environment_id = ?1 AND signal_id = ?2",
        )
        .map_err(to_io_error)?;
    let mut rows = statement
        .query(params![environment_id, signal_id])
        .map_err(to_io_error)?;
    let Some(row) = rows.next().map_err(to_io_error)? else {
        return Ok(None);
    };
    Ok(Some(SignalPublishState {
        last_state: row.get(0).map_err(to_io_error)?,
        consecutive: row.get(1).map_err(to_io_error)?,
        previous_state: row.get(2).map_err(to_io_error)?,
    }))
}

pub fn store_signal_publish_state(
    connection: &Connection,
    environment_id: &str,
    signal_id: &str,
    state: &SignalPublishState,
) -> io::Result<()> {
    connection
        .execute(
            "INSERT INTO signal_publish_state (
                environment_id, signal_id, last_state, consecutive, previous_state
              ) VALUES (?1, ?2, ?3, ?4, ?5)
              ON CONFLICT(environment_id, signal_id) DO UPDATE SET
                 last_state = excluded.last_state,
                 consecutive = excluded.consecutive,
                 previous_state = excluded.previous_state",
            params![
                environment_id,
                signal_id,
                state.last_state,
                state.consecutive,
                state.previous_state
            ],
        )
        .map_err(to_io_error)?;
    Ok(())
}

pub fn load_publish_state(
    connection: &Connection,
    environment_id: &str,
) -> io::Result<Option<PublishState>> {
    let mut statement = connection
        .prepare(
            "SELECT last_health, consecutive, previous_health, last_build
             FROM dashboard_publish_state
             WHERE environment_id = ?1",
        )
        .map_err(to_io_error)?;
    let mut rows = statement
        .query(params![environment_id])
        .map_err(to_io_error)?;
    let Some(row) = rows.next().map_err(to_io_error)? else {
        return Ok(None);
    };
    Ok(Some(PublishState {
        last_health: row.get(0).map_err(to_io_error)?,
        consecutive: row.get(1).map_err(to_io_error)?,
        previous_health: row.get(2).map_err(to_io_error)?,
        last_build: row.get(3).map_err(to_io_error)?,
    }))
}

pub fn store_publish_state(
    connection: &Connection,
    environment_id: &str,
    state: &PublishState,
) -> io::Result<()> {
    connection
        .execute(
            "INSERT INTO dashboard_publish_state (
                environment_id, last_health, consecutive, previous_health, last_build
             ) VALUES (?1, ?2, ?3, ?4, ?5)
             ON CONFLICT(environment_id) DO UPDATE SET
                last_health = excluded.last_health,
                consecutive = excluded.consecutive,
                previous_health = excluded.previous_health,
                last_build = excluded.last_build",
            params![
                environment_id,
                state.last_health,
                state.consecutive,
                state.previous_health,
                state.last_build
            ],
        )
        .map_err(to_io_error)?;
    Ok(())
}

/// Additive webhook delivery tables, created idempotently outside the
/// numbered migration chain so old binaries keep working: they simply never
/// create or read these tables. New binaries treat a missing row as
/// "seed at current time" to avoid an initial burst.
pub fn ensure_webhook_tables(connection: &Connection) -> io::Result<()> {
    connection
        .execute_batch(
            "CREATE TABLE IF NOT EXISTS webhook_cursor (
                environment_id TEXT PRIMARY KEY,
                last_sent INTEGER NOT NULL
             );
             CREATE TABLE IF NOT EXISTS webhook_dead_letters (
                environment_id TEXT NOT NULL,
                observed_at INTEGER NOT NULL,
                kind TEXT NOT NULL,
                error TEXT NOT NULL,
                PRIMARY KEY (environment_id, observed_at, kind)
             );",
        )
        .map_err(to_io_error)?;
    Ok(())
}

pub fn load_webhook_cursor(
    connection: &Connection,
    environment_id: &str,
) -> io::Result<Option<i64>> {
    ensure_webhook_tables(connection)?;
    let mut statement = connection
        .prepare("SELECT last_sent FROM webhook_cursor WHERE environment_id = ?1")
        .map_err(to_io_error)?;
    let mut rows = statement
        .query(params![environment_id])
        .map_err(to_io_error)?;
    let Some(row) = rows.next().map_err(to_io_error)? else {
        return Ok(None);
    };
    Ok(Some(row.get(0).map_err(to_io_error)?))
}

pub fn store_webhook_cursor(
    connection: &Connection,
    environment_id: &str,
    last_sent: i64,
) -> io::Result<()> {
    ensure_webhook_tables(connection)?;
    connection
        .execute(
            "INSERT INTO webhook_cursor (environment_id, last_sent)
             VALUES (?1, ?2)
             ON CONFLICT(environment_id) DO UPDATE SET last_sent = excluded.last_sent",
            params![environment_id, last_sent],
        )
        .map_err(to_io_error)?;
    Ok(())
}

pub fn record_webhook_dead_letter(
    connection: &Connection,
    environment_id: &str,
    observed_at: i64,
    kind: &str,
    error: &str,
) -> io::Result<()> {
    ensure_webhook_tables(connection)?;
    connection
        .execute(
            "INSERT OR IGNORE INTO webhook_dead_letters
                (environment_id, observed_at, kind, error)
             VALUES (?1, ?2, ?3, ?4)",
            params![environment_id, observed_at, kind, error],
        )
        .map_err(to_io_error)?;
    // Bounded retention: keep the newest 500 per environment so a flapping
    // endpoint cannot grow the table without bound.
    connection
        .execute(
            "DELETE FROM webhook_dead_letters WHERE environment_id = ?1 AND rowid NOT IN (
                 SELECT rowid FROM webhook_dead_letters
                 WHERE environment_id = ?1
                 ORDER BY observed_at DESC, kind DESC LIMIT 500
             )",
            params![environment_id],
        )
        .map_err(to_io_error)?;
    Ok(())
}

pub fn load_webhook_dead_letter_count(
    connection: &Connection,
    environment_id: &str,
) -> io::Result<i64> {
    ensure_webhook_tables(connection)?;
    let mut statement = connection
        .prepare("SELECT COUNT(*) FROM webhook_dead_letters WHERE environment_id = ?1")
        .map_err(to_io_error)?;
    let mut rows = statement
        .query(params![environment_id])
        .map_err(to_io_error)?;
    let Some(row) = rows.next().map_err(to_io_error)? else {
        return Ok(0);
    };
    row.get(0).map_err(to_io_error)
}

/// Additive per-tick timing telemetry (FEAT-004): one row per tick with its
/// wall-clock duration, created idempotently outside the numbered migration
/// chain like the webhook tables. Bounded to the newest 10_000 ticks.
pub fn ensure_tick_stats_table(connection: &Connection) -> io::Result<()> {
    connection
        .execute_batch(
            "CREATE TABLE IF NOT EXISTS tick_stats (
                started_at INTEGER NOT NULL,
                duration_ms INTEGER NOT NULL,
                env_count INTEGER NOT NULL DEFAULT 0
             );
             CREATE INDEX IF NOT EXISTS tick_stats_by_time ON tick_stats (started_at);",
        )
        .map_err(to_io_error)?;
    // Additive migration for pre-existing tables created without env_count.
    let _ = connection.execute(
        "ALTER TABLE tick_stats ADD COLUMN env_count INTEGER NOT NULL DEFAULT 0",
        [],
    );
    Ok(())
}

pub fn record_tick_stat(
    connection: &Connection,
    started_at: i64,
    duration_ms: i64,
) -> io::Result<()> {
    record_tick_stat_with_envs(connection, started_at, duration_ms, 0)
}

pub fn record_tick_stat_with_envs(
    connection: &Connection,
    started_at: i64,
    duration_ms: i64,
    env_count: i64,
) -> io::Result<()> {
    ensure_tick_stats_table(connection)?;
    connection
        .execute(
            "INSERT INTO tick_stats (started_at, duration_ms, env_count) VALUES (?1, ?2, ?3)",
            params![started_at, duration_ms, env_count],
        )
        .map_err(to_io_error)?;
    connection
        .execute(
            "DELETE FROM tick_stats WHERE rowid NOT IN (
                 SELECT rowid FROM tick_stats ORDER BY started_at DESC LIMIT 10000
             )",
            [],
        )
        .map_err(to_io_error)?;
    Ok(())
}

/// `(ticks, overruns)` over the retained window: a tick overruns when its
/// duration meets or exceeds the poll interval.
pub fn load_tick_stats_summary(
    connection: &Connection,
    interval_secs: i64,
) -> io::Result<(i64, i64)> {
    ensure_tick_stats_table(connection)?;
    let mut statement = connection
        .prepare("SELECT COUNT(*), SUM(CASE WHEN duration_ms >= ?1 * 1000 THEN 1 ELSE 0 END) FROM tick_stats")
        .map_err(to_io_error)?;
    let mut rows = statement
        .query(params![interval_secs])
        .map_err(to_io_error)?;
    let Some(row) = rows.next().map_err(to_io_error)? else {
        return Ok((0, 0));
    };
    Ok((
        row.get(0).map_err(to_io_error)?,
        row.get::<_, Option<i64>>(1)
            .map_err(to_io_error)?
            .unwrap_or(0),
    ))
}

pub fn prune_signal_samples(connection: &Connection, now: i64) -> io::Result<usize> {
    let cutoff = now.saturating_sub(SAMPLE_RETENTION_SECS);
    connection
        .execute(
            "DELETE FROM signal_samples WHERE observed_at < ?1",
            params![cutoff],
        )
        .map_err(to_io_error)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::TempDb;

    fn table_exists(connection: &Connection, name: &str) -> bool {
        connection
            .query_row(
                "SELECT EXISTS(SELECT 1 FROM sqlite_master WHERE type = 'table' AND name = ?1)",
                params![name],
                |row| row.get(0),
            )
            .unwrap()
    }

    #[test]
    fn apply_migrations_creates_signal_tables() {
        let db = TempDb::new("apply");
        let connection = Connection::open(db.path()).unwrap();
        let applied = apply_migrations(&connection).unwrap();
        assert!(applied >= 1);
        assert!(table_exists(&connection, "signal_snapshots"));
        assert!(table_exists(&connection, "signal_samples"));
        assert!(!table_exists(&connection, "environments"));
        assert!(!table_exists(&connection, "projects"));
        assert_eq!(apply_migrations(&connection).unwrap(), 0);
    }

    #[test]
    fn tick_stats_record_and_summarize_overruns() {
        let db = TempDb::new("ticks");
        let store = db.store();
        let connection = store.open().unwrap();
        assert_eq!(load_tick_stats_summary(&connection, 60).unwrap(), (0, 0));
        record_tick_stat(&connection, 1000, 5_000).unwrap();
        record_tick_stat(&connection, 2000, 90_000).unwrap();
        assert_eq!(load_tick_stats_summary(&connection, 60).unwrap(), (2, 1));
        store.prune_all_for_startup();
    }

    #[test]
    fn db_stats_counts_rows_and_formats_one_line() {
        use crate::availability::AvailabilityObservation;
        use daku_protocol::{Reachability, SignalState};

        let db = TempDb::new("stats");
        // A never-opened database reads as all zeros and creates nothing.
        assert_eq!(db_stats(db.path()), DbStats::default());
        assert!(!db.path().exists());
        let store = db.store();
        let connection = store.open().unwrap();
        crate::availability::persist_availability_snapshot(
            &connection,
            "prod",
            &AvailabilityObservation {
                reachability: Reachability::Reachable,
                state: SignalState::Healthy,
                build: None,
                rtt_ms: 10,
                error: None,
            },
            1_700_000_000,
        )
        .unwrap();
        persist_signal_sample(&connection, "prod", "jobs", 1_700_000_000, Some(2.0), None).unwrap();
        record_hour_rollup(&connection, "prod", "jobs", 1_700_000_000).unwrap();
        record_health_event(
            &connection,
            &HealthEvent {
                environment_id: "prod".into(),
                observed_at: 1_700_000_000,
                kind: "health".into(),
                from_health: Some("healthy".into()),
                to_health: "degraded".into(),
                build: None,
                note: None,
            },
        )
        .unwrap();
        let stats = db_stats(db.path());
        assert_eq!(stats.snapshots, 1);
        assert_eq!(stats.samples, 1);
        assert_eq!(stats.rollups, 1);
        assert_eq!(stats.health_events, 1);
        assert!(stats.bytes > 0);
        let line = format_db_stats(db.path(), &stats);
        assert!(line.contains("snapshots 1"), "{line}");
        assert!(line.contains("health events 1"), "{line}");
    }

    #[test]
    fn db_stats_warns_past_200_mb_and_scales_units() {
        let db = TempDb::new("stats-units");
        let big = DbStats {
            bytes: 210 * 1024 * 1024,
            ..DbStats::default()
        };
        let line = format_db_stats(db.path(), &big);
        assert!(line.contains("210 MB"), "{line}");
        assert!(line.contains("WARNING"), "{line}");
        let small = DbStats {
            bytes: 41 * 1024,
            ..DbStats::default()
        };
        let line = format_db_stats(db.path(), &small);
        assert!(line.contains("41 KB"), "{line}");
        assert!(!line.contains("WARNING"), "{line}");
    }

    #[test]
    fn apply_migrations_matches_by_numeric_prefix() {
        let db = TempDb::new("prefix");
        let connection = Connection::open(db.path()).unwrap();
        assert!(apply_migrations(&connection).unwrap() >= 1);
        // Simulate a regenerated migration name for the same index.
        connection
            .execute(
                "UPDATE migrations SET tag = '0000_renamed_by_regeneration' WHERE tag LIKE '0000%'",
                [],
            )
            .unwrap();
        assert_eq!(apply_migrations(&connection).unwrap(), 0);
        assert!(table_exists(&connection, "signal_snapshots"));
    }

    #[test]
    fn apply_migrations_is_safe_under_concurrent_openers() {
        // Two collector threads opening a fresh DB at once (plan 022) must not
        // both try to create the tables.
        let db = TempDb::new("concurrent");
        let store = db.store();
        std::thread::scope(|scope| {
            let handles: Vec<_> = (0..4)
                .map(|_| scope.spawn(|| store.open().map(|_| ())))
                .collect();
            for handle in handles {
                handle.join().unwrap().unwrap();
            }
        });
        let connection = Connection::open(db.path()).unwrap();
        let rows: i64 = connection
            .query_row("SELECT COUNT(*) FROM migrations", [], |row| row.get(0))
            .unwrap();
        assert_eq!(rows as usize, MIGRATIONS.len());
    }

    #[test]
    fn daku_dir_permissions_are_0700_and_0600() {
        let root = std::env::temp_dir().join(format!("daku-home-{}", uuid::Uuid::new_v4()));
        let db_path = root.join(".daku").join("app.db");
        ensure_daku_dir(&db_path).unwrap();

        let custom_db = root.join("custom").join("app.db");
        ensure_daku_dir(&custom_db).unwrap();

        let pre = root.join("pre");
        fs::create_dir_all(&pre).unwrap();
        #[cfg(unix)]
        fs::set_permissions(&pre, fs::Permissions::from_mode(0o755)).unwrap();
        let pre_db = pre.join("app.db");
        ensure_daku_dir(&pre_db).unwrap();

        #[cfg(unix)]
        {
            let mode_of = |path: &Path| fs::metadata(path).unwrap().permissions().mode() & 0o777;
            assert_eq!(mode_of(db_path.parent().unwrap()), 0o700);
            assert_eq!(mode_of(&db_path), 0o600);
            assert_eq!(mode_of(custom_db.parent().unwrap()), 0o700);
            assert_eq!(mode_of(&custom_db), 0o600);
            assert_eq!(mode_of(&pre), 0o755);
            assert_eq!(mode_of(&pre_db), 0o600);
        }

        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn prune_signal_samples_drops_older_than_24h() {
        let db = TempDb::new("prune");
        let connection = db.store().open().unwrap();
        let now = 1_700_000_000;
        persist_signal_sample(
            &connection,
            "prod",
            "jobs",
            now - 25 * 60 * 60,
            Some(1.0),
            None,
        )
        .unwrap();
        persist_signal_sample(&connection, "prod", "jobs", now, Some(2.0), None).unwrap();
        persist_signal_sample(
            &connection,
            "prod",
            "syslog",
            now - 25 * 60 * 60,
            Some(3.0),
            None,
        )
        .unwrap();

        assert_eq!(prune_signal_samples(&connection, now).unwrap(), 2);

        let jobs = load_signal_samples(&connection, "prod", "jobs").unwrap();
        assert_eq!(jobs.len(), 1);
        assert_eq!(jobs[0].observed_at, now);
        assert_eq!(jobs[0].value_real, Some(2.0));
        assert!(
            load_signal_samples(&connection, "prod", "syslog")
                .unwrap()
                .is_empty()
        );
    }

    fn health_event(
        environment_id: &str,
        observed_at: i64,
        kind: &str,
        from_health: Option<&str>,
        to_health: &str,
    ) -> HealthEvent {
        HealthEvent {
            environment_id: environment_id.into(),
            observed_at,
            kind: kind.into(),
            from_health: from_health.map(str::to_owned),
            to_health: to_health.into(),
            build: None,
            note: None,
        }
    }

    #[test]
    fn health_events_round_trip_and_republish_is_idempotent() {
        let db = TempDb::new("health-events");
        let connection = db.store().open().unwrap();
        assert!(table_exists(&connection, "health_events"));
        assert!(table_exists(&connection, "dashboard_publish_state"));

        let event = health_event("prod", 1_700_000_000, "health", Some("healthy"), "degraded");
        record_health_event(&connection, &event).unwrap();
        // Same tick re-published (e.g. daemon restart) must not duplicate.
        record_health_event(&connection, &event).unwrap();

        let events = load_health_events(&connection, "prod", 100).unwrap();
        assert_eq!(events, vec![event]);
        assert!(
            load_health_events(&connection, "test", 100)
                .unwrap()
                .is_empty()
        );
    }

    #[test]
    fn prune_health_events_enforces_age_and_count_bounds() {
        let db = TempDb::new("health-events-prune");
        let connection = db.store().open().unwrap();
        let now = 1_700_000_000;
        record_health_event(
            &connection,
            &health_event(
                "prod",
                now - HEALTH_EVENTS_RETENTION_SECS - 1,
                "health",
                None,
                "healthy",
            ),
        )
        .unwrap();
        record_health_event(
            &connection,
            &health_event("prod", now, "health", None, "healthy"),
        )
        .unwrap();
        assert_eq!(prune_health_events(&connection, "prod", now).unwrap(), 1);
        assert_eq!(
            load_health_events(&connection, "prod", 100).unwrap().len(),
            1
        );
    }

    #[test]
    fn hour_rollup_recompute_is_idempotent_and_prunes() {
        let db = TempDb::new("rollups");
        let connection = db.store().open().unwrap();
        assert!(table_exists(&connection, "signal_rollups_hourly"));
        let hour = 1_700_000_000 - 1_700_000_000 % 3600;
        for (at, value) in [(hour + 10, 1.0), (hour + 20, 3.0)] {
            persist_signal_sample(&connection, "prod", "jobs", at, Some(value), None).unwrap();
        }
        record_hour_rollup(&connection, "prod", "jobs", hour).unwrap();
        record_hour_rollup(&connection, "prod", "jobs", hour).unwrap();
        let rollups = load_signal_rollups(&connection, "prod", "jobs", 0).unwrap();
        assert_eq!(rollups.len(), 1);
        assert_eq!(rollups[0].avg_real, Some(2.0));
        assert_eq!(rollups[0].max_real, Some(3.0));
        assert_eq!(rollups[0].sample_count, 2);
        // Empty hours leave no row.
        record_hour_rollup(&connection, "prod", "jobs", hour + 3600).unwrap();
        assert_eq!(
            load_signal_rollups(&connection, "prod", "jobs", 0)
                .unwrap()
                .len(),
            1
        );
        // Retention drops old buckets.
        assert_eq!(
            prune_signal_rollups(&connection, hour + ROLLUP_RETENTION_SECS + 1).unwrap(),
            1
        );
        assert!(
            load_signal_rollups(&connection, "prod", "jobs", 0)
                .unwrap()
                .is_empty()
        );
    }

    #[test]
    fn publish_state_round_trips_per_environment() {
        let db = TempDb::new("publish-state");
        let connection = db.store().open().unwrap();
        assert!(load_publish_state(&connection, "prod").unwrap().is_none());
        let state = PublishState {
            last_health: "degraded".into(),
            consecutive: 2,
            previous_health: Some("healthy".into()),
            last_build: Some("glide-1".into()),
        };
        store_publish_state(&connection, "prod", &state).unwrap();
        assert_eq!(
            load_publish_state(&connection, "prod").unwrap(),
            Some(state)
        );
        assert!(load_publish_state(&connection, "test").unwrap().is_none());
    }

    #[test]
    fn annotations_attach_clear_and_report_unknown_events() {
        let db = TempDb::new("notes");
        let connection = db.store().open().unwrap();
        let mut event = health_event("prod", 1_700_000_000, "health", Some("healthy"), "degraded");
        event.note = None;
        record_health_event(&connection, &event).unwrap();
        assert_eq!(
            annotate_health_event(
                &connection,
                "prod",
                1_700_000_000,
                "health",
                "  bad deploy  "
            )
            .unwrap(),
            1
        );
        let events = load_health_events(&connection, "prod", 100).unwrap();
        assert_eq!(
            events[0].note.as_deref(),
            Some("bad deploy"),
            "notes trim on write"
        );
        // Re-recording the tick must not wipe the note.
        record_health_event(&connection, &event).unwrap();
        assert_eq!(
            load_health_events(&connection, "prod", 100).unwrap()[0]
                .note
                .as_deref(),
            Some("bad deploy")
        );
        assert_eq!(
            annotate_health_event(&connection, "prod", 1_700_000_000, "health", "   ").unwrap(),
            1,
            "empty clears"
        );
        assert_eq!(
            load_health_events(&connection, "prod", 100).unwrap()[0].note,
            None
        );
        assert_eq!(
            annotate_health_event(&connection, "prod", 9, "health", "x").unwrap(),
            0,
            "unknown events report zero rows"
        );
    }

    #[test]
    fn signal_events_round_trip_republish_and_prune() {
        let db = TempDb::new("signal-events");
        let connection = db.store().open().unwrap();
        let event = SignalEvent {
            environment_id: "prod".into(),
            signal_id: "jobs".into(),
            observed_at: 1_700_000_000,
            from_state: Some("healthy".into()),
            to_state: "degraded".into(),
        };
        record_signal_event(&connection, &event).unwrap();
        record_signal_event(&connection, &event).unwrap();
        let events = load_signal_events(&connection, "prod", 100).unwrap();
        assert_eq!(events, vec![event]);
        assert!(
            load_signal_events(&connection, "test", 100)
                .unwrap()
                .is_empty()
        );

        let state = SignalPublishState {
            last_state: "degraded".into(),
            consecutive: 2,
            previous_state: Some("healthy".into()),
        };
        store_signal_publish_state(&connection, "prod", "jobs", &state).unwrap();
        assert_eq!(
            load_signal_publish_state(&connection, "prod", "jobs").unwrap(),
            Some(state)
        );
        assert!(
            load_signal_publish_state(&connection, "prod", "syslog")
                .unwrap()
                .is_none()
        );

        // Age bound drops the old row.
        assert_eq!(
            prune_signal_events(
                &connection,
                "prod",
                1_700_000_000 + HEALTH_EVENTS_RETENTION_SECS + 1
            )
            .unwrap(),
            1
        );
        assert!(
            load_signal_events(&connection, "prod", 100)
                .unwrap()
                .is_empty()
        );
    }

    #[test]
    fn load_health_events_returns_newest_window_oldest_first() {
        let db = TempDb::new("health-events-order");
        let connection = db.store().open().unwrap();
        for at in [100, 200, 300, 400] {
            record_health_event(
                &connection,
                &health_event("prod", at, "health", None, "healthy"),
            )
            .unwrap();
        }
        let events = load_health_events(&connection, "prod", 2).unwrap();
        assert_eq!(
            events
                .iter()
                .map(|event| event.observed_at)
                .collect::<Vec<_>>(),
            vec![300, 400],
            "publish window is the newest rows, still oldest-first"
        );
    }

    #[test]
    fn load_signal_events_returns_newest_window_oldest_first() {
        let db = TempDb::new("signal-events-order");
        let connection = db.store().open().unwrap();
        for at in [100, 200, 300, 400] {
            record_signal_event(
                &connection,
                &SignalEvent {
                    environment_id: "prod".into(),
                    signal_id: format!("jobs-{at}"),
                    observed_at: at,
                    from_state: None,
                    to_state: "degraded".into(),
                },
            )
            .unwrap();
        }
        let events = load_signal_events(&connection, "prod", 2).unwrap();
        assert_eq!(
            events
                .iter()
                .map(|event| event.observed_at)
                .collect::<Vec<_>>(),
            vec![300, 400],
            "publish window is the newest rows, still oldest-first"
        );
    }

    #[test]
    fn prune_health_events_enforces_newest_500_cap() {
        let db = TempDb::new("health-events-cap");
        let connection = db.store().open().unwrap();
        let now = 1_700_000_000;
        for offset in 0..HEALTH_EVENTS_MAX_PER_ENV + 2 {
            record_health_event(
                &connection,
                &health_event("prod", now - 1000 + offset, "health", None, "healthy"),
            )
            .unwrap();
        }
        assert_eq!(prune_health_events(&connection, "prod", now).unwrap(), 2);
        let events =
            load_health_events(&connection, "prod", HEALTH_EVENTS_MAX_PER_ENV + 10).unwrap();
        assert_eq!(events.len(), HEALTH_EVENTS_MAX_PER_ENV as usize);
        assert_eq!(events[0].observed_at, now - 1000 + 2);
    }

    #[test]
    fn prune_signal_events_enforces_newest_500_cap() {
        let db = TempDb::new("signal-events-cap");
        let connection = db.store().open().unwrap();
        let now = 1_700_000_000;
        for offset in 0..HEALTH_EVENTS_MAX_PER_ENV + 2 {
            record_signal_event(
                &connection,
                &SignalEvent {
                    environment_id: "prod".into(),
                    signal_id: format!("jobs-{offset}"),
                    observed_at: now - 1000 + offset,
                    from_state: None,
                    to_state: "degraded".into(),
                },
            )
            .unwrap();
        }
        assert_eq!(prune_signal_events(&connection, "prod", now).unwrap(), 2);
        let events =
            load_signal_events(&connection, "prod", HEALTH_EVENTS_MAX_PER_ENV + 10).unwrap();
        assert_eq!(events.len(), HEALTH_EVENTS_MAX_PER_ENV as usize);
        assert_eq!(events[0].observed_at, now - 1000 + 2);
    }
}
