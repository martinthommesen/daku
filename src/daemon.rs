//! Desktop ownership of the daku daemon process.

use std::path::PathBuf;

use anyhow::{Context as _, anyhow, bail};

pub fn start_process() -> anyhow::Result<daku_client::DaemonSupervisor> {
    let address = std::env::var(daku_protocol::DAEMON_ADDRESS_ENV)
        .ok()
        .filter(|value| !value.trim().is_empty());
    let auth = std::env::var(daku_protocol::DAEMON_TOKEN_ENV)
        .ok()
        .filter(|value| !value.is_empty());
    match (address, auth) {
        (Some(address), Some(auth)) => {
            return daku_client::DaemonSupervisor::connect(address.trim(), auth);
        }
        (Some(_), None) => bail!(
            "{} is set but {} is missing",
            daku_protocol::DAEMON_ADDRESS_ENV,
            daku_protocol::DAEMON_TOKEN_ENV
        ),
        (None, Some(_)) => bail!(
            "{} is set but {} is missing",
            daku_protocol::DAEMON_TOKEN_ENV,
            daku_protocol::DAEMON_ADDRESS_ENV
        ),
        (None, None) => {}
    }
    let app_settings = daku_client::persistence::load_or_create_app_settings()
        .context("could not load desktop daemon settings")?;
    daku_client::DaemonSupervisor::spawn_configured(
        &daemon_executable_path()?,
        cfg!(debug_assertions),
        app_settings.daemon_exposure,
    )
}

fn daemon_executable_path() -> anyhow::Result<PathBuf> {
    if let Some(path) = std::env::var_os("DAKU_DAEMON_PATH").filter(|path| !path.is_empty()) {
        return Ok(path.into());
    }
    let executable = format!("daku-daemon{}", std::env::consts::EXE_SUFFIX);
    let current = std::env::current_exe().context("could not locate the daku executable")?;

    // Development keeps the daemon beside Cargo's debug artifacts rather than
    // inside daku Debug.app. The supervisor watches this file and swaps only
    // the daemon when the development watcher relinks it.
    #[cfg(debug_assertions)]
    if let Some(debug_directory) = current
        .ancestors()
        .find(|candidate| candidate.file_name().is_some_and(|name| name == "debug"))
    {
        let external = debug_directory.join(&executable);
        if external.is_file() {
            return Ok(external);
        }
    }

    let sibling = current
        .parent()
        .map(|directory| directory.join(&executable))
        .ok_or_else(|| anyhow!("daku executable has no parent directory"))?;
    if sibling.is_file() {
        return Ok(sibling);
    }
    #[cfg(debug_assertions)]
    bail!(
        "daku daemon was not found in Cargo's debug directory or next to the app executable: {}",
        sibling.display(),
    );
    #[cfg(not(debug_assertions))]
    bail!(
        "daku daemon is missing next to the app executable: {}",
        sibling.display(),
    )
}
