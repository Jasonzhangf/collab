use super::{
    knock::AgentState,
    state::{Event, Message, State, WorkerRec},
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
}

fn actionable(status: &str) -> bool {
    matches!(
        status,
        "assigned" | "working" | "verifying" | "reviewed" | "rework" | "delivered" | "merged"
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

fn managed_subagent_target(state: &State, server: &Server) -> Option<String> {
    let target = super::live_master_id(server, state)?;
    let worker = state.workers.get(&target)?;
    let pane = worker.pane.as_deref()?;
    ((server.pane_alive_check)(pane) && (server.pane_owner_check)(&target, pane)).then_some(target)
}

fn handle_managed_subagent(
    server: &Server,
    state: &mut State,
    worker: &WorkerRec,
    tasks: &[String],
    agent: AgentState,
    now: i64,
) -> (bool, Option<(String, String)>) {
    let Some(subagent) = state
        .subagents
        .values()
        .find(|subagent| subagent.peer == worker.id)
        .cloned()
    else {
        return (false, None);
    };
    if !matches!(agent, AgentState::Waiting | AgentState::Working) {
        return (false, None);
    }
    let observed = observed_label(agent);
    let old = state.keepalives.get(&worker.id).cloned().unwrap_or_default();
    let changed = old.observed != observed || subagent.status != observed;
    let mut record = old;
    record.observed = observed.into();
    if changed {
        record.idle_since_ms = now;
        record.activity_ms = now;
        record.unacked = 0;
        record.last_notice_id = None;
    }
    let mut current = subagent.clone();
    current.status = observed.into();
    let target = changed
        .then(|| managed_subagent_target(state, server))
        .flatten();
    let subscription = target.as_ref().and_then(|target| {
        state
            .matching_subscription(target, "direct-message", None, now)
            .cloned()
    });
    let mut events = vec![Event::KeepaliveUpdated {
        worker_id: worker.id.clone(),
        record,
    }];
    if changed {
        events.push(Event::SubagentUpdated { subagent: current });
        if let Some(target) = &target {
            let id = super::gen_msg_id();
            let body = format!("subagent={} state={} tasks={}", subagent.id, observed, tasks.join(","));
            events.push(Event::Sent {
                msg: Message {
                    id: id.clone(),
                    from: "collab-server".into(),
                    to: target.clone(),
                    mtype: "subagent-status".into(),
                    subject: Some("subagent-status".into()),
                    body,
                    in_reply_to: None,
                    created_ms: now,
                    state: "pending".into(),
                    wake_attempt_count: 0,
                    last_wake_attempt_ms: 0,
                },
            });
            events.push(Event::DeliveryMode {
                msg_id: id.clone(),
                mode: "explicit-notification".into(),
            });
            if let Some(subscription) = &subscription {
                events.push(Event::WakeBound {
                    message_id: id,
                    subscription_id: subscription.id.clone(),
                });
            }
        }
    }
    server.commit_locked(state, &events);
    let notification = if let (Some(target), Some(subscription)) = (target, subscription) {
        let message_id = events.iter().find_map(|event| match event {
            Event::Sent { msg } if msg.to == target => Some(msg.id.clone()),
            _ => None,
        });
        message_id.map(|message_id| (message_id, subscription.id))
    } else {
        None
    };
    (true, notification)
}

fn advance(
    record: &mut Record,
    now: i64,
    agent: AgentState,
    activity: i64,
    acked: bool,
    interval: i64,
    limit: u8,
) -> bool {
    let observed = match agent {
        AgentState::Waiting => "idle",
        AgentState::Working => "working",
        AgentState::Unknown => "unknown",
        AgentState::Absent => "absent",
    };
    if record.observed != observed {
        record.observed = observed.into();
        record.idle_since_ms = now;
    }
    if record.suspected_offline {
        return false;
    }
    if activity > record.activity_ms || acked || agent == AgentState::Working {
        if activity > record.activity_ms || acked || record.unacked > 0 {
            record.activity_ms = now.max(activity);
            record.unacked = 0;
            record.last_notice_id = None;
            record.idle_since_ms = now;
        }
        return false;
    }
    if record.unacked >= limit {
        if now - record.last_notice_ms >= interval {
            record.suspected_offline = true;
        }
        return false;
    }
    if agent != AgentState::Waiting
        || now - record.idle_since_ms < interval
        || (record.unacked > 0 && now - record.last_notice_ms < interval)
    {
        return false;
    }
    record.unacked += 1;
    record.last_notice_ms = now;
    true
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
    tick_with(
        server,
        super::state::now_ms(),
        &super::knock::probe_agent_state,
        &|pane, text| super::knock_or_log(&server.log_path(), pane, text),
        &|worker_id, pane| (server.pane_owner_check)(worker_id, pane),
    );
}

pub(crate) fn tick_with(
    server: &Server,
    now: i64,
    probe: &dyn Fn(&str) -> AgentState,
    wake: &dyn Fn(&str, &str) -> bool,
    owns_pane: &dyn Fn(&str, &str) -> bool,
) {
    if !server.config.keepalive.enabled || !server.config.timers.enabled {
        return;
    }
    let mut state = server.state.lock().unwrap();
    if state.admission_frozen() {
        return;
    }
    let workers: Vec<_> = state.workers.values().cloned().collect();
    for worker in workers {
        let mut tasks: Vec<_> = state
            .tasks
            .values()
            .filter(|t| t.owner == worker.id && actionable(&t.status))
            .map(|t| t.id.clone())
            .collect();
        tasks.sort();
        let Some(pane) = worker.pane.as_deref() else {
            continue;
        };
        let agent = probe(pane);
        if !(server.pane_alive_check)(pane) || !owns_pane(&worker.id, pane) || agent == AgentState::Absent {
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
                if let Some(master_id) = super::live_master_id(server, &state) {
                    if master_id != worker.id {
                        let alert_id = super::gen_msg_id();
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
                        if let Some(sub) = state.matching_subscription(&master_id, "direct-message", None, now) {
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
        let (managed, notification) =
            handle_managed_subagent(server, &mut state, &worker, &tasks, agent, now);
        if managed {
            drop(state);
            if let Some((message_id, subscription_id)) = notification {
                super::attempt_notification(server, &message_id, &subscription_id);
            }
            state = server.state.lock().unwrap();
            continue;
        }
        let old = state
            .keepalives
            .get(&worker.id)
            .cloned()
            .unwrap_or_default();
        let mut record = old.clone();

        if tasks.is_empty() {
            let was_working = old.observed == "working";
            let is_idle = agent == AgentState::Waiting;
            let observed_str = match agent {
                AgentState::Waiting => "idle",
                AgentState::Working => "working",
                AgentState::Unknown => "unknown",
                AgentState::Absent => "absent",
            };
            if record.observed != observed_str {
                record.observed = observed_str.into();
                record.idle_since_ms = now;
            }
            let mut events = Vec::new();
            if was_working && is_idle {
                if let Some(master_id) = super::live_master_id(server, &state) {
                    if master_id != worker.id {
                        let alert_id = super::gen_msg_id();
                        events.push(Event::Sent {
                            msg: Message {
                                id: alert_id.clone(),
                                from: "collab-server".into(),
                                to: master_id.clone(),
                                mtype: "notify".into(),
                                subject: Some(format!("worker-idle: {}", worker.id)),
                                body: format!(
                                    "Worker {} is now idle with no active task. Action required: check task graph for unblocked downstream tasks, or pull from appsdk bug list --status open (P0/P1). If all work is complete, propose next steps to user and pause.",
                                    worker.id
                                ),
                                in_reply_to: None,
                                created_ms: now,
                                state: "pending".into(),
                                wake_attempt_count: 0,
                                last_wake_attempt_ms: 0,
                            },
                        });
                        if let Some(sub) = state.matching_subscription(&master_id, "direct-message", None, now) {
                            events.push(Event::WakeBound {
                                message_id: alert_id,
                                subscription_id: sub.id.clone(),
                            });
                        }
                    }
                }
            }
            if record != old {
                events.insert(0, Event::KeepaliveUpdated {
                    worker_id: worker.id.clone(),
                    record,
                });
            }
            if !events.is_empty() {
                server.commit_locked(&mut state, &events);
            }
            continue;
        }
        let activity = state
            .msgs
            .values()
            .filter(|m| m.from == worker.id)
            .map(|m| m.created_ms)
            .chain(
                state
                    .tasks
                    .values()
                    .filter(|t| t.owner == worker.id)
                    .map(|t| t.updated_ms),
            )
            .max()
            .unwrap_or(0);
        let acked = record
            .last_notice_id
            .as_ref()
            .and_then(|id| state.msgs.get(id))
            .is_some_and(|m| m.state == "read");
        let subscription = state
            .matching_subscription(&worker.id, "direct-message", None, now)
            .cloned();
        let due = advance(
            &mut record,
            now,
            agent,
            activity,
            acked,
            server.config.keepalive.interval_seconds as i64 * 1000,
            server.config.keepalive.max_unacked,
        );
        if !old.suspected_offline && record.suspected_offline {
            if let Some(master_id) = super::live_master_id(server, &state) {
                if master_id != worker.id {
                    let alert_id = super::gen_msg_id();
                    let mut alert_events = vec![Event::Sent {
                        msg: Message {
                            id: alert_id.clone(),
                            from: "collab-server".into(),
                            to: master_id.clone(),
                            mtype: "notify".into(),
                            subject: Some(format!("worker-unresponsive: {}", worker.id)),
                            body: format!(
                                "Worker {} is unresponsive ({} unacked keepalives). Diagnostic closure required: run collab subagent snapshot {} --lines 40 to inspect ground truth and determine recovery action.",
                                worker.id, server.config.keepalive.max_unacked, worker.id
                            ),
                            in_reply_to: None,
                            created_ms: now,
                            state: "pending".into(),
                            wake_attempt_count: 0,
                            last_wake_attempt_ms: 0,
                        },
                    }];
                    if let Some(sub) = state.matching_subscription(&master_id, "direct-message", None, now) {
                        alert_events.push(Event::WakeBound {
                            message_id: alert_id,
                            subscription_id: sub.id.clone(),
                        });
                    }
                    server.commit_locked(&mut state, &alert_events);
                }
            }
        }
        // Missing subscription/disabled notifications never consumes a send or queues one.
        if due && (subscription.is_none() || !server.config.notifications.enabled) {
            record.unacked = old.unacked;
            record.last_notice_ms = old.last_notice_ms;
        }
        if !due || subscription.is_none() || !server.config.notifications.enabled {
            if record != old {
                server.commit_locked(
                    &mut state,
                    &[Event::KeepaliveUpdated {
                        worker_id: worker.id.clone(),
                        record,
                    }],
                );
            }
            continue;
        }
        let id = super::gen_msg_id();
        let subject = format!(
            "task-keepalive {}/{}",
            record.unacked, server.config.keepalive.max_unacked
        );
        let body = format!("Unfinished tasks: {}. Read task state, ACK this notice once with collab ack {}, then resume actionable work or record a real blocker. Do not reply with another ACK request.", tasks.join(", "), id);
        record.last_notice_id = Some(id.clone());
        server.commit_locked(
            &mut state,
            &[
                Event::KeepaliveUpdated {
                    worker_id: worker.id.clone(),
                    record,
                },
                Event::Sent {
                    msg: Message {
                        id: id.clone(),
                        from: "collab-server".into(),
                        to: worker.id.clone(),
                        mtype: "keepalive".into(),
                        subject: Some(subject.clone()),
                        body: body.clone(),
                        in_reply_to: None,
                        created_ms: now,
                        state: "pending".into(),
                        wake_attempt_count: 0,
                        last_wake_attempt_ms: 0,
                    },
                },
                Event::WakeAttempted {
                    ids: vec![id.clone()],
                    attempted_ms: now,
                },
            ],
        );
        // No WakeBound: this reserved one-shot must never join a delayed/replayed queue.
        drop(state);
        if probe(pane) == AgentState::Waiting
            && wake(pane, &format!("COLLAB_NOTIFY {id} [{subject}] {body}"))
        {
            server.commit(&[Event::Delivered { ids: vec![id] }]);
        }
        state = server.state.lock().unwrap();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn scheduler_groups_tasks_and_persists_failed_attempts() {
        use super::super::{
            peer_tests::{register, test_server},
            state::TaskRec,
        };
        let (server, root) = test_server();
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
            assert!(text.contains("one, two"));
            sends.set(sends.get() + 1);
            false
        };
        tick_with(&server, base, &|_| AgentState::Waiting, &send, &|_, _| true);
        for n in 1..=3 {
            tick_with(
                &server,
                base + n * 900_000,
                &|_| AgentState::Waiting,
                &send,
                &|_, _| true,
            );
            tick_with(
                &server,
                base + n * 900_000,
                &|_| AgentState::Waiting,
                &send,
                &|_, _| true,
            );
        }
        assert_eq!(sends.get(), 3);
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
            &|_, _| true,
        );
        tick_with(
            &server,
            base + 9_000_000,
            &|_| AgentState::Waiting,
            &send,
            &|_, _| true,
        );
        let state = server.state.lock().unwrap();
        assert!(state.keepalives["worker"].suspected_offline);
        assert_eq!(state.msgs.len(), 3);
        assert!(state.msgs.values().all(|m| m.wake_attempt_count == 1));
        assert!(state.wake_bindings.is_empty());
        assert_eq!(sends.get(), 3);
        drop(state);
        std::fs::remove_dir_all(root).unwrap();
    }
    #[test]
    fn three_unanswered_attempts_stop_across_replay() {
        let mut r = Record::default();
        assert!(!advance(&mut r, 1, AgentState::Waiting, 0, false, 900, 3));
        for t in [901, 1801, 2701] {
            assert!(advance(&mut r, t, AgentState::Waiting, 0, false, 900, 3));
            r = serde_json::from_str(&serde_json::to_string(&r).unwrap()).unwrap();
        }
        assert!(!advance(
            &mut r,
            3601,
            AgentState::Waiting,
            0,
            false,
            900,
            3
        ));
        assert!(r.suspected_offline);
        assert!(!advance(
            &mut r,
            4501,
            AgentState::Working,
            4501,
            true,
            900,
            3
        ));
        assert_eq!(r.unacked, 3);
    }
    #[test]
    fn ack_or_sending_rearms_only_before_exhaustion() {
        let mut r = Record::default();
        advance(&mut r, 1, AgentState::Waiting, 0, false, 900, 3);
        assert!(advance(&mut r, 901, AgentState::Waiting, 0, false, 900, 3));
        assert!(!advance(&mut r, 902, AgentState::Waiting, 0, true, 900, 3));
        assert_eq!(r.unacked, 0);
        assert!(!advance(
            &mut r,
            1801,
            AgentState::Waiting,
            0,
            false,
            900,
            3
        ));
        assert!(advance(&mut r, 1802, AgentState::Waiting, 0, false, 900, 3));
        assert!(!advance(
            &mut r,
            1803,
            AgentState::Waiting,
            1803,
            false,
            900,
            3
        ));
        assert_eq!(r.unacked, 0);
    }
    #[test]
    fn working_unknown_absent_never_wake() {
        for agent in [AgentState::Working, AgentState::Unknown, AgentState::Absent] {
            let mut r = Record::default();
            for t in [1, 901, 1801, 9001] {
                assert!(!advance(&mut r, t, agent, 0, false, 900, 3));
            }
            assert_eq!(r.unacked, 0);
        }
    }

    #[test]
    fn idle_managed_subagent_reports_to_master_without_child_wake_loop() {
        use super::super::peer_tests::{register, test_server};
        use crate::subagent::Record as SubagentRecord;
        use std::cell::Cell;

        let (server, root) = test_server();
        register(&server, "master", "%master");
        register(&server, "child", "%child");
        assert!(super::super::handle_master_promote(
            &server,
            "master".into(),
            "token-master".into(),
            "user approved master".into(),
        ).ok);
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
        tick_with(&server, now + 900_000, &|_| AgentState::Waiting, &wake, &|_, _| true);
        let state = server.state.lock().unwrap();
        assert_eq!(wakes.get(), 0);
        assert_eq!(state.subagents["managed"].status, "idle");
        assert_eq!(state.msgs.values().filter(|m| m.to == "child").count(), 0);
        let status_messages: Vec<_> = state
            .msgs
            .values()
            .filter(|m| m.to == "master" && m.mtype == "subagent-status")
            .collect();
        assert_eq!(status_messages.len(), 1);
        assert!(status_messages[0].body.contains("state=idle"));
        drop(state);
        tick_with(&server, now + 1_800_000, &|_| AgentState::Waiting, &wake, &|_, _| true);
        let state = server.state.lock().unwrap();
        assert_eq!(wakes.get(), 0);
        assert_eq!(state.msgs.values().filter(|m| m.mtype == "subagent-status").count(), 1);
        drop(state);
        std::fs::remove_dir_all(root).unwrap();
    }
}
