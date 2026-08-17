use std::path::PathBuf;
use std::sync::Arc;

use daku_protocol::{Command, Request, ResponsePayload};

use crate::settings::DaemonSettingsStore;
use crate::{Backend, EventSink};

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
    fn handle(&self, request: Request, _: EventSink) -> anyhow::Result<ResponsePayload> {
        match request.command {
            Command::Ping => Ok(ResponsePayload::Ack),
            Command::GetSettings => Ok(ResponsePayload::Settings {
                settings: self.settings.get(),
            }),
            Command::UpdateSettings { settings } => {
                self.settings.replace(settings)?;
                Ok(ResponsePayload::Ack)
            }
            // ponytail: empty catalog until Environments/Signals (plan 002+); replace with real task state.
            Command::LoadTaskState => Ok(ResponsePayload::TaskState {
                projects: Vec::new(),
                sessions: Vec::new(),
                default_cwd: std::env::current_dir().unwrap_or_else(|_| PathBuf::from(".")),
                projectless_root: None,
            }),
        }
    }
}
