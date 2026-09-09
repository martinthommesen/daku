//! Diagnostics bundle: redacted operator state for tickets and debugging.
//!
//! `daku-daemon diagnostics [--out DIR]` writes a directory with the
//! Operator's configuration minus everything identifying or authenticating:
//! Environment labels and first DNS labels (never full URLs), settings
//! without secret-bearing query strings, a scrubbed daemon-log tail, and a
//! database census (counts only — never rows).
//!
//! Deliberately offline: no probes, no Keychain reads. The credentials file
//! is never even opened — its absence from the bundle is pinned by test.

use std::path::{Path, PathBuf};

/// First DNS label of an `https://` URL (`acme-prod` from
/// `https://acme-prod.example.service-now.com/x`). `None` when the URL does
/// not parse, so callers can fall back to a placeholder instead of the URL.
fn first_label(url: &str) -> Option<String> {
    let host = url.strip_prefix("https://")?.split('/').next()?;
    let label = host.split('.').next()?.trim();
    if label.is_empty() {
        return None;
    }
    Some(label.to_owned())
}

/// Drops everything after the host from URL-looking tokens and masks
/// `secret=value` carriers, then truncates long lines. Plain words pass
/// through untouched. Webhook paths are secret-bearing (some integrations
/// put the secret in the path), so only scheme+host survives.
fn scrub_log_line(line: &str) -> String {
    let scrubbed: Vec<String> = line
        .split_whitespace()
        .map(|token| {
            if token.contains("://") {
                crate::webhook::redacted_url(token)
            } else if let Some((key, _)) = token.split_once('=')
                && matches!(
                    key.to_ascii_lowercase().as_str(),
                    "token"
                        | "secret"
                        | "password"
                        | "passwd"
                        | "auth"
                        | "credential"
                        | "access_token"
                        | "api_key"
                        | "apikey"
                        | "client_secret"
                )
            {
                format!("{key}=…")
            } else {
                token.to_owned()
            }
        })
        .collect();
    let joined = scrubbed.join(" ");
    if joined.len() > 300 {
        format!("{}…", joined.chars().take(299).collect::<String>())
    } else {
        joined
    }
}

fn redact_environments(home: &Path) -> serde_json::Value {
    let path = home.join("environments.json");
    let Ok(bytes) = std::fs::read(&path) else {
        return serde_json::json!({"missing": path.display().to_string()});
    };
    let Ok(list) = serde_json::from_slice::<Vec<serde_json::Value>>(&bytes) else {
        return serde_json::json!({"unparsable": path.display().to_string()});
    };
    serde_json::Value::Array(
        list.into_iter()
            .map(|env| {
                serde_json::json!({
                    "id": env.get("id"),
                    "label": env.get("label"),
                    "platform": env.get("platform").unwrap_or(&serde_json::Value::String("servicenow".into())),
                    "host": env.get("instance_url").and_then(|url| url.as_str()).and_then(first_label).unwrap_or_else(|| "?".into()),
                    "auth_method": env.get("auth_method"),
                    "sort_order": env.get("sort_order"),
                    "clone_source": env.get("clone_source"),
                    "thresholds": env.get("thresholds"),
                    "expected_drift": env.get("expected_drift"),
                })
            })
            .collect(),
    )
}

fn redact_settings(home: &Path) -> serde_json::Value {
    let path = home.join("settings.json");
    let Ok(bytes) = std::fs::read(&path) else {
        return serde_json::json!({"missing": path.display().to_string()});
    };
    let Ok(settings) = serde_json::from_slice::<serde_json::Value>(&bytes) else {
        return serde_json::json!({"unparsable": path.display().to_string()});
    };
    let webhook = settings
        .get("webhook_url")
        .and_then(|url| url.as_str())
        // Host only: forwarder URLs carry tokens in the query string, and
        // some integrations put the secret in the path itself.
        .map(crate::webhook::redacted_url);
    serde_json::json!({
        "poll_interval_secs": settings.get("poll_interval_secs"),
        "slow_poll_interval_secs": settings.get("slow_poll_interval_secs"),
        "webhook_host": webhook,
    })
}

fn log_tail(home: &Path, lines: usize) -> String {
    use std::io::{Read as _, Seek as _};
    const MAX_TAIL_BYTES: u64 = 1024 * 1024;
    let path = home.join("daemon.log");
    let mut file = match std::fs::File::open(&path) {
        Ok(file) => file,
        Err(_) => return "(no daemon.log)\n".into(),
    };
    // Bounded tail read: seek to the last megabyte instead of loading a
    // potentially unbounded log into memory, then keep the last lines.
    let text = (|| -> std::io::Result<String> {
        let len = file.metadata()?.len();
        if len > MAX_TAIL_BYTES {
            file.seek(std::io::SeekFrom::Start(len - MAX_TAIL_BYTES))?;
            let mut buf = Vec::new();
            file.read_to_end(&mut buf)?;
            let text = String::from_utf8_lossy(&buf).into_owned();
            // Drop a potential partial first line after the seek.
            if let Some(pos) = text.find('\n') {
                return Ok(text[pos + 1..].to_owned());
            }
            return Ok(text);
        }
        let mut text = String::new();
        file.read_to_string(&mut text)?;
        Ok(text)
    })();
    let text = text.unwrap_or_default();
    let all: Vec<&str> = text.lines().collect();
    let start = all.len().saturating_sub(lines);
    all[start..]
        .iter()
        .map(|line| scrub_log_line(line))
        .collect::<Vec<_>>()
        .join("\n")
}

/// Writes the bundle into `out_dir` (created). Returns the written files.
/// `home` is the daku data directory (`daku_home_dir()` in production),
/// which keeps `DAKU_HOME` sandboxes working.
pub fn write_diagnostics(out_dir: &Path, home: &Path) -> anyhow::Result<Vec<PathBuf>> {
    std::fs::create_dir_all(out_dir)?;
    let mut written = Vec::new();
    let write = |name: &str, bytes: &[u8], written: &mut Vec<PathBuf>| -> anyhow::Result<()> {
        let path = out_dir.join(name);
        std::fs::write(&path, bytes)?;
        written.push(path);
        Ok(())
    };
    write(
        "environments.redacted.json",
        &serde_json::to_vec_pretty(&redact_environments(home))?,
        &mut written,
    )?;
    write(
        "settings.redacted.json",
        &serde_json::to_vec_pretty(&redact_settings(home))?,
        &mut written,
    )?;
    write(
        "daemon.log.tail.txt",
        log_tail(home, 200).as_bytes(),
        &mut written,
    )?;
    let db_path = home.join("app.db");
    write(
        "census.txt",
        format!(
            "{}\n",
            crate::persistence::format_db_stats(&db_path, &crate::persistence::db_stats(&db_path))
        )
        .as_bytes(),
        &mut written,
    )?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt as _;
        for path in &written {
            let _ = std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600));
        }
    }
    Ok(written)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn first_label_keeps_only_the_leftmost_name() {
        assert_eq!(
            first_label("https://acme-prod.example.service-now.com"),
            Some("acme-prod".into())
        );
        assert_eq!(
            first_label("https://acme-prod.example.service-now.com/a?x=1#f"),
            Some("acme-prod".into())
        );
        assert_eq!(first_label("https://"), None);
        assert_eq!(first_label("http://acme.example.com"), None);
    }

    #[test]
    fn scrub_log_line_strips_queries_and_truncates() {
        let line = "probe https://x.example.com/hook?token=abc#frag ok";
        assert_eq!(scrub_log_line(line), "probe https://x.example.com ok");
        assert_eq!(scrub_log_line(&"y".repeat(400)).len(), 302);
        assert_eq!(scrub_log_line("plain words 123"), "plain words 123");
        assert_eq!(
            scrub_log_line("refresh token=CANARY-9 user=alice"),
            "refresh token=… user=alice"
        );
    }

    fn sandbox(files: &[(&str, &str)]) -> PathBuf {
        let home = std::env::temp_dir().join(format!("daku-diag-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&home).unwrap();
        for (name, contents) in files {
            std::fs::write(home.join(name), contents).unwrap();
        }
        home
    }

    #[test]
    fn bundle_redacts_urls_queries_and_never_reads_credentials() {
        let home = sandbox(&[
            (
                "environments.json",
                r#"[{"id":"prod","label":"Production","instance_url":"https://acme-prod.example.service-now.com","auth_method":"oauth_client_credentials","sort_order":0,"clone_source":true}]"#,
            ),
            (
                "settings.json",
                r#"{"poll_interval_secs":60,"slow_poll_interval_secs":900,"webhook_url":"https://hooks.example.com/secret/PATH-CANARY-1?token=CANARY-1"}"#,
            ),
            (
                "daemon.log",
                "tick ok https://partner.example.com/hook?token=CANARY-2 done\n",
            ),
            ("credentials.json", r#"{"prod":"CANARY-3"}"#),
        ]);
        let out = home.join("bundle");
        let written = write_diagnostics(&out, &home).unwrap();
        assert_eq!(written.len(), 4);
        let bundle: String = written
            .iter()
            .map(|path| std::fs::read_to_string(path).unwrap())
            .collect::<Vec<_>>()
            .join("\n");
        for canary in ["CANARY-1", "CANARY-2", "CANARY-3", "PATH-CANARY-1"] {
            assert!(!bundle.contains(canary), "{canary} leaked into the bundle");
        }
        assert!(
            bundle.contains("hooks.example.com"),
            "webhook host stays for identification"
        );
        assert!(
            bundle.contains("900"),
            "slow cadence belongs in a cadence ticket bundle"
        );
        assert!(
            bundle.contains("acme-prod"),
            "first labels stay for identification"
        );
        assert!(
            !bundle.contains("acme-prod.example.service-now.com"),
            "full hosts must not appear"
        );
        assert!(bundle.contains("census") || bundle.contains("snapshots"));
        let _ = std::fs::remove_dir_all(home);
    }

    #[test]
    fn bundle_survives_a_missing_home() {
        let home = std::env::temp_dir().join(format!("daku-diag-{}", uuid::Uuid::new_v4()));
        let out = home.join("bundle");
        let written = write_diagnostics(&out, &home).unwrap();
        assert_eq!(written.len(), 4);
        let _ = std::fs::remove_dir_all(home);
    }
}
