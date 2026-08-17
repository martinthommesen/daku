//! Test-only helpers shared by daku-core unit tests.

use std::path::{Path, PathBuf};

use crate::config::{AuthMethod, EnvironmentConfig};
use crate::persistence::StateStore;

/// Unique SQLite path under the OS temp dir; removes the db and its WAL/SHM
/// sidecars on drop (also on panic).
pub struct TempDb {
    path: PathBuf,
}

impl TempDb {
    pub fn new(label: &str) -> Self {
        let path = std::env::temp_dir().join(format!("daku-{label}-{}.db", uuid::Uuid::new_v4()));
        let _ = std::fs::remove_file(&path);
        Self { path }
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    pub fn store(&self) -> StateStore {
        StateStore::daemon(self.path.clone())
    }
}

impl Drop for TempDb {
    fn drop(&mut self) {
        for suffix in ["", "-wal", "-shm"] {
            let mut sidecar = self.path.clone().into_os_string();
            sidecar.push(suffix);
            let _ = std::fs::remove_file(sidecar);
        }
    }
}

/// The Basic-auth `prod` Environment used across collector tests.
pub fn prod() -> EnvironmentConfig {
    EnvironmentConfig {
        id: "prod".into(),
        label: "Production".into(),
        instance_url: "https://acme-prod.example.service-now.com".into(),
        auth_method: AuthMethod::Basic,
        sort_order: 0,
        clone_source: false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn temp_db_removes_db_and_sidecars_on_drop() {
        let path;
        {
            let db = TempDb::new("self");
            path = db.path().to_path_buf();
            let _connection = db.store().open().unwrap(); // creates .db (+ -wal/-shm under WAL)
        }
        assert!(!path.exists());
        let mut wal = path.clone().into_os_string();
        wal.push("-wal");
        assert!(!Path::new(&wal).exists());
    }
}
