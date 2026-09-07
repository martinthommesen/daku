//! Environment management sheet (080): add / edit / remove Environments,
//! Credential included.
//!
//! Secrets never read back: credential fields always start empty, and an
//! empty pair means "leave the stored Credential untouched". Shape-checking
//! reuses the wire contract (`validate_credential`), so the sheet and the
//! daemon reject the same garbage with the same message.

use daku_protocol::{AuthMethod, EnvironmentConfig};
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
        Self {
            editing_id: None,
            id_field: Self::field(window, cx, "", false),
            label_field: Self::field(window, cx, "", false),
            url_field: Self::field(window, cx, "https://", false),
            auth: AuthMethod::OauthClientCredentials,
            clone_source: false,
            secret_a: Self::field(window, cx, "", false),
            secret_b: Self::field(window, cx, "", true),
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
}
