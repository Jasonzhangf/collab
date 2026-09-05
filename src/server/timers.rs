use crate::server::knock::pane_idle;
use crate::server::state::{now_ms, Event, Message, MAX_WAKE_ATTEMPTS};
use crate::server::Server;
use std::sync::Arc;

const WAKE_ATTEMPT_LEASE_MS: i64 = 10_000;

/// Server-side scheduler for finite subscriptions and bounded waits. It never
/// creates task continuations or infers that ordinary work needs a wake.
pub fn tick(server: &Arc<Server>) {
    tick_with_idle(server, &pane_idle);
}

fn tick_with_idle(server: &Arc<Server>, is_idle: &dyn Fn(&str) -> bool) {
    if server.state.lock().unwrap().admission_frozen() {
        return;
    }
    let now = now_ms();
    let mut lifecycle_events = Vec::new();
    {
        let state = server.state.lock().unwrap();
        for subscription in state.notification_subscriptions.values() {
            if subscription.status == "armed" && subscription.expires_ms <= now {
                lifecycle_events.push(Event::NotificationStatus {
                    subscription_id: subscription.id.clone(),
                    status: "expired".into(),
                    updated_ms: now,
                });
            }
        }
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
                "WAIT_TIMEOUT waiting_for={} responsible_actor={} escalation={}",
                wait.waiting_for, wait.responsible_actor, wait.escalation
            ));
            expired.wait = None;
            expired.updated_ms = now;
            lifecycle_events.push(Event::TaskUpdated { task: expired });
        }
    }
    if !lifecycle_events.is_empty() {
        server.commit(&lifecycle_events);
    }

    let mut due_events = Vec::new();
    {
        let state = server.state.lock().unwrap();
        for subscription in state.notification_subscriptions.values() {
            if subscription.status != "armed"
                || subscription.event != "deadline"
                || subscription.trigger_ms.is_none_or(|trigger| trigger > now)
                || state
                    .wake_bindings
                    .values()
                    .any(|bound| bound == &subscription.id)
            {
                continue;
            }
            let message_id = super::gen_msg_id();
            due_events.extend([
                Event::Sent {
                    msg: Message {
                        id: message_id.clone(),
                        from: "collab-server".into(),
                        to: subscription.worker_id.clone(),
                        mtype: "notification".into(),
                        subject: subscription
                            .subject
                            .as_ref()
                            .map(|subject| format!("deadline:{subject}")),
                        body: format!(
                            "DEADLINE_REACHED subject={}",
                            subscription.subject.as_deref().unwrap_or_default()
                        ),
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
                Event::DeliveryMode {
                    msg_id: message_id,
                    mode: "explicit-notification".into(),
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
            is_idle,
            &|pane, id| super::queue_system_knock(server, pane, id),
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::server::state::{NotificationSubscription, State, TaskRec, WaitSpec, WorkerRec};
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
                root: root.clone(),
                state: Mutex::new(State::default()),
                journal: Mutex::new(journal),
                pane_alive_check: |_| true,
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
                expires_ms: now_ms() + 60_000,
                status: "armed".into(),
                created_ms: now_ms(),
                updated_ms: now_ms(),
            },
        }]);
        id
    }

    fn bind_message(server: &Server, worker_id: &str, subscription_id: &str) -> String {
        let message_id = format!("message-{worker_id}");
        server.commit(&[
            Event::Sent {
                msg: Message {
                    id: message_id.clone(),
                    from: "peer".into(),
                    to: worker_id.into(),
                    mtype: "notify".into(),
                    subject: Some("released:held".into()),
                    body: "RESOURCE_RELEASED task=held".into(),
                    in_reply_to: None,
                    created_ms: now_ms(),
                    state: "pending".into(),
                    wake_attempt_count: 0,
                    last_wake_attempt_ms: 0,
                },
            },
            Event::WakeBound {
                message_id: message_id.clone(),
                subscription_id: subscription_id.into(),
            },
        ]);
        message_id
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
    fn failed_one_shot_notification_has_hard_three_attempt_lifetime_cap() {
        let (server, root) = test_server();
        register(&server, "owner");
        let subscription_id = subscribe(&server, "owner", "resource-released", Some("held"), None);
        let message_id = bind_message(&server, "owner", &subscription_id);
        for _ in 0..4 {
            super::super::attempt_notification_with(
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
            "attempts-exhausted"
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
            super::super::attempt_notification_with(
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
            super::super::attempt_notification_with(
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
            root: root.clone(),
            state: Mutex::new(replayed),
            journal: Mutex::new(journal),
            pane_alive_check: |_| true,
        };
        let sent = std::sync::atomic::AtomicBool::new(false);
        assert!(!super::super::attempt_notification_with(
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
        assert!(super::super::attempt_notification_with(
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
        assert!(super::super::attempt_notification_with(
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
        assert!(!super::super::attempt_notification_with(
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
        assert!(super::super::attempt_notification_with(
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
    fn wait_expiry_changes_task_without_unsolicited_messages() {
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
        assert!(state.msgs.is_empty());
        drop(state);
        std::fs::remove_dir_all(root).ok();
    }
}
