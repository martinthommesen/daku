//! Operator Environment list and Credential lookup.

use std::collections::HashMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::Mutex;

use anyhow::{Context, anyhow};

use daku_protocol::identity::DATA_DIRECTORY_NAME;

pub const KEYCHAIN_SERVICE: &str = "daku";

pub use daku_protocol::{AuthMethod, EnvironmentConfig, Thresholds};

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

/// Repairs what `doctor --fix` may safely touch: the parent directory
/// (created, `0700` when daku owns it), a missing `environments.json`
/// (created holding an empty list, `0600`), and lax file modes (`0600`).
/// Never invents Credentials, never guesses URLs, never rewrites content —
/// an unparsable file is left for the Operator to fix by hand. Returns a
/// line per applied fix for the caller to print.
pub fn repair_environments_setup(path: &Path) -> anyhow::Result<Vec<String>> {
    use std::io::Write as _;

    let mut fixed = Vec::new();
    if let Some(parent) = path.parent().filter(|p| !p.as_os_str().is_empty()) {
        #[cfg(unix)]
        let existed = parent.exists();
        fs::create_dir_all(parent).with_context(|| format!("creating {}", parent.display()))?;
        #[cfg(unix)]
        if !existed
            || parent
                .file_name()
                .is_some_and(|name| name == std::ffi::OsStr::new(".daku"))
        {
            use std::os::unix::fs::PermissionsExt as _;
            fs::set_permissions(parent, fs::Permissions::from_mode(0o700))
                .with_context(|| format!("securing {}", parent.display()))?;
            fixed.push(format!("secured {} to 0700", parent.display()));
        }
    }
    if !path.exists() {
        let mut options = fs::OpenOptions::new();
        options.write(true).create_new(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt as _;
            options.mode(0o600);
        }
        let mut file = options
            .open(path)
            .with_context(|| format!("creating {}", path.display()))?;
        file.write_all(b"[]\n")
            .with_context(|| format!("writing {}", path.display()))?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt as _;
            fs::set_permissions(path, fs::Permissions::from_mode(0o600))
                .with_context(|| format!("securing {}", path.display()))?;
        }
        fixed.push(format!(
            "created {} holding an empty Environment list — add Environments from the app menu",
            path.display()
        ));
        return Ok(fixed);
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt as _;
        let mode = fs::metadata(path)
            .with_context(|| format!("reading {}", path.display()))?
            .permissions()
            .mode()
            & 0o777;
        if mode != 0o600 {
            fs::set_permissions(path, fs::Permissions::from_mode(0o600))
                .with_context(|| format!("securing {}", path.display()))?;
            fixed.push(format!("secured {} to 0600", path.display()));
        }
    }
    Ok(fixed)
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
    /// Writes (creates or rotates) the secret blob. Only the daemon calls
    /// this, and only for an explicit Operator save over the authenticated
    /// loopback socket (ADR-0004 amendment, plan `080`).
    fn set(&self, environment_id: &str, secret: &str) -> anyhow::Result<()>;
    /// Deletes the secret blob. Missing items are not an error.
    fn delete(&self, environment_id: &str) -> anyhow::Result<()>;
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

    fn set(&self, environment_id: &str, secret: &str) -> anyhow::Result<()> {
        self.insert(environment_id, secret);
        Ok(())
    }

    fn delete(&self, environment_id: &str) -> anyhow::Result<()> {
        self.secrets
            .lock()
            .expect("credential map")
            .remove(environment_id);
        Ok(())
    }
}

pub struct KeychainCredentialStore;

impl CredentialStore for KeychainCredentialStore {
    fn get(&self, environment_id: &str) -> anyhow::Result<Option<String>> {
        keychain_get(environment_id)
    }

    fn set(&self, environment_id: &str, secret: &str) -> anyhow::Result<()> {
        keychain_set(environment_id, secret)
    }

    fn delete(&self, environment_id: &str) -> anyhow::Result<()> {
        keychain_delete(environment_id)
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

#[cfg(target_os = "macos")]
fn keychain_set(environment_id: &str, secret: &str) -> anyhow::Result<()> {
    use security_framework::passwords::set_generic_password;

    // Creates or rotates (`set_generic_password` updates on duplicate), so a
    // re-save never fails on the existing item. The bytes travel process
    // memory only — never argv, env, logs, or SQLite.
    set_generic_password(KEYCHAIN_SERVICE, environment_id, secret.as_bytes())
        .with_context(|| format!("keychain write for {environment_id}"))
}

#[cfg(target_os = "macos")]
fn keychain_delete(environment_id: &str) -> anyhow::Result<()> {
    use security_framework::passwords::delete_generic_password;

    match delete_generic_password(KEYCHAIN_SERVICE, environment_id) {
        Ok(()) => Ok(()),
        // Already gone is the desired end state.
        Err(error) if error.code() == -25300 => Ok(()),
        Err(error) => Err(anyhow!("keychain delete for {environment_id}: {error}")),
    }
}

#[cfg(not(target_os = "macos"))]
fn keychain_get(_environment_id: &str) -> anyhow::Result<Option<String>> {
    Err(anyhow!("macOS Keychain is not available on this platform"))
}

#[cfg(not(target_os = "macos"))]
fn keychain_set(_environment_id: &str, _secret: &str) -> anyhow::Result<()> {
    Err(anyhow!("macOS Keychain is not available on this platform"))
}

#[cfg(not(target_os = "macos"))]
fn keychain_delete(_environment_id: &str) -> anyhow::Result<()> {
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

    #[test]
    fn repair_creates_a_missing_file_holding_an_empty_list() {
        let missing = TempFile::new("repair-missing");
        assert!(!missing.path().exists());
        let fixed = repair_environments_setup(missing.path()).unwrap();
        assert_eq!(load_environments(missing.path()).unwrap(), vec![]);
        assert!(
            fixed
                .iter()
                .any(|line| line.contains("empty Environment list")),
            "{fixed:?}"
        );
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt as _;
            assert_eq!(
                fs::metadata(missing.path()).unwrap().permissions().mode() & 0o777,
                0o600
            );
        }
    }

    #[test]
    fn repair_secures_a_lax_mode_and_leaves_content_alone() {
        let file = write_temp(&one_environment("https://acme.example.service-now.com"));
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt as _;
            fs::set_permissions(file.path(), fs::Permissions::from_mode(0o644)).unwrap();
        }
        let fixed = repair_environments_setup(file.path()).unwrap();
        assert_eq!(load_environments(file.path()).unwrap().len(), 1);
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt as _;
            assert_eq!(
                fs::metadata(file.path()).unwrap().permissions().mode() & 0o777,
                0o600
            );
        }
        assert!(fixed.iter().any(|line| line.contains("0600")), "{fixed:?}");
    }

    #[test]
    fn repair_never_rewrites_an_unparsable_file() {
        let file = write_temp("{not json");
        let before = fs::read(file.path()).unwrap();
        let _ = repair_environments_setup(file.path()).unwrap();
        assert_eq!(fs::read(file.path()).unwrap(), before);
        assert!(load_environments(file.path()).is_err());
    }

    /// Operator-run check for plan 080: an item created by this binary reads
    /// back in the same binary. Ignored by default — it touches the real login
    /// keychain (throwaway service name, deleted afterwards). Run with
    /// `cargo test -p daku-core -- --ignored keychain`.
    /// A first-run authorization dialog is the expected macOS behaviour; a
    /// failure here re-plans 080 around client-side `security` prompting.
    #[cfg(target_os = "macos")]
    #[ignore]
    #[test]
    fn keychain_write_reads_back_in_same_binary() {
        use security_framework::passwords::{
            delete_generic_password, get_generic_password, set_generic_password,
        };
        let service = "daku-keychain-check-throwaway";
        let account = "check-1";
        let secret = b"{\"probe\": true}";
        set_generic_password(service, account, secret).expect("keychain write");
        let back = get_generic_password(service, account).expect("keychain read-back");
        assert_eq!(back, secret, "same binary must read what it wrote");
        delete_generic_password(service, account).expect("keychain cleanup");
        assert!(
            get_generic_password(service, account).is_err(),
            "throwaway item must be gone"
        );
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
