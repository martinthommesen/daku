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

#[derive(Debug, Clone, PartialEq, Eq, Deserialize, Serialize)]
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

/// Shape-checks a Credential blob against its auth method without ever
/// echoing the blob: OAuth needs non-empty `client_id` + `client_secret`,
/// basic needs non-empty `username` + `password`. Shared by the daemon
/// (save path) and the desktop sheet (pre-flight) so both reject the same
/// garbage with the same message.
pub fn validate_credential(auth_method: AuthMethod, blob: &str) -> Result<(), String> {
    let value: serde_json::Value =
        serde_json::from_str(blob).map_err(|_| "credential is not valid JSON".to_owned())?;
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
        return Err(format!(
            "credential does not match its auth method (needs {want})"
        ));
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
            "jobs≥{}/err≥{} syslog≥{} outbound≥{} flow≥{} email≥{} upgrade≥{} updates≥{} mid≥{}/ecc-err≥{}/queue≥{} drift≥{} rtt>{} txn>{}",
            self.jobs_overdue_degraded_at,
            off(self.jobs_error_degraded_at),
            self.syslog_error_degraded_at,
            self.outbound_failures_degraded_at,
            self.flow_error_degraded_at,
            off(self.email_failure_degraded_at),
            self.upgrade_failed_degraded_at,
            off(self.update_sets_open_degraded_at),
            self.mid_unhealthy_degraded_at,
            self.ecc_error_degraded_at,
            self.ecc_output_ready_degraded_at,
            self.drift_mismatches_degraded_at,
            rtt,
            txn,
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
    }
}
