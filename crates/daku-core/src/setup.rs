//! Non-interactive Environment onboarding: `daku-daemon setup`.
//!
//! The GUI sheet (`106`) and this command share one code path
//! (`save_environment` + `test_environment`): validation rejects before
//! anything is written, the Credential travels memory-only, and the file
//! write is atomic. Differences from the sheet are deliberate:
//!
//! * The probe runs by default and a failed probe aborts the save —
//!   scripted onboarding should fail fast on typos. `--no-probe` skips it
//!   for known-asleep Environments.
//! * Nothing is printed but one summary line (plus a stderr warning when
//!   `--no-probe` skips the check). Diagnostics belong to `doctor`.

use std::path::Path;

use crate::config::{AuthMethod, CredentialStore, EnvironmentConfig, Platform, Thresholds};
use crate::environments::{save_environment, test_environment};
use crate::servicenow::ServiceNowClient;

pub struct SetupArgs {
    pub id: String,
    pub label: String,
    pub instance_url: String,
    pub platform: Platform,
    pub auth_method: AuthMethod,
    pub clone_source: bool,
    /// Exact Credential blob, if any. The CLI reads this from a file
    /// (`--credential-file`) — never argv — so the secret avoids `ps` and
    /// shell history.
    pub credential_json: Option<String>,
    /// Dry-run the probe and abort the save when it fails.
    pub probe: bool,
}

/// Validates, optionally probes, and saves one Environment. An unreachable
/// probe aborts before writing (typo'd URL or Credential); anything
/// answered — reachable, asleep, even down — saves. Returns the one-line
/// summary the CLI prints.
pub fn run_setup(
    environments_path: &Path,
    credentials: &dyn CredentialStore,
    client: &ServiceNowClient,
    args: SetupArgs,
) -> anyhow::Result<String> {
    let sort_order = match existing_sort_order(environments_path, &args.id) {
        // Ids are immutable: re-running setup keeps the stored order (sheet
        // parity); only new ids append past the maximum.
        Some(order) => order,
        None => next_sort_order(environments_path),
    };
    let environment = EnvironmentConfig {
        id: args.id.clone(),
        label: if args.label.trim().is_empty() {
            args.id.clone()
        } else {
            args.label.clone()
        },
        instance_url: args.instance_url.clone(),
        auth_method: args.auth_method,
        sort_order,
        clone_source: args.clone_source,
        platform: args.platform,
        thresholds: Thresholds::default(),
        expected_drift: Vec::new(),
    };
    let probe_line = if args.probe {
        let observation = test_environment(
            &environment,
            args.credential_json.as_deref(),
            credentials,
            client,
        )?;
        // Unreachable aborts the save (typo'd URL or Credential); anything
        // answered — even down, asleep, or degraded — is a valid config.
        if observation.reachability == daku_protocol::Reachability::Unreachable {
            anyhow::bail!(
                "probe of {} is unreachable ({}); fix the URL/Credential or pass --no-probe",
                environment.id,
                observation.error.as_deref().unwrap_or("no route")
            );
        }
        format!(
            " · probe {} {}",
            observation.reachability.as_str(),
            observation.state.as_str()
        )
    } else {
        String::new()
    };
    save_environment(
        environments_path,
        &environment,
        args.credential_json.as_deref(),
        credentials,
        true,
    )?;
    Ok(format!(
        "saved {} ({}) [{}{}] · credential {}{probe_line}",
        environment.id,
        environment.label,
        environment.platform.id(),
        if environment.clone_source {
            ", clone source"
        } else {
            ""
        },
        if args.credential_json.is_some() {
            "stored"
        } else {
            "untouched"
        },
    ))
}

/// Stored sort order for a known id, if any.
fn existing_sort_order(environments_path: &Path, id: &str) -> Option<i64> {
    crate::config::load_environments(environments_path)
        .ok()?
        .iter()
        .find(|environment| environment.id == id)
        .map(|environment| environment.sort_order)
}

/// One past the current maximum sort order (0 for a fresh file), so setup
/// appends without reordering what the Operator arranged.
fn next_sort_order(environments_path: &Path) -> i64 {
    crate::config::load_environments(environments_path)
        .map(|environments| {
            environments
                .iter()
                .map(|environment| environment.sort_order)
                .max()
                .unwrap_or(-1)
                + 1
        })
        .unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::MemoryCredentialStore;
    use crate::servicenow::{HttpRequest, HttpResponse, HttpTransport, SystemClock};
    use crate::test_support::TempFile;
    use std::sync::Arc;

    const BASIC: &str = r#"{"username":"reader","password":"secret"}"#;

    struct OkTransport;

    impl HttpTransport for OkTransport {
        fn execute(&self, request: &HttpRequest) -> anyhow::Result<HttpResponse> {
            if request.url.contains("sys_properties") {
                return Ok(HttpResponse {
                    status: 200,
                    headers: vec![("content-type".into(), "application/json".into())],
                    body: r#"{"result":[{"value":"glide-test"}]}"#.into(),
                });
            }
            if request.url.contains("api.github.com") {
                return Ok(HttpResponse {
                    status: 200,
                    headers: vec![("content-type".into(), "application/json".into())],
                    body: r#"{"workflow_runs":[]}"#.into(),
                });
            }
            if request.url.contains("sys_update_set")
                || request.url.contains("sys_flow_context")
                || request.url.contains("sys_email")
                || request.url.contains("sys_upgrade_history")
                || request.url.contains("v_user_session")
                || request.url.contains("/api/now/stats/")
            {
                return Ok(HttpResponse {
                    status: 200,
                    headers: vec![("content-type".into(), "application/json".into())],
                    body: if request.url.contains("/api/now/stats/") {
                        r#"{"result":{"stats":{"count":"0"}}}"#.into()
                    } else if request.url.contains("syslog_transaction") {
                        r#"{"result":{"stats":{"avg":{"response_time":"10"}}}}"#.into()
                    } else {
                        r#"{"result":[]}"#.into()
                    },
                });
            }
            Ok(HttpResponse {
                status: 200,
                headers: vec![("content-type".into(), "application/json".into())],
                body: String::new(),
            })
        }
    }

    fn args(id: &str) -> SetupArgs {
        SetupArgs {
            id: id.into(),
            label: format!("{id} label"),
            instance_url: "https://acme-dev.example.service-now.com".into(),
            platform: Platform::Servicenow,
            auth_method: AuthMethod::Basic,
            clone_source: false,
            credential_json: Some(BASIC.into()),
            probe: true,
        }
    }

    fn setup(args: SetupArgs) -> (TempFile, Arc<MemoryCredentialStore>, anyhow::Result<String>) {
        let file = TempFile::new("setup");
        let credentials = Arc::new(MemoryCredentialStore::default());
        let client = ServiceNowClient::new(OkTransport, SystemClock);
        let result = run_setup(file.path(), credentials.as_ref(), &client, args);
        (file, credentials, result)
    }

    #[test]
    fn setup_validates_probes_saves_and_reports() {
        let (file, credentials, result) = setup(args("dev"));
        let line = result.unwrap();
        assert!(line.contains("saved dev (dev label)"), "{line}");
        assert!(line.contains("credential stored"), "{line}");
        assert!(line.contains("probe"), "{line}");
        let environments = crate::config::load_environments(file.path()).unwrap();
        assert_eq!(environments.len(), 1);
        assert_eq!(environments[0].platform, Platform::Servicenow);
        assert_eq!(credentials.get("dev").unwrap().as_deref(), Some(BASIC));
    }

    #[test]
    fn setup_rejects_bad_urls_and_mismatched_credentials_before_writing() {
        let mut bad_url = args("dev");
        bad_url.instance_url = "http://acme.example.com".into();
        let (file, _, result) = setup(bad_url);
        assert!(result.is_err());
        assert!(!file.path().exists(), "failed validation writes nothing");

        let mut bad_cred = args("dev");
        bad_cred.credential_json = Some(r#"{"client_id":"x"}"#.into());
        let (file, _, result) = setup(bad_cred);
        assert!(
            result
                .unwrap_err()
                .to_string()
                .contains("username and password")
        );
        assert!(!file.path().exists());
    }

    #[test]
    fn setup_no_probe_saves_when_the_environment_is_dark() {
        struct Offline;
        impl HttpTransport for Offline {
            fn execute(&self, _request: &HttpRequest) -> anyhow::Result<HttpResponse> {
                anyhow::bail!("offline")
            }
        }
        let file = TempFile::new("setup-dark");
        let credentials = MemoryCredentialStore::default();
        let client = ServiceNowClient::new(Offline, SystemClock);
        let mut dark = args("dev");
        dark.probe = false;
        let line = run_setup(file.path(), &credentials, &client, dark).unwrap();
        assert!(line.contains("saved dev"), "{line}");
        assert!(
            !line.contains("probe"),
            "skipped probes stay out of the summary"
        );
    }

    #[test]
    fn setup_probe_failure_aborts_before_writing() {
        struct Offline;
        impl HttpTransport for Offline {
            fn execute(&self, _request: &HttpRequest) -> anyhow::Result<HttpResponse> {
                anyhow::bail!("offline")
            }
        }
        let file = TempFile::new("setup-abort");
        let credentials = MemoryCredentialStore::default();
        let client = ServiceNowClient::new(Offline, SystemClock);
        let result = run_setup(file.path(), &credentials, &client, args("dev"));
        assert!(result.is_err());
        assert!(!file.path().exists());
    }

    #[test]
    fn setup_appends_sort_order_and_upserts_known_ids() {
        let file = TempFile::new("setup-order");
        let credentials = MemoryCredentialStore::default();
        let client = ServiceNowClient::new(OkTransport, SystemClock);
        run_setup(file.path(), &credentials, &client, args("a")).unwrap();
        run_setup(file.path(), &credentials, &client, args("b")).unwrap();
        let mut renamed = args("a");
        renamed.label = "A renamed".into();
        run_setup(file.path(), &credentials, &client, renamed).unwrap();
        let environments = crate::config::load_environments(file.path()).unwrap();
        assert_eq!(environments.len(), 2);
        assert_eq!(environments[0].id, "a");
        assert_eq!(environments[0].label, "A renamed");
        assert_eq!(environments[0].sort_order, 0);
        assert_eq!(environments[1].sort_order, 1);
    }
}
