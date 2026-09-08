//! Operator-managed Environments (080): validate + persist
//! `environments.json` entries and their Keychain Credentials over the
//! authenticated loopback wire.
//!
//! Trust rules (ADR-0004 amendment):
//! - Credential writes are refused unless the daemon bound loopback
//!   (`allow_credential_write`); a `--allow-non-loopback` daemon never holds
//!   a secret it was handed over the network.
//! - Secrets travel process memory only — never argv, env, logs, SQLite, or
//!   `environments.json`. Every error below is written to avoid interpolating
//!   the blob (pinned by test).

use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::sync::Arc;

#[cfg(unix)]
use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};

use anyhow::{Context, anyhow, bail};

use daku_protocol::{Command, EnvironmentConfig, ResponsePayload, validate_credential};

use crate::Backend;
use crate::availability::AvailabilitySignal;
use crate::config::CredentialStore;
use crate::servicenow::ServiceNowClient;

pub struct EnvironmentsBackend {
    environments_path: PathBuf,
    credentials: Arc<dyn CredentialStore>,
    client: Arc<ServiceNowClient>,
    allow_credential_write: bool,
}

impl EnvironmentsBackend {
    pub fn new(
        environments_path: PathBuf,
        credentials: Arc<dyn CredentialStore>,
        client: Arc<ServiceNowClient>,
        allow_credential_write: bool,
    ) -> Self {
        Self {
            environments_path,
            credentials,
            client,
            allow_credential_write,
        }
    }
}

impl Backend for EnvironmentsBackend {
    fn handle(&self, command: Command) -> anyhow::Result<ResponsePayload> {
        match command {
            Command::SaveEnvironment {
                environment,
                credential_json,
            } => {
                save_environment(
                    &self.environments_path,
                    &environment,
                    credential_json.as_deref(),
                    self.credentials.as_ref(),
                    self.allow_credential_write,
                )?;
                Ok(ResponsePayload::Ack)
            }
            Command::DeleteEnvironment { id } => {
                delete_environment(
                    &self.environments_path,
                    &id,
                    self.credentials.as_ref(),
                    self.allow_credential_write,
                )?;
                Ok(ResponsePayload::Ack)
            }
            Command::TestEnvironment {
                environment,
                credential_json,
            } => {
                let observation = test_environment(
                    &environment,
                    credential_json.as_deref(),
                    self.credentials.as_ref(),
                    &self.client,
                )?;
                Ok(ResponsePayload::EnvironmentTest {
                    reachability: observation.reachability,
                    state: observation.state,
                    build: observation.build,
                    error: observation.error,
                    rtt_ms: observation.rtt_ms,
                })
            }
            Command::Ping | Command::GetSettings | Command::UpdateSettings { .. } => {
                bail!("settings commands are served by SettingsBackend")
            }
            Command::GetDigest { .. } => {
                bail!("digests are served by the digest handler")
            }
            Command::AddHealthEventNote { .. } => {
                bail!("notes are served by the digest handler")
            }
        }
    }
}

fn validate_environment(environment: &EnvironmentConfig) -> anyhow::Result<()> {
    if environment.id.trim().is_empty() {
        bail!("environment id must not be empty");
    }
    if environment.label.trim().is_empty() {
        bail!("environment label must not be empty");
    }
    if let Some(reason) = daku_protocol::instance_url_error(&environment.instance_url) {
        bail!("environment {}: {reason}", environment.id);
    }
    Ok(())
}

/// Atomic `environments.json` write: pretty JSON via tmp + rename, `0600` on
/// unix. Entries sort by `sort_order`, like the loader expects.
pub fn save_environments(path: &Path, environments: &[EnvironmentConfig]) -> anyhow::Result<()> {
    if let Some(parent) = path.parent().filter(|p| !p.as_os_str().is_empty()) {
        fs::create_dir_all(parent).with_context(|| format!("creating {}", parent.display()))?;
    }
    let mut sorted = environments.to_vec();
    sorted.sort_by_key(|environment| environment.sort_order);
    let data = serde_json::to_vec_pretty(&sorted).context("encoding environments.json")?;
    let temporary = path.with_extension("json.tmp");
    {
        let mut options = fs::OpenOptions::new();
        options.write(true).create(true).truncate(true);
        #[cfg(unix)]
        options.mode(0o600);
        io::Write::write_all(
            &mut options
                .open(&temporary)
                .with_context(|| format!("writing {}", temporary.display()))?,
            &data,
        )
        .with_context(|| format!("writing {}", temporary.display()))?;
    }
    fs::rename(&temporary, path).with_context(|| format!("replacing {}", path.display()))?;
    #[cfg(unix)]
    fs::set_permissions(path, fs::Permissions::from_mode(0o600))
        .with_context(|| format!("securing {}", path.display()))?;
    Ok(())
}

fn load_or_empty(path: &Path) -> anyhow::Result<Vec<EnvironmentConfig>> {
    match fs::read(path) {
        Ok(bytes) => {
            serde_json::from_slice(&bytes).with_context(|| format!("parsing {}", path.display()))
        }
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(Vec::new()),
        Err(error) => Err(anyhow!("reading {}: {error}", path.display())),
    }
}

/// Validates and upserts one Environment, optionally rotating its Credential.
/// Config first, Credential second: a config write failure changes nothing,
/// and a Credential failure afterwards surfaces with the config already
/// saved for retry. Refuses Credential writes on non-loopback daemons before
/// touching anything.
pub fn save_environment(
    path: &Path,
    environment: &EnvironmentConfig,
    credential_json: Option<&str>,
    credentials: &dyn CredentialStore,
    allow_credential_write: bool,
) -> anyhow::Result<()> {
    validate_environment(environment)?;
    if let Some(blob) = credential_json {
        if !allow_credential_write {
            bail!("refusing credential write on a non-loopback daemon");
        }
        validate_credential(environment.auth_method, blob).map_err(|error| anyhow!("{error}"))?;
    }
    let mut environments = load_or_empty(path)?;
    match environments
        .iter_mut()
        .find(|item| item.id == environment.id)
    {
        Some(item) => *item = environment.clone(),
        None => environments.push(environment.clone()),
    }
    save_environments(path, &environments)?;
    if let Some(blob) = credential_json {
        credentials
            .set(&environment.id, blob)
            .with_context(|| format!("storing credential for {}", environment.id))?;
    }
    Ok(())
}

/// Removes one Environment from the file and deletes its Keychain item
/// (already-missing items are fine). Config first: a config failure changes
/// nothing; a Keychain failure afterwards leaves an orphan item for an id
/// that no longer exists, which a later save recreates cleanly.
pub fn delete_environment(
    path: &Path,
    id: &str,
    credentials: &dyn CredentialStore,
    allow_credential_write: bool,
) -> anyhow::Result<()> {
    if !allow_credential_write {
        bail!("refusing credential delete on a non-loopback daemon");
    }
    let mut environments = load_or_empty(path)?;
    let before = environments.len();
    environments.retain(|item| item.id != id);
    if environments.len() == before {
        bail!("unknown environment {id}");
    }
    save_environments(path, &environments)?;
    credentials
        .delete(id)
        .with_context(|| format!("deleting credential for {id}"))?;
    Ok(())
}

/// Dry-run availability probe for unsaved edits. Writes nothing: an explicit
/// blob is held in an ephemeral store, otherwise the stored item is used.
/// Non-ServiceNow Environments probe their own platform signal and map the
/// outcome onto the availability shape the sheet renders.
pub fn test_environment(
    environment: &EnvironmentConfig,
    credential_json: Option<&str>,
    credentials: &dyn CredentialStore,
    client: &ServiceNowClient,
) -> anyhow::Result<crate::availability::AvailabilityObservation> {
    use crate::config::MemoryCredentialStore;

    validate_environment(environment)?;
    let ephemeral;
    let store: &dyn CredentialStore = match credential_json {
        Some(blob) => {
            validate_credential(environment.auth_method, blob)
                .map_err(|error| anyhow!("{error}"))?;
            ephemeral = MemoryCredentialStore::default();
            ephemeral.insert(&environment.id, blob);
            &ephemeral
        }
        None => credentials,
    };
    match environment.platform {
        daku_protocol::Platform::Servicenow => {
            Ok(AvailabilitySignal.observe(client, store, environment))
        }
        daku_protocol::Platform::Http => Ok(crate::availability::observe_probe_signal(
            &crate::http_probe::HttpProbeSignal,
            client,
            store,
            environment,
        )),
        daku_protocol::Platform::Github => Ok(crate::availability::observe_probe_signal(
            &crate::github::ActionsSignal,
            client,
            store,
            environment,
        )),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::{MemoryCredentialStore, Platform};
    use crate::test_support::TempFile;
    use daku_protocol::AuthMethod;

    const OAUTH: &str = r#"{"client_id":"id","client_secret":"secret"}"#;
    const BASIC: &str = r#"{"username":"reader","password":"secret"}"#;

    fn env(id: &str) -> EnvironmentConfig {
        EnvironmentConfig {
            id: id.into(),
            label: id.into(),
            instance_url: format!("https://{id}.example.service-now.com"),
            auth_method: AuthMethod::Basic,
            sort_order: 0,
            clone_source: false,
            platform: Platform::Servicenow,
            thresholds: daku_protocol::Thresholds::default(),
            expected_drift: Vec::new(),
        }
    }

    fn read(path: &Path) -> Vec<EnvironmentConfig> {
        serde_json::from_slice(&fs::read(path).unwrap()).unwrap()
    }

    #[test]
    fn save_creates_missing_file_with_0600() {
        let file = TempFile::new("env-save");
        let credentials = MemoryCredentialStore::default();
        save_environment(file.path(), &env("dev"), Some(BASIC), &credentials, true).unwrap();
        assert_eq!(read(file.path()).len(), 1);
        assert_eq!(credentials.get("dev").unwrap().as_deref(), Some(BASIC));
        #[cfg(unix)]
        assert_eq!(
            fs::metadata(file.path()).unwrap().permissions().mode() & 0o777,
            0o600
        );
    }

    #[test]
    fn save_without_credential_leaves_stored_item_alone() {
        let file = TempFile::new("env-nocred");
        let credentials = MemoryCredentialStore::default();
        credentials.insert("dev", "old");
        let mut edited = env("dev");
        edited.label = "Dev 2".into();
        save_environment(file.path(), &edited, None, &credentials, true).unwrap();
        assert_eq!(read(file.path())[0].label, "Dev 2");
        assert_eq!(credentials.get("dev").unwrap().as_deref(), Some("old"));
    }

    #[test]
    fn save_upserts_by_id_and_sorts() {
        let file = TempFile::new("env-upsert");
        let credentials = MemoryCredentialStore::default();
        let mut second = env("second");
        second.sort_order = 2;
        let mut first = env("first");
        first.sort_order = 1;
        save_environment(file.path(), &second, None, &credentials, true).unwrap();
        save_environment(file.path(), &first, None, &credentials, true).unwrap();
        let mut renamed = first.clone();
        renamed.label = "First!".into();
        save_environment(file.path(), &renamed, None, &credentials, true).unwrap();
        let environments = read(file.path());
        assert_eq!(
            environments
                .iter()
                .map(|item| item.id.as_str())
                .collect::<Vec<_>>(),
            ["first", "second"]
        );
        assert_eq!(environments[0].label, "First!");
    }

    #[test]
    fn save_rejects_bad_url_shape_mismatch_and_locked_daemon() {
        let file = TempFile::new("env-reject");
        let credentials = MemoryCredentialStore::default();
        let mut bad = env("dev");
        bad.instance_url = "http://x.example.com".into();
        let error = save_environment(file.path(), &bad, None, &credentials, true)
            .unwrap_err()
            .to_string();
        assert!(error.contains("https://"), "{error}");
        assert!(!file.path().exists(), "failed validation writes nothing");

        let error = save_environment(file.path(), &env("dev"), Some(BASIC), &credentials, false)
            .unwrap_err()
            .to_string();
        assert!(error.contains("non-loopback"), "{error}");
        assert!(!file.path().exists());

        let error = save_environment(file.path(), &env("dev"), Some(OAUTH), &credentials, true)
            .unwrap_err()
            .to_string();
        assert!(
            error.contains("username and password"),
            "basic shape, oauth blob: {error}"
        );
        assert!(!error.contains("secret"), "errors never echo the blob");
    }

    #[test]
    fn delete_removes_config_and_credential() {
        let file = TempFile::new("env-delete");
        let credentials = MemoryCredentialStore::default();
        save_environment(file.path(), &env("dev"), Some(BASIC), &credentials, true).unwrap();
        save_environment(file.path(), &env("ops"), None, &credentials, true).unwrap();
        delete_environment(file.path(), "dev", &credentials, true).unwrap();
        assert_eq!(read(file.path()).len(), 1);
        assert!(credentials.get("dev").unwrap().is_none());
        let error = delete_environment(file.path(), "dev", &credentials, true)
            .unwrap_err()
            .to_string();
        assert!(error.contains("unknown environment"), "{error}");
        let error = delete_environment(file.path(), "ops", &credentials, false)
            .unwrap_err()
            .to_string();
        assert!(error.contains("non-loopback"), "{error}");
    }

    #[test]
    fn test_environment_dry_runs_without_writing() {
        use crate::servicenow::{
            HttpRequest, HttpResponse, HttpTransport, ServiceNowClient, SystemClock,
        };

        struct OkTransport;
        impl HttpTransport for OkTransport {
            fn execute(&self, request: &HttpRequest) -> anyhow::Result<HttpResponse> {
                assert!(request.url.contains("glide.war"), "{}", request.url);
                Ok(HttpResponse {
                    status: 200,
                    headers: vec![("content-type".into(), "application/json".into())],
                    body: r#"{"result":[{"value":"glide-1"}]}"#.into(),
                })
            }
        }

        let file = TempFile::new("env-test");
        let credentials = MemoryCredentialStore::default();
        let client = ServiceNowClient::new(OkTransport, SystemClock);
        // Unsaved edits probe with an ephemeral blob; nothing is stored.
        let observation =
            test_environment(&env("dev"), Some(BASIC), &credentials, &client).unwrap();
        assert_eq!(
            observation.reachability,
            daku_protocol::Reachability::Reachable
        );
        assert_eq!(observation.build.as_deref(), Some("glide-1"));
        assert!(credentials.get("dev").unwrap().is_none());
        assert!(!file.path().exists());
        // A shape mismatch fails before any HTTP.
        struct NoProbeTransport;
        impl HttpTransport for NoProbeTransport {
            fn execute(&self, _request: &HttpRequest) -> anyhow::Result<HttpResponse> {
                panic!("invalid credential must fail before probing");
            }
        }
        let error = test_environment(
            &env("dev"),
            Some(OAUTH),
            &credentials,
            &ServiceNowClient::new(NoProbeTransport, SystemClock),
        )
        .unwrap_err()
        .to_string();
        assert!(error.contains("username and password"), "{error}");
    }

    #[test]
    fn test_environment_probes_each_platform_its_own_way() {
        use crate::config::Platform;
        use crate::servicenow::{HttpRequest, HttpResponse, HttpTransport, SystemClock};

        struct ProbeTransport;
        impl HttpTransport for ProbeTransport {
            fn execute(&self, request: &HttpRequest) -> anyhow::Result<HttpResponse> {
                if request.url.contains("status.example.com") {
                    return Ok(HttpResponse {
                        status: 200,
                        headers: Vec::new(),
                        body: String::new(),
                    });
                }
                assert!(
                    request
                        .url
                        .starts_with("https://api.github.com/repos/acme/app/"),
                    "github test probes the runs API: {}",
                    request.url
                );
                Ok(HttpResponse {
                    status: 200,
                    headers: vec![("content-type".into(), "application/json".into())],
                    body: r#"{"workflow_runs":[]}"#.into(),
                })
            }
        }

        fn platform_env(id: &str, url: &str, platform: Platform) -> EnvironmentConfig {
            EnvironmentConfig {
                id: id.into(),
                label: id.into(),
                instance_url: url.into(),
                auth_method: AuthMethod::Basic,
                sort_order: 0,
                clone_source: false,
                platform,
                thresholds: daku_protocol::Thresholds::default(),
                expected_drift: Vec::new(),
            }
        }

        let credentials = MemoryCredentialStore::default();
        let client = ServiceNowClient::new(ProbeTransport, SystemClock);
        let http = test_environment(
            &platform_env("status", "https://status.example.com/", Platform::Http),
            None,
            &credentials,
            &client,
        )
        .unwrap();
        assert_eq!(http.reachability, daku_protocol::Reachability::Reachable);
        assert_eq!(http.state, daku_protocol::SignalState::Healthy);
        assert!(http.build.is_none());

        let github = test_environment(
            &platform_env("repo", "https://github.com/acme/app", Platform::Github),
            Some(r#"{"username":"token","password":"test-pat-x"}"#),
            &credentials,
            &client,
        )
        .unwrap();
        assert_eq!(github.reachability, daku_protocol::Reachability::Reachable);
        assert_eq!(github.state, daku_protocol::SignalState::Healthy);
    }

    #[test]
    fn validate_credential_never_echoes_the_blob() {
        // Values are unique per case so key names ("client_secret") never
        // collide with the assertion; only values must stay out of errors.
        for (method, blob, secret) in [
            (
                AuthMethod::OauthClientCredentials,
                r#"{"username":"reader-a","password":"hunter2-a"}"#,
                "hunter2-a",
            ),
            (
                AuthMethod::Basic,
                r#"{"client_id":"id-b","client_secret":"s3cr3t-b"}"#,
                "s3cr3t-b",
            ),
            (AuthMethod::Basic, "{not json", "not json"),
            (
                AuthMethod::Basic,
                r#"{"username":"","password":"x"}"#,
                "irrelevant",
            ),
        ] {
            let error = validate_credential(method, blob).unwrap_err();
            assert!(!error.contains(secret), "{error}");
        }
        assert!(validate_credential(AuthMethod::Basic, BASIC).is_ok());
        assert!(validate_credential(AuthMethod::OauthClientCredentials, OAUTH).is_ok());
    }
}
