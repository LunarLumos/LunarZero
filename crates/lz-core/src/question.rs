//! "Ask the user" request queue backing the `question` tool.

use std::collections::BTreeMap;
use std::sync::Mutex;

use lz_schema::Event;
use lz_schema::ids::{self, Prefix};
use lz_schema::session::{QuestionInfo, QuestionRequest, ToolRef};
use tokio::sync::oneshot;

use crate::bus::Bus;

#[derive(Debug, Clone, thiserror::Error, PartialEq)]
pub enum QuestionError {
    #[error("The user dismissed the question without answering.")]
    Rejected,
}

struct Pending {
    request: QuestionRequest,
    tx: oneshot::Sender<Result<Vec<Vec<String>>, QuestionError>>,
}

#[derive(Default)]
pub struct Questions {
    pending: Mutex<BTreeMap<String, Pending>>,
}

impl Questions {
    pub fn pending(&self) -> Vec<QuestionRequest> {
        self.pending
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .values()
            .map(|p| p.request.clone())
            .collect()
    }

    pub async fn ask(
        &self,
        bus: &Bus,
        session_id: &str,
        questions: Vec<QuestionInfo>,
        tool: Option<ToolRef>,
    ) -> Result<Vec<Vec<String>>, QuestionError> {
        let request = QuestionRequest {
            id: ids::ascending(Prefix::Question),
            session_id: session_id.into(),
            questions,
            tool,
        };
        let (tx, rx) = oneshot::channel();
        self.pending.lock().unwrap_or_else(|e| e.into_inner()).insert(
            request.id.clone(),
            Pending {
                request: request.clone(),
                tx,
            },
        );
        bus.publish(Event::QuestionAsked(request));
        rx.await.unwrap_or(Err(QuestionError::Rejected))
    }

    pub fn reply(&self, bus: &Bus, id: &str, answers: Vec<Vec<String>>) -> Result<(), String> {
        let p = self
            .pending
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .remove(id)
            .ok_or_else(|| format!("no pending question {id}"))?;
        bus.publish(Event::QuestionReplied {
            session_id: p.request.session_id.clone(),
            request_id: id.into(),
            answers: answers.clone(),
        });
        let _ = p.tx.send(Ok(answers));
        Ok(())
    }

    pub fn reject(&self, bus: &Bus, id: &str) -> Result<(), String> {
        let p = self
            .pending
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .remove(id)
            .ok_or_else(|| format!("no pending question {id}"))?;
        bus.publish(Event::QuestionRejected {
            session_id: p.request.session_id.clone(),
            request_id: id.into(),
        });
        let _ = p.tx.send(Err(QuestionError::Rejected));
        Ok(())
    }

    pub fn cancel_session(&self, session_id: &str) {
        let mut m = self.pending.lock().unwrap_or_else(|e| e.into_inner());
        let ids: Vec<String> = m
            .iter()
            .filter(|(_, p)| p.request.session_id == session_id)
            .map(|(k, _)| k.clone())
            .collect();
        for id in ids {
            if let Some(p) = m.remove(&id) {
                let _ = p.tx.send(Err(QuestionError::Rejected));
            }
        }
    }
}
