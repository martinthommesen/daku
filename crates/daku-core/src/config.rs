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
fn validate_instance_url(id: &str, url: &str) -> anyhow::Result<()> {
    let Some(rest) = url.strip_prefix("https://") else {
        return Err(anyhow!(
            "environment {id}: instance_url must start with https://"
        ));
    };
    let host = rest.split('/').next().unwrap_or("");
    if host.is_empty() {
        return Err(anyhow!("environment {id}: instance_url has no host"));
    }
    if host.contains('@') {
        return Err(anyhow!(
            "environment {id}: instance_url must not contain userinfo"
        ));
    }
    if rest.contains('?') || rest.contains('#') {
        return Err(anyhow!(
            "environment {id}: instance_url must not contain a query or fragment"
        ));
    }
    Ok(())
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

    fn write_temp(json: &str) -> PathBuf {
        let path = std::env::temp_dir().join(format!("daku-env-{}.json", uuid::Uuid::new_v4()));
        fs::write(&path, json).unwrap();
        path
    }

    fn one_environment(instance_url: &str) -> String {
        format!(
            r#"[{{"id":"dev","label":"Dev","instance_url":"{instance_url}","auth_method":"basic","sort_order":0}}]"#
        )
    }

    #[test]
    fn load_environments_rejects_http_url() {
        let path = write_temp(&one_environment("http://acme-dev.example.service-now.com"));
        let error = load_environments(&path).unwrap_err().to_string();
        let _ = fs::remove_file(&path);
        assert!(error.contains("must start with https://"), "{error}");
    }

    #[test]
    fn load_environments_rejects_userinfo() {
        let path = write_temp(&one_environment(
            "https://user:pw@acme.example.service-now.com",
        ));
        let error = load_environments(&path).unwrap_err().to_string();
        let _ = fs::remove_file(&path);
        assert!(error.contains("userinfo"), "{error}");
    }

    #[test]
    fn load_environments_rejects_query_and_fragment() {
        for url in [
            "https://acme.example.service-now.com/?x=1",
            "https://acme.example.service-now.com/#frag",
        ] {
            let path = write_temp(&one_environment(url));
            let error = load_environments(&path).unwrap_err().to_string();
            let _ = fs::remove_file(&path);
            assert!(error.contains("query or fragment"), "{error}");
        }
    }

    #[test]
    fn load_environments_accepts_trailing_slash() {
        let path = write_temp(&one_environment("https://acme.example.service-now.com/"));
        let environments = load_environments(&path).unwrap();
        let _ = fs::remove_file(&path);
        assert_eq!(environments.len(), 1);
    }
    #[test]
    fn load_environments_invalid_json_error_names_the_path() {
        let path = write_temp("{not json");
        let error = format!("{:#}", load_environments(&path).unwrap_err());
        let _ = fs::remove_file(&path);
        assert!(error.contains("parsing "), "{error}");
        assert!(
            error.contains(path.file_name().unwrap().to_str().unwrap()),
            "{error}"
        );
    }

    #[test]
    fn load_environments_missing_file_error_names_the_path() {
        let path = std::env::temp_dir().join(format!("daku-missing-{}.json", uuid::Uuid::new_v4()));
        let error = load_environments(&path).unwrap_err();
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
        let path = write_temp(
            r#"[{"id":"dev","label":"Dev","instance_url":"https://acme.example.service-now.com","auth_method":"saml","sort_order":0}]"#,
        );
        let error = load_environments(&path).unwrap_err();
        let _ = fs::remove_file(&path);
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
        let path = write_temp(&format!("[{},{}]", entry("second", 2), entry("first", 1)));
        let environments = load_environments(&path).unwrap();
        let _ = fs::remove_file(&path);
        assert_eq!(
            environments
                .iter()
                .map(|environment| environment.id.as_str())
                .collect::<Vec<_>>(),
            ["first", "second"]
        );

        let path = write_temp(&format!("[{},{}]", entry("prod", 0), entry("prod", 1)));
        let duplicates = load_environments(&path).unwrap();
        let _ = fs::remove_file(&path);
        assert_eq!(duplicates.len(), 2);
    }
}
