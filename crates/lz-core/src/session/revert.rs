//! `/undo` and `/redo`: revert the worktree to the snapshot taken before a
//! message and stage the removal of later messages (applied on next prompt).

use lz_schema::Event;
use lz_schema::session::*;

use crate::engine::Engine;

pub async fn revert(
    engine: &Engine,
    session_id: &str,
    message_id: &str,
    part_id: Option<&str>,
) -> anyhow::Result<SessionInfo> {
    if engine.runner.is_running(session_id) {
        anyhow::bail!("session is busy");
    }
    let all = engine.sessions.messages(session_id, None, None).await?;
    let session = engine.sessions.get(session_id).await?;
    let mut last_user: Option<String> = None;
    let mut rev: Option<SessionRevert> = None;
    let mut patches: Vec<(String, Vec<String>)> = Vec::new();
    for m in &all {
        if let Message::User(u) = &m.info {
            last_user = Some(u.id.clone());
        }
        let mut remaining: Vec<&Part> = Vec::new();
        for p in &m.parts {
            if rev.is_some() {
                if let PartKind::Patch { hash, files } = &p.kind {
                    patches.push((hash.clone(), files.clone()));
                }
                continue;
            }
            if (m.info.id() == message_id && part_id.is_none()) || part_id == Some(p.id.as_str()) {
                let pid = if remaining
                    .iter()
                    .any(|x| matches!(x.kind, PartKind::Text { .. } | PartKind::Tool { .. }))
                {
                    part_id.map(str::to_string)
                } else {
                    None
                };
                rev = Some(SessionRevert {
                    message_id: if pid.is_none() {
                        last_user.clone().unwrap_or_else(|| m.info.id().to_string())
                    } else {
                        m.info.id().to_string()
                    },
                    part_id: pid,
                    snapshot: None,
                    diff: None,
                });
            }
            remaining.push(p);
        }
    }
    let Some(mut rev) = rev else { return Ok(session) };
    rev.snapshot = match session.revert.as_ref().and_then(|r| r.snapshot.clone()) {
        Some(s) => Some(s),
        None => engine.snapshot.track().await,
    };
    if let Some(prev) = session.revert.as_ref().and_then(|r| r.snapshot.clone()) {
        let _ = engine.snapshot.restore(&prev).await;
    }
    engine.snapshot.revert(&patches).await;
    let diffs = match &rev.snapshot {
        Some(s) => engine.snapshot.diff(s, None).await,
        None => Vec::new(),
    };
    rev.diff = Some(
        diffs
            .iter()
            .filter_map(|d| d.patch.clone())
            .collect::<Vec<_>>()
            .join("\n"),
    );
    engine.bus.publish(Event::SessionDiff {
        session_id: session_id.into(),
        diff: diffs.clone(),
    });
    let summary = SessionSummary {
        additions: diffs.iter().map(|d| d.additions).sum(),
        deletions: diffs.iter().map(|d| d.deletions).sum(),
        files: diffs.len() as f64,
        diffs: None,
    };
    let updated = engine
        .sessions
        .modify(session_id, move |s| {
            s.revert = Some(rev);
            s.summary = Some(summary);
        })
        .await?;
    Ok(updated)
}

pub async fn unrevert(engine: &Engine, session_id: &str) -> anyhow::Result<SessionInfo> {
    if engine.runner.is_running(session_id) {
        anyhow::bail!("session is busy");
    }
    let session = engine.sessions.get(session_id).await?;
    let Some(rev) = &session.revert else {
        return Ok(session);
    };
    if let Some(s) = &rev.snapshot {
        engine.snapshot.restore(s).await.map_err(|e| anyhow::anyhow!(e))?;
    }
    Ok(engine.sessions.modify(session_id, |s| s.revert = None).await?)
}

/// Drop the reverted messages/parts for real (called before the next prompt).
pub async fn cleanup(engine: &Engine, session: &SessionInfo) -> anyhow::Result<()> {
    let Some(rev) = &session.revert else { return Ok(()) };
    let msgs = engine.sessions.messages(&session.id, None, None).await?;
    let index = msgs.iter().position(|m| m.info.id() == rev.message_id);
    if let Some(i) = index {
        let start = i + usize::from(rev.part_id.is_some());
        for m in &msgs[start..] {
            engine.sessions.remove_message(&session.id, m.info.id()).await?;
        }
        if let Some(pid) = &rev.part_id {
            let target = &msgs[i];
            if let Some(idx) = target.parts.iter().position(|p| &p.id == pid) {
                for p in &target.parts[idx..] {
                    engine.sessions.remove_part(p).await?;
                }
            }
        }
    }
    engine.sessions.modify(&session.id, |s| s.revert = None).await?;
    Ok(())
}
