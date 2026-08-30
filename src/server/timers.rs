use crate::config;
use crate::server::knock::pane_idle;
use crate::server::state::{now_ms, task_continuation_active, Event, Message, TaskRec};
use crate::server::Server;
use std::collections::HashSet;
use std::sync::Arc;

const WAKE_ATTEMPT_LEASE_MS: i64 = 10_000;

fn continuation_interval_ms(server: &Server) -> i64 {
    config::load_or_default(&server.root)
        .continuation_minutes
        .saturating_mul(60 * 1000)
}

fn continuation_body(task: &TaskRec) -> String {
    format!(
        "CONTINUE_TASK task={} owner={} status={} next={} | query collab context, verify durable state, then continue the next safe lifecycle step",
        task.id,
        task.owner,
        task.status,
        task.next_step.as_deref().unwrap_or("inspect own task state")
    )
}

/// Server-side watchdog. Runs every 5s; emits journal events only, all
/// mutations go through the same commit path as client ops.
pub fn tick(server: &Arc<Server>) {
    tick_with_idle(server, &pane_idle);
}

fn tick_with_idle(server: &Arc<Server>, is_idle: &dyn Fn(&str) -> bool) {
    if server.state.lock().unwrap().admission_frozen() {
        return;
    }
    let now = now_ms();
    let mut evs: Vec<Event> = Vec::new();
    let mut knocks: Vec<(String, Vec<String>, String)> = Vec::new();

    let idle_panes: HashSet<String> = {
        let st = server.state.lock().unwrap();
        st.workers
            .values()
            .filter_map(|worker| worker.pane.clone())
            .filter(|pane| is_idle(pane))
            .collect()
    };
    {
        let st = server.state.lock().unwrap();

        // A wait always has a finite liveness boundary. Expiry becomes an
        // explicit blocker. Both peers receive durable truth; tmux remains a
        // best-effort wake and never decides the transition.
        for task in st.tasks.values() {
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
            expired.continuation_pending = false;
            expired.continuation_message_id = None;
            evs.push(Event::TaskUpdated { task: expired });
            for recipient in [&wait.waiter, &wait.responsible_actor] {
                let (event, msg_id, body) = sent_system(
                    recipient,
                    format!(
                        "TASK_WAIT_TIMEOUT task={} waiter={} waiting_for={} responsible_actor={} | recheck the resource through Collab; no claim was released",
                        task.id, wait.waiter, wait.waiting_for, wait.responsible_actor
                    ),
                );
                evs.push(event);
                evs.push(Event::DeliveryMode {
                    msg_id: msg_id.clone(),
                    mode: "immediate".into(),
                });
                if let Some(pane) = st.worker_pane(recipient) {
                    if idle_panes.contains(&pane) {
                        knocks.push((pane, vec![msg_id], body));
                    }
                }
            }
        }

        // Local continuation is task-scoped and deduped by the durable pending
        // marker. A working pane is never interrupted.
        for task in st.tasks.values() {
            if !task_continuation_active(&task.status) || task.continuation_pending {
                continue;
            }
            if now - task.last_continuation_sent_ms < continuation_interval_ms(server) {
                continue;
            }
            let Some(pane) = st.worker_pane(&task.owner) else {
                continue;
            };
            if !idle_panes.contains(&pane) {
                continue;
            }
            let body = continuation_body(task);
            let sid = super::gen_msg_id();
            let msg = Message {
                id: sid.clone(),
                from: "collab-server".into(),
                to: task.owner.clone(),
                mtype: "system".into(),
                body: body.clone(),
                in_reply_to: None,
                created_ms: now,
                state: "pending".into(),
                wake_attempt_count: 0,
                last_wake_attempt_ms: 0,
            };
            evs.push(Event::Sent { msg });
            evs.push(Event::DeliveryMode {
                msg_id: sid.clone(),
                mode: "immediate".into(),
            });
            evs.push(Event::TaskUpdated {
                task: TaskRec {
                    last_continuation_sent_ms: now,
                    continuation_pending: true,
                    continuation_message_id: Some(sid.clone()),
                    ..task.clone()
                },
            });
            knocks.push((pane, vec![sid], body));
        }

        for worker in st.workers.values() {
            let Some(pane) = worker.pane.as_deref() else {
                continue;
            };
            if !idle_panes.contains(pane) {
                continue;
            }
            let pending: Vec<&Message> = st
                .msgs
                .values()
                .filter(|m| {
                    m.to == worker.id
                        && m.state == "pending"
                        && now - m.last_wake_attempt_ms >= WAKE_ATTEMPT_LEASE_MS
                        && st
                            .delivery_modes
                            .get(&m.id)
                            .is_some_and(|mode| matches!(mode.as_str(), "immediate" | "idle"))
                })
                .collect();
            if pending.is_empty() {
                continue;
            }
            let ids = pending.iter().map(|m| m.id.clone()).collect::<Vec<_>>();
            let body = pending
                .iter()
                .map(|m| format!("[{}] {}", m.mtype, m.body))
                .collect::<Vec<_>>()
                .join("; ");
            let prompt = format!(
                "[COLLAB QUEUE] {}; Process these messages now through Collab, execute the required task actions, and continue; do not reply without executing an action. message_ids={}",
                body,
                ids.join(",")
            );
            knocks.push((pane.to_string(), ids, prompt));
        }
    }

    if evs.is_empty() && knocks.is_empty() {
        return;
    }
    if !evs.is_empty() {
        server.commit(&evs);
    }
    for (pane, ids, body) in knocks {
        server.commit(&[Event::WakeAttempted {
            ids: ids.clone(),
            attempted_ms: now_ms(),
        }]);
        let delivered = if ids.len() > 1 {
            super::queue_batch_knock(server, &pane, &ids, &body)
        } else {
            super::queue_system_knock(server, &pane, &ids[0], &body)
        };
        if delivered {
            server.commit(&[Event::Delivered { ids }]);
        }
    }
}

fn sent_system(to: &str, body: String) -> (Event, String, String) {
    let id = super::gen_msg_id();
    (
        Event::Sent {
            msg: Message {
                id: id.clone(),
                from: "collab-server".into(),
                to: to.into(),
                mtype: "system".into(),
                body: body.clone(),
                in_reply_to: None,
                created_ms: now_ms(),
                state: "pending".into(),
                wake_attempt_count: 0,
                last_wake_attempt_ms: 0,
            },
        },
        id,
        body,
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::server::state::{State, WaitSpec, WorkerRec};
    use std::sync::Mutex;

    fn test_server() -> (Arc<Server>, std::path::PathBuf) {
        static SEQ: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
        let n = SEQ.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        let root = std::env::temp_dir().join(format!("collab-timer-{}-{n}", std::process::id()));
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

    fn register(server: &Server, id: &str) {
        server.commit(&[Event::Registered {
            worker: WorkerRec {
                id: id.into(),
                token: format!("token-{id}"),
                pane: Some(format!("%test-{id}")),
                cwd: "/tmp".into(),
                registered_ms: now_ms(),
            },
        }]);
    }

    fn task(server: &Server, id: &str, owner: &str, status: &str, last_continuation: i64) {
        let now = now_ms();
        server.commit(&[Event::TaskCreated {
            task: TaskRec {
                id: id.into(),
                owner: owner.into(),
                created_by: owner.into(),
                feature_id: Some("feature".into()),
                worktree_path: Some("/tmp/wt".into()),
                branch: None,
                base_commit: None,
                priority: "p2".into(),
                status: status.into(),
                next_step: Some("continue".into()),
                created_ms: now,
                updated_ms: now,
                last_continuation_sent_ms: last_continuation,
                continuation_pending: false,
                continuation_message_id: None,
                wait: None,
            },
        }]);
    }

    #[test]
    fn continuation_wake_is_durable_and_deduped() {
        let (server, root) = test_server();
        register(&server, "peer-a");
        register(&server, "owner");
        task(
            &server,
            "t1",
            "owner",
            "working",
            now_ms() - continuation_interval_ms(&server) - 1000,
        );

        tick_with_idle(&server, &|_| true);
        {
            let st = server.state.lock().unwrap();
            assert!(st.msgs.len() >= 1);
            assert!(st.tasks["t1"].continuation_pending);
            let hb_id = st.tasks["t1"].continuation_message_id.clone().unwrap();
            assert!(st.msgs[&hb_id].body.starts_with("CONTINUE_TASK "));
        }

        tick_with_idle(&server, &|_| true);
        {
            let st = server.state.lock().unwrap();
            let continuation_wakes = st
                .msgs
                .values()
                .filter(|m| m.body.starts_with("CONTINUE_TASK "))
                .count();
            assert_eq!(continuation_wakes, 1, "pending wake must not be resent");
        }
        std::fs::remove_dir_all(root).ok();
    }

    #[test]
    fn delivered_task_stops_continuation_wake() {
        let (server, root) = test_server();
        register(&server, "peer-a");
        register(&server, "owner");
        task(&server, "t1", "owner", "delivered", 0);

        tick(&server);
        let st = server.state.lock().unwrap();
        assert!(st
            .msgs
            .values()
            .all(|message| !message.body.starts_with("CONTINUE_TASK ")));
        std::fs::remove_dir_all(root).ok();
    }

    #[test]
    fn working_or_offline_owner_is_not_interrupted() {
        let (server, root) = test_server();
        register(&server, "owner");
        task(
            &server,
            "t1",
            "owner",
            "working",
            now_ms() - continuation_interval_ms(&server) - 1000,
        );

        tick_with_idle(&server, &|_| false);
        let state = server.state.lock().unwrap();
        assert!(state.msgs.is_empty());
        assert!(!state.tasks["t1"].continuation_pending);
        drop(state);
        std::fs::remove_dir_all(root).ok();
    }

    #[test]
    fn expired_wait_blocks_and_notifies_waiter_and_holder_once() {
        let (server, root) = test_server();
        register(&server, "waiter");
        register(&server, "holder");
        task(&server, "waiting", "waiter", "waiting", now_ms());
        task(&server, "held", "holder", "working", now_ms());
        let mut waiting = server.state.lock().unwrap().tasks["waiting"].clone();
        waiting.wait = Some(WaitSpec {
            waiter: "waiter".into(),
            waiting_for: "held".into(),
            responsible_actor: "holder".into(),
            reason: "resource_conflict".into(),
            deadline_ms: now_ms() - 1,
            resume_on: vec!["resource_released".into()],
            escalation: "resource_owner_and_waiter_recheck".into(),
        });
        server.commit(&[Event::TaskUpdated { task: waiting }]);

        tick_with_idle(&server, &|_| false);
        tick_with_idle(&server, &|_| false);
        let state = server.state.lock().unwrap();
        assert_eq!(state.tasks["waiting"].status, "blocked");
        let timeout_messages: Vec<&Message> = state
            .msgs
            .values()
            .filter(|message| message.body.starts_with("TASK_WAIT_TIMEOUT "))
            .collect();
        assert_eq!(timeout_messages.len(), 2);
        assert!(timeout_messages
            .iter()
            .any(|message| message.to == "waiter"));
        assert!(timeout_messages
            .iter()
            .any(|message| message.to == "holder"));
        drop(state);
        std::fs::remove_dir_all(root).ok();
    }

    #[test]
    fn continuation_interval_reads_project_config_from_server_root() {
        let (server, root) = test_server();
        config::save(
            &root,
            &config::Config {
                continuation_minutes: 2,
            },
        )
        .unwrap();

        assert_eq!(continuation_interval_ms(&server), 2 * 60 * 1000);
        std::fs::remove_dir_all(root).ok();
    }

    #[test]
    fn migration_freeze_stops_timer_mutations() {
        let (server, root) = test_server();
        register(&server, "owner");
        task(
            &server,
            "task",
            "owner",
            "working",
            now_ms() - continuation_interval_ms(&server) - 1000,
        );
        {
            let mut state = server.state.lock().unwrap();
            state.migration = Some(crate::server::state::MigrationRecord {
                id: "migration".into(),
                from_version: "v1-legacy".into(),
                to_version: "v1-low-intervention".into(),
                phase: "applied".into(),
                admission_frozen: true,
                snapshot_hash: Some("snapshot".into()),
                worker_count: 1,
                task_count: 1,
                message_count: 0,
                operator: "owner".into(),
                issues: Vec::new(),
                created_ms: now_ms(),
                updated_ms: now_ms(),
            });
        }

        tick_with_idle(&server, &|_| true);
        let state = server.state.lock().unwrap();
        assert!(state.msgs.is_empty());
        assert!(!state.tasks["task"].continuation_pending);
        drop(state);
        std::fs::remove_dir_all(root).ok();
    }

    #[test]
    fn pending_wake_retries_without_new_state_and_failed_retry_stays_pending() {
        let (server, root) = test_server();
        register(&server, "owner");
        let id = "pending-resource".to_string();
        server.commit(&[
            Event::Sent {
                msg: Message {
                    id: id.clone(),
                    from: "collab-server".into(),
                    to: "owner".into(),
                    mtype: "system".into(),
                    body: "RESOURCE_OCCUPIED feature=shared".into(),
                    in_reply_to: None,
                    created_ms: now_ms(),
                    state: "pending".into(),
                    wake_attempt_count: 0,
                    last_wake_attempt_ms: 0,
                },
            },
            Event::DeliveryMode {
                msg_id: id.clone(),
                mode: "immediate".into(),
            },
        ]);

        tick_with_idle(&server, &|_| true);
        tick_with_idle(&server, &|_| true);
        let state = server.state.lock().unwrap();
        assert_eq!(state.msgs[&id].state, "pending");
        assert_eq!(state.msgs[&id].wake_attempt_count, 1);
        drop(state);
        let log = std::fs::read_to_string(server.log_path()).unwrap();
        assert_eq!(log.matches("knock failed pane=%test-owner").count(), 1);
        std::fs::remove_dir_all(root).ok();
    }
}
