use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;

#[cfg(feature = "net_impairment_ui")]
pub const ALLOWED_ONE_SHOT_DELAYS_MS: [u64; 7] = [50, 100, 200, 300, 500, 700, 1000];

pub struct NetImpairmentController {
    pending_gap_ms: AtomicU64,
}

impl NetImpairmentController {
    pub fn new() -> Self {
        Self {
            pending_gap_ms: AtomicU64::new(0),
        }
    }

    #[cfg(feature = "net_impairment_ui")]
    pub fn trigger_gap_once_ms(&self, delay_ms: u64) -> Result<(), String> {
        if !ALLOWED_ONE_SHOT_DELAYS_MS.contains(&delay_ms) {
            return Err(format!("Unsupported delay: {delay_ms} ms"));
        }

        self.pending_gap_ms.store(delay_ms, Ordering::Release);
        Ok(())
    }

    pub fn take_gap_once(&self) -> Option<Duration> {
        let delay_ms = self.pending_gap_ms.swap(0, Ordering::AcqRel);
        if delay_ms == 0 {
            None
        } else {
            Some(Duration::from_millis(delay_ms))
        }
    }
}
