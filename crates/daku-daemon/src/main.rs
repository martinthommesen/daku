use std::io::Write as _;
use std::net::{SocketAddr, TcpListener};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use anyhow::{Context as _, anyhow, bail};
use daku_protocol::{DAEMON_TOKEN_ENV, DaemonReady, PROTOCOL_VERSION};

fn main() -> anyhow::Result<()> {
    let arguments = Arguments::parse(std::env::args().skip(1))?;
    if arguments.probe_availability {
        return run_probe_availability();
    }
    if arguments.doctor {
        return run_doctor_command();
    }
    let auth = require_token(std::env::var(DAEMON_TOKEN_ENV))?;
    // The bearer capability belongs only to this server process. Remove it
    // before any provider or workspace subprocess can inherit the daemon's
    // environment.
    unsafe { std::env::remove_var(DAEMON_TOKEN_ENV) };
    let listener = TcpListener::bind(&arguments.bind)
        .with_context(|| format!("could not bind daku daemon to {}", arguments.bind))?;
    let address = listener.local_addr()?;
    ensure_bind_allowed(address, arguments.allow_non_loopback)?;
    let ready = DaemonReady {
        address: address.to_string(),
        protocol_version: PROTOCOL_VERSION,
        pid: std::process::id(),
    };
    println!("{}", serde_json::to_string(&ready)?);
    std::io::stdout().flush()?;

    let shutdown = Arc::new(AtomicBool::new(false));
    if let Some(parent_pid) = arguments.parent_pid {
        let monitor_shutdown = shutdown.clone();
        std::thread::Builder::new()
            .name("daku-daemon-parent".into())
            .spawn(move || {
                while !monitor_shutdown.load(Ordering::Acquire) {
                    if !process_is_alive(parent_pid) {
                        monitor_shutdown.store(true, Ordering::Release);
                        break;
                    }
                    std::thread::sleep(Duration::from_millis(500));
                }
            })?;
    }

    let task_path = daku_core::persistence::StateStore::default_path();
    let settings = daku_core::DaemonSettingsStore::open(daku_core::DaemonSettings::default_path())
        .context("could not load daemon settings")?;
    let task_store = daku_core::persistence::StateStore::daemon(task_path.clone());
    let dashboard_events = daku_core::start_default_loop(
        &daku_core::default_environments_path(),
        daku_core::persistence::StateStore::daemon(task_path),
        &settings.get(),
        shutdown.clone(),
    );
    daku_core::serve(
        listener,
        auth,
        Arc::new(daku_core::HollowBackend::new(settings, task_store)?),
        shutdown,
        daku_core::ServerOptions {
            allowed_origins: arguments.allowed_origins.into_iter().collect(),
            allow_shutdown: arguments.parent_pid.is_some(),
        },
        dashboard_events,
    )
}

fn run_probe_availability() -> anyhow::Result<()> {
    let store = daku_core::persistence::StateStore::daemon(
        daku_core::persistence::StateStore::default_path(),
    );
    daku_core::probe_availability_once(&daku_core::default_environments_path(), store)?;
    println!("availability probe complete");
    Ok(())
}

fn run_doctor_command() -> anyhow::Result<()> {
    let settings = daku_core::DaemonSettingsStore::open(daku_core::DaemonSettings::default_path())
        .context("could not load daemon settings")?
        .get();
    let environments_path = daku_core::default_environments_path();
    let report = daku_core::run_doctor(
        &environments_path,
        &settings,
        Arc::new(daku_core::config::KeychainCredentialStore),
        daku_core::servicenow::ServiceNowClient::new(
            daku_core::servicenow::UreqTransport::default(),
            daku_core::servicenow::SystemClock,
        ),
        daku_core::persistence::StateStore::daemon(
            daku_core::persistence::StateStore::default_path(),
        ),
    )
    .with_context(|| format!("doctor: {}", environments_path.display()))?;
    println!("config: {}", report.environments_path.display());
    println!("poll interval: {} s", report.poll_interval_secs);
    for row in &report.rows {
        println!("{}", format_doctor_row(row));
    }
    std::process::exit(doctor_exit_code(&report.rows));
}

/// Exit 1 when any Environment lacks a Credential or is unreachable; `asleep`
/// (hibernating PDI) is not a failure.
fn doctor_exit_code(rows: &[daku_core::DoctorRow]) -> i32 {
    i32::from(
        rows.iter()
            .any(|row| !row.credential_present || row.reachability == "unreachable"),
    )
}

fn format_doctor_row(row: &daku_core::DoctorRow) -> String {
    let credential = match (row.credential_present, &row.credential_error) {
        (true, _) => "credential: present".to_owned(),
        (false, None) => "credential: MISSING (Keychain service daku, account = id)".to_owned(),
        (false, Some(error)) => format!("credential: ERROR {error}"),
    };
    format!(
        "{} ({}) · {} · {} {} · build {} · {} ms{}",
        row.id,
        row.label,
        credential,
        row.reachability,
        row.state,
        row.build.as_deref().unwrap_or("—"),
        row.rtt_ms,
        row.error
            .as_deref()
            .map(|error| format!(" · {error}"))
            .unwrap_or_default(),
    )
}

fn ensure_bind_allowed(address: SocketAddr, allow_non_loopback: bool) -> anyhow::Result<()> {
    if address.ip().is_loopback() || allow_non_loopback {
        return Ok(());
    }
    bail!(
        "refusing non-loopback daemon bind {address}; pass --allow-non-loopback only after configuring authentication and exact browser origins"
    )
}

struct Arguments {
    bind: String,
    parent_pid: Option<u32>,
    allowed_origins: Vec<String>,
    allow_non_loopback: bool,
    probe_availability: bool,
    doctor: bool,
}

impl Arguments {
    fn parse(arguments: impl IntoIterator<Item = String>) -> anyhow::Result<Self> {
        let mut bind = "127.0.0.1:0".to_owned();
        let mut parent_pid = None;
        let mut allowed_origins = Vec::new();
        let mut allow_non_loopback = false;
        let mut probe_availability = false;
        let mut doctor = false;
        let mut arguments = arguments.into_iter();
        while let Some(argument) = arguments.next() {
            match argument.as_str() {
                "probe-availability" => {
                    probe_availability = true;
                }
                "doctor" => {
                    doctor = true;
                }
                "--bind" => {
                    bind = arguments
                        .next()
                        .ok_or_else(|| anyhow!("--bind requires an address"))?;
                }
                "--parent-pid" => {
                    parent_pid = Some(
                        arguments
                            .next()
                            .ok_or_else(|| anyhow!("--parent-pid requires a process id"))?
                            .parse()
                            .context("--parent-pid is not a valid process id")?,
                    );
                }
                "--allow-origin" => {
                    let origin = arguments
                        .next()
                        .filter(|origin| !origin.trim().is_empty())
                        .ok_or_else(|| anyhow!("--allow-origin requires an origin"))?;
                    allowed_origins.push(origin);
                }
                "--allow-non-loopback" => {
                    allow_non_loopback = true;
                }
                "--help" | "-h" => {
                    println!(
                        "usage: {} [probe-availability] [doctor] [--bind ADDRESS] [--allow-non-loopback] [--parent-pid PID] [--allow-origin ORIGIN]...",
                        env!("CARGO_BIN_NAME")
                    );
                    std::process::exit(0);
                }
                unknown => bail!("unknown argument {unknown:?}"),
            }
        }
        Ok(Self {
            bind,
            parent_pid,
            allowed_origins,
            allow_non_loopback,
            probe_availability,
            doctor,
        })
    }
}

fn require_token(value: Result<String, std::env::VarError>) -> anyhow::Result<String> {
    let bearer = value.context("DAKU_DAEMON_TOKEN is missing")?;
    if bearer.trim().is_empty() {
        bail!("DAKU_DAEMON_TOKEN is empty; refusing to start an unauthenticated daemon");
    }
    Ok(bearer)
}

#[cfg(unix)]
fn process_is_alive(pid: u32) -> bool {
    let result = unsafe { libc::kill(pid as i32, 0) };
    result == 0 || std::io::Error::last_os_error().raw_os_error() == Some(libc::EPERM)
}

#[cfg(not(unix))]
fn process_is_alive(_pid: u32) -> bool {
    true
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn non_loopback_listener_requires_an_explicit_flag() {
        assert!(ensure_bind_allowed("127.0.0.1:3000".parse().unwrap(), false).is_ok());
        assert!(ensure_bind_allowed("[::1]:3000".parse().unwrap(), false).is_ok());
        assert!(ensure_bind_allowed("0.0.0.0:3000".parse().unwrap(), false).is_err());
        assert!(ensure_bind_allowed("[::]:3000".parse().unwrap(), false).is_err());
        assert!(ensure_bind_allowed("0.0.0.0:3000".parse().unwrap(), true).is_ok());
    }

    #[test]
    fn parses_repeated_browser_origin_allowlist_entries() {
        let arguments = Arguments::parse([
            "--allow-origin".into(),
            "https://app.daku.test".into(),
            "--allow-origin".into(),
            "http://localhost:3000".into(),
        ])
        .unwrap();

        assert_eq!(
            arguments.allowed_origins,
            ["https://app.daku.test", "http://localhost:3000"]
        );
        assert!(!arguments.allow_non_loopback);
    }

    #[test]
    fn parses_explicit_non_loopback_opt_in() {
        let arguments = Arguments::parse(["--allow-non-loopback".into()]).unwrap();
        assert!(arguments.allow_non_loopback);
    }

    #[test]
    fn empty_daemon_token_is_refused() {
        assert!(require_token(Ok(String::new())).is_err());
        assert!(require_token(Ok("   ".into())).is_err());
        assert!(require_token(Err(std::env::VarError::NotPresent)).is_err());
        assert_eq!(require_token(Ok("secret".into())).unwrap(), "secret");
    }

    #[test]
    fn parses_probe_availability() {
        let arguments = Arguments::parse(["probe-availability".into()]).unwrap();
        assert!(arguments.probe_availability);
    }

    #[test]
    fn parses_doctor() {
        let arguments = Arguments::parse(["doctor".into()]).unwrap();
        assert!(arguments.doctor);
    }

    fn doctor_row(credential_present: bool) -> daku_core::DoctorRow {
        daku_core::DoctorRow {
            id: "prod".into(),
            label: "Production".into(),
            credential_present,
            credential_error: None,
            reachability: if credential_present {
                "asleep"
            } else {
                "unreachable"
            },
            state: "healthy",
            build: None,
            error: None,
            rtt_ms: 12,
        }
    }

    #[test]
    fn format_doctor_row_never_prints_secrets_and_flags_missing_credential() {
        let missing = format_doctor_row(&doctor_row(false));
        assert!(missing.contains("MISSING"), "{missing}");
        assert!(!missing.contains("client_secret") && !missing.contains("password"));
        assert!(format_doctor_row(&doctor_row(true)).contains("credential: present"));
    }

    #[test]
    fn doctor_exits_non_zero_only_for_missing_credentials_or_unreachable() {
        assert_eq!(doctor_exit_code(&[doctor_row(true)]), 0);
        assert_eq!(doctor_exit_code(&[doctor_row(true), doctor_row(false)]), 1);
        let unreachable = daku_core::DoctorRow {
            reachability: "unreachable",
            ..doctor_row(true)
        };
        assert_eq!(doctor_exit_code(&[unreachable]), 1);
    }
}
