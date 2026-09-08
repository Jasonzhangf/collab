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

pub const MAX_WAKE_ATTEMPTS: u32 = 1;
pub const MAX_NOTIFICATION_REPEATS: u32 = 100;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct NotificationSubscription {
    pub id: String,
    pub worker_id: String,
    pub event: String,
    pub subject: Option<String>,
    pub pane: String,
    pub method: String,
    pub trigger_ms: Option<i64>,
    #[serde(default)]
    pub trigger_times_ms: Vec<i64>,
    #[serde(default)]
    pub interval_ms: Option<i64>,
    #[serde(default = "default_repeat_count")]
    pub repeat_count: u32,
    #[serde(default)]
    pub fired_count: u32,
    pub expires_ms: i64,
    pub status: String,
    pub created_ms: i64,
    pub updated_ms: i64,
}

pub fn default_repeat_count() -> u32 { 1 }

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
    #[serde(default)]
    pub subject: Option<String>,
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

/// Durable, project-level scheduling signals.  The sets contain identifiers
/// only; task, bug, and mailbox truth remains in their respective stores and
/// is materialized when the master reads its briefing.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
pub struct MasterWakeAccumulator {
    pub generation: u64,
    pub goal_due: bool,
    #[serde(default)]
    pub idle_workers: Vec<String>,
    #[serde(default)]
    pub unresponsive_workers: Vec<String>,
    #[serde(default)]
    pub blocked_or_timed_out_tasks: Vec<String>,
    #[serde(default)]
    pub completed_or_freed_tasks: Vec<String>,
    #[serde(default)]
    pub highest_bug_revision: Option<u64>,
    #[serde(default)]
    pub active_goal_revision: Option<u64>,
    pub ready_authorized_work: bool,
    pub scheduling_decision_revision: u64,
    pub first_pending_ms: i64,
    pub last_updated_ms: i64,
    #[serde(default)]
    pub wake_hold: Option<WakeHold>,
    #[serde(default = "default_delivery_state")]
    pub delivery_state: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct WakeHold {
    pub reason: String,
    pub held_by: String,
    pub held_ms: i64,
    pub until_ms: i64,
}

fn default_delivery_state() -> String {
    "clean".into()
}

impl MasterWakeAccumulator {
    fn add_unique(items: &mut Vec<String>, value: String) -> bool {
        if items.contains(&value) {
            return false;
        }
        items.push(value);
        items.sort();
        true
    }

    fn note_signal(&mut self, signal: &MasterWakeSignal, created_ms: i64) {
        let changed = match signal {
            MasterWakeSignal::GoalDue { revision } => {
                let changed = !self.goal_due
                    || self
                        .active_goal_revision
                        .is_none_or(|current| *revision > current);
                self.goal_due = true;
                if changed {
                    self.active_goal_revision = Some(*revision);
                }
                changed
            }
            MasterWakeSignal::WorkerIdle { worker_id }
            | MasterWakeSignal::MasterIdle { worker_id } => {
                let added = Self::add_unique(&mut self.idle_workers, worker_id.clone());
                let recovered = self
                    .unresponsive_workers
                    .iter()
                    .position(|id| id == worker_id)
                    .map(|index| self.unresponsive_workers.remove(index))
                    .is_some();
                added || recovered
            }
            MasterWakeSignal::WorkerUnresponsive { worker_id } => {
                Self::add_unique(&mut self.unresponsive_workers, worker_id.clone())
            }
            MasterWakeSignal::WorkerRecovered { worker_id }
            | MasterWakeSignal::WorkerWorking { worker_id } => {
                let idle_removed = self.idle_workers.iter().position(|id| id == worker_id);
                if let Some(index) = idle_removed {
                    self.idle_workers.remove(index);
                }
                let unresponsive_removed =
                    self.unresponsive_workers.iter().position(|id| id == worker_id);
                if let Some(index) = unresponsive_removed {
                    self.unresponsive_workers.remove(index);
                }
                idle_removed.is_some() || unresponsive_removed.is_some()
            }
            MasterWakeSignal::TaskBlocked { task_id } => {
                Self::add_unique(&mut self.blocked_or_timed_out_tasks, task_id.clone())
            }
            MasterWakeSignal::TaskFreed { task_id } => {
                Self::add_unique(&mut self.completed_or_freed_tasks, task_id.clone())
            }
            MasterWakeSignal::SubagentStatus { subagent_id } => {
                Self::add_unique(&mut self.idle_workers, format!("subagent:{subagent_id}"))
            }
            MasterWakeSignal::SubagentWorking { subagent_id } => {
                let id = format!("subagent:{subagent_id}");
                self.idle_workers
                    .iter()
                    .position(|existing| existing == &id)
                    .map(|index| self.idle_workers.remove(index))
                    .is_some()
            }
        };
        if changed {
            if self.generation == 0 {
                self.generation = 1;
                self.first_pending_ms = created_ms;
            }
            self.last_updated_ms = created_ms;
            self.delivery_state = "pending".into();
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub enum MasterWakeSignal {
    GoalDue { revision: u64 },
    WorkerIdle { worker_id: String },
    MasterIdle { worker_id: String },
    WorkerUnresponsive { worker_id: String },
    WorkerRecovered { worker_id: String },
    WorkerWorking { worker_id: String },
    TaskBlocked { task_id: String },
    TaskFreed { task_id: String },
    SubagentStatus { subagent_id: String },
    SubagentWorking { subagent_id: String },
}

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

/// Lifecycle evidence is separate from TaskRec so older producers and journal
/// events remain replayable as the task contract gains new milestones.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct TaskLifecycleRecord {
    #[serde(default)]
    pub delivery_evidence: Option<String>,
    #[serde(default)]
    pub delivered_ms: Option<i64>,
    #[serde(default)]
    pub review_evidence: Option<String>,
    #[serde(default)]
    pub reviewer: Option<String>,
    #[serde(default)]
    pub reviewed_ms: Option<i64>,
    #[serde(default)]
    pub integration_commit: Option<String>,
    #[serde(default)]
    pub integration_evidence: Option<String>,
    #[serde(default)]
    pub integrated_ms: Option<i64>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CleanupReceipt {
    pub id: String,
    pub task_id: String,
    pub worktree_path: Option<String>,
    pub branch: Option<String>,
    pub verified_ms: i64,
    #[serde(default)]
    pub manual_reason: Option<String>,
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
    MasterWakeSignal {
        signal: MasterWakeSignal,
        at_ms: i64,
    },
    MasterWakeUpdated {
        accumulator: MasterWakeAccumulator,
    },
    KeepaliveUpdated {
        worker_id: String,
        record: super::keepalive::Record,
    },
    SubagentUpdated {
        subagent: crate::subagent::Record,
    },
    Registered {
        worker: WorkerRec,
    },
    #[serde(rename = "WorkerRemoved")]
    LegacyWorkerRemoved {
        worker_id: String,
    },
    /// Master-authorized retirement of a worker registration. Unlike the legacy
    /// remove-worker path this records who closed it and why.
    WorkerClosed {
        worker_id: String,
        closed_by: String,
        reason: String,
        killed_session: bool,
        at_ms: i64,
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
    TaskLifecycleUpdated {
        task_id: String,
        record: TaskLifecycleRecord,
    },
    CleanupVerified {
        receipt: CleanupReceipt,
    },
    MigrationUpdated {
        migration: MigrationRecord,
    },
    #[serde(alias = "RootAssigned")]
    MasterAssigned {
        worker_id: String,
        assigned_by: String,
        approval: Option<String>,
        assigned_ms: i64,
    },
}

#[derive(Default)]
pub struct State {
    pub master_wake: MasterWakeAccumulator,
    pub keepalives: HashMap<String, super::keepalive::Record>,
    pub subagents: HashMap<String, crate::subagent::Record>,
    pub workers: HashMap<String, WorkerRec>,
    pub msgs: HashMap<String, Message>,
    pub tasks: HashMap<String, TaskRec>,
    pub task_lifecycle: HashMap<String, TaskLifecycleRecord>,
    pub cleanup_receipts: HashMap<String, CleanupReceipt>,
    pub delivery_modes: HashMap<String, String>,
    pub notification_subscriptions: HashMap<String, NotificationSubscription>,
    pub wake_bindings: HashMap<String, String>,
    pub migration: Option<MigrationRecord>,
    pub master_worker_id: Option<String>,
    pub master_assigned_by: Option<String>,
    pub master_approval: Option<String>,
    pub master_assigned_ms: Option<i64>,
}

impl State {
    fn consume_notification(&mut self, subscription_id: &str, consumed_ms: Option<i64>) {
        let Some(subscription) = self.notification_subscriptions.get_mut(subscription_id) else {
            return;
        };
        subscription.fired_count = subscription.fired_count.saturating_add(1);
        let total = if subscription.interval_ms.is_some() {
            subscription.repeat_count
        } else {
            subscription.trigger_times_ms.len().max(1) as u32
        };
        if subscription.fired_count >= total {
            subscription.status = "consumed".into();
        } else if let Some(interval) = subscription.interval_ms {
            let next_trigger = subscription
                .trigger_ms
                .map(|trigger| trigger.saturating_add(interval))
                .unwrap_or_else(|| {
                    subscription.created_ms.saturating_add(
                        interval.saturating_mul(subscription.fired_count.saturating_add(1) as i64),
                    )
                });
            subscription.trigger_ms = Some(next_trigger);
            subscription.status = "armed".into();
        } else {
            subscription.status = "armed".into();
        }
        if let Some(consumed_ms) = consumed_ms {
            subscription.updated_ms = consumed_ms;
        }
    }

    pub fn apply(&mut self, ev: &Event) {
        match ev {
            Event::MasterWakeSignal { signal, at_ms } => {
                self.master_wake.note_signal(signal, *at_ms);
            }
            Event::KeepaliveUpdated { worker_id, record } => {
                self.keepalives.insert(worker_id.clone(), record.clone());
            }
            Event::MasterWakeUpdated { accumulator } => {
                self.master_wake = accumulator.clone();
            }
            Event::SubagentUpdated { subagent } => {
                self.subagents.insert(subagent.id.clone(), subagent.clone());
            }
            Event::Registered { worker } => {
                self.workers.insert(worker.id.clone(), worker.clone());
            }
            Event::LegacyWorkerRemoved { worker_id } => {
                self.workers.remove(worker_id);
            }
            Event::WorkerClosed { worker_id, .. } => {
                self.workers.remove(worker_id);
                self.keepalives.remove(worker_id);
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
                self.consume_notification(subscription_id, Some(*consumed_ms));
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
                if ids.iter().any(|id| {
                    self.wake_bindings.contains_key(id)
                        && self
                            .msgs
                            .get(id)
                            .is_some_and(|message| {
                                message.from == "collab-server"
                                    && self.master_worker_id.as_deref()
                                        == Some(message.to.as_str())
                            })
                }) {
                    self.master_wake.delivery_state = "notified_unconsumed".into();
                }
            }
            Event::Acked { ids } => {
                for id in ids {
                    if let Some(m) = self.msgs.get_mut(id) {
                        m.state = "read".into();
                    }
                    let Some(subscription_id) = self.wake_bindings.get(id).cloned() else {
                        continue;
                    };
                    let Some(subscription) = self.notification_subscriptions.get(&subscription_id)
                    else {
                        continue;
                    };
                    let consumes_on_read = subscription.event != "direct-message"
                        || subscription.trigger_ms.is_some()
                        || !subscription.trigger_times_ms.is_empty()
                        || subscription.interval_ms.is_some();
                    if !consumes_on_read {
                        continue;
                    }
                    let fired_count = subscription.fired_count;
                    let read_count = self
                        .wake_bindings
                        .iter()
                        .filter(|(_, bound)| *bound == &subscription_id)
                        .filter(|(message_id, _)| {
                            self.msgs
                                .get(*message_id)
                                .is_some_and(|message| message.state == "read")
                        })
                        .count() as u32;
                    // recv records Delivered + Acked without a separate
                    // NotificationConsumed event. Compare durable read
                    // occurrences with the cursor so timer delivery, which
                    // already records NotificationConsumed, remains idempotent.
                    if read_count > fired_count {
                        self.consume_notification(&subscription_id, None);
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
            Event::TaskLifecycleUpdated { task_id, record } => {
                self.task_lifecycle.insert(task_id.clone(), record.clone());
            }
            Event::CleanupVerified { receipt } => {
                self.cleanup_receipts
                    .insert(receipt.task_id.clone(), receipt.clone());
            }
            Event::MigrationUpdated { migration } => {
                self.migration = Some(migration.clone());
            }
            Event::MasterAssigned {
                worker_id,
                assigned_by,
                approval,
                assigned_ms,
            } => {
                self.master_worker_id = Some(worker_id.clone());
                self.master_assigned_by = Some(assigned_by.clone());
                self.master_approval = approval.clone();
                self.master_assigned_ms = Some(*assigned_ms);
            }
        }
    }

    pub fn drop_message(&mut self, id: &str) {
        self.msgs.remove(id);
        self.delivery_modes.remove(id);
        self.wake_bindings.remove(id);
    }

    pub fn snapshot_events(&self) -> Vec<Event> {
        let mut events = Vec::new();
        if self.master_wake.generation > 0 {
            events.push(Event::MasterWakeUpdated {
                accumulator: self.master_wake.clone(),
            });
        }
        let mut workers: Vec<_> = self.workers.values().cloned().collect();
        workers.sort_by(|a, b| a.id.cmp(&b.id));
        events.extend(workers.into_iter().map(|worker| Event::Registered { worker }));
        let mut keepalives: Vec<_> = self.keepalives.iter().collect();
        keepalives.sort_by(|a, b| a.0.cmp(b.0));
        events.extend(keepalives.into_iter().map(|(worker_id, record)| {
            Event::KeepaliveUpdated {
                worker_id: worker_id.clone(),
                record: record.clone(),
            }
        }));
        let mut subagents: Vec<_> = self.subagents.values().cloned().collect();
        subagents.sort_by(|a, b| a.id.cmp(&b.id));
        events.extend(
            subagents
                .into_iter()
                .map(|subagent| Event::SubagentUpdated { subagent }),
        );
        let mut tasks: Vec<_> = self.tasks.values().cloned().collect();
        tasks.sort_by(|a, b| a.id.cmp(&b.id));
        events.extend(tasks.into_iter().map(|task| Event::TaskCreated { task }));
        let mut lifecycle: Vec<_> = self.task_lifecycle.iter().collect();
        lifecycle.sort_by(|a, b| a.0.cmp(b.0));
        events.extend(
            lifecycle
                .into_iter()
                .map(|(task_id, record)| Event::TaskLifecycleUpdated {
                    task_id: task_id.clone(),
                    record: record.clone(),
                }),
        );
        let mut receipts: Vec<_> = self.cleanup_receipts.values().cloned().collect();
        receipts.sort_by(|a, b| a.task_id.cmp(&b.task_id));
        events.extend(
            receipts
                .into_iter()
                .map(|receipt| Event::CleanupVerified { receipt }),
        );
        let mut subscriptions: Vec<_> = self.notification_subscriptions.values().cloned().collect();
        subscriptions.sort_by(|a, b| a.id.cmp(&b.id));
        events.extend(
            subscriptions
                .into_iter()
                .map(|subscription| Event::NotificationSubscribed { subscription }),
        );
        let mut messages: Vec<_> = self.msgs.values().cloned().collect();
        messages.sort_by(|a, b| (a.created_ms, a.id.clone()).cmp(&(b.created_ms, b.id.clone())));
        for msg in messages {
            let id = msg.id.clone();
            events.push(Event::Sent { msg });
            if let Some(mode) = self.delivery_modes.get(&id) {
                events.push(Event::DeliveryMode {
                    msg_id: id.clone(),
                    mode: mode.clone(),
                });
            }
            if let Some(subscription_id) = self.wake_bindings.get(&id) {
                events.push(Event::WakeBound {
                    message_id: id,
                    subscription_id: subscription_id.clone(),
                });
            }
        }
        if let Some(migration) = self.migration.clone() {
            events.push(Event::MigrationUpdated { migration });
        }
        if let Some(worker_id) = self.master_worker_id.clone() {
            events.push(Event::MasterAssigned {
                worker_id,
                assigned_by: self.master_assigned_by.clone().unwrap_or_default(),
                approval: self.master_approval.clone(),
                assigned_ms: self.master_assigned_ms.unwrap_or(0),
            });
        }
        events
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

    #[test]
    fn master_wake_accumulator_coalesces_generated_signals_until_decision() {
        let mut state = State::default();
        state.apply(&Event::MasterAssigned {
            worker_id: "master".into(),
            assigned_by: "operator".into(),
            approval: Some("approved".into()),
            assigned_ms: 1,
        });
        let generated = |id: &str, subject: &str, created_ms: i64| Message {
            id: id.into(),
            from: "collab-server".into(),
            to: "master".into(),
            mtype: "notify".into(),
            subject: Some(subject.into()),
            body: "durable detail".into(),
            in_reply_to: None,
            created_ms,
            state: "pending".into(),
            wake_attempt_count: 0,
            last_wake_attempt_ms: 0,
        };
        state.apply(&Event::MasterWakeSignal {
            signal: MasterWakeSignal::WorkerIdle {
                worker_id: "worker".into(),
            },
            at_ms: 10,
        });
        state.apply(&Event::Sent {
            msg: generated("idle-1", "worker-idle: worker", 10),
        });
        state.apply(&Event::MasterWakeSignal {
            signal: MasterWakeSignal::WorkerIdle {
                worker_id: "worker".into(),
            },
            at_ms: 20,
        });
        state.apply(&Event::Sent {
            msg: generated("idle-duplicate", "worker-idle: worker", 20),
        });
        state.apply(&Event::MasterWakeSignal {
            signal: MasterWakeSignal::WorkerUnresponsive {
                worker_id: "offline".into(),
            },
            at_ms: 30,
        });
        state.apply(&Event::Sent {
            msg: generated("unresponsive", "worker-unresponsive: offline", 30),
        });
        assert_eq!(state.master_wake.generation, 1);
        assert_eq!(state.master_wake.first_pending_ms, 10);
        assert_eq!(state.master_wake.last_updated_ms, 30);
        assert_eq!(state.master_wake.idle_workers, vec!["worker"]);
        assert_eq!(state.master_wake.unresponsive_workers, vec!["offline"]);
        assert_eq!(state.master_wake.delivery_state, "pending");

        let explicit = Message {
            from: "peer".into(),
            ..generated("explicit", "worker-idle: ignored", 40)
        };
        state.apply(&Event::Sent { msg: explicit });
        assert_eq!(state.master_wake.idle_workers, vec!["worker"]);
        state.apply(&Event::WakeBound {
            message_id: "idle-1".into(),
            subscription_id: "sub-master".into(),
        });
        state.apply(&Event::Delivered {
            ids: vec!["idle-1".into()],
        });
        assert_eq!(state.master_wake.delivery_state, "notified_unconsumed");
        state.apply(&Event::Acked {
            ids: vec!["idle-1".into()],
        });
        assert_eq!(state.master_wake.generation, 1);
        assert_eq!(state.master_wake.delivery_state, "notified_unconsumed");
        state.apply(&Event::MasterWakeSignal {
            signal: MasterWakeSignal::WorkerWorking {
                worker_id: "worker".into(),
            },
            at_ms: 50,
        });
        assert!(state.master_wake.idle_workers.is_empty());
        assert_eq!(state.master_wake.delivery_state, "pending");
    }

    fn msg(id: &str, to: &str, mtype: &str) -> Message {
        Message {
            id: id.into(),
            from: "a".into(),
            to: to.into(),
            mtype: mtype.into(),
            subject: Some("test".into()),
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
                trigger_times_ms: Vec::new(),
                interval_ms: None,
                repeat_count: 1,
                fired_count: 0,
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

    #[test]
    fn periodic_subscription_consumes_exactly_its_repeat_count() {
        let mut state = State::default();
        state.apply(&Event::NotificationSubscribed {
            subscription: NotificationSubscription {
                id: "sub-periodic".into(), worker_id: "waiter".into(), event: "deadline".into(),
                subject: Some("timer".into()), pane: "%7".into(), method: "tmux".into(),
                trigger_ms: None, trigger_times_ms: Vec::new(), interval_ms: Some(1_000),
                repeat_count: 3, fired_count: 0, expires_ms: 10_000,
                status: "armed".into(), created_ms: 1, updated_ms: 1,
            },
        });
        for count in 1..=3 {
            state.apply(&Event::NotificationConsumed { subscription_id: "sub-periodic".into(), message_id: format!("m{count}"), consumed_ms: count * 1_000 });
            if count < 3 { assert_eq!(state.notification_subscriptions["sub-periodic"].status, "armed"); }
        }
        assert_eq!(state.notification_subscriptions["sub-periodic"].status, "consumed");
        assert_eq!(state.notification_subscriptions["sub-periodic"].fired_count, 3);
    }

    #[test]
    fn periodic_subscription_persists_fixed_absolute_cursor_across_replay() {
        let subscription = |id: &str, trigger_ms: Option<i64>| NotificationSubscription {
            id: id.into(),
            worker_id: "master".into(),
            event: "deadline".into(),
            subject: Some("periodic".into()),
            pane: "%7".into(),
            method: "tmux".into(),
            trigger_ms,
            trigger_times_ms: Vec::new(),
            interval_ms: Some(1_000),
            repeat_count: 3,
            fired_count: 0,
            expires_ms: 20_000,
            status: "armed".into(),
            created_ms: 1_000,
            updated_ms: 1_000,
        };

        let mut none_state = State::default();
        none_state.apply(&Event::NotificationSubscribed {
            subscription: subscription("sub-periodic-none", None),
        });
        let mut none_cursor = Vec::new();
        for count in 1..=3 {
            none_state.apply(&Event::NotificationConsumed {
                subscription_id: "sub-periodic-none".into(),
                message_id: format!("none-{count}"),
                consumed_ms: 1_000 + (count as i64 * 1_000),
            });
            if count < 3 {
                none_cursor
                    .push(none_state.notification_subscriptions["sub-periodic-none"].trigger_ms);
            }
        }
        assert_eq!(none_cursor, vec![Some(3_000), Some(4_000)]);

        let mut seeded_state = State::default();
        seeded_state.apply(&Event::NotificationSubscribed {
            subscription: subscription("sub-periodic-seeded", Some(2_000)),
        });
        for count in 1..=2 {
            seeded_state.apply(&Event::NotificationConsumed {
                subscription_id: "sub-periodic-seeded".into(),
                message_id: format!("seeded-{count}"),
                consumed_ms: 1_000 + (count as i64 * 1_000),
            });
            assert_eq!(
                seeded_state.notification_subscriptions["sub-periodic-seeded"].trigger_ms,
                Some(2_000 + count as i64 * 1_000)
            );
        }

        let mut replayed = State::default();
        for event in seeded_state.snapshot_events() {
            replayed.apply(&event);
        }
        assert_eq!(
            replayed.notification_subscriptions["sub-periodic-seeded"].trigger_ms,
            seeded_state.notification_subscriptions["sub-periodic-seeded"].trigger_ms
        );
        assert_eq!(
            replayed.notification_subscriptions["sub-periodic-seeded"].fired_count,
            seeded_state.notification_subscriptions["sub-periodic-seeded"].fired_count
        );
    }

    #[test]
    fn ack_consumes_scheduled_occurrence_once_and_replay_preserves_cursor() {
        let subscription_id = "sub-ack-periodic";
        let message_id = "message-ack-periodic";
        let mut state = State::default();
        state.apply(&Event::NotificationSubscribed {
            subscription: NotificationSubscription {
                id: subscription_id.into(),
                worker_id: "master".into(),
                event: "deadline".into(),
                subject: Some("ack-periodic".into()),
                pane: "%7".into(),
                method: "tmux".into(),
                trigger_ms: None,
                trigger_times_ms: Vec::new(),
                interval_ms: Some(1_000),
                repeat_count: 3,
                fired_count: 0,
                expires_ms: 20_000,
                status: "armed".into(),
                created_ms: 1_000,
                updated_ms: 1_000,
            },
        });
        state.apply(&Event::Sent {
            msg: Message {
                id: message_id.into(),
                from: "collab-server".into(),
                to: "master".into(),
                mtype: "notification".into(),
                subject: Some("deadline:ack-periodic".into()),
                body: "scheduled occurrence".into(),
                in_reply_to: None,
                created_ms: 2_000,
                state: "pending".into(),
                wake_attempt_count: 0,
                last_wake_attempt_ms: 0,
            },
        });
        state.apply(&Event::WakeBound {
            message_id: message_id.into(),
            subscription_id: subscription_id.into(),
        });
        state.apply(&Event::Delivered {
            ids: vec![message_id.into()],
        });
        state.apply(&Event::Acked {
            ids: vec![message_id.into()],
        });

        let subscription = &state.notification_subscriptions[subscription_id];
        assert_eq!(subscription.fired_count, 1);
        assert_eq!(subscription.trigger_ms, Some(3_000));
        assert_eq!(subscription.status, "armed");

        // A duplicate ACK sees the same read occurrence and cannot advance the
        // durable cursor a second time.
        state.apply(&Event::Acked {
            ids: vec![message_id.into()],
        });
        let subscription = &state.notification_subscriptions[subscription_id];
        assert_eq!(subscription.fired_count, 1);
        assert_eq!(subscription.trigger_ms, Some(3_000));

        let mut replayed = State::default();
        for event in state.snapshot_events() {
            replayed.apply(&event);
        }
        let replayed_subscription = &replayed.notification_subscriptions[subscription_id];
        assert_eq!(replayed_subscription.fired_count, 1);
        assert_eq!(replayed_subscription.trigger_ms, Some(3_000));
        assert_eq!(replayed.msgs[message_id].state, "read");
    }

    #[test]
    fn ack_after_timer_consumption_does_not_double_consume() {
        let subscription_id = "sub-ack-timer";
        let message_id = "message-ack-timer";
        let mut state = State::default();
        state.apply(&Event::NotificationSubscribed {
            subscription: NotificationSubscription {
                id: subscription_id.into(),
                worker_id: "master".into(),
                event: "deadline".into(),
                subject: Some("ack-timer".into()),
                pane: "%7".into(),
                method: "tmux".into(),
                trigger_ms: Some(2_000),
                trigger_times_ms: Vec::new(),
                interval_ms: Some(1_000),
                repeat_count: 3,
                fired_count: 0,
                expires_ms: 20_000,
                status: "armed".into(),
                created_ms: 1_000,
                updated_ms: 1_000,
            },
        });
        state.apply(&Event::Sent {
            msg: Message {
                id: message_id.into(),
                from: "collab-server".into(),
                to: "master".into(),
                mtype: "notification".into(),
                subject: Some("deadline:ack-timer".into()),
                body: "timer occurrence".into(),
                in_reply_to: None,
                created_ms: 2_000,
                state: "pending".into(),
                wake_attempt_count: 0,
                last_wake_attempt_ms: 0,
            },
        });
        state.apply(&Event::WakeBound {
            message_id: message_id.into(),
            subscription_id: subscription_id.into(),
        });
        state.apply(&Event::Delivered {
            ids: vec![message_id.into()],
        });
        state.apply(&Event::NotificationConsumed {
            subscription_id: subscription_id.into(),
            message_id: message_id.into(),
            consumed_ms: 2_001,
        });
        assert_eq!(
            state.notification_subscriptions[subscription_id].fired_count,
            1
        );
        assert_eq!(
            state.notification_subscriptions[subscription_id].trigger_ms,
            Some(3_000)
        );

        state.apply(&Event::Acked {
            ids: vec![message_id.into()],
        });
        assert_eq!(
            state.notification_subscriptions[subscription_id].fired_count,
            1
        );
        assert_eq!(
            state.notification_subscriptions[subscription_id].trigger_ms,
            Some(3_000)
        );
    }

    #[test]
    fn ack_on_reusable_direct_message_does_not_consume_subscription() {
        let subscription_id = "sub-ack-direct";
        let message_id = "message-ack-direct";
        let mut state = State::default();
        state.apply(&Event::NotificationSubscribed {
            subscription: NotificationSubscription {
                id: subscription_id.into(),
                worker_id: "worker".into(),
                event: "direct-message".into(),
                subject: None,
                pane: "%7".into(),
                method: "tmux".into(),
                trigger_ms: None,
                trigger_times_ms: Vec::new(),
                interval_ms: None,
                repeat_count: 1,
                fired_count: 0,
                expires_ms: 20_000,
                status: "armed".into(),
                created_ms: 1_000,
                updated_ms: 1_000,
            },
        });
        state.apply(&Event::Sent {
            msg: Message {
                id: message_id.into(),
                from: "peer".into(),
                to: "worker".into(),
                mtype: "notify".into(),
                subject: Some("direct".into()),
                body: "reusable message".into(),
                in_reply_to: None,
                created_ms: 2_000,
                state: "pending".into(),
                wake_attempt_count: 0,
                last_wake_attempt_ms: 0,
            },
        });
        state.apply(&Event::WakeBound {
            message_id: message_id.into(),
            subscription_id: subscription_id.into(),
        });
        state.apply(&Event::Delivered {
            ids: vec![message_id.into()],
        });
        state.apply(&Event::Acked {
            ids: vec![message_id.into()],
        });

        let subscription = &state.notification_subscriptions[subscription_id];
        assert_eq!(subscription.fired_count, 0);
        assert_eq!(subscription.status, "armed");
        assert_eq!(state.msgs[message_id].state, "read");
    }
}
