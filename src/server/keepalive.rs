use super::{
    knock::AgentState,
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
        AgentState::Absent => "absent",
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

/// Run the keepalive coordinator against the scheduler's tick timestamp so
/// timer and liveness producers share one state snapshot boundary.
pub(crate) fn tick_at(server: &Server, now: i64) {
    tick_with(
        server,
        now,
        &super::knock::probe_agent_state,
        &|pane, text| super::knock_or_log(&server.log_path(), pane, text),
        &|worker_id, pane| (server.pane_owner_check)(worker_id, pane),
    );
}

pub(crate) fn tick_with(
    server: &Server,
    now: i64,
    probe: &dyn Fn(&str) -> AgentState,
    _wake: &dyn Fn(&str, &str) -> bool,
    owns_pane: &dyn Fn(&str, &str) -> Result<bool, ()>,
) {
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
        let Some(pane) = worker.pane.clone() else {
            continue;
        };
        let presence = (server.pane_alive_check)(&pane);
        if presence == super::knock::PanePresence::Unknown {
            continue;
        }
        let is_alive = presence == super::knock::PanePresence::Present;
        let is_owned = if is_alive {
            match owns_pane(&worker.id, &pane) {
                Ok(owned) => owned,
                Err(()) => continue,
            }
        } else {
            false
        };
        let agent = if is_alive && is_owned {
            probe(&pane)
        } else {
            AgentState::Absent
        };
        // Failed observation must not erase a working -> idle edge or an
        // existing idle episode.
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

        if !is_alive || !is_owned || agent == AgentState::Absent {
            let mut record = state
                .keepalives
                .get(&worker.id)
                .cloned()
                .unwrap_or_default();
            if !record.suspected_offline {
                record.suspected_offline = true;
                record.observed = "absent".into();
                let mut events = vec![Event::KeepaliveUpdated {
                    worker_id: worker.id.clone(),
                    record,
                }];
                if let Ok(Some(master_id)) = super::live_master_id(server, &state) {
                    if master_id != worker.id {
                        let alert_id = super::gen_msg_id();
                        events.push(Event::MasterWakeSignal {
                            signal: super::state::MasterWakeSignal::WorkerUnresponsive {
                                worker_id: worker.id.clone(),
                            },
                            at_ms: now,
                        });
                        events.push(Event::Sent {
                            msg: Message {
                                id: alert_id.clone(),
                                from: "collab-server".into(),
                                to: master_id.clone(),
                                mtype: "notify".into(),
                                subject: Some(format!("worker-unresponsive: {}", worker.id)),
                                body: format!(
                                    "Worker {} pane is absent, unowned or dead. Diagnostic closure required: run collab subagent snapshot {} --lines 40 to inspect ground truth.",
                                    worker.id, worker.id
                                ),
                                in_reply_to: None,
                                created_ms: now,
                                state: "pending".into(),
                                wake_attempt_count: 0,
                                last_wake_attempt_ms: 0,
                            },
                        });
                        if let Some(sub) =
                            state.matching_subscription(&master_id, "direct-message", None, now)
                        {
                            events.push(Event::WakeBound {
                                message_id: alert_id,
                                subscription_id: sub.id.clone(),
                            });
                        }
                    }
                }
                server.commit_locked(&mut state, &events);
            }
            continue;
        }
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
            if !is_idle {
                record.pending_since_ms = 0;
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
                    // pane. Validate its current delivery target before the
                    // idle transition is consumed by Sent/WakeBound.
                    let subscription = state
                        .matching_subscription(&master_id, "direct-message", None, now)
                        .filter(|sub| {
                            state
                                .workers
                                .get(&master_id)
                                .and_then(|master| master.pane.as_deref())
                                == Some(sub.pane.as_str())
                                && matches!(
                                    (server.pane_alive_check)(&sub.pane),
                                    super::knock::PanePresence::Present
                                )
                                && matches!(
                                    (server.pane_owner_check)(&master_id, &sub.pane),
                                    Ok(true)
                                )
                        });
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
    use super::*;
    #[test]
    fn actionable_worker_never_self_wakes_across_replay() {
        use super::super::{
            peer_tests::{register, test_server},
            state::TaskRec,
        };
        let (server, root) = test_server();
        // Even immediate notification mode cannot turn task observation into
        // an unsolicited worker wake.
        let mut server = server;
        server.config.notifications.mode = "immediate".into();
        register(&server, "worker", "%fake");
        let base = super::super::state::now_ms();
        for id in ["one", "two"] {
            server.commit(&[Event::TaskCreated {
                task: TaskRec {
                    id: id.into(),
                    owner: "worker".into(),
                    created_by: "worker".into(),
                    feature_id: None,
                    worktree_path: None,
                    branch: None,
                    base_commit: None,
                    priority: "p2".into(),
                    status: "working".into(),
                    next_step: None,
                    wait: None,
                    created_ms: base,
                    updated_ms: base,
                },
            }]);
        }
        let sends = std::cell::Cell::new(0);
        let send = |_: &str, text: &str| {
            assert!(text.contains("message_ids="));
            sends.set(sends.get() + 1);
            false
        };
        tick_with(&server, base, &|_| AgentState::Waiting, &send, &|_, _| {
            Ok(true)
        });
        for n in 1..=3 {
            tick_with(
                &server,
                base + n * 900_000,
                &|_| AgentState::Waiting,
                &send,
                &|_, _| Ok(true),
            );
            tick_with(
                &server,
                base + n * 900_000,
                &|_| AgentState::Waiting,
                &send,
                &|_, _| Ok(true),
            );
        }
        assert_eq!(sends.get(), 0, "actionable worker must never self-wake");
        let mut replay = State::default();
        for line in std::fs::read_to_string(root.join(".agent-collab/server/journal.jsonl"))
            .unwrap()
            .lines()
        {
            replay.apply(&serde_json::from_str::<Event>(line).unwrap());
        }
        *server.state.lock().unwrap() = replay;
        tick_with(
            &server,
            base + 3_600_000,
            &|_| AgentState::Waiting,
            &send,
            &|_, _| Ok(true),
        );
        tick_with(
            &server,
            base + 9_000_000,
            &|_| AgentState::Waiting,
            &send,
            &|_, _| Ok(true),
        );
        let state = server.state.lock().unwrap();
        assert!(!state.keepalives["worker"].suspected_offline);
        assert_eq!(state.msgs.len(), 0);
        assert!(state.wake_bindings.is_empty());
        assert_eq!(sends.get(), 0);
        drop(state);
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn idle_managed_subagent_reports_to_master_without_child_wake_loop() {
        use super::super::peer_tests::{register, test_server};
        use crate::subagent::Record as SubagentRecord;
        use std::cell::Cell;

        let (server, root) = test_server();
        register(&server, "master", "%master");
        register(&server, "child", "%child");
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
                subagent: SubagentRecord {
                    id: "managed".into(),
                    parent: "master".into(),
                    peer: "child".into(),
                    status: "working".into(),
                    session: Some("session".into()),
                    pane: Some("%child".into()),
                    profile: None,
                    created_ms: now,
                    ready_deadline_ms: now + 90_000,
                    last_message: None,
                    error: None,
                    probe_failures: Vec::new(),
                    runtime: Some("cursor".into()),
                },
            },
            Event::TaskCreated {
                task: crate::server::state::TaskRec {
                    id: "task-child".into(),
                    owner: "child".into(),
                    created_by: "master".into(),
                    feature_id: None,
                    worktree_path: None,
                    branch: None,
                    base_commit: None,
                    priority: "p2".into(),
                    status: "working".into(),
                    next_step: Some("work".into()),
                    wait: None,
                    created_ms: now,
                    updated_ms: now,
                },
            },
        ]);
        let wakes = Cell::new(0);
        let wake = |_: &str, _: &str| {
            wakes.set(wakes.get() + 1);
            true
        };
        tick_with(
            &server,
            now + 900_000,
            &|_| AgentState::Waiting,
            &wake,
            &|_, _| Ok(true),
        );
        let state = server.state.lock().unwrap();
        assert_eq!(wakes.get(), 0);
        assert_eq!(state.subagents["managed"].status, "idle");
        assert_eq!(state.msgs.values().filter(|m| m.to == "child").count(), 0);
        // Durable state is current immediately, but the master is not told
        // until the state has settled.
        assert_eq!(
            state
                .msgs
                .values()
                .filter(|m| m.mtype == "subagent-status")
                .count(),
            0
        );
        drop(state);

        tick_with(
            &server,
            now + 1_800_000,
            &|_| AgentState::Waiting,
            &wake,
            &|_, _| Ok(true),
        );
        let state = server.state.lock().unwrap();
        assert_eq!(wakes.get(), 0);
        let status_messages: Vec<_> = state
            .msgs
            .values()
            .filter(|m| m.to == "master" && m.mtype == "subagent-status")
            .collect();
        assert_eq!(status_messages.len(), 1);
        assert!(status_messages[0].body.contains("state=idle"));
        drop(state);

        // A settled state is reported once, not on every later tick.
        tick_with(
            &server,
            now + 2_700_000,
            &|_| AgentState::Waiting,
            &wake,
            &|_, _| Ok(true),
        );
        let state = server.state.lock().unwrap();
        assert_eq!(wakes.get(), 0);
        assert_eq!(
            state
                .msgs
                .values()
                .filter(|m| m.mtype == "subagent-status")
                .count(),
            1
        );
        drop(state);
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn flapping_managed_subagent_does_not_flood_master() {
        use super::super::peer_tests::{register, test_server};
        use crate::subagent::Record as SubagentRecord;
        use std::cell::Cell;

        let (server, root) = test_server();
        register(&server, "master", "%master");
        register(&server, "child", "%child");
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
            subagent: SubagentRecord {
                id: "managed".into(),
                parent: "master".into(),
                peer: "child".into(),
                status: "working".into(),
                session: Some("session".into()),
                pane: Some("%child".into()),
                profile: None,
                created_ms: now,
                ready_deadline_ms: now + 90_000,
                last_message: None,
                error: None,
                probe_failures: Vec::new(),
                runtime: Some("cursor".into()),
            },
        }]);

        let wakes = Cell::new(0);
        let wake = |_: &str, _: &str| {
            wakes.set(wakes.get() + 1);
            true
        };
        let status_count = |server: &Server| {
            server
                .state
                .lock()
                .unwrap()
                .msgs
                .values()
                .filter(|m| m.mtype == "subagent-status")
                .count()
        };

        // A booting pane flaps faster than the settle window. None of these
        // transitions is worth a master notification.
        let flaps = [
            AgentState::Waiting,
            AgentState::Working,
            AgentState::Waiting,
            AgentState::Working,
            AgentState::Waiting,
        ];
        for (i, agent) in flaps.iter().enumerate() {
            let at = now + 1_000 + (i as i64 * 5_000);
            tick_with(&server, at, &|_| *agent, &wake, &|_, _| Ok(true));
        }
        assert_eq!(status_count(&server), 0, "flaps must not notify");

        // A settled managed state without actionable work remains quiet.
        tick_with(
            &server,
            now + 200_000,
            &|_| AgentState::Waiting,
            &wake,
            &|_, _| Ok(true),
        );
        assert_eq!(
            status_count(&server),
            0,
            "a managed idle state without actionable work must not wake the master"
        );
        tick_with(
            &server,
            now + 400_000,
            &|_| AgentState::Waiting,
            &wake,
            &|_, _| Ok(true),
        );
        assert_eq!(
            status_count(&server),
            0,
            "a settled managed idle state without actionable work stays quiet"
        );

        // Even a long monitoring round is not a new task episode.
        tick_with(
            &server,
            now + 500_000,
            &|_| AgentState::Working,
            &wake,
            &|_, _| Ok(true),
        );
        tick_with(
            &server,
            now + 600_000,
            &|_| AgentState::Working,
            &wake,
            &|_, _| Ok(true),
        );
        tick_with(
            &server,
            now + 700_000,
            &|_| AgentState::Waiting,
            &wake,
            &|_, _| Ok(true),
        );
        tick_with(
            &server,
            now + 800_000,
            &|_| AgentState::Waiting,
            &wake,
            &|_, _| Ok(true),
        );
        assert_eq!(
            status_count(&server),
            0,
            "monitoring cannot open a no-task managed idle episode"
        );

        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn initial_master_idle_only_records_observation() {
        use super::super::peer_tests::{register, test_server};

        let (server, root) = test_server();
        register(&server, "master", "%master");
        assert!(
            super::super::handle_master_promote(
                &server,
                "master".into(),
                "token-master".into(),
                "user approved master".into(),
            )
            .ok
        );

        let base = super::super::state::now_ms();
        tick_with(
            &server,
            base,
            &|_| AgentState::Waiting,
            &|_, _| panic!("initial idle must not wake the master"),
            &|_, _| Ok(true),
        );
        tick_with(
            &server,
            base + 120_000,
            &|_| AgentState::Waiting,
            &|_, _| panic!("initial idle must not send a reminder"),
            &|_, _| Ok(true),
        );

        let state = server.state.lock().unwrap();
        let record = state
            .keepalives
            .get("master")
            .expect("idle observation persists");
        assert_eq!(record.observed, "idle");
        assert_eq!(record.idle_episode_notices, 0);
        assert_eq!(record.idle_episode_reason, "no-actionable-tasks");
        assert!(!state
            .msgs
            .values()
            .any(|message| message.subject == Some("master-idle: master".into())));
        drop(state);
        std::fs::remove_dir_all(root).unwrap();
    }

    fn episode_server() -> (Server, std::path::PathBuf, i64) {
        use super::super::peer_tests::{register, test_server};
        let (server, root) = test_server();
        register(&server, "master", "%master");
        register(&server, "worker", "%worker");
        assert!(
            super::super::handle_master_promote(
                &server,
                "master".into(),
                "token-master".into(),
                "user approved master".into(),
            )
            .ok
        );
        let base = super::super::state::now_ms();
        (server, root, base)
    }

    fn observe(server: &Server, at: i64, worker: &str, agent: AgentState) {
        tick_with(
            server,
            at,
            &|pane| {
                if pane == format!("%{worker}") {
                    agent
                } else {
                    AgentState::Waiting
                }
            },
            &|_, _| panic!("observation must not wake workers"),
            &|_, _| Ok(true),
        );
    }

    fn idle_count(server: &Server, worker: &str) -> usize {
        let subject = format!(
            "{}-idle: {worker}",
            if worker == "master" {
                "master"
            } else {
                "worker"
            }
        );
        server
            .state
            .lock()
            .unwrap()
            .msgs
            .values()
            .filter(|message| message.subject.as_deref() == Some(subject.as_str()))
            .count()
    }

    #[test]
    fn unknown_observation_preserves_working_to_idle_transition() {
        let (server, root, base) = episode_server();
        observe(&server, base, "worker", AgentState::Working);
        observe(&server, base + 1_000, "worker", AgentState::Unknown);
        observe(&server, base + 2_000, "worker", AgentState::Waiting);
        assert_eq!(
            idle_count(&server, "worker"),
            1,
            "Unknown cannot erase real Working"
        );
        observe(&server, base + 3_000, "worker", AgentState::Waiting);
        assert_eq!(idle_count(&server, "worker"), 1);
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn actionable_worker_reports_only_once_to_master() {
        let (server, root, base) = episode_server();
        server.commit(&[Event::TaskCreated {
            task: crate::server::state::TaskRec {
                id: "assigned-work".into(),
                owner: "worker".into(),
                created_by: "master".into(),
                feature_id: None,
                worktree_path: None,
                branch: None,
                base_commit: None,
                priority: "p0".into(),
                status: "working".into(),
                next_step: None,
                wait: None,
                created_ms: base,
                updated_ms: base,
            },
        }]);
        observe(&server, base, "worker", AgentState::Working);
        observe(&server, base + 1_000, "worker", AgentState::Waiting);
        observe(&server, base + 2_000, "worker", AgentState::Working);
        observe(&server, base + 3_000, "worker", AgentState::Waiting);
        observe(&server, base + 9_000_000, "worker", AgentState::Waiting);
        assert_eq!(
            idle_count(&server, "worker"),
            1,
            "unchanged actionable tasks do not re-arm"
        );
        let state = server.state.lock().unwrap();
        assert!(state.msgs.values().all(|message| message.to == "master"));
        assert!(state
            .msgs
            .values()
            .any(|message| message.body.contains("assigned-work")));
        drop(state);
        let mut task = server.state.lock().unwrap().tasks["assigned-work"].clone();
        task.status = "verifying".into();
        server.commit(&[Event::TaskUpdated { task }]);
        observe(&server, base + 9_001_000, "worker", AgentState::Waiting);
        assert_eq!(
            idle_count(&server, "worker"),
            1,
            "task revision alone is not a Working-to-idle transition"
        );
        observe(&server, base + 9_002_000, "worker", AgentState::Working);
        observe(&server, base + 9_003_000, "worker", AgentState::Waiting);
        assert_eq!(
            idle_count(&server, "worker"),
            2,
            "new work permits the next idle episode"
        );
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn master_three_reminders_are_bounded_across_replay() {
        let (server, root, base) = episode_server();
        observe(&server, base, "master", AgentState::Working);
        for offset in [1_000, 2_000, 120_999] {
            observe(&server, base + offset, "master", AgentState::Waiting);
        }
        assert_eq!(idle_count(&server, "master"), 1);
        for offset in [121_000, 121_001, 240_999] {
            observe(&server, base + offset, "master", AgentState::Waiting);
        }
        assert_eq!(idle_count(&server, "master"), 2);
        observe(&server, base + 241_000, "master", AgentState::Waiting);
        assert_eq!(idle_count(&server, "master"), 3);
        let mut replay = State::default();
        for line in std::fs::read_to_string(root.join(".agent-collab/server/journal.jsonl"))
            .unwrap()
            .lines()
        {
            replay.apply(&serde_json::from_str::<Event>(line).unwrap());
        }
        *server.state.lock().unwrap() = replay;
        observe(&server, base + 9_000_000, "master", AgentState::Waiting);
        assert_eq!(idle_count(&server, "master"), 3);
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn missing_subscription_does_not_consume_worker_idle_episode() {
        let (server, root, base) = episode_server();
        let subscriptions = {
            let mut state = server.state.lock().unwrap();
            std::mem::take(&mut state.notification_subscriptions)
        };
        observe(&server, base, "worker", AgentState::Working);
        observe(&server, base + 1_000, "worker", AgentState::Waiting);
        assert_eq!(
            idle_count(&server, "worker"),
            0,
            "no unbound scheduling notice"
        );
        assert_eq!(
            server.state.lock().unwrap().keepalives["worker"].idle_episode_notices,
            0
        );
        server.state.lock().unwrap().notification_subscriptions = subscriptions;
        observe(&server, base + 2_000, "worker", AgentState::Waiting);
        assert_eq!(
            idle_count(&server, "worker"),
            1,
            "available subscription binds pending transition"
        );
        let state = server.state.lock().unwrap();
        let notice = state
            .msgs
            .values()
            .find(|message| message.subject.as_deref() == Some("worker-idle: worker"))
            .unwrap();
        assert!(state.wake_bindings.contains_key(&notice.id));
        drop(state);
        std::fs::remove_dir_all(root).unwrap();
    }

    fn stale_master_subscription_preserves_episode(observed_worker: &str, managed: bool) {
        for stale_kind in ["mismatched", "dead", "unowned"] {
            let (mut server, root, base) = episode_server();
            if stale_kind == "dead" {
                server.pane_alive_check = |pane| {
                    if pane == "%stale" {
                        super::super::knock::PanePresence::Missing
                    } else {
                        super::super::knock::PanePresence::Present
                    }
                };
            }
            if stale_kind == "unowned" {
                server.pane_owner_check = |_, pane| Ok(pane != "%stale");
            }
            if managed {
                server.commit(&[
                    Event::SubagentUpdated {
                        subagent: crate::subagent::Record {
                            id: "managed".into(),
                            parent: "master".into(),
                            peer: "worker".into(),
                            status: "working".into(),
                            session: Some("session".into()),
                            pane: Some("%worker".into()),
                            profile: None,
                            created_ms: base,
                            ready_deadline_ms: base + 90_000,
                            last_message: None,
                            error: None,
                            probe_failures: Vec::new(),
                            runtime: Some("cursor".into()),
                        },
                    },
                    Event::TaskCreated {
                        task: crate::server::state::TaskRec {
                            id: "managed-task".into(),
                            owner: "worker".into(),
                            created_by: "master".into(),
                            feature_id: None,
                            worktree_path: None,
                            branch: None,
                            base_commit: None,
                            priority: "p2".into(),
                            status: "working".into(),
                            next_step: None,
                            wait: None,
                            created_ms: base,
                            updated_ms: base,
                        },
                    },
                ]);
            }
            let valid = server
                .state
                .lock()
                .unwrap()
                .matching_subscription("master", "direct-message", None, base)
                .unwrap()
                .clone();
            let mut stale = valid.clone();
            stale.pane = "%stale".into();
            server.commit(&[Event::NotificationSubscribed {
                subscription: stale,
            }]);

            observe(&server, base, observed_worker, AgentState::Working);
            observe(&server, base + 1_000, observed_worker, AgentState::Waiting);
            observe(&server, base + 62_000, observed_worker, AgentState::Waiting);
            {
                let state = server.state.lock().unwrap();
                assert_eq!(
                    state.msgs.len(),
                    0,
                    "{stale_kind} subscription must not create a notice"
                );
                assert!(state.wake_bindings.is_empty());
                let record = &state.keepalives[observed_worker];
                assert_eq!(
                    record.idle_episode_notices, 0,
                    "{stale_kind} subscription cannot consume the episode"
                );
                assert!(record.working_seen, "real transition remains pending");
                assert!(record.notified_state.is_empty());
            }

            server.commit(&[Event::NotificationSubscribed {
                subscription: valid.clone(),
            }]);
            observe(&server, base + 63_000, observed_worker, AgentState::Waiting);
            observe(&server, base + 64_000, observed_worker, AgentState::Waiting);
            let state = server.state.lock().unwrap();
            assert_eq!(
                state.msgs.len(),
                1,
                "valid subscription recovers the same transition once"
            );
            let message = state.msgs.values().next().unwrap();
            assert_eq!(message.to, "master");
            assert_eq!(state.wake_bindings.get(&message.id), Some(&valid.id));
            assert_eq!(state.keepalives[observed_worker].idle_episode_notices, 1);
            drop(state);
            std::fs::remove_dir_all(root).unwrap();
        }
    }

    #[test]
    fn stale_subscription_preserves_worker_idle_until_rebound() {
        stale_master_subscription_preserves_episode("worker", false);
    }

    #[test]
    fn stale_subscription_preserves_master_idle_until_rebound() {
        stale_master_subscription_preserves_episode("master", false);
    }

    #[test]
    fn stale_subscription_preserves_managed_idle_until_rebound() {
        stale_master_subscription_preserves_episode("worker", true);
    }

    #[test]
    fn master_working_stops_reminders_across_replay() {
        let (server, root, base) = episode_server();
        observe(&server, base, "master", AgentState::Working);
        observe(&server, base + 1_000, "master", AgentState::Waiting);
        assert_eq!(idle_count(&server, "master"), 1);
        observe(&server, base + 2_000, "master", AgentState::Working);
        let mut replay = State::default();
        for line in std::fs::read_to_string(root.join(".agent-collab/server/journal.jsonl"))
            .unwrap()
            .lines()
        {
            replay.apply(&serde_json::from_str::<Event>(line).unwrap());
        }
        *server.state.lock().unwrap() = replay;
        observe(&server, base + 130_000, "master", AgentState::Waiting);
        observe(&server, base + 260_000, "master", AgentState::Waiting);
        assert_eq!(
            idle_count(&server, "master"),
            1,
            "Working ends the reminder episode durably"
        );
        assert_eq!(
            server.state.lock().unwrap().keepalives["master"].idle_episode_notices,
            1,
            "count remains truthful"
        );
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn managed_idle_waits_for_master_subscription() {
        let (server, root, base) = episode_server();
        server.commit(&[
            Event::SubagentUpdated {
                subagent: crate::subagent::Record {
                    id: "managed".into(),
                    parent: "master".into(),
                    peer: "worker".into(),
                    status: "working".into(),
                    session: Some("session".into()),
                    pane: Some("%worker".into()),
                    profile: None,
                    created_ms: base,
                    ready_deadline_ms: base + 90_000,
                    last_message: None,
                    error: None,
                    probe_failures: Vec::new(),
                    runtime: Some("cursor".into()),
                },
            },
            Event::TaskCreated {
                task: crate::server::state::TaskRec {
                    id: "managed-task".into(),
                    owner: "worker".into(),
                    created_by: "master".into(),
                    feature_id: None,
                    worktree_path: None,
                    branch: None,
                    base_commit: None,
                    priority: "p2".into(),
                    status: "working".into(),
                    next_step: None,
                    wait: None,
                    created_ms: base,
                    updated_ms: base,
                },
            },
        ]);
        let subscriptions =
            std::mem::take(&mut server.state.lock().unwrap().notification_subscriptions);
        observe(&server, base + 1_000, "worker", AgentState::Waiting);
        observe(&server, base + 62_000, "worker", AgentState::Waiting);
        assert_eq!(
            server.state.lock().unwrap().keepalives["worker"].notified_state,
            "",
            "unbound managed notice is not reported"
        );
        server.state.lock().unwrap().notification_subscriptions = subscriptions;
        observe(&server, base + 63_000, "worker", AgentState::Waiting);
        let state = server.state.lock().unwrap();
        let notices: Vec<_> = state
            .msgs
            .values()
            .filter(|message| message.mtype == "subagent-status")
            .collect();
        assert_eq!(notices.len(), 1);
        assert!(state.wake_bindings.contains_key(&notices[0].id));
        drop(state);
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn idle_worker_episode_stays_one_shot_until_actionable_task_changes() {
        use super::super::peer_tests::{register, test_server};
        use std::cell::Cell;
        use std::sync::Arc;

        let (server, root) = test_server();
        register(&server, "master", "%master");
        register(&server, "worker", "%worker");
        register(&server, "initial-idle", "%initial-idle");
        let server = Arc::new(server);
        assert!(
            super::super::handle_master_promote(
                &server,
                "master".into(),
                "token-master".into(),
                "user approved master".into(),
            )
            .ok
        );

        let base = super::super::state::now_ms();
        let mut initial = Record::default();
        initial.observed = "working".into();
        initial.idle_since_ms = base;
        server.commit(&[Event::KeepaliveUpdated {
            worker_id: "worker".into(),
            record: initial,
        }]);

        let wakes = Cell::new(0);
        let wake = |_: &str, _: &str| {
            wakes.set(wakes.get() + 1);
            true
        };
        let tick = |server: &Server, at: i64, agent: AgentState| {
            tick_with(server, at, &|_| agent, &wake, &|_, _| Ok(true));
        };

        tick(&server, base, AgentState::Waiting);
        assert!(!server.state.lock().unwrap().msgs.values().any(|message| {
            message.to == "master" && message.subject == Some("worker-idle: initial-idle".into())
        }));
        tick(&server, base + 1_000, AgentState::Waiting);
        tick(&server, base + 2_000, AgentState::Waiting);
        tick(&server, base + 3_000, AgentState::Working);
        tick(&server, base + 4_000, AgentState::Waiting);
        tick(&server, base + 5_000, AgentState::Waiting);

        let state = server.state.lock().unwrap();
        let idle_alerts: Vec<_> = state
            .msgs
            .values()
            .filter(|message| {
                message.to == "master" && message.subject == Some("worker-idle: worker".into())
            })
            .collect();
        assert_eq!(
            idle_alerts.len(),
            1,
            "monitor probe flaps do not re-arm idle"
        );
        assert_eq!(
            state
                .msgs
                .values()
                .filter(|message| message.to == "worker")
                .count(),
            0,
            "idle observation never wakes the worker"
        );
        assert_eq!(
            wakes.get(),
            0,
            "idle observation does not call the worker wake path"
        );
        drop(state);

        // A real master dispatch remains an explicit, immediate message and
        // is the only worker notification in this sequence.
        let dispatch = super::super::handle_send(
            &server,
            "master".into(),
            "worker".into(),
            "notify".into(),
            Some("monitor-open-p0p1-worker".into()),
            "dispatch the assigned work".into(),
            None,
            "immediate".into(),
        );
        assert!(dispatch.ok, "master dispatch must remain accepted");
        let dispatch_id = dispatch.data["msg_id"].as_str().unwrap().to_owned();
        let duplicate = super::super::handle_send(
            &server,
            "master".into(),
            "worker".into(),
            "notify".into(),
            Some("monitor-open-p0p1-worker".into()),
            "dispatch the assigned work".into(),
            None,
            "immediate".into(),
        );
        assert!(duplicate.ok);
        assert_eq!(duplicate.data["deduplicated"], true);
        let state = server.state.lock().unwrap();
        assert_eq!(
            state
                .msgs
                .values()
                .filter(|message| {
                    message.to == "worker"
                        && message.subject == Some("monitor-open-p0p1-worker".into())
                })
                .count(),
            1,
            "unchanged monitor is one durable dispatch"
        );
        drop(state);

        // Reading/ACKing the explicit dispatch and a restart preserve the
        // already reported idle revision; neither is a new capacity event.
        server.commit(&[
            Event::Delivered {
                ids: vec![dispatch_id.clone()],
            },
            Event::Acked {
                ids: vec![dispatch_id],
            },
        ]);
        tick(&server, base + 6_000, AgentState::Working);
        tick(&server, base + 7_000, AgentState::Waiting);
        let mut replay = State::default();
        for line in std::fs::read_to_string(root.join(".agent-collab/server/journal.jsonl"))
            .unwrap()
            .lines()
        {
            replay.apply(&serde_json::from_str::<Event>(line).unwrap());
        }
        *server.state.lock().unwrap() = replay;
        tick(&server, base + 8_000, AgentState::Waiting);
        let state = server.state.lock().unwrap();
        assert_eq!(
            state
                .msgs
                .values()
                .filter(|message| {
                    message.to == "master" && message.subject == Some("worker-idle: worker".into())
                })
                .count(),
            1,
            "ACK/read and replay do not reset the idle revision"
        );
        drop(state);

        // Only an actionable task changes the observation revision and permits
        // a later worker-idle signal.
        let task = crate::server::state::TaskRec {
            id: "task-worker".into(),
            owner: "worker".into(),
            created_by: "master".into(),
            feature_id: None,
            worktree_path: None,
            branch: None,
            base_commit: None,
            priority: "p2".into(),
            status: "working".into(),
            next_step: Some("work".into()),
            wait: None,
            created_ms: base,
            updated_ms: base,
        };
        server.commit(&[Event::TaskCreated { task: task.clone() }]);
        tick(&server, base + 9_000, AgentState::Working);
        let mut closed = task;
        closed.status = "closed".into();
        closed.updated_ms = base + 10_000;
        server.commit(&[Event::TaskUpdated { task: closed }]);
        tick(&server, base + 11_000, AgentState::Waiting);
        let state = server.state.lock().unwrap();
        assert_eq!(
            state
                .msgs
                .values()
                .filter(|message| {
                    message.to == "master" && message.subject == Some("worker-idle: worker".into())
                })
                .count(),
            2,
            "task assignment and release permit a new idle revision"
        );
        drop(state);
        std::fs::remove_dir_all(root).unwrap();
    }
}
