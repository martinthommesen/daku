//! Operator Environment list and Credential lookup.

use std::collections::HashMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::Mutex;

use anyhow::{Context, anyhow};
use serde::Deserialize;

use daku_protocol::identity::DATA_DIRECTORY_NAME;

pub const KEYCHAIN_SERVICE: &str = "daku";

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AuthMethod {
    OauthClientCredentials,
    Basic,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub struct EnvironmentConfig {
    pub id: String,
    pub label: String,
    pub instance_url: String,
    pub auth_method: AuthMethod,
    pub sort_order: i64,
    #[serde(default)]
    pub clone_source: bool,
    /// Per-Environment threshold overrides; missing keys fall back to the
    /// defaults below. Unknown keys are rejected so typos fail fast.
    #[serde(default)]
    pub thresholds: Thresholds,
    /// Plugin/app ids (or store-app scopes — the same `id` the drift
    /// mismatch list carries) that are planned differences from the clone
    /// source. Only unexpected drift votes toward degraded.
    #[serde(default)]
    pub expected_drift: Vec<String>,
}

/// Degrade thresholds per Environment. Defaults preserve the historical
/// hard-coded behaviour exactly: one overdue job, one syslog error, one
/// outbound failure, any unhealthy MID or ECC error, ECC output-ready ≥ 100,
/// any plugin/build mismatch degrades. Availability RTT never degraded before
/// (`None` = disabled); jobs error count never voted before (`u64::MAX`).
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct Thresholds {
    pub jobs_overdue_degraded_at: u64,
    pub jobs_error_degraded_at: u64,
    pub syslog_error_degraded_at: u64,
    pub outbound_failures_degraded_at: u64,
    pub mid_unhealthy_degraded_at: u64,
    pub ecc_error_degraded_at: u64,
    pub ecc_output_ready_degraded_at: u64,
    pub drift_mismatches_degraded_at: u64,
    pub availability_rtt_degraded_ms: Option<u64>,
}

impl Default for Thresholds {
    fn default() -> Self {
        Self {
            jobs_overdue_degraded_at: 1,
            jobs_error_degraded_at: u64::MAX,
            syslog_error_degraded_at: 1,
            outbound_failures_degraded_at: 1,
            mid_unhealthy_degraded_at: 1,
            ecc_error_degraded_at: 1,
            ecc_output_ready_degraded_at: 100,
            drift_mismatches_degraded_at: 1,
            availability_rtt_degraded_ms: None,
        }
    }
}

impl Thresholds {
    /// One-line effective values for `daku-daemon doctor`.
    pub fn summary(&self) -> String {
        let off = |value: u64| {
            if value == u64::MAX {
                "off".to_owned()
            } else {
                value.to_string()
            }
        };
        let rtt = self
            .availability_rtt_degraded_ms
            .map(|ms| format!("{ms}ms"))
            .unwrap_or_else(|| "off".to_owned());
        format!(
            "jobs≥{}/err≥{} syslog≥{} outbound≥{} mid≥{}/ecc-err≥{}/queue≥{} drift≥{} rtt>{}",
            self.jobs_overdue_degraded_at,
            off(self.jobs_error_degraded_at),
            self.syslog_error_degraded_at,
            self.outbound_failures_degraded_at,
            self.mid_unhealthy_degraded_at,
            self.ecc_error_degraded_at,
            self.ecc_output_ready_degraded_at,
            self.drift_mismatches_degraded_at,
            rtt,
        )
    }
}

pub fn default_environments_path() -> PathBuf {
    dirs::home_dir()
        .unwrap_or_else(std::env::temp_dir)
        .join(format!(".{DATA_DIRECTORY_NAME}"))
        .join("environments.json")
}

pub fn load_environments(path: &Path) -> anyhow::Result<Vec<EnvironmentConfig>> {
    let bytes = fs::read(path).with_context(|| format!("reading {}", path.display()))?;
    let mut environments: Vec<EnvironmentConfig> =
        serde_json::from_slice(&bytes).with_context(|| format!("parsing {}", path.display()))?;
    for environment in &environments {
        validate_instance_url(&environment.id, &environment.instance_url)?;
    }
    environments.sort_by_key(|environment| environment.sort_order);
    Ok(environments)
}

/// Environment URLs carry Credentials on every request: https only, no
/// userinfo, no query/fragment. Trailing `/` is tolerated (`join_url` trims it).
/// The rules themselves live in `daku_protocol::instance_url_error` so the
/// desktop can apply the same policy (ADR-0004).
fn validate_instance_url(id: &str, url: &str) -> anyhow::Result<()> {
    match daku_protocol::instance_url_error(url) {
        Some(reason) => Err(anyhow!("environment {id}: {reason}")),
        None => Ok(()),
    }
}

/// Looks up the secret blob for an Environment id.
///
/// One Keychain item per Environment (`service=daku`, `account=<id>`).
/// Value is JSON: oauth → `{"client_id","client_secret"}`; basic → `{"username","password"}`.
pub trait CredentialStore: Send + Sync {
    fn get(&self, environment_id: &str) -> anyhow::Result<Option<String>>;
}

#[derive(Default)]
pub struct MemoryCredentialStore {
    secrets: Mutex<HashMap<String, String>>,
}

impl MemoryCredentialStore {
    pub fn insert(&self, environment_id: impl Into<String>, secret: impl Into<String>) {
        self.secrets
            .lock()
            .expect("credential map")
            .insert(environment_id.into(), secret.into());
    }
}

impl CredentialStore for MemoryCredentialStore {
    fn get(&self, environment_id: &str) -> anyhow::Result<Option<String>> {
        Ok(self
            .secrets
            .lock()
            .expect("credential map")
            .get(environment_id)
            .cloned())
    }
}

pub struct KeychainCredentialStore;

impl CredentialStore for KeychainCredentialStore {
    fn get(&self, environment_id: &str) -> anyhow::Result<Option<String>> {
        keychain_get(environment_id)
    }
}

#[cfg(target_os = "macos")]
fn keychain_get(environment_id: &str) -> anyhow::Result<Option<String>> {
    use security_framework::passwords::get_generic_password;

    match get_generic_password(KEYCHAIN_SERVICE, environment_id) {
        Ok(bytes) => Ok(Some(
            String::from_utf8(bytes).context("keychain secret is not utf-8")?,
        )),
        Err(error) if error.code() == -25300 => Ok(None),
        Err(error) => Err(anyhow!("keychain read for {environment_id}: {error}")),
    }
}

#[cfg(not(target_os = "macos"))]
fn keychain_get(_environment_id: &str) -> anyhow::Result<Option<String>> {
    Err(anyhow!("macOS Keychain is not available on this platform"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::TempFile;

    #[test]
    fn example_environments_json_parses() {
        let path = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../environments.example.json");
        let environments = load_environments(&path).unwrap();
        assert_eq!(environments[0].id, "prod");
        assert!(environments[0].clone_source);
        assert_eq!(
            environments[0].auth_method,
            AuthMethod::OauthClientCredentials
        );
        assert_eq!(environments[2].auth_method, AuthMethod::Basic);
        assert!(
            environments
                .iter()
                .all(|environment| environment.instance_url.contains("example.service-now.com"))
        );
    }

    fn write_temp(json: &str) -> TempFile {
        TempFile::with_contents("env", json)
    }

    fn one_environment(instance_url: &str) -> String {
        format!(
            r#"[{{"id":"dev","label":"Dev","instance_url":"{instance_url}","auth_method":"basic","sort_order":0}}]"#
        )
    }

    #[test]
    fn load_environments_rejects_http_url() {
        let file = write_temp(&one_environment("http://acme-dev.example.service-now.com"));
        let error = load_environments(file.path()).unwrap_err().to_string();
        assert!(error.contains("must start with https://"), "{error}");
    }

    #[test]
    fn load_environments_rejects_userinfo() {
        let file = write_temp(&one_environment(
            "https://user:pw@acme.example.service-now.com",
        ));
        let error = load_environments(file.path()).unwrap_err().to_string();
        assert!(error.contains("userinfo"), "{error}");
    }

    #[test]
    fn load_environments_rejects_query_and_fragment() {
        for url in [
            "https://acme.example.service-now.com/?x=1",
            "https://acme.example.service-now.com/#frag",
        ] {
            let file = write_temp(&one_environment(url));
            let error = load_environments(file.path()).unwrap_err().to_string();
            assert!(error.contains("query or fragment"), "{error}");
        }
    }

    #[test]
    fn load_environments_accepts_trailing_slash() {
        let file = write_temp(&one_environment("https://acme.example.service-now.com/"));
        let environments = load_environments(file.path()).unwrap();
        assert_eq!(environments.len(), 1);
    }
    #[test]
    fn load_environments_invalid_json_error_names_the_path() {
        let file = write_temp("{not json");
        let error = format!("{:#}", load_environments(file.path()).unwrap_err());
        assert!(error.contains("parsing "), "{error}");
        assert!(
            error.contains(file.path().file_name().unwrap().to_str().unwrap()),
            "{error}"
        );
    }

    #[test]
    fn load_environments_missing_file_error_names_the_path() {
        let missing = TempFile::new("missing");
        let error = load_environments(missing.path()).unwrap_err();
        assert!(format!("{error:#}").contains("reading "), "{error:#}");
        // `collector::is_not_found` relies on the io::Error surviving in the chain.
        assert!(error.chain().any(|cause| {
            cause
                .downcast_ref::<std::io::Error>()
                .is_some_and(|io_error| io_error.kind() == std::io::ErrorKind::NotFound)
        }));
    }

    #[test]
    fn load_environments_rejects_unknown_auth_method() {
        let file = write_temp(
            r#"[{"id":"dev","label":"Dev","instance_url":"https://acme.example.service-now.com","auth_method":"saml","sort_order":0}]"#,
        );
        let error = load_environments(file.path()).unwrap_err();
        assert!(format!("{error:#}").contains("parsing "), "{error:#}");
    }

    /// Duplicate ids parse today; pinned so a future validation change is deliberate.
    #[test]
    fn load_environments_sorts_by_sort_order_and_keeps_duplicate_ids() {
        let entry = |id: &str, sort_order: i64| {
            format!(
                r#"{{"id":"{id}","label":"{id}","instance_url":"https://{id}.example.service-now.com","auth_method":"basic","sort_order":{sort_order}}}"#
            )
        };
        let file = write_temp(&format!("[{},{}]", entry("second", 2), entry("first", 1)));
        let environments = load_environments(file.path()).unwrap();
        assert_eq!(
            environments
                .iter()
                .map(|environment| environment.id.as_str())
                .collect::<Vec<_>>(),
            ["first", "second"]
        );

        let file = write_temp(&format!("[{},{}]", entry("prod", 0), entry("prod", 1)));
        let duplicates = load_environments(file.path()).unwrap();
        assert_eq!(duplicates.len(), 2);
    }

    #[test]
    fn thresholds_default_to_historical_behaviour() {
        let thresholds = Thresholds::default();
        assert_eq!(thresholds.jobs_overdue_degraded_at, 1);
        assert_eq!(thresholds.syslog_error_degraded_at, 1);
        assert_eq!(thresholds.outbound_failures_degraded_at, 1);
        assert_eq!(thresholds.ecc_output_ready_degraded_at, 100);
        assert_eq!(thresholds.availability_rtt_degraded_ms, None);
        assert!(thresholds.summary().contains("rtt>off"));
    }

    #[test]
    fn environments_parse_thresholds_and_expected_drift() {
        let file = write_temp(
            r#"[{"id":"dev","label":"Dev","instance_url":"https://acme.example.service-now.com","auth_method":"basic","sort_order":0,"thresholds":{"syslog_error_degraded_at":10},"expected_drift":["com.example.staged"]}]"#,
        );
        let environments = load_environments(file.path()).unwrap();
        assert_eq!(environments[0].thresholds.syslog_error_degraded_at, 10);
        assert_eq!(environments[0].thresholds.jobs_overdue_degraded_at, 1);
        assert_eq!(
            environments[0].expected_drift,
            vec!["com.example.staged".to_owned()]
        );
    }

    #[test]
    fn environments_reject_unknown_threshold_keys() {
        let file = write_temp(
            r#"[{"id":"dev","label":"Dev","instance_url":"https://acme.example.service-now.com","auth_method":"basic","sort_order":0,"thresholds":{"syslog_typo":10}}]"#,
        );
        let error = format!("{:#}", load_environments(file.path()).unwrap_err());
        assert!(error.contains("parsing"), "{error}");
    }

    #[test]
    fn environments_without_thresholds_get_defaults() {
        let file = write_temp(&one_environment("https://acme.example.service-now.com"));
        let environments = load_environments(file.path()).unwrap();
        assert_eq!(environments[0].thresholds, Thresholds::default());
        assert!(environments[0].expected_drift.is_empty());
    }

    /// The rules moved to `daku-protocol`; these are the strings `daku-daemon
    /// doctor` shows, so they are pinned verbatim.
    #[test]
    fn validate_instance_url_messages_are_unchanged() {
        let message = |url: &str| validate_instance_url("test", url).unwrap_err().to_string();
        assert_eq!(
            message("http://x.example.com"),
            "environment test: instance_url must start with https://"
        );
        assert_eq!(
            message("https://"),
            "environment test: instance_url has no host"
        );
        assert_eq!(
            message("https://user@x.example.com"),
            "environment test: instance_url must not contain userinfo"
        );
        assert_eq!(
            message("https://x.example.com/?a=1"),
            "environment test: instance_url must not contain a query or fragment"
        );
        assert!(validate_instance_url("test", "https://x.example.com/").is_ok());
    }
}
