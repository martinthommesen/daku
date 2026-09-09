//! Operator Environment configuration: the shape of `~/.daku/environments.json`
//! entries and of the `SaveEnvironment` / `TestEnvironment` wire commands.
//!
//! The serde shape is `snake_case` for the JSON file on disk, where Operator
//! files already exist — the wire commands reuse the same struct rather than
//! a second DTO, so unknown threshold keys are rejected on both paths alike.

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum AuthMethod {
    OauthClientCredentials,
    Basic,
}

/// Monitored product family. v1 was ServiceNow-only; the collector
/// dispatches per-Environment on this, and the sidebar groups by it once a
/// second platform is configured. Unknown values are rejected so typos fail
/// fast; missing values read as ServiceNow so v1 files keep loading.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Deserialize, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Platform {
    #[default]
    Servicenow,
    /// Generic HTTPS probe: status + latency on any URL.
    Http,
    /// GitHub Actions failures for one `owner/repo` via the REST API.
    Github,
}

impl Platform {
    /// Wire/sidebar id: what `EnvironmentSummary.platform_id` carries.
    pub fn id(self) -> &'static str {
        match self {
            Self::Servicenow => "servicenow",
            Self::Http => "http",
            Self::Github => "github",
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            Self::Servicenow => "ServiceNow",
            Self::Http => "HTTP",
            Self::Github => "GitHub",
        }
    }

    pub fn parse(id: &str) -> Option<Self> {
        Some(match id {
            "servicenow" => Self::Servicenow,
            "http" => Self::Http,
            "github" => Self::Github,
            _ => return None,
        })
    }
}

/// `owner/repo` from a GitHub URL, tolerating a trailing slash or `.git`.
/// Pure URL parsing shared by the daemon loader and the desktop sheet so
/// both accept the same spellings.
pub fn split_github_repo(instance_url: &str) -> Option<(String, String)> {
    let path = instance_url
        .strip_prefix("https://")?
        .split('/')
        .collect::<Vec<_>>();
    if path.len() < 3 || !path[0].eq_ignore_ascii_case("github.com") {
        return None;
    }
    let owner = path[1].trim();
    let repo = path[2].trim().trim_end_matches(".git").trim();
    if owner.is_empty() || repo.is_empty() {
        return None;
    }
    Some((owner.to_owned(), repo.to_owned()))
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct EnvironmentConfig {
    pub id: String,
    pub label: String,
    pub instance_url: String,
    pub auth_method: AuthMethod,
    pub sort_order: i64,
    #[serde(default)]
    pub clone_source: bool,
    /// Monitored product family. Missing reads as ServiceNow so v1
    /// `environments.json` files keep loading.
    #[serde(default)]
    pub platform: Platform,
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
/// outbound failure, one flow error, any unhealthy MID or ECC error, ECC output-ready ≥ 100,
/// any plugin/build mismatch degrades. Availability RTT never degraded before
/// (`None` = disabled); jobs error count never voted before (`u64::MAX`).
#[derive(Debug, Clone, PartialEq, Eq, Deserialize, Serialize)]
#[serde(default, deny_unknown_fields)]
pub struct Thresholds {
    pub jobs_overdue_degraded_at: u64,
    pub jobs_error_degraded_at: u64,
    pub syslog_error_degraded_at: u64,
    pub outbound_failures_degraded_at: u64,
    pub flow_error_degraded_at: u64,
    pub email_failure_degraded_at: u64,
    pub upgrade_failed_degraded_at: u64,
    pub transaction_avg_degraded_ms: Option<u64>,
    pub update_sets_open_degraded_at: u64,
    pub scan_p1_degraded_at: u64,
    pub http_probe_rtt_degraded_ms: Option<u64>,
    pub actions_failed_degraded_at: u64,
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
            flow_error_degraded_at: 1,
            // Email send failures are noisy on dev (password resets, test
            // notifications), so this Signal is opt-in: `u64::MAX` never
            // votes until the Operator sets a ceiling.
            email_failure_degraded_at: u64::MAX,
            upgrade_failed_degraded_at: 1,
            // Developers keep work-in-progress sets open while building, so
            // the backlog Signal is opt-in like email: `u64::MAX` never votes
            // until the Operator sets a ceiling.
            update_sets_open_degraded_at: u64::MAX,
            scan_p1_degraded_at: 1,
            // Generic probes vary per target, so the ceiling is opt-in like
            // the other RTT ceilings: `None` never votes.
            http_probe_rtt_degraded_ms: None,
            actions_failed_degraded_at: 1,
            // Transaction slowness varies wildly per instance (PDI hardware vs
            // prod), so this Signal is opt-in like the availability RTT
            // ceiling: `None` never votes until the Operator sets a ceiling.
            transaction_avg_degraded_ms: None,
            mid_unhealthy_degraded_at: 1,
            ecc_error_degraded_at: 1,
            ecc_output_ready_degraded_at: 100,
            drift_mismatches_degraded_at: 1,
            availability_rtt_degraded_ms: None,
        }
    }
}

/// Signals that never vote in the Environment health rollup: history and
/// capacity context (`health.rs`), no matter their state. The desktop's
/// health explainer uses the same list so "Degraded because" never names a
/// Signal that did not vote.
pub const NON_VOTING_SIGNALS: [&str; 3] = ["last_clone", "sessions", "table_growth"];

/// Shape-checks a Credential blob against its auth method without ever
/// echoing the blob: OAuth needs non-empty `client_id` + `client_secret`,
/// basic needs non-empty `username` + `password`. Shared by the daemon
/// (save path) and the desktop sheet (pre-flight) so both reject the same
/// garbage with the same message. A typed error (not `String`) so anyhow
/// callers keep context chains instead of flattening to one line.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CredentialShapeError {
    reason: String,
}

impl std::fmt::Display for CredentialShapeError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.reason)
    }
}

impl std::error::Error for CredentialShapeError {}

pub fn validate_credential(
    auth_method: AuthMethod,
    blob: &str,
) -> Result<(), CredentialShapeError> {
    let err = |reason: String| CredentialShapeError { reason };
    let value: serde_json::Value =
        serde_json::from_str(blob).map_err(|_| err("credential is not valid JSON".to_owned()))?;
    let present = |key: &str| {
        value
            .get(key)
            .and_then(|item| item.as_str())
            .is_some_and(|text| !text.trim().is_empty())
    };
    let ok = match auth_method {
        AuthMethod::OauthClientCredentials => present("client_id") && present("client_secret"),
        AuthMethod::Basic => present("username") && present("password"),
    };
    if !ok {
        let want = match auth_method {
            AuthMethod::OauthClientCredentials => "client_id and client_secret",
            AuthMethod::Basic => "username and password",
        };
        return Err(err(format!(
            "credential does not match its auth method (needs {want})"
        )));
    }
    Ok(())
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
        let txn = self
            .transaction_avg_degraded_ms
            .map(|ms| format!("{ms}ms"))
            .unwrap_or_else(|| "off".to_owned());
        format!(
            "jobs≥{}/err≥{} syslog≥{} outbound≥{} flow≥{} email≥{} upgrade≥{} updates≥{} scan≥{} actions≥{} mid≥{}/ecc-err≥{}/queue≥{} drift≥{} rtt>{} txn>{} probe>{}",
            self.jobs_overdue_degraded_at,
            off(self.jobs_error_degraded_at),
            self.syslog_error_degraded_at,
            self.outbound_failures_degraded_at,
            self.flow_error_degraded_at,
            off(self.email_failure_degraded_at),
            self.upgrade_failed_degraded_at,
            off(self.update_sets_open_degraded_at),
            self.scan_p1_degraded_at,
            self.actions_failed_degraded_at,
            self.mid_unhealthy_degraded_at,
            self.ecc_error_degraded_at,
            self.ecc_output_ready_degraded_at,
            self.drift_mismatches_degraded_at,
            rtt,
            txn,
            self.http_probe_rtt_degraded_ms
                .map(|ms| format!("{ms}ms"))
                .unwrap_or_else(|| "off".to_owned()),
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn config_and_wire_share_one_shape() {
        // `environments.json` (snake_case) and the Save/Test wire commands
        // must parse the same struct: an Operator file stays loadable.
        let file_json = r#"{"id":"dev","label":"Dev","instance_url":"https://x.example.service-now.com","auth_method":"basic","sort_order":0}"#;
        let from_file: EnvironmentConfig = serde_json::from_str(file_json).unwrap();
        assert_eq!(from_file.thresholds, Thresholds::default());
        let wire = serde_json::to_value(&from_file).unwrap();
        assert_eq!(wire["auth_method"], "basic");
        assert_eq!(wire["instance_url"], "https://x.example.service-now.com");
        let back: EnvironmentConfig = serde_json::from_value(wire).unwrap();
        assert_eq!(back, from_file);
        // v1 files without a platform read as ServiceNow.
        assert_eq!(from_file.platform, Platform::Servicenow);
    }

    #[test]
    fn platform_ids_round_trip_and_reject_unknowns() {
        for (id, platform) in [
            ("servicenow", Platform::Servicenow),
            ("http", Platform::Http),
            ("github", Platform::Github),
        ] {
            assert_eq!(Platform::parse(id), Some(platform));
            assert_eq!(platform.id(), id);
            let json = serde_json::to_value(platform).unwrap();
            assert_eq!(json, serde_json::Value::String(id.into()));
            assert_eq!(serde_json::from_value::<Platform>(json).unwrap(), platform);
        }
        assert_eq!(Platform::parse("jira"), None);
        assert_eq!(Platform::default(), Platform::Servicenow);
    }
}
