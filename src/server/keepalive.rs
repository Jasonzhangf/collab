use super::{
    presence::AgentState,
    state::{Event, Message, State},
    Server,
};
use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct Record {
    pub observed: String,
    pub idle_since_ms: i64,
    pub activity_ms: i64,
    pub last_notice_ms: i64,
    pub last_notice_id: Option<String>,
    pub unacked: u8,
    pub suspected_offline: bool,
    /// Last state actually reported to the master. Durable state advances every
    /// tick; the master is told only when a state settles into something new.
    #[serde(default)]
    pub notified_state: String,
    /// When the current not-yet-reported observation first appeared.
    #[serde(default)]
    pub pending_since_ms: i64,
    #[serde(default)]
    pub idle_episode_notices: u8,
    #[serde(default)]
    pub idle_episode_reason: String,
    /// A valid Working observation survives inconclusive probes and an
    /// unavailable master subscription until its idle transition is reported.
    #[serde(default)]
    pub working_seen: bool,
    #[serde(default)]
    pub idle_episode_stopped: bool,
    /// Scheduler-owned actionable task identities/statuses delimit episodes;
    /// ACKs, monitoring messages, and runtime flaps do not.
    #[serde(default)]
    pub idle_episode_tasks: Vec<(String, String)>,
}

/// A starting agent legitimately flaps between idle and working while it boots
/// and renders. Reporting each flap floods the master, so a state must hold
/// before it is worth one scheduling notification.
const SUBAGENT_STATE_SETTLE_MS: i64 = 60_000;

pub(crate) fn actionable(status: &str) -> bool {
    matches!(
        status,
        "assigned" | "working" | "verifying" | "reviewed" | "rework"
    )
}

fn observed_label(agent: AgentState) -> &'static str {
    match agent {
        AgentState::Waiting => "idle",
        AgentState::Working => "working",
        AgentState::Unknown => "unknown",
    }
}

pub fn view(state: &State, worker: &str) -> serde_json::Value {
    let mut history: Vec<_> = state.msgs.values().filter(|m| m.to == worker && m.mtype == "keepalive")
        .map(|m| serde_json::json!({"id":m.id,"subject":m.subject,"created_ms":m.created_ms,
            "state":m.state,"acked":m.state=="read","delivery_confirmed":matches!(m.state.as_str(),"delivered"|"read"),"wake_attempts":m.wake_attempt_count})).collect();
    history.sort_by_key(|m| m["created_ms"].as_i64());
    serde_json::json!({"state":state.keepalives.get(worker),"notification_history":history,
        "has_actionable_tasks":state.tasks.values().any(|t|t.owner==worker && actionable(&t.status))})
}

pub fn tick(server: &Server) {
    tick_at(server, super::state::now_ms());
}

/// App Server transport liveness is verified at registration and at each
/// notification attempt. The keepalive coordinator owns only the durable
/// worker-idle transition; it does not infer agent state from terminal text.
pub(crate) fn tick_at(server: &Server, now: i64) {
    if !server.config.keepalive.enabled || !server.config.timers.enabled {
        return;
    }
    let workers: Vec<_> = {
        let state = server.state.lock().unwrap();
        if state.admission_frozen() {
            return;
        }
        state.workers.values().cloned().collect()
    };
    for worker in workers {
        // App Server transport exposes thread liveness, not the agent's
        // execution state. Managed children report that state through their
        // typed lifecycle; ordinary peers are left unknown until they do.
        let agent = {
            let state = server.state.lock().unwrap();
            match state
                .subagents
                .values()
                .find(|child| child.peer == worker.id)
                .map(|child| child.status.as_str())
            {
                Some("working") => AgentState::Working,
                Some("idle") => AgentState::Waiting,
                _ => AgentState::Unknown,
            }
        };
        if agent == AgentState::Unknown {
            continue;
        }

        let mut state = server.state.lock().unwrap();
        if state.admission_frozen() {
            return;
        }
        if !state.workers.contains_key(&worker.id) {
            continue;
        }
        let mut tasks: Vec<_> = state
            .tasks
            .values()
            .filter(|t| t.owner == worker.id && actionable(&t.status))
            .map(|t| t.id.clone())
            .collect();
        tasks.sort();

        let old = state
            .keepalives
            .get(&worker.id)
            .cloned()
            .unwrap_or_default();
        let mut record = old.clone();

        let task_revision: Vec<_> = tasks
            .iter()
            .map(|id| (id.clone(), state.tasks[id].status.clone()))
            .collect();
        let task_revision_changed = record.idle_episode_tasks != task_revision;
        if task_revision_changed {
            record.idle_episode_notices = 0;
            record.idle_episode_reason.clear();
            record.idle_episode_stopped = false;
            record.idle_episode_tasks = task_revision;
            if !tasks.is_empty() {
                record.working_seen = false;
            }
        }
        let managed = state
            .subagents
            .values()
            .find(|child| child.peer == worker.id)
            .cloned();
        let master_id = match super::live_master_id(server, &state) {
            Ok(master_id) => master_id,
            Err(_) => continue,
        };
        let is_live_master = master_id.as_deref() == Some(worker.id.as_str());
        if agent == AgentState::Working && record.pending_since_ms == 0 {
            record.pending_since_ms = now;
        }
        if old.observed == "working"
            || agent == AgentState::Working
            || (old.observed.is_empty()
                && !tasks.is_empty()
                && managed
                    .as_ref()
                    .is_some_and(|child| child.status == "working"))
        {
            record.working_seen = true;
        }
        if is_live_master && agent == AgentState::Working && record.idle_episode_notices > 0 {
            record.idle_episode_stopped = true;
        }
        {
            let is_idle = agent == AgentState::Waiting;
            let idle_reason = if tasks.is_empty() {
                "no-actionable-tasks"
            } else {
                "actionable-tasks-idle"
            };
            let observed_str = observed_label(agent);
            let observed_changed = record.observed != observed_str;
            if observed_changed {
                record.observed = observed_str.into();
                record.idle_since_ms = now;
                if agent == AgentState::Working {
                    record.activity_ms = now;
                }
            }
            record.unacked = 0;
            record.last_notice_id = None;
            let mut events = Vec::new();
            if observed_changed {
                let signal = if is_idle {
                    if is_live_master {
                        super::state::MasterWakeSignal::MasterIdle {
                            worker_id: worker.id.clone(),
                        }
                    } else if let Some(child) = &managed {
                        super::state::MasterWakeSignal::SubagentStatus {
                            subagent_id: child.id.clone(),
                        }
                    } else {
                        super::state::MasterWakeSignal::WorkerIdle {
                            worker_id: worker.id.clone(),
                        }
                    }
                } else if let Some(child) = &managed {
                    super::state::MasterWakeSignal::SubagentWorking {
                        subagent_id: child.id.clone(),
                    }
                } else {
                    super::state::MasterWakeSignal::WorkerWorking {
                        worker_id: worker.id.clone(),
                    }
                };
                events.push(Event::MasterWakeSignal { signal, at_ms: now });
            }
            if let Some(mut child) = managed.clone() {
                if matches!(agent, AgentState::Working | AgentState::Waiting)
                    && child.status != observed_str
                {
                    child.status = observed_str.into();
                    events.push(Event::SubagentUpdated { subagent: child });
                }
            }
            if !is_idle && record.pending_since_ms == 0 {
                record.pending_since_ms = now;
            } else if record.pending_since_ms == 0 {
                record.pending_since_ms = now;
            }
            let new_idle_reason = is_idle && record.idle_episode_reason != idle_reason;
            if new_idle_reason {
                record.idle_episode_reason = idle_reason.into();
            }
            let idle_notification_due = if is_live_master {
                is_idle
                    && tasks.is_empty()
                    && !record.idle_episode_stopped
                    && record.idle_episode_notices < 3
                    && ((record.working_seen && record.idle_episode_notices == 0)
                        || (record.idle_episode_notices > 0
                            && now.saturating_sub(record.last_notice_ms)
                                >= super::mailbox::AUTOMATIC_BATCH_WINDOW_MS))
            } else {
                is_idle
                    && record.working_seen
                    && record.idle_episode_notices == 0
                    && (managed.is_none() || !tasks.is_empty() || task_revision_changed)
                    && (managed.is_none()
                        || now.saturating_sub(record.pending_since_ms) >= SUBAGENT_STATE_SETTLE_MS)
            };
            if idle_notification_due && server.config.notifications.enabled {
                if let Some(master_id) = master_id {
                    // An armed subscription can still name the master's old
                    // endpoint. Validate its current delivery target before the
                    // idle transition is consumed by Sent/WakeBound.
                    let subscription =
                        state.matching_subscription(&master_id, "direct-message", None, now);
                    if master_id == worker.id {
                        if let Some(sub) = subscription {
                            let alert_id = super::gen_msg_id();
                            events.push(Event::Sent {
                                msg: Message {
                                    id: alert_id.clone(),
                                    from: "collab-server".into(),
                                    to: worker.id.clone(),
                                    mtype: "notify".into(),
                                    subject: Some(format!("master-idle: {}", worker.id)),
                                    body: format!(
                                        "Master {} is now idle with no actionable task. Scheduling continues: inspect the task graph, worker/subagent load, liveness, saturation, and blockers; dispatch authorized work or resolve and reassign blockers. Cancel only iff no actionable task, dependency, resolvable blocker, or authorized open bug remains, using collab notify unsubscribe {} and record the receipt. If the goal is complete, report the evidence to the user.",
                                        worker.id, sub.id
                                    ),
                                    in_reply_to: None,
                                    created_ms: now,
                                    state: "pending".into(),
                                    wake_attempt_count: 0,
                                    last_wake_attempt_ms: 0,
                                },
                            });
                            events.push(Event::WakeBound {
                                message_id: alert_id,
                                subscription_id: sub.id.clone(),
                            });
                            record.last_notice_ms = now;
                            record.idle_episode_notices =
                                record.idle_episode_notices.saturating_add(1);
                        }
                    } else if let Some(sub) = subscription {
                        let alert_id = super::gen_msg_id();
                        events.push(Event::Sent {
                            msg: Message {
                                id: alert_id.clone(),
                                from: "collab-server".into(),
                                to: master_id.clone(),
                                mtype: if managed.is_some() { "subagent-status" } else { "notify" }.into(),
                                subject: Some(if managed.is_some() { "subagent-status".into() } else { format!("worker-idle: {}", worker.id) }),
                                body: if let Some(child) = &managed {
                                    format!("subagent={} state=idle tasks={}", child.id, tasks.join(","))
                                } else if tasks.is_empty() {
                                    format!("Worker {} is now idle with no active task. Action required: check task graph for unblocked downstream tasks, or pull from appsdk bug list --status open (P0/P1). If all work is complete, propose next steps to user and pause.", worker.id)
                                } else {
                                    format!("Worker {} is now idle. Actionable tasks: {}. Action required: inspect these tasks and the task graph for unblocked downstream work, or pull from appsdk bug list --status open (P0/P1). If all work is complete, propose next steps to user and pause.", worker.id, tasks.join(","))
                                },
                                in_reply_to: None,
                                created_ms: now,
                                state: "pending".into(),
                                wake_attempt_count: 0,
                                last_wake_attempt_ms: 0,
                            },
                        });
                        record.idle_episode_notices = record.idle_episode_notices.saturating_add(1);
                        record.last_notice_ms = now;
                        record.notified_state = "idle".into();
                        record.working_seen = false;
                        events.push(Event::WakeBound {
                            message_id: alert_id,
                            subscription_id: sub.id.clone(),
                        });
                    }
                }
            }
            if record != old {
                events.insert(
                    0,
                    Event::KeepaliveUpdated {
                        worker_id: worker.id.clone(),
                        record,
                    },
                );
            }
            if !events.is_empty() {
                server.commit_locked(&mut state, &events);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::super::peer_tests::{register, test_server};
    use super::*;
    use crate::subagent::Record as SubagentRecord;

    fn managed(id: &str, parent: &str, peer: &str, status: &str, now: i64) -> SubagentRecord {
        SubagentRecord {
            id: id.into(),
            parent: parent.into(),
            peer: peer.into(),
            status: status.into(),
            thread_id: Some(format!("thread-{peer}")),
            profile: None,
            created_ms: now,
            ready_deadline_ms: now + 90_000,
            last_message: None,
            error: None,
            probe_failures: Vec::new(),
            runtime: Some("codex".into()),
        }
    }

    fn task(id: &str, owner: &str, status: &str, now: i64) -> crate::server::state::TaskRec {
        crate::server::state::TaskRec {
            id: id.into(),
            owner: owner.into(),
            created_by: "master".into(),
            feature_id: None,
            worktree_path: None,
            branch: None,
            base_commit: None,
            priority: "p2".into(),
            status: status.into(),
            next_step: Some("work".into()),
            wait: None,
            created_ms: now,
            updated_ms: now,
        }
    }

    fn status_count(server: &Server) -> usize {
        server
            .state
            .lock()
            .unwrap()
            .msgs
            .values()
            .filter(|message| message.mtype == "subagent-status")
            .count()
    }

    #[test]
    fn appserver_worker_without_managed_state_stays_unknown() {
        let (server, root) = test_server();
        register(&server, "worker", "thread-worker");
        let base = super::super::state::now_ms();
        tick_at(&server, base);
        let state = server.state.lock().unwrap();
        assert!(!state.keepalives.contains_key("worker"));
        assert!(state.msgs.is_empty());
        drop(state);
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn managed_working_to_idle_reports_once_after_settle() {
        let (server, root) = test_server();
        register(&server, "master", "thread-master");
        register(&server, "child", "thread-child");
        assert!(
            super::super::handle_master_promote(
                &server,
                "master".into(),
                "token-master".into(),
                "user approved master".into(),
            )
            .ok
        );
        let now = super::super::state::now_ms();
        server.commit(&[
            Event::SubagentUpdated {
                subagent: managed("managed", "master", "child", "working", now),
            },
            Event::TaskCreated {
                task: task("task-child", "child", "working", now),
            },
        ]);
        tick_at(&server, now + 900_000);
        assert_eq!(status_count(&server), 0);
        {
            let mut state = server.state.lock().unwrap();
            let mut child = state.subagents["managed"].clone();
            child.status = "idle".into();
            state.subagents.insert(child.id.clone(), child.clone());
        }
        tick_at(&server, now + 1_800_000);
        assert_eq!(status_count(&server), 1);
        tick_at(&server, now + 2_700_000);
        assert_eq!(status_count(&server), 1);
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn managed_idle_without_actionable_task_stays_quiet() {
        let (server, root) = test_server();
        register(&server, "master", "thread-master");
        register(&server, "child", "thread-child");
        assert!(
            super::super::handle_master_promote(
                &server,
                "master".into(),
                "token-master".into(),
                "user approved master".into(),
            )
            .ok
        );
        let now = super::super::state::now_ms();
        server.commit(&[Event::SubagentUpdated {
            subagent: managed("managed", "master", "child", "idle", now),
        }]);
        tick_at(&server, now + 1_000);
        tick_at(&server, now + 200_000);
        tick_at(&server, now + 400_000);
        assert_eq!(status_count(&server), 0);
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn managed_working_stops_master_reminder_episode() {
        let (server, root) = test_server();
        register(&server, "master", "thread-master");
        register(&server, "child", "thread-child");
        assert!(
            super::super::handle_master_promote(
                &server,
                "master".into(),
                "token-master".into(),
                "user approved master".into(),
            )
            .ok
        );
        let now = super::super::state::now_ms();
        server.commit(&[
            Event::SubagentUpdated {
                subagent: managed("managed", "master", "child", "working", now),
            },
            Event::TaskCreated {
                task: task("task-child", "child", "working", now),
            },
        ]);
        tick_at(&server, now + 1_000);
        tick_at(&server, now + 2_000);
        {
            let mut state = server.state.lock().unwrap();
            let mut child = state.subagents["managed"].clone();
            child.status = "idle".into();
            state.subagents.insert(child.id.clone(), child.clone());
        }
        tick_at(&server, now + 3_000);
        let state = server.state.lock().unwrap();
        assert_eq!(
            state
                .msgs
                .values()
                .filter(|message| message.mtype == "subagent-status")
                .count(),
            0
        );
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn transport_identity_is_the_only_liveness_signal() {
        let (server, root) = test_server();
        register(&server, "worker", "thread-worker");
        let state = server.state.lock().unwrap();
        let worker = &state.workers["worker"];
        let transport = worker.transport.as_ref().unwrap();
        assert_eq!(transport.kind, crate::proto::TransportKind::AppServer);
        assert_eq!(transport.thread_id.as_deref(), Some("thread-worker"));
        drop(state);
        std::fs::remove_dir_all(root).unwrap();
    }
}
