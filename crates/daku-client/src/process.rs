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

fn next_backoff(current: Duration) -> Duration {
    (current * 2).min(RESTART_BACKOFF_MAX)
}

/// Fresh bearer token per spawn. Loopback-only launches have no browser
/// clients to keep working across restarts, so nothing persists this.
fn new_daemon_token() -> String {
    Uuid::new_v4().simple().to_string()
}

pub(crate) struct DaemonProcess {
    client: DaemonClient,
    child: Child,
}

impl DaemonProcess {
    /// Loopback-only spawn: `--bind 127.0.0.1:0`, a fresh token per spawn,
    /// no origins, never `--allow-non-loopback` (that envelope is the
    /// daemon's own CLI for Operator-run debugging, not desktop launches).
    fn spawn(executable: &Path) -> anyhow::Result<Self> {
        let auth = new_daemon_token();
        let app_executable = std::env::current_exe().context("could not locate daku executable")?;
        let mut command = ProcessCommand::new(executable);
        command
            .arg("--bind")
            .arg("127.0.0.1:0")
            .arg("--parent-pid")
            .arg(std::process::id().to_string());
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

/// Test-only spelling of the production log path: production resolves the
/// home via [`daemon_home_dir`] (which honors `DAKU_HOME`), then appends
/// `daemon.log` directly.
#[cfg(test)]
fn daemon_log_path(home: &Path) -> PathBuf {
    home.join(".daku").join("daemon.log")
}

/// Operator data directory honoring `DAKU_HOME`, mirroring
/// `daku_core::config::daku_home_dir` (daku-client must not depend on
/// daku-core outside tests): `DAKU_HOME` when set and non-empty, else
/// `~/.daku/`. The daemon log, DB, and config then share one root instead
/// of splitting logs and state across two directories under automation.
fn daemon_home_dir() -> PathBuf {
    if let Ok(path) = std::env::var("DAKU_HOME")
        && !path.is_empty()
    {
        return PathBuf::from(path);
    }
    dirs::home_dir()
        .unwrap_or_else(std::env::temp_dir)
        .join(".daku")
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
    let path = daemon_home_dir().join("daemon.log");
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
        let process = DaemonProcess::spawn(executable)?;
        let initial_stamp = ExecutableStamp::read(executable)?;
        let supervisor = Self::from_target(
            DaemonTarget::Local(process),
            Some(executable.to_owned()),
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
        let supervisor = Self::from_target(DaemonTarget::Remote(client), None, remote)?;
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
        remote: Option<(String, String)>,
    ) -> anyhow::Result<Self> {
        Ok(Self {
            inner: Arc::new(SupervisorInner {
                executable,
                remote,
                target: Mutex::new(target),
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

    /// True when the daemon is a locally spawned child that this supervisor
    /// owns. Reload (shutdown + respawn) is only safe then: reloading a remote
    /// daemon would kill it and re-dial a dead address forever.
    pub fn is_local(&self) -> bool {
        self.inner.executable.is_some()
    }

    /// Reload config and poll now by restarting the locally supervised daemon.
    /// The fresh daemon re-reads `environments.json`/`settings.json` and ticks
    /// immediately; last-known cards replay from SQLite so the UI never blanks.
    /// No-op (error) for remote daemons. No protocol change.
    pub fn reload(&self) -> anyhow::Result<()> {
        if !self.is_local() {
            bail!("the connected daemon is managed outside daku Desktop");
        }
        // Shutdown is graceful: the daemon sends `ShuttingDown`, the client's
        // reader marks disconnected, and `monitor_daemon` (~500 ms poll)
        // respawns via `replace_local_daemon`. Total is ~0.5 s measured.
        self.client().shutdown();
        Ok(())
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
        let needs_restart = match &mut *inner.target.lock() {
            // A live child with a dead socket is unrecoverable from the UI's
            // side: `listen_dashboard` only ever gets a new client from
            // `replace_local_daemon`, so respawn rather than sit disconnected.
            DaemonTarget::Local(process) => {
                process.has_exited() || process.client().is_disconnected()
            }
            DaemonTarget::Restarting(_) => true,
            DaemonTarget::Remote(_) => return,
        };
        let Some(executable) = inner.executable.as_ref() else {
            return;
        };
        let observed_stamp = ExecutableStamp::read(executable).ok();
        let executable_changed =
            watch_for_rebuilds && observed_stamp.is_some_and(|observed| observed != active_stamp);
        if !needs_restart && !executable_changed {
            // Same as `monitor_remote`: a healthy poll clears the backoff so a
            // later failure starts from the minimum, not an inherited delay.
            backoff = RESTART_BACKOFF_MIN;
            continue;
        }
        let _restart = inner.restart.lock();
        match replace_local_daemon(&inner, executable) {
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

fn replace_local_daemon(inner: &SupervisorInner, executable: &Path) -> anyhow::Result<()> {
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
    let replacement = DaemonProcess::spawn(executable)?;
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

    #[test]
    fn daemon_home_honors_override_and_log_shares_it() {
        let override_dir = std::env::temp_dir().join(format!("daku-home-{}", Uuid::new_v4()));
        // SAFETY: test-only env mutation, restored below.
        unsafe { std::env::set_var("DAKU_HOME", &override_dir) };
        assert_eq!(daemon_home_dir(), override_dir);
        assert_eq!(
            daemon_home_dir().join("daemon.log"),
            override_dir.join("daemon.log")
        );
        unsafe { std::env::remove_var("DAKU_HOME") };
        std::fs::remove_dir_all(&override_dir).ok();
    }
}
