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

fn managed_subagent_target(state: &State, server: &Server) -> Option<String> {
    let target = super::live_master_id(server, state)?;
    let worker = state.workers.get(&target)?;
    let pane = worker.pane.as_deref()?;
    ((server.pane_alive_check)(pane) == super::knock::PanePresence::Present
        && (server.pane_owner_check)(&target, pane) == Ok(true))
    .then_some(target)
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
    let old = state
        .keepalives
        .get(&worker.id)
        .cloned()
        .unwrap_or_default();
    let changed = old.observed != observed || subagent.status != observed;
    let mut record = old;
    record.observed = observed.into();
    if changed {
        record.idle_since_ms = now;
        record.activity_ms = now;
        record.unacked = 0;
        record.last_notice_id = None;
    }

    // Durable state follows every observation; the master hears only settled,
    // genuinely new states. A flap that returns to what the master already
    // knows produces no message at all.
    let notify_due = if observed == record.notified_state {
        record.pending_since_ms = 0;
        false
    } else {
        if record.pending_since_ms == 0 || changed {
            record.pending_since_ms = now;
        }
        now - record.pending_since_ms >= SUBAGENT_STATE_SETTLE_MS
    };
    if notify_due {
        record.notified_state = observed.into();
        record.pending_since_ms = 0;
    }

    let mut current = subagent.clone();
    current.status = observed.into();
    let target = notify_due
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
    }
    if notify_due {
        if let Some(target) = &target {
            let id = super::gen_msg_id();
            let body = format!(
                "subagent={} state={} tasks={}",
                subagent.id,
                observed,
                tasks.join(",")
            );
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
            if let Some(subscription) = &subscription {
                events.push(Event::WakeBound {
                    message_id: id,
                    subscription_id: subscription.id.clone(),
                });
            }
        }
    } else if !changed {
        return (true, None);
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
        // Failed observation must not erase the working -> idle edge or an
        // existing idle episode, including managed-subagent observations.
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
        let (managed, notification) =
            handle_managed_subagent(server, &mut state, &worker, &tasks, agent, now);
        if managed {
            drop(state);
            if let Some((message_id, subscription_id)) = notification {
                super::attempt_notification(server, &message_id, &subscription_id);
            }
            continue;
        }
        let old = state
            .keepalives
            .get(&worker.id)
            .cloned()
            .unwrap_or_default();
        let mut record = old.clone();

        // An actionable task is the only scheduler-owned transition that
        // starts a fresh idle episode. A monitor/ACK round can make an idle
        // worker briefly appear working while it handles the message; that
        // probe flap must not re-arm the same worker-idle observation.
        if !tasks.is_empty()
            && (record.idle_episode_notices != 0 || !record.idle_episode_reason.is_empty())
        {
            record.idle_episode_notices = 0;
            record.idle_episode_reason.clear();
        }

        if tasks.is_empty() {
            let was_working = old.observed == "working";
            let is_idle = agent == AgentState::Waiting;
            let idle_reason = "no-actionable-tasks";
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
            record.unacked = 0;
            record.last_notice_id = None;
            let mut events = Vec::new();
            let new_idle_reason = is_idle && record.idle_episode_reason != idle_reason;
            if new_idle_reason {
                record.idle_episode_reason = idle_reason.into();
            }
            let is_live_master = super::live_master_id(server, &state)
                .is_some_and(|master_id| master_id == worker.id);
            let idle_notification_due = if is_live_master {
                is_idle
                    && record.idle_episode_notices < 3
                    && ((was_working && record.idle_episode_notices == 0)
                        || (record.idle_episode_notices > 0
                            && now.saturating_sub(record.last_notice_ms) >= 120_000))
            } else {
                is_idle && was_working && record.idle_episode_notices == 0
            };
            if idle_notification_due {
                if let Some(master_id) = super::live_master_id(server, &state) {
                    if master_id == worker.id {
                        if let Some(sub) =
                            state.matching_subscription(&worker.id, "direct-message", None, now)
                        {
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
                    } else {
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
                        record.idle_episode_notices = record.idle_episode_notices.saturating_add(1);
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
                    if let Some(sub) =
                        state.matching_subscription(&master_id, "direct-message", None, now)
                    {
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
        let body = format!("Unfinished tasks: {}. Continue the named task now: read its state, do the next concrete step, and update it. If it is genuinely blocked, record the blocker and the concrete proposed fix. Reading this notice is not progress.", tasks.join(", "));
        record.last_notice_id = Some(id.clone());
        let subscription_id = subscription.as_ref().expect("checked above").id.clone();
        let mut events = vec![
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
        ];
        events.push(Event::WakeBound {
            message_id: id.clone(),
            subscription_id: subscription_id.clone(),
        });
        server.commit_locked(&mut state, &events);
        drop(state);
        // The common notification path owns batching, attempt accounting, and
        // delivery; task keepalives must not bypass it with a direct wake.
        super::attempt_notification_with_at(
            server,
            &id,
            &subscription_id,
            &|_| true,
            &|pane, text| wake(pane, text),
            &|worker_id, pane| (server.pane_owner_check)(worker_id, pane),
            now,
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn unknown_prechecks_preserve_working_edge_and_idle_episode_without_worker_wake() {
        use super::super::knock::PanePresence;
        use super::super::peer_tests::{register, test_server};
        for failure in ["presence", "ownership", "view"] {
            let (mut server, root) = test_server();
            register(&server, "master", "%master");
            register(&server, "worker", "%worker");
            assert!(
                super::super::handle_master_promote(
                    &server,
                    "master".into(),
                    "token-master".into(),
                    "approved".into()
                )
                .ok
            );
            let base = super::super::state::now_ms();
            let initial = Record {
                observed: "working".into(),
                idle_since_ms: base,
                ..Record::default()
            };
            server.commit(&[Event::KeepaliveUpdated {
                worker_id: "worker".into(),
                record: initial.clone(),
            }]);
            for episode in 0..2 {
                let before = server.state.lock().unwrap().keepalives["worker"].clone();
                let message_count = server.state.lock().unwrap().msgs.len();
                match failure {
                    "presence" => {
                        server.pane_alive_check = |pane| {
                            if pane == "%worker" {
                                PanePresence::Unknown
                            } else {
                                PanePresence::Present
                            }
                        }
                    }
                    "ownership" => {
                        server.pane_owner_check = |worker, _| {
                            if worker == "worker" {
                                Err(())
                            } else {
                                Ok(true)
                            }
                        }
                    }
                    _ => {
                        server.pane_state_check = |pane| {
                            if pane == "%worker" {
                                AgentState::Unknown
                            } else {
                                AgentState::Waiting
                            }
                        }
                    }
                }
                tick_with(
                    &server,
                    base + episode * 10_000 + 1,
                    &server.pane_state_check,
                    &|_, _| panic!("unknown never wakes a worker"),
                    &server.pane_owner_check,
                );
                {
                    let state = server.state.lock().unwrap();
                    assert_eq!(
                        state.keepalives["worker"], before,
                        "{failure} must preserve the episode"
                    );
                    assert_eq!(state.msgs.len(), message_count, "unknown must not notify");
                }
                server.pane_alive_check = |_| PanePresence::Present;
                server.pane_owner_check = |_, _| Ok(true);
                server.pane_state_check = |_| AgentState::Waiting;
                tick_with(
                    &server,
                    base + episode * 10_000 + 2,
                    &server.pane_state_check,
                    &|_, _| panic!("idle observation never wakes the worker"),
                    &server.pane_owner_check,
                );
                let state = server.state.lock().unwrap();
                assert_eq!(
                    state
                        .msgs
                        .values()
                        .filter(|m| m.subject.as_deref() == Some("worker-idle: worker"))
                        .count(),
                    1
                );
                assert!(!state.msgs.values().any(|m| m.to == "worker"));
                assert!(!state.keepalives["worker"].suspected_offline);
            }
            std::fs::remove_dir_all(root).unwrap();
        }
    }

    #[test]
    fn confirmed_missing_precheck_still_records_absence() {
        use super::super::peer_tests::{register, test_server};
        let (mut server, root) = test_server();
        register(&server, "worker", "%worker");
        server.pane_alive_check = |_| super::super::knock::PanePresence::Missing;
        tick_with(
            &server,
            super::super::state::now_ms(),
            &|_| panic!("missing pane is not probed"),
            &|_, _| panic!("missing pane is not woken"),
            &|_, _| panic!("missing pane ownership is not queried"),
        );
        let state = server.state.lock().unwrap();
        assert!(state.keepalives["worker"].suspected_offline);
        assert_eq!(state.keepalives["worker"].observed, "absent");
        drop(state);
        std::fs::remove_dir_all(root).unwrap();
    }
    #[test]
    fn scheduler_groups_tasks_and_persists_failed_attempts() {
        use super::super::{
            peer_tests::{register, test_server},
            state::TaskRec,
        };
        let (server, root) = test_server();
        // This regression preserves the immediate legacy keepalive contract;
        // batch behavior is covered by the timer notification tests.
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
        assert!(state.keepalives["worker"].suspected_offline);
        assert_eq!(state.msgs.len(), 3);
        assert!(state.msgs.values().all(|m| m.wake_attempt_count == 1));
        assert!(state
            .wake_bindings
            .keys()
            .all(|id| state.msgs.get(id).is_some_and(|m| m.state == "pending")));
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

        // Once a state holds past the settle window it is reported exactly once.
        tick_with(
            &server,
            now + 200_000,
            &|_| AgentState::Waiting,
            &wake,
            &|_, _| Ok(true),
        );
        assert_eq!(status_count(&server), 1);
        tick_with(
            &server,
            now + 400_000,
            &|_| AgentState::Waiting,
            &wake,
            &|_, _| Ok(true),
        );
        assert_eq!(status_count(&server), 1, "settled state reports once");

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
