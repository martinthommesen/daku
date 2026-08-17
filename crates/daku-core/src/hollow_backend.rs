use std::sync::Arc;

use daku_protocol::{Command, ResponsePayload};

use crate::Backend;
use crate::settings::DaemonSettingsStore;

pub struct HollowBackend {
    settings: Arc<DaemonSettingsStore>,
}

impl HollowBackend {
    pub fn new(
        settings: DaemonSettingsStore,
        task_store: crate::persistence::StateStore,
    ) -> anyhow::Result<Self> {
        let _ = task_store.open()?;
        Ok(Self {
            settings: Arc::new(settings),
        })
    }
}

impl Backend for HollowBackend {
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
