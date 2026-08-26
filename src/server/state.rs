use serde::{Deserialize, Serialize};
use std::collections::HashMap;

pub fn now_ms() -> i64 {
    chrono::Utc::now().timestamp_millis()
}

pub fn default_role() -> String {
    "worker".into()
}

pub fn default_task_status() -> String {
    "working".into()
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WorkerRec {
    pub id: String,
    pub token: String,
    pub pane: Option<String>,
    pub cwd: String,
    pub registered_ms: i64,
    #[serde(default = "default_role")]
    pub role: String,
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
    /// pending -> delivered -> read; replies may also become superseded.
    pub state: String,
    pub nudge_count: u32,
    pub last_nudge_ms: i64,
}

pub const REQUEST_COOLDOWN_MS: i64 = 5 * 60 * 1000;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TaskRec {
    pub id: String,
    pub owner: String,
    pub created_by: String,
    #[serde(default)]
    pub feature_id: Option<String>,
    #[serde(default)]
    pub worktree_path: Option<String>,
    #[serde(default)]
    pub branch: Option<String>,
    #[serde(default)]
    pub base_commit: Option<String>,
    #[serde(default = "default_task_status")]
    pub status: String,
    #[serde(default)]
    pub next_step: Option<String>,
    pub created_ms: i64,
    pub updated_ms: i64,
    pub last_heartbeat_sent_ms: i64,
    pub heartbeat_pending: bool,
    #[serde(default)]
    pub heartbeat_message_id: Option<String>,
    pub heartbeat_stale_notified: bool,
}

pub fn task_heartbeat_active(status: &str) -> bool {
    !matches!(status, "delivered" | "merged" | "closed" | "cancelled")
}

pub fn task_resource_active(status: &str) -> bool {
    !matches!(status, "merged" | "closed" | "cancelled")
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
    Nudged { msg_id: String },
    Superseded { ids: Vec<String> },
    TaskCreated { task: TaskRec },
    TaskUpdated { task: TaskRec },
}

#[derive(Default)]
pub struct State {
    pub workers: HashMap<String, WorkerRec>,
    pub msgs: HashMap<String, Message>,
    pub tasks: HashMap<String, TaskRec>,
}

impl State {
    pub fn apply(&mut self, ev: &Event) {
        match ev {
            Event::Registered { worker } => {
                let worker = if self.workers.is_empty() && worker.role != "master" {
                    WorkerRec {
                        role: "master".into(),
                        ..worker.clone()
                    }
                } else {
                    worker.clone()
                };
                self.workers.insert(worker.id.clone(), worker);
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
            Event::Superseded { ids } => {
                for id in ids {
                    if let Some(m) = self.msgs.get_mut(id) {
                        m.state = "superseded".into();
                    }
                }
            }
            Event::Nudged { msg_id } => {
                if let Some(m) = self.msgs.get_mut(msg_id) {
                    m.nudge_count += 1;
                    m.last_nudge_ms = now_ms();
                }
            }
            Event::TaskCreated { task } | Event::TaskUpdated { task } => {
                self.tasks.insert(task.id.clone(), task.clone());
            }
        }
    }

    pub fn has_master(&self) -> bool {
        self.workers.values().any(|w| w.role == "master")
    }

    pub fn master_id(&self) -> Option<String> {
        self.workers
            .values()
            .find(|w| w.role == "master")
            .map(|w| w.id.clone())
    }

    /// Unread (not yet acked) inbox of a worker, oldest first.
    pub fn inbox_of(&self, worker_id: &str) -> Vec<&Message> {
        let mut v: Vec<&Message> = self
            .msgs
            .values()
            .filter(|m| m.to == worker_id && m.state != "read" && m.state != "superseded")
            .collect();
        v.sort_by_key(|m| m.created_ms);
        v
    }

    /// True when some other message is a reply to `msg`.
    pub fn answered(&self, msg_id: &str) -> bool {
        self.msgs
            .values()
            .any(|m| m.in_reply_to.as_deref() == Some(msg_id))
    }

    /// One live request per direction during the cooldown window.
    pub fn recent_live_request(
        &self,
        from: &str,
        to: &str,
        now_ms: i64,
    ) -> Option<(&String, &Message)> {
        self.msgs.iter().find(|(_, m)| {
            m.from == from
                && m.to == to
                && m.mtype == "request"
                && m.state != "read"
                && !self.answered(&m.id)
                && now_ms - m.created_ms < REQUEST_COOLDOWN_MS
        })
    }

    /// Earlier replies remain journaled, but only the newest one is active.
    pub fn superseded_replies(&self, request_id: &str) -> Vec<String> {
        let mut ids: Vec<String> = self
            .msgs
            .values()
            .filter(|m| {
                m.mtype == "reply"
                    && m.in_reply_to.as_deref() == Some(request_id)
                    && m.state != "superseded"
            })
            .map(|m| m.id.clone())
            .collect();
        ids.sort_by(|a, b| {
            let rank = |id: &str| {
                self.msgs
                    .get(id)
                    .map(|m| (m.created_ms, m.id.clone()))
                    .unwrap_or_default()
            };
            rank(a).cmp(&rank(b))
        });
        ids
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
        st.apply(&Event::Sent {
            msg: msg("m1", "w2", "request"),
        });
        assert_eq!(st.inbox_of("w2").len(), 1);
        assert!(st.inbox_of("w1").is_empty());

        st.apply(&Event::Delivered {
            ids: vec!["m1".into()],
        });
        assert_eq!(st.msgs["m1"].state, "delivered");
        assert_eq!(st.inbox_of("w2").len(), 1);

        st.apply(&Event::Acked {
            ids: vec!["m1".into()],
        });
        assert_eq!(st.msgs["m1"].state, "read");
        assert!(st.inbox_of("w2").is_empty());
    }

    #[test]
    fn answered_detection() {
        let mut st = State::default();
        st.apply(&Event::Sent {
            msg: msg("m1", "w2", "request"),
        });
        let mut reply = msg("m2", "w1", "reply");
        reply.in_reply_to = Some("m1".into());
        st.apply(&Event::Sent { msg: reply });
        assert!(st.answered("m1"));
        assert!(!st.answered("m2"));
    }

    #[test]
    fn request_cooldown_uses_only_recent_live_request() {
        let mut st = State::default();
        let mut request = msg("request", "w2", "request");
        request.from = "w1".into();
        request.created_ms = 500;
        st.apply(&Event::Sent { msg: request });

        let (id, _) = st
            .recent_live_request("w1", "w2", 500 + REQUEST_COOLDOWN_MS - 1)
            .expect("recent live request blocks a new send");
        assert_eq!(id, "request");
        assert!(st
            .recent_live_request("w1", "w2", 500 + REQUEST_COOLDOWN_MS)
            .is_none());
    }

    #[test]
    fn latest_reply_supersedes_previous_replies() {
        let mut st = State::default();
        st.apply(&Event::Sent {
            msg: msg("request", "w1", "request"),
        });

        let mut first = msg("reply-1", "w1", "reply");
        first.in_reply_to = Some("request".into());
        first.created_ms = 2;
        st.apply(&Event::Sent { msg: first });
        let stale_replies = st.superseded_replies("request");

        let mut latest = msg("reply-2", "w1", "reply");
        latest.in_reply_to = Some("request".into());
        latest.created_ms = 3;
        st.apply(&Event::Sent { msg: latest });
        st.apply(&Event::Superseded { ids: stale_replies });

        assert!(st.answered("request"));
        assert_eq!(st.msgs["reply-1"].state, "superseded");
        assert_eq!(st.msgs["reply-2"].state, "pending");
        assert_eq!(
            st.inbox_of("w1")
                .iter()
                .filter(|m| m.mtype == "reply")
                .map(|m| m.id.as_str())
                .collect::<Vec<_>>(),
            vec!["reply-2"]
        );
    }
}
