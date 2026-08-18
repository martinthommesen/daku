//! Black-box tests for the daemon process: spawn, ready line, connect, reap.
//!
//! Every daemon spawned here runs with `HOME` pointed at a fresh temp
//! directory, so the child never sees the operator's `~/.daku` (settings, DB,
//! `environments.json`) and never reaches the Keychain or the network.
//!
//! **Every test in this file takes `serialize_tests()` as its first statement.**
//! The supervisor spawns its child with the *inherited* environment, so the
//! sandbox has to be this process's own `HOME` for the length of those tests —
//! and `set_var` is only sound while no sibling thread is reading the
//! environment (`spawn_daemon` reads `PATH` on every call). Serializing the
//! binary is what removes that race; libtest still starts a thread per test,
//! but each blocks on the lock before touching anything.

use std::io::{BufRead as _, BufReader, Read as _};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::mpsc;
use std::time::{Duration, Instant};

use daku_protocol::{Command as DaemonCommand, DaemonReady, PROTOCOL_VERSION, ResponsePayload};

const DAEMON: &str = env!("CARGO_BIN_EXE_daku-daemon");
const READY_TIMEOUT: Duration = Duration::from_secs(15);

/// Serializes the whole binary — see the module docs. Supervisor tests also
/// identify their child by pid via pgrep, so they must not overlap either.
fn serialize_tests() -> std::sync::MutexGuard<'static, ()> {
    static LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());
    LOCK.lock().unwrap_or_else(|poisoned| poisoned.into_inner())
}

/// Fresh empty `HOME` so the daemon never sees the operator's `~/.daku`;
/// removed on drop, so a failing assertion litters nothing.
struct SandboxHome(PathBuf);

impl SandboxHome {
    fn new() -> Self {
        static COUNTER: AtomicU32 = AtomicU32::new(0);
        let home = std::env::temp_dir().join(format!(
            "daku-home-{}-{}",
            std::process::id(),
            COUNTER.fetch_add(1, Ordering::Relaxed)
        ));
        std::fs::create_dir_all(&home).unwrap();
        Self(home)
    }

    fn path(&self) -> &Path {
        &self.0
    }
}

impl Drop for SandboxHome {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

/// A sandbox that is also *this* process's `HOME`, because the supervisor
/// spawns its child with the inherited environment and derives its own daemon
/// log path from `HOME`. Sound only while `serialize_tests()` is held: no other
/// thread in this binary is reading the environment.
#[cfg(unix)]
fn supervisor_home() -> SandboxHome {
    let home = SandboxHome::new();
    unsafe { std::env::set_var("HOME", home.path()) };
    home
}

/// A spawned daemon that is killed and reaped on drop, so a panicking
/// assertion cannot leave a live daemon rewriting its `SandboxHome` after the
/// sandbox was removed. Derefs to `Child`: `kill`, `wait`, `id` all still work.
struct Daemon(Child);

impl std::ops::Deref for Daemon {
    type Target = Child;

    fn deref(&self) -> &Child {
        &self.0
    }
}

impl std::ops::DerefMut for Daemon {
    fn deref_mut(&mut self) -> &mut Child {
        &mut self.0
    }
}

impl Drop for Daemon {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

fn spawn_daemon(home: &Path, token: Option<&str>, extra: &[&str]) -> Daemon {
    let mut command = Command::new(DAEMON);
    command
        .env_clear()
        .env("HOME", home)
        .env("PATH", std::env::var_os("PATH").unwrap_or_default())
        .arg("--bind")
        .arg("127.0.0.1:0")
        .args(extra)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    if let Some(token) = token {
        command.env("DAKU_DAEMON_TOKEN", token);
    }
    Daemon(command.spawn().unwrap())
}

fn read_ready(child: &mut Child) -> DaemonReady {
    let stdout = child.stdout.take().unwrap();
    let (sender, receiver) = mpsc::sync_channel(1);
    std::thread::spawn(move || {
        let mut line = String::new();
        let read = BufReader::new(stdout).read_line(&mut line);
        let _ = sender.send(read.map(|bytes| (bytes, line)));
    });
    match receiver.recv_timeout(READY_TIMEOUT) {
        Ok(Ok((bytes, line))) => {
            assert!(bytes > 0, "daemon exited before printing a ready line");
            serde_json::from_str(&line).unwrap()
        }
        other => panic!("no ready line from the daemon: {other:?}"),
    }
}

fn read_stderr(child: &mut Child) -> String {
    let mut stderr = child.stderr.take().unwrap();
    let mut text = String::new();
    stderr.read_to_string(&mut text).unwrap();
    text
}

#[cfg(unix)]
fn pid_alive(pid: u32) -> bool {
    unsafe { libc::kill(pid as i32, 0) == 0 }
}

fn wait_until(deadline: Duration, mut condition: impl FnMut() -> bool) -> bool {
    let until = Instant::now() + deadline;
    while Instant::now() < until {
        if condition() {
            return true;
        }
        std::thread::sleep(Duration::from_millis(50));
    }
    condition()
}

#[test]
fn daemon_prints_one_ready_line_and_serves() {
    let _serialized = serialize_tests();
    let home = SandboxHome::new();
    let mut child = spawn_daemon(home.path(), Some("test-token"), &[]);
    let ready = read_ready(&mut child);

    assert_eq!(ready.protocol_version, PROTOCOL_VERSION);
    assert_eq!(ready.pid, child.id());
    let address: std::net::SocketAddr = ready.address.parse().unwrap();
    assert!(address.ip().is_loopback());
    assert_ne!(address.port(), 0);

    let client = daku_client::DaemonClient::connect(&ready.address, "test-token".into()).unwrap();
    assert!(matches!(
        client.request(DaemonCommand::Ping).unwrap(),
        ResponsePayload::Ack
    ));
    drop(client);

    child.kill().unwrap();
    child.wait().unwrap();
    let stderr = read_stderr(&mut child);

    assert!(home.path().join(".daku/app.db").exists());
    assert!(!home.path().join(".daku/environments.json").exists());
    assert!(
        stderr.contains("daku collector idle: missing"),
        "expected an idle collector, got stderr: {stderr}"
    );
}

#[test]
fn daemon_refuses_to_start_without_token() {
    let _serialized = serialize_tests();
    let home = SandboxHome::new();
    let mut child = spawn_daemon(home.path(), None, &[]);
    let status = child.wait().unwrap();
    let stderr = read_stderr(&mut child);
    assert!(!status.success());
    assert!(
        stderr.contains("DAKU_DAEMON_TOKEN is missing"),
        "unexpected stderr: {stderr}"
    );
}

#[test]
fn daemon_refuses_to_start_with_an_empty_token() {
    let _serialized = serialize_tests();
    let home = SandboxHome::new();
    let mut child = spawn_daemon(home.path(), Some(""), &[]);
    let status = child.wait().unwrap();
    let stderr = read_stderr(&mut child);
    assert!(!status.success());
    assert!(stderr.contains("is empty"), "unexpected stderr: {stderr}");
}

/// Other tests in this file spawn daemons from the same process, so match on
/// the supervisor's own `--parent-pid <us>` argument rather than the binary
/// name. The trailing space anchors the pid against a longer one, and the
/// pattern drops the leading dashes so pgrep does not read it as an option.
#[cfg(unix)]
fn supervised_daemon_pids() -> Vec<u32> {
    let self_pid = std::process::id().to_string();
    let listed = Command::new("pgrep")
        .args(["-P", &self_pid, "-f", &format!("parent-pid {self_pid} ")])
        .output()
        .unwrap();
    String::from_utf8_lossy(&listed.stdout)
        .lines()
        .map(|line| line.trim().parse().unwrap())
        .collect()
}

#[cfg(unix)]
fn only_supervised_daemon_pid() -> u32 {
    let pids = supervised_daemon_pids();
    assert_eq!(
        pids.len(),
        1,
        "expected one supervised daemon, got {pids:?}"
    );
    pids[0]
}

#[cfg(unix)]
#[test]
fn supervisor_spawns_and_reaps_the_daemon() {
    let _serialized = serialize_tests();
    let _home = supervisor_home();
    let supervisor = daku_client::DaemonSupervisor::spawn(Path::new(DAEMON), false).unwrap();
    let client = supervisor.client();
    assert!(matches!(
        client.request(DaemonCommand::Ping).unwrap(),
        ResponsePayload::Ack
    ));

    let pid = only_supervised_daemon_pid();
    assert!(pid_alive(pid));

    drop(client);
    drop(supervisor);
    assert!(
        wait_until(Duration::from_secs(5), || !pid_alive(pid)),
        "daemon {pid} outlived its supervisor"
    );
}

/// The daemon is thread-per-connection: a broken connection thread returns
/// while the process keeps running, so the supervisor has to watch the socket
/// and not only the child's exit status.
#[cfg(unix)]
#[test]
fn supervisor_replaces_a_daemon_whose_socket_dropped() {
    let _serialized = serialize_tests();
    let _home = supervisor_home();
    let supervisor = daku_client::DaemonSupervisor::spawn(Path::new(DAEMON), false).unwrap();
    let clients = supervisor.subscribe_clients();
    let first = clients.recv_timeout(READY_TIMEOUT).unwrap();
    let pid = only_supervised_daemon_pid();

    // Stop the child first, then close the connection from the client side: the
    // daemon never processes the shutdown, so it stays alive with a dead socket
    // — precisely the state `has_exited()` cannot see.
    assert_eq!(unsafe { libc::kill(pid as i32, libc::SIGSTOP) }, 0);
    first.shutdown();
    assert!(
        wait_until(Duration::from_secs(5), || first.is_disconnected()),
        "the client never noticed its socket closing"
    );
    assert!(pid_alive(pid), "the stalled daemon exited on its own");

    let replacement = clients
        .recv_timeout(Duration::from_secs(10))
        .expect("the supervisor never replaced the disconnected client");
    assert!(!replacement.is_disconnected());
    assert!(matches!(
        replacement.request(DaemonCommand::Ping).unwrap(),
        ResponsePayload::Ack
    ));
    assert!(
        wait_until(Duration::from_secs(5), || !pid_alive(pid)),
        "the stalled daemon {pid} was left running"
    );
}

#[cfg(unix)]
#[test]
fn supervisor_records_an_error_when_the_daemon_cannot_be_respawned() {
    use std::os::unix::fs::PermissionsExt as _;

    let _serialized = serialize_tests();
    let _home = supervisor_home();
    let directory = SandboxHome::new();
    let executable = directory.path().join("daku-daemon-copy");
    std::fs::copy(DAEMON, &executable).unwrap();

    let supervisor = daku_client::DaemonSupervisor::spawn(&executable, false).unwrap();
    // Unspawnable, then ask the daemon to exit: the respawn must fail.
    std::fs::set_permissions(&executable, std::fs::Permissions::from_mode(0o600)).unwrap();
    supervisor.client().shutdown();
    assert!(
        wait_until(Duration::from_secs(20), || supervisor
            .last_error()
            .is_some()),
        "a failed respawn was never reported"
    );

    // The monitor must keep retrying rather than unwind on the first failure.
    std::fs::set_permissions(&executable, std::fs::Permissions::from_mode(0o755)).unwrap();
    assert!(
        wait_until(Duration::from_secs(30), || supervisor
            .last_error()
            .is_none()),
        "the supervisor stopped retrying after a failed respawn"
    );
    assert!(matches!(
        supervisor.client().request(DaemonCommand::Ping).unwrap(),
        ResponsePayload::Ack
    ));

    drop(supervisor);
}

#[cfg(unix)]
#[test]
fn daemon_exits_when_parent_pid_dies() {
    let _serialized = serialize_tests();
    let home = SandboxHome::new();
    let mut parent = Command::new("sleep").arg("30").spawn().unwrap();
    let mut child = spawn_daemon(
        home.path(),
        Some("test-token"),
        &["--parent-pid", &parent.id().to_string()],
    );
    read_ready(&mut child);

    parent.kill().unwrap();
    parent.wait().unwrap();
    assert!(
        wait_until(Duration::from_secs(5), || matches!(
            child.try_wait(),
            Ok(Some(_))
        )),
        "daemon outlived its parent"
    );
}
