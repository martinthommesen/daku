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
                self.settings.replace(settings)?;
                Ok(ResponsePayload::Ack)
            }
        }
    }
}
