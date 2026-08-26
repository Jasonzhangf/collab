pub mod knock;
pub mod state;
pub mod timers;

use crate::proto::{Req, Resp, MSG_TYPES};
use crate::scope::Scope;
use crate::server::knock::{append_log, knock_or_log, pane_alive};
use serde_json::json;
use state::{
    default_role, now_ms, task_heartbeat_active, task_resource_active, Event, Message, State,
    TaskRec, WorkerRec,
};
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};
use tokio::net::UnixListener;

const MAX_POLL_MS: u64 = 3_600_000;
const POLL_TICK_MS: u64 = 250;
const TASK_STATUSES: [&str; 9] = [
    "available",
    "working",
    "verifying",
    "reviewed",
    "delivered",
    "rework",
    "merged",
    "closed",
    "cancelled",
];

fn priority_rank(priority: &str) -> u8 {
    match priority {
        "p0" => 0,
        "p1" => 1,
        "p2" => 2,
        "p3" => 3,
        _ => 4,
    }
}

pub struct Server {
    pub root: PathBuf,
    pub state: Mutex<State>,
    pub journal: Mutex<std::fs::File>,
    pub pane_alive_check: fn(&str) -> bool,
}

impl Server {
    pub fn log_path(&self) -> PathBuf {
        self.root
            .join(".agent-collab")
            .join("server")
            .join("log.txt")
    }

    /// Apply events to memory and persist them atomically-ordered in the journal.
    fn commit(&self, evs: &[Event]) {
        let mut st = self.state.lock().unwrap();
        self.commit_locked(&mut st, evs);
    }

    fn commit_locked(&self, st: &mut State, evs: &[Event]) {
        let mut j = self.journal.lock().unwrap();
        use std::io::Write;
        for ev in evs {
            st.apply(ev);
            let line = serde_json::to_string(ev).expect("serialize event");
            let _ = writeln!(j, "{}", line);
            if let Event::Sent { msg } = ev {
                self.backup_message(msg);
            }
            if let Event::Delivered { ids } = ev {
                for id in ids {
                    if let Some(msg) = st.msgs.get(id) {
                        self.backup_message(msg);
                    }
                }
            }
            if let Event::Acked { ids } = ev {
                for id in ids {
                    if let Some(msg) = st.msgs.get(id) {
                        self.backup_message(msg);
                    }
                }
            }
        }
        let _ = j.flush();
    }

    fn backup_message(&self, msg: &Message) {
        let dir = self.root.join(".agent-collab").join("mailbox");
        let _ = std::fs::create_dir_all(&dir);
        let path = dir.join(format!("{}.json", msg.id));
        if let Ok(data) = serde_json::to_string_pretty(msg) {
            let _ = std::fs::write(&path, data);
        }
    }
}

pub fn gen_msg_id() -> String {
    use std::sync::atomic::{AtomicU64, Ordering};
    static SEQ: AtomicU64 = AtomicU64::new(1);
    let n = SEQ.fetch_add(1, Ordering::Relaxed);
    format!("m{}-{}", now_ms(), n)
}

const LONG_BODY_THRESHOLD_CHARS: usize = 500;

fn message_doc_path(root: &Path, msg_id: &str) -> PathBuf {
    root.join(".agent-collab")
        .join("messages")
        .join(format!("{msg_id}.md"))
}

fn write_message_doc(root: &Path, msg_id: &str, body: &str) -> anyhow::Result<PathBuf> {
    let path = message_doc_path(root, msg_id);
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    std::fs::write(&path, body)?;
    Ok(path)
}

fn one_line(text: &str) -> String {
    text.chars().filter(|c| !c.is_control()).collect()
}

fn delivery_text(
    root: &Path,
    from: &str,
    mtype: &str,
    msg_id: &str,
    body: &str,
) -> Result<String, String> {
    let prefix = format!("[MAIL] from={} type={} id={}:", from, mtype, msg_id);
    let next = format!(
        "next=\"Process this collab input now: read the mailbox body, decide collaborate/defer/reject with reason, reply if requested, ack id={}, then immediately continue the current run's next step.\"",
        msg_id
    );
    if body.chars().count() <= LONG_BODY_THRESHOLD_CHARS {
        return Ok(format!("{prefix} {} | {next}", one_line(body)));
    }
    let path = write_message_doc(root, msg_id, body)
        .map_err(|e| format!("cannot store long message {}: {e}", msg_id))?;
    let relative = path
        .strip_prefix(root)
        .map_err(|e| format!("cannot make message reference relative: {e}"))?;
    Ok(format!(
        "{prefix} body-ref={} {next}",
        relative.display(),
        next = next,
    ))
}

pub(super) fn queue_system_knock(server: &Server, pane: &str, msg_id: &str, body: &str) {
    match delivery_text(&server.root, "collab-server", "system", msg_id, body) {
        Ok(text) => knock_or_log(&server.log_path(), pane, &text),
        Err(error) => append_log(
            &server.log_path(),
            &format!("system delivery prompt failed id={} err={}", msg_id, error),
        ),
    }
}

fn timeout_prompt(kind: &str, worker_id: &str, timeout_ms: u64) -> String {
    format!(
        "Blocking collab {} returned without a message for worker {} after {}ms. Continue the current run from its notes/actor next step; do not idle or wait for another request.",
        kind, worker_id, timeout_ms
    )
}

fn notify_wait_timeout(server: &Server, kind: &str, worker_id: &str, timeout_ms: u64) {
    let body = timeout_prompt(kind, worker_id, timeout_ms);
    let id = gen_msg_id();
    let msg = Message {
        id: id.clone(),
        from: "collab-server".into(),
        to: worker_id.into(),
        mtype: "system".into(),
        body: body.clone(),
        in_reply_to: None,
        created_ms: now_ms(),
        state: "pending".into(),
        nudge_count: 0,
        last_nudge_ms: 0,
    };
    let pane = server.state.lock().unwrap().worker_pane(worker_id);
    server.commit(&[Event::Sent { msg }]);
    if let Some(pane) = pane {
        queue_system_knock(server, &pane, &id, &body);
    }
}

fn iso(ms: i64) -> String {
    chrono::DateTime::from_timestamp_millis(ms)
        .map(|d| d.to_rfc3339())
        .unwrap_or_default()
}

// ---------- handlers ----------

fn verify(state: &State, worker_id: &str, token: &str) -> Result<WorkerRec, Resp> {
    match state.workers.get(worker_id) {
        Some(w) if w.token == token => Ok(w.clone()),
        Some(_) => Err(Resp::err(
            "token mismatch: identity does not own this worker_id",
        )),
        None => Err(Resp::err(format!("worker {} not registered", worker_id))),
    }
}

fn handle_register(
    server: &Server,
    worker_id: String,
    token: String,
    pane: Option<String>,
    cwd: String,
) -> Resp {
    let mut st = server.state.lock().unwrap();
    if let Some(existing) = st.workers.get(&worker_id).cloned() {
        if existing.token != token {
            return Resp::err(format!(
                "worker_id {} already registered by another token",
                worker_id
            ));
        }
        let role = existing.role.clone();
        let refreshed = WorkerRec {
            id: worker_id.clone(),
            token: existing.token.clone(),
            pane: pane.or_else(|| existing.pane.clone()),
            cwd,
            registered_ms: existing.registered_ms,
            role: role.clone(),
        };
        server.commit_locked(&mut st, &[Event::Registered { worker: refreshed }]);
        return Resp::data(json!({"worker_id": worker_id, "role": role, "reused": true}));
    }
    let role = if st.has_master() {
        default_role()
    } else {
        "master".into()
    };
    let rec = WorkerRec {
        id: worker_id.clone(),
        token,
        pane,
        cwd,
        registered_ms: now_ms(),
        role,
    };
    server.commit_locked(
        &mut st,
        &[Event::Registered {
            worker: rec.clone(),
        }],
    );
    Resp::data(json!({
        "worker_id": worker_id,
        "role": rec.role,
        "registered_at": iso(rec.registered_ms),
        "role_decision": if rec.role == "master" { "first-registered-worker-default-master" } else { "master-exists-default-worker" }
    }))
}

fn handle_send(
    server: &Server,
    from: String,
    to: String,
    mtype: String,
    body: String,
    in_reply_to: Option<String>,
) -> Resp {
    if !MSG_TYPES.contains(&mtype.as_str()) {
        return Resp::err(format!(
            "invalid type {}; must be one of {:?}",
            mtype, MSG_TYPES
        ));
    }
    let mut st = server.state.lock().unwrap();
    if !st.workers.contains_key(&to) {
        return Resp::err(format!("recipient {} not registered", to));
    }
    if let Some(ref rid) = in_reply_to {
        if !st.msgs.contains_key(rid) {
            return Resp::err(format!("in_reply_to message {} not found", rid));
        }
    }
    if mtype == "request" {
        if let Some((existing_id, existing)) = st.recent_live_request(&from, &to, now_ms()) {
            let retry_at = iso(existing.created_ms + state::REQUEST_COOLDOWN_MS);
            return Resp::err(format!(
                "request cooldown active: existing_request_id={}, retry_at={}",
                existing_id, retry_at
            ));
        }
    }
    let superseded_ids = match (mtype.as_str(), in_reply_to.as_deref()) {
        ("reply", Some(request_id)) => st.superseded_replies(request_id),
        _ => Vec::new(),
    };
    let msg = Message {
        id: gen_msg_id(),
        from: from.clone(),
        to: to.clone(),
        mtype: mtype.clone(),
        body,
        in_reply_to,
        created_ms: now_ms(),
        state: "pending".into(),
        nudge_count: 0,
        last_nudge_ms: 0,
    };
    let mid = msg.id.clone();
    let delivery = delivery_text(&server.root, &from, &mtype, &mid, &msg.body);
    let delivery = match delivery {
        Ok(text) => text,
        Err(e) => return Resp::err(e),
    };
    let mut events = vec![Event::Sent { msg }];
    if !superseded_ids.is_empty() {
        events.push(Event::Superseded {
            ids: superseded_ids,
        });
    }
    let pane = st.worker_pane(&to);
    server.commit_locked(&mut st, &events);
    drop(st);
    if let Some(p) = pane {
        knock_or_log(&server.log_path(), &p, &delivery);
    }
    Resp::data(json!({"msg_id": mid}))
}

#[cfg(test)]
fn handle_task_register(
    server: &Server,
    worker_id: String,
    token: String,
    task_id: String,
    owner: Option<String>,
    feature_id: Option<String>,
    worktree_path: Option<String>,
    branch: Option<String>,
    base_commit: Option<String>,
    priority: String,
) -> Resp {
    handle_task_register_with_next(
        server,
        worker_id,
        token,
        task_id,
        owner,
        feature_id,
        worktree_path,
        branch,
        base_commit,
        priority,
        None,
    )
}

fn handle_task_register_with_next(
    server: &Server,
    worker_id: String,
    token: String,
    task_id: String,
    owner: Option<String>,
    feature_id: Option<String>,
    worktree_path: Option<String>,
    branch: Option<String>,
    base_commit: Option<String>,
    priority: String,
    next_step: Option<String>,
) -> Resp {
    let mut st = server.state.lock().unwrap();
    let Some(worker) = st.workers.get(&worker_id).cloned() else {
        return Resp::err(format!("worker {} not registered", worker_id));
    };
    if worker.token != token {
        return Resp::err("token mismatch: identity does not own this worker_id");
    }
    if st.tasks.contains_key(&task_id) {
        return Resp::err(format!("task {} already registered", task_id));
    }
    if worker.role != "master" {
        return Resp::err("only master may register tasks; claim available tasks instead");
    }
    if !matches!(priority.as_str(), "p0" | "p1" | "p2" | "p3" | "p4") {
        return Resp::err(format!(
            "invalid priority {}; must be p0, p1, p2, p3, or p4",
            priority
        ));
    }
    let is_available = owner.is_none();
    let task_owner = owner.unwrap_or_else(|| worker_id.clone());
    if !st.workers.contains_key(&task_owner) {
        return Resp::err(format!("task owner {} not registered", task_owner));
    }
    if let Some(existing) = st.tasks.values().find(|task| {
        task_resource_active(&task.status)
            && (feature_id.is_some() && task.feature_id == feature_id
                || worktree_path.is_some() && task.worktree_path == worktree_path)
    }) {
        return Resp::err(format!("task resource conflict with {}", existing.id));
    }
    let now = now_ms();
    let task = TaskRec {
        id: task_id.clone(),
        owner: task_owner,
        created_by: worker_id,
        feature_id,
        worktree_path,
        branch,
        base_commit,
        priority,
        status: if is_available {
            "available".to_string()
        } else {
            "working".to_string()
        },
        next_step,
        created_ms: now,
        updated_ms: now,
        last_heartbeat_sent_ms: now,
        heartbeat_pending: false,
        heartbeat_message_id: None,
        heartbeat_stale_notified: false,
    };
    server.commit_locked(&mut st, &[Event::TaskCreated { task: task.clone() }]);
    Resp::data(
        json!({"task": task.id, "owner": task.owner, "status": task.status, "heartbeat": "active"}),
    )
}

fn handle_task_dispatch(server: &Server, worker_id: String, token: String) -> Resp {
    let mut st = server.state.lock().unwrap();
    let Some(master) = st.workers.get(&worker_id).cloned() else {
        return Resp::err(format!("worker {} not registered", worker_id));
    };
    if master.token != token {
        return Resp::err("token mismatch: identity does not own this worker_id");
    }
    if master.role != "master" {
        return Resp::err("only master may dispatch available tasks");
    }
    let (dispatch_events, dispatch_knocks, dispatched) =
        dispatch_available_to_idle(&mut st, &server.pane_alive_check);
    if !dispatch_events.is_empty() {
        server.commit_locked(&mut st, &dispatch_events);
    }
    let available_tasks = available_task_views(&st);
    drop(st);
    for (pane, prompt_id, prompt) in dispatch_knocks {
        queue_system_knock(server, &pane, &prompt_id, &prompt);
    }
    Resp::data(json!({
        "dispatched_tasks": dispatched,
        "available_tasks": available_tasks,
    }))
}

fn handle_task_claim(server: &Server, worker_id: String, token: String, task_id: String) -> Resp {
    let mut st = server.state.lock().unwrap();
    let Some(worker) = st.workers.get(&worker_id).cloned() else {
        return Resp::err(format!("worker {} not registered", worker_id));
    };
    if worker.token != token {
        return Resp::err("token mismatch: identity does not own this worker_id");
    }
    if worker.role != "worker" {
        return Resp::err("only workers may claim available tasks");
    }
    let Some(mut task) = st.tasks.get(&task_id).cloned() else {
        return Resp::err(format!("task {} not found", task_id));
    };
    if task.status != "available" {
        return Resp::err(format!(
            "task {} is not available (current status: {})",
            task_id, task.status
        ));
    }
    if let Some(existing) = st
        .tasks
        .values()
        .find(|t| t.id != task.id && t.owner == worker_id && task_heartbeat_active(&t.status))
    {
        return Resp::err(format!(
            "you already hold active task {}; deliver or await master close before claiming another",
            existing.id
        ));
    }
    task.owner = worker_id;
    task.status = "working".to_string();
    task.updated_ms = now_ms();
    task.last_heartbeat_sent_ms = now_ms();
    server.commit_locked(&mut st, &[Event::TaskUpdated { task: task.clone() }]);
    Resp::data(json!({
        "task": task.id,
        "owner": task.owner,
        "status": task.status
    }))
}

fn handle_task_update(
    server: &Server,
    worker_id: String,
    token: String,
    task_id: String,
    status: Option<String>,
    next_step: Option<String>,
) -> Resp {
    let mut st = server.state.lock().unwrap();
    let Some(worker) = st.workers.get(&worker_id).cloned() else {
        return Resp::err(format!("worker {} not registered", worker_id));
    };
    if worker.token != token {
        return Resp::err("token mismatch: identity does not own this worker_id");
    }
    let Some(mut task) = st.tasks.get(&task_id).cloned() else {
        return Resp::err(format!("task {} not found", task_id));
    };
    if task.owner != worker_id && worker.role != "master" {
        return Resp::err("only task owner or master may update this task");
    }
    if let Some(new_status) = status {
        if !TASK_STATUSES.contains(&new_status.as_str()) {
            return Resp::err(format!(
                "invalid status {}; must be one of {:?}",
                new_status, TASK_STATUSES
            ));
        }
        if matches!(new_status.as_str(), "merged" | "cancelled") && worker.role != "master" {
            return Resp::err("only master may merge or cancel a task");
        }
        if new_status == "closed" && worker.role != "master" {
            return Resp::err(
                "only master may close; use collab task close after merge and cleanup",
            );
        }
        if new_status == "rework" {
            return Resp::err(
                "rework must be requested by master message; use working after receiving it",
            );
        }
        task.status = new_status;
    }
    if next_step.is_some() {
        task.next_step = next_step;
    }
    task.heartbeat_pending = false;
    task.heartbeat_message_id = None;
    task.updated_ms = now_ms();
    if task_heartbeat_active(&task.status) {
        task.heartbeat_stale_notified = false;
    } else {
        task.heartbeat_pending = false;
    }
    server.commit_locked(&mut st, &[Event::TaskUpdated { task: task.clone() }]);
    Resp::data(
        json!({"task": task.id, "status": task.status, "heartbeat": if task_heartbeat_active(&task.status) { "active" } else { "unregistered" }}),
    )
}

fn available_task_views(st: &State) -> Vec<serde_json::Value> {
    let mut tasks: Vec<&TaskRec> = st
        .tasks
        .values()
        .filter(|task| task.status == "available")
        .collect();
    tasks.sort_by(|a, b| {
        (priority_rank(&a.priority), a.created_ms, a.id.as_str()).cmp(&(
            priority_rank(&b.priority),
            b.created_ms,
            b.id.as_str(),
        ))
    });
    tasks
        .into_iter()
        .map(|task| {
            json!({
                "id": task.id,
                "status": task.status,
                "owner": task.owner,
                "feature_id": task.feature_id,
                "priority": task.priority,
                "branch": task.branch,
            })
        })
        .collect()
}

fn idle_worker_ids(st: &State) -> Vec<String> {
    let mut workers: Vec<&WorkerRec> = st
        .workers
        .values()
        .filter(|w| {
            w.role == "worker"
                && w.pane.is_some()
                && !st
                    .tasks
                    .values()
                    .any(|task| task.owner == w.id && task_heartbeat_active(&task.status))
        })
        .collect();
    workers.sort_by(|a, b| a.id.cmp(&b.id));
    workers
        .into_iter()
        .map(|worker| worker.id.clone())
        .collect()
}

fn assignment_body(task: &TaskRec, master_id: Option<&str>) -> String {
    format!(
        "TASK_ASSIGNED id={} owner={} feature={} worktree={} branch={} base={} priority={} master={} next={} | Process this collab input now: run collab role and collab task status {}; claim is already active, so read the task scope and immediately continue its next step. Do not ask for approval or send an ACK.",
        task.id,
        task.owner,
        task.feature_id.as_deref().unwrap_or("none"),
        task.worktree_path.as_deref().unwrap_or("none"),
        task.branch.as_deref().unwrap_or("none"),
        task.base_commit.as_deref().unwrap_or("none"),
        task.priority,
        master_id.unwrap_or("unassigned"),
        task.next_step.as_deref().unwrap_or("inspect task status; continue the first safe implementation step"),
        task.id,
    )
}

fn dispatch_available_to_idle(
    st: &mut State,
    is_reachable: &dyn Fn(&str) -> bool,
) -> (
    Vec<Event>,
    Vec<(String, String, String)>,
    Vec<serde_json::Value>,
) {
    let mut available: Vec<TaskRec> = st
        .tasks
        .values()
        .filter(|task| task.status == "available")
        .cloned()
        .collect();
    available.sort_by(|a, b| {
        (priority_rank(&a.priority), a.created_ms, a.id.as_str()).cmp(&(
            priority_rank(&b.priority),
            b.created_ms,
            b.id.as_str(),
        ))
    });

    let mut events = Vec::new();
    let mut knocks = Vec::new();
    let mut assignments = Vec::new();
    let mut idle: Vec<String> = idle_worker_ids(st)
        .into_iter()
        .filter(|worker_id| {
            st.worker_pane(worker_id)
                .as_deref()
                .is_some_and(|pane| is_reachable(pane))
        })
        .collect();
    let master = st.master_id();

    for mut task in available {
        if idle.is_empty() {
            break;
        }
        let worker_id = idle.remove(0);
        let Some(pane) = st.worker_pane(&worker_id) else {
            continue;
        };
        let body = assignment_body(&task, master.as_deref());
        let mid = gen_msg_id();
        task.owner = worker_id.clone();
        task.status = "working".to_string();
        task.updated_ms = now_ms();
        task.last_heartbeat_sent_ms = now_ms();

        let message = Message {
            id: mid.clone(),
            from: "collab-server".into(),
            to: worker_id.clone(),
            mtype: "system".to_string(),
            body: body.clone(),
            in_reply_to: None,
            created_ms: now_ms(),
            state: "pending".into(),
            nudge_count: 0,
            last_nudge_ms: 0,
        };
        events.push(Event::Sent { msg: message });
        events.push(Event::TaskUpdated { task: task.clone() });
        assignments.push(json!({
            "task": task.id,
            "owner": task.owner,
            "status": task.status,
            "message": mid,
        }));
        knocks.push((pane, mid, body));
    }
    (events, knocks, assignments)
}

fn close_task_resources(
    root: &Path,
    worktree_path: Option<&str>,
    branch: Option<&str>,
) -> Result<(), String> {
    if let Some(branch) = branch {
        let merged = Command::new("git")
            .current_dir(root)
            .args(["merge-base", "--is-ancestor", branch, "HEAD"])
            .output()
            .map_err(|e| format!("cannot verify branch {branch}: {e}"))?;
        if !merged.status.success() {
            return Err(format!(
                "branch {branch} is not merged into HEAD; refusing delete"
            ));
        }
    }
    if let Some(relative) = worktree_path {
        let worktree = root.join(relative);
        let allowed_root = root
            .canonicalize()
            .unwrap_or_else(|_| root.to_path_buf())
            .join("playground");
        let canonical_worktree = worktree
            .canonicalize()
            .map_err(|e| format!("declared worktree {} is missing: {e}", relative))?;
        if !canonical_worktree.starts_with(allowed_root) {
            return Err(format!("refusing cleanup outside playground: {}", relative));
        }
        let dirty = Command::new("git")
            .arg("-C")
            .arg(&worktree)
            .args(["status", "--porcelain"])
            .output()
            .map_err(|e| format!("cannot inspect worktree {}: {e}", relative))?;
        if !dirty.status.success() {
            return Err(format!(
                "cannot verify clean worktree {}: {}",
                relative,
                String::from_utf8_lossy(&dirty.stderr).trim()
            ));
        }
        if !dirty.stdout.is_empty() {
            return Err(format!("worktree {} has uncommitted changes", relative));
        }

        let removed = Command::new("git")
            .current_dir(root)
            .args(["worktree", "remove", &worktree.display().to_string()])
            .output()
            .map_err(|e| format!("cannot remove worktree {}: {e}", relative))?;
        if !removed.status.success() {
            return Err(format!(
                "worktree cleanup failed for {}: {}",
                relative,
                String::from_utf8_lossy(&removed.stderr).trim()
            ));
        }
    }

    if let Some(branch) = branch {
        let deleted = Command::new("git")
            .current_dir(root)
            .args(["branch", "-d", branch])
            .output()
            .map_err(|e| format!("cannot delete branch {branch}: {e}"))?;
        if !deleted.status.success() {
            return Err(format!(
                "branch cleanup failed for {branch}: {}",
                String::from_utf8_lossy(&deleted.stderr).trim()
            ));
        }
    }
    Ok(())
}

fn handle_task_deliver(
    server: &Server,
    worker_id: String,
    token: String,
    task_id: String,
    evidence: Option<String>,
) -> Resp {
    let mut st = server.state.lock().unwrap();
    let Some(worker) = st.workers.get(&worker_id).cloned() else {
        return Resp::err(format!("worker {} not registered", worker_id));
    };
    if worker.token != token {
        return Resp::err("token mismatch: identity does not own this worker_id");
    }
    if worker.role != "worker" {
        return Resp::err("only workers may deliver claimed tasks");
    }
    let Some(mut task) = st.tasks.get(&task_id).cloned() else {
        return Resp::err(format!("task {} not found", task_id));
    };
    if task.owner != worker_id || !task_heartbeat_active(&task.status) {
        return Resp::err(format!(
            "task {} is not an active claim owned by {}",
            task_id, worker_id
        ));
    }

    let evidence = evidence.unwrap_or_else(|| "not provided".to_string());
    let master = st.master_id().unwrap_or_else(|| "unassigned".to_string());
    let body = format!(
        "TASK_DELIVERED id={} status=delivered owner={} feature={} branch={} evidence={}",
        task.id,
        task.owner,
        task.feature_id.as_deref().unwrap_or("none"),
        task.branch.as_deref().unwrap_or("none"),
        evidence
    );
    let mid = gen_msg_id();
    let message = Message {
        id: mid.clone(),
        from: "collab-server".into(),
        to: master.clone(),
        mtype: "system".to_string(),
        body: body.clone(),
        in_reply_to: None,
        created_ms: now_ms(),
        state: "pending".into(),
        nudge_count: 0,
        last_nudge_ms: 0,
    };

    task.status = "delivered".to_string();
    task.next_step = Some("master review and merge".to_string());
    task.heartbeat_pending = false;
    task.heartbeat_message_id = None;
    task.heartbeat_stale_notified = false;
    task.updated_ms = now_ms();
    server.commit_locked(
        &mut st,
        &[
            Event::Sent { msg: message },
            Event::TaskUpdated { task: task.clone() },
        ],
    );

    if let Some(pane) = st.worker_pane(&master) {
        queue_system_knock(server, &pane, &mid, &body);
    }

    Resp::data(json!({
        "delivered": task.id,
        "status": task.status,
        "master_notified": master,
        "next_action": "claim one available task immediately; do not report delivery again",
        "identity": {"worker_id": worker.id, "role": worker.role},
        "available_tasks": available_task_views(&st),
    }))
}

fn handle_task_close(server: &Server, worker_id: String, token: String, task_id: String) -> Resp {
    let mut st = server.state.lock().unwrap();
    let Some(master) = st.workers.get(&worker_id).cloned() else {
        return Resp::err(format!("worker {} not registered", worker_id));
    };
    if master.token != token {
        return Resp::err("token mismatch: identity does not own this worker_id");
    }
    if master.role != "master" {
        return Resp::err("only master may close delivered tasks");
    }
    let Some(task) = st.tasks.get(&task_id).cloned() else {
        return Resp::err(format!("task {} not found", task_id));
    };
    if task.owner == worker_id && task.status == "available" {
        return Resp::err("cancel an unclaimed task with task update --status cancelled");
    }
    if task.status != "delivered" && task.status != "merged" {
        return Resp::err(format!(
            "task {} must be delivered or merged before close (current: {})",
            task_id, task.status
        ));
    }
    if let Err(e) = close_task_resources(
        &server.root,
        task.worktree_path.as_deref(),
        task.branch.as_deref(),
    ) {
        return Resp::err(e);
    }
    let mut closed = task;
    closed.status = "closed".to_string();
    closed.next_step = Some("closed after master merge and cleanup".to_string());
    closed.heartbeat_pending = false;
    closed.heartbeat_message_id = None;
    closed.updated_ms = now_ms();
    server.commit_locked(
        &mut st,
        &[Event::TaskUpdated {
            task: closed.clone(),
        }],
    );

    let body = format!(
        "TASK_CLOSED id={} owner={} feature={} status=closed; claim the next available task or remain idle without heartbeat.",
        closed.id, closed.owner, closed.feature_id.as_deref().unwrap_or("none")
    );
    let mid = gen_msg_id();
    let message = Message {
        id: mid.clone(),
        from: "collab-server".into(),
        to: closed.owner.clone(),
        mtype: "system".to_string(),
        body: body.clone(),
        in_reply_to: None,
        created_ms: now_ms(),
        state: "pending".into(),
        nudge_count: 0,
        last_nudge_ms: 0,
    };
    server.commit_locked(&mut st, &[Event::Sent { msg: message }]);
    if let Some(pane) = st.worker_pane(&closed.owner) {
        queue_system_knock(server, &pane, &mid, &body);
    }

    // Merge closure frees the feature/worktree resource. Reconcile the board
    // immediately so an already-published task reaches an idle worker without
    // another master approval round.
    let (dispatch_events, dispatch_knocks, dispatched) =
        dispatch_available_to_idle(&mut st, &server.pane_alive_check);
    if !dispatch_events.is_empty() {
        server.commit_locked(&mut st, &dispatch_events);
    }
    let available_tasks = available_task_views(&st);
    drop(st);
    for (pane, prompt_id, prompt) in dispatch_knocks {
        queue_system_knock(server, &pane, &prompt_id, &prompt);
    }

    Resp::data(json!({
        "task": closed.id,
        "status": closed.status,
        "owner": closed.owner,
        "cleanup": {
            "worktree": closed.worktree_path,
            "branch": closed.branch,
            "result": "removed-if-declared-and-safe"
        },
        "dispatched_tasks": dispatched,
        "available_tasks": available_tasks,
    }))
}

fn task_view(task: &TaskRec) -> serde_json::Value {
    json!({
        "id": task.id,
        "owner": task.owner,
        "created_by": task.created_by,
        "feature_id": task.feature_id,
        "worktree": task.worktree_path,
        "branch": task.branch,
        "base_commit": task.base_commit,
        "priority": task.priority,
        "status": task.status,
        "next_step": task.next_step,
        "heartbeat": if task_heartbeat_active(&task.status) { "active" } else { "unregistered" },
        "updated_at": iso(task.updated_ms),
    })
}

fn handle_poll(server: &Server, worker_id: String, timeout_ms: u64) -> Resp {
    let timeout_ms = timeout_ms.min(MAX_POLL_MS);
    let deadline = Instant::now() + Duration::from_millis(timeout_ms);
    loop {
        let ids: Vec<String>;
        let msgs: Vec<Message>;
        {
            let st = server.state.lock().unwrap();
            let unread = st.inbox_of(&worker_id);
            if unread.is_empty() {
                ids = Vec::new();
                msgs = Vec::new();
            } else {
                ids = unread.iter().map(|m| m.id.clone()).collect();
                msgs = unread.into_iter().cloned().collect();
            }
        }
        if !ids.is_empty() {
            server.commit(&[Event::Delivered { ids }]);
            return Resp::data(json!({
                "messages": msgs,
                "count": msgs.len(),
                "fetched_at": iso(now_ms()),
            }));
        }
        if Instant::now() >= deadline {
            notify_wait_timeout(server, "wait", &worker_id, timeout_ms);
            return Resp::data(json!({"messages": [], "count": 0, "timeout": true}));
        }
        std::thread::sleep(Duration::from_millis(POLL_TICK_MS));
    }
}

fn task_conflicts(
    server: &Server,
    feature_id: Option<String>,
    worktree_path: Option<String>,
) -> Resp {
    let st = server.state.lock().unwrap();
    let conflicts: Vec<serde_json::Value> = st
        .tasks
        .values()
        .filter(|task| {
            task_resource_active(&task.status)
                && ((feature_id.is_some() && task.feature_id == feature_id)
                    || (worktree_path.is_some() && task.worktree_path == worktree_path))
        })
        .map(task_view)
        .collect();
    Resp::data(json!({"conflicts": conflicts}))
}

// ---------- dispatch ----------

fn dispatch(server: &Arc<Server>, req: Req) -> Resp {
    match req {
        Req::Register {
            worker_id,
            token,
            pane,
            cwd,
        } => handle_register(server, worker_id, token, pane, cwd),
        Req::Send {
            from,
            to,
            mtype,
            body,
            in_reply_to,
        } => handle_send(server, from, to, mtype, body, in_reply_to),
        Req::Poll {
            worker_id,
            token,
            timeout_ms,
        } => {
            let check = server.state.lock().unwrap();
            if let Err(e) = verify(&check, &worker_id, &token) {
                return e;
            }
            drop(check);
            handle_poll(server, worker_id, timeout_ms)
        }
        Req::Ack {
            worker_id,
            token,
            ids,
        } => {
            let st = server.state.lock().unwrap();
            if let Err(e) = verify(&st, &worker_id, &token) {
                return e;
            }
            let owned: Vec<String> = ids
                .into_iter()
                .filter(|id| st.msgs.get(id).map(|m| m.to == worker_id).unwrap_or(false))
                .collect();
            let heartbeat_task_updates: Vec<TaskRec> = owned
                .iter()
                .filter_map(|id| st.msgs.get(id))
                .filter(|m| m.from == "collab-server")
                .filter_map(|m| {
                    st.tasks.values().find(|t| {
                        t.owner == worker_id
                            && t.heartbeat_message_id.as_deref() == Some(m.id.as_str())
                    })
                })
                .map(|task| TaskRec {
                    heartbeat_pending: false,
                    heartbeat_message_id: None,
                    updated_ms: now_ms(),
                    ..task.clone()
                })
                .collect();
            drop(st);
            if owned.is_empty() {
                return Resp::err("no ackable messages (must address your own inbox)");
            }
            let mut events = vec![Event::Acked { ids: owned.clone() }];
            for task in heartbeat_task_updates {
                events.push(Event::TaskUpdated { task });
            }
            server.commit(&events);
            Resp::data(json!({"acked": owned}))
        }
        Req::Inbox { worker_id, token } => {
            let st = server.state.lock().unwrap();
            if let Err(e) = verify(&st, &worker_id, &token) {
                return e;
            }
            let inbox: Vec<&Message> = st.inbox_of(&worker_id);
            let items: Vec<serde_json::Value> = inbox
                .iter()
                .map(|m| {
                    json!({
                        "id": m.id, "from": m.from, "type": m.mtype,
                        "state": m.state, "created_at": iso(m.created_ms),
                        "body": m.body,
                    })
                })
                .collect();
            Resp::data(json!({"unread": items.len(), "messages": items}))
        }
        Req::MsgStatus { msg_id } => {
            let st = server.state.lock().unwrap();
            match st.msgs.get(&msg_id) {
                Some(m) => Resp::data(json!({
                    "id": m.id, "from": m.from, "to": m.to, "type": m.mtype,
                    "state": m.state, "nudges": m.nudge_count,
                    "created_at": iso(m.created_ms), "answered": st.answered(&msg_id),
                })),
                None => Resp::err(format!("message {} not found", msg_id)),
            }
        }
        Req::TaskRegister {
            worker_id,
            token,
            task_id,
            owner,
            feature_id,
            worktree_path,
            branch,
            base_commit,
            priority,
            next_step,
        } => handle_task_register_with_next(
            server,
            worker_id,
            token,
            task_id,
            owner,
            feature_id,
            worktree_path,
            branch,
            base_commit,
            priority,
            next_step,
        ),
        Req::TaskUpdate {
            worker_id,
            token,
            task_id,
            status,
            next_step,
        } => handle_task_update(server, worker_id, token, task_id, status, next_step),
        Req::TaskClaim {
            worker_id,
            token,
            task_id,
        } => handle_task_claim(server, worker_id, token, task_id),
        Req::TaskDeliver {
            worker_id,
            token,
            task_id,
            evidence,
        } => handle_task_deliver(server, worker_id, token, task_id, evidence),
        Req::TaskClose {
            worker_id,
            token,
            task_id,
        } => handle_task_close(server, worker_id, token, task_id),
        Req::TaskDispatch { worker_id, token } => handle_task_dispatch(server, worker_id, token),
        Req::TaskStatus { task_id } => {
            let st = server.state.lock().unwrap();
            match task_id {
                Some(id) => st
                    .tasks
                    .get(&id)
                    .map(task_view)
                    .map(Resp::data)
                    .unwrap_or_else(|| Resp::err(format!("task {} not found", id))),
                None => Resp::data(
                    json!({"tasks": st.tasks.values().map(task_view).collect::<Vec<_>>()}),
                ),
            }
        }
        Req::TaskConflicts {
            feature_id,
            worktree_path,
        } => task_conflicts(server, feature_id, worktree_path),
        Req::Role { worker_id } => {
            let st = server.state.lock().unwrap();
            match st.workers.get(&worker_id) {
                Some(w) => Resp::data(json!({"worker_id": worker_id, "role": w.role})),
                None => Resp::err(format!("worker {} not registered", worker_id)),
            }
        }
        Req::Workers => {
            let st = server.state.lock().unwrap();
            let workers: Vec<serde_json::Value> = st
                .workers
                .values()
                .map(|w| {
                    let active = st
                        .tasks
                        .values()
                        .find(|task| task.owner == w.id && task_heartbeat_active(&task.status));
                    json!({
                        "id": w.id,
                        "role": w.role,
                        "pane": w.pane,
                        "active_task": active.map(|task| task.id.as_str()),
                        "active_status": active.map(|task| task.status.as_str()),
                    })
                })
                .collect();
            Resp::data(json!({
                "workers": workers,
                "master": st.master_id(),
                "count": workers.len()
            }))
        }
        Req::MasterId => {
            let st = server.state.lock().unwrap();
            Resp::data(json!({"master": st.master_id()}))
        }
        Req::Ping => {
            let st = server.state.lock().unwrap();
            Resp::data(json!({
                "workers": st.workers.len(),
                "messages": st.msgs.len(),
                "tasks": st.tasks.len(),
                "now": iso(now_ms()),
            }))
        }
    }
}

async fn conn_task(server: Arc<Server>, stream: tokio::net::UnixStream) {
    use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
    let (reader, mut writer) = stream.into_split();
    let mut lines = BufReader::new(reader).lines();
    while let Ok(Some(line)) = lines.next_line().await {
        if line.trim().is_empty() {
            continue;
        }
        let resp = match serde_json::from_str::<Req>(&line) {
            Ok(req) => {
                // blocking handlers (poll/wait) run off the async reactor thread pool
                let srv = server.clone();
                tokio::task::spawn_blocking(move || dispatch(&srv, req))
                    .await
                    .unwrap_or_else(|e| Resp::err(format!("handler join error: {}", e)))
            }
            Err(e) => Resp::err(format!("bad request: {}", e)),
        };
        let mut out = serde_json::to_string(&resp).expect("serialize resp");
        out.push('\n');
        if writer.write_all(out.as_bytes()).await.is_err() {
            break;
        }
    }
}

fn replay(root: &Path) -> anyhow::Result<State> {
    let journal = root.join(".agent-collab/server/journal.jsonl");
    let mut st = State::default();
    if !journal.exists() {
        return Ok(st);
    }
    let content = std::fs::read_to_string(journal)?;
    for line in content.lines() {
        if line.trim().is_empty() {
            continue;
        }
        match serde_json::from_str::<Event>(line) {
            Ok(ev) => st.apply(&ev),
            Err(e) => append_log(
                &root.join(".agent-collab/server/log.txt"),
                &format!("journal replay skip: {}", e),
            ),
        }
    }
    Ok(st)
}

pub async fn run(scope: Scope) -> anyhow::Result<()> {
    let sock_path = scope.sock_path();
    let server_dir = scope.server_dir();
    std::fs::create_dir_all(&server_dir)?;

    if sock_path.exists() {
        if crate::client::alive(&sock_path) {
            anyhow::bail!("server already running at {}", sock_path.display());
        }
        std::fs::remove_file(&sock_path)?;
    }

    let journal_file = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(server_dir.join("journal.jsonl"))?;

    let state = replay(&scope.root)?;
    append_log(&server_dir.join("log.txt"), "server starting");

    let server = Arc::new(Server {
        root: scope.root.clone(),
        state: Mutex::new(state),
        journal: Mutex::new(journal_file),
        pane_alive_check: pane_alive,
    });
    let listener = UnixListener::bind(&sock_path)?;

    // background scheduler: task heartbeats + request escalation
    let sched = server.clone();
    tokio::spawn(async move {
        let mut interval = tokio::time::interval(Duration::from_secs(5));
        loop {
            interval.tick().await;
            crate::server::timers::tick(&sched);
        }
    });

    loop {
        match listener.accept().await {
            Ok((stream, _)) => {
                let srv = server.clone();
                tokio::spawn(conn_task(srv, stream));
            }
            Err(e) => append_log(&server_dir.join("log.txt"), &format!("accept error: {}", e)),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::server::state::default_priority;
    use std::sync::atomic::{AtomicU64, Ordering};

    fn test_server() -> (Server, PathBuf) {
        static SEQ: AtomicU64 = AtomicU64::new(0);
        let n = SEQ.fetch_add(1, Ordering::Relaxed);
        let root =
            std::env::temp_dir().join(format!("collab-send-filter-{}-{n}", std::process::id()));
        let server_dir = root.join(".agent-collab/server");
        std::fs::create_dir_all(&server_dir).unwrap();
        let journal = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(server_dir.join("journal.jsonl"))
            .unwrap();
        let server = Server {
            root: root.clone(),
            state: Mutex::new(State::default()),
            journal: Mutex::new(journal),
            pane_alive_check: |_| true,
        };
        (server, root)
    }

    fn register(server: &Server, id: &str) {
        register_with_pane(server, id, None);
    }

    fn register_role(server: &Server, id: &str, role: &str) {
        server.commit(&[Event::Registered {
            worker: WorkerRec {
                id: id.into(),
                token: format!("token-{id}"),
                pane: (role == "worker").then(|| format!("%test-{id}")),
                cwd: "/tmp".into(),
                registered_ms: now_ms(),
                role: role.into(),
            },
        }]);
    }

    fn register_with_pane(server: &Server, id: &str, pane: Option<&str>) {
        server.commit(&[Event::Registered {
            worker: WorkerRec {
                id: id.into(),
                token: format!("token-{id}"),
                pane: pane.map(str::to_string),
                cwd: "/tmp".into(),
                registered_ms: now_ms(),
                role: default_role(), // empty role means worker is invisible to idle_worker_ids
            },
        }]);
    }

    fn send(
        server: &Server,
        from: &str,
        to: &str,
        mtype: &str,
        body: &str,
        in_reply_to: Option<String>,
    ) -> Resp {
        handle_send(
            server,
            from.into(),
            to.into(),
            mtype.into(),
            body.into(),
            in_reply_to,
        )
    }

    #[test]
    fn duplicate_direction_request_is_rate_limited() {
        let (server, root) = test_server();
        register(&server, "sender");
        register(&server, "receiver");

        let first = send(&server, "sender", "receiver", "request", "first", None);
        assert!(first.ok);
        let first_id = first.data["msg_id"].as_str().unwrap().to_string();

        let second = send(&server, "sender", "receiver", "request", "second", None);
        assert!(!second.ok);
        assert!(second
            .error
            .unwrap()
            .contains(&format!("existing_request_id={first_id}")));

        std::fs::remove_dir_all(root).ok();
    }

    #[test]
    fn repeated_replies_deliver_only_latest() {
        let (server, root) = test_server();
        register(&server, "asker");
        register(&server, "answerer");

        let request = send(&server, "asker", "answerer", "request", "question", None);
        let request_id = request.data["msg_id"].as_str().unwrap().to_string();
        send(
            &server,
            "answerer",
            "asker",
            "reply",
            "old",
            Some(request_id.clone()),
        );
        let latest = send(
            &server,
            "answerer",
            "asker",
            "reply",
            "latest",
            Some(request_id),
        );
        let latest_id = latest.data["msg_id"].as_str().unwrap().to_string();

        let st = server.state.lock().unwrap();
        assert_eq!(
            st.inbox_of("asker")
                .iter()
                .map(|m| m.id.as_str())
                .collect::<Vec<_>>(),
            vec![latest_id.as_str()]
        );
        drop(st);
        std::fs::remove_dir_all(root).ok();
    }

    #[test]
    fn concurrent_requests_cannot_bypass_cooldown() {
        let (server, root) = test_server();
        register(&server, "sender");
        register(&server, "receiver");
        let server = Arc::new(server);

        let handles: Vec<_> = (0..4)
            .map(|i| {
                let server = server.clone();
                std::thread::spawn(move || {
                    send(
                        &server,
                        "sender",
                        "receiver",
                        "request",
                        &format!("r{i}"),
                        None,
                    )
                })
            })
            .collect();
        let accepted = handles
            .into_iter()
            .filter_map(|handle| handle.join().ok())
            .filter(|resp| resp.ok)
            .count();

        assert_eq!(accepted, 1);
        assert_eq!(server.state.lock().unwrap().msgs.len(), 1);
        drop(server);
        std::fs::remove_dir_all(root).ok();
    }

    #[test]
    fn recv_timeout_logs_submitted_tmux_reminder() {
        let (server, root) = test_server();
        register_with_pane(&server, "receiver", Some("%collab-test-missing-pane"));

        let response = handle_poll(&server, "receiver".into(), 0);
        assert_eq!(response.data["timeout"], json!(true));
        let (message_id, message_type) = {
            let state = server.state.lock().unwrap();
            let inbox = state.inbox_of("receiver");
            assert_eq!(inbox.len(), 1);
            (inbox[0].id.clone(), inbox[0].mtype.clone())
        };
        assert_eq!(message_type, "system");
        let backup = root
            .join(".agent-collab")
            .join("mailbox")
            .join(format!("{}.json", message_id));
        assert!(backup.exists());
        assert!(std::fs::read_to_string(server.log_path())
            .unwrap()
            .contains("knock failed pane=%collab-test-missing-pane"));
        std::fs::remove_dir_all(root).ok();
    }

    #[test]
    fn short_delivery_is_single_line_and_bounded() {
        let (_, root) = test_server();
        let body = "x".repeat(500);
        let text = delivery_text(&root, "sender", "notify", "m1", &body).unwrap();
        assert!(text.starts_with("[MAIL] from=sender type=notify id=m1: "));
        assert!(text.contains(&body));
        assert!(text.contains("Process this collab input now"));
        assert!(text.contains("ack id=m1"));
        assert!(text.contains("continue the current run's next step"));
        assert!(text.chars().count() <= 850);
        assert!(!text.contains('\n'));
        assert!(!message_doc_path(&root, "m1").exists());
        std::fs::remove_dir_all(root).ok();
    }

    #[test]
    fn long_delivery_stores_body_and_sends_reference() {
        let root = std::env::temp_dir().join("collab-delivery-long-test");
        let body = "long message\n".repeat(100);
        let text = delivery_text(&root, "sender", "notify", "m2", &body).unwrap();
        assert!(text.chars().count() <= 300);
        assert!(text.contains("body-ref=.agent-collab/messages/m2.md"));
        assert!(text.contains("Process this collab input now"));
        assert!(text.contains("ack id=m2"));
        assert_eq!(
            std::fs::read_to_string(message_doc_path(&root, "m2")).unwrap(),
            body
        );
        std::fs::remove_dir_all(root).ok();
    }

    #[test]
    fn system_delivery_uses_same_reasoning_prompt() {
        let (_, root) = test_server();
        let text = delivery_text(
            &root,
            "collab-server",
            "system",
            "system-1",
            "NUDGE: request needs a substantive response",
        )
        .unwrap();
        assert!(text.contains("from=collab-server type=system id=system-1"));
        assert!(text.contains("NUDGE: request needs a substantive response"));
        assert!(text.contains("decide collaborate/defer/reject with reason"));
        assert!(text.contains("reply if requested, ack id=system-1"));
        assert!(text.contains("immediately continue the current run's next step"));
        std::fs::remove_dir_all(root).ok();
    }

    #[test]
    fn first_worker_is_master_and_second_is_worker() {
        let (server, root) = test_server();
        let first = handle_register(
            &server,
            "first".into(),
            "token-first".into(),
            None,
            "/tmp".into(),
        );
        assert!(first.ok);
        assert_eq!(first.data["role"], "master");

        let second = handle_register(
            &server,
            "second".into(),
            "token-second".into(),
            None,
            "/tmp".into(),
        );
        assert!(second.ok);
        assert_eq!(second.data["role"], "worker");
        assert_eq!(
            server.state.lock().unwrap().workers["second"].role,
            "worker"
        );
        std::fs::remove_dir_all(root).ok();
    }

    #[test]
    fn task_register_sets_owner_and_conflicts_on_worktree() {
        let (server, root) = test_server();
        register_role(&server, "master", "master");
        register_role(&server, "worker", "worker");

        let master = handle_task_register(
            &server,
            "master".into(),
            "token-master".into(),
            "task-a".into(),
            Some("worker".into()),
            Some("feature-a".into()),
            Some("/tmp/worktree-a".into()),
            None,
            None,
            default_priority(),
        );
        assert!(master.ok);
        assert_eq!(master.data["owner"], "worker");

        let conflict = handle_task_register(
            &server,
            "master".into(),
            "token-master".into(),
            "task-b".into(),
            Some("worker".into()),
            Some("feature-a".into()),
            Some("/tmp/worktree-b".into()),
            None,
            None,
            default_priority(),
        );
        assert!(!conflict.ok);
        assert!(conflict.error.unwrap().contains("task resource conflict"));
        std::fs::remove_dir_all(root).ok();
    }

    #[test]
    fn worker_cannot_register_tasks() {
        let (server, root) = test_server();
        register_role(&server, "master", "master");
        register_role(&server, "worker", "worker");

        let denied = handle_task_register(
            &server,
            "worker".into(),
            "token-worker".into(),
            "task-x".into(),
            None,
            None,
            None,
            None,
            None,
            default_priority(),
        );
        assert!(!denied.ok);
        assert!(denied.error.unwrap().contains("only master"));
        std::fs::remove_dir_all(root).ok();
    }

    #[test]
    fn closing_task_unregisters_heartbeat() {
        let (server, root) = test_server();
        register_role(&server, "master", "master");
        register_role(&server, "worker", "worker");
        let created = handle_task_register(
            &server,
            "master".into(),
            "token-master".into(),
            "task-heartbeat".into(),
            Some("worker".into()),
            None,
            None,
            None,
            None,
            default_priority(),
        );
        assert!(created.ok);

        let active = server.state.lock().unwrap();
        assert!(task_heartbeat_active(
            &active.tasks["task-heartbeat"].status
        ));
        drop(active);

        let delivered = handle_task_update(
            &server,
            "worker".into(),
            "token-worker".into(),
            "task-heartbeat".into(),
            Some("delivered".into()),
            None,
        );
        assert!(delivered.ok);
        assert_eq!(delivered.data["heartbeat"], "unregistered");

        {
            let st = server.state.lock().unwrap();
            assert!(task_resource_active(&st.tasks["task-heartbeat"].status));
        }

        let closed = handle_task_update(
            &server,
            "master".into(),
            "token-master".into(),
            "task-heartbeat".into(),
            Some("merged".into()),
            None,
        );
        assert!(closed.ok);
        assert_eq!(closed.data["heartbeat"], "unregistered");

        let st = server.state.lock().unwrap();
        assert!(!task_heartbeat_active(&st.tasks["task-heartbeat"].status));
        assert!(!task_resource_active(&st.tasks["task-heartbeat"].status));
        assert_eq!(st.tasks["task-heartbeat"].status, "merged");
        std::fs::remove_dir_all(root).ok();
    }

    #[test]
    fn worker_cannot_merge_task() {
        let (server, root) = test_server();
        register_role(&server, "master", "master");
        register_role(&server, "worker", "worker");
        handle_task_register(
            &server,
            "master".into(),
            "token-master".into(),
            "task-gate".into(),
            Some("worker".into()),
            None,
            None,
            None,
            None,
            default_priority(),
        );
        let denied = handle_task_update(
            &server,
            "worker".into(),
            "token-worker".into(),
            "task-gate".into(),
            Some("merged".into()),
            None,
        );
        assert!(!denied.ok);
        assert!(denied.error.unwrap().contains("only master"));
        std::fs::remove_dir_all(root).ok();
    }

    #[test]
    fn delivered_task_keeps_resource_conflict_until_master_close() {
        let (server, root) = test_server();
        register_role(&server, "master", "master");
        register_role(&server, "worker", "worker");
        handle_task_register(
            &server,
            "master".into(),
            "token-master".into(),
            "task-held".into(),
            Some("worker".into()),
            Some("feature-held".into()),
            Some("/tmp/held".into()),
            None,
            None,
            default_priority(),
        );
        let delivered = handle_task_update(
            &server,
            "worker".into(),
            "token-worker".into(),
            "task-held".into(),
            Some("delivered".into()),
            None,
        );
        assert!(delivered.ok);

        let conflict = handle_task_register(
            &server,
            "master".into(),
            "token-master".into(),
            "task-next".into(),
            Some("worker".into()),
            Some("feature-held".into()),
            None,
            None,
            None,
            default_priority(),
        );
        assert!(!conflict.ok);

        let closed = handle_task_update(
            &server,
            "master".into(),
            "token-master".into(),
            "task-held".into(),
            Some("closed".into()),
            None,
        );
        assert!(closed.ok);

        let recreated = handle_task_register(
            &server,
            "master".into(),
            "token-master".into(),
            "task-next".into(),
            None,
            Some("feature-held".into()),
            None,
            None,
            None,
            default_priority(),
        );
        assert!(recreated.ok);

        let acquired = handle_task_claim(
            &server,
            "worker".into(),
            "token-worker".into(),
            "task-next".into(),
        );
        assert!(acquired.ok);
        std::fs::remove_dir_all(root).ok();
    }

    #[test]
    fn master_creates_available_and_worker_claims() {
        let (server, root) = test_server();
        register_role(&server, "master", "master");
        register_role(&server, "worker", "worker");

        let created = handle_task_register(
            &server,
            "master".into(),
            "token-master".into(),
            "task-avail".into(),
            None,
            Some("feature-claim".into()),
            None,
            None,
            None,
            default_priority(),
        );
        assert!(created.ok);
        assert_eq!(created.data["status"], "available");

        let claimed = handle_task_claim(
            &server,
            "worker".into(),
            "token-worker".into(),
            "task-avail".into(),
        );
        assert!(claimed.ok);
        assert_eq!(claimed.data["owner"], "worker");
        assert_eq!(claimed.data["status"], "working");

        std::fs::remove_dir_all(root).ok();
    }

    #[test]
    fn claim_fails_on_non_available_task() {
        let (server, root) = test_server();
        register_role(&server, "master", "master");
        register_role(&server, "worker", "worker");

        handle_task_register(
            &server,
            "master".into(),
            "token-master".into(),
            "task-working".into(),
            Some("worker".into()),
            Some("feature-x".into()),
            None,
            None,
            None,
            default_priority(),
        );
        let denied = handle_task_claim(
            &server,
            "worker".into(),
            "token-worker".into(),
            "task-working".into(),
        );
        assert!(!denied.ok);
        assert!(denied.error.unwrap().contains("not available"));

        std::fs::remove_dir_all(root).ok();
    }

    #[test]
    fn available_claim_transfers_owner_and_blocks_second_active_task() {
        let (server, root) = test_server();
        register_role(&server, "master", "master");
        register_role(&server, "worker", "worker");

        let created = handle_task_register(
            &server,
            "master".into(),
            "token-master".into(),
            "task-available".into(),
            None,
            Some("feature-transfer".into()),
            Some("/tmp/available".into()),
            None,
            None,
            default_priority(),
        );
        assert!(created.ok);
        {
            let st = server.state.lock().unwrap();
            assert_eq!(st.tasks["task-available"].owner, "master");
            assert_eq!(st.tasks["task-available"].status, "available");
        }

        let claimed = handle_task_claim(
            &server,
            "worker".into(),
            "token-worker".into(),
            "task-available".into(),
        );
        assert!(claimed.ok);
        assert_eq!(claimed.data["owner"], "worker");
        assert_eq!(claimed.data["status"], "working");

        let second = handle_task_register(
            &server,
            "master".into(),
            "token-master".into(),
            "task-second".into(),
            None,
            Some("feature-second".into()),
            Some("/tmp/second".into()),
            None,
            None,
            default_priority(),
        );
        assert!(second.ok);
        let denied = handle_task_claim(
            &server,
            "worker".into(),
            "token-worker".into(),
            "task-second".into(),
        );
        assert!(!denied.ok);
        assert!(denied.error.unwrap().contains("already hold active task"));

        std::fs::remove_dir_all(root).ok();
    }

    #[test]
    fn master_cannot_claim_available_task() {
        let (server, root) = test_server();
        register_role(&server, "master", "master");
        register_role(&server, "worker", "worker");
        let created = handle_task_register(
            &server,
            "master".into(),
            "token-master".into(),
            "task-master-claim".into(),
            None,
            Some("feature-master-claim".into()),
            Some("/tmp/master-claim".into()),
            None,
            None,
            default_priority(),
        );
        assert!(created.ok);
        let denied = handle_task_claim(
            &server,
            "master".into(),
            "token-master".into(),
            "task-master-claim".into(),
        );
        assert!(!denied.ok);
        assert!(denied.error.unwrap().contains("only workers may claim"));
        std::fs::remove_dir_all(root).ok();
    }

    #[test]
    fn deliver_notifies_master_and_returns_priority_ordered_available_tasks() {
        let (server, root) = test_server();
        register_role(&server, "master", "master");
        register_role(&server, "worker", "worker");

        let claimed = handle_task_register(
            &server,
            "master".into(),
            "token-master".into(),
            "claimed".into(),
            None,
            Some("feature-claimed".into()),
            None,
            None,
            None,
            "p2".into(),
        );
        assert!(claimed.ok);
        let low = handle_task_register(
            &server,
            "master".into(),
            "token-master".into(),
            "low".into(),
            None,
            Some("feature-low".into()),
            None,
            None,
            None,
            "p3".into(),
        );
        assert!(low.ok);
        let high = handle_task_register(
            &server,
            "master".into(),
            "token-master".into(),
            "high".into(),
            None,
            Some("feature-high".into()),
            None,
            None,
            None,
            "p0".into(),
        );
        assert!(high.ok);
        assert!(
            handle_task_claim(
                &server,
                "worker".into(),
                "token-worker".into(),
                "claimed".into(),
            )
            .ok
        );

        let delivered = handle_task_deliver(
            &server,
            "worker".into(),
            "token-worker".into(),
            "claimed".into(),
            Some("all gates pass".into()),
        );
        assert!(delivered.ok);
        assert_eq!(delivered.data["status"], "delivered");
        assert_eq!(delivered.data["master_notified"], "master");
        assert_eq!(delivered.data["identity"]["worker_id"], "worker");
        let available = delivered.data["available_tasks"].as_array().unwrap();
        assert_eq!(available[0]["id"], "high");
        assert_eq!(available[1]["id"], "low");

        let st = server.state.lock().unwrap();
        assert_eq!(st.tasks["claimed"].status, "delivered");
        let system = st
            .msgs
            .values()
            .find(|m| m.from == "collab-server" && m.to == "master")
            .unwrap();
        assert!(system.body.contains("TASK_DELIVERED id=claimed"));
        assert!(system.body.contains("all gates pass"));
        drop(st);
        std::fs::remove_dir_all(root).ok();
    }

    #[test]
    fn master_close_rejects_worker_and_closes_without_resources() {
        let (server, root) = test_server();
        register_role(&server, "master", "master");
        register_role(&server, "worker", "worker");
        let created = handle_task_register(
            &server,
            "master".into(),
            "token-master".into(),
            "task-close".into(),
            Some("worker".into()),
            None,
            None,
            None,
            None,
            default_priority(),
        );
        assert!(created.ok);
        assert!(
            handle_task_update(
                &server,
                "worker".into(),
                "token-worker".into(),
                "task-close".into(),
                Some("delivered".into()),
                None,
            )
            .ok
        );

        let denied = handle_task_close(
            &server,
            "worker".into(),
            "token-worker".into(),
            "task-close".into(),
        );
        assert!(!denied.ok);
        assert!(denied.error.unwrap().contains("only master"));

        let closed = handle_task_close(
            &server,
            "master".into(),
            "token-master".into(),
            "task-close".into(),
        );
        assert!(closed.ok);
        assert_eq!(closed.data["status"], "closed");
        let st = server.state.lock().unwrap();
        assert!(!task_heartbeat_active(&st.tasks["task-close"].status));
        assert!(!task_resource_active(&st.tasks["task-close"].status));
        drop(st);
        std::fs::remove_dir_all(root).ok();
    }

    #[test]
    fn close_dispatches_available_tasks_to_idle_workers_in_priority_order() {
        let (server, root) = test_server();
        register_role(&server, "master", "master");
        register_role(&server, "worker-a", "worker");
        register_role(&server, "worker-b", "worker");

        let created = handle_task_register(
            &server,
            "master".into(),
            "token-master".into(),
            "task-close".into(),
            Some("worker-a".into()),
            None,
            None,
            None,
            None,
            default_priority(),
        );
        assert!(created.ok);
        assert!(
            handle_task_update(
                &server,
                "worker-a".into(),
                "token-worker-a".into(),
                "task-close".into(),
                Some("delivered".into()),
                None,
            )
            .ok
        );

        let low = handle_task_register(
            &server,
            "master".into(),
            "token-master".into(),
            "dispatch-low".into(),
            None,
            Some("feature-low".into()),
            None,
            None,
            None,
            "p2".into(),
        );
        assert!(low.ok);
        let high = handle_task_register(
            &server,
            "master".into(),
            "token-master".into(),
            "dispatch-high".into(),
            None,
            Some("feature-high".into()),
            None,
            None,
            None,
            "p0".into(),
        );
        assert!(high.ok);

        let closed = handle_task_close(
            &server,
            "master".into(),
            "token-master".into(),
            "task-close".into(),
        );
        assert!(closed.ok);
        let dispatched = closed.data["dispatched_tasks"].as_array().unwrap();
        assert_eq!(dispatched.len(), 2);
        assert_eq!(dispatched[0]["task"], "dispatch-high");
        assert_eq!(dispatched[0]["owner"], "worker-a");
        assert_eq!(dispatched[1]["task"], "dispatch-low");
        assert_eq!(dispatched[1]["owner"], "worker-b");
        assert_eq!(closed.data["available_tasks"].as_array().unwrap().len(), 0);

        let st = server.state.lock().unwrap();
        assert_eq!(st.tasks["dispatch-high"].status, "working");
        assert_eq!(st.tasks["dispatch-high"].owner, "worker-a");
        assert_eq!(st.tasks["dispatch-low"].status, "working");
        assert_eq!(st.tasks["dispatch-low"].owner, "worker-b");
        let assigned = st
            .msgs
            .values()
            .find(|m| {
                m.from == "collab-server" && m.body.contains("TASK_ASSIGNED id=dispatch-high")
            })
            .unwrap();
        assert!(assigned.body.contains("feature=feature-high"));
        assert!(assigned.body.contains("claim is already active"));
        drop(st);
        std::fs::remove_dir_all(root).ok();
    }

    #[test]
    fn dispatch_command_assigns_available_tasks_to_idle_workers() {
        let (server, root) = test_server();
        register_role(&server, "master", "master");
        register_role(&server, "worker-a", "worker");
        register_role(&server, "worker-b", "worker");

        let low = handle_task_register(
            &server,
            "master".into(),
            "token-master".into(),
            "dispatch-low".into(),
            None,
            Some("feature-low".into()),
            None,
            None,
            None,
            "p2".into(),
        );
        assert!(low.ok);
        let high = handle_task_register(
            &server,
            "master".into(),
            "token-master".into(),
            "dispatch-high".into(),
            None,
            Some("feature-high".into()),
            None,
            None,
            None,
            "p0".into(),
        );
        assert!(high.ok);

        let dispatched = handle_task_dispatch(&server, "master".into(), "token-master".into());
        assert!(dispatched.ok);
        let tasks = dispatched.data["dispatched_tasks"].as_array().unwrap();
        assert_eq!(tasks[0]["task"], "dispatch-high");
        assert_eq!(tasks[0]["owner"], "worker-a");
        assert_eq!(tasks[1]["task"], "dispatch-low");
        assert_eq!(tasks[1]["owner"], "worker-b");
        assert_eq!(
            dispatched.data["available_tasks"].as_array().unwrap().len(),
            0
        );

        let st = server.state.lock().unwrap();
        assert_eq!(st.tasks["dispatch-high"].status, "working");
        assert_eq!(st.tasks["dispatch-low"].owner, "worker-b");
        drop(st);
        std::fs::remove_dir_all(root).ok();
    }

    #[test]
    fn dispatch_skips_registered_workers_without_a_live_pane() {
        let (server, root) = test_server();
        register_role(&server, "master", "master");
        server.commit(&[Event::Registered {
            worker: WorkerRec {
                id: "worker-no-pane".into(),
                token: "token-worker-no-pane".into(),
                pane: None,
                cwd: "/tmp".into(),
                registered_ms: now_ms(),
                role: "worker".into(),
            },
        }]);

        let created = handle_task_register(
            &server,
            "master".into(),
            "token-master".into(),
            "dispatch-unreachable".into(),
            None,
            Some("feature-dispatch-unreachable".into()),
            None,
            None,
            None,
            default_priority(),
        );
        assert!(created.ok);

        let mut st = server.state.lock().unwrap();
        assert!(
            idle_worker_ids(&st).is_empty(),
            "worker with pane=None must not appear in idle list"
        );
        let (events, knocks, dispatched) = dispatch_available_to_idle(&mut st, &|_| true);
        assert!(events.is_empty());
        assert!(knocks.is_empty());
        assert!(dispatched.is_empty());

        assert_eq!(
            st.tasks["dispatch-unreachable"].owner, "master",
            "task must stay with master when no idle pane workers exist"
        );
        assert_eq!(
            st.tasks["dispatch-unreachable"].status, "available",
            "task must stay available"
        );
        drop(st);
        std::fs::remove_dir_all(root).ok();
    }

    #[test]
    fn dispatch_assigns_only_to_registered_workers_with_live_panes() {
        let (server, root) = test_server();
        register_role(&server, "master", "master");
        register_with_pane(&server, "worker-live-pane", Some("%5"));

        let created = handle_task_register(
            &server,
            "master".into(),
            "token-master".into(),
            "dispatch-reachable".into(),
            None,
            Some("feature-dispatch-reachable".into()),
            None,
            None,
            None,
            default_priority(),
        );
        assert!(created.ok);

        let mut st = server.state.lock().unwrap();
        assert_eq!(
            idle_worker_ids(&st),
            vec!["worker-live-pane"],
            "idle list must contain exactly worker-live-pane"
        );
        let (events, knocks, dispatched) = dispatch_available_to_idle(&mut st, &|_| true);
        for event in &events {
            st.apply(event);
        }
        assert_eq!(events.len(), 2, "assignment message + task update");
        assert_eq!(knocks.len(), 1);
        assert_eq!(dispatched.len(), 1);
        assert_eq!(
            dispatched[0]["owner"], "worker-live-pane",
            "task must be assigned to worker-live-pane"
        );
        assert_eq!(dispatched[0]["status"], "working");

        assert_eq!(
            st.tasks["dispatch-reachable"].owner, "worker-live-pane",
            "task owner must be worker-live-pane after dispatch"
        );
        assert_eq!(st.tasks["dispatch-reachable"].status, "working");
        drop(st);
        std::fs::remove_dir_all(root).ok();
    }

    #[test]
    fn close_cleans_only_merged_clean_playground_worktree() {
        let root = std::env::temp_dir().join(format!(
            "collab-close-git-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        let playground = root.join("playground");
        std::fs::create_dir_all(&playground).unwrap();
        let git = |args: &[&str]| {
            Command::new("git")
                .current_dir(&root)
                .args(args)
                .output()
                .unwrap()
        };
        assert!(git(&["init", "-q"]).status.success());
        assert!(git(&["config", "user.email", "test@example.com"])
            .status
            .success());
        assert!(git(&["config", "user.name", "collab test"])
            .status
            .success());
        std::fs::write(root.join("README.md"), "base\n").unwrap();
        assert!(git(&["add", "README.md"]).status.success());
        assert!(git(&["commit", "-q", "-m", "base"]).status.success());
        assert!(
            git(&["worktree", "add", "-q", "-b", "feature", "playground/wt"])
                .status
                .success()
        );
        std::fs::write(root.join("playground/wt/feature.txt"), "work\n").unwrap();
        assert!(git(&["-C", "playground/wt", "add", "feature.txt"])
            .status
            .success());
        assert!(
            git(&["-C", "playground/wt", "commit", "-q", "-m", "feature"])
                .status
                .success()
        );

        let refused = close_task_resources(&root, Some("playground/wt"), Some("feature"));
        assert!(refused.is_err());
        assert!(refused.unwrap_err().contains("not merged"));
        assert!(playground.join("wt").is_dir());

        assert!(git(&["merge", "-q", "feature"]).status.success());
        assert!(close_task_resources(&root, Some("playground/wt"), Some("feature")).is_ok());
        assert!(!playground.join("wt").exists());
        assert!(!git(&["rev-parse", "--verify", "feature"]).status.success());
        std::fs::remove_dir_all(root).ok();
    }
}
