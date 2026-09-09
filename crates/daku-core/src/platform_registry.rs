//! Platform registry: the one module that knows Platforms.
//!
//! The registry was an enum plus N-way `match environment.platform` at
//! four sites (`build_default_loop`, `poll_hosts`, doctor dispatch, URL
//! validation) plus a string→Platform duplicate in the daemon entry.
//! This module deepens it to a real seam: one adapter per Platform —
//! hypothetical today, real the day a fourth Platform lands.
//! Adding a Platform touches this file, not four.
//!
//! Transport stays explicit: ServiceNow Signals use the authed client,
//! HTTP/GitHub probes use raw requests. The type split lives with the
//! callers (`execute_raw` vs `request`); this module owns the decision
//! of *which* kind each Platform needs so the rule is stated once.
//!
//! ADR-0011 holds: compiled-in registry, no dynamic plugins.

use crate::config::{EnvironmentConfig, Platform, split_github_repo};

/// Which transport a Platform's probes need.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TransportKind {
    /// Authed ServiceNow client (`request`: OAuth/basic + 401 refresh).
    Authed,
    /// Raw HTTP (`execute_raw`: own headers, no ServiceNow auth).
    Raw,
}

/// Transport per Platform. Stated once — callers must match it.
pub fn transport_for(platform: Platform) -> TransportKind {
    match platform {
        Platform::Servicenow => TransportKind::Authed,
        Platform::Http | Platform::Github => TransportKind::Raw,
    }
}

/// Signal ids registered per Platform. Mirrors `build_default_loop` wiring.
pub fn signals_for(platform: Platform) -> &'static [&'static str] {
    match platform {
        Platform::Servicenow => &[
            "availability",
            "jobs",
            "syslog",
            "mid_ecc",
            "outbound",
            "flow",
            "email",
            "upgrade",
            "sessions",
            "table_growth",
            "slow_txn",
            "update_sets",
        ],
        Platform::Http => &["http_probe"],
        Platform::Github => &["actions"],
    }
}

/// Slow collectors (drift, last-clone, scan) read ServiceNow inventories
/// and history: they ride the slow cadence only when a ServiceNow
/// Environment exists.
pub fn needs_slow_collectors(environments: &[EnvironmentConfig]) -> bool {
    environments
        .iter()
        .any(|environment| environment.platform == Platform::Servicenow)
}

/// ServiceNow subset for the slow collectors.
pub fn servicenow_subset(environments: &[EnvironmentConfig]) -> Vec<EnvironmentConfig> {
    environments
        .iter()
        .filter(|environment| environment.platform == Platform::Servicenow)
        .cloned()
        .collect()
}

/// Host one poll round must reach for one Environment: the GitHub API for
/// repo Environments, the configured host verbatim otherwise.
pub fn poll_host(environment: &EnvironmentConfig) -> Option<String> {
    match environment.platform {
        Platform::Github => Some("api.github.com".to_owned()),
        _ => host_of_url(&environment.instance_url),
    }
}

/// Hosts one poll round must reach, deduped, in config order.
pub fn poll_hosts(environments: &[EnvironmentConfig]) -> Vec<String> {
    let mut hosts = Vec::new();
    for environment in environments {
        if let Some(host) = poll_host(environment)
            && !host.is_empty()
            && !hosts.contains(&host)
        {
            hosts.push(host);
        }
    }
    hosts
}

pub(crate) fn host_of_url(url: &str) -> Option<String> {
    let authority = url.split("://").nth(1)?.split('/').next()?;
    let host = authority.rsplit('@').next().unwrap_or(authority);
    let host = match host.strip_prefix('[') {
        Some(bracketed) => bracketed.split(']').next().unwrap_or(""),
        None => host.split(':').next().unwrap_or(""),
    };
    if host.is_empty() {
        None
    } else {
        Some(host.to_owned())
    }
}

/// Per-platform URL shape on top of the generic https rule: a GitHub
/// Environment must point at one `owner/repo`.
pub fn validate_platform_url(environment: &EnvironmentConfig) -> anyhow::Result<()> {
    if environment.platform == Platform::Github
        && split_github_repo(&environment.instance_url).is_none()
    {
        anyhow::bail!(
            "environment {}: github instance_url must look like https://github.com/<owner>/<repo>",
            environment.id
        );
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::{AuthMethod, Thresholds};

    fn env(platform: Platform, url: &str) -> EnvironmentConfig {
        EnvironmentConfig {
            id: "e".into(),
            label: "E".into(),
            instance_url: url.into(),
            auth_method: AuthMethod::Basic,
            sort_order: 0,
            clone_source: false,
            platform,
            thresholds: Thresholds::default(),
            expected_drift: Vec::new(),
        }
    }

    #[test]
    fn transports_are_stated_once() {
        assert_eq!(transport_for(Platform::Servicenow), TransportKind::Authed);
        assert_eq!(transport_for(Platform::Http), TransportKind::Raw);
        assert_eq!(transport_for(Platform::Github), TransportKind::Raw);
    }

    #[test]
    fn signals_cover_every_platform() {
        assert_eq!(signals_for(Platform::Servicenow).len(), 12);
        assert_eq!(signals_for(Platform::Http), &["http_probe"]);
        assert_eq!(signals_for(Platform::Github), &["actions"]);
    }

    #[test]
    fn poll_hosts_pins_github_api_and_dedupes() {
        let envs = vec![
            env(Platform::Github, "https://github.com/acme/app"),
            env(Platform::Servicenow, "https://prod.example.com"),
            env(Platform::Servicenow, "https://prod.example.com/"),
        ];
        assert_eq!(
            poll_hosts(&envs),
            vec!["api.github.com".to_owned(), "prod.example.com".to_owned()]
        );
    }

    #[test]
    fn slow_collectors_only_with_servicenow() {
        assert!(needs_slow_collectors(&[env(
            Platform::Servicenow,
            "https://prod.example.com"
        )]));
        assert!(!needs_slow_collectors(&[env(
            Platform::Github,
            "https://github.com/acme/app"
        )]));
    }

    #[test]
    fn github_url_shape_rejects_bare_owner() {
        assert!(validate_platform_url(&env(Platform::Github, "https://github.com/acme")).is_err());
        assert!(
            validate_platform_url(&env(Platform::Github, "https://github.com/acme/app")).is_ok()
        );
    }
}
