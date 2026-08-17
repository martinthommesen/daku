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
    environments.sort_by_key(|environment| environment.sort_order);
    Ok(environments)
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
}
