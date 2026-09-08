use crate::server::state::{now_ms, Event, Message, MAX_WAKE_ATTEMPTS};
use crate::server::Server;
use std::collections::HashMap;
use std::sync::Arc;

const WAKE_ATTEMPT_LEASE_MS: i64 = 10_000;

/// Server-side scheduler for finite subscriptions and bounded waits. It never
/// creates task continuations or infers that ordinary work needs a wake.
pub fn tick(server: &Arc<Server>) {
    super::keepalive::tick(server);
    super::purge_expired_storage(server, now_ms());
    tick_with_idle(server, &|_| true);
}

fn tick_with_idle(server: &Arc<Server>, _can_receive: &dyn Fn(&str) -> bool) {
    if server.state.lock().unwrap().admission_frozen() {
        return;
    }
    let now = now_ms();
    let checks: Vec<(String, String, String, Option<String>)> = {
        let state = server.state.lock().unwrap();
        state
            .notification_subscriptions
            .values()
            .filter(|s| s.status == "armed" && s.expires_ms > now)
            .map(|s| {
                (
                    s.id.clone(),
                    s.worker_id.clone(),
                    s.pane.clone(),
                    state.workers.get(&s.worker_id).and_then(|w| w.pane.clone()),
                )
            })
            .collect()
    };
    let mut lost_sub_ids = Vec::new();
    let mut unknown_sub_ids = Vec::new();
    for (id, worker_id, pane, current_worker_pane) in checks {
        if current_worker_pane.as_deref() != Some(&pane) {
            lost_sub_ids.push(id);
            continue;
        }
        let presence = (server.pane_alive_check)(&pane);
        if presence == super::knock::PanePresence::Unknown {
            unknown_sub_ids.push(id);
            continue;
        }
        let owned = if presence == super::knock::PanePresence::Present {
            match (server.pane_owner_check)(&worker_id, &pane) {
                Ok(owned) => owned,
                Err(()) => {
                    unknown_sub_ids.push(id);
                    continue;
                }
            }
        } else {
            false
        };
        let agent = if presence == super::knock::PanePresence::Present {
            (server.pane_state_check)(&pane)
        } else {
            super::knock::AgentState::Absent
        };
        if presence == super::knock::PanePresence::Missing
            || !owned
            || agent == crate::server::knock::AgentState::Absent
        {
            lost_sub_ids.push(id);
        } else if agent == super::knock::AgentState::Unknown {
            unknown_sub_ids.push(id);
        }
    }

    let mut lifecycle_events = Vec::new();
    {
        let state = server.state.lock().unwrap();
        for subscription in state.notification_subscriptions.values() {
            if subscription.status == "armed" {
                if subscription.expires_ms <= now {
                    lifecycle_events.push(Event::NotificationStatus {
                        subscription_id: subscription.id.clone(),
                        status: "expired".into(),
                        updated_ms: now,
                    });
                } else if lost_sub_ids.contains(&subscription.id) {
                    lifecycle_events.push(Event::NotificationStatus {
                        subscription_id: subscription.id.clone(),
                        status: "pane-lost".into(),
                        updated_ms: now,
                    });
                }
            }
        }
        let live_master = super::live_master_id(server, &state);
        for task in state.tasks.values() {
            let Some(wait) = task.wait.as_ref() else {
                continue;
            };
            if task.status != "waiting" || wait.deadline_ms > now {
                continue;
            }
            let mut expired = task.clone();
            expired.status = "blocked".into();
            expired.next_step = Some(format!(
                "WAIT_TIMEOUT waiting_for={} responsible_actor={} reason={} escalation={}",
                wait.waiting_for, wait.responsible_actor, wait.reason, wait.escalation
            ));
            expired.wait = None;
            expired.updated_ms = now;
            lifecycle_events.push(Event::TaskUpdated { task: expired });

            if let Some(master_id) = live_master.as_ref() {
                let message_id = super::gen_msg_id();
                lifecycle_events.push(Event::Sent {
                    msg: Message {
                        id: message_id.clone(),
                        from: "collab-server".into(),
                        to: master_id.clone(),
                        mtype: "notify".into(),
                        subject: Some(format!("wait-timeout:{}", task.id)),
                        body: format!(
                            "Task {} is blocked after WAIT_TIMEOUT: waiting_for={} responsible_actor={} reason={} escalation={} resume_on={}. Inspect the blocker and task graph, then resolve or reassign it.",
                            task.id,
                            wait.waiting_for,
                            wait.responsible_actor,
                            wait.reason,
                            wait.escalation,
                            wait.resume_on.join(",")
                        ),
                        in_reply_to: None,
                        created_ms: now,
                        state: "pending".into(),
                        wake_attempt_count: 0,
                        last_wake_attempt_ms: 0,
                    },
                });
                if let Some(subscription) =
                    state.matching_subscription(master_id, "direct-message", None, now)
                {
                    lifecycle_events.push(Event::WakeBound {
                        message_id,
                        subscription_id: subscription.id.clone(),
                    });
                }
            }
        }
    }
    if !lifecycle_events.is_empty() {
        server.commit(&lifecycle_events);
    }

    let mut due_events = Vec::new();
    {
        let state = server.state.lock().unwrap();
        let mut idle_gate_records = HashMap::new();
        for subscription in state.notification_subscriptions.values() {
            let next_trigger = subscription
                .interval_ms
                .map(|interval| {
                    subscription
                        .trigger_ms
                        .unwrap_or(subscription.created_ms.saturating_add(interval))
                })
                .or_else(|| {
                    subscription
                        .trigger_times_ms
                        .get(subscription.fired_count as usize)
                        .copied()
                })
                .or(subscription.trigger_ms);
            let master_idle_gate_open = if subscription.event == "master-idle" {
                match state.keepalives.get(&subscription.worker_id) {
                    None => false,
                    Some(record) => {
                        let record = idle_gate_records
                            .entry(subscription.worker_id.clone())
                            .or_insert_with(|| record.clone());
                        let pending_keepalive_notice = state.msgs.values().any(|message| {
                            message.to == subscription.worker_id
                                && message.state == "pending"
                                && message
                                    .subject
                                    .as_deref()
                                    .is_some_and(|subject| subject.starts_with("master-idle"))
                        });
                        record.idle_episode_notices < 3
                            && !record.idle_episode_stopped
                            && record.idle_since_ms > subscription.created_ms
                            && record.last_notice_ms != now
                            && !pending_keepalive_notice
                    }
                }
            } else {
                true
            };
            let master_idle_ready = if subscription.event == "master-idle" {
                matches!(super::live_master_id(server, &state), Ok(Some(id)) if id == subscription.worker_id)
                    && state
                        .keepalives
                        .get(&subscription.worker_id)
                        .is_some_and(|record| record.observed == "idle" && record.idle_since_ms > 0)
                    && (server.pane_state_check)(&subscription.pane)
                        == crate::server::knock::AgentState::Waiting
                    && !state.tasks.values().any(|task| {
                        task.owner == subscription.worker_id
                            && super::keepalive::actionable(&task.status)
                    })
            } else {
                true
            };
            if subscription.status != "armed"
                || unknown_sub_ids.contains(&subscription.id)
                || !server.config.timers.enabled
                || !matches!(subscription.event.as_str(), "deadline" | "master-idle")
                || !master_idle_ready
                || !master_idle_gate_open
                || next_trigger.is_none_or(|trigger| trigger > now)
                || state.wake_bindings.iter().any(|(message_id, bound)| {
                    bound == &subscription.id
                        && state
                            .msgs
                            .get(message_id)
                            .is_some_and(|message| message.state == "pending")
                })
            {
                continue;
            }
            let message_id = super::gen_msg_id();
            if subscription.event == "master-idle" {
                let record = idle_gate_records
                    .get_mut(&subscription.worker_id)
                    .expect("master-idle gate record");
                record.idle_episode_notices = record.idle_episode_notices.saturating_add(1);
                record.last_notice_ms = now;
                due_events.push(Event::KeepaliveUpdated {
                    worker_id: subscription.worker_id.clone(),
                    record: record.clone(),
                });
            }
            due_events.extend([
                Event::Sent {
                    msg: Message {
                        id: message_id.clone(),
                        from: "collab-server".into(),
                        to: subscription.worker_id.clone(),
                        mtype: "notification".into(),
                        subject: subscription.subject.as_ref().map(|subject| {
                            if subscription.event == "master-idle" {
                                format!("master-idle:{subject}")
                            } else {
                                format!("deadline:{subject}")
                            }
                        }),
                        body: if subscription.event == "master-idle" {
                            format!("MASTER_IDLE_WAKE subject={} scheduling continues; inspect actionable tasks and authorized open bugs. Cancel this subscription only iff no actionable task, dependency, resolvable blocker, or authorized open bug remains: collab notify unsubscribe {}", subscription.subject.as_deref().unwrap_or_default(), subscription.id)
                        } else {
                            format!("DEADLINE_REACHED subject={}{}", subscription.subject.as_deref().unwrap_or_default(), if subscription.fired_count + 1 >= if subscription.interval_ms.is_some() { subscription.repeat_count } else { subscription.trigger_times_ms.len().max(1) as u32 } { "; LAST_REMINDER=true; renew explicitly: collab notify subscribe --event deadline --subject <subject> --at-ms <future-epoch-ms> --ttl-seconds <bounded>" } else { "" })
                        },
                        in_reply_to: None,
                        created_ms: now,
                        state: "pending".into(),
                        wake_attempt_count: 0,
                        last_wake_attempt_ms: 0,
                    },
                },
                Event::WakeBound {
                    message_id: message_id.clone(),
                    subscription_id: subscription.id.clone(),
                },
            ]);
        }
    }
    if !due_events.is_empty() {
        server.commit(&due_events);
    }

    let candidates: Vec<(String, String)> = {
        let state = server.state.lock().unwrap();
        state
            .wake_bindings
            .iter()
            .filter_map(|(message_id, subscription_id)| {
                let message = state.msgs.get(message_id)?;
                let subscription = state.notification_subscriptions.get(subscription_id)?;
                (message.state == "pending"
                    && !unknown_sub_ids.contains(subscription_id)
                    && message.wake_attempt_count < MAX_WAKE_ATTEMPTS
                    && now - message.last_wake_attempt_ms >= WAKE_ATTEMPT_LEASE_MS
                    && subscription.status == "armed"
                    && subscription.expires_ms > now)
                    .then(|| (message_id.clone(), subscription_id.clone()))
            })
            .collect()
    };
    for (message_id, subscription_id) in candidates {
        super::attempt_notification_with(
            server,
            &message_id,
            &subscription_id,
            &|pane| (server.pane_state_check)(pane) == super::knock::AgentState::Waiting,
            &|pane, text| super::knock_or_log(&server.log_path(), pane, text),
            &|worker_id, pane| (server.pane_owner_check)(worker_id, pane),
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::server::keepalive::Record;
    use crate::server::state::{
        Event, NotificationSubscription, State, TaskRec, WaitSpec, WorkerRec,
    };
    use std::sync::Mutex;

    fn test_server() -> (Arc<Server>, std::path::PathBuf) {
        static SEQ: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
        let sequence = SEQ.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        let root = std::env::temp_dir().join(format!(
            "collab-notification-timer-{}-{sequence}",
            std::process::id()
        ));
        let server_dir = root.join(".agent-collab/server");
        std::fs::create_dir_all(&server_dir).unwrap();
        let journal = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(server_dir.join("journal.jsonl"))
            .unwrap();
        (
            Arc::new(Server {
                config: crate::config::Config::default(),
                root: root.clone(),
                state: Mutex::new(State::default()),
                journal: Mutex::new(journal),
                pane_alive_check: |_| super::super::knock::PanePresence::Present,
                pane_owner_check: |_, _| Ok(true),
                pane_state_check: |_| crate::server::knock::AgentState::Waiting,
                mailbox_notify: tokio::sync::Notify::new(),
            }),
            root,
        )
    }

    fn register(server: &Server, worker_id: &str) {
        server.commit(&[Event::Registered {
            worker: WorkerRec {
                id: worker_id.into(),
                token: format!("token-{worker_id}"),
                pane: Some(format!("%test-{worker_id}")),
                cwd: "/tmp".into(),
                registered_ms: now_ms(),
            },
        }]);
    }

    fn register_master(server: &Server) {
        register(server, "master");
        let now = now_ms();
        server.commit(&[
            Event::MasterAssigned {
                worker_id: "master".into(),
                assigned_by: "operator".into(),
                approval: Some("user-approved".into()),
                assigned_ms: now,
            },
            Event::KeepaliveUpdated {
                worker_id: "master".into(),
                record: Record {
                    observed: "idle".into(),
                    idle_since_ms: now - 900_001,
                    ..Record::default()
                },
            },
        ]);
    }

    fn master_idle_subscription(server: &Server, interval_ms: i64) -> String {
        let now = now_ms();
        let id = format!("sub-master-idle-{interval_ms}");
        server.commit(&[Event::NotificationSubscribed {
            subscription: NotificationSubscription {
                id: id.clone(),
                worker_id: "master".into(),
                event: "master-idle".into(),
                subject: Some("master-idle".into()),
                pane: "%test-master".into(),
                method: "tmux".into(),
                trigger_ms: Some(now - 1),
                trigger_times_ms: Vec::new(),
                interval_ms: Some(interval_ms),
                repeat_count: 3,
                fired_count: 0,
                expires_ms: now + 86_400_000,
                status: "armed".into(),
                created_ms: now - interval_ms,
                updated_ms: now,
            },
        }]);
        id
    }

    fn subscribe(
        server: &Server,
        worker_id: &str,
        event: &str,
        subject: Option<&str>,
        trigger_ms: Option<i64>,
    ) -> String {
        let id = format!("sub-{worker_id}-{event}");
        server.commit(&[Event::NotificationSubscribed {
            subscription: NotificationSubscription {
                id: id.clone(),
                worker_id: worker_id.into(),
                event: event.into(),
                subject: subject.map(str::to_owned),
                pane: format!("%test-{worker_id}"),
                method: "tmux".into(),
                trigger_ms,
                trigger_times_ms: Vec::new(),
                interval_ms: None,
                repeat_count: 1,
                fired_count: 0,
                expires_ms: now_ms() + 60_000,
                status: "armed".into(),
                created_ms: now_ms(),
                updated_ms: now_ms(),
            },
        }]);
        id
    }

    fn bind_message(server: &Server, worker_id: &str, subscription_id: &str) -> String {
        bind_message_with_id(
            server,
            worker_id,
            subscription_id,
            &format!("message-{worker_id}"),
        )
    }

    fn bind_message_with_id(
        server: &Server,
        worker_id: &str,
        subscription_id: &str,
        message_id: &str,
    ) -> String {
        server.commit(&[
            Event::Sent {
                msg: Message {
                    id: message_id.to_string(),
                    from: "peer".into(),
                    to: worker_id.into(),
                    mtype: "notify".into(),
                    subject: Some("released:held".into()),
                    body: "RESOURCE_RELEASED task=held".into(),
                    in_reply_to: None,
                    created_ms: now_ms() - 120_001,
                    state: "pending".into(),
                    wake_attempt_count: 0,
                    last_wake_attempt_ms: 0,
                },
            },
            Event::WakeBound {
                message_id: message_id.to_string(),
                subscription_id: subscription_id.into(),
            },
        ]);
        message_id.to_string()
    }

    fn working_task(server: &Server, worker_id: &str) {
        let now = now_ms();
        server.commit(&[Event::TaskCreated {
            task: TaskRec {
                id: "task".into(),
                owner: worker_id.into(),
                created_by: worker_id.into(),
                feature_id: Some("feature".into()),
                worktree_path: None,
                branch: None,
                base_commit: None,
                priority: "p2".into(),
                status: "working".into(),
                next_step: Some("keep working".into()),
                wait: None,
                created_ms: now,
                updated_ms: now,
            },
        }]);
    }

    #[test]
    fn ordinary_work_never_generates_periodic_continuation() {
        let (server, root) = test_server();
        register(&server, "owner");
        working_task(&server, "owner");
        tick_with_idle(&server, &|_| true);
        assert!(server.state.lock().unwrap().msgs.is_empty());
        std::fs::remove_dir_all(root).ok();
    }

    #[test]
    fn configured_immediate_and_disabled_delivery_are_respected() {
        for (mode, enabled, expected) in [
            ("immediate", true, true),
            ("batch", true, false),
            ("immediate", false, false),
        ] {
            let (mut server, root) = test_server();
            let config = &mut Arc::get_mut(&mut server).unwrap().config;
            config.notifications.mode = mode.into();
            config.notifications.enabled = enabled;
            register(&server, "owner");
            let sub = subscribe(&server, "owner", "direct-message", None, None);
            let id = bind_message(&server, "owner", &sub);
            server
                .state
                .lock()
                .unwrap()
                .msgs
                .get_mut(&id)
                .unwrap()
                .created_ms = now_ms();
            assert_eq!(
                super::super::attempt_notification_with_default(
                    &server,
                    &id,
                    &sub,
                    &|_| true,
                    &|_, _| { true }
                ),
                expected
            );
            assert_eq!(
                server.state.lock().unwrap().msgs[&id].wake_attempt_count,
                if expected { 1 } else { 0 }
            );
            std::fs::remove_dir_all(root).unwrap();
        }
    }

    #[test]
    fn no_subscription_means_zero_wake_attempts() {
        let (server, root) = test_server();
        register(&server, "owner");
        server.commit(&[Event::Sent {
            msg: Message {
                id: "message".into(),
                from: "peer".into(),
                to: "owner".into(),
                mtype: "notify".into(),
                subject: Some("released:held".into()),
                body: "RESOURCE_RELEASED task=held".into(),
                in_reply_to: None,
                created_ms: now_ms(),
                state: "pending".into(),
                wake_attempt_count: 0,
                last_wake_attempt_ms: 0,
            },
        }]);
        tick_with_idle(&server, &|_| true);
        assert_eq!(
            server.state.lock().unwrap().msgs["message"].wake_attempt_count,
            0
        );
        std::fs::remove_dir_all(root).ok();
    }

    #[test]
    fn failed_one_shot_notification_has_one_attempt_lifetime_cap() {
        let (server, root) = test_server();
        register(&server, "owner");
        let subscription_id = subscribe(&server, "owner", "resource-released", Some("held"), None);
        let message_id = bind_message(&server, "owner", &subscription_id);
        for _ in 0..4 {
            super::super::attempt_notification_with_default(
                &server,
                &message_id,
                &subscription_id,
                &|_| true,
                &|_, _| false,
            );
        }
        let state = server.state.lock().unwrap();
        assert_eq!(
            state.msgs[&message_id].wake_attempt_count,
            MAX_WAKE_ATTEMPTS
        );
        assert_eq!(
            state.notification_subscriptions[&subscription_id].status,
            "armed"
        );
        drop(state);
        std::fs::remove_dir_all(root).ok();
    }

    #[test]
    fn failed_direct_message_exhausts_only_the_message() {
        let (server, root) = test_server();
        register(&server, "owner");
        let subscription_id = subscribe(&server, "owner", "direct-message", None, None);
        let message_id = bind_message(&server, "owner", &subscription_id);
        for _ in 0..MAX_WAKE_ATTEMPTS {
            super::super::attempt_notification_with_default(
                &server,
                &message_id,
                &subscription_id,
                &|_| true,
                &|_, _| false,
            );
        }
        let state = server.state.lock().unwrap();
        assert_eq!(
            state.msgs[&message_id].wake_attempt_count,
            MAX_WAKE_ATTEMPTS
        );
        assert_eq!(
            state.notification_subscriptions[&subscription_id].status,
            "armed"
        );
        drop(state);
        std::fs::remove_dir_all(root).ok();
    }

    #[test]
    fn restart_does_not_reset_exhausted_direct_message_attempts() {
        let (server, root) = test_server();
        register(&server, "owner");
        let subscription_id = subscribe(&server, "owner", "direct-message", None, None);
        let message_id = bind_message(&server, "owner", &subscription_id);
        for _ in 0..MAX_WAKE_ATTEMPTS {
            super::super::attempt_notification_with_default(
                &server,
                &message_id,
                &subscription_id,
                &|_| true,
                &|_, _| false,
            );
        }
        drop(server);

        let replayed = super::super::replay(&root).unwrap();
        let journal = std::fs::OpenOptions::new()
            .append(true)
            .open(root.join(".agent-collab/server/journal.jsonl"))
            .unwrap();
        let restarted = Server {
            config: crate::config::Config::default(),
            root: root.clone(),
            state: Mutex::new(replayed),
            journal: Mutex::new(journal),
            pane_alive_check: |_| super::super::knock::PanePresence::Present,
            pane_owner_check: |_, _| Ok(true),
            pane_state_check: |_| crate::server::knock::AgentState::Waiting,
            mailbox_notify: tokio::sync::Notify::new(),
        };
        let sent = std::sync::atomic::AtomicBool::new(false);
        assert!(!super::super::attempt_notification_with_default(
            &restarted,
            &message_id,
            &subscription_id,
            &|_| true,
            &|_, _| {
                sent.store(true, std::sync::atomic::Ordering::Relaxed);
                true
            },
        ));
        let state = restarted.state.lock().unwrap();
        assert!(!sent.load(std::sync::atomic::Ordering::Relaxed));
        assert_eq!(
            state.msgs[&message_id].wake_attempt_count,
            MAX_WAKE_ATTEMPTS
        );
        assert_eq!(
            state.notification_subscriptions[&subscription_id].status,
            "armed"
        );
        drop(state);
        std::fs::remove_dir_all(root).ok();
    }

    #[test]
    fn successful_event_notification_consumes_one_shot_subscription() {
        let (server, root) = test_server();
        register(&server, "owner");
        let subscription_id = subscribe(&server, "owner", "resource-released", Some("held"), None);
        let message_id = bind_message(&server, "owner", &subscription_id);
        assert!(super::super::attempt_notification_with_default(
            &server,
            &message_id,
            &subscription_id,
            &|_| true,
            &|_, _| true,
        ));
        let state = server.state.lock().unwrap();
        assert_eq!(state.msgs[&message_id].state, "delivered");
        assert_eq!(
            state.notification_subscriptions[&subscription_id].status,
            "consumed"
        );
        drop(state);
        std::fs::remove_dir_all(root).ok();
    }

    #[test]
    fn successful_direct_messages_reuse_subscription_without_a_burst() {
        let (server, root) = test_server();
        register(&server, "owner");
        let subscription_id = subscribe(&server, "owner", "direct-message", None, None);
        let first_id = bind_message(&server, "owner", &subscription_id);
        assert!(super::super::attempt_notification_with_default(
            &server,
            &first_id,
            &subscription_id,
            &|_| true,
            &|_, _| true,
        ));

        let second_id = "message-owner-second".to_string();
        server.commit(&[
            Event::Sent {
                msg: Message {
                    id: second_id.clone(),
                    from: "peer".into(),
                    to: "owner".into(),
                    mtype: "notify".into(),
                    subject: Some("second".into()),
                    body: "SECOND_NOTICE".into(),
                    in_reply_to: None,
                    created_ms: now_ms(),
                    state: "pending".into(),
                    wake_attempt_count: 0,
                    last_wake_attempt_ms: 0,
                },
            },
            Event::WakeBound {
                message_id: second_id.clone(),
                subscription_id: subscription_id.clone(),
            },
        ]);
        assert!(!super::super::attempt_notification_with_default(
            &server,
            &second_id,
            &subscription_id,
            &|_| true,
            &|_, _| true,
        ));
        assert_eq!(
            server.state.lock().unwrap().msgs[&second_id].wake_attempt_count,
            0
        );

        server.commit(&[Event::NotificationStatus {
            subscription_id: subscription_id.clone(),
            status: "armed".into(),
            updated_ms: now_ms() - super::super::DIRECT_MESSAGE_WAKE_COOLDOWN_MS - 1,
        }]);
        {
            let mut state = server.state.lock().unwrap();
            state.msgs.get_mut(&second_id).unwrap().created_ms = now_ms() - 120_001;
            state.msgs.get_mut(&first_id).unwrap().last_wake_attempt_ms = now_ms() - 120_001;
        }
        assert!(super::super::attempt_notification_with_default(
            &server,
            &second_id,
            &subscription_id,
            &|_| true,
            &|_, _| true,
        ));
        let state = server.state.lock().unwrap();
        assert_eq!(state.msgs[&first_id].state, "delivered");
        assert_eq!(state.msgs[&second_id].state, "delivered");
        assert_eq!(
            state.notification_subscriptions[&subscription_id].status,
            "armed"
        );
        drop(state);
        std::fs::remove_dir_all(root).ok();
    }

    #[test]
    fn batch_includes_newer_pending_messages_and_never_replays() {
        let (server, root) = test_server();
        register(&server, "owner");
        let subscription = subscribe(&server, "owner", "direct-message", None, None);
        let first = bind_message(&server, "owner", &subscription);
        let mut second = server.state.lock().unwrap().msgs[&first].clone();
        second.id = "second".into();
        second.subject = Some("new topic".into());
        second.created_ms = now_ms();
        server.commit(&[
            Event::Sent { msg: second },
            Event::WakeBound {
                message_id: "second".into(),
                subscription_id: subscription.clone(),
            },
        ]);
        let calls = std::cell::RefCell::new(Vec::new());
        assert!(super::super::attempt_notification_with_default(
            &server,
            &first,
            &subscription,
            &|_| true,
            &|_, text| {
                calls.borrow_mut().push(text.to_string());
                true
            }
        ));
        assert_eq!(calls.borrow().len(), 1);
        assert!(calls.borrow()[0].contains(&first));
        assert!(calls.borrow()[0].contains("message_ids=message-owner,second"));
        assert!(calls.borrow()[0].contains("action_categories="));
        assert!(calls.borrow()[0].contains("new topic"));
        assert_eq!(
            server.state.lock().unwrap().msgs["second"].state,
            "delivered"
        );
        assert!(!super::super::attempt_notification_with_default(
            &server,
            &first,
            &subscription,
            &|_| true,
            &|_, _| panic!("duplicate delivery")
        ));
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn explicit_delivery_mode_bypasses_batch_window() {
        let (server, root) = test_server();
        register(&server, "owner");
        let sub = subscribe(&server, "owner", "direct-message", None, None);
        let id = bind_message(&server, "owner", &sub);
        server.commit(&[Event::DeliveryMode {
            msg_id: id.clone(),
            mode: "explicit-notification".into(),
        }]);
        let calls = std::cell::Cell::new(0);
        assert!(super::super::attempt_notification_with_default(
            &server,
            &id,
            &sub,
            &|_| true,
            &|_, _| {
                calls.set(calls.get() + 1);
                true
            }
        ));
        assert_eq!(calls.get(), 1);
        assert_eq!(server.state.lock().unwrap().msgs[&id].state, "delivered");
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn fresh_batch_waits_two_minutes_and_absent_batch_is_not_replayed() {
        let (server, root) = test_server();
        register(&server, "owner");
        let sub = subscribe(&server, "owner", "direct-message", None, None);
        let id = bind_message(&server, "owner", &sub);
        server
            .state
            .lock()
            .unwrap()
            .msgs
            .get_mut(&id)
            .unwrap()
            .created_ms = now_ms();
        assert!(!super::super::attempt_notification_with_default(
            &server,
            &id,
            &sub,
            &|_| true,
            &|_, _| panic!("early delivery")
        ));
        server
            .state
            .lock()
            .unwrap()
            .msgs
            .get_mut(&id)
            .unwrap()
            .created_ms -= 120_001;
        assert!(!super::super::attempt_notification_with_default(
            &server,
            &id,
            &sub,
            &|_| false,
            &|_, _| panic!("absent delivery")
        ));
        assert!(!super::super::attempt_notification_with_default(
            &server,
            &id,
            &sub,
            &|_| true,
            &|_, _| panic!("late replay")
        ));
        assert_eq!(server.state.lock().unwrap().msgs[&id].wake_attempt_count, 1);
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn deadline_subscription_emits_once_only_after_trigger() {
        let (server, root) = test_server();
        register(&server, "owner");
        let subscription_id = subscribe(
            &server,
            "owner",
            "deadline",
            Some("timer"),
            Some(now_ms() - 1),
        );
        tick_with_idle(&server, &|_| false);
        tick_with_idle(&server, &|_| false);
        let state = server.state.lock().unwrap();
        assert_eq!(
            state
                .wake_bindings
                .values()
                .filter(|bound| *bound == &subscription_id)
                .count(),
            1
        );
        drop(state);
        std::fs::remove_dir_all(root).ok();
    }

    #[test]
    fn master_idle_subscription_emits_at_15_minutes_only_for_live_idle_master() {
        let (server, root) = test_server();
        register_master(&server);
        let subscription_id = master_idle_subscription(&server, 15 * 60 * 1000);
        let created_ms =
            server.state.lock().unwrap().notification_subscriptions[&subscription_id].created_ms;
        let mut record = server.state.lock().unwrap().keepalives["master"].clone();
        record.idle_since_ms = created_ms + 1;
        server.commit(&[Event::KeepaliveUpdated {
            worker_id: "master".into(),
            record,
        }]);

        tick_with_idle(&server, &|_| false);
        tick_with_idle(&server, &|_| false);

        let state = server.state.lock().unwrap();
        assert_eq!(
            state
                .wake_bindings
                .values()
                .filter(|bound| *bound == &subscription_id)
                .count(),
            1
        );
        let message_id = state
            .wake_bindings
            .iter()
            .find_map(|(message_id, bound)| (bound == &subscription_id).then_some(message_id))
            .unwrap();
        assert_eq!(state.msgs[message_id].to, "master");
        assert!(state.msgs[message_id].body.contains("scheduling"));
        drop(state);
        std::fs::remove_dir_all(root).ok();
    }

    #[test]
    fn master_idle_subscription_accepts_60_minute_interval_and_wakes_master() {
        let (server, root) = test_server();
        register_master(&server);
        let subscription_id = master_idle_subscription(&server, 60 * 60 * 1000);
        let created_ms =
            server.state.lock().unwrap().notification_subscriptions[&subscription_id].created_ms;
        let mut record = server.state.lock().unwrap().keepalives["master"].clone();
        record.idle_since_ms = created_ms + 1;
        server.commit(&[Event::KeepaliveUpdated {
            worker_id: "master".into(),
            record,
        }]);

        tick_with_idle(&server, &|_| false);
        let state = server.state.lock().unwrap();
        let master_wakes = state
            .wake_bindings
            .values()
            .filter(|bound| *bound == &subscription_id)
            .count();
        assert_eq!(master_wakes, 1, "60-minute master wake must be non-vacuous");
        assert!(state
            .wake_bindings
            .values()
            .all(|bound| bound == &subscription_id));
        assert!(state.msgs.values().all(|message| message.to == "master"));
        drop(state);
        std::fs::remove_dir_all(root).ok();
    }

    #[test]
    fn master_idle_wake_waits_for_actionable_tasks_to_clear() {
        let (server, root) = test_server();
        register_master(&server);
        let subscription_id = master_idle_subscription(&server, 15 * 60 * 1000);
        working_task(&server, "master");

        tick_with_idle(&server, &|_| false);

        let state = server.state.lock().unwrap();
        assert!(state
            .wake_bindings
            .values()
            .all(|bound| bound != &subscription_id));
        assert!(state.msgs.is_empty());
        drop(state);
        std::fs::remove_dir_all(root).ok();
    }

    #[test]
    fn master_idle_wake_requires_observed_idle_state() {
        let (server, root) = test_server();
        register_master(&server);
        let subscription_id = master_idle_subscription(&server, 15 * 60 * 1000);

        server.commit(&[Event::KeepaliveUpdated {
            worker_id: "master".into(),
            record: Record {
                observed: "working".into(),
                idle_since_ms: 0,
                ..Record::default()
            },
        }]);
        tick_with_idle(&server, &|_| false);

        let state = server.state.lock().unwrap();
        assert!(state
            .wake_bindings
            .values()
            .all(|bound| bound != &subscription_id));
        assert!(state.msgs.is_empty());
        drop(state);
        std::fs::remove_dir_all(root).ok();
    }

    #[test]
    fn recurring_master_idle_wake_advances_after_consumed_binding() {
        let (server, root) = test_server();
        register_master(&server);
        let subscription_id = master_idle_subscription(&server, 15 * 60 * 1000);
        let now = now_ms();
        server
            .state
            .lock()
            .unwrap()
            .notification_subscriptions
            .get_mut(&subscription_id)
            .unwrap()
            .trigger_ms = Some(now - 15 * 60 * 1000 - 1);
        let mut record = server.state.lock().unwrap().keepalives["master"].clone();
        record.idle_since_ms = now.saturating_add(1);
        server.commit(&[Event::KeepaliveUpdated {
            worker_id: "master".into(),
            record,
        }]);

        tick_with_idle(&server, &|_| false);
        let first_message_id = server
            .state
            .lock()
            .unwrap()
            .wake_bindings
            .iter()
            .find_map(|(message_id, bound)| {
                (bound == &subscription_id).then_some(message_id.clone())
            })
            .expect("first master idle wake");
        server.commit(&[
            Event::Delivered {
                ids: vec![first_message_id.clone()],
            },
            Event::NotificationConsumed {
                subscription_id: subscription_id.clone(),
                message_id: first_message_id,
                consumed_ms: now_ms(),
            },
        ]);

        tick_with_idle(&server, &|_| false);
        let state = server.state.lock().unwrap();
        assert_eq!(
            state
                .wake_bindings
                .values()
                .filter(|bound| *bound == &subscription_id)
                .count(),
            2
        );
        drop(state);
        std::fs::remove_dir_all(root).ok();
    }

    #[test]
    fn master_idle_timer_respects_keepalive_episode_notice_budget() {
        let (server, root) = test_server();
        register_master(&server);
        let subscription_id = master_idle_subscription(&server, 15 * 60 * 1000);
        let mut record = server.state.lock().unwrap().keepalives["master"].clone();
        record.idle_episode_notices = 3;
        server.commit(&[Event::KeepaliveUpdated {
            worker_id: "master".into(),
            record,
        }]);

        tick_with_idle(&server, &|_| false);

        let state = server.state.lock().unwrap();
        assert!(
            state.msgs.is_empty(),
            "three keepalive notices stop timer wakes"
        );
        assert_eq!(
            state.notification_subscriptions[&subscription_id].fired_count,
            0
        );
        assert_eq!(state.keepalives["master"].idle_episode_notices, 3);
        drop(state);
        std::fs::remove_dir_all(root).ok();
    }

    #[test]
    fn master_idle_timer_counts_its_wake_in_keepalive_episode_gate() {
        let (server, root) = test_server();
        register_master(&server);
        let subscription_id = master_idle_subscription(&server, 15 * 60 * 1000);
        let created_ms =
            server.state.lock().unwrap().notification_subscriptions[&subscription_id].created_ms;
        let mut record = server.state.lock().unwrap().keepalives["master"].clone();
        record.idle_since_ms = created_ms + 1;
        server.commit(&[Event::KeepaliveUpdated {
            worker_id: "master".into(),
            record,
        }]);

        tick_with_idle(&server, &|_| false);

        let state = server.state.lock().unwrap();
        assert_eq!(state.msgs.len(), 1);
        assert_eq!(state.keepalives["master"].idle_episode_notices, 1);
        assert_eq!(
            state
                .wake_bindings
                .values()
                .filter(|bound| *bound == &subscription_id)
                .count(),
            1
        );
        drop(state);
        std::fs::remove_dir_all(root).ok();
    }

    #[test]
    fn master_idle_timer_stops_after_keepalive_working_to_waiting() {
        let (server, root) = test_server();
        register_master(&server);
        let subscription_id = master_idle_subscription(&server, 15 * 60 * 1000);
        let now = now_ms();
        let created_ms =
            server.state.lock().unwrap().notification_subscriptions[&subscription_id].created_ms;
        server
            .state
            .lock()
            .unwrap()
            .notification_subscriptions
            .get_mut(&subscription_id)
            .unwrap()
            .trigger_ms = Some(now - 15 * 60 * 1000 - 1);
        let mut record = server.state.lock().unwrap().keepalives["master"].clone();
        record.idle_since_ms = created_ms + 1;
        server.commit(&[Event::KeepaliveUpdated {
            worker_id: "master".into(),
            record,
        }]);

        tick_with_idle(&server, &|_| false);
        let first_message_id = server
            .state
            .lock()
            .unwrap()
            .wake_bindings
            .iter()
            .find_map(|(message_id, bound)| {
                (bound == &subscription_id).then_some(message_id.clone())
            })
            .expect("first master idle timer wake");
        server.commit(&[
            Event::Delivered {
                ids: vec![first_message_id.clone()],
            },
            Event::NotificationConsumed {
                subscription_id: subscription_id.clone(),
                message_id: first_message_id,
                consumed_ms: now_ms(),
            },
        ]);

        let working_at = now_ms();
        super::super::keepalive::tick_with(
            &server,
            working_at,
            &|_| crate::server::knock::AgentState::Working,
            &|_, _| true,
            &|_, _| true,
        );
        super::super::keepalive::tick_with(
            &server,
            working_at + 1,
            &|_| crate::server::knock::AgentState::Waiting,
            &|_, _| true,
            &|_, _| true,
        );
        tick_with_idle(&server, &|_| false);

        let state = server.state.lock().unwrap();
        assert_eq!(
            state
                .wake_bindings
                .values()
                .filter(|bound| *bound == &subscription_id)
                .count(),
            1,
            "Working -> Waiting must not reopen the consumed timer occurrence"
        );
        assert_eq!(state.msgs.len(), 1);
        assert_eq!(state.keepalives["master"].idle_episode_notices, 1);
        assert!(state.keepalives["master"].idle_episode_stopped);
        assert_eq!(
            state.notification_subscriptions[&subscription_id].fired_count,
            1
        );
        drop(state);
        std::fs::remove_dir_all(root).ok();
    }

    #[test]
    fn master_idle_timer_and_keepalive_share_episode_notice_budget() {
        let (server, root) = test_server();
        register_master(&server);
        let timer_subscription_id = master_idle_subscription(&server, 15 * 60 * 1000);
        let direct_subscription_id = subscribe(&server, "master", "direct-message", None, None);
        let now = now_ms();
        let mut direct_subscription = server.state.lock().unwrap().notification_subscriptions
            [&direct_subscription_id]
            .clone();
        direct_subscription.expires_ms = now + 86_400_000;
        server.commit(&[Event::NotificationSubscribed {
            subscription: direct_subscription,
        }]);
        server
            .state
            .lock()
            .unwrap()
            .notification_subscriptions
            .get_mut(&timer_subscription_id)
            .unwrap()
            .trigger_ms = Some(now - 15 * 60 * 1000 - 1);

        let base = now_ms();
        super::super::keepalive::tick_with(
            &server,
            base,
            &|_| crate::server::knock::AgentState::Working,
            &|_, _| true,
            &|_, _| true,
        );
        super::super::keepalive::tick_with(
            &server,
            base + 1,
            &|_| crate::server::knock::AgentState::Waiting,
            &|_, _| true,
            &|_, _| true,
        );
        let first_keepalive_message_id = server
            .state
            .lock()
            .unwrap()
            .wake_bindings
            .iter()
            .find_map(|(message_id, bound)| {
                (bound == &direct_subscription_id).then_some(message_id.clone())
            })
            .expect("first keepalive idle notice");
        server.commit(&[
            Event::Delivered {
                ids: vec![first_keepalive_message_id.clone()],
            },
            Event::Acked {
                ids: vec![first_keepalive_message_id],
            },
        ]);

        super::super::keepalive::tick_with(
            &server,
            base + 120_001,
            &|_| crate::server::knock::AgentState::Waiting,
            &|_, _| true,
            &|_, _| true,
        );
        let second_keepalive_message_id = {
            let state = server.state.lock().unwrap();
            state
                .wake_bindings
                .iter()
                .filter_map(|(message_id, bound)| {
                    let message = state.msgs.get(message_id)?;
                    (bound == &direct_subscription_id && message.state == "pending")
                        .then_some(message_id.clone())
                })
                .next()
                .expect("second keepalive idle notice")
        };
        server.commit(&[
            Event::Delivered {
                ids: vec![second_keepalive_message_id.clone()],
            },
            Event::Acked {
                ids: vec![second_keepalive_message_id],
            },
        ]);

        tick_with_idle(&server, &|_| false);
        let timer_message_id = server
            .state
            .lock()
            .unwrap()
            .wake_bindings
            .iter()
            .find_map(|(message_id, bound)| {
                (bound == &timer_subscription_id).then_some(message_id.clone())
            })
            .expect("timer consumes the third shared notice");
        {
            let state = server.state.lock().unwrap();
            assert_eq!(state.keepalives["master"].idle_episode_notices, 3);
            assert_eq!(state.msgs.len(), 3);
        }
        server.commit(&[
            Event::Delivered {
                ids: vec![timer_message_id.clone()],
            },
            Event::NotificationConsumed {
                subscription_id: timer_subscription_id.clone(),
                message_id: timer_message_id,
                consumed_ms: now_ms(),
            },
        ]);

        tick_with_idle(&server, &|_| false);
        let state = server.state.lock().unwrap();
        assert_eq!(
            state
                .wake_bindings
                .values()
                .filter(|bound| *bound == &timer_subscription_id)
                .count(),
            1,
            "timer must stop after keepalive and timer notices consume all three slots"
        );
        assert_eq!(state.keepalives["master"].idle_episode_notices, 3);
        assert_eq!(
            state.notification_subscriptions[&timer_subscription_id].fired_count,
            1
        );
        drop(state);
        std::fs::remove_dir_all(root).ok();
    }

    #[test]
    fn master_idle_timer_does_not_wake_an_episode_seen_before_subscription() {
        let (server, root) = test_server();
        register_master(&server);
        let subscription_id = master_idle_subscription(&server, 15 * 60 * 1000);
        let created_ms =
            server.state.lock().unwrap().notification_subscriptions[&subscription_id].created_ms;
        let mut record = server.state.lock().unwrap().keepalives["master"].clone();
        record.idle_since_ms = created_ms - 1;
        record.idle_episode_notices = 1;
        server.commit(&[Event::KeepaliveUpdated {
            worker_id: "master".into(),
            record,
        }]);

        tick_with_idle(&server, &|_| false);

        let state = server.state.lock().unwrap();
        assert!(state.msgs.is_empty());
        assert_eq!(
            state.notification_subscriptions[&subscription_id].fired_count,
            0
        );
        drop(state);
        std::fs::remove_dir_all(root).ok();
    }

    #[test]
    fn wait_expiry_without_live_master_does_not_fabricate_a_recipient() {
        let (server, root) = test_server();
        register(&server, "waiter");
        working_task(&server, "waiter");
        let now = now_ms();
        let mut task = server.state.lock().unwrap().tasks["task"].clone();
        task.status = "waiting".into();
        task.wait = Some(WaitSpec {
            waiter: "waiter".into(),
            waiting_for: "holder".into(),
            responsible_actor: "holder-owner".into(),
            reason: "resource_conflict".into(),
            deadline_ms: now - 1,
            resume_on: vec!["resource_released".into()],
            escalation: "resource_owner_and_waiter_recheck".into(),
        });
        server.commit(&[Event::TaskUpdated { task }]);
        tick_with_idle(&server, &|_| false);
        let state = server.state.lock().unwrap();
        assert_eq!(state.tasks["task"].status, "blocked");
        // There is no live owner for a scheduling reason, so the timeout is
        // still durable in the blocked task and does not invent a mailbox
        // recipient or wake the waiting worker.
        assert!(state.tasks["task"]
            .next_step
            .as_deref()
            .unwrap()
            .contains("reason=resource_conflict"));
        assert!(state.msgs.is_empty());
        drop(state);
        std::fs::remove_dir_all(root).ok();
    }

    #[test]
    fn wait_timeout_without_direct_subscription_stays_in_master_mailbox() {
        let (server, root) = test_server();
        register_master(&server);
        register(&server, "waiter");
        working_task(&server, "waiter");
        let now = now_ms();
        let mut task = server.state.lock().unwrap().tasks["task"].clone();
        task.status = "waiting".into();
        task.wait = Some(WaitSpec {
            waiter: "waiter".into(),
            waiting_for: "holder".into(),
            responsible_actor: "holder-owner".into(),
            reason: "resource_conflict".into(),
            deadline_ms: now - 1,
            resume_on: vec!["resource_released".into()],
            escalation: "resource_owner_and_waiter_recheck".into(),
        });
        server.commit(&[Event::TaskUpdated { task }]);

        tick_with_idle(&server, &|_| false);
        tick_with_idle(&server, &|_| false);
        let state = server.state.lock().unwrap();
        assert_eq!(state.tasks["task"].status, "blocked");
        assert_eq!(state.msgs.len(), 1);
        let message = state.msgs.values().next().unwrap();
        assert_eq!(message.to, "master");
        assert_eq!(message.state, "pending");
        assert!(state.wake_bindings.is_empty());
        assert!(!state.msgs.values().any(|message| message.to == "waiter"));
        drop(state);
        std::fs::remove_dir_all(root).ok();
    }

    #[test]
    fn wait_timeout_reason_is_durable_and_visible_only_to_live_master() {
        let (mut server, root) = test_server();
        Arc::get_mut(&mut server).unwrap().config.notifications.mode = "immediate".into();
        Arc::get_mut(&mut server).unwrap().pane_state_check =
            |_| crate::server::knock::AgentState::Working;
        register_master(&server);
        register(&server, "waiter");
        working_task(&server, "waiter");
        let subscription_id = subscribe(&server, "master", "direct-message", None, None);
        let now = now_ms();
        let mut task = server.state.lock().unwrap().tasks["task"].clone();
        task.status = "waiting".into();
        task.wait = Some(WaitSpec {
            waiter: "waiter".into(),
            waiting_for: "holder".into(),
            responsible_actor: "holder-owner".into(),
            reason: "resource_conflict".into(),
            deadline_ms: now - 1,
            resume_on: vec!["resource_released".into()],
            escalation: "resource_owner_and_waiter_recheck".into(),
        });
        server.commit(&[Event::TaskUpdated { task }]);

        // A timeout is recorded while the live master is working. Its reason
        // stays pending in the durable mailbox and no worker is notified.
        tick_with_idle(&server, &|_| false);
        let (message_id, message) = {
            let state = server.state.lock().unwrap();
            assert_eq!(state.tasks["task"].status, "blocked");
            assert_eq!(
                state.tasks["task"].next_step.as_deref(),
                Some(
                    "WAIT_TIMEOUT waiting_for=holder responsible_actor=holder-owner reason=resource_conflict escalation=resource_owner_and_waiter_recheck"
                )
            );
            assert_eq!(state.msgs.len(), 1);
            let (message_id, message) = state.msgs.iter().next().unwrap();
            assert_eq!(message.to, "master");
            assert_eq!(message.subject.as_deref(), Some("wait-timeout:task"));
            assert!(message.body.contains("reason=resource_conflict"));
            assert!(message.body.contains("WAIT_TIMEOUT"));
            assert_eq!(message.state, "pending");
            assert_eq!(message.wake_attempt_count, 0);
            assert_eq!(state.wake_bindings.get(message_id), Some(&subscription_id));
            assert!(!state.msgs.values().any(|message| message.to == "waiter"));
            (message_id.clone(), message.clone())
        };

        // A repeated tick observes the blocked task and cannot create a second
        // scheduling reason. Switching the master to Waiting lets the existing
        // notification batch path deliver the pending occurrence.
        tick_with_idle(&server, &|_| false);
        assert_eq!(server.state.lock().unwrap().msgs.len(), 1);
        Arc::get_mut(&mut server).unwrap().pane_state_check =
            |_| crate::server::knock::AgentState::Waiting;
        assert!(super::super::attempt_notification_with_default(
            &server,
            &message_id,
            &subscription_id,
            &|_| true,
            &|_, _| true,
        ));
        let state = server.state.lock().unwrap();
        assert_eq!(state.msgs[&message_id].to, message.to);
        assert_eq!(state.msgs[&message_id].subject, message.subject);
        assert_eq!(state.msgs[&message_id].body, message.body);
        assert_eq!(state.msgs[&message_id].state, "delivered");
        assert_eq!(state.msgs[&message_id].wake_attempt_count, 1);
        assert_eq!(state.msgs.len(), 1);
        drop(state);

        // Snapshot replay retains the blocked task and its single mailbox
        // reason; another timer tick still has no timeout transition to emit.
        let snapshot = server.state.lock().unwrap().snapshot_events();
        let (mut replayed, replay_root) = test_server();
        Arc::get_mut(&mut replayed).unwrap().pane_state_check =
            |_| crate::server::knock::AgentState::Working;
        for event in &snapshot {
            replayed.commit(std::slice::from_ref(event));
        }
        tick_with_idle(&replayed, &|_| false);
        let replayed_state = replayed.state.lock().unwrap();
        assert_eq!(replayed_state.tasks["task"].status, "blocked");
        assert_eq!(replayed_state.msgs.len(), 1);
        assert_eq!(replayed_state.msgs[&message_id].to, "master");
        drop(replayed_state);
        std::fs::remove_dir_all(replay_root).ok();
        std::fs::remove_dir_all(root).ok();
    }

    #[test]
    fn pane_lost_transitions_subscription_to_pane_lost_and_stops_storm() {
        let (mut server, root) = test_server();
        Arc::get_mut(&mut server).unwrap().pane_alive_check =
            |_| super::super::knock::PanePresence::Missing;
        register(&server, "lost-worker");
        let sub = subscribe(&server, "lost-worker", "direct-message", None, None);
        let id = bind_message(&server, "lost-worker", &sub);
        // First tick discovers dead pane, cancels armed subscription to pane-lost.
        tick_with_idle(&server, &|_| true);
        let state = server.state.lock().unwrap();
        assert_eq!(state.notification_subscriptions[&sub].status, "pane-lost");
        assert_eq!(state.msgs[&id].wake_attempt_count, 0);
        drop(state);

        // Subsequent tick does not query or wake dead pane.
        tick_with_idle(&server, &|_| true);
        let state = server.state.lock().unwrap();
        assert_eq!(state.notification_subscriptions[&sub].status, "pane-lost");
        assert_eq!(state.msgs[&id].wake_attempt_count, 0);
        drop(state);
        std::fs::remove_dir_all(root).ok();
    }

    #[test]
    fn durable_pane_mismatch_cancels_even_when_liveness_is_unknown() {
        use super::super::knock::PanePresence;
        for notification_entry in [false, true] {
            let (mut server, root) = test_server();
            register(&server, "worker");
            let sub = subscribe(&server, "worker", "direct-message", None, None);
            let message = bind_message(&server, "worker", &sub);
            let mut worker = server.state.lock().unwrap().workers["worker"].clone();
            worker.pane = Some("%replacement".into());
            server.commit(&[Event::Registered { worker }]);
            Arc::get_mut(&mut server).unwrap().pane_alive_check = |_| PanePresence::Unknown;
            if notification_entry {
                assert!(!super::super::attempt_notification_with(
                    &server,
                    &message,
                    &sub,
                    &|_| panic!("mismatched pane cannot receive"),
                    &|_, _| panic!("mismatched pane cannot be sent to"),
                    &server.pane_owner_check
                ));
            } else {
                tick_with_idle(&server, &|_| true);
            }
            let state = server.state.lock().unwrap();
            assert_eq!(
                state.notification_subscriptions[&sub].status, "pane-lost",
                "notification_entry={notification_entry}"
            );
            assert_eq!(state.msgs[&message].wake_attempt_count, 0);
            drop(state);
            std::fs::remove_dir_all(root).unwrap();
        }
    }

    #[test]
    fn unknown_prechecks_defer_timer_and_notification_without_cancelling_or_sending() {
        use super::super::knock::{AgentState, PanePresence};
        for failure in ["presence", "ownership", "view"] {
            let (mut server, root) = test_server();
            register(&server, "worker");
            let direct = subscribe(&server, "worker", "direct-message", None, None);
            let deadline = subscribe(
                &server,
                "worker",
                "deadline",
                Some("due"),
                Some(now_ms() - 1),
            );
            let message = bind_message(&server, "worker", &direct);
            let inner = Arc::get_mut(&mut server).unwrap();
            match failure {
                "presence" => {
                    inner.pane_alive_check = |_| PanePresence::Unknown;
                    inner.pane_owner_check =
                        |_, _| panic!("unknown presence must short-circuit ownership");
                    inner.pane_state_check =
                        |_| panic!("unknown presence must short-circuit state probe");
                }
                "ownership" => {
                    inner.pane_owner_check = |_, _| Err(());
                    inner.pane_state_check =
                        |_| panic!("unknown ownership must short-circuit state probe");
                }
                _ => inner.pane_state_check = |_| AgentState::Unknown,
            }
            assert!(!super::super::attempt_notification_with(
                &server,
                &message,
                &direct,
                &|_| panic!("unknown must not reach delivery readiness"),
                &|_, _| panic!("unknown must not send"),
                &server.pane_owner_check
            ));
            tick_with_idle(&server, &|_| true);
            let state = server.state.lock().unwrap();
            assert_eq!(
                state.notification_subscriptions[&direct].status, "armed",
                "{failure}"
            );
            assert_eq!(
                state.notification_subscriptions[&deadline].status, "armed",
                "{failure}"
            );
            assert_eq!(state.notification_subscriptions[&deadline].fired_count, 0);
            assert_eq!(
                state.msgs.len(),
                1,
                "unknown deadline must not enqueue a notification"
            );
            assert_eq!(state.msgs[&message].state, "pending");
            assert_eq!(state.msgs[&message].wake_attempt_count, 0);
            drop(state);
            std::fs::remove_dir_all(root).unwrap();
        }
    }

    #[test]
    fn working_agent_defers_notification_without_attempt() {
        let (mut server, root) = test_server();
        Arc::get_mut(&mut server).unwrap().pane_state_check =
            |_| crate::server::knock::AgentState::Working;
        register(&server, "busy-worker");
        let sub = subscribe(&server, "busy-worker", "direct-message", None, None);
        let id = bind_message(&server, "busy-worker", &sub);
        server
            .state
            .lock()
            .unwrap()
            .msgs
            .get_mut(&id)
            .unwrap()
            .created_ms = now_ms() - 120_001;

        assert!(!super::super::attempt_notification_with_default(
            &server,
            &id,
            &sub,
            &|_| true,
            &|_, _| panic!("should not deliver while busy")
        ));
        assert_eq!(server.state.lock().unwrap().msgs[&id].wake_attempt_count, 0);
        assert_eq!(
            server.state.lock().unwrap().notification_subscriptions[&sub].status,
            "armed"
        );
        // When agent transitions to idle (state becomes Waiting), wake succeeds.
        Arc::get_mut(&mut server).unwrap().pane_state_check =
            |_| crate::server::knock::AgentState::Waiting;
        assert!(super::super::attempt_notification_with_default(
            &server,
            &id,
            &sub,
            &|_| true,
            &|_, _| true
        ));
        assert_eq!(server.state.lock().unwrap().msgs[&id].wake_attempt_count, 1);
        std::fs::remove_dir_all(root).ok();
    }

    #[test]
    fn unknown_agent_keeps_notification_pending_without_burning_attempt() {
        let (mut server, root) = test_server();
        Arc::get_mut(&mut server).unwrap().pane_state_check =
            |_| crate::server::knock::AgentState::Unknown;
        register(&server, "unknown-worker");
        let sub = subscribe(&server, "unknown-worker", "direct-message", None, None);
        let id = bind_message_with_id(&server, "unknown-worker", &sub, "msg-unknown");

        assert!(!super::super::attempt_notification_with_default(
            &server,
            &id,
            &sub,
            &|_| true,
            &|_, _| panic!("unknown agent must not receive a notification"),
        ));
        let state = server.state.lock().unwrap();
        assert_eq!(state.msgs[&id].state, "pending");
        assert_eq!(state.msgs[&id].wake_attempt_count, 0);
        drop(state);
        std::fs::remove_dir_all(root).ok();
    }

    #[test]
    fn explicit_notification_remains_immediate_while_agent_is_working() {
        let (mut server, root) = test_server();
        Arc::get_mut(&mut server).unwrap().pane_state_check =
            |_| crate::server::knock::AgentState::Working;
        register(&server, "busy-worker");
        let sub = subscribe(&server, "busy-worker", "direct-message", None, None);
        let id = bind_message(&server, "busy-worker", &sub);
        server.commit(&[Event::DeliveryMode {
            msg_id: id.clone(),
            mode: "explicit-notification".into(),
        }]);
        server
            .state
            .lock()
            .unwrap()
            .msgs
            .get_mut(&id)
            .unwrap()
            .created_ms = now_ms() - 60_001;

        assert!(super::super::attempt_notification_with_default(
            &server,
            &id,
            &sub,
            &|_| true,
            &|_, _| true
        ));
        assert_eq!(server.state.lock().unwrap().msgs[&id].wake_attempt_count, 1);
        std::fs::remove_dir_all(root).ok();
    }

    #[test]
    fn unacked_notifications_limit_pauses_wakes_and_resumes_after_ack() {
        let (mut server, root) = test_server();
        Arc::get_mut(&mut server).unwrap().config.notifications.mode = "immediate".into();
        register(&server, "ack-worker");
        let sub = subscribe(&server, "ack-worker", "direct-message", None, None);

        // Deliver 3 notifications (max_unacked = 3)
        let mut msg_ids = Vec::new();
        for i in 0..3 {
            let id = bind_message_with_id(&server, "ack-worker", &sub, &format!("msg-ack-{i}"));
            assert!(super::super::attempt_notification_with_default(
                &server,
                &id,
                &sub,
                &|_| true,
                &|_, _| true,
            ));
            msg_ids.push(id);
        }

        // 3 messages are now delivered and unacked
        assert_eq!(
            server
                .state
                .lock()
                .unwrap()
                .msgs
                .values()
                .filter(|m| m.to == "ack-worker" && m.state == "delivered")
                .count(),
            3
        );

        let id4 = bind_message_with_id(&server, "ack-worker", &sub, "msg-ack-4");

        assert!(!super::super::attempt_notification_with_default(
            &server,
            &id4,
            &sub,
            &|_| true,
            &|_, _| panic!("unacked notification limit must defer delivery"),
        ));
        assert_eq!(
            server.state.lock().unwrap().msgs[&id4].wake_attempt_count,
            0
        );

        server.commit(&[Event::Acked { ids: msg_ids }]);
        assert_eq!(
            server
                .state
                .lock()
                .unwrap()
                .msgs
                .values()
                .filter(|m| m.to == "ack-worker" && m.state == "delivered")
                .count(),
            0
        );

        assert!(super::super::attempt_notification_with_default(
            &server,
            &id4,
            &sub,
            &|_| true,
            &|_, _| true,
        ));
        assert_eq!(
            server.state.lock().unwrap().msgs[&id4].wake_attempt_count,
            1
        );

        assert!(!super::super::attempt_notification_with_default(
            &server,
            &id4,
            &sub,
            &|_| true,
            &|_, _| panic!("already delivered message must not replay"),
        ));
        assert_eq!(
            server.state.lock().unwrap().msgs[&id4].wake_attempt_count,
            1
        );
        std::fs::remove_dir_all(root).ok();
    }

    #[test]
    fn backlog_sends_latest_three_once_and_truncates_to_1024_chars() {
        let (server, root) = test_server();
        register(&server, "batch-worker");
        let sub = subscribe(&server, "batch-worker", "direct-message", None, None);

        // Queue 5 messages with unique IDs for batch-worker
        for i in 0..5 {
            let _ = bind_message_with_id(&server, "batch-worker", &sub, &format!("msg-batch-{i}"));
        }
        server
            .state
            .lock()
            .unwrap()
            .msgs
            .get_mut("msg-batch-4")
            .unwrap()
            .body = "界".repeat(2_000);

        let first_id = "msg-batch-0";
        let delivered_text = std::sync::Mutex::new(String::new());
        assert!(super::super::attempt_notification_with_default(
            &server,
            first_id,
            &sub,
            &|_| true,
            &|_, text| {
                *delivered_text.lock().unwrap() = text.to_string();
                true
            },
        ));

        let text = delivered_text.lock().unwrap().clone();
        assert!(text.chars().count() <= 1024);
        assert!(!text.contains("msg-batch-0"));
        assert!(!text.contains("msg-batch-1"));
        assert!(text.contains("msg-batch-2"));
        assert!(text.contains("msg-batch-3"));
        assert!(text.contains("msg-batch-4"));
        assert!(text.contains("message_ids="));
        assert!(text.contains("task_ids=none"));
        assert!(text.contains("action_categories="));
        assert!(text.contains("collab inbox"));

        let state = server.state.lock().unwrap();
        let delivered_count = state
            .msgs
            .values()
            .filter(|m| m.to == "batch-worker" && m.state == "delivered")
            .count();
        let pending_count = state
            .msgs
            .values()
            .filter(|m| m.to == "batch-worker" && m.state == "pending")
            .count();
        assert_eq!(delivered_count, 3);
        assert_eq!(pending_count, 2);
        assert_eq!(state.msgs["msg-batch-0"].state, "pending");
        assert_eq!(state.msgs["msg-batch-1"].state, "pending");
        assert_eq!(state.msgs["msg-batch-0"].wake_attempt_count, 0);
        assert_eq!(state.msgs["msg-batch-1"].wake_attempt_count, 0);
        assert_eq!(state.msgs["msg-batch-2"].state, "delivered");
        assert_eq!(state.msgs["msg-batch-3"].state, "delivered");
        assert_eq!(state.msgs["msg-batch-4"].state, "delivered");
        drop(state);
        assert!(!super::super::attempt_notification_with_default(
            &server,
            "msg-batch-0",
            &sub,
            &|_| true,
            &|_, _| panic!("older backlog must not be pushed later"),
        ));
        std::fs::remove_dir_all(root).ok();
    }

    #[test]
    fn identity_mismatch_or_absent_worker_transitions_subscription_to_pane_lost() {
        let (mut server, root) = test_server();
        register(&server, "mismatch-worker");
        let sub = subscribe(&server, "mismatch-worker", "direct-message", None, None);

        // Case 1: Worker's registered pane changed (identity mismatch with old subscription)
        server.commit(&[Event::Registered {
            worker: WorkerRec {
                id: "mismatch-worker".into(),
                token: "token-mismatch-worker".into(),
                pane: Some("%new-pane".into()),
                cwd: "/tmp".into(),
                registered_ms: now_ms(),
            },
        }]);
        tick_with_idle(&server, &|_| true);
        assert_eq!(
            server.state.lock().unwrap().notification_subscriptions[&sub].status,
            "pane-lost"
        );

        // Case 2: Agent process absent transitions armed subscription to pane-lost
        let sub2 = subscribe(&server, "mismatch-worker", "direct-message", None, None);
        server.commit(&[Event::Registered {
            worker: WorkerRec {
                id: "mismatch-worker".into(),
                token: "token-mismatch-worker".into(),
                pane: Some(format!("%test-mismatch-worker")),
                cwd: "/tmp".into(),
                registered_ms: now_ms(),
            },
        }]);
        Arc::get_mut(&mut server).unwrap().pane_state_check =
            |_| crate::server::knock::AgentState::Absent;
        tick_with_idle(&server, &|_| true);
        assert_eq!(
            server.state.lock().unwrap().notification_subscriptions[&sub2].status,
            "pane-lost"
        );

        std::fs::remove_dir_all(root).ok();
    }
}
