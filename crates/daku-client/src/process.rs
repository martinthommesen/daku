use std::collections::HashSet;
use std::io::{BufRead as _, BufReader};
use std::path::{Path, PathBuf};
use std::process::{Child, Command as ProcessCommand, Stdio};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc;
use std::time::SystemTime;
use std::time::{Duration, Instant};

use anyhow::{Context as _, bail};
use crossbeam_channel::{Receiver, Sender, unbounded};
use parking_lot::Mutex;
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::DaemonClient;
use daku_protocol::{APP_EXECUTABLE_ENV, DAEMON_TOKEN_ENV, DaemonReady, PROTOCOL_VERSION};
const START_TIMEOUT: Duration = Duration::from_secs(15);
const SHUTDOWN_TIMEOUT: Duration = Duration::from_secs(1);
const REBUILD_POLL_INTERVAL: Duration = Duration::from_millis(500);
/// First delay after a failed respawn/reconnect; doubles per failure.
const RESTART_BACKOFF_MIN: Duration = Duration::from_millis(500);
/// Ceiling for the doubling — a dead daemon costs one spawn per 30 s, not two per second.
const RESTART_BACKOFF_MAX: Duration = Duration::from_secs(30);
pub const DEFAULT_EXPOSED_DAEMON_PORT: u16 = 34_123;

fn next_backoff(current: Duration) -> Duration {
    (current * 2).min(RESTART_BACKOFF_MAX)
}

/// Desktop-owned launch configuration for the daemon it supervises.
///
/// Provider settings belong to the daemon and live in `settings.json`; this
/// is an app preference because it controls how the desktop launches its own
/// child process. The bearer token is intentionally stable across daemon-only
/// rebuilds and desktop relaunches so a configured web client keeps working.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(default)]
pub struct DaemonExposureSettings {
    pub enabled: bool,
    pub port: u16,
    pub allowed_origins: Vec<String>,
    pub token: String,
}

impl Default for DaemonExposureSettings {
    fn default() -> Self {
        Self {
            enabled: false,
            port: DEFAULT_EXPOSED_DAEMON_PORT,
            allowed_origins: vec!["http://localhost:3001".into()],
            token: Self::new_token(),
        }
    }
}

impl DaemonExposureSettings {
    pub fn new_token() -> String {
        Uuid::new_v4().simple().to_string()
    }

    pub fn ensure_token(&mut self) -> bool {
        if !self.token.trim().is_empty() {
            return false;
        }
        self.token.clear();
        self.token.push_str(&Self::new_token());
        true
    }

    pub fn allowed_origins_text(&self) -> String {
        self.allowed_origins.join(", ")
    }

    pub fn with_allowed_origins_text(mut self, text: &str) -> anyhow::Result<Self> {
        self.allowed_origins = parse_allowed_origins(text)?;
        Ok(self)
    }

    pub fn validate(mut self) -> anyhow::Result<Self> {
        if self.port == 0 {
            bail!("daemon port must be between 1 and 65535");
        }
        if self.token.trim().is_empty() {
            bail!("daemon authentication token is empty");
        }
        self.allowed_origins = parse_allowed_origins(&self.allowed_origins_text())?;
        Ok(self)
    }

    fn bind_address(&self) -> String {
        if self.enabled {
            format!("0.0.0.0:{}", self.port)
        } else {
            "127.0.0.1:0".into()
        }
    }
}

/// Parse the comma-separated exact browser origins edited by the desktop.
/// Browser Origin headers contain only an HTTP(S) origin, never a path.
pub fn parse_allowed_origins(text: &str) -> anyhow::Result<Vec<String>> {
    let mut origins = Vec::new();
    let mut seen = HashSet::new();
    for candidate in text
        .split(',')
        .map(str::trim)
        .filter(|value| !value.is_empty())
    {
        let url = url::Url::parse(candidate)
            .with_context(|| format!("invalid browser origin {candidate:?}"))?;
        if !matches!(url.scheme(), "http" | "https")
            || !url.username().is_empty()
            || url.password().is_some()
            || url.query().is_some()
            || url.fragment().is_some()
            || url.path() != "/"
        {
            bail!(
                "browser origin {candidate:?} must be an exact http:// or https:// origin without a path"
            );
        }
        let origin = url.origin().ascii_serialization();
        if origin == "null" {
            bail!("browser origin {candidate:?} is not a network origin");
        }
        if seen.insert(origin.clone()) {
            origins.push(origin);
        }
    }
    Ok(origins)
}

pub(crate) struct DaemonProcess {
    client: DaemonClient,
    child: Child,
}

impl DaemonProcess {
    fn spawn_configured(
        executable: &Path,
        settings: DaemonExposureSettings,
    ) -> anyhow::Result<Self> {
        let settings = settings.validate()?;
        let auth = settings.token.clone();
        let app_executable = std::env::current_exe().context("could not locate daku executable")?;
        let mut command = ProcessCommand::new(executable);
        command
            .arg("--bind")
            .arg(settings.bind_address())
            .arg("--parent-pid")
            .arg(std::process::id().to_string());
        if settings.enabled {
            command.arg("--allow-non-loopback");
        }
        for origin in &settings.allowed_origins {
            command.arg("--allow-origin").arg(origin);
        }
        let mut child = command
            .env(DAEMON_TOKEN_ENV, &auth)
            .env(APP_EXECUTABLE_ENV, app_executable)
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(daemon_log_stdio())
            .spawn()
            .with_context(|| format!("could not launch {}", executable.display()))?;
        let stdout = child
            .stdout
            .take()
            .context("daku daemon did not expose its readiness stream")?;
        let (ready_tx, ready_rx) = mpsc::sync_channel(1);
        std::thread::Builder::new()
            .name("daku-daemon-ready".into())
            .spawn(move || {
                let mut line = String::new();
                let result = BufReader::new(stdout)
                    .read_line(&mut line)
                    .map_err(anyhow::Error::from)
                    .and_then(|bytes| {
                        if bytes == 0 {
                            bail!("daku daemon exited before becoming ready")
                        }
                        serde_json::from_str::<DaemonReady>(&line).map_err(anyhow::Error::from)
                    });
                let _ = ready_tx.send(result);
            })
            .context("could not start daku daemon readiness reader")?;
        let ready = match ready_rx.recv_timeout(START_TIMEOUT) {
            Ok(Ok(ready)) => ready,
            Ok(Err(error)) => {
                let _ = child.kill();
                let _ = child.wait();
                return Err(error);
            }
            Err(error) => {
                let _ = child.kill();
                let _ = child.wait();
                bail!("timed out waiting for daku daemon: {error}");
            }
        };
        if ready.protocol_version != PROTOCOL_VERSION {
            let _ = child.kill();
            let _ = child.wait();
            bail!(
                "daemon protocol {} does not match desktop protocol {}",
                ready.protocol_version,
                PROTOCOL_VERSION
            );
        }
        let client_address = match desktop_client_address(&ready.address) {
            Ok(address) => address,
            Err(error) => {
                let _ = child.kill();
                let _ = child.wait();
                return Err(error);
            }
        };
        let client = match DaemonClient::connect(&client_address, auth) {
            Ok(client) => client,
            Err(error) => {
                let _ = child.kill();
                let _ = child.wait();
                return Err(error);
            }
        };
        Ok(Self { client, child })
    }

    pub fn client(&self) -> DaemonClient {
        self.client.clone()
    }

    fn has_exited(&mut self) -> bool {
        !matches!(self.child.try_wait(), Ok(None))
    }

    fn stop(&mut self) {
        self.client.shutdown();
        let deadline = Instant::now() + SHUTDOWN_TIMEOUT;
        while Instant::now() < deadline {
            match self.child.try_wait() {
                Ok(Some(_)) => return,
                Ok(None) => std::thread::sleep(Duration::from_millis(25)),
                Err(_) => break,
            }
        }
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

impl Drop for DaemonProcess {
    fn drop(&mut self) {
        self.stop();
    }
}

fn daemon_log_path(home: &Path) -> PathBuf {
    home.join(".daku").join("daemon.log")
}

fn open_daemon_log(path: &Path) -> std::io::Result<std::fs::File> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let mut options = std::fs::OpenOptions::new();
    options.create(true).append(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt as _;
        options.mode(0o600);
    }
    options.open(path)
}

/// `~/.daku/daemon.log`, append-only, 0600. The daemon writes its diagnostics
/// to stderr; a packaged app has no terminal, so the supervisor points stderr
/// here. Falls back to inheriting stderr when the file cannot be opened.
fn daemon_log_stdio() -> Stdio {
    let path = daemon_log_path(&dirs::home_dir().unwrap_or_else(std::env::temp_dir));
    match open_daemon_log(&path) {
        Ok(file) => Stdio::from(file),
        Err(error) => {
            eprintln!("could not open {} for daemon logs: {error}", path.display());
            Stdio::inherit()
        }
    }
}

fn desktop_client_address(address: &str) -> anyhow::Result<String> {
    let address = address
        .parse::<std::net::SocketAddr>()
        .with_context(|| format!("daku daemon returned an invalid address {address:?}"))?;
    let ip = if address.ip().is_unspecified() {
        if address.is_ipv4() {
            std::net::IpAddr::V4(std::net::Ipv4Addr::LOCALHOST)
        } else {
            std::net::IpAddr::V6(std::net::Ipv6Addr::LOCALHOST)
        }
    } else {
        address.ip()
    };
    Ok(std::net::SocketAddr::new(ip, address.port()).to_string())
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct ExecutableStamp {
    modified: Option<SystemTime>,
    len: u64,
}

impl ExecutableStamp {
    fn read(path: &Path) -> anyhow::Result<Self> {
        let metadata = std::fs::metadata(path)
            .with_context(|| format!("could not inspect {}", path.display()))?;
        Ok(Self {
            modified: metadata.modified().ok(),
            len: metadata.len(),
        })
    }
}

struct SupervisorInner {
    executable: Option<PathBuf>,
    /// Address and token of a daemon managed elsewhere, kept for reconnects.
    remote: Option<(String, String)>,
    target: Mutex<DaemonTarget>,
    exposure: Mutex<Option<DaemonExposureSettings>>,
    restart: Mutex<()>,
    client_updates: Mutex<Vec<Sender<DaemonClient>>>,
    last_error: Mutex<Option<String>>,
    running: AtomicBool,
}

enum DaemonTarget {
    Local(DaemonProcess),
    Restarting(DaemonClient),
    Remote(DaemonClient),
}

impl DaemonTarget {
    fn client(&self) -> DaemonClient {
        match self {
            Self::Local(process) => process.client(),
            Self::Restarting(client) => client.clone(),
            Self::Remote(client) => client.clone(),
        }
    }
}

/// Owns the current daemon and, in development, swaps it after a successful
/// rebuild without requiring the desktop process to relaunch.
#[derive(Clone)]
pub struct DaemonSupervisor {
    inner: Arc<SupervisorInner>,
}

impl DaemonSupervisor {
    pub fn spawn(executable: &Path, watch_for_rebuilds: bool) -> anyhow::Result<Self> {
        Self::spawn_configured(
            executable,
            watch_for_rebuilds,
            DaemonExposureSettings::default(),
        )
    }

    pub fn spawn_configured(
        executable: &Path,
        watch_for_rebuilds: bool,
        exposure: DaemonExposureSettings,
    ) -> anyhow::Result<Self> {
        let exposure = exposure.validate()?;
        let process = DaemonProcess::spawn_configured(executable, exposure.clone())?;
        let initial_stamp = ExecutableStamp::read(executable)?;
        let supervisor = Self::from_target(
            DaemonTarget::Local(process),
            Some(executable.to_owned()),
            Some(exposure),
            None,
        )?;
        let weak_inner = Arc::downgrade(&supervisor.inner);
        std::thread::Builder::new()
            .name("daku-daemon-supervisor".into())
            .spawn(move || monitor_daemon(weak_inner, initial_stamp, watch_for_rebuilds))
            .context("could not start daku daemon supervisor")?;
        Ok(supervisor)
    }

    /// Connect to a daemon managed on another host (or by an external local
    /// service manager). Dropping the desktop never shuts this daemon down.
    pub fn connect(address: &str, token: String) -> anyhow::Result<Self> {
        let remote = Some((address.to_owned(), token.clone()));
        let client = DaemonClient::connect(address, token)?;
        let supervisor = Self::from_target(DaemonTarget::Remote(client), None, None, remote)?;
        let weak_inner = Arc::downgrade(&supervisor.inner);
        std::thread::Builder::new()
            .name("daku-daemon-reconnect".into())
            .spawn(move || monitor_remote(weak_inner))
            .context("could not start daku daemon reconnect monitor")?;
        Ok(supervisor)
    }

    fn from_target(
        target: DaemonTarget,
        executable: Option<PathBuf>,
        exposure: Option<DaemonExposureSettings>,
        remote: Option<(String, String)>,
    ) -> anyhow::Result<Self> {
        Ok(Self {
            inner: Arc::new(SupervisorInner {
                executable,
                remote,
                target: Mutex::new(target),
                exposure: Mutex::new(exposure),
                restart: Mutex::new(()),
                client_updates: Mutex::new(Vec::new()),
                last_error: Mutex::new(None),
                running: AtomicBool::new(true),
            }),
        })
    }

    /// Why the last respawn/reconnect failed, if it did (cleared on success).
    pub fn last_error(&self) -> Option<String> {
        self.inner.last_error.lock().clone()
    }

    pub fn client(&self) -> DaemonClient {
        self.inner.target.lock().client()
    }

    /// Subscribe to the active daemon connection. The current client is sent
    /// immediately, followed by each replacement after a managed restart.
    pub fn subscribe_clients(&self) -> Receiver<DaemonClient> {
        let (updates, receiver) = unbounded();
        // Holding the target lock through registration makes the initial send
        // atomic with respect to replacement: a subscriber sees either the old
        // client followed by the new one, or the new client directly.
        let target = self.inner.target.lock();
        self.inner.client_updates.lock().push(updates.clone());
        let _ = updates.send(target.client());
        receiver
    }
}

impl Drop for DaemonSupervisor {
    fn drop(&mut self) {
        if Arc::strong_count(&self.inner) == 1 {
            self.inner.running.store(false, Ordering::Release);
        }
    }
}

fn monitor_daemon(
    weak_inner: std::sync::Weak<SupervisorInner>,
    mut active_stamp: ExecutableStamp,
    watch_for_rebuilds: bool,
) {
    let mut backoff = RESTART_BACKOFF_MIN;
    loop {
        std::thread::sleep(REBUILD_POLL_INTERVAL);
        let Some(inner) = weak_inner.upgrade() else {
            return;
        };
        if !inner.running.load(Ordering::Acquire) {
            return;
        }
        let process_exited = match &mut *inner.target.lock() {
            DaemonTarget::Local(process) => process.has_exited(),
            DaemonTarget::Restarting(_) => true,
            DaemonTarget::Remote(_) => return,
        };
        let Some(executable) = inner.executable.as_ref() else {
            return;
        };
        let observed_stamp = ExecutableStamp::read(executable).ok();
        let executable_changed =
            watch_for_rebuilds && observed_stamp.is_some_and(|observed| observed != active_stamp);
        if !process_exited && !executable_changed {
            continue;
        }
        let _restart = inner.restart.lock();
        let Some(exposure) = inner.exposure.lock().clone() else {
            return;
        };
        match replace_local_daemon(&inner, executable, &exposure) {
            Ok(()) => {
                *inner.last_error.lock() = None;
                backoff = RESTART_BACKOFF_MIN;
            }
            Err(error) => {
                let message = format!("{error:#}");
                eprintln!("could not restart daku daemon (retry in {backoff:?}): {message}");
                *inner.last_error.lock() = Some(message);
                drop(_restart);
                drop(inner);
                std::thread::sleep(backoff);
                backoff = next_backoff(backoff);
                continue;
            }
        }
        if let Some(observed_stamp) = observed_stamp {
            active_stamp = observed_stamp;
        }
        drop(_restart);
        drop(inner);
    }
}

/// Poll a daemon managed elsewhere and re-dial it when the socket drops, so a
/// daemon upgrade or a laptop sleep does not require relaunching the desktop.
fn monitor_remote(weak_inner: std::sync::Weak<SupervisorInner>) {
    let mut backoff = RESTART_BACKOFF_MIN;
    loop {
        std::thread::sleep(REBUILD_POLL_INTERVAL);
        let Some(inner) = weak_inner.upgrade() else {
            return;
        };
        if !inner.running.load(Ordering::Acquire) {
            return;
        }
        let Some((address, token)) = inner.remote.clone() else {
            return;
        };
        let disconnected = match &*inner.target.lock() {
            DaemonTarget::Remote(client) => client.is_disconnected(),
            _ => return,
        };
        if !disconnected {
            backoff = RESTART_BACKOFF_MIN;
            continue;
        }
        match DaemonClient::connect(&address, token) {
            Ok(client) => {
                *inner.target.lock() = DaemonTarget::Remote(client.clone());
                inner
                    .client_updates
                    .lock()
                    .retain(|subscriber| subscriber.send(client.clone()).is_ok());
                *inner.last_error.lock() = None;
                backoff = RESTART_BACKOFF_MIN;
            }
            Err(error) => {
                let message = format!("{error:#}");
                eprintln!(
                    "could not reconnect to daku daemon at {address} (retry in {backoff:?}): {message}"
                );
                *inner.last_error.lock() = Some(message);
                drop(inner);
                std::thread::sleep(backoff);
                backoff = next_backoff(backoff);
            }
        }
    }
}

fn replace_local_daemon(
    inner: &SupervisorInner,
    executable: &Path,
    exposure: &DaemonExposureSettings,
) -> anyhow::Result<()> {
    let previous = {
        let mut target = inner.target.lock();
        match &*target {
            DaemonTarget::Remote(_) => {
                bail!("the connected daemon is managed outside daku Desktop")
            }
            DaemonTarget::Restarting(_) => None,
            DaemonTarget::Local(process) => {
                let disconnected = process.client();
                let previous =
                    std::mem::replace(&mut *target, DaemonTarget::Restarting(disconnected));
                match previous {
                    DaemonTarget::Local(process) => Some(process),
                    _ => unreachable!("local daemon target changed while locked"),
                }
            }
        }
    };
    // Dropping can wait briefly for graceful shutdown, but the target lock is
    // already released so UI actions never block behind process teardown.
    drop(previous);
    let replacement = DaemonProcess::spawn_configured(executable, exposure.clone())?;
    let client = replacement.client();
    *inner.target.lock() = DaemonTarget::Local(replacement);
    inner
        .client_updates
        .lock()
        .retain(|subscriber| subscriber.send(client.clone()).is_ok());
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn browser_origins_are_exact_and_deduplicated() {
        assert_eq!(
            parse_allowed_origins(
                "https://app.daku.test, http://localhost:3001, https://app.daku.test"
            )
            .unwrap(),
            ["https://app.daku.test", "http://localhost:3001"]
        );
        assert!(parse_allowed_origins("https://app.daku.test/path").is_err());
        assert!(parse_allowed_origins("ws://app.daku.test").is_err());
    }

    #[test]
    fn backoff_doubles_and_caps() {
        assert_eq!(next_backoff(RESTART_BACKOFF_MIN), Duration::from_secs(1));
        assert_eq!(next_backoff(Duration::from_secs(20)), RESTART_BACKOFF_MAX);
        assert_eq!(next_backoff(RESTART_BACKOFF_MAX), RESTART_BACKOFF_MAX);
    }

    #[test]
    fn desktop_uses_loopback_to_reach_an_unspecified_listener() {
        assert_eq!(
            desktop_client_address("0.0.0.0:34123").unwrap(),
            "127.0.0.1:34123"
        );
        assert_eq!(desktop_client_address("[::]:34123").unwrap(), "[::1]:34123");
    }

    #[test]
    fn exposure_validate_rejects_port_zero_and_empty_token() {
        let settings = DaemonExposureSettings {
            port: 0,
            ..DaemonExposureSettings::default()
        };
        assert!(
            settings
                .validate()
                .unwrap_err()
                .to_string()
                .contains("port")
        );
        let settings = DaemonExposureSettings {
            token: "   ".into(),
            ..DaemonExposureSettings::default()
        };
        assert!(
            settings
                .validate()
                .unwrap_err()
                .to_string()
                .contains("token")
        );
        assert!(DaemonExposureSettings::default().validate().is_ok());
    }

    #[test]
    fn ensure_token_mints_only_when_empty() {
        let mut settings = DaemonExposureSettings::default();
        let before = settings.token.clone();
        assert!(!settings.ensure_token());
        assert_eq!(settings.token, before);
        settings.token.clear();
        assert!(settings.ensure_token());
        assert!(!settings.token.trim().is_empty());
    }

    #[test]
    fn daemon_log_opens_append_only_0600() {
        let home = std::env::temp_dir().join(format!("daku-log-{}", Uuid::new_v4()));
        let path = daemon_log_path(&home);
        for line in ["first\n", "second\n"] {
            let mut file = open_daemon_log(&path).unwrap();
            std::io::Write::write_all(&mut file, line.as_bytes()).unwrap();
        }
        assert_eq!(std::fs::read_to_string(&path).unwrap(), "first\nsecond\n");
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt as _;
            let mode = std::fs::metadata(&path).unwrap().permissions().mode();
            assert_eq!(mode & 0o777, 0o600);
        }
        std::fs::remove_dir_all(&home).unwrap();
    }
}
