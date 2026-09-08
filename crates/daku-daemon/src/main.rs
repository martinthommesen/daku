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
        return run_probe_availability(&arguments);
    }
    if arguments.doctor {
        return run_doctor_command(arguments.doctor_fix, &arguments);
    }
    if let Some(env_id) = arguments.digest_env.clone() {
        return run_digest_command(&env_id, arguments.digest_days);
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

    let store = daku_core::persistence::StateStore::daemon(
        daku_core::persistence::StateStore::default_path(),
    );
    // Migrate once at startup so an unwritable database fails fast.
    store
        .open()
        .with_context(|| format!("could not open {}", store.path().display()))?;
    let settings =
        daku_core::DaemonSettingsStore::open(daku_core::DaemonSettingsStore::default_path())
            .context("could not load daemon settings")?;
    let dashboard_events = daku_core::start_default_loop_with_store(
        &daku_core::default_environments_path(),
        store.clone(),
        &settings.get(),
        shutdown.clone(),
        resolve_credential_store(&arguments),
    );
    daku_core::serve(
        listener,
        auth,
        Arc::new(CombinedBackend {
            settings: daku_core::SettingsBackend::new(settings),
            environments: daku_core::environments::EnvironmentsBackend::new(
                daku_core::default_environments_path(),
                resolve_credential_store(&arguments),
                Arc::new(daku_core::servicenow::ServiceNowClient::new(
                    daku_core::servicenow::UreqTransport::default(),
                    daku_core::servicenow::SystemClock,
                )),
                // Credential writes arrive over the wire: only a loopback
                // daemon may perform them (ADR-0004 amendment).
                !arguments.allow_non_loopback,
            ),
            digest_store: store,
        }),
        shutdown,
        daku_core::ServerOptions {
            allowed_origins: arguments.allowed_origins.into_iter().collect(),
            allow_shutdown: arguments.parent_pid.is_some(),
        },
        dashboard_events,
    )
}

/// One wire backend: settings commands go to `SettingsBackend`,
/// Environment management and digests to their own handlers.
struct CombinedBackend {
    settings: daku_core::SettingsBackend,
    environments: daku_core::environments::EnvironmentsBackend,
    digest_store: daku_core::persistence::StateStore,
}

impl daku_core::Backend for CombinedBackend {
    fn handle(
        &self,
        command: daku_protocol::Command,
    ) -> anyhow::Result<daku_protocol::ResponsePayload> {
        use daku_protocol::Command;
        match command {
            Command::Ping | Command::GetSettings | Command::UpdateSettings { .. } => {
                self.settings.handle(command)
            }
            Command::SaveEnvironment { .. }
            | Command::DeleteEnvironment { .. }
            | Command::TestEnvironment { .. } => self.environments.handle(command),
            Command::GetDigest {
                environment_id,
                days,
            } => Ok(daku_protocol::ResponsePayload::Digest {
                markdown: daku_core::digest::handle_get_digest(
                    &daku_core::default_environments_path(),
                    &self.digest_store,
                    &environment_id,
                    days,
                )?,
            }),
        }
    }
}

fn run_probe_availability(arguments: &Arguments) -> anyhow::Result<()> {
    let store = daku_core::persistence::StateStore::daemon(
        daku_core::persistence::StateStore::default_path(),
    );
    daku_core::probe_availability_once_with_store(
        &daku_core::default_environments_path(),
        store,
        resolve_credential_store(arguments),
    )?;
    println!("availability probe complete");
    Ok(())
}

/// Prints a Markdown digest of one Environment's local history: health
/// transitions, builds, and current Signal states over the last `days`.
/// Read-only over SQLite; needs no token and writes nothing.
fn run_digest_command(environment_id: &str, days: i64) -> anyhow::Result<()> {
    let environments =
        daku_core::config::load_environments(&daku_core::default_environments_path())
            .with_context(|| "could not load environments.json")?;
    let environment = environments
        .iter()
        .find(|environment| environment.id == environment_id)
        .ok_or_else(|| anyhow!("unknown environment {environment_id}"))?;
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|duration| duration.as_secs() as i64)
        .unwrap_or(0);
    print!(
        "{}",
        daku_core::digest::weekly_digest(
            &daku_core::persistence::StateStore::daemon(
                daku_core::persistence::StateStore::default_path(),
            ),
            environment,
            now,
            days,
        )?
    );
    Ok(())
}

fn resolve_credential_store(arguments: &Arguments) -> Arc<dyn daku_core::config::CredentialStore> {
    let wants_file = arguments
        .credential_store
        .as_deref()
        .is_some_and(|value| value == "file")
        || (arguments.credential_store.is_none()
            && matches!(
                std::env::var(daku_core::CREDENTIAL_STORE_ENV).as_deref(),
                Ok("file")
            ));
    if wants_file {
        let path = arguments
            .credential_file
            .clone()
            .unwrap_or_else(daku_core::default_credential_file_path);
        Arc::new(daku_core::FileCredentialStore::new(path))
    } else {
        Arc::new(daku_core::config::KeychainCredentialStore)
    }
}

fn run_doctor_command(fix: bool, arguments: &Arguments) -> anyhow::Result<()> {
    let environments_path = daku_core::default_environments_path();
    if fix {
        for line in daku_core::config::repair_environments_setup(&environments_path)? {
            println!("fix: {line}");
        }
    }
    let settings =
        daku_core::DaemonSettingsStore::open(daku_core::DaemonSettingsStore::default_path())
            .context("could not load daemon settings")?
            .get();
    let environments_path = daku_core::default_environments_path();
    let report = daku_core::run_doctor(
        &environments_path,
        &settings,
        resolve_credential_store(arguments),
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
    let db_path = daku_core::persistence::StateStore::default_path();
    println!(
        "{}",
        daku_core::persistence::format_db_stats(
            &db_path,
            &daku_core::persistence::db_stats(&db_path)
        )
    );
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
        (true, None) => "credential: present".to_owned(),
        (true, Some(error)) => format!("credential: present ({error})"),
        (false, None) => "credential: MISSING (Keychain service daku, account = id)".to_owned(),
        (false, Some(error)) => format!("credential: ERROR {error}"),
    };
    format!(
        "{} ({}) [{}] · {} · {} {} · build {} · {} ms{} · thresholds {}{}",
        row.id,
        row.label,
        row.platform,
        credential,
        row.reachability,
        row.state,
        row.build.as_deref().unwrap_or("—"),
        row.rtt_ms,
        row.error
            .as_deref()
            .map(|error| format!(" · {error}"))
            .unwrap_or_default(),
        row.thresholds,
        if row.expected_drift > 0 {
            format!(" · expected drift {}", row.expected_drift)
        } else {
            String::new()
        },
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
    doctor_fix: bool,
    credential_store: Option<String>,
    credential_file: Option<std::path::PathBuf>,
    digest_env: Option<String>,
    digest_days: i64,
}

impl Arguments {
    fn parse(arguments: impl IntoIterator<Item = String>) -> anyhow::Result<Self> {
        let mut bind = "127.0.0.1:0".to_owned();
        let mut parent_pid = None;
        let mut allowed_origins = Vec::new();
        let mut allow_non_loopback = false;
        let mut probe_availability = false;
        let mut doctor = false;
        let mut doctor_fix = false;
        let mut credential_store = None;
        let mut credential_file = None;
        let mut digest_env = None;
        let mut digest_days = 7;
        let mut arguments = arguments.into_iter();
        while let Some(argument) = arguments.next() {
            match argument.as_str() {
                "probe-availability" => {
                    probe_availability = true;
                }
                "doctor" => {
                    doctor = true;
                }
                "digest" => {
                    digest_env = Some(String::new());
                }
                "--env" => {
                    let id = arguments
                        .next()
                        .ok_or_else(|| anyhow!("--env requires an environment id"))?;
                    if digest_env.is_none() {
                        bail!("--env requires the digest command");
                    }
                    digest_env = Some(id);
                }
                "--days" => {
                    digest_days = arguments
                        .next()
                        .ok_or_else(|| anyhow!("--days requires a number"))?
                        .parse()
                        .context("--days is not a whole number")?;
                }
                "--fix" => {
                    doctor_fix = true;
                }
                "--bind" => {
                    bind = arguments
                        .next()
                        .ok_or_else(|| anyhow!("--bind requires an address"))?;
                }
                "--credential-store" => {
                    let value = arguments
                        .next()
                        .ok_or_else(|| anyhow!("--credential-store requires keychain or file"))?;
                    if value != "keychain" && value != "file" {
                        bail!("--credential-store must be keychain or file, got {value:?}");
                    }
                    credential_store = Some(value);
                }
                "--credential-file" => {
                    let value = arguments
                        .next()
                        .ok_or_else(|| anyhow!("--credential-file requires a path"))?;
                    credential_file = Some(std::path::PathBuf::from(value));
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
                        "usage: {} [probe-availability] [doctor [--fix]] [digest --env ID [--days N]] [--bind ADDRESS] [--allow-non-loopback] [--parent-pid PID] [--allow-origin ORIGIN]... [--credential-store keychain|file] [--credential-file PATH]",
                        env!("CARGO_BIN_NAME")
                    );
                    std::process::exit(0);
                }
                unknown => bail!("unknown argument {unknown:?}"),
            }
        }
        if doctor_fix && !doctor {
            bail!("--fix requires doctor");
        }
        if credential_file.is_some() && credential_store.as_deref() != Some("file") {
            bail!("--credential-file requires --credential-store file");
        }
        if digest_env.as_deref() == Some("") || digest_env.is_none() && digest_days != 7 {
            bail!("digest requires --env <id>");
        }
        if digest_days < 1 {
            bail!("--days must be at least 1");
        }
        Ok(Self {
            bind,
            parent_pid,
            allowed_origins,
            allow_non_loopback,
            probe_availability,
            doctor,
            doctor_fix,
            credential_store,
            credential_file,
            digest_env,
            digest_days,
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
        assert!(!arguments.doctor_fix);
    }

    #[test]
    fn parses_doctor_fix_and_rejects_bare_fix() {
        let arguments = Arguments::parse(["doctor".into(), "--fix".into()]).unwrap();
        assert!(arguments.doctor_fix);
        assert!(Arguments::parse(["--fix".into()]).is_err());
    }

    #[test]
    fn parses_digest_with_env_and_days() {
        let arguments = Arguments::parse([
            "digest".into(),
            "--env".into(),
            "prod".into(),
            "--days".into(),
            "3".into(),
        ])
        .unwrap();
        assert_eq!(arguments.digest_env.as_deref(), Some("prod"));
        assert_eq!(arguments.digest_days, 3);
        assert!(Arguments::parse(["digest".into()]).is_err());
        assert!(Arguments::parse(["--days".into(), "3".into()]).is_err());
        assert!(
            Arguments::parse([
                "digest".into(),
                "--env".into(),
                "prod".into(),
                "--days".into(),
                "0".into()
            ])
            .is_err()
        );
    }

    #[test]
    fn parses_credential_store_flags() {
        let arguments = Arguments::parse([
            "--credential-store".into(),
            "file".into(),
            "--credential-file".into(),
            "/tmp/daku-creds.json".into(),
        ])
        .unwrap();
        assert_eq!(arguments.credential_store.as_deref(), Some("file"));
        assert_eq!(
            arguments.credential_file,
            Some(std::path::PathBuf::from("/tmp/daku-creds.json"))
        );
        assert!(Arguments::parse(["--credential-store".into(), "vault".into()]).is_err());
        assert!(
            Arguments::parse(["--credential-file".into(), "/tmp/x.json".into(),]).is_err(),
            "--credential-file without --credential-store file is rejected"
        );
        let keychain_with_file = Arguments::parse([
            "--credential-store".into(),
            "keychain".into(),
            "--credential-file".into(),
            "/tmp/x.json".into(),
        ]);
        assert!(keychain_with_file.is_err());
    }

    #[test]
    fn resolve_credential_store_prefers_explicit_flag_over_env() {
        let path = std::env::temp_dir().join(format!("daku-resolve-{}.json", std::process::id()));
        let _ = std::fs::remove_file(&path);
        let file = Arguments::parse([
            "--credential-store".into(),
            "file".into(),
            "--credential-file".into(),
            path.to_string_lossy().into_owned(),
        ])
        .unwrap();
        let store = resolve_credential_store(&file);
        store
            .set("prod", r#"{"username":"u","password":"p"}"#)
            .unwrap();
        assert_eq!(
            store.get("prod").unwrap().as_deref(),
            Some(r#"{"username":"u","password":"p"}"#)
        );
        assert!(path.exists(), "file store writes to the flagged path");
        let _ = std::fs::remove_file(&path);
    }

    fn doctor_row(credential_present: bool) -> daku_core::DoctorRow {
        daku_core::DoctorRow {
            id: "prod".into(),
            label: "Production".into(),
            platform: "servicenow".into(),
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
            thresholds: daku_core::config::Thresholds::default().summary(),
            expected_drift: 0,
        }
    }

    #[test]
    fn format_doctor_row_never_prints_secrets_and_flags_missing_credential() {
        let missing = format_doctor_row(&doctor_row(false));
        assert!(missing.contains("MISSING"), "{missing}");
        assert!(!missing.contains("client_secret") && !missing.contains("password"));
        assert!(format_doctor_row(&doctor_row(true)).contains("credential: present"));
        assert!(
            format_doctor_row(&doctor_row(true)).contains("thresholds jobs"),
            "doctor prints effective thresholds"
        );
    }

    #[test]
    fn format_doctor_row_shows_shape_problems_without_secrets() {
        let row = daku_core::DoctorRow {
            credential_error: Some(
                "shape: credential does not match its auth method (needs username and password)"
                    .into(),
            ),
            ..doctor_row(true)
        };
        let line = format_doctor_row(&row);
        assert!(line.contains("credential: present (shape:"), "{line}");
        assert!(!line.contains("client_secret") && !line.contains("password="));
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
