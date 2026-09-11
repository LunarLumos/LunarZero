//! Ordered, forward-only migrations tracked in `PRAGMA user_version`.

use rusqlite::Connection;

use super::StorageResult;

const MIGRATIONS: &[&str] = &[
    // 1: initial schema
    "CREATE TABLE IF NOT EXISTS project (
        id TEXT PRIMARY KEY,
        worktree TEXT NOT NULL,
        vcs TEXT,
        name TEXT,
        time_created INTEGER NOT NULL,
        time_updated INTEGER NOT NULL,
        time_initialized INTEGER
    );
    CREATE TABLE IF NOT EXISTS session (
        id TEXT PRIMARY KEY,
        project_id TEXT NOT NULL,
        parent_id TEXT,
        slug TEXT NOT NULL,
        directory TEXT NOT NULL,
        title TEXT NOT NULL,
        version TEXT NOT NULL,
        agent TEXT,
        model TEXT,
        summary TEXT,
        cost REAL,
        tokens TEXT,
        metadata TEXT,
        permission TEXT,
        revert TEXT,
        time_created INTEGER NOT NULL,
        time_updated INTEGER NOT NULL,
        time_compacting INTEGER,
        time_archived REAL
    );
    CREATE INDEX IF NOT EXISTS session_project_idx ON session(project_id, time_updated);
    CREATE INDEX IF NOT EXISTS session_parent_idx ON session(parent_id);
    CREATE TABLE IF NOT EXISTS message (
        id TEXT PRIMARY KEY,
        session_id TEXT NOT NULL REFERENCES session(id) ON DELETE CASCADE,
        time_created INTEGER NOT NULL,
        data TEXT NOT NULL
    );
    CREATE INDEX IF NOT EXISTS message_session_idx ON message(session_id, id);
    CREATE TABLE IF NOT EXISTS part (
        id TEXT PRIMARY KEY,
        message_id TEXT NOT NULL REFERENCES message(id) ON DELETE CASCADE,
        session_id TEXT NOT NULL,
        time_created INTEGER NOT NULL,
        data TEXT NOT NULL
    );
    CREATE INDEX IF NOT EXISTS part_message_idx ON part(message_id, id);
    CREATE INDEX IF NOT EXISTS part_session_idx ON part(session_id);
    CREATE TABLE IF NOT EXISTS todo (
        session_id TEXT NOT NULL REFERENCES session(id) ON DELETE CASCADE,
        position INTEGER NOT NULL,
        content TEXT NOT NULL,
        status TEXT NOT NULL,
        priority TEXT NOT NULL,
        PRIMARY KEY (session_id, position)
    );
    CREATE TABLE IF NOT EXISTS permission (
        id TEXT PRIMARY KEY,
        project_id TEXT NOT NULL,
        session_id TEXT,
        permission TEXT NOT NULL,
        pattern TEXT NOT NULL,
        action TEXT NOT NULL,
        time_created INTEGER NOT NULL
    );
    CREATE INDEX IF NOT EXISTS permission_project_idx ON permission(project_id);
    CREATE TABLE IF NOT EXISTS event (
        seq INTEGER PRIMARY KEY AUTOINCREMENT,
        aggregate_id TEXT NOT NULL,
        type TEXT NOT NULL,
        data TEXT NOT NULL,
        time_created INTEGER NOT NULL
    );
    CREATE INDEX IF NOT EXISTS event_aggregate_idx ON event(aggregate_id, seq);
    CREATE TABLE IF NOT EXISTS kv (
        key TEXT PRIMARY KEY,
        value TEXT NOT NULL
    );",
];

pub fn apply(conn: &mut Connection) -> StorageResult<()> {
    let current: i64 = conn.query_row("PRAGMA user_version", [], |r| r.get(0))?;
    for (i, sql) in MIGRATIONS.iter().enumerate() {
        let version = (i + 1) as i64;
        if version <= current {
            continue;
        }
        let tx = conn.transaction()?;
        tx.execute_batch(sql)?;
        tx.pragma_update(None, "user_version", version)?;
        tx.commit()?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn applies_once() {
        let mut conn = Connection::open_in_memory().unwrap();
        apply(&mut conn).unwrap();
        apply(&mut conn).unwrap();
        let v: i64 = conn.query_row("PRAGMA user_version", [], |r| r.get(0)).unwrap();
        assert_eq!(v as usize, MIGRATIONS.len());
    }
}
