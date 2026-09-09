//! Shared Signal evaluation helpers.
//!
//! The fifteen Signal modules were shallow copies of the same shape:
//! `row_text` + `fetch_*_rows` (request, `status != 200 => empty`, parse
//! `result` array, `take_bounded`) + `*_state(count, &Thresholds)`.
//! This module is the deep seam behind them: one place where row text,
//! URL redaction, `result`-array parsing, threshold comparison, and the
//! tolerant/bailing table fetch live. Per-Signal adapters keep only their
//! query path and field mapping.

use daku_protocol::SignalState;

use crate::collector::take_bounded;
use crate::config::{CredentialStore, EnvironmentConfig};
use crate::servicenow::ServiceNowClient;

/// Text of one field: strings verbatim, numbers stringified, else empty.
/// Superset of the old string-only variant — number-aware everywhere.
pub fn row_text(row: &serde_json::Value, key: &str) -> String {
    match row.get(key) {
        Some(serde_json::Value::String(text)) => text.clone(),
        Some(value) if value.is_number() => value.to_string(),
        _ => String::new(),
    }
}

/// Keeps scheme + host + path; drops query and fragment, which can carry
/// third-party secrets or session tokens the drill-in never needs.
pub fn redact_url(url: &str) -> String {
    let without_fragment = url.split('#').next().unwrap_or("");
    without_fragment.split('?').next().unwrap_or("").to_owned()
}

/// Tolerant `result`-array parse: malformed bodies read as no rows.
/// The count already determined the state; rows only back the drill-in.
pub fn parse_result_array(body: &str) -> Vec<serde_json::Value> {
    serde_json::from_slice::<serde_json::Value>(body.as_bytes())
        .ok()
        .and_then(|value| {
            value
                .get("result")
                .and_then(|result| result.as_array())
                .cloned()
        })
        .unwrap_or_default()
}

/// Strict `result`-array parse: callers that count from the rows themselves
/// (update sets, scan) bail when the shape is missing.
pub fn parse_result_array_required(body: &str) -> anyhow::Result<Vec<serde_json::Value>> {
    serde_json::from_slice::<serde_json::Value>(body.as_bytes())
        .ok()
        .and_then(|value| {
            value
                .get("result")
                .and_then(|result| result.as_array())
                .cloned()
        })
        .ok_or_else(|| anyhow::anyhow!("response has no result array"))
}

/// `count >= degraded_at` reads degraded, else healthy.
/// Every counting Signal shares this; availability RTT and HTTP-probe
/// status keep their own variants.
pub fn evaluate_ge(count: u64, degraded_at: u64) -> SignalState {
    if count >= degraded_at {
        SignalState::Degraded
    } else {
        SignalState::Healthy
    }
}

/// Tolerant table fetch: transport error or non-200 yields no rows (not an
/// error) — the count already determined the state. Maps each `result`
/// entry through `map`, drops `None`, bounds to `ROW_LIST_LIMIT`.
/// A 429 still yields no rows here, but callers that need visible pressure
/// should use `fetch_table_rows_with_throttle` and surface `throttled`.
pub fn fetch_table_rows<T, F>(
    client: &ServiceNowClient,
    environment: &EnvironmentConfig,
    credentials: &dyn CredentialStore,
    path: &str,
    map: F,
) -> (Vec<T>, bool)
where
    F: Fn(&serde_json::Value) -> Option<T>,
{
    fetch_table_rows_with_throttle(client, environment, credentials, path, map).0
}

/// Tolerant fetch that also reports whether the rows request was rate
/// limited (transport succeeded with 429 after the client retry budget).
/// Returns `((rows, truncated), throttled)`.
pub fn fetch_table_rows_with_throttle<T, F>(
    client: &ServiceNowClient,
    environment: &EnvironmentConfig,
    credentials: &dyn CredentialStore,
    path: &str,
    map: F,
) -> ((Vec<T>, bool), bool)
where
    F: Fn(&serde_json::Value) -> Option<T>,
{
    let Ok(response) = client.request(environment, credentials, "GET", path, None) else {
        return ((Vec::new(), false), false);
    };
    if crate::servicenow::is_rate_limited(response.status) {
        return ((Vec::new(), false), true);
    }
    if response.status != 200 {
        return ((Vec::new(), false), false);
    }
    let rows: Vec<T> = parse_result_array(&response.body)
        .iter()
        .filter_map(map)
        .collect();
    (take_bounded(rows), false)
}

/// Bailing table fetch: transport error or non-200 is an error (the rows
/// are the count here). Missing `result` array also bails.
pub fn fetch_table_rows_required<T, F>(
    client: &ServiceNowClient,
    environment: &EnvironmentConfig,
    credentials: &dyn CredentialStore,
    path: &str,
    map: F,
) -> anyhow::Result<(Vec<T>, bool)>
where
    F: Fn(&serde_json::Value) -> Option<T>,
{
    let response = client.request(environment, credentials, "GET", path, None)?;
    if crate::servicenow::is_rate_limited(response.status) {
        anyhow::bail!("{}", crate::servicenow::THROTTLED_DETAIL);
    }
    if response.status != 200 {
        anyhow::bail!("HTTP {}", response.status);
    }
    let rows: Vec<T> = parse_result_array_required(&response.body)?
        .iter()
        .filter_map(map)
        .collect();
    Ok(take_bounded(rows))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn evaluate_ge_boundary() {
        assert_eq!(evaluate_ge(0, 1), SignalState::Healthy);
        assert_eq!(evaluate_ge(1, 1), SignalState::Degraded);
        assert_eq!(evaluate_ge(41, u64::MAX), SignalState::Healthy);
    }

    #[test]
    fn row_text_handles_strings_numbers_missing() {
        let row = serde_json::json!({"a": "x", "b": 500, "c": true});
        assert_eq!(row_text(&row, "a"), "x");
        assert_eq!(row_text(&row, "b"), "500");
        assert_eq!(row_text(&row, "c"), "");
        assert_eq!(row_text(&row, "missing"), "");
    }

    #[test]
    fn redact_url_drops_query_and_fragment() {
        assert_eq!(
            redact_url("https://partner.example.com/hook?token=secret#frag"),
            "https://partner.example.com/hook"
        );
        assert_eq!(
            redact_url("https://status.example.com/health"),
            "https://status.example.com/health"
        );
    }

    #[test]
    fn parse_result_array_tolerates_garbage() {
        assert!(parse_result_array("not json").is_empty());
        assert!(parse_result_array(r#"{"result": {}}"#).is_empty());
        assert_eq!(parse_result_array(r#"{"result": [{"a": 1}]}"#).len(), 1);
        assert!(parse_result_array_required("nope").is_err());
    }

    #[test]
    fn rate_limited_status_maps_to_throttled_detail() {
        use crate::servicenow::{THROTTLED_DETAIL, is_rate_limited};
        assert!(is_rate_limited(429));
        assert!(!is_rate_limited(200));
        assert!(!is_rate_limited(500));
        assert!(THROTTLED_DETAIL.contains("429"));
    }

    #[test]
    fn tolerant_fetch_reports_throttled_flag() {
        use crate::config::MemoryCredentialStore;
        use crate::servicenow::ServiceNowClient;
        use crate::test_support::{MockTransport, NoSleepClock, canned, prod};
        use std::sync::Arc;

        // One request costs three transport executions (initial + 2 retries).
        let transport = Arc::new(MockTransport::new(vec![
            canned::rate_limited(),
            canned::rate_limited(),
            canned::rate_limited(),
        ]));
        let credentials = MemoryCredentialStore::default();
        credentials.insert("prod", r#"{"username":"reader","password":"secret"}"#);
        let client = ServiceNowClient::new(transport, NoSleepClock);
        let environment = prod();
        let ((_rows, _truncated), throttled): ((Vec<String>, bool), bool) =
            fetch_table_rows_with_throttle(
                &client,
                &environment,
                &credentials,
                "/api/now/table/sys_trigger",
                |row| {
                    row.get("sys_id")
                        .and_then(|v| v.as_str())
                        .map(str::to_owned)
                },
            );
        assert!(throttled, "429 must surface as throttled, not silent empty");
    }

    #[test]
    fn bailing_fetch_carries_throttled_detail() {
        use crate::config::MemoryCredentialStore;
        use crate::servicenow::ServiceNowClient;
        use crate::test_support::{MockTransport, NoSleepClock, canned, prod};
        use std::sync::Arc;

        let transport = Arc::new(MockTransport::new(vec![
            canned::rate_limited(),
            canned::rate_limited(),
            canned::rate_limited(),
        ]));
        let credentials = MemoryCredentialStore::default();
        credentials.insert("prod", r#"{"username":"reader","password":"secret"}"#);
        let client = ServiceNowClient::new(transport, NoSleepClock);
        let environment = prod();
        assert!(
            fetch_table_rows_required::<String, _>(
                &client,
                &environment,
                &credentials,
                "/api/now/table/sys_trigger",
                |row| row
                    .get("sys_id")
                    .and_then(|v| v.as_str())
                    .map(str::to_owned),
            )
            .unwrap_err()
            .to_string()
            .contains("429"),
            "bailing fetch must carry throttled detail"
        );
    }
}
