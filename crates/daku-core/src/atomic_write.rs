//! Single hardened atomic-write helper for small JSON state files.
//!
//! Replaces three copied tmp-plus-rename writers (credentials, environments,
//! settings). Owns unique tmp names, 0600 modes, file plus directory fsync,
//! atomic rename, owner-only parent repair, and process-wide serialization.

use std::fs;
use std::io::Write as _;
use std::path::Path;

use anyhow::Context as _;

static WRITE_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

/// Best-effort directory fsync so a directory entry is durable.
fn sync_dir(parent: &Path) {
    if let Ok(dir) = fs::File::open(parent) {
        let _ = dir.sync_all();
    }
}

/// Atomically replaces `path` with `data`.
///
/// Creates missing parents, repairs the immediate parent to owner-only on
/// unix, writes a uniquely named tmp file with 0600, fsyncs file and parent
/// directory, then renames over the target and enforces 0600 on it.
pub fn atomic_write(path: &Path, data: &[u8]) -> anyhow::Result<()> {
    let _guard = crate::lock_or_poisoned(&WRITE_LOCK);
    if let Some(parent) = path.parent().filter(|p| !p.as_os_str().is_empty()) {
        fs::create_dir_all(parent).with_context(|| format!("creating {}", parent.display()))?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt as _;
            // Best-effort repair of lax custom parents. Shared system roots
            // (e.g. $TMPDIR) are not owned by us, so a chmod failure there
            // must not fail the write — the 0600 tmp and target below still
            // hold.
            let _ = fs::set_permissions(parent, fs::Permissions::from_mode(0o700));
        }
    }
    let temporary = path.with_extension(format!("json.tmp-{}", uuid::Uuid::new_v4().simple()));
    {
        let mut options = fs::OpenOptions::new();
        options.write(true).create_new(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt as _;
            options.mode(0o600);
        }
        let mut file = options
            .open(&temporary)
            .with_context(|| format!("writing {}", temporary.display()))?;
        file.write_all(data)
            .with_context(|| format!("writing {}", temporary.display()))?;
        file.sync_all()
            .with_context(|| format!("syncing {}", temporary.display()))?;
    }
    if let Some(parent) = path.parent().filter(|p| !p.as_os_str().is_empty()) {
        // Best-effort directory fsync so the tmp entry is durable before rename.
        sync_dir(parent);
    }
    fs::rename(&temporary, path).with_context(|| format!("replacing {}", path.display()))?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt as _;
        fs::set_permissions(path, fs::Permissions::from_mode(0o600))
            .with_context(|| format!("securing {}", path.display()))?;
    }
    if let Some(parent) = path.parent().filter(|p| !p.as_os_str().is_empty()) {
        sync_dir(parent);
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[cfg(unix)]
    #[test]
    fn atomic_write_repairs_parent_and_keeps_0600() {
        use std::os::unix::fs::PermissionsExt as _;
        let directory =
            std::env::temp_dir().join(format!("daku-atomic-{}", uuid::Uuid::new_v4().simple()));
        // Lax custom parent on purpose.
        fs::create_dir_all(&directory).unwrap();
        fs::set_permissions(&directory, fs::Permissions::from_mode(0o755)).unwrap();
        let path = directory.join("nested").join("state.json");
        atomic_write(&path, b"{\"a\":1}").unwrap();
        let parent_mode = fs::metadata(path.parent().unwrap())
            .unwrap()
            .permissions()
            .mode()
            & 0o777;
        assert_eq!(parent_mode, 0o700);
        let file_mode = fs::metadata(&path).unwrap().permissions().mode() & 0o777;
        assert_eq!(file_mode, 0o600);
        assert_eq!(fs::read(&path).unwrap(), b"{\"a\":1}");
        fs::remove_dir_all(&directory).ok();
    }

    #[test]
    fn atomic_write_concurrent_writers_stay_valid() {
        let directory =
            std::env::temp_dir().join(format!("daku-atomic-{}", uuid::Uuid::new_v4().simple()));
        let path = directory.join("shared.json");
        let handles: Vec<_> = (0..8)
            .map(|i| {
                let path = path.clone();
                std::thread::spawn(move || {
                    atomic_write(&path, format!("{{\"writer\":{i}}}").as_bytes()).unwrap();
                })
            })
            .collect();
        for handle in handles {
            handle.join().unwrap();
        }
        let body = fs::read(&path).unwrap();
        let value: serde_json::Value = serde_json::from_slice(&body).unwrap();
        assert!(
            value.get("writer").is_some(),
            "final write must be valid JSON"
        );
        fs::remove_dir_all(&directory).ok();
    }
}
