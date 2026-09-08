//! Test-only (compiled under `#[cfg(test)]`): regenerates every Signal's
//! payload from the code that actually writes it and pins the result to
//! `tests/fixtures/payloads.json` — the file the desktop's
//! `src/dashboard_state.rs` tests render. Change a key on either side and one
//! of the two test suites fails.
//!
//! Re-bless with `DAKU_BLESS_PAYLOADS=1 cargo test -p daku-core payload`.

use std::collections::BTreeMap;
use std::sync::Arc;
use std::time::Duration;

use rusqlite::Connection;
use serde_json::{Value, json};

use crate::availability::{classify_availability_response, persist_availability_snapshot};
use crate::collector::SignalCollector;
use crate::config::{AuthMethod, EnvironmentConfig, MemoryCredentialStore, Thresholds};
use crate::drift::{DRIFT_SIGNAL_ID, DriftCollector};
use crate::email::{EMAIL_SIGNAL_ID, EmailCollector};
use crate::flow::{FLOW_SIGNAL_ID, FlowCollector};
use crate::jobs::{JOBS_SIGNAL_ID, JobsCollector};
use crate::last_clone::{
    CloneRow, LAST_CLONE_SIGNAL_ID, persist_clone_source, persist_clone_target,
};
use crate::mid_ecc::{MID_ECC_SIGNAL_ID, MidEccCollector};
use crate::outbound::{OUTBOUND_SIGNAL_ID, OutboundCollector};
use crate::persistence;
use crate::servicenow::{HttpRequest, HttpResponse, HttpTransport, ServiceNowClient, SystemClock};
use crate::sessions::{SESSIONS_SIGNAL_ID, SessionsCollector};
use crate::syslog::{SYSLOG_SIGNAL_ID, SyslogCollector};
use crate::test_support::{TempDb, prod};
use crate::upgrade::{UPGRADE_SIGNAL_ID, UpgradeCollector};

const PINNED: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/tests/fixtures/payloads.json");

/// 2026-01-27 01:00 UTC. Fixed because `age_days` is the one payload key
/// derived from the clock — with `unix_now()` the pinned file would change
/// value every day.
const OBSERVED_AT: i64 = 1_769_475_600;

/// Serves every endpoint the collectors below reach, keyed by URL, with the
/// same canned bodies daku-core's own collector tests use.
struct ContractTransport;

impl HttpTransport for ContractTransport {
    fn execute(&self, request: &HttpRequest) -> anyhow::Result<HttpResponse> {
        let url = &request.url;
        let source = url.contains("acme-prod");
        let body = if url.contains("glide.war") {
            if source {
                include_str!("../tests/fixtures/availability/ok.json")
            } else {
                include_str!("../tests/fixtures/availability/ok_yokohama.json")
            }
        } else if url.contains("/api/now/table/sys_plugins") {
            if source {
                include_str!("../tests/fixtures/drift/contract_source.json")
            } else {
                include_str!("../tests/fixtures/drift/contract_other.json")
            }
        } else if url.contains("/api/now/table/sys_store_app") {
            include_str!("../tests/fixtures/drift/store_apps_empty.json")
        } else if url.contains("/api/now/stats/sys_trigger") {
            if source && !url.contains("state=3") {
                include_str!("../tests/fixtures/jobs/count_2.json")
            } else {
                include_str!("../tests/fixtures/jobs/count_0.json")
            }
        } else if url.contains("/api/now/stats/syslog") {
            if source {
                include_str!("../tests/fixtures/syslog/count_4.json")
            } else {
                include_str!("../tests/fixtures/syslog/count_0.json")
            }
        } else if url.contains("/api/now/table/sys_trigger") {
            r#"{"result":[
                {"sys_id":"job-1","name":"Nightly sync","state":"0","next_action":"2026-01-27 02:00:00"}
            ]}"#
        } else if url.contains("/api/now/table/syslog") {
            r#"{"result":[
                {"sys_id":"log-1","source":"Scheduled job","message":"Null pointer in transform","level":"2","sys_created_on":"2026-01-27 00:12:00"}
            ]}"#
        } else if url.contains("/api/now/table/sys_outbound_http_log") {
            r#"{"result":[
                {"sys_id":"out-1","url":"https://partner.example.com/hook","http_status":502,"sys_created_on":"2026-01-27 00:12:00"}
            ]}"#
        } else if url.contains("/api/now/stats/sys_outbound_http_log") {
            if source {
                include_str!("../tests/fixtures/outbound/count_3.json")
            } else {
                include_str!("../tests/fixtures/outbound/count_0.json")
            }
        } else if url.contains("/api/now/stats/sys_flow_context") {
            if source {
                include_str!("../tests/fixtures/flow/count_2.json")
            } else {
                include_str!("../tests/fixtures/flow/count_0.json")
            }
        } else if url.contains("/api/now/table/sys_flow_context") {
            r#"{"result":[
                {"sys_id":"flow-1","name":"Sync orders","state":"ERROR","sys_updated_on":"2026-01-27 00:12:00"}
            ]}"#
        } else if url.contains("/api/now/stats/sys_email") {
            if source {
                include_str!("../tests/fixtures/email/count_2.json")
            } else {
                include_str!("../tests/fixtures/email/count_0.json")
            }
        } else if url.contains("/api/now/table/sys_email") {
            r#"{"result":[
                {"sys_id":"email-1","subject":"Approval requested","recipients":"owner@example.com","error_string":"SMTP 550","sys_created_on":"2026-01-27 00:12:00"}
            ]}"#
        } else if url.contains("/api/now/table/sys_upgrade_history") {
            if source {
                r#"{"result":[
                    {"sys_id":"up-1","from_version":"Zurich P0","to_version":"Zurich P1","state":"Failed","upgrade_started":"2099-01-01 01:00:00","upgrade_finished":"2099-01-01 02:00:00"},
                    {"sys_id":"up-0","from_version":"Yokohama","to_version":"Zurich","state":"Completed","upgrade_started":"2020-01-01 01:00:00","upgrade_finished":"2020-01-01 02:00:00"}
                ]}"#
            } else {
                r#"{"result":[
                    {"sys_id":"up-1","from_version":"Zurich P0","to_version":"Zurich P1","state":"Completed","upgrade_started":"2099-01-01 01:00:00","upgrade_finished":"2099-01-01 02:00:00"}
                ]}"#
            }
        } else if url.contains("/api/now/table/v_user_session") {
            if source {
                r#"{"result":[{"sys_id":"s1"},{"sys_id":"s2"},{"sys_id":"s3"}]}"#
            } else {
                r#"{"result":[]}"#
            }
        } else if url.contains("/api/now/table/ecc_agent") {
            if source {
                include_str!("../tests/fixtures/mid_ecc/agents_healthy.json")
            } else {
                include_str!("../tests/fixtures/mid_ecc/agents_mixed.json")
            }
        } else if url.contains("/api/now/stats/ecc_queue") {
            if url.contains("state=error") {
                include_str!("../tests/fixtures/mid_ecc/count_0.json")
            } else {
                include_str!("../tests/fixtures/mid_ecc/count_2.json")
            }
        } else {
            panic!("unexpected URL: {url}");
        };
        Ok(HttpResponse {
            status: 200,
            headers: vec![("content-type".into(), "application/json".into())],
            body: body.into(),
        })
    }
}

fn client() -> ServiceNowClient {
    ServiceNowClient::new(ContractTransport, SystemClock)
}

fn credentials(ids: &[&str]) -> Arc<MemoryCredentialStore> {
    let store = Arc::new(MemoryCredentialStore::default());
    for id in ids {
        store.insert(*id, r#"{"username":"reader","password":"secret"}"#);
    }
    store
}

fn env(id: &str, host: &str, clone_source: bool) -> EnvironmentConfig {
    EnvironmentConfig {
        id: id.into(),
        label: id.into(),
        instance_url: format!("https://{host}.example.service-now.com"),
        auth_method: AuthMethod::Basic,
        sort_order: if clone_source { 0 } else { 1 },
        clone_source,
        thresholds: Thresholds::default(),
        expected_drift: Vec::new(),
    }
}

/// One pinned case: what the daemon persisted for one Signal.
fn case(connection: &Connection, environment_id: &str, signal_id: &str) -> Value {
    let row = persistence::load_signal_snapshot(connection, environment_id, signal_id)
        .unwrap()
        .unwrap_or_else(|| panic!("no {signal_id} snapshot for {environment_id}"));
    json!({
        "signal_id": signal_id,
        "state": row.state,
        "payload": serde_json::from_str::<Value>(&row.payload_json).unwrap(),
    })
}

fn generate() -> BTreeMap<String, Value> {
    let mut cases: BTreeMap<String, Value> = BTreeMap::new();

    // Availability: a pure classifier plus the snapshot writer, one env id per
    // case so the snapshots do not overwrite each other.
    let db = TempDb::new("payload-availability");
    let connection = db.store().open().unwrap();
    for (name, id, status, content_type, body) in [
        (
            "availability_reachable",
            "a-ok",
            200,
            "application/json",
            include_str!("../tests/fixtures/availability/ok.json"),
        ),
        (
            "availability_reachable_other_build",
            "a-ok2",
            200,
            "application/json",
            include_str!("../tests/fixtures/availability/ok_yokohama.json"),
        ),
        (
            "availability_asleep",
            "a-asleep",
            200,
            "text/html",
            include_str!("../tests/fixtures/availability/hibernating.html"),
        ),
        (
            "availability_unreachable",
            "a-429",
            429,
            "application/json",
            include_str!("../tests/fixtures/availability/401.json"),
        ),
    ] {
        let observation = classify_availability_response(status, content_type, body, 142);
        persist_availability_snapshot(&connection, id, &observation, OBSERVED_AT).unwrap();
        cases.insert(name.into(), case(&connection, id, "availability"));
    }
    drop(connection);

    // The four counting Signals, through their real collectors, over two
    // Environments so both a loaded and a quiet reading get pinned.
    let db = TempDb::new("payload-counts");
    let environments = vec![prod(), env("test", "acme-test", false)];
    let store = credentials(&["prod", "test"]);
    for collector in [
        Box::new(JobsCollector::new(
            environments.clone(),
            store.clone(),
            client(),
            db.store(),
        )) as Box<dyn SignalCollector>,
        Box::new(SyslogCollector::new(
            environments.clone(),
            store.clone(),
            client(),
            db.store(),
        )),
        Box::new(OutboundCollector::new(
            environments.clone(),
            store.clone(),
            client(),
            db.store(),
        )),
        Box::new(FlowCollector::new(
            environments.clone(),
            store.clone(),
            client(),
            db.store(),
        )),
        Box::new(EmailCollector::new(
            environments.clone(),
            store.clone(),
            client(),
            db.store(),
        )),
        Box::new(UpgradeCollector::new(
            environments.clone(),
            store.clone(),
            client(),
            db.store(),
        )),
        Box::new(SessionsCollector::new(
            environments.clone(),
            store.clone(),
            client(),
            db.store(),
        )),
        Box::new(MidEccCollector::new(
            environments.clone(),
            store.clone(),
            client(),
            db.store(),
        )),
    ] {
        collector.collect().unwrap();
    }
    let connection = db.store().open().unwrap();
    for (name, id, signal_id) in [
        ("jobs_counts", "prod", JOBS_SIGNAL_ID),
        ("jobs_zero", "test", JOBS_SIGNAL_ID),
        ("syslog_count", "prod", SYSLOG_SIGNAL_ID),
        ("syslog_zero", "test", SYSLOG_SIGNAL_ID),
        ("outbound_count", "prod", OUTBOUND_SIGNAL_ID),
        ("outbound_zero", "test", OUTBOUND_SIGNAL_ID),
        ("flow_count", "prod", FLOW_SIGNAL_ID),
        ("flow_zero", "test", FLOW_SIGNAL_ID),
        ("email_count", "prod", EMAIL_SIGNAL_ID),
        ("email_zero", "test", EMAIL_SIGNAL_ID),
        ("upgrade_failed", "prod", UPGRADE_SIGNAL_ID),
        ("upgrade_clean", "test", UPGRADE_SIGNAL_ID),
        ("sessions_count", "prod", SESSIONS_SIGNAL_ID),
        ("sessions_zero", "test", SESSIONS_SIGNAL_ID),
        ("mid_ecc_healthy", "prod", MID_ECC_SIGNAL_ID),
        ("mid_ecc_unhealthy", "test", MID_ECC_SIGNAL_ID),
    ] {
        cases.insert(name.into(), case(&connection, id, signal_id));
    }
    drop(connection);

    // Drift: the source's own snapshot and one target's comparison, from the
    // collector — differing plugin inventories and differing builds.
    let db = TempDb::new("payload-drift");
    DriftCollector::new(
        vec![
            env("prod", "acme-prod", true),
            env("test", "acme-test", false),
        ],
        credentials(&["prod", "test"]),
        client(),
        db.store(),
        Duration::from_secs(120),
    )
    .collect()
    .unwrap();
    let connection = db.store().open().unwrap();
    cases.insert(
        "drift_source".into(),
        case(&connection, "prod", DRIFT_SIGNAL_ID),
    );
    cases.insert(
        "drift_compare".into(),
        case(&connection, "test", DRIFT_SIGNAL_ID),
    );
    drop(connection);

    // Last clone: the collector's writers, called with a fixed `observed_at`
    // (it takes its own from `unix_now()`, which `age_days` would follow).
    let db = TempDb::new("payload-last-clone");
    let connection = db.store().open().unwrap();
    persist_clone_source(&connection, "ls-yes", true, OBSERVED_AT).unwrap();
    persist_clone_source(&connection, "ls-no", false, OBSERVED_AT).unwrap();
    let row = CloneRow {
        target: "acme-test".into(),
        completed: "2026-01-15 12:00:00".into(),
    };
    persist_clone_target(
        &connection,
        "ls-done",
        Some(&row),
        "prod",
        false,
        OBSERVED_AT,
    )
    .unwrap();
    persist_clone_target(&connection, "ls-never", None, "prod", false, OBSERVED_AT).unwrap();
    persist_clone_target(&connection, "ls-page", None, "prod", true, OBSERVED_AT).unwrap();
    for (name, id) in [
        ("last_clone_source_supported", "ls-yes"),
        ("last_clone_source_cannot_list", "ls-no"),
        ("last_clone_target_completed", "ls-done"),
        ("last_clone_target_never", "ls-never"),
        ("last_clone_target_older_than_page", "ls-page"),
    ] {
        cases.insert(name.into(), case(&connection, id, LAST_CLONE_SIGNAL_ID));
    }
    drop(connection);

    // The two shapes every Signal shares. `persist_signal_skipped` is the sole
    // writer of `skipped`, so this pins the shape plus the reason vocabulary
    // the collectors pass it today (grep `persist_signal_skipped` /
    // `skip_targets` for the callers).
    let db = TempDb::new("payload-shared");
    let connection = db.store().open().unwrap();
    for (name, signal_id, reason) in [
        ("skipped_asleep", JOBS_SIGNAL_ID, "asleep"),
        ("skipped_unreachable", JOBS_SIGNAL_ID, "unreachable"),
        (
            "skipped_need_two_environments",
            DRIFT_SIGNAL_ID,
            "need_two_environments",
        ),
        (
            "skipped_no_clone_source",
            DRIFT_SIGNAL_ID,
            "no_clone_source",
        ),
        (
            "skipped_clone_source_cannot_list_clones",
            LAST_CLONE_SIGNAL_ID,
            "clone_source_cannot_list_clones",
        ),
        (
            "skipped_clone_source_asleep",
            LAST_CLONE_SIGNAL_ID,
            "clone_source_asleep",
        ),
        (
            "skipped_clone_source_unreachable",
            LAST_CLONE_SIGNAL_ID,
            "clone_source_unreachable",
        ),
    ] {
        let id = format!("s-{name}");
        persistence::persist_signal_skipped(&connection, &id, signal_id, OBSERVED_AT, reason)
            .unwrap();
        cases.insert(name.into(), case(&connection, &id, signal_id));
    }
    persistence::persist_signal_down(
        &connection,
        "d-1",
        OUTBOUND_SIGNAL_ID,
        OBSERVED_AT,
        "HTTP 429",
    )
    .unwrap();
    cases.insert(
        "down_probe_failed".into(),
        case(&connection, "d-1", OUTBOUND_SIGNAL_ID),
    );

    cases
}

#[test]
fn pinned_payloads_match_what_the_collectors_write() {
    let generated = generate();
    // The one payload key derived from the clock; a drifting fixture must
    // fail here and now, not on some future date.
    assert_eq!(
        generated["last_clone_target_completed"]["payload"]["age_days"],
        12
    );
    let generated_value = serde_json::to_value(&generated).unwrap();
    if std::env::var("DAKU_BLESS_PAYLOADS").as_deref() == Ok("1") {
        std::fs::write(
            PINNED,
            format!(
                "{}\n",
                serde_json::to_string_pretty(&generated_value).unwrap()
            ),
        )
        .unwrap();
    }
    let pinned: BTreeMap<String, Value> =
        serde_json::from_str(&std::fs::read_to_string(PINNED).unwrap()).unwrap();
    let stale = "tests/fixtures/payloads.json is stale — re-bless with \
                 DAKU_BLESS_PAYLOADS=1 cargo test -p daku-core payload, then update \
                 the expectations in src/dashboard_state.rs";
    assert_eq!(
        pinned.keys().collect::<Vec<_>>(),
        generated.keys().collect::<Vec<_>>(),
        "{stale}"
    );
    for (name, case) in &generated {
        assert_eq!(pinned.get(name), Some(case), "{name}: {stale}");
    }
}

/// Every known Signal must have at least one pinned case, so a new Signal
/// cannot land without fixture data for the desktop's `fixture_events()`.
#[test]
fn every_known_signal_has_a_pinned_case() {
    use crate::availability::AVAILABILITY_SIGNAL_ID;
    use crate::drift::DRIFT_SIGNAL_ID;
    use crate::email::EMAIL_SIGNAL_ID;
    use crate::flow::FLOW_SIGNAL_ID;
    use crate::jobs::JOBS_SIGNAL_ID;
    use crate::last_clone::LAST_CLONE_SIGNAL_ID;
    use crate::mid_ecc::MID_ECC_SIGNAL_ID;
    use crate::outbound::OUTBOUND_SIGNAL_ID;
    use crate::sessions::SESSIONS_SIGNAL_ID;
    use crate::syslog::SYSLOG_SIGNAL_ID;
    use crate::upgrade::UPGRADE_SIGNAL_ID;

    const KNOWN: [&str; 11] = [
        AVAILABILITY_SIGNAL_ID,
        JOBS_SIGNAL_ID,
        SYSLOG_SIGNAL_ID,
        MID_ECC_SIGNAL_ID,
        OUTBOUND_SIGNAL_ID,
        FLOW_SIGNAL_ID,
        EMAIL_SIGNAL_ID,
        UPGRADE_SIGNAL_ID,
        SESSIONS_SIGNAL_ID,
        DRIFT_SIGNAL_ID,
        LAST_CLONE_SIGNAL_ID,
    ];
    let generated = generate();
    let covered: std::collections::HashSet<&str> = generated
        .values()
        .filter_map(|case| case.get("signal_id")?.as_str())
        .collect();
    for signal_id in KNOWN {
        assert!(
            covered.contains(signal_id),
            "no pinned payload case for signal {signal_id:?} — add one in generate()"
        );
    }
}
