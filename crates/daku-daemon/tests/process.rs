//! Black-box tests for the daemon process: spawn, ready line, connect, reap.
//!
//! Every daemon spawned here runs with `HOME` pointed at a fresh temp
//! directory, so the child never sees the operator's `~/.daku` (settings, DB,
//! `environments.json`) and never reaches the Keychain or the network.

use std::io::{BufRead as _, BufReader, Read as _};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::mpsc;
use std::time::{Duration, Instant};

use daku_protocol::{Command as DaemonCommand, DaemonReady, PROTOCOL_VERSION, ResponsePayload};

const DAEMON: &str = env!("CARGO_BIN_EXE_daku-daemon");
const READY_TIMEOUT: Duration = Duration::from_secs(15);

/// Fresh empty `HOME` so the daemon never sees the operator's `~/.daku`.
fn sandbox_home() -> PathBuf {
    static COUNTER: AtomicU32 = AtomicU32::new(0);
    let home = std::env::temp_dir().join(format!(
        "daku-home-{}-{}",
        std::process::id(),
        COUNTER.fetch_add(1, Ordering::Relaxed)
    ));
    std::fs::create_dir_all(&home).unwrap();
    home
}

/// The supervisor spawns its child with the *inherited* environment, so `HOME`
/// has to be set for this whole test binary. Only the supervisor test relies on
/// it; every other test spawns with `env_clear()`.
fn ensure_process_home() -> PathBuf {
    static HOME: std::sync::OnceLock<PathBuf> = std::sync::OnceLock::new();
    HOME.get_or_init(|| {
        let home = sandbox_home();
        unsafe { std::env::set_var("HOME", &home) };
        home
    })
    .clone()
}

fn spawn_daemon(home: &Path, token: Option<&str>, extra: &[&str]) -> Child {
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
    command.spawn().unwrap()
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
    let home = sandbox_home();
    let mut child = spawn_daemon(&home, Some("test-token"), &[]);
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

    assert!(home.join(".daku/app.db").exists());
    assert!(!home.join(".daku/environments.json").exists());
    assert!(
        stderr.contains("daku collector idle: missing"),
        "expected an idle collector, got stderr: {stderr}"
    );
    std::fs::remove_dir_all(&home).unwrap();
}

#[test]
fn daemon_refuses_to_start_without_token() {
    let home = sandbox_home();
    let mut child = spawn_daemon(&home, None, &[]);
    let status = child.wait().unwrap();
    let stderr = read_stderr(&mut child);
    assert!(!status.success());
    assert!(
        stderr.contains("DAKU_DAEMON_TOKEN is missing"),
        "unexpected stderr: {stderr}"
    );
    std::fs::remove_dir_all(&home).unwrap();
}

#[test]
fn daemon_refuses_to_start_with_an_empty_token() {
    let home = sandbox_home();
    let mut child = spawn_daemon(&home, Some(""), &[]);
    let status = child.wait().unwrap();
    let stderr = read_stderr(&mut child);
    assert!(!status.success());
    assert!(stderr.contains("is empty"), "unexpected stderr: {stderr}");
    std::fs::remove_dir_all(&home).unwrap();
}

#[cfg(unix)]
#[test]
fn supervisor_spawns_and_reaps_the_daemon() {
    let _home = ensure_process_home();
    let supervisor = daku_client::DaemonSupervisor::spawn(Path::new(DAEMON), false).unwrap();
    let client = supervisor.client();
    assert!(matches!(
        client.request(DaemonCommand::Ping).unwrap(),
        ResponsePayload::Ack
    ));

    // Other tests in this file spawn daemons from the same process, so match on
    // the supervisor's own `--parent-pid <us>` argument rather than the binary
    // name. The trailing space anchors the pid against a longer one, and the
    // pattern drops the leading dashes so pgrep does not read it as an option.
    let self_pid = std::process::id().to_string();
    let listed = Command::new("pgrep")
        .args(["-P", &self_pid, "-f", &format!("parent-pid {self_pid} ")])
        .output()
        .unwrap();
    let found = String::from_utf8_lossy(&listed.stdout);
    let mut pids = found.lines();
    let pid: u32 = pids
        .next()
        .expect("no daemon child found")
        .trim()
        .parse()
        .unwrap();
    assert_eq!(pids.next(), None, "more than one supervised daemon");
    assert!(pid_alive(pid));

    drop(client);
    drop(supervisor);
    assert!(
        wait_until(Duration::from_secs(5), || !pid_alive(pid)),
        "daemon {pid} outlived its supervisor"
    );
}

#[cfg(unix)]
#[test]
fn daemon_exits_when_parent_pid_dies() {
    let home = sandbox_home();
    let mut parent = Command::new("sleep").arg("30").spawn().unwrap();
    let mut child = spawn_daemon(
        &home,
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
    std::fs::remove_dir_all(&home).unwrap();
}
