//! Per-session run status (idle / busy / retry) published on the bus.

use std::collections::BTreeMap;
use std::sync::Mutex;

use lz_schema::Event;
use lz_schema::session::SessionStatus;

use crate::bus::Bus;

#[derive(Default)]
pub struct StatusTracker {
    inner: Mutex<BTreeMap<String, SessionStatus>>,
}

impl StatusTracker {
    pub fn set(&self, bus: &Bus, session_id: &str, status: SessionStatus) {
        {
            let mut m = self.inner.lock().unwrap_or_else(|e| e.into_inner());
            if matches!(status, SessionStatus::Idle) {
                m.remove(session_id);
            } else {
                m.insert(session_id.to_string(), status.clone());
            }
        }
        bus.publish(Event::SessionStatus {
            session_id: session_id.into(),
            status,
        });
    }

    pub fn get(&self, session_id: &str) -> SessionStatus {
        self.inner
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .get(session_id)
            .cloned()
            .unwrap_or_default()
    }

    pub fn all(&self) -> BTreeMap<String, SessionStatus> {
        self.inner.lock().unwrap_or_else(|e| e.into_inner()).clone()
    }

    pub fn is_busy(&self, session_id: &str) -> bool {
        !matches!(self.get(session_id), SessionStatus::Idle)
    }
}
