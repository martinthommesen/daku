//! Environment management sheet (080): add / edit / remove Environments,
//! Credential included.
//!
//! Secrets never read back: credential fields always start empty, and an
//! empty pair means "leave the stored Credential untouched". Shape-checking
//! reuses the wire contract (`validate_credential`), so the sheet and the
//! daemon reject the same garbage with the same message.

use daku_protocol::{AuthMethod, EnvironmentConfig, Thresholds};
use gpui::{App, Entity, Styled, Window, div, prelude::*, px};
use gpui_component::ActiveTheme as _;
use gpui_component::input::{Input, InputState};

/// Live add/edit sheet. Credential entities always start empty and are never
/// prefilled: secrets are write-only from the sheet's side.
pub struct EnvSheet {
    /// `None` adds; `Some(id)` edits (ids are immutable).
    pub editing_id: Option<String>,
    pub id_field: Entity<InputState>,
    pub label_field: Entity<InputState>,
    pub url_field: Entity<InputState>,
    pub auth: AuthMethod,
    pub clone_source: bool,
    pub secret_a: Entity<InputState>,
    pub secret_b: Entity<InputState>,
    /// Per-Environment threshold overrides, as edited text. Empty means
    /// default; `jobs_error`, `email`, `updates`, `rtt` and `txn` also accept
    /// `off`.
    pub threshold_jobs_overdue: Entity<InputState>,
    pub threshold_jobs_error: Entity<InputState>,
    pub threshold_syslog: Entity<InputState>,
    pub threshold_outbound: Entity<InputState>,
    pub threshold_flow: Entity<InputState>,
    pub threshold_email: Entity<InputState>,
    pub threshold_upgrade: Entity<InputState>,
    pub threshold_txn: Entity<InputState>,
    pub threshold_updates: Entity<InputState>,
    pub threshold_mid: Entity<InputState>,
    pub threshold_ecc_error: Entity<InputState>,
    pub threshold_ecc_queue: Entity<InputState>,
    pub threshold_drift: Entity<InputState>,
    pub threshold_rtt: Entity<InputState>,
    /// Planned drift ids, comma- or newline-separated.
    pub expected_drift_field: Entity<InputState>,
    pub busy: bool,
    /// (is_error, text) line under the fields.
    pub notice: Option<(bool, String)>,
    pub delete_armed: bool,
}

impl EnvSheet {
    fn field(window: &mut Window, cx: &mut App, initial: &str, masked: bool) -> Entity<InputState> {
        cx.new(|cx| {
            InputState::new(window, cx)
                .default_value(initial)
                .masked(masked)
        })
    }

    pub fn add(window: &mut Window, cx: &mut App) -> Self {
        let defaults = Thresholds::default();
        Self {
            editing_id: None,
            id_field: Self::field(window, cx, "", false),
            label_field: Self::field(window, cx, "", false),
            url_field: Self::field(window, cx, "https://", false),
            auth: AuthMethod::OauthClientCredentials,
            clone_source: false,
            secret_a: Self::field(window, cx, "", false),
            secret_b: Self::field(window, cx, "", true),
            threshold_jobs_overdue: Self::field(
                window,
                cx,
                &format_u64_threshold(defaults.jobs_overdue_degraded_at),
                false,
            ),
            threshold_jobs_error: Self::field(
                window,
                cx,
                &format_u64_threshold(defaults.jobs_error_degraded_at),
                false,
            ),
            threshold_syslog: Self::field(
                window,
                cx,
                &format_u64_threshold(defaults.syslog_error_degraded_at),
                false,
            ),
            threshold_outbound: Self::field(
                window,
                cx,
                &format_u64_threshold(defaults.outbound_failures_degraded_at),
                false,
            ),
            threshold_flow: Self::field(
                window,
                cx,
                &format_u64_threshold(defaults.flow_error_degraded_at),
                false,
            ),
            threshold_email: Self::field(
                window,
                cx,
                &format_u64_threshold(defaults.email_failure_degraded_at),
                false,
            ),
            threshold_upgrade: Self::field(
                window,
                cx,
                &format_u64_threshold(defaults.upgrade_failed_degraded_at),
                false,
            ),
            threshold_txn: Self::field(
                window,
                cx,
                &format_rtt_threshold(defaults.transaction_avg_degraded_ms),
                false,
            ),
            threshold_updates: Self::field(
                window,
                cx,
                &format_u64_threshold(defaults.update_sets_open_degraded_at),
                false,
            ),
            threshold_mid: Self::field(
                window,
                cx,
                &format_u64_threshold(defaults.mid_unhealthy_degraded_at),
                false,
            ),
            threshold_ecc_error: Self::field(
                window,
                cx,
                &format_u64_threshold(defaults.ecc_error_degraded_at),
                false,
            ),
            threshold_ecc_queue: Self::field(
                window,
                cx,
                &format_u64_threshold(defaults.ecc_output_ready_degraded_at),
                false,
            ),
            threshold_drift: Self::field(
                window,
                cx,
                &format_u64_threshold(defaults.drift_mismatches_degraded_at),
                false,
            ),
            threshold_rtt: Self::field(
                window,
                cx,
                &format_rtt_threshold(defaults.availability_rtt_degraded_ms),
                false,
            ),
            expected_drift_field: Self::field(window, cx, "", false),
            busy: false,
            notice: None,
            delete_armed: false,
        }
    }

    pub fn edit(window: &mut Window, cx: &mut App, env: &EnvironmentConfig) -> Self {
        Self {
            editing_id: Some(env.id.clone()),
            id_field: Self::field(window, cx, &env.id, false),
            label_field: Self::field(window, cx, &env.label, false),
            url_field: Self::field(window, cx, &env.instance_url, false),
            auth: env.auth_method,
            clone_source: env.clone_source,
            secret_a: Self::field(window, cx, "", false),
            secret_b: Self::field(window, cx, "", true),
            threshold_jobs_overdue: Self::field(
                window,
                cx,
                &format_u64_threshold(env.thresholds.jobs_overdue_degraded_at),
                false,
            ),
            threshold_jobs_error: Self::field(
                window,
                cx,
                &format_u64_threshold(env.thresholds.jobs_error_degraded_at),
                false,
            ),
            threshold_syslog: Self::field(
                window,
                cx,
                &format_u64_threshold(env.thresholds.syslog_error_degraded_at),
                false,
            ),
            threshold_outbound: Self::field(
                window,
                cx,
                &format_u64_threshold(env.thresholds.outbound_failures_degraded_at),
                false,
            ),
            threshold_flow: Self::field(
                window,
                cx,
                &format_u64_threshold(env.thresholds.flow_error_degraded_at),
                false,
            ),
            threshold_email: Self::field(
                window,
                cx,
                &format_u64_threshold(env.thresholds.email_failure_degraded_at),
                false,
            ),
            threshold_upgrade: Self::field(
                window,
                cx,
                &format_u64_threshold(env.thresholds.upgrade_failed_degraded_at),
                false,
            ),
            threshold_txn: Self::field(
                window,
                cx,
                &format_rtt_threshold(env.thresholds.transaction_avg_degraded_ms),
                false,
            ),
            threshold_updates: Self::field(
                window,
                cx,
                &format_u64_threshold(env.thresholds.update_sets_open_degraded_at),
                false,
            ),
            threshold_mid: Self::field(
                window,
                cx,
                &format_u64_threshold(env.thresholds.mid_unhealthy_degraded_at),
                false,
            ),
            threshold_ecc_error: Self::field(
                window,
                cx,
                &format_u64_threshold(env.thresholds.ecc_error_degraded_at),
                false,
            ),
            threshold_ecc_queue: Self::field(
                window,
                cx,
                &format_u64_threshold(env.thresholds.ecc_output_ready_degraded_at),
                false,
            ),
            threshold_drift: Self::field(
                window,
                cx,
                &format_u64_threshold(env.thresholds.drift_mismatches_degraded_at),
                false,
            ),
            threshold_rtt: Self::field(
                window,
                cx,
                &format_rtt_threshold(env.thresholds.availability_rtt_degraded_ms),
                false,
            ),
            expected_drift_field: Self::field(
                window,
                cx,
                &format_expected_drift_text(&env.expected_drift),
                false,
            ),
            busy: false,
            notice: None,
            delete_armed: false,
        }
    }

    /// Current field values for rendering and RPC assembly.
    pub fn values(&self, cx: &App) -> SheetValues {
        SheetValues {
            id: self.id_field.read(cx).value().to_string(),
            label: self.label_field.read(cx).value().to_string(),
            url: self.url_field.read(cx).value().to_string(),
            secret_a: self.secret_a.read(cx).value().to_string(),
            secret_b: self.secret_b.read(cx).value().to_string(),
        }
    }

    /// Parses the fourteen threshold fields into effective values. Empty means
    /// default; `jobs_error`, `email`, `updates`, `rtt` and `txn` also accept
    /// `off`.
    pub fn thresholds_result(&self, cx: &App) -> Result<Thresholds, String> {
        parse_thresholds(&ThresholdTexts {
            jobs_overdue: self.threshold_jobs_overdue.read(cx).value().to_string(),
            jobs_error: self.threshold_jobs_error.read(cx).value().to_string(),
            syslog: self.threshold_syslog.read(cx).value().to_string(),
            outbound: self.threshold_outbound.read(cx).value().to_string(),
            flow: self.threshold_flow.read(cx).value().to_string(),
            email: self.threshold_email.read(cx).value().to_string(),
            upgrade: self.threshold_upgrade.read(cx).value().to_string(),
            txn: self.threshold_txn.read(cx).value().to_string(),
            updates: self.threshold_updates.read(cx).value().to_string(),
            mid: self.threshold_mid.read(cx).value().to_string(),
            ecc_error: self.threshold_ecc_error.read(cx).value().to_string(),
            ecc_queue: self.threshold_ecc_queue.read(cx).value().to_string(),
            drift: self.threshold_drift.read(cx).value().to_string(),
            rtt: self.threshold_rtt.read(cx).value().to_string(),
        })
    }

    /// Parses the expected-drift field into sorted, deduped ids.
    pub fn expected_drift_result(&self, cx: &App) -> Result<Vec<String>, String> {
        parse_expected_drift_text(&self.expected_drift_field.read(cx).value())
    }

    /// Renders one labelled input row. Pure layout over the caller's entity.
    pub fn field_row(
        caption: &'static str,
        input: &Entity<InputState>,
        cx: &App,
    ) -> gpui::AnyElement {
        div()
            .flex()
            .flex_col()
            .gap(px(4.0))
            .child(
                div()
                    .text_xs()
                    .text_color(cx.theme().muted_foreground)
                    .child(caption),
            )
            .child(Input::new(input))
            .into_any_element()
    }
}

/// Plain field snapshot, detached from entities for validation and RPC.
pub struct SheetValues {
    pub id: String,
    pub label: String,
    pub url: String,
    pub secret_a: String,
    pub secret_b: String,
}

/// Formats a count threshold for the sheet: `u64::MAX` (off) renders empty.
pub fn format_u64_threshold(value: u64) -> String {
    if value == u64::MAX {
        String::new()
    } else {
        value.to_string()
    }
}

/// Formats the RTT ceiling for the sheet: `None` (off) renders empty.
pub fn format_rtt_threshold(value: Option<u64>) -> String {
    value.map(|ms| ms.to_string()).unwrap_or_default()
}

fn parse_count_field(
    caption: &'static str,
    text: &str,
    default: u64,
    off_value: Option<u64>,
) -> Result<u64, String> {
    let trimmed = text.trim();
    if trimmed.is_empty() {
        return Ok(default);
    }
    if trimmed.eq_ignore_ascii_case("off") {
        if let Some(off) = off_value {
            return Ok(off);
        }
        return Err(format!("{caption} does not accept off"));
    }
    trimmed
        .parse::<u64>()
        .map_err(|_| format!("{caption} must be a whole number or empty"))
}

/// The fourteen threshold fields as edited text. One struct instead of
/// fourteen positional arguments.
pub struct ThresholdTexts {
    pub jobs_overdue: String,
    pub jobs_error: String,
    pub syslog: String,
    pub outbound: String,
    pub flow: String,
    pub email: String,
    pub upgrade: String,
    pub txn: String,
    pub updates: String,
    pub mid: String,
    pub ecc_error: String,
    pub ecc_queue: String,
    pub drift: String,
    pub rtt: String,
}

impl ThresholdTexts {
    #[cfg(test)]
    fn blank() -> Self {
        Self {
            jobs_overdue: String::new(),
            jobs_error: String::new(),
            syslog: String::new(),
            outbound: String::new(),
            flow: String::new(),
            email: String::new(),
            upgrade: String::new(),
            txn: String::new(),
            updates: String::new(),
            mid: String::new(),
            ecc_error: String::new(),
            ecc_queue: String::new(),
            drift: String::new(),
            rtt: String::new(),
        }
    }
}

/// Parses the fourteen threshold fields. Empty means default; `jobs_error`,
/// `email`, `updates`, `rtt` and `txn` also accept `off` (never vote).
pub fn parse_thresholds(texts: &ThresholdTexts) -> Result<Thresholds, String> {
    let defaults = Thresholds::default();
    let rtt_trimmed = texts.rtt.trim();
    let availability_rtt_degraded_ms =
        if rtt_trimmed.is_empty() || rtt_trimmed.eq_ignore_ascii_case("off") {
            None
        } else {
            Some(
                rtt_trimmed
                    .parse::<u64>()
                    .map_err(|_| "RTT ceiling must be a whole number of ms, off, or empty")?,
            )
        };
    let txn_trimmed = texts.txn.trim();
    let transaction_avg_degraded_ms =
        if txn_trimmed.is_empty() || txn_trimmed.eq_ignore_ascii_case("off") {
            None
        } else {
            Some(
                txn_trimmed.parse::<u64>().map_err(
                    |_| "Transaction average must be a whole number of ms, off, or empty",
                )?,
            )
        };
    Ok(Thresholds {
        jobs_overdue_degraded_at: parse_count_field(
            "Jobs overdue",
            &texts.jobs_overdue,
            defaults.jobs_overdue_degraded_at,
            None,
        )?,
        jobs_error_degraded_at: parse_count_field(
            "Jobs errors",
            &texts.jobs_error,
            defaults.jobs_error_degraded_at,
            Some(u64::MAX),
        )?,
        syslog_error_degraded_at: parse_count_field(
            "Syslog errors",
            &texts.syslog,
            defaults.syslog_error_degraded_at,
            None,
        )?,
        outbound_failures_degraded_at: parse_count_field(
            "Outbound failures",
            &texts.outbound,
            defaults.outbound_failures_degraded_at,
            None,
        )?,
        flow_error_degraded_at: parse_count_field(
            "Flow errors",
            &texts.flow,
            defaults.flow_error_degraded_at,
            None,
        )?,
        email_failure_degraded_at: parse_count_field(
            "Email failures",
            &texts.email,
            defaults.email_failure_degraded_at,
            Some(u64::MAX),
        )?,
        upgrade_failed_degraded_at: parse_count_field(
            "Failed upgrades",
            &texts.upgrade,
            defaults.upgrade_failed_degraded_at,
            None,
        )?,
        update_sets_open_degraded_at: parse_count_field(
            "Open update sets",
            &texts.updates,
            defaults.update_sets_open_degraded_at,
            Some(u64::MAX),
        )?,
        mid_unhealthy_degraded_at: parse_count_field(
            "Unhealthy MIDs",
            &texts.mid,
            defaults.mid_unhealthy_degraded_at,
            None,
        )?,
        ecc_error_degraded_at: parse_count_field(
            "ECC errors",
            &texts.ecc_error,
            defaults.ecc_error_degraded_at,
            None,
        )?,
        ecc_output_ready_degraded_at: parse_count_field(
            "ECC queue",
            &texts.ecc_queue,
            defaults.ecc_output_ready_degraded_at,
            None,
        )?,
        drift_mismatches_degraded_at: parse_count_field(
            "Drift mismatches",
            &texts.drift,
            defaults.drift_mismatches_degraded_at,
            None,
        )?,
        availability_rtt_degraded_ms,
        transaction_avg_degraded_ms,
    })
}

/// Formats planned drift ids for the single-line sheet field.
pub fn format_expected_drift_text(ids: &[String]) -> String {
    ids.join(", ")
}

/// Parses comma- or newline-separated drift ids: trims, drops empties,
/// rejects whitespace inside a token, sorts and dedupes for stable JSON.
pub fn parse_expected_drift_text(text: &str) -> Result<Vec<String>, String> {
    let mut ids: Vec<String> = text
        .split([',', '\n'])
        .map(str::trim)
        .filter(|token| !token.is_empty())
        .map(str::to_owned)
        .collect();
    for id in &ids {
        if id.chars().any(char::is_whitespace) {
            return Err(format!(
                "expected drift id must not contain whitespace: {id}"
            ));
        }
    }
    ids.sort();
    ids.dedup();
    Ok(ids)
}

/// Assembles the Keychain blob from the two credential fields, or `None`
/// when both are empty (leave stored). Pure and unit-tested.
pub fn credential_blob(auth_method: AuthMethod, first: &str, second: &str) -> Option<String> {
    if first.trim().is_empty() && second.trim().is_empty() {
        return None;
    }
    let (key_a, key_b) = match auth_method {
        AuthMethod::OauthClientCredentials => ("client_id", "client_secret"),
        AuthMethod::Basic => ("username", "password"),
    };
    Some(format!(
        "{{\"{key_a}\":{},\"{key_b}\":{}}}",
        serde_json::Value::String(first.to_owned()),
        serde_json::Value::String(second.to_owned()),
    ))
}

/// Builds the config to save: fresh ids sort last, edits keep sort order,
/// thresholds and expected drift (edited as JSON for now).
pub fn build_config(
    id: String,
    label: String,
    instance_url: String,
    auth_method: AuthMethod,
    clone_source: bool,
    sort_order: i64,
    existing: Option<&EnvironmentConfig>,
) -> EnvironmentConfig {
    EnvironmentConfig {
        id,
        label,
        instance_url,
        auth_method,
        sort_order,
        clone_source,
        thresholds: existing
            .map(|env| env.thresholds.clone())
            .unwrap_or_default(),
        expected_drift: existing
            .map(|env| env.expected_drift.clone())
            .unwrap_or_default(),
    }
}

/// Pre-flight validation of the plain fields. Credential shape is checked
/// separately so an empty pair (leave stored) skips it.
pub fn validate_fields(id: &str, label: &str, instance_url: &str) -> Result<(), String> {
    if id.trim().is_empty() {
        return Err("id must not be empty".to_owned());
    }
    if id.contains(char::is_whitespace) {
        return Err("id must not contain whitespace".to_owned());
    }
    if label.trim().is_empty() {
        return Err("label must not be empty".to_owned());
    }
    if let Some(reason) = daku_protocol::instance_url_error(instance_url) {
        return Err(reason.to_owned());
    }
    Ok(())
}

/// Display names for the auth-method toggle.
pub fn auth_label(auth_method: AuthMethod) -> &'static str {
    match auth_method {
        AuthMethod::OauthClientCredentials => "OAuth client credentials",
        AuthMethod::Basic => "Basic (PDI stand-in only)",
    }
}

/// Field captions for the credential pair.
pub fn credential_captions(auth_method: AuthMethod) -> (&'static str, &'static str) {
    match auth_method {
        AuthMethod::OauthClientCredentials => ("Client ID", "Client secret"),
        AuthMethod::Basic => ("Username", "Password"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use daku_protocol::validate_credential;

    #[test]
    fn empty_credential_pair_leaves_stored() {
        assert_eq!(credential_blob(AuthMethod::Basic, "", "  "), None);
        let blob = credential_blob(AuthMethod::Basic, "reader", "s3cr3t").unwrap();
        assert!(validate_credential(AuthMethod::Basic, &blob).is_ok());
        let blob = credential_blob(AuthMethod::OauthClientCredentials, "id", "secret").unwrap();
        assert!(validate_credential(AuthMethod::OauthClientCredentials, &blob).is_ok());
        // Special characters survive JSON encoding.
        let blob = credential_blob(AuthMethod::Basic, "a\"b", "c\\d").unwrap();
        assert!(validate_credential(AuthMethod::Basic, &blob).is_ok());
        let value: serde_json::Value = serde_json::from_str(&blob).unwrap();
        assert_eq!(value["username"], "a\"b");
    }

    #[test]
    fn half_empty_pair_still_assembles_and_fails_shape() {
        let blob = credential_blob(AuthMethod::Basic, "reader", "").unwrap();
        assert!(validate_credential(AuthMethod::Basic, &blob).is_err());
    }

    #[test]
    fn field_validation_rejects_empties_and_bad_urls() {
        assert!(validate_fields("", "L", "https://x.example.service-now.com").is_err());
        assert!(validate_fields("a b", "L", "https://x.example.service-now.com").is_err());
        assert!(validate_fields("a", "", "https://x.example.service-now.com").is_err());
        assert!(validate_fields("a", "L", "http://x.example.com").is_err());
        assert!(validate_fields("a", "L", "https://x.example.service-now.com").is_ok());
    }

    #[test]
    fn build_config_preserves_tuning_on_edit() {
        let mut existing = build_config(
            "dev".into(),
            "Dev".into(),
            "https://dev.example.service-now.com".into(),
            AuthMethod::Basic,
            false,
            3,
            None,
        );
        existing.thresholds.syslog_error_degraded_at = 10;
        existing.expected_drift = vec!["com.example.staged".into()];
        let edited = build_config(
            "dev".into(),
            "Dev 2".into(),
            "https://dev.example.service-now.com".into(),
            AuthMethod::Basic,
            true,
            3,
            Some(&existing),
        );
        assert_eq!(edited.label, "Dev 2");
        assert!(edited.clone_source);
        assert_eq!(edited.thresholds.syslog_error_degraded_at, 10);
        assert_eq!(edited.expected_drift.len(), 1);
    }

    #[test]
    fn thresholds_empty_means_default_and_off_is_explicit() {
        assert_eq!(
            parse_thresholds(&ThresholdTexts::blank()).unwrap(),
            Thresholds::default()
        );
        let tuned = ThresholdTexts {
            jobs_overdue: "5".into(),
            jobs_error: "off".into(),
            syslog: "10".into(),
            outbound: "5".into(),
            flow: "4".into(),
            email: "3".into(),
            upgrade: "2".into(),
            txn: "250".into(),
            updates: "4".into(),
            mid: "3".into(),
            ecc_error: "2".into(),
            ecc_queue: "500".into(),
            drift: "2".into(),
            rtt: "off".into(),
        };
        let parsed = parse_thresholds(&tuned).unwrap();
        assert_eq!(parsed.jobs_overdue_degraded_at, 5);
        assert_eq!(parsed.jobs_error_degraded_at, u64::MAX);
        assert_eq!(parsed.syslog_error_degraded_at, 10);
        assert_eq!(parsed.flow_error_degraded_at, 4);
        assert_eq!(parsed.email_failure_degraded_at, 3);
        assert_eq!(parsed.upgrade_failed_degraded_at, 2);
        assert_eq!(parsed.transaction_avg_degraded_ms, Some(250));
        assert_eq!(parsed.update_sets_open_degraded_at, 4);
        assert_eq!(parsed.availability_rtt_degraded_ms, None);
        let rtt = ThresholdTexts {
            rtt: "500".into(),
            ..ThresholdTexts::blank()
        };
        assert_eq!(
            parse_thresholds(&rtt).unwrap().availability_rtt_degraded_ms,
            Some(500)
        );
    }

    #[test]
    fn thresholds_reject_garbage_without_guessing() {
        let bad = |jobs_overdue: &str, rtt: &str| ThresholdTexts {
            jobs_overdue: jobs_overdue.into(),
            rtt: rtt.into(),
            ..ThresholdTexts::blank()
        };
        assert!(parse_thresholds(&bad("x", "")).is_err());
        assert!(parse_thresholds(&bad("1.5", "")).is_err());
        assert!(parse_thresholds(&bad("-1", "")).is_err());
        assert!(parse_thresholds(&bad("1", "fast")).is_err());
        // Off is only meaningful where a never-vote state exists.
        assert!(parse_thresholds(&bad("off", "")).is_err());
    }

    #[test]
    fn threshold_formatting_round_trips_through_parse() {
        let tuned = Thresholds {
            syslog_error_degraded_at: 10,
            availability_rtt_degraded_ms: Some(250),
            ..Thresholds::default()
        };
        let parsed = parse_thresholds(&ThresholdTexts {
            jobs_overdue: format_u64_threshold(tuned.jobs_overdue_degraded_at),
            jobs_error: format_u64_threshold(tuned.jobs_error_degraded_at),
            syslog: format_u64_threshold(tuned.syslog_error_degraded_at),
            outbound: format_u64_threshold(tuned.outbound_failures_degraded_at),
            flow: format_u64_threshold(tuned.flow_error_degraded_at),
            email: format_u64_threshold(tuned.email_failure_degraded_at),
            upgrade: format_u64_threshold(tuned.upgrade_failed_degraded_at),
            txn: format_rtt_threshold(tuned.transaction_avg_degraded_ms),
            updates: format_u64_threshold(tuned.update_sets_open_degraded_at),
            mid: format_u64_threshold(tuned.mid_unhealthy_degraded_at),
            ecc_error: format_u64_threshold(tuned.ecc_error_degraded_at),
            ecc_queue: format_u64_threshold(tuned.ecc_output_ready_degraded_at),
            drift: format_u64_threshold(tuned.drift_mismatches_degraded_at),
            rtt: format_rtt_threshold(tuned.availability_rtt_degraded_ms),
        })
        .unwrap();
        assert_eq!(parsed, tuned);
        assert_eq!(format_u64_threshold(u64::MAX), "");
        assert_eq!(format_rtt_threshold(None), "");
    }

    #[test]
    fn expected_drift_splits_trims_sorts_and_dedupes() {
        assert_eq!(parse_expected_drift_text("").unwrap(), Vec::<String>::new());
        assert_eq!(
            parse_expected_drift_text("b, a\nb ,, c").unwrap(),
            vec!["a".to_owned(), "b".to_owned(), "c".to_owned()]
        );
        assert!(parse_expected_drift_text("has space").is_err());
        assert_eq!(
            format_expected_drift_text(&["b".to_owned(), "a".to_owned()]),
            "b, a"
        );
    }
}
