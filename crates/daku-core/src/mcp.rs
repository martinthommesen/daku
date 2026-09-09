//! Local MCP server: daku state for coding agents over stdio JSON-RPC.
//!
//! `daku-daemon mcp` speaks the MCP surface agents need — `initialize`,
//! `tools/list`, `tools/call` — over stdin/stdout, one JSON-RPC message per
//! line. No sockets, no tokens: stdio inherits the invoking user's trust,
//! the same envelope every other local subcommand (`doctor`, `digest`)
//! runs in. Everything is read-only over SQLite plus `environments.json`.
//!
//! Redaction is structural, not best-effort: tool outputs carry ids,
//! labels, states, counts, and summaries — never instance URLs (hosts
//! identify the Operator's estate), payloads (syslog rows carry message
//! text), or credentials (which live outside SQLite entirely).

use std::path::Path;

use crate::config::EnvironmentConfig;
use crate::health::health_rollup;
use crate::persistence::StateStore;
use daku_protocol::{Reachability, SignalState};

/// MCP tools this server answers.
pub const TOOL_NAMES: [&str; 5] = [
    "environments_list",
    "health_get",
    "signals_get",
    "events_recent",
    "digest_week",
];

/// Dispatches one parsed JSON-RPC message. Returns `None` for
/// notifications (no `id`), which need no reply.
pub fn handle_message(
    store: &StateStore,
    environments_path: &Path,
    message: serde_json::Value,
) -> Option<serde_json::Value> {
    let id = message.get("id").cloned();
    let method = message.get("method")?.as_str()?.to_owned();
    let reply = |result: serde_json::Value| {
        id.clone()
            .map(|id| serde_json::json!({"jsonrpc": "2.0", "id": id, "result": result}))
    };
    let fail = |code: i64, text: String| {
        id.clone().map(|id| {
            serde_json::json!({"jsonrpc": "2.0", "id": id, "error": {"code": code, "message": text}})
        })
    };
    match method.as_str() {
        "initialize" => reply(serde_json::json!({
            "protocolVersion": "2024-11-05",
            "capabilities": {"tools": {}},
            "serverInfo": {"name": "daku", "version": env!("CARGO_PKG_VERSION")},
        })),
        "tools/list" => reply(serde_json::json!({
            "tools": [
                {"name": "environments_list", "description": "List monitored Environments with platform and health.", "inputSchema": {"type": "object", "properties": {}}},
                {"name": "health_get", "description": "Rolled-up health plus per-Signal votes for one Environment.", "inputSchema": {"type": "object", "properties": {"environment_id": {"type": "string"}}, "required": ["environment_id"]}},
                {"name": "signals_get", "description": "Latest snapshot per Signal for one Environment (states plus redacted drill-in rows, never raw payloads or URLs).", "inputSchema": {"type": "object", "properties": {"environment_id": {"type": "string"}}, "required": ["environment_id"]}},
                {"name": "events_recent", "description": "Recent health and Signal transitions for one Environment, newest first.", "inputSchema": {"type": "object", "properties": {"environment_id": {"type": "string"}, "limit": {"type": "integer", "default": 20}}, "required": ["environment_id"]}},
                {"name": "digest_week", "description": "Markdown week-in-review for one Environment: transitions, builds, current states.", "inputSchema": {"type": "object", "properties": {"environment_id": {"type": "string"}, "days": {"type": "integer", "default": 7}}, "required": ["environment_id"]}},
            ],
        })),
        "tools/call" => {
            let params = message.get("params").cloned().unwrap_or_default();
            let name = params
                .get("name")
                .and_then(|name| name.as_str())
                .unwrap_or("");
            let args = params.get("arguments").cloned().unwrap_or_default();
            match call_tool(store, environments_path, name, &args) {
                Ok(text) => reply(serde_json::json!({"content": [{"type": "text", "text": text}]})),
                Err(error) => fail(-32602, error),
            }
        }
        _ => fail(-32601, format!("unknown method {method}")),
    }
}

fn environments_of(path: &Path) -> Result<Vec<EnvironmentConfig>, String> {
    crate::config::load_environments(path).map_err(|error| format!("{error:#}"))
}

fn find_environment<'a>(
    environments: &'a [EnvironmentConfig],
    id: &str,
) -> Result<&'a EnvironmentConfig, String> {
    environments
        .iter()
        .find(|environment| environment.id == id)
        .ok_or_else(|| format!("unknown environment {id}"))
}

/// One Environment's computed state: reachability, per-Signal votes, the
/// raw payload per Signal (for redacted drill-in rows), and the latest
/// observation time (0 when nothing was ever recorded).
pub struct EnvVotes {
    pub reachability: Reachability,
    pub votes: Vec<(String, SignalState)>,
    pub payloads: Vec<(String, String)>,
    pub observed_at: i64,
}

fn votes_of(store: &StateStore, environment_id: &str) -> Result<EnvVotes, String> {
    let connection = store.open().map_err(|error| format!("{error}"))?;
    let snapshots = crate::persistence::load_all_signal_snapshots(&connection)
        .map_err(|error| format!("{error}"))?;
    let mut reachability = Reachability::Reachable;
    let mut votes = Vec::new();
    let mut payloads = Vec::new();
    let mut observed_at = None;
    for snapshot in snapshots
        .iter()
        .filter(|row| row.environment_id == environment_id)
    {
        if snapshot.signal_id == crate::availability::AVAILABILITY_SIGNAL_ID {
            reachability = serde_json::from_str::<serde_json::Value>(&snapshot.payload_json)
                .ok()
                .and_then(|payload| {
                    payload
                        .get("reachability")?
                        .as_str()
                        .and_then(Reachability::parse)
                })
                .unwrap_or(Reachability::Reachable);
        }
        votes.push((
            snapshot.signal_id.clone(),
            SignalState::parse(&snapshot.state).unwrap_or(SignalState::Skipped),
        ));
        payloads.push((snapshot.signal_id.clone(), snapshot.payload_json.clone()));
        observed_at = Some(observed_at.unwrap_or(0).max(snapshot.observed_at));
    }
    Ok(EnvVotes {
        reachability,
        votes,
        payloads,
        observed_at: observed_at.unwrap_or(0),
    })
}

fn call_tool(
    store: &StateStore,
    environments_path: &Path,
    name: &str,
    args: &serde_json::Value,
) -> Result<String, String> {
    let environments = environments_of(environments_path)?;
    let env_id = args
        .get("environment_id")
        .and_then(|id| id.as_str())
        .unwrap_or("");
    match name {
        "environments_list" => {
            let mut rows = Vec::new();
            for environment in &environments {
                let computed = votes_of(store, &environment.id)?;
                let reachability = computed.reachability;
                let votes = computed.votes;
                let health = health_rollup(
                    reachability,
                    &votes
                        .iter()
                        .map(|(id, state)| (id.as_str(), *state))
                        .collect::<Vec<_>>(),
                );
                rows.push(serde_json::json!({
                    "id": environment.id,
                    "label": environment.label,
                    "platform": environment.platform.id(),
                    "health": health.as_str(),
                    "reachability": reachability.as_str(),
                }));
            }
            serde_json::to_string_pretty(&rows).map_err(|error| format!("{error}"))
        }
        "health_get" => {
            let environment = find_environment(&environments, env_id)?;
            let computed = votes_of(store, env_id)?;
            let reachability = computed.reachability;
            let votes = computed.votes;
            let health = health_rollup(
                reachability,
                &votes
                    .iter()
                    .map(|(id, state)| (id.as_str(), *state))
                    .collect::<Vec<_>>(),
            );
            serde_json::to_string_pretty(&serde_json::json!({
                "id": environment.id,
                "label": environment.label,
                "health": health.as_str(),
                "reachability": reachability.as_str(),
                "votes": votes.iter().map(|(id, state)| serde_json::json!({
                    "signal_id": id,
                    "state": state.as_str(),
                })).collect::<Vec<_>>(),
            }))
            .map_err(|error| format!("{error}"))
        }
        "signals_get" => {
            find_environment(&environments, env_id)?;
            let computed = votes_of(store, env_id)?;
            let payloads: std::collections::HashMap<&str, &str> = computed
                .payloads
                .iter()
                .map(|(id, payload)| (id.as_str(), payload.as_str()))
                .collect();
            // States plus redacted drill-in rows; summaries live
            // desktop-side, payloads stay out.
            serde_json::to_string_pretty(
                &computed
                    .votes
                    .iter()
                    .map(|(id, state)| {
                        serde_json::json!({
                            "signal_id": id,
                            "state": state.as_str(),
                            "rows": payloads
                                .get(id.as_str())
                                .map(|payload| redacted_rows(id, payload))
                                .unwrap_or_default(),
                        })
                    })
                    .collect::<Vec<_>>(),
            )
            .map_err(|error| format!("{error}"))
        }
        "events_recent" => {
            find_environment(&environments, env_id)?;
            let limit = args
                .get("limit")
                .and_then(|limit| limit.as_i64())
                .unwrap_or(20)
                .clamp(1, 100) as usize;
            let connection = store.open().map_err(|error| format!("{error}"))?;
            let mut rows: Vec<(i64, serde_json::Value)> = Vec::new();
            for event in crate::persistence::load_health_events(&connection, env_id, 500)
                .map_err(|error| format!("{error}"))?
            {
                rows.push((
                    event.observed_at,
                    serde_json::json!({
                        "kind": event.kind,
                        "from": event.from_health,
                        "to": event.to_health,
                        "build": event.build,
                        "note": event.note,
                    }),
                ));
            }
            for event in crate::persistence::load_signal_events(&connection, env_id, 500)
                .map_err(|error| format!("{error}"))?
            {
                rows.push((
                    event.observed_at,
                    serde_json::json!({
                        "kind": "signal",
                        "signal_id": event.signal_id,
                        "from": event.from_state,
                        "to": event.to_state,
                    }),
                ));
            }
            rows.sort_by_key(|(at, _)| -*at);
            rows.truncate(limit);
            serde_json::to_string_pretty(
                &rows
                    .into_iter()
                    .map(|(at, row)| {
                        let mut row = row;
                        row["observed_at"] = at.into();
                        row
                    })
                    .collect::<Vec<_>>(),
            )
            .map_err(|error| format!("{error}"))
        }
        "digest_week" => {
            let environment = find_environment(&environments, env_id)?.clone();
            let days = args
                .get("days")
                .and_then(|days| days.as_i64())
                .unwrap_or(7)
                .clamp(1, 90);
            let now = crate::collector::unix_now();
            crate::digest::weekly_digest(store, &environment, now, days)
                .map_err(|error| format!("{error:#}"))
        }
        _ => Err(format!("unknown tool {name}")),
    }
}

/// Redacted drill-in rows for one Signal payload: which items are offending,
/// without the payload itself. The allowlist names exactly one display field
/// per list — config-ish names (job names, MID host names, plugin ids, user
/// display names), never message bodies (syslog `message`, email `subject`),
/// URLs (outbound, transaction), or credentials. Lists without a safe display
/// field report counts only (`names` empty). Bounded to `ROW_NAMES_CAP` names;
/// `truncated` ORs the payload's own flag with the cap.
pub const ROW_NAMES_CAP: usize = 10;

/// (signal id, list key, display-field key or `None` for counts only).
const REDACTED_ROW_TABLE: &[(&str, &str, Option<&str>)] = &[
    ("jobs", "overdue_rows", Some("name")),
    ("jobs", "error_rows", Some("name")),
    ("syslog", "error_rows", Some("source")),
    ("mid_ecc", "agents_unhealthy_list", Some("host_name")),
    ("outbound", "failure_rows", None),
    ("flow", "error_rows", Some("name")),
    ("email", "error_rows", None),
    ("upgrade", "upgrades", Some("from_version")),
    ("sessions", "session_rows", Some("user")),
    ("update_sets", "open_rows", Some("name")),
    ("scan", "finding_rows", Some("priority")),
    ("drift", "mismatch_list", Some("id")),
    ("github", "run_rows", Some("name")),
    ("transaction", "slow_rows", None),
];

fn redacted_rows(signal_id: &str, payload_json: &str) -> Vec<serde_json::Value> {
    let Ok(payload) = serde_json::from_str::<serde_json::Value>(payload_json) else {
        return Vec::new();
    };
    let mut rows = Vec::new();
    for (_, list_key, name_key) in REDACTED_ROW_TABLE
        .iter()
        .filter(|(signal, _, _)| *signal == signal_id)
    {
        let Some(list) = payload.get(list_key).and_then(|list| list.as_array()) else {
            continue;
        };
        if list.is_empty() {
            continue;
        }
        let names: Vec<String> = match name_key {
            Some(key) => list
                .iter()
                .filter_map(|row| {
                    row.get(key)
                        .and_then(|name| name.as_str())
                        .filter(|name| !name.is_empty())
                        .map(str::to_owned)
                })
                .take(ROW_NAMES_CAP)
                .collect(),
            None => Vec::new(),
        };
        let truncated = payload
            .get(format!("{list_key}_truncated"))
            .and_then(|flag| flag.as_bool())
            .unwrap_or(false)
            || (name_key.is_some() && list.len() > names.len());
        rows.push(serde_json::json!({
            "list": list_key,
            "count": list.len(),
            "names": names,
            "truncated": truncated,
        }));
    }
    rows
}

/// Runs the stdio loop: one JSON-RPC message per stdin line, replies on
/// stdout. Parse errors answer `-32700` and continue; notifications (no
/// `id`) get no reply. Ends on EOF.
pub fn run_stdio(store: &StateStore, environments_path: &Path) -> anyhow::Result<()> {
    run_lines(
        std::io::stdin().lock(),
        std::io::stdout(),
        store,
        environments_path,
    )
}

/// The stdio loop over injected streams: `run_stdio` wires the real stdio,
/// tests feed piped bytes. Broken-pipe on reply is terminal (nobody is
/// listening); a bad stdin line is a `-32700` reply, never a panic.
fn run_lines(
    reader: impl std::io::BufRead,
    mut writer: impl std::io::Write,
    store: &StateStore,
    environments_path: &Path,
) -> anyhow::Result<()> {
    for line in reader.lines() {
        let line = line?;
        if line.trim().is_empty() {
            continue;
        }
        let reply = match serde_json::from_str::<serde_json::Value>(&line) {
            Ok(message) => handle_message(store, environments_path, message),
            Err(_) => Some(serde_json::json!({
                "jsonrpc": "2.0", "id": null,
                "error": {"code": -32700, "message": "parse error"},
            })),
        };
        if let Some(reply) = reply {
            writeln!(writer, "{}", serde_json::to_string(&reply)?)?;
            writer.flush()?;
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::availability::{AvailabilityObservation, persist_availability_snapshot};
    use crate::persistence::{self, HealthEvent};
    use crate::test_support::{TempDb, TempFile};
    use daku_protocol::SignalState as ProtocolState;

    /// Store with one reachable availability snapshot plus a health event,
    /// and an environments file with one Environment.
    fn rig() -> (TempDb, TempFile) {
        let db = TempDb::new("mcp");
        let now = crate::collector::unix_now();
        let connection = db.store().open().unwrap();
        persist_availability_snapshot(
            &connection,
            "prod",
            &AvailabilityObservation {
                reachability: Reachability::Reachable,
                state: ProtocolState::Healthy,
                build: Some("glide-9".into()),
                rtt_ms: 4,
                error: None,
            },
            now,
        )
        .unwrap();
        persistence::persist_signal_snapshot(
            &connection,
            "prod",
            "jobs",
            now,
            ProtocolState::Degraded,
            r#"{"overdue_ready":2,"error":0}"#,
        )
        .unwrap();
        persistence::record_health_event(
            &connection,
            &HealthEvent {
                environment_id: "prod".into(),
                observed_at: now - 60,
                kind: "health".into(),
                from_health: Some("healthy".into()),
                to_health: "degraded".into(),
                build: None,
                note: None,
            },
        )
        .unwrap();
        let file = TempFile::with_contents(
            "mcp-envs",
            serde_json::to_vec(&[serde_json::json!({
                "id": "prod",
                "label": "Production",
                "instance_url": "https://acme-prod.example.service-now.com",
                "auth_method": "basic",
                "sort_order": 0,
            })])
            .unwrap(),
        );
        (db, file)
    }

    fn call(
        store: &StateStore,
        path: &Path,
        name: &str,
        args: serde_json::Value,
    ) -> serde_json::Value {
        let message = serde_json::json!({
            "jsonrpc": "2.0", "id": 7,
            "method": "tools/call",
            "params": {"name": name, "arguments": args},
        });
        handle_message(store, path, message).expect("a call always replies")
    }

    fn text_of(reply: &serde_json::Value) -> &str {
        reply["result"]["content"][0]["text"].as_str().unwrap()
    }

    #[test]
    fn initialize_lists_five_tools() {
        let (db, file) = rig();
        let hello = handle_message(
            &db.store(),
            file.path(),
            serde_json::json!({"jsonrpc": "2.0", "id": 1, "method": "initialize", "params": {}}),
        )
        .unwrap();
        assert_eq!(hello["result"]["serverInfo"]["name"], "daku");
        let list = handle_message(
            &db.store(),
            file.path(),
            serde_json::json!({"jsonrpc": "2.0", "id": 2, "method": "tools/list", "params": {}}),
        )
        .unwrap();
        let names: Vec<&str> = list["result"]["tools"]
            .as_array()
            .unwrap()
            .iter()
            .map(|tool| tool["name"].as_str().unwrap())
            .collect();
        assert_eq!(names, TOOL_NAMES);
    }

    #[test]
    fn notifications_get_no_reply_and_unknown_methods_error() {
        let (db, file) = rig();
        assert_eq!(
            handle_message(
                &db.store(),
                file.path(),
                serde_json::json!({"jsonrpc": "2.0", "method": "notifications/initialized"}),
            ),
            None
        );
        let reply = handle_message(
            &db.store(),
            file.path(),
            serde_json::json!({"jsonrpc": "2.0", "id": 3, "method": "tools/eat"}),
        )
        .unwrap();
        assert_eq!(reply["error"]["code"], -32601);
        let reply = handle_message(
            &db.store(),
            file.path(),
            serde_json::json!({"jsonrpc": "2.0", "id": 4, "method": "tools/call", "params": {"name": "nope", "arguments": {}}}),
        )
        .unwrap();
        assert_eq!(reply["error"]["code"], -32602);
    }

    #[test]
    fn redacted_rows_name_offenders_without_payloads_or_urls() {
        // Job names ride; counts and truncation survive.
        let rows = redacted_rows(
            "jobs",
            r#"{"overdue_rows":[{"name":"Nightly sync"}],"overdue_rows_truncated":false,"error_rows":[]}"#,
        );
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0]["list"], "overdue_rows");
        assert_eq!(rows[0]["count"], 1);
        assert_eq!(rows[0]["names"], serde_json::json!(["Nightly sync"]));
        assert_eq!(rows[0]["truncated"], false);
        // Syslog message text never rides — only the source.
        let rows = redacted_rows(
            "syslog",
            r#"{"error_rows":[{"source":"Scheduled job","message":"Null pointer in SECRET-1"}],"error_rows_truncated":false}"#,
        );
        let text = serde_json::to_string(&rows).unwrap();
        assert!(!text.contains("SECRET-1"), "{text}");
        assert_eq!(rows[0]["names"], serde_json::json!(["Scheduled job"]));
        // Email subjects and outbound URLs are user content: counts only.
        let rows = redacted_rows(
            "email",
            r#"{"error_rows":[{"subject":"SECRET-2"}],"error_rows_truncated":false}"#,
        );
        assert_eq!(rows[0]["count"], 1);
        assert_eq!(rows[0]["names"], serde_json::json!([]));
        assert!(!serde_json::to_string(&rows).unwrap().contains("SECRET-2"));
        let rows = redacted_rows(
            "outbound",
            r#"{"failure_rows":[{"url":"https://SECRET-3.example.com/hook"}],"failure_rows_truncated":true}"#,
        );
        assert_eq!(rows[0]["count"], 1);
        assert_eq!(rows[0]["truncated"], true);
        assert!(!serde_json::to_string(&rows).unwrap().contains("SECRET-3"));
        // Session user display names ride; ids stay out of the names.
        let rows = redacted_rows(
            "sessions",
            r#"{"active_sessions":2,"truncated":false,"session_rows":[{"sys_id":"s1","user":"Fred Johnson","user_id":"u1","sys_created_on":"2026-01-27 00:12:00"},{"sys_id":"s2","user":"Ada Lovelace","user_id":"u2","sys_created_on":"2026-01-27 00:10:00"}],"session_rows_truncated":false}"#,
        );
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0]["list"], "session_rows");
        assert_eq!(rows[0]["count"], 2);
        assert_eq!(
            rows[0]["names"],
            serde_json::json!(["Fred Johnson", "Ada Lovelace"])
        );
        assert_eq!(rows[0]["truncated"], false);
        // Unknown signals, unparsable payloads, and empty lists yield nothing.
        assert!(redacted_rows("availability", r#"{"rtt_ms":4}"#).is_empty());
        assert!(redacted_rows("jobs", "not json").is_empty());
        assert!(redacted_rows("jobs", r#"{"overdue_rows":[],"error_rows":[]}"#).is_empty());
        assert!(
            redacted_rows(
                "sessions",
                r#"{"active_sessions":0,"truncated":false,"session_rows":[],"session_rows_truncated":false}"#
            )
            .is_empty()
        );
    }

    #[test]
    fn stdio_loop_answers_parse_errors_skips_blanks_and_ends_on_eof() {
        let (db, file) = rig();
        let input = concat!(
            "{\"jsonrpc\": \"2.0\", \"id\": 1, \"method\": \"initialize\", \"params\": {}}\n",
            "\n",
            "   \n",
            "{not json\n",
            "{\"jsonrpc\": \"2.0\", \"method\": \"notifications/initialized\"}\n",
        );
        let mut output = Vec::new();
        run_lines(input.as_bytes(), &mut output, &db.store(), file.path()).unwrap();
        let replies: Vec<serde_json::Value> = String::from_utf8(output)
            .unwrap()
            .lines()
            .map(serde_json::from_str)
            .collect::<Result<_, _>>()
            .unwrap();
        // initialize reply + parse-error reply; blanks and the notification
        // produce nothing.
        assert_eq!(replies.len(), 2);
        assert_eq!(replies[0]["id"], 1);
        assert_eq!(replies[0]["result"]["serverInfo"]["name"], "daku");
        assert_eq!(replies[1]["id"], serde_json::Value::Null);
        assert_eq!(replies[1]["error"]["code"], -32700);
    }

    #[test]
    fn tools_read_seeded_state_and_redact_urls() {
        let (db, file) = rig();
        let store = db.store();
        let list: serde_json::Value = serde_json::from_str(text_of(&call(
            &store,
            file.path(),
            "environments_list",
            serde_json::json!({}),
        )))
        .unwrap();
        assert_eq!(list[0]["health"], "degraded");
        assert_eq!(list[0]["platform"], "servicenow");
        assert!(list[0].get("instance_url").is_none());

        let health: serde_json::Value = serde_json::from_str(text_of(&call(
            &store,
            file.path(),
            "health_get",
            serde_json::json!({"environment_id": "prod"}),
        )))
        .unwrap();
        assert_eq!(health["health"], "degraded");
        assert_eq!(health["votes"].as_array().unwrap().len(), 2);

        let signals: serde_json::Value = serde_json::from_str(text_of(&call(
            &store,
            file.path(),
            "signals_get",
            serde_json::json!({"environment_id": "prod"}),
        )))
        .unwrap();
        assert!(
            signals
                .as_array()
                .unwrap()
                .iter()
                .all(|row| row.get("payload").is_none())
        );

        let events: serde_json::Value = serde_json::from_str(text_of(&call(
            &store,
            file.path(),
            "events_recent",
            serde_json::json!({"environment_id": "prod", "limit": 5}),
        )))
        .unwrap();
        assert_eq!(events.as_array().unwrap().len(), 1);

        let digest_reply = call(
            &store,
            file.path(),
            "digest_week",
            serde_json::json!({"environment_id": "prod", "days": 7}),
        );
        let digest = text_of(&digest_reply);
        assert!(digest.contains("# Daku digest"), "{digest}");

        let missing = call(
            &store,
            file.path(),
            "health_get",
            serde_json::json!({"environment_id": "nope"}),
        );
        assert_eq!(missing["error"]["code"], -32602);
    }
}
