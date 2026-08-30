pub mod knock;
pub mod state;
pub mod timers;
pub mod tmux_probe;

use crate::proto::{Req, Resp, MSG_TYPES};
use crate::scope::Scope;
use crate::server::knock::{append_log, knock_or_log, pane_alive, pane_idle};
use serde_json::json;
use state::{
    default_role, now_ms, runtime_for_pane, task_heartbeat_active, task_resource_active,
    wait_cycle, Event, Message, State, TaskRec, WaitSpec, WorkerRec,
};
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};
use tokio::net::UnixListener;

const MAX_POLL_MS: u64 = 3_600_000;
const POLL_TICK_MS: u64 = 250;
const TASK_STATUSES: [&str; 11] = [
    "available",
    "working",
    "blocked",
    "waiting",
    "verifying",
    "reviewed",
    "delivered",
    "rework",
    "merged",
    "closed",
    "cancelled",
];
const MAX_WORKTREE_PATH_BYTES: usize = 80;

fn validate_worktree_path(root: &Path, raw: &str) -> Result<(), String> {
    if raw.trim().is_empty() {
        return Err("worktree path must be non-empty".into());
    }
    let path = Path::new(raw);
    if path
        .components()
        .any(|c| matches!(c, std::path::Component::ParentDir))
    {
        return Err("worktree path may not contain '..'".into());
    }
    let playground = root.join("playground");
    let candidate = if path.is_absolute() {
        path.to_path_buf()
    } else {
        let relative = raw.strip_prefix("./").unwrap_or(raw);
        root.join(relative)
    };
    let candidate = candidate.to_string_lossy();
    let playground = playground.to_string_lossy();
    if !candidate.starts_with(&format!("{}/", playground)) {
        return Err("worktree path must be inside ./playground".into());
    }
    if candidate.as_bytes().len() > MAX_WORKTREE_PATH_BYTES {
        return Err(format!(
            "worktree path exceeds {} bytes; use a short slug under ./playground",
            MAX_WORKTREE_PATH_BYTES
        ));
    }
    let leaf = path
        .file_name()
        .and_then(|v| v.to_str())
        .unwrap_or_default();
    if leaf.is_empty()
        || leaf.len() > 32
        || !leaf
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'.' || b == b'-' || b == b'_')
    {
        return Err("worktree basename must be a short slug (ASCII letters, digits, '.', '-' or '_'; max 32 chars)".into());
    }
    Ok(())
}

fn priority_rank(priority: &str) -> u8 {
    match priority {
        "p0" => 0,
        "p1" => 1,
        "p2" => 2,
        "p3" => 3,
        _ => 4,
    }
}

fn task_claim_held(status: &str) -> bool {
    matches!(
        status,
        "working" | "verifying" | "reviewed" | "delivered" | "rework" | "merged"
    )
}

pub struct Server {
    pub root: PathBuf,
    pub state: Mutex<State>,
    pub journal: Mutex<std::fs::File>,
    pub pane_alive_check: fn(&str) -> bool,
}

fn record_activity(root: &Path, kind: &str, detail: serde_json::Value) {
    let path = root.join(".agent-collab/server/events.jsonl");
    let record = json!({
        "ts": now_ms(),
        "kind": kind,
        "detail": detail,
    });
    if let Ok(mut file) = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(path)
    {
        use std::io::Write;
        let mut line = serde_json::to_vec(&record).unwrap_or_default();
        line.push(b'\n');
        let _ = file.write_all(&line);
    }
}

fn request_activity(req: &Req, resp: &Resp) -> serde_json::Value {
    let mut request = serde_json::to_value(req).unwrap_or_else(|_| json!({}));
    if let Some(obj) = request.as_object_mut() {
        obj.remove("token");
    }
    json!({
        "op": request.get("op").cloned().unwrap_or(json!("unknown")),
        "actor": request.get("worker_id").or_else(|| request.get("from")).cloned(),
        "task_id": request.get("task_id").cloned(),
        "target": request.get("to").cloned(),
        "ok": resp.ok,
        "error": resp.error,
        "request": request,
    })
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

const LONG_BODY_THRESHOLD_CHARS: usize = 200;
const DELIVERY_SUMMARY_CHARS: usize = 80;

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
    if body.starts_with("/goal") {
        return Ok(body.to_string());
    }
    let next = if body.starts_with("TASK_DELIVERED ") || body.starts_with("TASK_CLOSED ") {
        "action=\"inspect status/evidence, execute review/merge/close or next dispatch; do not reply without acting\"".to_string()
    } else {
        format!(
            "action=\"inspect this input, execute the required Collab state action, then continue the current run; do not reply without acting\"",
        )
    };
    if body.chars().count() <= LONG_BODY_THRESHOLD_CHARS {
        return Ok(format!("{prefix} {} | {next}", one_line(body)));
    }
    let path = write_message_doc(root, msg_id, body)
        .map_err(|e| format!("cannot store long message {}: {e}", msg_id))?;
    let relative = path
        .strip_prefix(root)
        .map_err(|e| format!("cannot make message reference relative: {e}"))?;
    let summary: String = one_line(body)
        .chars()
        .take(DELIVERY_SUMMARY_CHARS)
        .collect();
    Ok(format!(
        "{prefix} summary={}... body-ref={} {next}",
        summary,
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

pub(super) fn queue_batch_knock(server: &Server, pane: &str, ids: &[String], body: &str) {
    let prompt = format!(
        "[MAIL] from=collab-server type=system ids={} | {}",
        ids.join(","),
        body
    );
    knock_or_log(&server.log_path(), pane, &prompt);
}

fn timeout_prompt(kind: &str, worker_id: &str, timeout_ms: u64) -> String {
    format!(
        "Blocking collab {} returned without a message for worker {} after {}ms. Verify role with collab role, inspect notes/actor next step, and continue the current run; do not idle or wait for another request.",
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
    let Some(runtime) = runtime_for_pane(pane.as_deref()) else {
        return Resp::err("collab registration requires a tmux or herdr pane");
    };
    if runtime != "tmux" {
        return Resp::err("collab registration requires a tmux pane; Herdr runtime is disabled");
    }
    if let Some(existing) = st.workers.get(&worker_id).cloned() {
        if existing.token != token {
            // The tmux session name is the sole external identity. When the
            // same session comes back after a restart, its persisted token is
            // stale; rotate it atomically instead of exposing an internal
            // token conflict to the correctly named agent.
            let same_session = pane
                .as_deref()
                .and_then(tmux_session_for_pane)
                .is_some_and(|session| session == worker_id)
                && existing
                    .pane
                    .as_deref()
                    .and_then(tmux_session_for_pane)
                    .is_some_and(|session| session == worker_id);
            if same_session {
                let refreshed = WorkerRec {
                    id: worker_id.clone(),
                    token,
                    pane,
                    cwd,
                    registered_ms: existing.registered_ms,
                    role: existing.role.clone(),
                };
                let role = refreshed.role.clone();
                server.commit_locked(&mut st, &[Event::Registered { worker: refreshed }]);
                return Resp::data(json!({
                    "worker_id": worker_id,
                    "role": role,
                    "runtime": runtime,
                    "recovered": true,
                    "identity_source": "tmux_session"
                }));
            }
            return Resp::err(format!(
                "worker_id {} already registered by another token",
                worker_id
            ));
        }
        let Some(existing_runtime) = runtime_for_pane(existing.pane.as_deref()) else {
            return Resp::err("existing worker has no valid tmux or herdr runtime");
        };
        if existing_runtime != runtime {
            return Resp::err(format!(
                "worker {} cannot change runtime from {} to {}",
                worker_id, existing_runtime, runtime
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
        return Resp::data(
            json!({"worker_id": worker_id, "role": role, "runtime": runtime, "reused": true}),
        );
    }
    if let Some(master_id) = st.master_id() {
        let master = st.workers.get(&master_id).expect("master exists");
        let Some(master_runtime) = runtime_for_pane(master.pane.as_deref()) else {
            return Resp::err("master has no valid tmux or herdr runtime");
        };
        if master_runtime != runtime {
            // A tmux pane is allowed to enter a legacy Herdr project only as
            // the explicit migration/recovery candidate. `master recover`
            // must complete the handoff before any task work or dispatch;
            // ordinary mixed-runtime messaging remains rejected.
            if master_runtime == "herdr" && runtime == "tmux" {
                let rec = WorkerRec {
                    id: worker_id.clone(),
                    token,
                    pane,
                    cwd,
                    registered_ms: now_ms(),
                    role: default_role(),
                };
                server.commit_locked(
                    &mut st,
                    &[Event::Registered {
                        worker: rec.clone(),
                    }],
                );
                return Resp::data(json!({
                    "worker_id": worker_id,
                    "role": rec.role,
                    "runtime": runtime,
                    "migration": "herdr-to-tmux-pending-master-recover"
                }));
            }
            return Resp::err(format!(
                "runtime mismatch: project master uses {}, new worker uses {}",
                master_runtime, runtime
            ));
        }
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
        "runtime": runtime,
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
    delivery_mode: String,
) -> Resp {
    if delivery_mode != "immediate" && delivery_mode != "idle" {
        return Resp::err("delivery must be immediate or idle");
    }
    if !MSG_TYPES.contains(&mtype.as_str()) {
        return Resp::err(format!(
            "invalid type {}; must be one of {:?}",
            mtype, MSG_TYPES
        ));
    }
    let mut st = server.state.lock().unwrap();
    let Some(sender) = st.workers.get(&from) else {
        return Resp::err(format!("sender {} not registered", from));
    };
    let Some(recipient) = st.workers.get(&to) else {
        return Resp::err(format!("recipient {} not registered", to));
    };
    let Some(sender_runtime) = runtime_for_pane(sender.pane.as_deref()) else {
        return Resp::err("sender has no valid tmux or herdr runtime");
    };
    let Some(recipient_runtime) = runtime_for_pane(recipient.pane.as_deref()) else {
        return Resp::err("recipient has no valid tmux or herdr runtime");
    };
    if sender_runtime != recipient_runtime {
        return Resp::err(format!(
            "runtime mismatch: sender uses {}, recipient uses {}",
            sender_runtime, recipient_runtime
        ));
    }
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
    if let Some(existing) = st.msgs.values().find(|m| {
        m.from == from
            && m.to == to
            && m.mtype == mtype
            && m.body == msg.body
            && m.state == "pending"
    }) {
        return Resp::data(json!({"msg_id": existing.id, "deduplicated": true}));
    }
    let mid = msg.id.clone();
    let prompt = delivery_text(&server.root, &from, &mtype, &mid, &msg.body);
    let prompt = match prompt {
        Ok(text) => text,
        Err(e) => return Resp::err(e),
    };
    let mut events = vec![Event::Sent { msg }];
    events.push(Event::DeliveryMode {
        msg_id: mid.clone(),
        mode: delivery_mode.clone(),
    });
    if !superseded_ids.is_empty() {
        events.push(Event::Superseded {
            ids: superseded_ids,
        });
    }
    let pane = st.worker_pane(&to);
    server.commit_locked(&mut st, &events);
    drop(st);
    if delivery_mode == "immediate" {
        if let Some(p) = pane {
            if p.starts_with('%') || p.starts_with("herdr:") {
                knock_or_log(&server.log_path(), &p, &prompt);
            }
        }
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
    goal_prompt: Option<String>,
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
    if let Some(prompt) = &goal_prompt {
        if !prompt.starts_with("/goal") {
            return Resp::err("goal prompt must begin with /goal");
        }
        if prompt.trim().len() <= 5 {
            return Resp::err("goal prompt must be a complete task, not only /goal");
        }
    }
    if worker.role != "master" && owner.as_deref() != Some(worker_id.as_str()) {
        return Resp::err(
            "worker may register only a self-created task with --owner <自身worker_id>",
        );
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
    if let Some(path) = &worktree_path {
        let project_path = path.starts_with("./playground/")
            || path.starts_with("playground/")
            || path.contains("/playground/");
        if project_path {
            if let Err(e) = validate_worktree_path(&server.root, path) {
                return Resp::err(e);
            }
        }
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
        goal_busy: goal_prompt.is_some(),
        goal_prompt,
        wait: None,
        created_ms: now,
        updated_ms: now,
        last_heartbeat_sent_ms: now,
        heartbeat_pending: false,
        heartbeat_message_id: None,
        heartbeat_stale_notified: false,
    };
    server.commit_locked(&mut st, &[Event::TaskCreated { task: task.clone() }]);
    Resp::data(
        json!({"task": task.id, "owner": task.owner, "status": task.status, "goal_busy": task.goal_busy, "heartbeat": "active"}),
    )
}

fn handle_task_relocate(
    server: &Server,
    worker_id: String,
    token: String,
    task_id: String,
    worktree_path: String,
    branch: Option<String>,
    base_commit: Option<String>,
) -> Resp {
    let mut st = server.state.lock().unwrap();
    let Some(master) = st.workers.get(&worker_id).cloned() else {
        return Resp::err(format!("worker {} not registered", worker_id));
    };
    if master.token != token || master.role != "master" {
        return Resp::err("only master may relocate a task worktree");
    }
    if let Err(e) = validate_worktree_path(&server.root, &worktree_path) {
        return Resp::err(e);
    }
    let Some(mut task) = st.tasks.get(&task_id).cloned() else {
        return Resp::err(format!("task {} not found", task_id));
    };
    if matches!(task.status.as_str(), "closed" | "cancelled") {
        return Resp::err("terminal tasks cannot be relocated");
    }
    if let Some(existing) = st.tasks.values().find(|other| {
        other.id != task_id
            && task_resource_active(&other.status)
            && other.worktree_path.as_deref() == Some(worktree_path.as_str())
    }) {
        return Resp::err(format!(
            "worktree is already declared by task {}",
            existing.id
        ));
    }
    let old_worktree = task.worktree_path.clone();
    task.worktree_path = Some(worktree_path.clone());
    if branch.is_some() {
        task.branch = branch;
    }
    if base_commit.is_some() {
        task.base_commit = base_commit;
    }
    task.updated_ms = now_ms();
    server.commit_locked(&mut st, &[Event::TaskUpdated { task: task.clone() }]);
    Resp::data(json!({
        "task": task.id,
        "relocated": true,
        "old_worktree": old_worktree,
        "worktree": task.worktree_path,
        "branch": task.branch,
        "base_commit": task.base_commit,
        "status": task.status,
        "next": "verify git worktree list and continue the existing claim; evidence remains attached"
    }))
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
    let stale_workers = stale_worker_views(&st, &server.pane_alive_check);
    drop(st);
    for (pane, prompt_id, prompt) in dispatch_knocks {
        queue_system_knock(server, &pane, &prompt_id, &prompt);
    }
    Resp::data(json!({
        "dispatched_tasks": dispatched,
        "available_tasks": available_tasks.clone(),
        "stale_workers": stale_workers,
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
    let Some(mut task) = st.tasks.get(&task_id).cloned() else {
        return Resp::err(format!("task {} not found", task_id));
    };
    if task.status != "available" {
        return Resp::err(format!(
            "task {} is not available (current status: {})",
            task_id, task.status
        ));
    }
    if worker.role == "master" {
        let idle_workers = idle_worker_ids(&st);
        if !idle_workers.is_empty() {
            return Resp::err(format!(
                "master must run collab task dispatch before self-claim; idle workers available: {}",
                idle_workers.join(", ")
            ));
        }
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
    let is_owner = task.owner == worker_id;
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
        if new_status == "closed" {
            return Resp::err("use collab task close after master merge and cleanup");
        }
        if new_status == "delivered" {
            return Resp::err(
                "use collab task deliver to complete a claim; direct status mutation is rejected",
            );
        }
        if new_status == "working" && worker.role == "worker" && task.status != "rework" {
            return Resp::err(
                "workers enter working through collab task claim, or resume only from rework",
            );
        }
        if new_status == "available" && worker.role != "master" {
            return Resp::err("only master may release a task back to available");
        }
        let master_management = worker.role == "master"
            && matches!(
                new_status.as_str(),
                "available" | "rework" | "merged" | "cancelled"
            );
        if !is_owner && !master_management {
            return Resp::err("only the task owner may update its work state; master may rework");
        }
        task.status = new_status.clone();
        if new_status != "waiting" {
            task.wait = None;
        }
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
    let mut dispatched = Vec::new();
    if worker.role == "master" {
        let (events, knocks, assigned) =
            dispatch_available_to_idle(&mut st, &server.pane_alive_check);
        if !events.is_empty() {
            server.commit_locked(&mut st, &events);
        }
        dispatched = assigned;
        for (pane, prompt_id, prompt) in knocks {
            queue_system_knock(server, &pane, &prompt_id, &prompt);
        }
    }
    let mut block_notification = serde_json::Value::Null;
    if task.status == "blocked" && worker.role == "worker" {
        let master = st.master_id().unwrap_or_else(|| "unassigned".into());
        let body = format!(
            "TASK_BLOCKED id={} owner={} next={}; MASTER_ACTION: inspect the blocker and set rework or available through Collab",
            task.id,
            task.owner,
            task.next_step.as_deref().unwrap_or("inspect blocker")
        );
        let mid = gen_msg_id();
        let message = Message {
            id: mid.clone(),
            from: "collab-server".into(),
            to: master.clone(),
            mtype: "system".into(),
            body: body.clone(),
            in_reply_to: None,
            created_ms: now_ms(),
            state: "pending".into(),
            nudge_count: 0,
            last_nudge_ms: 0,
        };
        server.commit_locked(&mut st, &[Event::Sent { msg: message }]);
        if let Some(pane) = st.worker_pane(&master) {
            queue_system_knock(server, &pane, &mid, &body);
        }
        block_notification = json!({"message_id": mid, "durable": true, "target": master});
    }
    Resp::data(
        json!({"task": task.id, "status": task.status, "heartbeat": if task_heartbeat_active(&task.status) { "active" } else { "unregistered" }, "dispatched_tasks": dispatched, "block_notification": block_notification}),
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
                "goal_busy": task.goal_busy,
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
                && w.pane.as_deref().is_some_and(pane_idle)
                && !st.tasks.values().any(|task| {
                    task.owner == w.id && task.goal_busy && task_claim_held(&task.status)
                })
        })
        .collect();
    workers.sort_by(|a, b| a.id.cmp(&b.id));
    workers
        .into_iter()
        .map(|worker| worker.id.clone())
        .collect()
}

fn stale_worker_views(st: &State, is_reachable: &dyn Fn(&str) -> bool) -> Vec<serde_json::Value> {
    st.workers
        .values()
        .filter(|worker| worker.pane.is_some() && !worker.pane.as_deref().is_some_and(is_reachable))
        .map(|worker| {
            let active_tasks: Vec<String> = st
                .tasks
                .values()
                .filter(|task| task.owner == worker.id && task_resource_active(&task.status))
                .map(|task| task.id.clone())
                .collect();
            json!({
                "worker": worker.id,
                "role": worker.role,
                "pane": worker.pane,
                "active_tasks": active_tasks,
                "action": "master verify zombie, then collab remove-worker <worker-id> [--force]"
            })
        })
        .collect()
}

fn tmux_sessions_for_cwd(cwd: &str) -> Vec<String> {
    let prefix = Path::new(cwd)
        .file_name()
        .and_then(|name| name.to_str())
        .map(|name| {
            name.chars()
                .map(|ch| {
                    if ch.is_ascii_alphanumeric() || ".-_".contains(ch) {
                        ch
                    } else {
                        '-'
                    }
                })
                .collect::<String>()
        })
        .unwrap_or_default();
    let output = Command::new("tmux")
        .args(["list-sessions", "-F", "#{session_name}\t#{session_path}"])
        .output();
    let Ok(output) = output else {
        return Vec::new();
    };
    if !output.status.success() {
        return Vec::new();
    }
    String::from_utf8_lossy(&output.stdout)
        .lines()
        .filter_map(|line| {
            let (name, path) = line.split_once('\t')?;
            (path == cwd && name.starts_with(&format!("{}-", prefix))).then_some(name.to_string())
        })
        .collect()
}

fn tmux_session_for_pane(pane: &str) -> Option<String> {
    let output = Command::new("tmux")
        .args(["display-message", "-p", "-t", pane, "#S"])
        .output()
        .ok()?;
    if !output.status.success() {
        return None;
    }
    let name = String::from_utf8_lossy(&output.stdout).trim().to_string();
    (!name.is_empty()).then_some(name)
}

fn tmux_panes_for_sessions(sessions: &[String]) -> Vec<(String, String)> {
    let output = Command::new("tmux")
        .args(["list-panes", "-a", "-F", "#{session_name}\t#{pane_id}"])
        .output();
    let Ok(output) = output else {
        return Vec::new();
    };
    if !output.status.success() {
        return Vec::new();
    }
    String::from_utf8_lossy(&output.stdout)
        .lines()
        .filter_map(|line| {
            let (session, pane) = line.split_once('\t')?;
            sessions
                .iter()
                .any(|candidate| candidate == session)
                .then_some((session.to_string(), pane.to_string()))
        })
        .collect()
}

fn dispatch_available_to_idle(
    st: &mut State,
    is_reachable: &dyn Fn(&str) -> bool,
) -> (
    Vec<Event>,
    Vec<(String, String, String)>,
    Vec<serde_json::Value>,
) {
    let available_tasks = available_task_views(st);

    let mut events = Vec::new();
    let mut knocks = Vec::new();
    let mut assignments = Vec::new();
    let idle: Vec<String> = idle_worker_ids(st)
        .into_iter()
        .filter(|worker_id| {
            st.worker_pane(worker_id)
                .as_deref()
                .is_some_and(|pane| is_reachable(pane))
        })
        .collect();
    for worker_id in idle {
        let Some(pane) = st.worker_pane(&worker_id) else {
            continue;
        };
        let goal = available_tasks
            .iter()
            .find(|task| task["goal_busy"] == true);
        let body = if let Some(goal) = goal {
            let id = goal["id"].as_str().unwrap_or_default();
            let prompt = st
                .tasks
                .get(id)
                .and_then(|task| task.goal_prompt.as_deref())
                .unwrap_or_default();
            prompt.to_string()
        } else {
            format!("TASK_OFFER worker={} available_tasks={} | Choose one task and call collab task claim <task-id>; this offer does not assign ownership", worker_id, serde_json::to_string(&available_tasks).unwrap_or_else(|_| "[]".into()))
        };
        let mid = gen_msg_id();

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
        assignments.push(json!({
            "worker": worker_id,
            "status": "offered",
            "available_tasks": available_tasks.clone(),
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
    worktree: Option<String>,
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
    if task.owner != worker_id || !task_heartbeat_active(&task.status) {
        return Resp::err(format!(
            "task {} is not an active claim owned by {}",
            task_id, worker_id
        ));
    }

    let Some(evidence) = evidence.filter(|value| !value.trim().is_empty()) else {
        return Resp::err("task deliver requires non-empty --evidence");
    };
    let Some(worktree) = worktree.filter(|value| !value.trim().is_empty()) else {
        return Resp::err("task deliver requires non-empty --worktree");
    };
    if task
        .worktree_path
        .as_deref()
        .is_some_and(|registered| registered != worktree)
    {
        return Resp::err("task deliver --worktree must match the registered task worktree");
    }
    let master = st.master_id().unwrap_or_else(|| "unassigned".to_string());
    let available_tasks = available_task_views(&st);
    let body = format!(
        "TASK_DELIVERED id={} status=delivered owner={} feature={} worktree={} branch={} evidence={} | MASTER_ACTION: review this delivery; rework or merge, then run collab task close before dispatching the next task",
        task.id,
        task.owner,
        task.feature_id.as_deref().unwrap_or("none"),
        worktree,
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
    task.wait = None;
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
        "master_notified": master.clone(),
        "notification": {
            "message_id": mid,
            "durable": true,
            "target": master,
            "wake_attempted": st.worker_pane(&master).is_some()
        },
        "master_action": {
            "required": true,
            "review": "review evidence and worktree",
            "on_valid": "collab task update <task-id> --status merged, then collab task close <task-id>",
            "on_invalid": "collab task update <task-id> --status rework"
        },
        "worker_action": {
            "claim_allowed": false,
            "next": "wait for master close; do not claim another task while this claim is delivered"
        },
        "available_tasks": available_tasks,
        "next_action": "master must review/merge or rework; worker claim remains held until collab task close",
        "identity": {"worker_id": worker.id, "role": worker.role},
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
    if task.status != "merged" {
        return Resp::err(format!(
            "task {} must be merged by master before close (current: {})",
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
    closed.wait = None;
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
        "TASK_CLOSED id={} owner={} feature={} status=closed; MASTER_ACTION: re-analyze the goal, publish the next tasks if the goal is incomplete, then dispatch; owner may claim only after close.",
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

    // Closing the lock-holder wakes every durable waiter. The wake is only a
    // prompt; each waiter must re-query conflicts before resuming.
    let waiting: Vec<(String, String)> = st
        .tasks
        .values()
        .filter(|candidate| {
            candidate.status == "waiting"
                && candidate.next_step.as_deref() == Some(&format!("WAITING_FOR={}", closed.id))
        })
        .filter_map(|candidate| {
            st.worker_pane(&candidate.owner)
                .map(|pane| (candidate.owner.clone(), pane))
        })
        .collect();
    let mut wait_knocks = Vec::new();
    for (waiter, pane) in waiting {
        let wait_id = gen_msg_id();
        let wait_body = format!(
            "RESOURCE_RELEASED task={} waiter={} | Re-check collab task conflicts now; claim/resume only after the Server confirms the resource is free",
            closed.id, waiter
        );
        server.commit_locked(
            &mut st,
            &[Event::Sent {
                msg: Message {
                    id: wait_id.clone(),
                    from: "collab-server".into(),
                    to: waiter,
                    mtype: "system".into(),
                    body: wait_body.clone(),
                    in_reply_to: None,
                    created_ms: now_ms(),
                    state: "pending".into(),
                    nudge_count: 0,
                    last_nudge_ms: 0,
                },
            }],
        );
        wait_knocks.push((pane, wait_id, wait_body));
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
    let stale_workers = stale_worker_views(&st, &server.pane_alive_check);
    drop(st);
    for (pane, prompt_id, prompt) in wait_knocks {
        queue_system_knock(server, &pane, &prompt_id, &prompt);
    }
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
        "stale_workers": stale_workers,
        "master_action": "re-analyze the goal and publish/dispatch next tasks if incomplete",
        "worker_action": {
            "claim_allowed": true,
            "available_tasks": available_tasks,
            "next": "claim one returned available task, or remain idle if the goal is complete"
        },
        "next_action": "master re-analyze and dispatch; owner may claim one returned available task after close",
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
        "goal_busy": task.goal_busy,
        "goal_prompt": task.goal_prompt,
        "wait": task.wait,
        "heartbeat": if task_heartbeat_active(&task.status) { "active" } else { "unregistered" },
        "updated_at": iso(task.updated_ms),
    })
}

fn handle_context(server: &Server, worker_id: String, token: String) -> Resp {
    let st = server.state.lock().unwrap();
    if let Err(e) = verify(&st, &worker_id, &token) {
        return e;
    }
    let Some(worker) = st.workers.get(&worker_id) else {
        return Resp::err(format!("worker {} not registered", worker_id));
    };
    let tasks: Vec<serde_json::Value> = st
        .tasks
        .values()
        .filter(|task| task.owner == worker_id || task.status == "available")
        .map(task_view)
        .collect();
    let unread: Vec<&Message> = st.inbox_of(&worker_id);
    let next_actions: Vec<String> = tasks
        .iter()
        .filter_map(|task| {
            task.get("next_step")
                .and_then(|v| v.as_str())
                .map(str::to_owned)
        })
        .collect();
    Resp::data(json!({
        "identity": {"worker_id": worker.id, "role": worker.role, "pane": worker.pane},
        "liveness": {
            "pane_alive": worker.pane.as_deref().is_some_and(pane_alive),
            "pane_idle": worker.pane.as_deref().is_some_and(pane_idle),
        },
        "tasks": tasks,
        "inbox": {"unread": unread.len()},
        "next_actions": next_actions,
        "truth": "server journal and mailbox; tmux is wake-only",
    }))
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

fn handle_task_wait(
    server: &Server,
    worker_id: String,
    token: String,
    task_id: String,
    blocking_task_id: String,
) -> Resp {
    let mut st = server.state.lock().unwrap();
    let Some(worker) = st.workers.get(&worker_id).cloned() else {
        return Resp::err(format!("worker {} not registered", worker_id));
    };
    if worker.token != token || worker.role != "worker" {
        return Resp::err("only the owning worker may wait on a resource conflict");
    }
    let Some(mut task) = st.tasks.get(&task_id).cloned() else {
        return Resp::err(format!("task {} not found", task_id));
    };
    if task.owner != worker_id || !task_claim_held(&task.status) {
        return Resp::err("only an owned active task may enter waiting");
    }
    let Some(blocking) = st.tasks.get(&blocking_task_id).cloned() else {
        return Resp::err(format!("blocking task {} not found", blocking_task_id));
    };
    if task.id == blocking_task_id || wait_cycle(&st.tasks, &task.id, &blocking_task_id) {
        return Resp::err("WAIT_CYCLE_DETECTED");
    }
    let conflict = task_resource_active(&blocking.status)
        && ((task.feature_id.is_some() && task.feature_id == blocking.feature_id)
            || (task.worktree_path.is_some() && task.worktree_path == blocking.worktree_path));
    if !conflict {
        return Resp::err("blocking task does not hold a matching active resource");
    }
    task.status = "waiting".into();
    task.next_step = Some(format!("WAITING_FOR={}", blocking_task_id));
    task.wait = Some(WaitSpec {
        waiting_for: blocking_task_id.clone(),
        responsible_actor: st.master_id().unwrap_or_else(|| "unassigned".into()),
        reason: "resource_conflict".into(),
        deadline_ms: now_ms() + 15 * 60 * 1000,
        resume_on: vec![
            "resource_released".into(),
            "rework".into(),
            "cancelled".into(),
        ],
        escalation: "master_review".into(),
    });
    task.heartbeat_pending = false;
    task.heartbeat_message_id = None;
    task.updated_ms = now_ms();
    server.commit_locked(&mut st, &[Event::TaskUpdated { task: task.clone() }]);
    let master = st.master_id().unwrap_or_else(|| "unassigned".into());
    let body = format!(
        "RESOURCE_WAITING id={} owner={} waiting_for={}; MASTER_ACTION: resolve priority and keep the waiter paused until the lock is released",
        task.id, worker_id, blocking_task_id
    );
    let mid = gen_msg_id();
    server.commit_locked(
        &mut st,
        &[Event::Sent {
            msg: Message {
                id: mid.clone(),
                from: "collab-server".into(),
                to: master.clone(),
                mtype: "system".into(),
                body: body.clone(),
                in_reply_to: None,
                created_ms: now_ms(),
                state: "pending".into(),
                nudge_count: 0,
                last_nudge_ms: 0,
            },
        }],
    );
    if let Some(pane) = st.worker_pane(&master) {
        queue_system_knock(server, &pane, &mid, &body);
    }
    Resp::data(
        json!({"task": task.id, "status": task.status, "waiting_for": blocking_task_id, "master_notification": mid}),
    )
}

fn handle_transfer_master(
    server: &Server,
    worker_id: String,
    token: String,
    target_id: String,
) -> Resp {
    let mut st = server.state.lock().unwrap();
    let Some(current) = st.workers.get(&worker_id).cloned() else {
        return Resp::err(format!("worker {} not registered", worker_id));
    };
    if current.token != token || current.role != "master" {
        return Resp::err("only the current master may transfer master role");
    }
    let Some(target) = st.workers.get(&target_id).cloned() else {
        return Resp::err(format!("target worker {} not registered", target_id));
    };
    if target_id == worker_id {
        return Resp::err("target worker must differ from current master");
    }
    if runtime_for_pane(current.pane.as_deref()) != runtime_for_pane(target.pane.as_deref()) {
        return Resp::err("master transfer requires the same runtime");
    }
    server.commit_locked(
        &mut st,
        &[Event::MasterTransferred {
            from: worker_id.clone(),
            to: target_id.clone(),
        }],
    );
    Resp::data(json!({"master": target_id, "previous_master": worker_id}))
}

fn handle_master_recover(
    server: &Server,
    worker_id: String,
    token: String,
    session: String,
) -> Resp {
    let mut st = server.state.lock().unwrap();
    let Some(current) = st.workers.get(&worker_id).cloned() else {
        return Resp::err(format!("worker {} not registered", worker_id));
    };
    if current.token != token {
        return Resp::err("token mismatch: identity does not own this worker_id");
    }
    if runtime_for_pane(current.pane.as_deref()) != Some("tmux") {
        return Resp::err("master recover requires a tmux pane");
    }
    if session.trim().is_empty() {
        return Resp::err("master recover requires a non-empty tmux session name");
    }
    let Some(previous) = st.master_id() else {
        return Resp::err("no registered master to recover");
    };
    if previous == worker_id {
        return Resp::data(
            json!({"master": worker_id, "session": session, "recovered": false, "broadcast": 0}),
        );
    }
    let previous_pane = st.worker_pane(&previous);
    if previous_pane.as_deref().is_some_and(pane_alive) {
        return Resp::err("current master endpoint is live; use collab transfer-master instead");
    }
    server.commit_locked(
        &mut st,
        &[Event::MasterTransferred {
            from: previous.clone(),
            to: worker_id.clone(),
        }],
    );
    let sessions = tmux_sessions_for_cwd(&current.cwd);
    let registered_recipients: Vec<(String, String)> = st
        .workers
        .values()
        .filter(|worker| worker.id != worker_id)
        .filter_map(|worker| {
            let pane = worker.pane.clone()?;
            let session_name = tmux_session_for_pane(&pane)?;
            sessions
                .contains(&session_name)
                .then_some((worker.id.clone(), pane))
        })
        .collect();
    let body = format!(
        "MASTER_RECOVERED master={} session={} previous_master={}; re-run collab role, collab who, and collab master, reconcile active claims, then resume review and dispatch; do not infer identity from this message",
        worker_id, session, previous
    );
    let mut knocks = Vec::new();
    let mut broadcast = 0usize;
    let mut registered_panes = std::collections::HashSet::new();
    for (recipient, pane) in registered_recipients {
        registered_panes.insert(pane.clone());
        let mid = gen_msg_id();
        server.commit_locked(
            &mut st,
            &[Event::Sent {
                msg: Message {
                    id: mid.clone(),
                    from: "collab-server".into(),
                    to: recipient,
                    mtype: "system".into(),
                    body: body.clone(),
                    in_reply_to: None,
                    created_ms: now_ms(),
                    state: "pending".into(),
                    nudge_count: 0,
                    last_nudge_ms: 0,
                },
            }],
        );
        knocks.push((pane, mid));
        broadcast += 1;
    }
    // The tm group is the discovery boundary. Wake every live pane in the
    // directory-prefixed session group, including panes that have not yet
    // rebuilt their identity. Unregistered panes receive no mailbox record;
    // they must run `collab init` and query the Server after waking.
    for (_session, pane) in tmux_panes_for_sessions(&sessions) {
        if pane == current.pane.clone().unwrap_or_default() || registered_panes.contains(&pane) {
            continue;
        }
        let mid = gen_msg_id();
        knocks.push((pane, mid));
        broadcast += 1;
    }
    drop(st);
    for (pane, mid) in knocks {
        queue_system_knock(server, &pane, &mid, &body);
    }
    Resp::data(json!({
        "master": worker_id,
        "previous_master": previous,
        "session": session,
        "recovered": true,
        "broadcast": broadcast,
        "next": "workers must re-run collab role/who/master; task state remains journal-authoritative"
    }))
}

fn handle_remove_worker(
    server: &Server,
    worker_id: String,
    token: String,
    target_id: String,
    force: bool,
) -> Resp {
    let mut st = server.state.lock().unwrap();
    let Some(master) = st.workers.get(&worker_id).cloned() else {
        return Resp::err(format!("worker {} not registered", worker_id));
    };
    if master.token != token || master.role != "master" {
        return Resp::err("only the current master may remove workers");
    }
    if target_id == worker_id {
        return Resp::err("master cannot remove itself; transfer master first");
    }
    if !st.workers.contains_key(&target_id) {
        return Resp::err(format!("target worker {} not registered", target_id));
    }
    let active_task_ids: Vec<String> = st
        .tasks
        .values()
        .filter(|task| task.owner == target_id && task_resource_active(&task.status))
        .map(|task| task.id.clone())
        .collect();
    let endpoint_live = st
        .worker_pane(&target_id)
        .as_deref()
        .is_some_and(pane_alive);
    if endpoint_live && !force {
        return Resp::err(
            "worker endpoint is still live; confirm zombie state before --force removal",
        );
    }
    if !active_task_ids.is_empty() && !force {
        return Resp::err("cannot remove worker holding an active task");
    }
    let mut events = Vec::new();
    for task_id in &active_task_ids {
        if let Some(task) = st.tasks.get(task_id).cloned() {
            events.push(Event::TaskUpdated {
                task: TaskRec {
                    owner: master.id.clone(),
                    status: "available".into(),
                    next_step: Some("requeued after master removed stale worker".into()),
                    heartbeat_pending: false,
                    heartbeat_message_id: None,
                    heartbeat_stale_notified: false,
                    updated_ms: now_ms(),
                    ..task
                },
            });
        }
    }
    events.push(Event::WorkerRemoved {
        worker_id: target_id.clone(),
    });
    server.commit_locked(&mut st, &events);
    Resp::data(json!({"removed": target_id, "requeued_tasks": active_task_ids}))
}

fn handle_reset_bindings(server: &Server, confirm: bool) -> Resp {
    if !confirm {
        return Resp::err("collab reset requires explicit confirmation");
    }
    let mut st = server.state.lock().unwrap();
    let worker_ids: Vec<String> = st.workers.keys().cloned().collect();
    let mut events = Vec::new();
    for task in st
        .tasks
        .values()
        .filter(|task| task_resource_active(&task.status))
    {
        let status = if task.status == "delivered" {
            "delivered"
        } else {
            "available"
        };
        events.push(Event::TaskUpdated {
            task: TaskRec {
                owner: String::new(),
                status: status.into(),
                next_step: Some("rebuild after runtime identity reset".into()),
                heartbeat_pending: false,
                heartbeat_message_id: None,
                heartbeat_stale_notified: false,
                updated_ms: now_ms(),
                ..task.clone()
            },
        });
    }
    for worker_id in &worker_ids {
        events.push(Event::WorkerRemoved {
            worker_id: worker_id.clone(),
        });
    }
    server.commit_locked(&mut st, &events);
    Resp::data(json!({
        "reset": true,
        "removed_workers": worker_ids,
        "requeued_tasks": st.tasks.values().filter(|task| task.status == "available" && task.next_step.as_deref() == Some("rebuild after runtime identity reset")).count(),
        "next": "start tmux group; run collab init in first pane; then verify role and rebuild board"
    }))
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
            delivery,
        } => handle_send(server, from, to, mtype, body, in_reply_to, delivery),
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
        Req::Context { worker_id, token } => handle_context(server, worker_id, token),
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
            goal_prompt,
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
            goal_prompt,
        ),
        Req::TaskRelocate {
            worker_id,
            token,
            task_id,
            worktree_path,
            branch,
            base_commit,
        } => handle_task_relocate(
            server,
            worker_id,
            token,
            task_id,
            worktree_path,
            branch,
            base_commit,
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
        Req::TaskWait {
            worker_id,
            token,
            task_id,
            blocking_task_id,
        } => handle_task_wait(server, worker_id, token, task_id, blocking_task_id),
        Req::TaskDeliver {
            worker_id,
            token,
            task_id,
            evidence,
            worktree,
        } => handle_task_deliver(server, worker_id, token, task_id, evidence, worktree),
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
                        "endpoint_live": w.pane.as_deref().is_some_and(pane_alive),
                        "active_task": active.map(|task| task.id.as_str()),
                        "active_status": active.map(|task| task.status.as_str()),
                        "goal_busy": active.is_some_and(|task| task.goal_busy),
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
        Req::MasterRecover {
            worker_id,
            token,
            session,
        } => handle_master_recover(server, worker_id, token, session),
        Req::TransferMaster {
            worker_id,
            token,
            target_id,
        } => handle_transfer_master(server, worker_id, token, target_id),
        Req::RemoveWorker {
            worker_id,
            token,
            target_id,
            force,
        } => handle_remove_worker(server, worker_id, token, target_id, force),
        Req::ResetBindings { confirm } => handle_reset_bindings(server, confirm),
        Req::Shutdown { worker_id } => {
            let st = server.state.lock().unwrap();
            let Some(worker) = st.workers.get(&worker_id) else {
                return Resp::err("shutdown denied: identity is not registered");
            };
            if worker.role != "master" {
                return Resp::err(
                    "shutdown denied: only the authenticated master may stop the daemon",
                );
            }
            Resp::data(json!({"authorized": true, "role": worker.role}))
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
                let activity_req = req.clone();
                // blocking handlers (poll/wait) run off the async reactor thread pool
                let srv = server.clone();
                let resp = tokio::task::spawn_blocking(move || dispatch(&srv, req))
                    .await
                    .unwrap_or_else(|e| Resp::err(format!("handler join error: {}", e)));
                record_activity(
                    &server.root,
                    "request",
                    request_activity(&activity_req, &resp),
                );
                resp
            }
            Err(e) => {
                let resp = Resp::err(format!("bad request: {}", e));
                record_activity(&server.root, "protocol_error", json!({"error": resp.error}));
                resp
            }
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
    std::fs::write(
        server_dir.join("server.pid"),
        std::process::id().to_string(),
    )?;
    record_activity(
        &scope.root,
        "daemon_start",
        json!({"pid": std::process::id()}),
    );

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

    #[test]
    fn worktree_path_budget_accepts_short_slug_and_rejects_long_or_escape() {
        let root = PathBuf::from("/tmp/project");
        assert!(validate_worktree_path(&root, "./playground/ar03-0828").is_ok());
        assert!(validate_worktree_path(
            &root,
            "./playground/v3-direct-sse-terminal-observability-20260827-long-run-id"
        )
        .is_err());
        assert!(validate_worktree_path(&root, "./playground/../outside").is_err());
    }
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
        register_with_pane(server, id, Some("%test-default"));
    }

    fn register_role(server: &Server, id: &str, role: &str) {
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
            "immediate".into(),
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
        let body = "x".repeat(150);
        let text = delivery_text(&root, "sender", "notify", "m1", &body).unwrap();
        assert!(text.starts_with("[MAIL] from=sender type=notify id=m1: "));
        assert!(text.contains(&body));
        assert!(text.contains("action=\"inspect this input"));
        assert!(!text.to_ascii_lowercase().contains("ack"));
        assert!(text.contains("continue the current run"));
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
        assert!(text.contains("body-ref=.agent-collab/messages/m2.md"));
        assert!(!text.to_ascii_lowercase().contains("ack"));
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
        assert!(text.contains("action=\"inspect this input"));
        assert!(!text.to_ascii_lowercase().contains("ack"));
        assert!(text.contains("execute the required Collab state action"));
        std::fs::remove_dir_all(root).ok();
    }

    #[test]
    fn task_delivery_and_close_have_actionable_next_steps() {
        let (_, root) = test_server();
        let delivered = delivery_text(
            &root,
            "collab-server",
            "system",
            "deliver-1",
            "TASK_DELIVERED id=t status=delivered | MASTER_ACTION: review",
        )
        .unwrap();
        assert!(delivered.contains("inspect status/evidence"));
        assert!(!delivered.contains("ack id=deliver-1"));
        let closed = delivery_text(
            &root,
            "collab-server",
            "system",
            "close-1",
            "TASK_CLOSED id=t status=closed",
        )
        .unwrap();
        assert!(closed.contains("inspect status/evidence"));
        std::fs::remove_dir_all(root).ok();
    }

    #[test]
    fn first_worker_is_master_and_second_is_worker() {
        let (server, root) = test_server();
        let first = handle_register(
            &server,
            "first".into(),
            "token-first".into(),
            Some("%test-first".into()),
            "/tmp".into(),
        );
        assert!(first.ok);
        assert_eq!(first.data["role"], "master");

        let second = handle_register(
            &server,
            "second".into(),
            "token-second".into(),
            Some("%test-second".into()),
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
    fn registration_rejects_mixed_runtime() {
        let (server, root) = test_server();
        let first = handle_register(
            &server,
            "tmux-master".into(),
            "token-master".into(),
            Some("%test-master".into()),
            "/tmp".into(),
        );
        assert!(first.ok);
        let second = handle_register(
            &server,
            "herdr-worker".into(),
            "token-worker".into(),
            Some("herdr:w4:p2".into()),
            "/tmp".into(),
        );
        assert!(!second.ok);
        assert!(second.error.unwrap().contains("Herdr runtime is disabled"));
        std::fs::remove_dir_all(root).ok();
    }

    #[test]
    fn tmux_registration_is_allowed_as_herdr_migration_candidate() {
        let (server, root) = test_server();
        server.commit(&[Event::Registered {
            worker: WorkerRec {
                id: "legacy-master".into(),
                token: "token-master".into(),
                pane: Some("herdr:w4:p1".into()),
                cwd: "/tmp".into(),
                registered_ms: now_ms(),
                role: "master".into(),
            },
        }]);
        let replacement = handle_register(
            &server,
            "tmux-replacement".into(),
            "token-replacement".into(),
            Some("%test-replacement".into()),
            "/tmp".into(),
        );
        assert!(replacement.ok);
        assert_eq!(
            replacement.data["migration"],
            "herdr-to-tmux-pending-master-recover"
        );
        std::fs::remove_dir_all(root).ok();
    }

    #[test]
    fn reset_bindings_removes_all_runtime_identities() {
        let (server, root) = test_server();
        server.commit(&[Event::Registered {
            worker: WorkerRec {
                id: "old-master".into(),
                token: "token-master".into(),
                pane: Some("herdr:w4:p1".into()),
                cwd: "/tmp".into(),
                registered_ms: now_ms(),
                role: "master".into(),
            },
        }]);
        let reset = handle_reset_bindings(&server, true);
        assert!(reset.ok);
        assert!(server.state.lock().unwrap().workers.is_empty());
        std::fs::remove_dir_all(root).ok();
    }

    #[test]
    fn message_rejects_cross_runtime_recipient() {
        let (server, root) = test_server();
        register_with_pane(&server, "tmux", Some("%test-tmux"));
        server.commit(&[Event::Registered {
            worker: WorkerRec {
                id: "herdr".into(),
                token: "token-herdr".into(),
                pane: Some("herdr:w4:p2".into()),
                cwd: "/tmp".into(),
                registered_ms: now_ms(),
                role: default_role(),
            },
        }]);
        let refused = send(&server, "tmux", "herdr", "notify", "x", None);
        assert!(!refused.ok);
        assert!(refused.error.unwrap().contains("runtime mismatch"));
        std::fs::remove_dir_all(root).ok();
    }

    #[test]
    fn master_can_transfer_and_remove_old_identity() {
        let (server, root) = test_server();
        register_role(&server, "master", "master");
        register_with_pane(&server, "replacement", Some("%test-replacement"));
        let moved = handle_transfer_master(
            &server,
            "master".into(),
            "token-master".into(),
            "replacement".into(),
        );
        assert!(moved.ok);
        assert_eq!(
            server.state.lock().unwrap().master_id().as_deref(),
            Some("replacement")
        );
        let removed = handle_remove_worker(
            &server,
            "replacement".into(),
            "token-replacement".into(),
            "master".into(),
            false,
        );
        assert!(removed.ok);
        assert!(!server.state.lock().unwrap().workers.contains_key("master"));
        std::fs::remove_dir_all(root).ok();
    }

    #[test]
    fn idle_delivery_is_journaled_and_duplicates_coalesce() {
        let (server, root) = test_server();
        register_with_pane(&server, "sender", Some("%test-sender"));
        register_with_pane(&server, "receiver", Some("%test-receiver"));
        let first = handle_send(
            &server,
            "sender".into(),
            "receiver".into(),
            "notify".into(),
            "same".into(),
            None,
            "idle".into(),
        );
        assert!(first.ok);
        assert_eq!(server.state.lock().unwrap().delivery_modes.len(), 1);
        let second = handle_send(
            &server,
            "sender".into(),
            "receiver".into(),
            "notify".into(),
            "same".into(),
            None,
            "idle".into(),
        );
        assert!(second.ok);
        assert_eq!(second.data["deduplicated"], true);
        assert_eq!(server.state.lock().unwrap().msgs.len(), 1);
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
    fn worker_registers_only_self_created_task() {
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
        assert!(denied.error.unwrap().contains("self-created"));
        let created = handle_task_register(
            &server,
            "worker".into(),
            "token-worker".into(),
            "task-self".into(),
            Some("worker".into()),
            Some("feature-self".into()),
            None,
            None,
            None,
            default_priority(),
        );
        assert!(created.ok);
        assert_eq!(created.data["status"], "working");
        assert_eq!(created.data["owner"], "worker");
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

        let delivered = handle_task_deliver(
            &server,
            "worker".into(),
            "token-worker".into(),
            "task-heartbeat".into(),
            Some("heartbeat gate passed".into()),
            Some("/tmp/task-heartbeat".into()),
        );
        assert!(delivered.ok);
        assert_eq!(delivered.data["status"], "delivered");

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
    fn master_and_owner_can_put_delivered_task_into_rework() {
        let (server, root) = test_server();
        register_role(&server, "master", "master");
        register_role(&server, "worker", "worker");
        handle_task_register(
            &server,
            "master".into(),
            "token-master".into(),
            "task-rework".into(),
            Some("worker".into()),
            Some("feature-rework".into()),
            None,
            None,
            None,
            default_priority(),
        );

        let delivered = handle_task_deliver(
            &server,
            "worker".into(),
            "token-worker".into(),
            "task-rework".into(),
            Some("rework gate passed".into()),
            Some("/tmp/task-rework".into()),
        );
        assert!(delivered.ok);

        let reworked = handle_task_update(
            &server,
            "master".into(),
            "token-master".into(),
            "task-rework".into(),
            Some("rework".into()),
            Some("fix findings and redeliver".into()),
        );
        assert!(reworked.ok);
        assert_eq!(
            server.state.lock().unwrap().tasks["task-rework"].status,
            "rework"
        );

        let resumed = handle_task_update(
            &server,
            "worker".into(),
            "token-worker".into(),
            "task-rework".into(),
            Some("working".into()),
            None,
        );
        assert!(resumed.ok);
        assert_eq!(
            server.state.lock().unwrap().tasks["task-rework"].status,
            "working"
        );
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
            None,
            None,
            None,
            default_priority(),
        );
        let delivered = handle_task_deliver(
            &server,
            "worker".into(),
            "token-worker".into(),
            "task-held".into(),
            Some("resource gate passed".into()),
            Some("/tmp/task-held".into()),
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

        let merged = handle_task_update(
            &server,
            "master".into(),
            "token-master".into(),
            "task-held".into(),
            Some("merged".into()),
            None,
        );
        assert!(merged.ok);
        let closed = handle_task_close(
            &server,
            "master".into(),
            "token-master".into(),
            "task-held".into(),
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
        let second_claim = handle_task_claim(
            &server,
            "worker".into(),
            "token-worker".into(),
            "task-second".into(),
        );
        assert!(second_claim.ok);

        std::fs::remove_dir_all(root).ok();
    }

    #[test]
    fn master_can_claim_available_task_as_owner() {
        let (server, root) = test_server();
        register_role(&server, "master", "master");
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
        let claimed = handle_task_claim(
            &server,
            "master".into(),
            "token-master".into(),
            "task-master-claim".into(),
        );
        assert!(claimed.ok);
        assert_eq!(claimed.data["owner"], "master");
        assert_eq!(claimed.data["status"], "working");
        std::fs::remove_dir_all(root).ok();
    }

    #[test]
    fn deliver_notifies_master_and_waits_for_close_before_next_claim() {
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
            Some("/tmp/claimed".into()),
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

        let missing_evidence = handle_task_deliver(
            &server,
            "worker".into(),
            "token-worker".into(),
            "claimed".into(),
            None,
            None,
        );
        assert!(!missing_evidence.ok);
        assert!(missing_evidence
            .error
            .unwrap()
            .contains("requires non-empty --evidence"));

        let delivered = handle_task_deliver(
            &server,
            "worker".into(),
            "token-worker".into(),
            "claimed".into(),
            Some("all gates pass".into()),
            Some("/tmp/claimed".into()),
        );
        assert!(delivered.ok);
        assert_eq!(delivered.data["status"], "delivered");
        assert_eq!(delivered.data["master_notified"], "master");
        assert_eq!(delivered.data["identity"]["worker_id"], "worker");
        assert!(delivered.data["available_tasks"].is_array());
        let second_claim = handle_task_claim(
            &server,
            "worker".into(),
            "token-worker".into(),
            "high".into(),
        );
        assert!(second_claim.ok);

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
            handle_task_deliver(
                &server,
                "worker".into(),
                "token-worker".into(),
                "task-close".into(),
                Some("close gate passed".into()),
                Some("/tmp/task-close".into()),
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

        let merged = handle_task_update(
            &server,
            "master".into(),
            "token-master".into(),
            "task-close".into(),
            Some("merged".into()),
            None,
        );
        assert!(merged.ok);
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
    fn worker_block_records_state_and_notifies_master() {
        let (server, root) = test_server();
        register_role(&server, "master", "master");
        register_role(&server, "worker", "worker");
        let created = handle_task_register(
            &server,
            "master".into(),
            "token-master".into(),
            "task-blocked".into(),
            None,
            Some("feature-blocked".into()),
            None,
            None,
            None,
            default_priority(),
        );
        assert!(created.ok);
        assert!(
            handle_task_claim(
                &server,
                "worker".into(),
                "token-worker".into(),
                "task-blocked".into(),
            )
            .ok
        );

        let blocked = handle_task_update(
            &server,
            "worker".into(),
            "token-worker".into(),
            "task-blocked".into(),
            Some("blocked".into()),
            Some("waiting for upstream schema".into()),
        );
        assert!(blocked.ok);
        assert_eq!(blocked.data["status"], "blocked");
        assert_eq!(blocked.data["block_notification"]["target"], "master");
        let st = server.state.lock().unwrap();
        assert_eq!(st.tasks["task-blocked"].status, "blocked");
        assert!(st.msgs.values().any(|m| {
            m.from == "collab-server"
                && m.to == "master"
                && m.body.contains("TASK_BLOCKED id=task-blocked")
        }));
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
            handle_task_deliver(
                &server,
                "worker-a".into(),
                "token-worker-a".into(),
                "task-close".into(),
                Some("dispatch gate passed".into()),
                Some("/tmp/task-close".into()),
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

        let merged = handle_task_update(
            &server,
            "master".into(),
            "token-master".into(),
            "task-close".into(),
            Some("merged".into()),
            None,
        );
        assert!(merged.ok);
        let closed = handle_task_close(
            &server,
            "master".into(),
            "token-master".into(),
            "task-close".into(),
        );
        assert!(closed.ok);
        let dispatched = closed.data["dispatched_tasks"].as_array().unwrap();
        assert_eq!(dispatched.len(), 2);
        assert_eq!(dispatched[0]["worker"], "worker-a");
        assert_eq!(dispatched[0]["status"], "offered");
        assert_eq!(dispatched[1]["worker"], "worker-b");
        assert_eq!(closed.data["available_tasks"].as_array().unwrap().len(), 2);

        let st = server.state.lock().unwrap();
        assert_eq!(st.tasks["dispatch-high"].status, "available");
        assert_eq!(st.tasks["dispatch-high"].owner, "master");
        assert_eq!(st.tasks["dispatch-low"].status, "available");
        assert_eq!(st.tasks["dispatch-low"].owner, "master");
        let offered = st
            .msgs
            .values()
            .find(|m| m.from == "collab-server" && m.body.contains("TASK_OFFER"))
            .unwrap();
        assert!(offered.body.contains("dispatch-high"));
        assert!(offered.body.contains("dispatch-low"));
        drop(st);
        std::fs::remove_dir_all(root).ok();
    }

    #[test]
    fn dispatch_command_offers_available_tasks_to_idle_workers() {
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
        assert_eq!(tasks[0]["worker"], "worker-a");
        assert_eq!(tasks[0]["status"], "offered");
        assert_eq!(tasks[1]["worker"], "worker-b");
        assert_eq!(tasks[1]["status"], "offered");
        assert_eq!(
            dispatched.data["available_tasks"].as_array().unwrap().len(),
            2
        );

        let st = server.state.lock().unwrap();
        assert_eq!(st.tasks["dispatch-high"].status, "available");
        assert_eq!(st.tasks["dispatch-high"].owner, "master");
        assert_eq!(st.tasks["dispatch-low"].status, "available");
        assert_eq!(st.tasks["dispatch-low"].owner, "master");
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
        assert_eq!(
            events.len(),
            1,
            "offer message only; worker claims separately"
        );
        assert_eq!(knocks.len(), 1);
        assert_eq!(dispatched.len(), 1);
        assert_eq!(
            dispatched[0]["worker"], "worker-live-pane",
            "offer must target worker-live-pane"
        );
        assert_eq!(dispatched[0]["status"], "offered");

        assert_eq!(
            st.tasks["dispatch-reachable"].owner, "master",
            "offer must not mutate task owner"
        );
        assert_eq!(st.tasks["dispatch-reachable"].status, "available");
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
