use crate::config;
use crate::server::state::{now_ms, task_heartbeat_active, Event, Message, TaskRec};
use crate::server::Server;
use crate::server::ROOT;
use std::sync::Arc;

const NUDGE_INTERVAL_MS: i64 = 5 * 60 * 1000;
const MAX_NUDGES: u32 = 3;

fn heartbeat_interval_ms(_server: &Server) -> i64 {
    config::load_or_default(&ROOT.with(|root| root.borrow().clone()))
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
        "[COLLAB HEARTBEAT] from=collab-server master={} task={} owner={} status={} pane={} next={} | You have an active claim. Run collab role and collab task status {}; report to master only for a state change, blocker, ETA change, decision, verification, or handoff; otherwise continue the task next step immediately. Do not wait for another message.",
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

    {
        let st = server.state.lock().unwrap();

        // 0) task-scoped heartbeat: only active tasks, one pending per task
        for task in st.tasks.values() {
            if !task_heartbeat_active(&task.status) || task.heartbeat_pending {
                continue;
            }
            if now - task.last_heartbeat_sent_ms < heartbeat_interval_ms(server) {
                continue;
            }
            let pane = st.worker_pane(&task.owner);
            let body = heartbeat_body(
                &task.id,
                &task.owner,
                st.master_id().as_deref(),
                &task.status,
                task.next_step.as_deref(),
                pane.as_deref(),
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
            if let Some(p) = pane {
                knocks.push((p, sid, body));
            }
        }

        // 1) unanswered request nudges: 5min each, escalate after MAX_NUDGES
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
                    knocks.push((pane, msg_id, message_body));
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
                    knocks.push((pane, msg_id, message_body));
                }
            }
            evs.push(Event::Nudged {
                msg_id: m.id.clone(),
            });
        }
    }

    if evs.is_empty() {
        return;
    }
    server.commit(&evs);
    for (pane, msg_id, body) in knocks {
        super::queue_system_knock(server, &pane, &msg_id, &body);
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
        "NUDGE: you have an unanswered request '{}' from {}; respond per protocol (HOLD/YIELD/SPLIT or REPLY)",
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
            }),
            root,
        )
    }

    fn register(server: &Server, id: &str, role: &str) {
        server.commit(&[Event::Registered {
            worker: WorkerRec {
                id: id.into(),
                token: format!("token-{id}"),
                pane: None,
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
            assert_eq!(st.msgs.len(), 1);
            assert_eq!(st.tasks["t1"].heartbeat_pending, true);
            let hb_id = st.tasks["t1"].heartbeat_message_id.clone().unwrap();
            assert!(st.msgs[&hb_id].body.starts_with("[COLLAB HEARTBEAT]"));
        }

        tick(&server);
        {
            let st = server.state.lock().unwrap();
            assert_eq!(st.msgs.len(), 1, "pending heartbeat must not be resent");
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
        assert!(st.msgs.is_empty());
        std::fs::remove_dir_all(root).ok();
    }
}
