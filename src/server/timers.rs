use crate::config;
use crate::server::knock::pane_idle;
use crate::server::state::{now_ms, task_heartbeat_active, Event, Message, TaskRec};
use crate::server::Server;
use std::collections::{HashMap, HashSet};
use std::sync::{Arc, Mutex, OnceLock};

const NUDGE_INTERVAL_MS: i64 = 5 * 60 * 1000;
const MAX_NUDGES: u32 = 3;
const DELIVERY_RETRY_INTERVAL_MS: i64 = 30 * 1000;
const RESOURCE_LOCK_NUDGE_MS: i64 = 15 * 60 * 1000;
static MASTER_HEARTBEATS: OnceLock<Mutex<HashMap<String, i64>>> = OnceLock::new();

fn heartbeat_interval_ms(server: &Server) -> i64 {
    config::load_or_default(&server.root)
        .heartbeat_minutes
        .saturating_mul(60 * 1000)
}

fn heartbeat_body(
    task_id: &str,
    owner: &str,
    master: Option<&str>,
    status: &str,
    next_step: Option<&str>,
    pane: Option<&str>,
) -> String {
    format!(
        "[COLLAB HEARTBEAT] from=collab-server master={} task={} owner={} status={} pane={} next={} | inspect collab role and task status {}, then continue the next safe task step; report only a state change, blocker, ETA change, decision, verification, or handoff; do not wait for another message.",
        master.unwrap_or("unassigned"),
        task_id,
        owner,
        status,
        pane.unwrap_or("unknown"),
        task_id,
        next_step.unwrap_or("inspect task state; continue the next safe step")
    )
}

/// Server-side watchdog. Runs every 5s; emits journal events only, all
/// mutations go through the same commit path as client ops.
pub fn tick(server: &Arc<Server>) {
    let now = now_ms();
    let mut evs: Vec<Event> = Vec::new();
    let mut knocks: Vec<(String, String, String)> = Vec::new();

    let idle_panes: HashSet<String> = {
        let st = server.state.lock().unwrap();
        st.workers
            .values()
            .filter_map(|worker| worker.pane.clone())
            .filter(|pane| pane_idle(pane))
            .collect()
    };
    // Probe outside the state lock: tmux capture can block on a dead pane and
    // must never stall task/message state transitions.
    let probe_panes: Vec<String> = {
        let st = server.state.lock().unwrap();
        st.workers
            .values()
            .filter(|worker| {
                st.tasks
                    .values()
                    .any(|task| task.owner == worker.id && task_heartbeat_active(&task.status))
            })
            .filter_map(|worker| worker.pane.clone())
            .filter(|pane| pane.starts_with('%'))
            .collect()
    };
    let probe_alerts = super::tmux_probe::poll(&probe_panes);

    {
        let st = server.state.lock().unwrap();

        // A wait always has a finite liveness boundary. Expiry becomes an
        // explicit blocker for master resolution; it never silently resumes
        // or releases the waiting claim.
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
            expired.heartbeat_pending = false;
            expired.heartbeat_message_id = None;
            evs.push(Event::TaskUpdated { task: expired });
            let master = st.master_id().unwrap_or_else(|| "unassigned".into());
            let (event, msg_id, body) = sent_system(
                &master,
                format!(
                    "TASK_WAIT_TIMEOUT id={} waiting_for={} responsible_actor={}; MASTER_ACTION: resolve the expired wait through Collab",
                    task.id, wait.waiting_for, wait.responsible_actor
                ),
            );
            evs.push(event);
            if let Some(pane) = st.worker_pane(&master) {
                if idle_panes.contains(&pane) {
                    knocks.push((pane, msg_id, body));
                }
            }
        }

        // 0) task-scoped heartbeat: only active tasks, one pending per task
        for task in st.tasks.values() {
            if matches!(
                task.status.as_str(),
                "working" | "verifying" | "reviewed" | "rework"
            ) && !task.heartbeat_stale_notified
                && now - task.updated_ms >= RESOURCE_LOCK_NUDGE_MS
            {
                let body = format!(
                    "RESOURCE_LOCK_NUDGE task={} owner={} held_for_ms={} | inspect the resource now; execute HOLD/YIELD/SPLIT through Collab and report the decision to master; this message does not release the resource",
                    task.id, task.owner, now - task.updated_ms
                );
                let (event, msg_id, message_body) = sent_system(&task.owner, body);
                evs.push(event);
                evs.push(Event::TaskUpdated {
                    task: TaskRec {
                        heartbeat_stale_notified: true,
                        ..task.clone()
                    },
                });
                if let Some(pane) = st.worker_pane(&task.owner) {
                    if !idle_panes.contains(&pane) {
                        continue;
                    }
                    knocks.push((pane, msg_id, message_body));
                }
            }
            if !task_heartbeat_active(&task.status) || task.heartbeat_pending {
                continue;
            }
            if now - task.last_heartbeat_sent_ms < heartbeat_interval_ms(server) {
                continue;
            }
            let Some(pane) = st.worker_pane(&task.owner) else {
                continue;
            };
            if !idle_panes.contains(&pane) {
                continue;
            }
            let body = heartbeat_body(
                &task.id,
                &task.owner,
                st.master_id().as_deref(),
                &task.status,
                task.next_step.as_deref(),
                Some(&pane),
            );
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
                nudge_count: 0,
                last_nudge_ms: 0,
            };
            evs.push(Event::Sent { msg });
            evs.push(Event::TaskUpdated {
                task: TaskRec {
                    last_heartbeat_sent_ms: now,
                    heartbeat_pending: true,
                    heartbeat_message_id: Some(sid.clone()),
                    ..task.clone()
                },
            });
            knocks.push((pane, sid, body));
        }

        // A non-empty board keeps an idle master from silently parking while
        // workers wait. The reminder is wake-only and never changes state.
        if !st.tasks.is_empty() {
            if let Some(master) = st.master_id() {
                if let Some(pane) = st.worker_pane(&master) {
                    let due = MASTER_HEARTBEATS
                        .get_or_init(|| Mutex::new(HashMap::new()))
                        .lock()
                        .unwrap()
                        .get(&master)
                        .copied()
                        .map(|last| now - last >= heartbeat_interval_ms(server))
                        .unwrap_or(true);
                    if due && idle_panes.contains(&pane) {
                        let body = "[COLLAB MASTER HEARTBEAT] task board is non-empty; run collab task status, collab who, and collab inbox; review every delivered/blocked/unfinished task, clean or reassign as needed, then dispatch available work. Stop only when all tasks are terminal and worktrees are clean.".to_string();
                        let (event, msg_id, message_body) = sent_system(&master, body);
                        evs.push(event);
                        knocks.push((pane, msg_id, message_body));
                        MASTER_HEARTBEATS
                            .get_or_init(|| Mutex::new(HashMap::new()))
                            .lock()
                            .unwrap()
                            .insert(master, now);
                    }
                }
            }
        }

        // Foundational terminal probe: capture each registered tmux pane every
        // daemon tick (5s). Three unchanged rendered samples indicate a likely
        // frozen TUI; the alert is durable and wake-only.
        for (pane, status) in &probe_alerts {
            let Some(master) = st.master_id() else {
                continue;
            };
            let body = format!(
                "TMUX_STATUS pane={} status={} | inspect the affected task and pane now; recover or reassign through Collab if work stopped, otherwise continue the next task step",
                pane, status
            );
            let (event, msg_id, message_body) = sent_system(&master, body);
            evs.push(event);
            if let Some(master_pane) = st.worker_pane(&master) {
                if !idle_panes.contains(&master_pane) {
                    continue;
                }
                knocks.push((master_pane, msg_id, message_body));
            }
            super::record_activity(
                &server.root,
                "tmux_probe",
                serde_json::json!({"pane": pane, "status": status}),
            );
        }

        // 1) durable task-delivery notifications are retried until the
        // master acknowledges them. A transient dead/stale pane must not
        // sever deliver -> master review.
        for m in st.msgs.values() {
            if m.from != "collab-server"
                || m.mtype != "system"
                || !m.body.starts_with("TASK_DELIVERED ")
                || m.state != "pending"
                || (if m.last_nudge_ms == 0 {
                    now - m.created_ms
                } else {
                    now - m.last_nudge_ms
                }) < DELIVERY_RETRY_INTERVAL_MS
            {
                continue;
            }
            let Some(pane) = st.worker_pane(&m.to) else {
                continue;
            };
            if !idle_panes.contains(&pane) {
                continue;
            }
            knocks.push((pane, m.id.clone(), m.body.clone()));
            evs.push(Event::Nudged {
                msg_id: m.id.clone(),
            });
        }

        // 2) unanswered request nudges: 5min each, escalate after MAX_NUDGES
        for m in st.msgs.values() {
            if m.mtype != "request" || m.state == "read" || st.answered(&m.id) {
                continue;
            }
            let due = m.nudge_count < MAX_NUDGES
                && now - m.created_ms >= NUDGE_INTERVAL_MS * (m.nudge_count as i64 + 1);
            if !due {
                continue;
            }
            let k = m.nudge_count + 1;
            // The recipient gets exactly one knock per request; later ticks do
            // not repeat the same wake-up while the request remains pending.
            if k == 1 {
                let (event, msg_id, message_body) = sent_system(&m.to, nudge_body(&m.id, &m.from));
                evs.push(event);
                if let Some(pane) = st.worker_pane(&m.to) {
                    if idle_panes.contains(&pane) {
                        knocks.push((pane, msg_id, message_body));
                    }
                }
            }
            if k >= MAX_NUDGES {
                let sender_body = format!(
                    "ESCALATE: request '{}' to {} has no reply after {} nudges; stop waiting and escalate per protocol",
                    m.id, m.to, MAX_NUDGES
                );
                let (event, msg_id, message_body) = sent_system(&m.from, sender_body);
                evs.push(event);
                if let Some(pane) = st.worker_pane(&m.from) {
                    if idle_panes.contains(&pane) {
                        knocks.push((pane, msg_id, message_body));
                    }
                }
            }
            evs.push(Event::Nudged {
                msg_id: m.id.clone(),
            });
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
                        && st.delivery_modes.get(&m.id).map(String::as_str) == Some("idle")
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
            knocks.push((pane.to_string(), ids.join(","), prompt));
            evs.push(Event::Delivered { ids });
        }
    }

    if evs.is_empty() {
        return;
    }
    server.commit(&evs);
    for (pane, msg_id, body) in knocks {
        if msg_id.contains(',') {
            let ids = msg_id.split(',').map(str::to_owned).collect::<Vec<_>>();
            super::queue_batch_knock(server, &pane, &ids, &body);
        } else {
            super::queue_system_knock(server, &pane, &msg_id, &body);
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
                nudge_count: 0,
                last_nudge_ms: 0,
            },
        },
        id,
        body,
    )
}

fn nudge_body(msg_id: &str, from: &str) -> String {
    format!(
        "NUDGE: inspect the unanswered request '{}' from {} now; execute the required Collab action (HOLD/YIELD/SPLIT or substantive reply) and continue the task. Do not answer without taking action.",
        msg_id, from
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::server::state::{State, WorkerRec};
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

    fn register(server: &Server, id: &str, role: &str) {
        server.commit(&[Event::Registered {
            worker: WorkerRec {
                id: id.into(),
                token: format!("token-{id}"),
                pane: Some(format!("%test-{id}")),
                cwd: "/tmp".into(),
                registered_ms: now_ms(),
                role: role.into(),
            },
        }]);
    }

    fn task(server: &Server, id: &str, owner: &str, status: &str, last_heartbeat: i64) {
        let now = now_ms();
        server.commit(&[Event::TaskCreated {
            task: TaskRec {
                id: id.into(),
                owner: owner.into(),
                created_by: "master".into(),
                feature_id: Some("feature".into()),
                worktree_path: Some("/tmp/wt".into()),
                branch: None,
                base_commit: None,
                priority: "p2".into(),
                status: status.into(),
                next_step: Some("continue".into()),
                created_ms: now,
                updated_ms: now,
                last_heartbeat_sent_ms: last_heartbeat,
                heartbeat_pending: false,
                heartbeat_message_id: None,
                heartbeat_stale_notified: false,
                goal_prompt: None,
                goal_busy: false,
                wait: None,
            },
        }]);
    }

    #[test]
    fn task_heartbeat_sends_once_until_acked() {
        let (server, root) = test_server();
        register(&server, "master", "master");
        register(&server, "owner", "worker");
        task(
            &server,
            "t1",
            "owner",
            "working",
            now_ms() - heartbeat_interval_ms(&server) - 1000,
        );

        tick(&server);
        {
            let st = server.state.lock().unwrap();
            assert!(st.msgs.len() >= 1);
            assert_eq!(st.tasks["t1"].heartbeat_pending, true);
            let hb_id = st.tasks["t1"].heartbeat_message_id.clone().unwrap();
            assert!(st.msgs[&hb_id].body.starts_with("[COLLAB HEARTBEAT]"));
        }

        tick(&server);
        {
            let st = server.state.lock().unwrap();
            let worker_hb = st
                .msgs
                .values()
                .filter(|m| m.body.starts_with("[COLLAB HEARTBEAT]"))
                .count();
            assert_eq!(worker_hb, 1, "pending heartbeat must not be resent");
        }
        std::fs::remove_dir_all(root).ok();
    }

    #[test]
    fn delivered_task_stops_heartbeat() {
        let (server, root) = test_server();
        register(&server, "master", "master");
        register(&server, "owner", "worker");
        task(&server, "t1", "owner", "delivered", 0);

        tick(&server);
        let st = server.state.lock().unwrap();
        assert!(st
            .msgs
            .values()
            .all(|message| !message.body.starts_with("[COLLAB HEARTBEAT]")));
        std::fs::remove_dir_all(root).ok();
    }

    #[test]
    fn heartbeat_reads_project_config_from_server_root() {
        let (server, root) = test_server();
        config::save(
            &root,
            &config::Config {
                heartbeat_minutes: 2,
            },
        )
        .unwrap();

        assert_eq!(heartbeat_interval_ms(&server), 2 * 60 * 1000);
        std::fs::remove_dir_all(root).ok();
    }
}
