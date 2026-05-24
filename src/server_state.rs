use std::sync::atomic::{AtomicBool, Ordering};
use tokio::sync::Mutex;

pub struct ServerState {
    change_server_volume_from_clients: AtomicBool,
    current_sink: Mutex<Option<String>>,
}

impl ServerState {
    pub fn new() -> Self {
        Self {
            change_server_volume_from_clients: AtomicBool::new(false),
            current_sink: Mutex::new(None),
        }
    }

    pub fn change_server_volume_from_clients(&self) -> bool {
        self.change_server_volume_from_clients
            .load(Ordering::Relaxed)
    }

    pub fn set_change_server_volume_from_clients(&self, enabled: bool) {
        self.change_server_volume_from_clients
            .store(enabled, Ordering::Relaxed);
    }

    pub async fn current_sink(&self) -> Option<String> {
        self.current_sink.lock().await.clone()
    }

    pub async fn set_current_sink(&self, sink: Option<String>) {
        *self.current_sink.lock().await = sink;
    }
}
