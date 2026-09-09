//! Typed Signal payload seam.
//!
//! Collectors write JSON; the Environment detail, Signal card, Drill-in,
//! and Compare strip used to key off the same strings in two places with
//! no compile-time seam — a renamed key rendered as an empty summary.
//! This module is that seam: the wire stays JSON, but every reader goes
//! through `parse` once. A shape change breaks here, not in six render
//! paths.

use serde_json::Value;

use crate::Reachability;

/// What one snapshot payload means, typed by Signal id.
/// Unknown ids and malformed bodies read as `Unknown`/`Skipped`/`Down`
/// rather than panicking — the daemon's quirks stay encoded once, here.
#[derive(Debug, Clone, PartialEq)]
pub enum TypedPayload {
    Availability {
        reachability: Reachability,
        build: Option<String>,
        rtt_ms: Option<u64>,
        error: Option<String>,
    },
    Drift {
        role: Option<String>,
        build_matches: Option<bool>,
        mismatched: u64,
        expected: u64,
    },
    Count {
        count: u64,
    },
    Skipped {
        reason: String,
    },
    Down {
        reachability: String,
        detail: Option<String>,
    },
    Unknown,
}

fn text(value: &Value, key: &str) -> Option<String> {
    value
        .get(key)
        .and_then(|item| item.as_str())
        .filter(|text| !text.is_empty())
        .map(str::to_owned)
}

fn count(value: &Value, keys: &[&str]) -> Option<u64> {
    keys.iter().find_map(|key| {
        value.get(*key).and_then(|item| {
            item.as_u64()
                .or_else(|| item.as_str().and_then(|text| text.parse::<u64>().ok()))
        })
    })
}

/// Reachability from an availability payload; unparseable reads Reachable
/// (the historical default in `health::wire_reachability`).
pub fn parse_reachability(payload: &Value) -> Reachability {
    payload
        .get("reachability")
        .and_then(|item| item.as_str())
        .and_then(Reachability::parse)
        .unwrap_or(Reachability::Reachable)
}

/// Build string from an availability payload, if the probe read one.
pub fn parse_build(payload: &Value) -> Option<String> {
    text(payload, "build")
}

/// Parse one snapshot payload behind the seam.
pub fn parse(signal_id: &str, payload: &Value) -> TypedPayload {
    if let Some(reason) = text(payload, "skipped") {
        return TypedPayload::Skipped { reason };
    }
    if payload.get("reachability") == Some(&Value::String("unreachable".into()))
        && text(payload, "build").is_none()
        && signal_id != "availability"
    {
        // Down snapshots carry `reachability: unreachable` + `detail`;
        // availability itself owns the canonical shape below.
        let detail = text(payload, "detail").or_else(|| text(payload, "error"));
        if payload.get("skipped").is_none()
            && count(payload, &["count"]).is_none()
            && !has_known_count_key(signal_id, payload)
        {
            return TypedPayload::Down {
                reachability: "unreachable".into(),
                detail,
            };
        }
    }
    match signal_id {
        "availability" | "http_probe" => TypedPayload::Availability {
            reachability: parse_reachability(payload),
            build: parse_build(payload),
            rtt_ms: payload.get("rtt_ms").and_then(|item| item.as_u64()),
            error: text(payload, "error").or_else(|| text(payload, "detail")),
        },
        "drift" => TypedPayload::Drift {
            role: text(payload, "role"),
            build_matches: payload.get("build_matches").and_then(|item| item.as_bool()),
            mismatched: payload
                .get("mismatches")
                .and_then(|item| item.as_u64())
                .or_else(|| {
                    payload
                        .get("mismatches")
                        .and_then(|item| item.as_array())
                        .map(|rows| rows.len() as u64)
                })
                .unwrap_or(0),
            expected: payload
                .get("expected")
                .and_then(|item| item.as_array())
                .map(|rows| rows.len() as u64)
                .or_else(|| payload.get("expected_count").and_then(|item| item.as_u64()))
                .unwrap_or(0),
        },
        id => {
            if let Some(n) = count_for(id, payload) {
                TypedPayload::Count { count: n }
            } else {
                TypedPayload::Unknown
            }
        }
    }
}

fn has_known_count_key(signal_id: &str, payload: &Value) -> bool {
    count_for(signal_id, payload).is_some()
}

/// Primary count per Signal id — the number the card summarizes.
fn count_for(signal_id: &str, payload: &Value) -> Option<u64> {
    match signal_id {
        "jobs" => count(payload, &["overdue_ready", "error", "error_count", "count"]).map(|_| {
            payload
                .get("overdue_ready")
                .and_then(|item| item.as_u64())
                .unwrap_or(0)
                + payload
                    .get("error")
                    .and_then(|item| item.as_u64())
                    .unwrap_or(0)
        }),
        "syslog" => count(payload, &["error_count_1h", "count"]),
        "outbound" => count(payload, &["outbound_http_4xx_5xx_1h", "count"]),
        "flow" => count(payload, &["flow_error_1h", "count"]),
        "email" => count(payload, &["email_failed_1h", "count"]),
        "mid_ecc" => count(payload, &["agents_unhealthy", "ecc_error", "count"]),
        "upgrade" => count(payload, &["failed_7d", "count"]),
        "update_sets" => count(payload, &["open_count", "count"]),
        "scan" => count(payload, &["p1_open", "count"]),
        "actions" => count(payload, &["failed_24h", "count"]),
        "slow_txn" => payload
            .get("avg_ms")
            .and_then(|item| item.as_f64())
            .map(|avg| avg as u64)
            .or_else(|| count(payload, &["count"])),
        _ => count(payload, &["count", "open_count", "failed_7d", "failed_24h"]),
    }
}

/// Drift role: `Some("source")` when this Environment is the clone source.
pub fn drift_role(payload: &Value) -> Option<String> {
    text(payload, "role")
}

/// Whether the drift payload reports a build mismatch: an explicit
/// `build_matches: false`, or a non-zero `mismatches` count/array.
/// Unknown builds never mismatch — quiet by design.
pub fn drift_mismatch(payload: &Value) -> bool {
    payload.get("build_matches") == Some(&Value::Bool(false))
        || payload
            .get("mismatches")
            .and_then(|item| item.as_u64())
            .is_some_and(|count| count > 0)
        || payload
            .get("mismatches")
            .and_then(|item| item.as_array())
            .is_some_and(|rows| !rows.is_empty())
}

/// Skipped reason, if this payload is a skip marker.
pub fn skipped_reason(payload: &Value) -> Option<String> {
    text(payload, "skipped")
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn availability_defaults_to_reachable() {
        assert_eq!(parse_reachability(&json!({})), Reachability::Reachable);
        assert_eq!(
            parse_reachability(&json!({"reachability": "asleep"})),
            Reachability::Asleep
        );
        assert_eq!(parse_build(&json!({"build": "v1"})), Some("v1".into()));
        assert_eq!(parse_build(&json!({"build": ""})), None);
    }

    #[test]
    fn drift_helpers() {
        let source = json!({"role": "source", "build_matches": true});
        assert_eq!(drift_role(&source), Some("source".into()));
        assert!(!drift_mismatch(&source));
        assert!(drift_mismatch(&json!({"build_matches": false})));
        assert!(!drift_mismatch(&json!({})));
    }

    #[test]
    fn parse_covers_known_signals() {
        assert!(matches!(
            parse("availability", &json!({"reachability": "reachable"})),
            TypedPayload::Availability { .. }
        ));
        assert!(matches!(
            parse("drift", &json!({"role": "source"})),
            TypedPayload::Drift { .. }
        ));
        assert!(matches!(
            parse("jobs", &json!({"overdue_ready": 2, "error": 0})),
            TypedPayload::Count { count: 2 }
        ));
        assert!(matches!(
            parse("syslog", &json!({"skipped": "asleep"})),
            TypedPayload::Skipped { .. }
        ));
        assert!(matches!(
            parse("jobs", &json!({"foo": 1})),
            TypedPayload::Unknown
        ));
    }
}
