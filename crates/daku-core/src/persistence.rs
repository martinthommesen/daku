//! Daemon-owned SQLite storage helpers.

use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

#[cfg(unix)]
use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};

use rusqlite::{Connection, params};

use daku_protocol::SignalState;
use daku_protocol::identity::DATA_DIRECTORY_NAME;

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
        let transaction = connection.unchecked_transaction().map_err(to_io_error)?;
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
    /// Default DB path: `DAKU_DB_PATH`, else `~/.daku/app.db`.
    pub fn default_path() -> PathBuf {
        if let Ok(path) = std::env::var(DAKU_DB_PATH_ENV) {
            if !path.is_empty() {
                return PathBuf::from(path);
            }
        }
        dirs::home_dir()
            .unwrap_or_else(std::env::temp_dir)
            .join(format!(".{DATA_DIRECTORY_NAME}"))
            .join("app.db")
    }

    pub fn daemon(path: PathBuf) -> Self {
        Self { path }
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    pub fn open(&self) -> io::Result<Connection> {
        ensure_daku_dir(&self.path)?;
        let connection = Connection::open(&self.path).map_err(to_io_error)?;
        connection
            .execute_batch("PRAGMA journal_mode = WAL; PRAGMA synchronous = NORMAL;")
            .map_err(to_io_error)?;
        // One connection per collector thread: wait for a writer instead of
        // failing straight away with SQLITE_BUSY.
        connection
            .busy_timeout(Duration::from_secs(5))
            .map_err(to_io_error)?;
        apply_migrations(&connection)?;
        // WAL may recreate sidecar modes; re-assert the main db file mode.
        #[cfg(unix)]
        fs::set_permissions(&self.path, fs::Permissions::from_mode(0o600))?;
        Ok(connection)
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
}
