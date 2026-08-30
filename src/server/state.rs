use serde::{Deserialize, Serialize};
use std::collections::HashMap;

pub fn now_ms() -> i64 {
    chrono::Utc::now().timestamp_millis()
}

pub fn default_task_status() -> String {
    "working".into()
}

pub fn default_priority() -> String {
    "p2".into()
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WaitSpec {
    #[serde(default)]
    pub waiter: String,
    pub waiting_for: String,
    pub responsible_actor: String,
    pub reason: String,
    pub deadline_ms: i64,
    pub resume_on: Vec<String>,
    pub escalation: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MigrationRecord {
    pub id: String,
    pub from_version: String,
    pub to_version: String,
    pub phase: String,
    pub admission_frozen: bool,
    pub snapshot_hash: Option<String>,
    pub worker_count: usize,
    pub task_count: usize,
    pub message_count: usize,
    pub operator: String,
    pub issues: Vec<String>,
    pub created_ms: i64,
    pub updated_ms: i64,
}

/// Runtime is encoded in the registered pane handle. tmux is the only live
/// notification channel.
pub fn runtime_for_pane(pane: Option<&str>) -> Option<&'static str> {
    let pane = pane?;
    if pane.starts_with('%') {
        Some("tmux")
    } else {
        None
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WorkerRec {
    pub id: String,
    pub token: String,
    pub pane: Option<String>,
    pub cwd: String,
    pub registered_ms: i64,
}

pub const MAX_WAKE_ATTEMPTS: u32 = 3;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct NotificationSubscription {
    pub id: String,
    pub worker_id: String,
    pub event: String,
    pub subject: Option<String>,
    pub pane: String,
    pub method: String,
    pub trigger_ms: Option<i64>,
    pub expires_ms: i64,
    pub status: String,
    pub created_ms: i64,
    pub updated_ms: i64,
}

impl NotificationSubscription {
    pub fn matches(&self, worker_id: &str, event: &str, subject: Option<&str>, now: i64) -> bool {
        self.worker_id == worker_id
            && self.event == event
            && self.subject.as_deref() == subject
            && self.method == "tmux"
            && self.status == "armed"
            && self.expires_ms > now
    }
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
    #[serde(default, alias = "nudge_count")]
    pub wake_attempt_count: u32,
    #[serde(default, alias = "last_nudge_ms")]
    pub last_wake_attempt_ms: i64,
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
    #[serde(default = "default_priority")]
    pub priority: String,
    #[serde(default = "default_task_status")]
    pub status: String,
    #[serde(default)]
    pub next_step: Option<String>,
    #[serde(default)]
    pub wait: Option<WaitSpec>,
    pub created_ms: i64,
    pub updated_ms: i64,
}

pub fn task_resource_active(status: &str) -> bool {
    !matches!(status, "waiting" | "merged" | "closed" | "cancelled")
}

pub fn wait_cycle(tasks: &HashMap<String, TaskRec>, task_id: &str, waiting_for: &str) -> bool {
    let mut current = waiting_for;
    let mut seen = std::collections::HashSet::new();
    while seen.insert(current.to_string()) {
        if current == task_id {
            return true;
        }
        let Some(task) = tasks.get(current) else {
            return false;
        };
        let Some(wait) = task.wait.as_ref() else {
            return false;
        };
        current = &wait.waiting_for;
    }
    true
}

/// Journal events. Every mutation is an event: live path applies + appends,
/// replay applies only. This is what makes restart recovery deterministic.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "ev")]
pub enum Event {
    Registered {
        worker: WorkerRec,
    },
    #[serde(rename = "WorkerRemoved")]
    LegacyWorkerRemoved {
        worker_id: String,
    },
    #[serde(rename = "MasterTransferred")]
    LegacyMasterTransferred {
        from: String,
        to: String,
    },
    Sent {
        msg: Message,
    },
    DeliveryMode {
        msg_id: String,
        mode: String,
    },
    WakeAttempted {
        ids: Vec<String>,
        #[serde(default)]
        attempted_ms: i64,
    },
    NotificationSubscribed {
        subscription: NotificationSubscription,
    },
    NotificationStatus {
        subscription_id: String,
        status: String,
        updated_ms: i64,
    },
    NotificationConsumed {
        subscription_id: String,
        message_id: String,
        consumed_ms: i64,
    },
    WakeBound {
        message_id: String,
        subscription_id: String,
    },
    Delivered {
        ids: Vec<String>,
    },
    Acked {
        ids: Vec<String>,
    },
    #[serde(rename = "Nudged")]
    LegacyNudged {
        msg_id: String,
    },
    Superseded {
        ids: Vec<String>,
    },
    TaskCreated {
        task: TaskRec,
    },
    TaskUpdated {
        task: TaskRec,
    },
    MigrationUpdated {
        migration: MigrationRecord,
    },
}

#[derive(Default)]
pub struct State {
    pub workers: HashMap<String, WorkerRec>,
    pub msgs: HashMap<String, Message>,
    pub tasks: HashMap<String, TaskRec>,
    pub delivery_modes: HashMap<String, String>,
    pub notification_subscriptions: HashMap<String, NotificationSubscription>,
    pub wake_bindings: HashMap<String, String>,
    pub migration: Option<MigrationRecord>,
}

impl State {
    pub fn apply(&mut self, ev: &Event) {
        match ev {
            Event::Registered { worker } => {
                self.workers.insert(worker.id.clone(), worker.clone());
            }
            Event::LegacyWorkerRemoved { worker_id } => {
                self.workers.remove(worker_id);
            }
            Event::LegacyMasterTransferred { .. } => {}
            Event::Sent { msg } => {
                self.msgs.insert(msg.id.clone(), msg.clone());
            }
            Event::DeliveryMode { msg_id, mode } => {
                self.delivery_modes.insert(msg_id.clone(), mode.clone());
            }
            Event::WakeAttempted { ids, attempted_ms } => {
                for id in ids {
                    if let Some(message) = self.msgs.get_mut(id) {
                        message.wake_attempt_count = message.wake_attempt_count.saturating_add(1);
                        message.last_wake_attempt_ms = *attempted_ms;
                    }
                }
            }
            Event::NotificationSubscribed { subscription } => {
                self.notification_subscriptions
                    .insert(subscription.id.clone(), subscription.clone());
            }
            Event::NotificationStatus {
                subscription_id,
                status,
                updated_ms,
            } => {
                if let Some(subscription) = self.notification_subscriptions.get_mut(subscription_id)
                {
                    subscription.status = status.clone();
                    subscription.updated_ms = *updated_ms;
                }
            }
            Event::NotificationConsumed {
                subscription_id,
                message_id: _,
                consumed_ms,
            } => {
                if let Some(subscription) = self.notification_subscriptions.get_mut(subscription_id)
                {
                    subscription.status = "consumed".into();
                    subscription.updated_ms = *consumed_ms;
                }
            }
            Event::WakeBound {
                message_id,
                subscription_id,
            } => {
                self.wake_bindings
                    .insert(message_id.clone(), subscription_id.clone());
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
            Event::LegacyNudged { msg_id } => {
                if let Some(m) = self.msgs.get_mut(msg_id) {
                    m.wake_attempt_count = m.wake_attempt_count.saturating_add(1);
                    m.last_wake_attempt_ms = 0;
                }
            }
            Event::TaskCreated { task } | Event::TaskUpdated { task } => {
                self.tasks.insert(task.id.clone(), task.clone());
            }
            Event::MigrationUpdated { migration } => {
                self.migration = Some(migration.clone());
            }
        }
    }

    pub fn admission_frozen(&self) -> bool {
        self.migration
            .as_ref()
            .is_some_and(|migration| migration.admission_frozen)
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

    pub fn matching_subscription(
        &self,
        worker_id: &str,
        event: &str,
        subject: Option<&str>,
        now: i64,
    ) -> Option<&NotificationSubscription> {
        self.notification_subscriptions
            .values()
            .filter(|subscription| subscription.matches(worker_id, event, subject, now))
            .min_by_key(|subscription| (subscription.created_ms, subscription.id.as_str()))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn runtime_is_derived_from_pane_namespace() {
        assert_eq!(runtime_for_pane(Some("%7")), Some("tmux"));
        assert_eq!(runtime_for_pane(Some("herdr:w4:p1")), None);
        assert_eq!(runtime_for_pane(None), None);
        assert_eq!(runtime_for_pane(Some("w4:p1")), None);
    }

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
            wake_attempt_count: 0,
            last_wake_attempt_ms: 0,
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
    fn legacy_wake_attempt_replay_is_clock_independent() {
        let event: Event = serde_json::from_str(r#"{"ev":"WakeAttempted","ids":["m1"]}"#).unwrap();
        let mut first = State::default();
        let mut second = State::default();
        for state in [&mut first, &mut second] {
            state.apply(&Event::Sent {
                msg: msg("m1", "worker", "system"),
            });
            state.apply(&event);
        }
        assert_eq!(first.msgs["m1"].wake_attempt_count, 1);
        assert_eq!(first.msgs["m1"].last_wake_attempt_ms, 0);
        assert_eq!(
            serde_json::to_value(&first.msgs["m1"]).unwrap(),
            serde_json::to_value(&second.msgs["m1"]).unwrap()
        );
    }

    #[test]
    fn legacy_role_field_is_ignored_on_replay() {
        let mut st = State::default();
        let event: Event = serde_json::from_str(
            r#"{"ev":"Registered","worker":{"id":"legacy","token":"t","pane":"%1","cwd":"/tmp","registered_ms":1,"role":"master"}}"#,
        )
        .unwrap();
        st.apply(&event);
        let worker = serde_json::to_value(&st.workers["legacy"]).unwrap();
        assert!(worker.get("role").is_none());
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
    fn wait_cycle_rejects_direct_and_transitive_cycles() {
        let base = |id: &str| TaskRec {
            id: id.into(),
            owner: "worker".into(),
            created_by: "worker".into(),
            feature_id: None,
            worktree_path: None,
            branch: None,
            base_commit: None,
            priority: "p2".into(),
            status: "waiting".into(),
            next_step: None,
            wait: None,
            created_ms: 0,
            updated_ms: 0,
        };
        let mut tasks = HashMap::new();
        let mut a = base("a");
        a.wait = Some(WaitSpec {
            waiter: "a".into(),
            waiting_for: "b".into(),
            responsible_actor: "worker".into(),
            reason: "resource_conflict".into(),
            deadline_ms: 1,
            resume_on: vec!["resource_released".into()],
            escalation: "resource_owner_and_waiter_recheck".into(),
        });
        tasks.insert("a".into(), a);
        assert!(wait_cycle(&tasks, "b", "a"));
        assert!(!wait_cycle(&tasks, "c", "a"));
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

    #[test]
    fn notification_subscription_is_explicit_exact_and_one_shot() {
        let mut state = State::default();
        state.apply(&Event::NotificationSubscribed {
            subscription: NotificationSubscription {
                id: "sub-release".into(),
                worker_id: "waiter".into(),
                event: "resource-released".into(),
                subject: Some("holder-task".into()),
                pane: "%7".into(),
                method: "tmux".into(),
                trigger_ms: None,
                expires_ms: 10_000,
                status: "armed".into(),
                created_ms: 1,
                updated_ms: 1,
            },
        });

        assert!(state
            .matching_subscription("waiter", "resource-released", Some("holder-task"), 9_999)
            .is_some());
        assert!(state
            .matching_subscription("waiter", "resource-released", Some("other-task"), 9_999)
            .is_none());
        assert!(state
            .matching_subscription("waiter", "resource-released", Some("holder-task"), 10_000)
            .is_none());

        state.apply(&Event::NotificationConsumed {
            subscription_id: "sub-release".into(),
            message_id: "message".into(),
            consumed_ms: 8_000,
        });
        assert!(state
            .matching_subscription("waiter", "resource-released", Some("holder-task"), 8_001)
            .is_none());
    }

    #[test]
    fn wake_binding_is_control_state_not_message_payload() {
        let mut state = State::default();
        state.apply(&Event::Sent {
            msg: msg("message", "waiter", "notify"),
        });
        state.apply(&Event::WakeBound {
            message_id: "message".into(),
            subscription_id: "subscription".into(),
        });

        assert_eq!(state.wake_bindings["message"], "subscription");
        assert!(serde_json::to_value(&state.msgs["message"])
            .unwrap()
            .get("subscription_id")
            .is_none());
    }
}
