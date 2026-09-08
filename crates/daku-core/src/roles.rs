//! Least-privilege role checker: `daku-daemon doctor --check-roles`.
//!
//! Reads one row per watched table and reports granted/denied per
//! Environment, with the minimal role each table needs and the Signals that
//! need it. Read-only (a single `sys_id` row per table); writes nothing.
//!
//! Role minimums come from `docs/research/servicenow-signals.md`. Where the
//! docs hedge, the map says so (`itil (varies)`): the check reports what the
//! instance answered, and the role column is guidance, not a verdict.

use std::sync::Arc;

use crate::config::{CredentialStore, EnvironmentConfig};
use crate::servicenow::ServiceNowClient;

/// One table probe: what to read, the least role that reads it, and the
/// Signals that stop working without it.
pub struct TableCheck {
    pub table: &'static str,
    pub min_role: &'static str,
    pub signals: &'static [&'static str],
}

pub const TABLE_CHECKS: &[TableCheck] = &[
    TableCheck {
        table: "sys_properties",
        min_role: "admin",
        signals: &["availability", "drift"],
    },
    TableCheck {
        table: "sys_trigger",
        min_role: "admin",
        signals: &["jobs"],
    },
    TableCheck {
        table: "syslog",
        min_role: "admin",
        signals: &["syslog", "table_growth"],
    },
    TableCheck {
        table: "ecc_agent",
        min_role: "mid_server",
        signals: &["mid_ecc"],
    },
    TableCheck {
        table: "ecc_queue",
        min_role: "mid_server",
        signals: &["mid_ecc", "table_growth"],
    },
    TableCheck {
        table: "sys_outbound_http_log",
        min_role: "admin",
        signals: &["outbound"],
    },
    TableCheck {
        table: "sys_flow_context",
        min_role: "admin",
        signals: &["flow"],
    },
    TableCheck {
        table: "sys_email",
        min_role: "admin",
        signals: &["email", "table_growth"],
    },
    TableCheck {
        table: "sys_upgrade_history",
        min_role: "admin",
        signals: &["upgrade"],
    },
    TableCheck {
        table: "v_user_session",
        min_role: "admin",
        signals: &["sessions"],
    },
    TableCheck {
        table: "sys_update_set",
        min_role: "admin",
        signals: &["update_sets"],
    },
    TableCheck {
        table: "scan_finding",
        min_role: "scan_user",
        signals: &["scan"],
    },
    TableCheck {
        table: "sys_plugins",
        min_role: "admin",
        signals: &["drift"],
    },
    TableCheck {
        table: "sys_store_app",
        min_role: "admin",
        signals: &["drift"],
    },
    TableCheck {
        table: "clone_instance",
        min_role: "clone_admin",
        signals: &["last_clone"],
    },
    TableCheck {
        table: "syslog_transaction",
        min_role: "admin",
        signals: &["slow_txn"],
    },
    TableCheck {
        table: "sys_attachment",
        min_role: "admin",
        signals: &["table_growth"],
    },
    TableCheck {
        table: "task",
        min_role: "itil (varies)",
        signals: &["table_growth"],
    },
];

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TableAccess {
    Granted,
    Denied,
    Error(String),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TableResult {
    pub table: &'static str,
    pub min_role: &'static str,
    pub signals: Vec<String>,
    pub access: TableAccess,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RoleReport {
    pub environment_id: String,
    pub label: String,
    pub tables: Vec<TableResult>,
}

impl RoleReport {
    pub fn denied(&self) -> Vec<&TableResult> {
        self.tables
            .iter()
            .filter(|row| row.access == TableAccess::Denied)
            .collect()
    }
}

/// One-line summary per Environment:
/// `prod roles: 16 granted · 2 denied (syslog, sys_trigger)`.
pub fn format_role_report(report: &RoleReport) -> String {
    let granted = report
        .tables
        .iter()
        .filter(|row| row.access == TableAccess::Granted)
        .count();
    let denied: Vec<&str> = report.denied().iter().map(|row| row.table).collect();
    if denied.is_empty() {
        format!("{} roles: {} granted", report.environment_id, granted)
    } else {
        format!(
            "{} roles: {} granted · {} denied ({})",
            report.environment_id,
            granted,
            denied.len(),
            denied.join(", ")
        )
    }
}

pub fn run_role_check(
    environments: &[EnvironmentConfig],
    credentials: Arc<dyn CredentialStore>,
    client: &ServiceNowClient,
) -> Vec<RoleReport> {
    environments
        .iter()
        .map(|environment| {
            let tables = TABLE_CHECKS
                .iter()
                .map(|check| {
                    let path = format!(
                        "/api/now/table/{}?sysparm_fields=sys_id&sysparm_limit=1",
                        check.table
                    );
                    let access =
                        match client.request(environment, credentials.as_ref(), "GET", &path, None)
                        {
                            Ok(response) if response.status == 200 => TableAccess::Granted,
                            Ok(response) if response.status == 401 || response.status == 403 => {
                                TableAccess::Denied
                            }
                            Ok(response) => TableAccess::Error(format!("HTTP {}", response.status)),
                            Err(error) => TableAccess::Error(error.to_string()),
                        };
                    TableResult {
                        table: check.table,
                        min_role: check.min_role,
                        signals: check
                            .signals
                            .iter()
                            .map(|signal| (*signal).to_owned())
                            .collect(),
                        access,
                    }
                })
                .collect();
            RoleReport {
                environment_id: environment.id.clone(),
                label: environment.label.clone(),
                tables,
            }
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::MemoryCredentialStore;
    use crate::servicenow::{Clock, HttpRequest, HttpResponse, HttpTransport};
    use crate::test_support::prod;
    use std::time::{Duration, SystemTime};

    struct NoSleepClock;

    impl Clock for NoSleepClock {
        fn now(&self) -> SystemTime {
            SystemTime::UNIX_EPOCH + Duration::from_secs(1_700_000_000)
        }

        fn sleep(&self, _: Duration) {}
    }

    /// Grants everything except the denied tables (403); errors nothing.
    struct ScriptedAccess {
        denied: Vec<&'static str>,
    }

    impl HttpTransport for ScriptedAccess {
        fn execute(&self, request: &HttpRequest) -> anyhow::Result<HttpResponse> {
            let table = request
                .url
                .split("/api/now/table/")
                .nth(1)
                .and_then(|rest| rest.split('?').next())
                .unwrap_or("");
            assert!(
                request.url.contains("sysparm_limit=1"),
                "role checks read one row: {}",
                request.url
            );
            if self.denied.contains(&table) {
                return Ok(HttpResponse {
                    status: 403,
                    headers: Vec::new(),
                    body: r#"{"error":{"message":"Operation not allowed"}}"#.into(),
                });
            }
            Ok(HttpResponse {
                status: 200,
                headers: vec![("content-type".into(), "application/json".into())],
                body: r#"{"result":[{"sys_id":"abc"}]}"#.into(),
            })
        }
    }

    fn check(denied: Vec<&'static str>) -> Vec<RoleReport> {
        let credentials = Arc::new(MemoryCredentialStore::default());
        credentials.insert("prod", r#"{"username":"reader","password":"secret"}"#);
        let client = ServiceNowClient::new(ScriptedAccess { denied }, NoSleepClock);
        run_role_check(&[prod()], credentials, &client)
    }

    #[test]
    fn role_check_reports_granted_and_denied_tables() {
        let reports = check(vec!["sys_trigger", "syslog"]);
        assert_eq!(reports.len(), 1);
        let report = &reports[0];
        assert_eq!(report.tables.len(), TABLE_CHECKS.len());
        let denied = report.denied();
        assert_eq!(denied.len(), 2);
        let trigger = denied
            .iter()
            .find(|row| row.table == "sys_trigger")
            .unwrap();
        assert_eq!(trigger.min_role, "admin");
        assert_eq!(trigger.signals, vec!["jobs".to_owned()]);
        assert!(format_role_report(report).contains("2 denied (sys_trigger, syslog)"));
    }

    #[test]
    fn role_check_clean_environments_read_all_granted() {
        let reports = check(Vec::new());
        assert!(reports[0].denied().is_empty());
        assert!(format_role_report(&reports[0]).contains("granted"));
        assert!(!format_role_report(&reports[0]).contains("denied"));
    }

    #[test]
    fn role_check_transport_errors_are_errors_not_denials() {
        struct Offline;
        impl HttpTransport for Offline {
            fn execute(&self, _request: &HttpRequest) -> anyhow::Result<HttpResponse> {
                anyhow::bail!("connection refused")
            }
        }
        let credentials = Arc::new(MemoryCredentialStore::default());
        credentials.insert("prod", r#"{"username":"reader","password":"secret"}"#);
        let client = ServiceNowClient::new(Offline, NoSleepClock);
        let reports = run_role_check(&[prod()], credentials, &client);
        assert!(reports[0].denied().is_empty());
        assert!(matches!(reports[0].tables[0].access, TableAccess::Error(_)));
    }
}
