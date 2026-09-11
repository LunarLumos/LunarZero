//! SQLite persistence. A single writer thread owns the connection; callers
//! submit closures via [`Storage::with`]. Reads and writes are serialized,
//! which is plenty for a single-user CLI and avoids `SQLITE_BUSY` entirely.

pub mod migrate;
pub mod repo;

use std::path::Path;
use std::sync::Arc;

use rusqlite::Connection;
use tokio::sync::{mpsc, oneshot};

type Job = Box<dyn FnOnce(&mut Connection) + Send + 'static>;

#[derive(Clone)]
pub struct Storage {
    tx: mpsc::UnboundedSender<Job>,
}

#[derive(Debug, thiserror::Error)]
pub enum StorageError {
    #[error("sqlite: {0}")]
    Sqlite(#[from] rusqlite::Error),
    #[error("json: {0}")]
    Json(#[from] serde_json::Error),
    #[error("storage thread stopped")]
    Closed,
    #[error("not found: {0}")]
    NotFound(String),
}

pub type StorageResult<T> = Result<T, StorageError>;

impl Storage {
    pub fn open(path: &Path) -> StorageResult<Self> {
        if let Some(parent) = path.parent() {
            let _ = std::fs::create_dir_all(parent);
        }
        let mut conn = Connection::open(path)?;
        Self::configure(&conn)?;
        migrate::apply(&mut conn)?;
        Ok(Self::spawn(conn))
    }

    pub fn open_in_memory() -> StorageResult<Self> {
        let mut conn = Connection::open_in_memory()?;
        Self::configure(&conn)?;
        migrate::apply(&mut conn)?;
        Ok(Self::spawn(conn))
    }

    fn configure(conn: &Connection) -> StorageResult<()> {
        conn.execute_batch(
            "PRAGMA journal_mode = WAL;
             PRAGMA synchronous = NORMAL;
             PRAGMA busy_timeout = 5000;
             PRAGMA foreign_keys = ON;
             PRAGMA temp_store = MEMORY;",
        )?;
        Ok(())
    }

    fn spawn(mut conn: Connection) -> Self {
        let (tx, mut rx) = mpsc::unbounded_channel::<Job>();
        std::thread::Builder::new()
            .name("lz-storage".into())
            .spawn(move || {
                while let Some(job) = rx.blocking_recv() {
                    job(&mut conn);
                }
            })
            .expect("spawn storage thread");
        Self { tx }
    }

    /// Run `f` on the storage thread and await its result.
    pub async fn with<T, F>(&self, f: F) -> StorageResult<T>
    where
        T: Send + 'static,
        F: FnOnce(&mut Connection) -> StorageResult<T> + Send + 'static,
    {
        let (tx, rx) = oneshot::channel();
        self.tx
            .send(Box::new(move |conn| {
                let _ = tx.send(f(conn));
            }))
            .map_err(|_| StorageError::Closed)?;
        rx.await.map_err(|_| StorageError::Closed)?
    }

    /// Blocking variant for synchronous call sites (CLI setup, tests).
    pub fn with_blocking<T, F>(&self, f: F) -> StorageResult<T>
    where
        T: Send + 'static,
        F: FnOnce(&mut Connection) -> StorageResult<T> + Send + 'static,
    {
        let (tx, rx) = std::sync::mpsc::channel();
        self.tx
            .send(Box::new(move |conn| {
                let _ = tx.send(f(conn));
            }))
            .map_err(|_| StorageError::Closed)?;
        rx.recv().map_err(|_| StorageError::Closed)?
    }
}

pub type SharedStorage = Arc<Storage>;

pub fn now_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}
