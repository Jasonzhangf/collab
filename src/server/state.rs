use serde::{Deserialize, Serialize};
use std::collections::HashMap;

pub fn now_ms() -> i64 {
    chrono::Utc::now().timestamp_millis()
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WorkerRec {
    pub id: String,
    pub token: String,
    pub pane: Option<String>,
    pub cwd: String,
    pub registered_ms: i64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Message {
    pub id: String,
    pub from: String,
    pub to: String,
    #[serde(rename = "type")]
    pub mtype: String,
    pub body: String,
    pub in_reply_to: Option<String>,
    pub created_ms: i64,
    /// pending -> delivered -> read
    pub state: String,
    pub nudge_count: u32,
    pub last_nudge_ms: i64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct QueueEntry {
    pub worker_id: String,
    pub since_ms: i64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ClaimRec {
    pub id: String,
    pub owner: Option<String>,
    pub intent: Option<String>,
    pub acquired_ms: i64,
    pub lease_until_ms: i64,
    pub expired_notified: bool,
    /// FIFO fairness: after release the claim is reserved for the longest-waiting requester.
    pub reserved_for: Option<String>,
    pub queue: Vec<QueueEntry>,
}

impl ClaimRec {
    pub fn lease_expired(&self, now: i64) -> bool {
        self.owner.is_some() && self.lease_until_ms < now
    }
}

/// Journal events. Every mutation is an event: live path applies + appends,
/// replay applies only. This is what makes restart recovery deterministic.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "ev")]
pub enum Event {
    Registered { worker: WorkerRec },
    Sent { msg: Message },
    Delivered { ids: Vec<String> },
    Acked { ids: Vec<String> },
    ClaimAcquired { id: String, owner: String, intent: Option<String>, lease_until_ms: i64, at_ms: i64 },
    ClaimReleased { id: String, by: String, reserved_for: Option<String> },
    ClaimRenewed { id: String, lease_until_ms: i64 },
    ClaimExpiredNotified { id: String },
    ClaimQueued { id: String, worker_id: String },
    Nudged { msg_id: String },
}

#[derive(Default)]
pub struct State {
    pub workers: HashMap<String, WorkerRec>,
    pub msgs: HashMap<String, Message>,
    pub claims: HashMap<String, ClaimRec>,
}

impl State {
    pub fn apply(&mut self, ev: &Event) {
        match ev {
            Event::Registered { worker } => {
                self.workers.insert(worker.id.clone(), worker.clone());
            }
            Event::Sent { msg } => {
                self.msgs.insert(msg.id.clone(), msg.clone());
            }
            Event::Delivered { ids } => {
                for id in ids {
                    if let Some(m) = self.msgs.get_mut(id) {
                        if m.state == "pending" {
                            m.state = "delivered".into();
                        }
                    }
                }
            }
            Event::Acked { ids } => {
                for id in ids {
                    if let Some(m) = self.msgs.get_mut(id) {
                        m.state = "read".into();
                    }
                }
            }
            Event::ClaimAcquired { id, owner, intent, lease_until_ms, at_ms } => {
                let rec = self.claims.entry(id.clone()).or_insert_with(|| ClaimRec {
                    id: id.clone(),
                    owner: None,
                    intent: None,
                    acquired_ms: *at_ms,
                    lease_until_ms: 0,
                    expired_notified: false,
                    reserved_for: None,
                    queue: Vec::new(),
                });
                rec.owner = Some(owner.clone());
                rec.intent = intent.clone();
                rec.acquired_ms = *at_ms;
                rec.lease_until_ms = *lease_until_ms;
                rec.expired_notified = false;
                // consume the FIFO reservation if it was ours
                if rec.reserved_for.as_deref() == Some(owner.as_str()) {
                    rec.reserved_for = None;
                }
            }
            Event::ClaimReleased { id, reserved_for, .. } => {
                if let Some(c) = self.claims.get_mut(id) {
                    c.owner = None;
                    c.intent = None;
                    c.expired_notified = false;
                    c.reserved_for = reserved_for.clone();
                }
            }
            Event::ClaimRenewed { id, lease_until_ms } => {
                if let Some(c) = self.claims.get_mut(id) {
                    c.lease_until_ms = *lease_until_ms;
                    c.expired_notified = false;
                }
            }
            Event::ClaimExpiredNotified { id } => {
                if let Some(c) = self.claims.get_mut(id) {
                    c.expired_notified = true;
                }
            }
            Event::ClaimQueued { id, worker_id } => {
                if let Some(c) = self.claims.get_mut(id) {
                    if !c.queue.iter().any(|q| q.worker_id == *worker_id) {
                        c.queue.push(QueueEntry { worker_id: worker_id.clone(), since_ms: now_ms() });
                    }
                }
            }
            Event::Nudged { msg_id } => {
                if let Some(m) = self.msgs.get_mut(msg_id) {
                    m.nudge_count += 1;
                    m.last_nudge_ms = now_ms();
                }
            }
        }
    }

    /// Unread (not yet acked) inbox of a worker, oldest first.
    pub fn inbox_of(&self, worker_id: &str) -> Vec<&Message> {
        let mut v: Vec<&Message> = self
            .msgs
            .values()
            .filter(|m| m.to == worker_id && m.state != "read")
            .collect();
        v.sort_by_key(|m| m.created_ms);
        v
    }

    /// True when some other message is a reply to `msg`.
    pub fn answered(&self, msg_id: &str) -> bool {
        self.msgs.values().any(|m| m.in_reply_to.as_deref() == Some(msg_id))
    }

    pub fn worker_pane(&self, worker_id: &str) -> Option<String> {
        self.workers.get(worker_id)?.pane.clone()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn msg(id: &str, to: &str, mtype: &str) -> Message {
        Message {
            id: id.into(),
            from: "a".into(),
            to: to.into(),
            mtype: mtype.into(),
            body: "b".into(),
            in_reply_to: None,
            created_ms: 1,
            state: "pending".into(),
            nudge_count: 0,
            last_nudge_ms: 0,
        }
    }

    #[test]
    fn message_lifecycle() {
        let mut st = State::default();
        st.apply(&Event::Sent { msg: msg("m1", "w2", "request") });
        assert_eq!(st.inbox_of("w2").len(), 1);
        assert!(st.inbox_of("w1").is_empty());

        st.apply(&Event::Delivered { ids: vec!["m1".into()] });
        assert_eq!(st.msgs["m1"].state, "delivered");
        assert_eq!(st.inbox_of("w2").len(), 1);

        st.apply(&Event::Acked { ids: vec!["m1".into()] });
        assert_eq!(st.msgs["m1"].state, "read");
        assert!(st.inbox_of("w2").is_empty());
    }

    #[test]
    fn answered_detection() {
        let mut st = State::default();
        st.apply(&Event::Sent { msg: msg("m1", "w2", "request") });
        let mut reply = msg("m2", "w1", "reply");
        reply.in_reply_to = Some("m1".into());
        st.apply(&Event::Sent { msg: reply });
        assert!(st.answered("m1"));
        assert!(!st.answered("m2"));
    }

    #[test]
    fn claim_lease_expiry() {
        let mut st = State::default();
        st.apply(&Event::ClaimAcquired { id: "build".into(), owner: "w1".into(), intent: Some("x".into()), lease_until_ms: 100, at_ms: 50 });
        assert!(st.claims["build"].lease_expired(101));
        assert!(!st.claims["build"].lease_expired(99));
        st.apply(&Event::ClaimRenewed { id: "build".into(), lease_until_ms: 500 });
        assert!(!st.claims["build"].lease_expired(101));
    }

    #[test]
    fn claim_release_reserves_fifo_head() {
        let mut st = State::default();
        st.apply(&Event::ClaimAcquired { id: "build".into(), owner: "w1".into(), intent: None, lease_until_ms: 1_000_000, at_ms: 1 });
        st.apply(&Event::ClaimQueued { id: "build".into(), worker_id: "w2".into() });
        st.apply(&Event::ClaimQueued { id: "build".into(), worker_id: "w3".into() });
        st.apply(&Event::ClaimReleased { id: "build".into(), by: "w1".into(), reserved_for: Some("w2".into()) });
        let c = &st.claims["build"];
        assert_eq!(c.reserved_for.as_deref(), Some("w2"));
        assert_eq!(c.queue.len(), 2);
        // acquiring by the non-reserved worker must be blocked by the handler;
        // state-level: reservation persists until the reserved owner acquires
        st.apply(&Event::ClaimAcquired { id: "build".into(), owner: "w2".into(), intent: None, lease_until_ms: 1_000_000, at_ms: 2 });
        assert!(st.claims["build"].reserved_for.is_none());
    }
}
