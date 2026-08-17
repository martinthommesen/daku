//! Daemon-owned SQLite storage helpers.

use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

#[cfg(unix)]
use std::os::unix::fs::PermissionsExt;

use rusqlite::{Connection, params};

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
        let already_applied: bool = connection
            .query_row(
                "SELECT EXISTS(SELECT 1 FROM migrations WHERE tag = ?1)",
                params![tag],
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

/// Ensures the db file exists as `0o600`. When the parent is `.daku`, also set it `0o700`.
pub fn ensure_daku_dir(db_path: &Path) -> io::Result<()> {
    if let Some(parent) = db_path.parent() {
        fs::create_dir_all(parent)?;
        #[cfg(unix)]
        if parent
            .file_name()
            .is_some_and(|name| name == std::ffi::OsStr::new(".daku"))
        {
            fs::set_permissions(parent, fs::Permissions::from_mode(0o700))?;
        }
    }
    if !db_path.exists() {
        fs::File::create(db_path)?;
    }
    #[cfg(unix)]
    fs::set_permissions(db_path, fs::Permissions::from_mode(0o600))?;
    Ok(())
}

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
        apply_migrations(&connection)?;
        // WAL may recreate sidecar modes; re-assert the main db file mode.
        #[cfg(unix)]
        fs::set_permissions(&self.path, fs::Permissions::from_mode(0o600))?;
        Ok(connection)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_db_path(label: &str) -> PathBuf {
        std::env::temp_dir().join(format!(
            "daku-{label}-{}.db",
            uuid::Uuid::new_v4()
        ))
    }

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
        let path = temp_db_path("apply");
        let _ = fs::remove_file(&path);
        let connection = Connection::open(&path).unwrap();
        let applied = apply_migrations(&connection).unwrap();
        assert!(applied >= 1);
        assert!(table_exists(&connection, "signal_snapshots"));
        assert!(table_exists(&connection, "signal_samples"));
        assert!(!table_exists(&connection, "environments"));
        assert!(!table_exists(&connection, "projects"));
        assert_eq!(apply_migrations(&connection).unwrap(), 0);
        let _ = fs::remove_file(path);
    }

    #[test]
    fn daku_dir_permissions_are_0700_and_0600() {
        let root = std::env::temp_dir().join(format!("daku-home-{}", uuid::Uuid::new_v4()));
        let db_path = root.join(".daku").join("app.db");
        ensure_daku_dir(&db_path).unwrap();

        #[cfg(unix)]
        {
            let dir_mode = fs::metadata(db_path.parent().unwrap())
                .unwrap()
                .permissions()
                .mode()
                & 0o777;
            let db_mode = fs::metadata(&db_path).unwrap().permissions().mode() & 0o777;
            assert_eq!(dir_mode, 0o700);
            assert_eq!(db_mode, 0o600);
        }

        let _ = fs::remove_dir_all(root);
    }
}
