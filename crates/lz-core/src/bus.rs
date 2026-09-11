//! In-process event bus: a bounded broadcast channel of `Arc<Event>`.
//! Durable events are additionally appended to the SQLite event log.

use std::sync::Arc;

use lz_schema::Event;
use tokio::sync::broadcast;

use crate::storage::{Storage, repo};

const CAPACITY: usize = 4096;

#[derive(Clone)]
pub struct Bus {
    tx: broadcast::Sender<Arc<Event>>,
    storage: Option<Storage>,
}

impl Bus {
    pub fn new(storage: Option<Storage>) -> Self {
        let (tx, _) = broadcast::channel(CAPACITY);
        Self { tx, storage }
    }

    pub fn publish(&self, event: Event) {
        if event.is_durable()
            && let (Some(storage), Some(aggregate)) = (&self.storage, event.session_id())
        {
            let aggregate = aggregate.to_string();
            let kind = event.type_name().to_string();
            if let Ok(data) = serde_json::to_value(&event) {
                let storage = storage.clone();
                // fire-and-forget; ordering is preserved by the single storage thread
                if let Ok(handle) = tokio::runtime::Handle::try_current() {
                    handle.spawn(async move {
                        let _ = storage
                            .with(move |c| repo::append_event(c, &aggregate, &kind, &data))
                            .await;
                    });
                } else {
                    let _ = storage.with_blocking(move |c| repo::append_event(c, &aggregate, &kind, &data));
                }
            }
        }
        let _ = self.tx.send(Arc::new(event));
    }

    pub fn subscribe(&self) -> broadcast::Receiver<Arc<Event>> {
        self.tx.subscribe()
    }

    pub fn receiver_count(&self) -> usize {
        self.tx.receiver_count()
    }
}
