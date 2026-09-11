//! Session service: create/list sessions, persist messages and parts, and
//! publish the matching bus events.

pub mod compaction;
pub mod history;
pub mod loop_guard;
pub mod processor;
pub mod reminders;
pub mod retry;
pub mod revert;
pub mod runner;
pub mod shell;
pub mod status;
pub mod subtask;
pub mod system;

use std::sync::Arc;

use lz_schema::Event;
use lz_schema::ids::{self, Prefix};
use lz_schema::session::*;

use crate::bus::Bus;
use crate::storage::{Storage, StorageError, now_ms, repo};

pub const VERSION: &str = env!("CARGO_PKG_VERSION");

#[derive(Clone)]
pub struct SessionService {
    pub storage: Storage,
    pub bus: Bus,
    pub project_id: Arc<str>,
    pub directory: Arc<str>,
}

fn slug() -> String {
    const ADJ: &[&str] = &[
        "quiet", "brave", "swift", "calm", "bright", "lunar", "amber", "misty", "bold", "gentle", "wild",
        "keen",
    ];
    const NOUN: &[&str] = &[
        "river", "falcon", "meadow", "comet", "harbor", "forest", "summit", "glacier", "ember", "orchid",
        "canyon", "tide",
    ];
    use rand::Rng;
    let mut r = rand::rng();
    format!(
        "{}-{}",
        ADJ[r.random_range(0..ADJ.len())],
        NOUN[r.random_range(0..NOUN.len())]
    )
}

impl SessionService {
    pub fn new(storage: Storage, bus: Bus, project_id: &str, directory: &str) -> Self {
        Self {
            storage,
            bus,
            project_id: project_id.into(),
            directory: directory.into(),
        }
    }

    pub async fn create(
        &self,
        parent_id: Option<String>,
        title: Option<String>,
        agent: Option<String>,
        permission: Option<lz_schema::permission::Ruleset>,
    ) -> Result<SessionInfo, StorageError> {
        let now = now_ms();
        let info = SessionInfo {
            id: ids::descending(Prefix::Session),
            slug: slug(),
            project_id: self.project_id.to_string(),
            directory: self.directory.to_string(),
            parent_id,
            summary: None,
            cost: None,
            tokens: None,
            title: title.unwrap_or_else(|| {
                let d = jiff::Timestamp::now().strftime("%Y-%m-%d %H:%M").to_string();
                format!("New session - {d}")
            }),
            agent,
            model: None,
            version: VERSION.into(),
            metadata: None,
            time: SessionTime {
                created: now,
                updated: now,
                compacting: None,
                archived: None,
            },
            permission,
            revert: None,
        };
        let s = info.clone();
        self.storage.with(move |c| repo::upsert_session(c, &s)).await?;
        self.bus.publish(Event::SessionCreated {
            session_id: info.id.clone(),
            info: info.clone(),
        });
        Ok(info)
    }

    pub async fn get(&self, id: &str) -> Result<SessionInfo, StorageError> {
        let id = id.to_string();
        self.storage.with(move |c| repo::get_session(c, &id)).await
    }

    pub async fn update(&self, mut info: SessionInfo) -> Result<SessionInfo, StorageError> {
        info.time.updated = now_ms();
        let s = info.clone();
        self.storage.with(move |c| repo::upsert_session(c, &s)).await?;
        self.bus.publish(Event::SessionUpdated {
            session_id: info.id.clone(),
            info: info.clone(),
        });
        Ok(info)
    }

    /// Load, mutate, save.
    pub async fn modify(
        &self,
        id: &str,
        f: impl FnOnce(&mut SessionInfo) + Send,
    ) -> Result<SessionInfo, StorageError> {
        let mut info = self.get(id).await?;
        f(&mut info);
        self.update(info).await
    }

    pub async fn delete(&self, id: &str) -> Result<(), StorageError> {
        let info = self.get(id).await?;
        let sid = id.to_string();
        self.storage.with(move |c| repo::delete_session(c, &sid)).await?;
        self.bus.publish(Event::SessionDeleted {
            session_id: info.id.clone(),
            info,
        });
        Ok(())
    }

    pub async fn list(
        &self,
        search: Option<String>,
        limit: Option<usize>,
        roots_only: bool,
    ) -> Result<Vec<SessionInfo>, StorageError> {
        let project = self.project_id.to_string();
        self.storage
            .with(move |c| {
                repo::list_sessions(
                    c,
                    repo::SessionFilter {
                        project_id: Some(&project),
                        parent_id: if roots_only { Some(None) } else { None },
                        search: search.as_deref(),
                        limit,
                        include_archived: false,
                    },
                )
            })
            .await
    }

    pub async fn children(&self, parent: &str) -> Result<Vec<SessionInfo>, StorageError> {
        let parent = parent.to_string();
        self.storage
            .with(move |c| {
                repo::list_sessions(
                    c,
                    repo::SessionFilter {
                        project_id: None,
                        parent_id: Some(Some(&parent)),
                        search: None,
                        limit: None,
                        include_archived: true,
                    },
                )
            })
            .await
    }

    // ───────────── messages / parts ─────────────

    pub async fn update_message(&self, message: Message) -> Result<Message, StorageError> {
        let m = message.clone();
        self.storage.with(move |c| repo::upsert_message(c, &m)).await?;
        self.bus.publish(Event::MessageUpdated {
            session_id: message.session_id().to_string(),
            info: message.clone(),
        });
        Ok(message)
    }

    pub async fn get_message(&self, id: &str) -> Result<Message, StorageError> {
        let id = id.to_string();
        self.storage.with(move |c| repo::get_message(c, &id)).await
    }

    pub async fn remove_message(&self, session_id: &str, id: &str) -> Result<(), StorageError> {
        let mid = id.to_string();
        self.storage.with(move |c| repo::delete_message(c, &mid)).await?;
        self.bus.publish(Event::MessageRemoved {
            session_id: session_id.into(),
            message_id: id.into(),
        });
        Ok(())
    }

    pub async fn update_part(&self, part: Part) -> Result<Part, StorageError> {
        let p = part.clone();
        self.storage.with(move |c| repo::upsert_part(c, &p)).await?;
        self.bus.publish(Event::PartUpdated {
            session_id: part.session_id.clone(),
            part: part.clone(),
        });
        Ok(part)
    }

    /// Persist a streaming delta on a text-bearing part without re-sending the
    /// whole part to subscribers.
    pub async fn update_part_delta(&self, part: &Part, field: &str, delta: &str) -> Result<(), StorageError> {
        let p = part.clone();
        self.storage.with(move |c| repo::upsert_part(c, &p)).await?;
        self.bus.publish(Event::PartDelta {
            session_id: part.session_id.clone(),
            message_id: part.message_id.clone(),
            part_id: part.id.clone(),
            field: field.into(),
            delta: delta.into(),
        });
        Ok(())
    }

    pub async fn remove_part(&self, part: &Part) -> Result<(), StorageError> {
        let id = part.id.clone();
        self.storage.with(move |c| repo::delete_part(c, &id)).await?;
        self.bus.publish(Event::PartRemoved {
            session_id: part.session_id.clone(),
            message_id: part.message_id.clone(),
            part_id: part.id.clone(),
        });
        Ok(())
    }

    pub async fn parts(&self, message_id: &str) -> Result<Vec<Part>, StorageError> {
        let id = message_id.to_string();
        self.storage.with(move |c| repo::list_parts(c, &id)).await
    }

    pub async fn messages(
        &self,
        session_id: &str,
        limit: Option<usize>,
        before: Option<String>,
    ) -> Result<Vec<MessageWithParts>, StorageError> {
        let sid = session_id.to_string();
        self.storage
            .with(move |c| repo::messages_with_parts(c, &sid, limit, before.as_deref()))
            .await
    }

    pub async fn todos(&self, session_id: &str) -> Result<Vec<Todo>, StorageError> {
        let sid = session_id.to_string();
        self.storage.with(move |c| repo::list_todos(c, &sid)).await
    }

    pub async fn set_todos(&self, session_id: &str, todos: Vec<Todo>) -> Result<(), StorageError> {
        let sid = session_id.to_string();
        let t = todos.clone();
        self.storage
            .with(move |c| repo::replace_todos(c, &sid, &t))
            .await?;
        self.bus.publish(Event::TodoUpdated {
            session_id: session_id.into(),
            todos,
        });
        Ok(())
    }

    pub fn new_part(&self, session_id: &str, message_id: &str, kind: PartKind) -> Part {
        Part {
            id: ids::ascending(Prefix::Part),
            session_id: session_id.into(),
            message_id: message_id.into(),
            kind,
        }
    }
}
