//! Command palette (⌘K) logic: a filterable action list over Environments
//! and shell commands. Entry construction and filtering are pure and
//! unit-tested; `app.rs` renders the overlay and dispatches by entry id.

/// One runnable palette row.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PaletteEntry {
    /// `switch:<env>` for Environments, a verb otherwise (`reload`,
    /// `copy`, `add-env`, `toggle-notifications`,
    /// `toggle-digest`, `mute-24h`, `unmute`, `open-snow`).
    pub id: String,
    pub title: String,
    pub hint: String,
}

/// Minimal Environment view the palette needs. Keeps this module free of
/// dashboard types.
pub struct EnvRef<'a> {
    pub id: &'a str,
    pub label: &'a str,
    pub health: &'a str,
    pub platform: &'a str,
}

/// The full list: one row per Environment first (switch), then shell
/// commands. Environment rows carry health so the operator sees state while
/// jumping; mute rows only make sense with a selection.
pub fn entries_for(envs: &[EnvRef<'_>], selected_id: Option<&str>) -> Vec<PaletteEntry> {
    let mut entries = Vec::new();
    for env in envs {
        entries.push(PaletteEntry {
            id: format!("switch:{}", env.id),
            title: format!("Go to {} ({})", env.label, env.health),
            hint: "switch environment".into(),
        });
    }
    if selected_id.is_some() {
        // The deep link leaves the product on probes, so the title names
        // the destination.
        let selected_platform = selected_id
            .and_then(|id| envs.iter().find(|env| env.id == id))
            .map(|env| env.platform);
        let open_title = match selected_platform {
            Some("github") => "Open repository",
            Some("http") => "Open probe target",
            _ => "Open in ServiceNow",
        };
        entries.push(PaletteEntry {
            id: "open-snow".into(),
            title: open_title.into(),
            hint: "deep link".into(),
        });
        for (id, title) in [("mute-24h", "Mute for 24 hours"), ("unmute", "Unmute")] {
            entries.push(PaletteEntry {
                id: id.into(),
                title: title.into(),
                hint: "mute".into(),
            });
        }
    }
    for (id, title, hint) in [
        ("reload", "Reload Daemon", "config"),
        ("copy", "Copy Environment Summary", "clipboard"),
        ("add-env", "Add Environment…", "setup"),
        (
            "toggle-notifications",
            "Toggle Health Notifications",
            "notifications",
        ),
        ("toggle-digest", "Toggle Weekly Digest", "notifications"),
    ] {
        entries.push(PaletteEntry {
            id: id.into(),
            title: title.into(),
            hint: hint.into(),
        });
    }
    entries
}

/// Substring filter over title and hint, case-insensitive. Empty query
/// returns everything in order.
pub fn filter_entries<'a>(entries: &'a [PaletteEntry], query: &str) -> Vec<&'a PaletteEntry> {
    let query = query.trim().to_lowercase();
    if query.is_empty() {
        return entries.iter().collect();
    }
    entries
        .iter()
        .filter(|entry| {
            entry.title.to_lowercase().contains(&query)
                || entry.hint.to_lowercase().contains(&query)
                || entry.id.to_lowercase().contains(&query)
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample() -> (Vec<String>, Vec<String>, Vec<String>, Vec<String>) {
        // Owned backing stores so `EnvRef` borrows stay valid.
        (
            vec!["prod".to_owned(), "test".to_owned()],
            vec!["Production".to_owned(), "Test".to_owned()],
            vec!["degraded".to_owned(), "healthy".to_owned()],
            vec!["servicenow".to_owned(), "http".to_owned()],
        )
    }

    #[test]
    fn entries_list_environments_first_then_commands() {
        let (ids, labels, healths, platforms) = sample();
        let refs: Vec<EnvRef<'_>> = ids
            .iter()
            .zip(labels.iter())
            .zip(healths.iter())
            .zip(platforms.iter())
            .map(|(((id, label), health), platform)| EnvRef {
                id,
                label,
                health,
                platform,
            })
            .collect();
        let entries = entries_for(&refs, Some("prod"));
        assert_eq!(entries[0].id, "switch:prod");
        assert!(entries[0].title.contains("degraded"));
        assert_eq!(entries[1].id, "switch:test");
        assert!(entries.iter().any(|entry| entry.id == "mute-24h"));
        assert!(entries.iter().any(|entry| entry.id == "reload"));
        // No selection, no selection-scoped rows.
        let bare = entries_for(&refs, None);
        assert!(bare.iter().all(|entry| !entry.id.starts_with("mute")));
        assert!(bare.iter().all(|entry| entry.id != "open-snow"));
        assert!(bare.iter().all(|entry| entry.id != "unmute"));
    }

    #[test]
    fn open_entry_names_the_probe_destination_per_platform() {
        let (ids, labels, healths, platforms) = sample();
        let refs: Vec<EnvRef<'_>> = ids
            .iter()
            .zip(labels.iter())
            .zip(healths.iter())
            .zip(platforms.iter())
            .map(|(((id, label), health), platform)| EnvRef {
                id,
                label,
                health,
                platform,
            })
            .collect();
        // prod is servicenow, test is http.
        let snow = entries_for(&refs, Some("prod"));
        assert!(
            snow.iter()
                .any(|entry| entry.id == "open-snow" && entry.title == "Open in ServiceNow")
        );
        let probe = entries_for(&refs, Some("test"));
        assert!(
            probe
                .iter()
                .any(|entry| entry.id == "open-snow" && entry.title == "Open probe target")
        );
    }

    #[test]
    fn filter_matches_title_hint_and_id_case_insensitively() {
        let (ids, labels, healths, platforms) = sample();
        let refs: Vec<EnvRef<'_>> = ids
            .iter()
            .zip(labels.iter())
            .zip(healths.iter())
            .zip(platforms.iter())
            .map(|(((id, label), health), platform)| EnvRef {
                id,
                label,
                health,
                platform,
            })
            .collect();
        let entries = entries_for(&refs, Some("prod"));
        assert_eq!(filter_entries(&entries, "").len(), entries.len());
        assert_eq!(filter_entries(&entries, "PROD").len(), 1);
        assert_eq!(filter_entries(&entries, "mute").len(), 2);
        assert_eq!(filter_entries(&entries, "zzz").len(), 0);
        // "switch" matches the two environment rows by id prefix.
        assert_eq!(filter_entries(&entries, "switch:").len(), 2);
    }
}
