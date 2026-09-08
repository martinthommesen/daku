//! Credential rotation: validate-then-replace for one Environment.
//!
//! `daku-daemon rotate-credential --env <id> --secret-file <path>`
//! shape-checks the new blob, dry-run probes with it, and only then replaces
//! the stored item. A failed probe aborts with the old item untouched, so a
//! typo'd secret can never lock the Operator out of their own monitoring.
//! `--no-probe` forces the write (documented risk, same flag as `setup`).

use crate::config::{CredentialStore, EnvironmentConfig};
use crate::environments::test_environment;
use crate::servicenow::ServiceNowClient;
use daku_protocol::Reachability;

/// Rotates one Environment's stored Credential. Returns the one-line
/// summary the CLI prints.
pub fn rotate_credential(
    credentials: &dyn CredentialStore,
    client: &ServiceNowClient,
    environment: &EnvironmentConfig,
    secret: &str,
    probe: bool,
) -> anyhow::Result<String> {
    daku_protocol::validate_credential(environment.auth_method, secret)?;
    if probe {
        let observation = test_environment(environment, Some(secret), credentials, client)?;
        if observation.reachability == Reachability::Unreachable {
            anyhow::bail!(
                "new credential for {} does not reach ({}); old item kept",
                environment.id,
                observation.error.as_deref().unwrap_or("no route")
            );
        }
    }
    credentials.set(&environment.id, secret)?;
    Ok(format!("rotated credential for {}", environment.id))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::{AuthMethod, MemoryCredentialStore, Platform, Thresholds};
    use crate::servicenow::{HttpRequest, HttpResponse, HttpTransport, SystemClock};

    const OLD: &str = r#"{"username":"reader","password":"old"}"#;
    const NEW: &str = r#"{"username":"reader","password":"new"}"#;

    fn env() -> EnvironmentConfig {
        EnvironmentConfig {
            id: "prod".into(),
            label: "Production".into(),
            instance_url: "https://acme-prod.example.service-now.com".into(),
            auth_method: AuthMethod::Basic,
            sort_order: 0,
            clone_source: false,
            platform: Platform::Servicenow,
            thresholds: Thresholds::default(),
            expected_drift: Vec::new(),
        }
    }

    struct ProbeTransport {
        ok: bool,
    }

    impl HttpTransport for ProbeTransport {
        fn execute(&self, request: &HttpRequest) -> anyhow::Result<HttpResponse> {
            if !self.ok {
                anyhow::bail!("offline")
            } else if request.url.contains("sys_properties") {
                Ok(HttpResponse {
                    status: 200,
                    headers: vec![("content-type".into(), "application/json".into())],
                    body: r#"{"result":[{"value":"glide-9"}]}"#.into(),
                })
            } else {
                Ok(HttpResponse {
                    status: 200,
                    headers: vec![("content-type".into(), "application/json".into())],
                    body: r#"{"result":[]}"#.into(),
                })
            }
        }
    }

    #[test]
    fn rotation_replaces_after_a_successful_probe() {
        let credentials = MemoryCredentialStore::default();
        credentials.insert("prod", OLD);
        let client = ServiceNowClient::new(ProbeTransport { ok: true }, SystemClock);
        let line = rotate_credential(&credentials, &client, &env(), NEW, true).unwrap();
        assert!(line.contains("rotated credential for prod"), "{line}");
        assert_eq!(credentials.get("prod").unwrap().as_deref(), Some(NEW));
    }

    #[test]
    fn rotation_aborts_on_probe_failure_and_keeps_the_old_item() {
        let credentials = MemoryCredentialStore::default();
        credentials.insert("prod", OLD);
        let client = ServiceNowClient::new(ProbeTransport { ok: false }, SystemClock);
        let error = rotate_credential(&credentials, &client, &env(), NEW, true)
            .unwrap_err()
            .to_string();
        assert!(error.contains("old item kept"), "{error}");
        assert_eq!(credentials.get("prod").unwrap().as_deref(), Some(OLD));
    }

    #[test]
    fn rotation_rejects_shape_mismatches_before_touching_the_store() {
        let credentials = MemoryCredentialStore::default();
        credentials.insert("prod", OLD);
        let client = ServiceNowClient::new(ProbeTransport { ok: true }, SystemClock);
        let error = rotate_credential(&credentials, &client, &env(), r#"{"client_id":"x"}"#, true)
            .unwrap_err()
            .to_string();
        assert!(error.contains("username and password"), "{error}");
        assert_eq!(credentials.get("prod").unwrap().as_deref(), Some(OLD));
    }

    #[test]
    fn rotation_no_probe_forces_the_write() {
        let credentials = MemoryCredentialStore::default();
        credentials.insert("prod", OLD);
        let client = ServiceNowClient::new(ProbeTransport { ok: false }, SystemClock);
        rotate_credential(&credentials, &client, &env(), NEW, false).unwrap();
        assert_eq!(credentials.get("prod").unwrap().as_deref(), Some(NEW));
    }
}
