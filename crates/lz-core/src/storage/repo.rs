//! Row-level access for sessions, messages, parts, todos, saved permissions
//! and the event log. All functions take the raw connection and run on the
//! storage thread.

use lz_schema::permission::{Action, Rule};
use lz_schema::session::*;
use rusqlite::{Connection, OptionalExtension, params};
use serde_json::Value;

use super::{StorageError, StorageResult, now_ms};

fn json<T: serde::Serialize>(v: &T) -> StorageResult<String> {
    Ok(serde_json::to_string(v)?)
}

fn opt_json<T: serde::Serialize>(v: &Option<T>) -> StorageResult<Option<String>> {
    v.as_ref().map(json).transpose()
}

fn parse_opt<T: serde::de::DeserializeOwned>(s: Option<String>) -> Option<T> {
    s.and_then(|s| serde_json::from_str(&s).ok())
}

// ───────────────────────────── project ─────────────────────────────

pub fn upsert_project(conn: &Connection, id: &str, worktree: &str, vcs: Option<&str>) -> StorageResult<()> {
    let now = now_ms() as i64;
    conn.execute(
        "INSERT INTO project (id, worktree, vcs, time_created, time_updated)
         VALUES (?1, ?2, ?3, ?4, ?4)
         ON CONFLICT(id) DO UPDATE SET worktree = excluded.worktree, vcs = excluded.vcs, time_updated = excluded.time_updated",
        params![id, worktree, vcs, now],
    )?;
    Ok(())
}

// ───────────────────────────── session ─────────────────────────────

fn row_to_session(r: &rusqlite::Row<'_>) -> rusqlite::Result<SessionInfo> {
    Ok(SessionInfo {
        id: r.get("id")?,
        project_id: r.get("project_id")?,
        parent_id: r.get("parent_id")?,
        slug: r.get("slug")?,
        directory: r.get("directory")?,
        title: r.get("title")?,
        version: r.get("version")?,
        agent: r.get("agent")?,
        model: parse_opt(r.get("model")?),
        summary: parse_opt(r.get("summary")?),
        cost: r.get("cost")?,
        tokens: parse_opt(r.get("tokens")?),
        metadata: parse_opt(r.get("metadata")?),
        permission: parse_opt(r.get("permission")?),
        mode: r
            .get::<_, Option<String>>("mode")?
            .and_then(|m| lz_schema::permission::PermissionMode::parse(&m)),
        revert: parse_opt(r.get("revert")?),
        time: SessionTime {
            created: r.get::<_, i64>("time_created")? as u64,
            updated: r.get::<_, i64>("time_updated")? as u64,
            compacting: r.get::<_, Option<i64>>("time_compacting")?.map(|v| v as u64),
            archived: r.get("time_archived")?,
        },
    })
}

const SESSION_COLS: &str = "id, project_id, parent_id, slug, directory, title, version, agent, model, summary, cost, tokens, metadata, permission, revert, time_created, time_updated, time_compacting, time_archived, mode";

pub fn upsert_session(conn: &Connection, s: &SessionInfo) -> StorageResult<()> {
    conn.execute(
        &format!(
            "INSERT INTO session ({SESSION_COLS}) VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,?12,?13,?14,?15,?16,?17,?18,?19,?20)
             ON CONFLICT(id) DO UPDATE SET
               project_id=excluded.project_id, parent_id=excluded.parent_id, slug=excluded.slug, directory=excluded.directory,
               title=excluded.title, version=excluded.version, agent=excluded.agent, model=excluded.model, summary=excluded.summary,
               cost=excluded.cost, tokens=excluded.tokens, metadata=excluded.metadata, permission=excluded.permission,
               revert=excluded.revert, time_created=excluded.time_created, time_updated=excluded.time_updated,
               time_compacting=excluded.time_compacting, time_archived=excluded.time_archived, mode=excluded.mode"
        ),
        params![
            s.id,
            s.project_id,
            s.parent_id,
            s.slug,
            s.directory,
            s.title,
            s.version,
            s.agent,
            opt_json(&s.model)?,
            opt_json(&s.summary)?,
            s.cost,
            opt_json(&s.tokens)?,
            opt_json(&s.metadata)?,
            opt_json(&s.permission)?,
            opt_json(&s.revert)?,
            s.time.created as i64,
            s.time.updated as i64,
            s.time.compacting.map(|v| v as i64),
            s.time.archived,
            s.mode.map(|m| m.id()),
        ],
    )?;
    Ok(())
}

pub fn get_session(conn: &Connection, id: &str) -> StorageResult<SessionInfo> {
    conn.query_row(
        &format!("SELECT {SESSION_COLS} FROM session WHERE id = ?1"),
        [id],
        row_to_session,
    )
    .optional()?
    .ok_or_else(|| StorageError::NotFound(format!("session {id}")))
}

pub struct SessionFilter<'a> {
    pub project_id: Option<&'a str>,
    pub parent_id: Option<Option<&'a str>>,
    pub search: Option<&'a str>,
    pub limit: Option<usize>,
    pub include_archived: bool,
}

pub fn list_sessions(conn: &Connection, f: SessionFilter<'_>) -> StorageResult<Vec<SessionInfo>> {
    let mut sql = format!("SELECT {SESSION_COLS} FROM session WHERE 1=1");
    let mut args: Vec<Box<dyn rusqlite::ToSql>> = Vec::new();
    if let Some(p) = f.project_id {
        sql.push_str(" AND project_id = ?");
        args.push(Box::new(p.to_string()));
    }
    match f.parent_id {
        Some(Some(p)) => {
            sql.push_str(" AND parent_id = ?");
            args.push(Box::new(p.to_string()));
        }
        Some(None) => sql.push_str(" AND parent_id IS NULL"),
        None => {}
    }
    if let Some(s) = f.search {
        sql.push_str(" AND title LIKE ?");
        args.push(Box::new(format!("%{s}%")));
    }
    if !f.include_archived {
        sql.push_str(" AND time_archived IS NULL");
    }
    sql.push_str(" ORDER BY time_updated DESC");
    if let Some(l) = f.limit {
        sql.push_str(&format!(" LIMIT {l}"));
    }
    let mut stmt = conn.prepare(&sql)?;
    let rows = stmt.query_map(
        rusqlite::params_from_iter(args.iter().map(|a| a.as_ref())),
        row_to_session,
    )?;
    Ok(rows.collect::<Result<Vec<_>, _>>()?)
}

pub fn delete_session(conn: &Connection, id: &str) -> StorageResult<()> {
    conn.execute("DELETE FROM session WHERE id = ?1", [id])?;
    Ok(())
}

// ───────────────────────────── message / part ─────────────────────────────

pub fn upsert_message(conn: &Connection, m: &Message) -> StorageResult<()> {
    conn.execute(
        "INSERT INTO message (id, session_id, time_created, data) VALUES (?1, ?2, ?3, ?4)
         ON CONFLICT(id) DO UPDATE SET data = excluded.data",
        params![m.id(), m.session_id(), m.created() as i64, json(m)?],
    )?;
    Ok(())
}

pub fn get_message(conn: &Connection, id: &str) -> StorageResult<Message> {
    let data: Option<String> = conn
        .query_row("SELECT data FROM message WHERE id = ?1", [id], |r| r.get(0))
        .optional()?;
    let data = data.ok_or_else(|| StorageError::NotFound(format!("message {id}")))?;
    Ok(serde_json::from_str(&data)?)
}

pub fn delete_message(conn: &Connection, id: &str) -> StorageResult<()> {
    conn.execute("DELETE FROM message WHERE id = ?1", [id])?;
    Ok(())
}

pub fn list_messages(
    conn: &Connection,
    session_id: &str,
    limit: Option<usize>,
    before: Option<&str>,
) -> StorageResult<Vec<Message>> {
    let mut sql = String::from("SELECT data FROM message WHERE session_id = ?1");
    if before.is_some() {
        sql.push_str(" AND id < ?2");
    }
    // newest-first when limiting, then reverse
    sql.push_str(if limit.is_some() {
        " ORDER BY id DESC"
    } else {
        " ORDER BY id ASC"
    });
    if let Some(l) = limit {
        sql.push_str(&format!(" LIMIT {l}"));
    }
    let mut stmt = conn.prepare(&sql)?;
    let mut out: Vec<Message> = match before {
        Some(b) => stmt
            .query_map(params![session_id, b], |r| r.get::<_, String>(0))?
            .map(|s| Ok(serde_json::from_str(&s?)?))
            .collect::<StorageResult<Vec<_>>>()?,
        None => stmt
            .query_map(params![session_id], |r| r.get::<_, String>(0))?
            .map(|s| Ok(serde_json::from_str(&s?)?))
            .collect::<StorageResult<Vec<_>>>()?,
    };
    if limit.is_some() {
        out.reverse();
    }
    Ok(out)
}

pub fn upsert_part(conn: &Connection, p: &Part) -> StorageResult<()> {
    conn.execute(
        "INSERT INTO part (id, message_id, session_id, time_created, data) VALUES (?1, ?2, ?3, ?4, ?5)
         ON CONFLICT(id) DO UPDATE SET data = excluded.data",
        params![p.id, p.message_id, p.session_id, now_ms() as i64, json(p)?],
    )?;
    Ok(())
}

pub fn delete_part(conn: &Connection, id: &str) -> StorageResult<()> {
    conn.execute("DELETE FROM part WHERE id = ?1", [id])?;
    Ok(())
}

pub fn list_parts(conn: &Connection, message_id: &str) -> StorageResult<Vec<Part>> {
    let mut stmt = conn.prepare("SELECT data FROM part WHERE message_id = ?1 ORDER BY id ASC")?;
    stmt.query_map([message_id], |r| r.get::<_, String>(0))?
        .map(|s| Ok(serde_json::from_str(&s?)?))
        .collect()
}

/// All parts for a session, grouped by message id, ordered by part id.
pub fn list_session_parts(conn: &Connection, session_id: &str) -> StorageResult<Vec<Part>> {
    let mut stmt = conn.prepare("SELECT data FROM part WHERE session_id = ?1 ORDER BY id ASC")?;
    stmt.query_map([session_id], |r| r.get::<_, String>(0))?
        .map(|s| Ok(serde_json::from_str(&s?)?))
        .collect()
}

pub fn messages_with_parts(
    conn: &Connection,
    session_id: &str,
    limit: Option<usize>,
    before: Option<&str>,
) -> StorageResult<Vec<MessageWithParts>> {
    let messages = list_messages(conn, session_id, limit, before)?;
    let parts = list_session_parts(conn, session_id)?;
    let mut by_msg: std::collections::HashMap<String, Vec<Part>> = std::collections::HashMap::new();
    for p in parts {
        by_msg.entry(p.message_id.clone()).or_default().push(p);
    }
    Ok(messages
        .into_iter()
        .map(|info| {
            let parts = by_msg.remove(info.id()).unwrap_or_default();
            MessageWithParts { info, parts }
        })
        .collect())
}

// ───────────────────────────── todo ─────────────────────────────

pub fn replace_todos(conn: &mut Connection, session_id: &str, todos: &[Todo]) -> StorageResult<()> {
    let tx = conn.transaction()?;
    tx.execute("DELETE FROM todo WHERE session_id = ?1", [session_id])?;
    for (i, t) in todos.iter().enumerate() {
        tx.execute(
            "INSERT INTO todo (session_id, position, content, status, priority) VALUES (?1, ?2, ?3, ?4, ?5)",
            params![session_id, i as i64, t.content, t.status, t.priority],
        )?;
    }
    tx.commit()?;
    Ok(())
}

pub fn list_todos(conn: &Connection, session_id: &str) -> StorageResult<Vec<Todo>> {
    let mut stmt =
        conn.prepare("SELECT content, status, priority FROM todo WHERE session_id = ?1 ORDER BY position")?;
    let rows = stmt.query_map([session_id], |r| {
        Ok(Todo {
            content: r.get(0)?,
            status: r.get(1)?,
            priority: r.get(2)?,
        })
    })?;
    Ok(rows.collect::<Result<Vec<_>, _>>()?)
}

// ───────────────────────────── saved permissions ─────────────────────────────

pub fn save_permission(conn: &Connection, id: &str, project_id: &str, rule: &Rule) -> StorageResult<()> {
    let action = match rule.action {
        Action::Allow => "allow",
        Action::Deny => "deny",
        Action::Ask => "ask",
    };
    conn.execute(
        "INSERT INTO permission (id, project_id, session_id, permission, pattern, action, time_created)
         VALUES (?1, ?2, NULL, ?3, ?4, ?5, ?6)",
        params![
            id,
            project_id,
            rule.permission,
            rule.pattern,
            action,
            now_ms() as i64
        ],
    )?;
    Ok(())
}

pub fn list_permissions(conn: &Connection, project_id: &str) -> StorageResult<Vec<Rule>> {
    let mut stmt = conn.prepare(
        "SELECT permission, pattern, action FROM permission WHERE project_id = ?1 ORDER BY time_created, id",
    )?;
    let rows = stmt.query_map([project_id], |r| {
        let action: String = r.get(2)?;
        Ok(Rule {
            permission: r.get(0)?,
            pattern: r.get(1)?,
            action: match action.as_str() {
                "allow" => Action::Allow,
                "deny" => Action::Deny,
                _ => Action::Ask,
            },
        })
    })?;
    Ok(rows.collect::<Result<Vec<_>, _>>()?)
}

pub fn clear_permissions(conn: &Connection, project_id: &str) -> StorageResult<()> {
    conn.execute("DELETE FROM permission WHERE project_id = ?1", [project_id])?;
    Ok(())
}

// ───────────────────────────── event log / kv ─────────────────────────────

pub fn append_event(conn: &Connection, aggregate_id: &str, kind: &str, data: &Value) -> StorageResult<i64> {
    conn.execute(
        "INSERT INTO event (aggregate_id, type, data, time_created) VALUES (?1, ?2, ?3, ?4)",
        params![aggregate_id, kind, data.to_string(), now_ms() as i64],
    )?;
    Ok(conn.last_insert_rowid())
}

pub fn kv_get(conn: &Connection, key: &str) -> StorageResult<Option<Value>> {
    let v: Option<String> = conn
        .query_row("SELECT value FROM kv WHERE key = ?1", [key], |r| r.get(0))
        .optional()?;
    Ok(v.and_then(|s| serde_json::from_str(&s).ok()))
}

pub fn kv_set(conn: &Connection, key: &str, value: &Value) -> StorageResult<()> {
    conn.execute(
        "INSERT INTO kv (key, value) VALUES (?1, ?2) ON CONFLICT(key) DO UPDATE SET value = excluded.value",
        params![key, value.to_string()],
    )?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::storage::migrate;

    fn conn() -> Connection {
        let mut c = Connection::open_in_memory().unwrap();
        c.execute_batch("PRAGMA foreign_keys = ON;").unwrap();
        migrate::apply(&mut c).unwrap();
        c
    }

    fn session(id: &str) -> SessionInfo {
        SessionInfo {
            id: id.into(),
            slug: "s".into(),
            project_id: "p".into(),
            directory: "/tmp".into(),
            parent_id: None,
            summary: None,
            cost: None,
            tokens: None,
            title: "t".into(),
            agent: None,
            model: None,
            version: "0".into(),
            metadata: None,
            time: SessionTime {
                created: 1,
                updated: 1,
                compacting: None,
                archived: None,
            },
            permission: None,
            mode: None,
            revert: None,
        }
    }

    #[test]
    fn session_message_part_roundtrip() {
        let mut c = conn();
        upsert_session(&c, &session("ses_1")).unwrap();
        let user = Message::User(UserMessage {
            id: "msg_1".into(),
            session_id: "ses_1".into(),
            time: UserTime { created: 1 },
            format: None,
            summary: None,
            agent: "build".into(),
            model: ModelRef {
                provider_id: "openai".into(),
                model_id: "gpt".into(),
                variant: None,
            },
            system: None,
            tools: None,
        });
        upsert_message(&c, &user).unwrap();
        let part = Part {
            id: "prt_1".into(),
            session_id: "ses_1".into(),
            message_id: "msg_1".into(),
            kind: PartKind::Text {
                text: "hi".into(),
                synthetic: false,
                ignored: false,
                time: None,
                metadata: None,
            },
        };
        upsert_part(&c, &part).unwrap();
        let got = messages_with_parts(&c, "ses_1", None, None).unwrap();
        assert_eq!(got.len(), 1);
        assert_eq!(got[0].parts[0], part);
        replace_todos(
            &mut c,
            "ses_1",
            &[Todo {
                content: "x".into(),
                status: "pending".into(),
                priority: "high".into(),
            }],
        )
        .unwrap();
        assert_eq!(list_todos(&c, "ses_1").unwrap().len(), 1);
        // cascade
        delete_session(&c, "ses_1").unwrap();
        assert!(list_parts(&c, "msg_1").unwrap().is_empty());
    }
}
