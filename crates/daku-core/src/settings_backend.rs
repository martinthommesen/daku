//! Serves the daemon's own settings over the wire; dashboard state is pushed
//! by the collector, not requested.

use std::sync::Arc;

use daku_protocol::{Command, ResponsePayload};

use crate::Backend;
use crate::settings::DaemonSettingsStore;

pub struct SettingsBackend {
    settings: Arc<DaemonSettingsStore>,
}

impl SettingsBackend {
    pub fn new(settings: DaemonSettingsStore) -> Self {
        Self {
            settings: Arc::new(settings),
        }
    }
}

impl Backend for SettingsBackend {
    fn handle(&self, command: Command) -> anyhow::Result<ResponsePayload> {
        match command {
            Command::Ping => Ok(ResponsePayload::Ack),
            Command::GetSettings => Ok(ResponsePayload::Settings {
                settings: self.settings.get(),
            }),
            Command::UpdateSettings { settings } => {
                if let Some(url) = settings.webhook_url.as_deref()
                    && !url.trim().is_empty()
                    && !crate::webhook::webhook_url_allowed(url)
                {
                    anyhow::bail!("webhook_url must be loopback http or any https; got {url:?}");
                }
                self.settings.replace(settings)?;
                Ok(ResponsePayload::Ack)
            }
            Command::SaveEnvironment { .. }
            | Command::DeleteEnvironment { .. }
            | Command::TestEnvironment { .. }
            | Command::GetDigest { .. } => {
                anyhow::bail!("environment management is served by EnvironmentsBackend")
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use daku_protocol::settings::DaemonSettings;

    fn backend() -> (std::path::PathBuf, SettingsBackend) {
        let dir = std::env::temp_dir().join(format!("daku-settings-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("settings.json");
        let backend = SettingsBackend::new(DaemonSettingsStore::open(path.clone()).unwrap());
        (dir, backend)
    }

    #[test]
    fn update_settings_rejects_non_loopback_http_webhooks() {
        let (dir, backend) = backend();
        let error = backend
            .handle(Command::UpdateSettings {
                settings: DaemonSettings {
                    webhook_url: Some("http://192.168.1.10/hook".into()),
                    ..DaemonSettings::default()
                },
            })
            .unwrap_err()
            .to_string();
        assert!(error.contains("loopback"), "{error}");
        assert!(
            backend
                .handle(Command::UpdateSettings {
                    settings: DaemonSettings {
                        webhook_url: Some("http://127.0.0.1:9000/hook".into()),
                        ..DaemonSettings::default()
                    },
                })
                .is_ok()
        );
        let _ = std::fs::remove_dir_all(dir);
    }
}
