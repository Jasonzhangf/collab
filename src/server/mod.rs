pub mod knock;
pub mod state;
pub mod timers;

use crate::proto::{Req, Resp, MSG_TYPES};
use crate::scope::Scope;
use crate::server::knock::{append_log, knock_or_log, pane_alive, pane_idle};
use serde_json::json;
use state::{
    now_ms, runtime_for_pane, task_resource_active, wait_cycle, Event, Message, MigrationRecord,
    NotificationSubscription, State, TaskRec, WaitSpec, WorkerRec, MAX_WAKE_ATTEMPTS,
};
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};
use tokio::net::UnixListener;

const MAX_POLL_MS: u64 = 3_600_000;
const POLL_TICK_MS: u64 = 250;
const TASK_STATUSES: [&str; 10] = [
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

fn task_claim_held(status: &str) -> bool {
    matches!(
        status,
        "working" | "blocked" | "verifying" | "reviewed" | "delivered" | "rework" | "merged"
    )
}

fn task_transition_allowed(current: &str, next: &str) -> bool {
    current == next
        || matches!(
            (current, next),
            ("working", "blocked" | "verifying" | "cancelled")
                | ("blocked", "working" | "cancelled")
                | (
                    "verifying",
                    "working" | "blocked" | "reviewed" | "cancelled"
                )
                | ("reviewed", "blocked" | "rework" | "cancelled")
                | ("rework", "working" | "blocked" | "verifying" | "cancelled")
                | ("delivered", "rework" | "merged" | "cancelled")
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

fn notification_text(message_id: &str) -> String {
    format!("COLLAB_NOTIFY {message_id}")
}

pub(super) fn queue_system_knock(server: &Server, pane: &str, msg_id: &str) -> bool {
    knock_or_log(&server.log_path(), pane, &notification_text(msg_id))
}

fn iso(ms: i64) -> String {
    chrono::DateTime::from_timestamp_millis(ms)
        .map(|d| d.to_rfc3339())
        .unwrap_or_default()
}

const MAX_NOTIFICATION_TTL_SECONDS: u64 = 7 * 24 * 60 * 60;
const NOTIFICATION_EVENTS: [&str; 4] = [
    "direct-message",
    "resource-released",
    "deadline",
    "async-result",
];

fn attempt_notification_with(
    server: &Server,
    message_id: &str,
    subscription_id: &str,
    is_waiting: &dyn Fn(&str) -> bool,
    deliver: &dyn Fn(&str, &str) -> bool,
) -> bool {
    let (pane, exhausted) = {
        let state = server.state.lock().unwrap();
        let Some(message) = state.msgs.get(message_id) else {
            return false;
        };
        let Some(subscription) = state.notification_subscriptions.get(subscription_id) else {
            return false;
        };
        let pane_matches = state
            .workers
            .get(&subscription.worker_id)
            .and_then(|worker| worker.pane.as_deref())
            == Some(subscription.pane.as_str());
        let valid = message.to == subscription.worker_id
            && message.state == "pending"
            && message.wake_attempt_count < MAX_WAKE_ATTEMPTS
            && subscription.status == "armed"
            && subscription.expires_ms > now_ms()
            && pane_matches;
        if !valid || !is_waiting(&subscription.pane) {
            return false;
        }
        (
            subscription.pane.clone(),
            message.wake_attempt_count + 1 >= MAX_WAKE_ATTEMPTS,
        )
    };

    server.commit(&[Event::WakeAttempted {
        ids: vec![message_id.to_string()],
        attempted_ms: now_ms(),
    }]);
    if deliver(&pane, message_id) {
        server.commit(&[
            Event::Delivered {
                ids: vec![message_id.to_string()],
            },
            Event::NotificationConsumed {
                subscription_id: subscription_id.to_string(),
                message_id: message_id.to_string(),
                consumed_ms: now_ms(),
            },
        ]);
        true
    } else {
        if exhausted {
            server.commit(&[Event::NotificationStatus {
                subscription_id: subscription_id.to_string(),
                status: "attempts-exhausted".into(),
                updated_ms: now_ms(),
            }]);
        }
        false
    }
}

fn attempt_notification(server: &Server, message_id: &str, subscription_id: &str) -> bool {
    attempt_notification_with(
        server,
        message_id,
        subscription_id,
        &pane_idle,
        &|pane, id| queue_system_knock(server, pane, id),
    )
}

fn handle_notification_subscribe(
    server: &Server,
    worker_id: String,
    token: String,
    event: String,
    subject: Option<String>,
    trigger_ms: Option<i64>,
    ttl_seconds: u64,
) -> Resp {
    if !NOTIFICATION_EVENTS.contains(&event.as_str()) {
        return Resp::err(format!(
            "unsupported notification event {}; expected one of {:?}",
            event, NOTIFICATION_EVENTS
        ));
    }
    if ttl_seconds == 0 || ttl_seconds > MAX_NOTIFICATION_TTL_SECONDS {
        return Resp::err(format!(
            "ttl_seconds must be between 1 and {}",
            MAX_NOTIFICATION_TTL_SECONDS
        ));
    }
    let exact_subject_required = event != "direct-message";
    if exact_subject_required != subject.as_deref().is_some_and(|value| !value.is_empty()) {
        return Resp::err(if exact_subject_required {
            "this notification event requires a non-empty exact subject"
        } else {
            "direct-message subscription must not specify a subject"
        });
    }
    if event == "deadline" && trigger_ms.is_none() {
        return Resp::err("deadline subscription requires trigger_ms");
    }
    if event != "deadline" && trigger_ms.is_some() {
        return Resp::err("trigger_ms is valid only for deadline subscriptions");
    }
    let now = now_ms();
    let expires_ms = now.saturating_add((ttl_seconds as i64).saturating_mul(1000));
    if trigger_ms.is_some_and(|trigger| trigger <= now || trigger >= expires_ms) {
        return Resp::err("deadline trigger_ms must be in the future and before expiry");
    }
    let mut state = server.state.lock().unwrap();
    if let Err(error) = verify(&state, &worker_id, &token) {
        return error;
    }
    let Some(pane) = state.worker_pane(&worker_id) else {
        return Resp::err("notification subscription requires a registered tmux pane");
    };
    if runtime_for_pane(Some(&pane)).is_none() {
        return Resp::err("notification subscription method tmux is unavailable for this pane");
    }
    let id = format!("sub-{}", gen_msg_id());
    let subscription = NotificationSubscription {
        id: id.clone(),
        worker_id,
        event,
        subject,
        pane,
        method: "tmux".into(),
        trigger_ms,
        expires_ms,
        status: "armed".into(),
        created_ms: now,
        updated_ms: now,
    };
    server.commit_locked(
        &mut state,
        &[Event::NotificationSubscribed {
            subscription: subscription.clone(),
        }],
    );
    Resp::data(json!({"subscription": subscription, "one_shot": true}))
}

fn handle_notification_status(server: &Server, worker_id: String, token: String) -> Resp {
    let state = server.state.lock().unwrap();
    if let Err(error) = verify(&state, &worker_id, &token) {
        return error;
    }
    let mut subscriptions: Vec<&NotificationSubscription> = state
        .notification_subscriptions
        .values()
        .filter(|subscription| subscription.worker_id == worker_id)
        .collect();
    subscriptions.sort_by_key(|subscription| (subscription.created_ms, &subscription.id));
    Resp::data(json!({"subscriptions": subscriptions}))
}

fn handle_notification_unsubscribe(
    server: &Server,
    worker_id: String,
    token: String,
    subscription_id: String,
) -> Resp {
    let mut state = server.state.lock().unwrap();
    if let Err(error) = verify(&state, &worker_id, &token) {
        return error;
    }
    let Some(subscription) = state.notification_subscriptions.get(&subscription_id) else {
        return Resp::err(format!(
            "notification subscription {} not found",
            subscription_id
        ));
    };
    if subscription.worker_id != worker_id {
        return Resp::err("only the subscription owner may unsubscribe");
    }
    server.commit_locked(
        &mut state,
        &[Event::NotificationStatus {
            subscription_id: subscription_id.clone(),
            status: "cancelled".into(),
            updated_ms: now_ms(),
        }],
    );
    Resp::data(json!({"subscription_id": subscription_id, "status": "cancelled"}))
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

fn migration_issues(server: &Server, state: &State) -> Vec<String> {
    let mut issues = Vec::new();
    for worker in state.workers.values() {
        match worker.pane.as_deref() {
            Some(pane) if pane.starts_with('%') => {
                if !(server.pane_alive_check)(pane) {
                    issues.push(format!("worker {} tmux pane is offline", worker.id));
                }
            }
            _ => issues.push(format!("worker {} is not bound to tmux", worker.id)),
        }
    }
    for task in state.tasks.values() {
        if task.status == "available" {
            issues.push(format!(
                "task {} uses deprecated available/dispatch state and needs an explicit owner decision",
                task.id
            ));
        }
        if let Some(wait) = task.wait.as_ref() {
            if task.status != "waiting" {
                issues.push(format!(
                    "task {} has wait metadata outside waiting",
                    task.id
                ));
            }
            if wait.waiter != task.owner || !state.workers.contains_key(&wait.waiter) {
                issues.push(format!("task {} wait has no valid waiter", task.id));
            }
            if wait.responsible_actor.trim().is_empty()
                || !state.workers.contains_key(&wait.responsible_actor)
            {
                issues.push(format!("task {} wait has no responsible actor", task.id));
            }
            match state.tasks.get(&wait.waiting_for) {
                None => issues.push(format!(
                    "task {} wait points to missing blocking task {}",
                    task.id, wait.waiting_for
                )),
                Some(blocking) => {
                    if !task_resource_active(&blocking.status) {
                        issues.push(format!(
                            "task {} wait points to inactive blocking task {}",
                            task.id, blocking.id
                        ));
                    }
                    if wait.responsible_actor != blocking.owner {
                        issues.push(format!(
                            "task {} wait responsible actor does not own blocking task {}",
                            task.id, blocking.id
                        ));
                    }
                    let same_feature =
                        task.feature_id.is_some() && task.feature_id == blocking.feature_id;
                    let same_worktree = task.worktree_path.is_some()
                        && task.worktree_path == blocking.worktree_path;
                    if !same_feature && !same_worktree {
                        issues.push(format!(
                            "task {} wait has no matching active resource on blocking task {}",
                            task.id, blocking.id
                        ));
                    }
                }
            }
            if wait.deadline_ms <= now_ms() {
                issues.push(format!(
                    "task {} wait deadline is missing or expired",
                    task.id
                ));
            }
            if wait.resume_on.is_empty() || wait.escalation.trim().is_empty() {
                issues.push(format!(
                    "task {} wait has no resume/escalation path",
                    task.id
                ));
            }
            if wait_cycle(&state.tasks, &task.id, &wait.waiting_for) {
                issues.push(format!("task {} participates in a wait cycle", task.id));
            }
        } else if task.status == "waiting" {
            issues.push(format!("task {} is waiting without WaitSpec", task.id));
        }
    }
    issues.sort();
    issues.dedup();
    issues
}

fn snapshot_hash(state: &State) -> String {
    let mut workers: Vec<_> = state
        .workers
        .values()
        .map(|worker| worker.id.clone())
        .collect();
    workers.sort();
    let mut tasks: Vec<_> = state.tasks.values().cloned().collect();
    tasks.sort_by(|left, right| left.id.cmp(&right.id));
    let mut messages: Vec<_> = state.msgs.values().cloned().collect();
    messages.sort_by(|left, right| left.id.cmp(&right.id));
    let mut delivery_modes: Vec<_> = state
        .delivery_modes
        .iter()
        .map(|(id, mode)| (id.clone(), mode.clone()))
        .collect();
    delivery_modes.sort();
    let bytes = serde_json::to_vec(&(workers, tasks, messages, delivery_modes))
        .expect("serialize deterministic migration snapshot");
    let mut hash = 0xcbf29ce484222325_u64;
    for byte in bytes {
        hash ^= u64::from(byte);
        hash = hash.wrapping_mul(0x100000001b3);
    }
    format!("fnv1a64:{hash:016x}")
}

fn migration_peer(state: &State, worker_id: &str, token: &str) -> Result<WorkerRec, Resp> {
    verify(state, worker_id, token)
}

fn verify_migration_lease(state: &State, worker_id: &str) -> Result<(), Resp> {
    if let Some(migration) = state.migration.as_ref().filter(|migration| {
        migration.operator != worker_id && matches!(migration.phase.as_str(), "planned" | "applied")
    }) {
        return Err(Resp::err_data(
            "MIGRATION_TRANSACTION_HELD_BY_ANOTHER_PEER",
            json!({
                "migration": migration,
                "holder": migration.operator,
                "requester": worker_id,
                "admission_frozen": state.admission_frozen(),
                "retry_allowed": false,
                "next": "do not retry plan/apply/verify; query collab migrate inspect, then let the holder complete the current migration or coordinate ownership transfer",
            }),
        ));
    }
    Ok(())
}

fn migration_state_rejection(state: &State, message: &str) -> Resp {
    Resp::err_data(
        message,
        json!({
            "migration": state.migration,
            "admission_frozen": state.admission_frozen(),
            "retry_allowed": false,
            "next": "run collab migrate inspect; do not create a new migration until the current record is resolved",
        }),
    )
}

fn migration_view(state: &State, issues: Vec<String>) -> serde_json::Value {
    json!({
        "migration": state.migration,
        "admissible": issues.is_empty(),
        "issues": issues,
        "state": {
            "workers": state.workers.len(),
            "tasks": state.tasks.len(),
            "messages": state.msgs.len(),
            "snapshot_hash": snapshot_hash(state),
        },
        "deprecated_paths": [
            "delete .agent-collab",
            "manual task/claim/journal JSON edits",
            "clear mailbox",
            "copy worker tokens",
            "start a second daemon",
            "mixed runtime writers",
            "guess pane identity",
        ],
    })
}

fn handle_migration_inspect(server: &Server, worker_id: String, token: String) -> Resp {
    let state = server.state.lock().unwrap();
    if let Err(error) = migration_peer(&state, &worker_id, &token) {
        return error;
    }
    let issues = migration_issues(server, &state);
    Resp::data(migration_view(&state, issues))
}

fn handle_migration_plan(server: &Server, worker_id: String, token: String) -> Resp {
    let mut state = server.state.lock().unwrap();
    if let Err(error) = migration_peer(&state, &worker_id, &token) {
        return error;
    }
    if let Err(error) = verify_migration_lease(&state, &worker_id) {
        return error;
    }
    if state.admission_frozen() {
        return Resp::err("migration admission is already frozen");
    }
    let issues = migration_issues(server, &state);
    let now = now_ms();
    let migration = MigrationRecord {
        id: format!("migration-{now}"),
        from_version: "v1-legacy".into(),
        to_version: "v1-low-intervention".into(),
        phase: if issues.is_empty() {
            "planned".into()
        } else {
            "migration_needs_operator".into()
        },
        admission_frozen: false,
        snapshot_hash: None,
        worker_count: state.workers.len(),
        task_count: state.tasks.len(),
        message_count: state.msgs.len(),
        operator: worker_id,
        issues: issues.clone(),
        created_ms: now,
        updated_ms: now,
    };
    server.commit_locked(
        &mut state,
        &[Event::MigrationUpdated {
            migration: migration.clone(),
        }],
    );
    Resp::data(json!({
        "migration": migration,
        "admissible": issues.is_empty(),
        "issues": issues,
        "next": if state.migration.as_ref().is_some_and(|record| record.phase == "planned") {
            "collab migrate apply"
        } else {
            "resolve every issue, then run collab migrate plan again"
        },
    }))
}

fn handle_migration_apply(server: &Server, worker_id: String, token: String) -> Resp {
    let mut state = server.state.lock().unwrap();
    if let Err(error) = migration_peer(&state, &worker_id, &token) {
        return error;
    }
    if let Err(error) = verify_migration_lease(&state, &worker_id) {
        return error;
    }
    let Some(mut migration) = state.migration.clone() else {
        return Resp::err("run collab migrate plan before apply");
    };
    if migration.phase != "planned" || !migration.issues.is_empty() {
        return Resp::err("migration plan is not admissible");
    }
    let issues = migration_issues(server, &state);
    if !issues.is_empty() {
        return Resp::err(format!(
            "migration admission changed: {}",
            issues.join("; ")
        ));
    }
    migration.phase = "applied".into();
    migration.admission_frozen = true;
    migration.snapshot_hash = Some(snapshot_hash(&state));
    migration.worker_count = state.workers.len();
    migration.task_count = state.tasks.len();
    migration.message_count = state.msgs.len();
    migration.updated_ms = now_ms();
    server.commit_locked(
        &mut state,
        &[Event::MigrationUpdated {
            migration: migration.clone(),
        }],
    );
    Resp::data(json!({
        "migration": migration,
        "admission_frozen": true,
        "next": "upgrade/restart the single daemon, rebind existing tmux identities, then run collab migrate verify",
    }))
}

fn handle_migration_verify(server: &Server, worker_id: String, token: String) -> Resp {
    let mut state = server.state.lock().unwrap();
    if let Err(error) = migration_peer(&state, &worker_id, &token) {
        return error;
    }
    if let Err(error) = verify_migration_lease(&state, &worker_id) {
        return error;
    }
    let Some(mut migration) = state.migration.clone() else {
        return migration_state_rejection(&state, "no migration record to verify");
    };
    if migration.phase == "verified" && !migration.admission_frozen {
        let current_snapshot_hash = snapshot_hash(&state);
        return Resp::data(json!({
            "migration": migration,
            "verified": true,
            "resumed": false,
            "idempotent": true,
            "issues": [],
            "current_snapshot_hash": current_snapshot_hash,
            "next": "migration already verified; continue task lifecycle; do not rerun plan or apply",
        }));
    }
    if migration.phase != "applied" || !migration.admission_frozen {
        return migration_state_rejection(
            &state,
            "migration must be applied and frozen before verify",
        );
    }
    let mut issues = migration_issues(server, &state);
    let current_hash = snapshot_hash(&state);
    if migration.snapshot_hash.as_deref() != Some(current_hash.as_str()) {
        issues.push("migration snapshot hash mismatch".into());
    }
    if migration.worker_count != state.workers.len()
        || migration.task_count != state.tasks.len()
        || migration.message_count != state.msgs.len()
    {
        issues.push("migration state counts changed during admission freeze".into());
    }
    issues.sort();
    issues.dedup();
    migration.updated_ms = now_ms();
    migration.issues = issues.clone();
    if issues.is_empty() {
        migration.phase = "verified".into();
        migration.admission_frozen = false;
    } else {
        migration.phase = "migration_needs_operator".into();
    }
    server.commit_locked(
        &mut state,
        &[Event::MigrationUpdated {
            migration: migration.clone(),
        }],
    );
    Resp::data(json!({
        "migration": migration,
        "verified": issues.is_empty(),
        "resumed": issues.is_empty(),
        "issues": issues,
        "current_snapshot_hash": current_hash,
    }))
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
        return Resp::err("collab registration requires a live tmux pane");
    };
    if st.admission_frozen() && !st.workers.contains_key(&worker_id) {
        return Resp::err("MIGRATION_ADMISSION_FROZEN: only an existing tmux identity may rebind");
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
                };
                server.commit_locked(&mut st, &[Event::Registered { worker: refreshed }]);
                return Resp::data(json!({
                    "worker_id": worker_id,
                    "identity_kind": "peer",
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
            return Resp::err("existing peer has no valid tmux pane");
        };
        if existing_runtime != runtime {
            return Resp::err(format!(
                "worker {} cannot change runtime from {} to {}",
                worker_id, existing_runtime, runtime
            ));
        }
        let refreshed = WorkerRec {
            id: worker_id.clone(),
            token: existing.token.clone(),
            pane: pane.or_else(|| existing.pane.clone()),
            cwd,
            registered_ms: existing.registered_ms,
        };
        server.commit_locked(&mut st, &[Event::Registered { worker: refreshed }]);
        return Resp::data(
            json!({"worker_id": worker_id, "identity_kind": "peer", "runtime": runtime, "reused": true}),
        );
    }
    let rec = WorkerRec {
        id: worker_id.clone(),
        token,
        pane,
        cwd,
        registered_ms: now_ms(),
    };
    server.commit_locked(
        &mut st,
        &[Event::Registered {
            worker: rec.clone(),
        }],
    );
    Resp::data(json!({
        "worker_id": worker_id,
        "identity_kind": "peer",
        "runtime": runtime,
        "registered_at": iso(rec.registered_ms)
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
    if mtype != "notify"
        || !(body.starts_with("RESOURCE_OCCUPIED ") || body.starts_with("RESOURCE_RELEASED "))
    {
        return Resp::err(
            "peer messaging is limited to RESOURCE_OCCUPIED/RESOURCE_RELEASED coordination",
        );
    }
    if delivery_mode != "immediate" {
        return Resp::err(
            "implicit idle delivery is removed; use an explicit notification subscription",
        );
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
    if runtime_for_pane(sender.pane.as_deref()).is_none() {
        return Resp::err("sender has no valid tmux pane");
    }
    if runtime_for_pane(recipient.pane.as_deref()).is_none() {
        return Resp::err("recipient has no valid tmux pane");
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
        wake_attempt_count: 0,
        last_wake_attempt_ms: 0,
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
    let subscription = st
        .matching_subscription(&to, "direct-message", None, now_ms())
        .cloned();
    let mut events = vec![Event::Sent { msg }];
    events.push(Event::DeliveryMode {
        msg_id: mid.clone(),
        mode: "explicit-notification".into(),
    });
    if let Some(subscription) = &subscription {
        events.push(Event::WakeBound {
            message_id: mid.clone(),
            subscription_id: subscription.id.clone(),
        });
    }
    if !superseded_ids.is_empty() {
        events.push(Event::Superseded {
            ids: superseded_ids,
        });
    }
    server.commit_locked(&mut st, &events);
    drop(st);
    let notified = subscription
        .as_ref()
        .is_some_and(|subscription| attempt_notification(server, &mid, &subscription.id));
    Resp::data(json!({
        "msg_id": mid,
        "durable": true,
        "notification": if subscription.is_none() {
            "mailbox-only-no-subscription"
        } else if notified {
            "sent"
        } else {
            "subscribed-not-sent"
        }
    }))
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
    if goal_prompt.is_some() {
        return Resp::err(
            "/goal registration is deferred; register the peer-owned task without --goal-prompt",
        );
    }
    if owner.as_deref().is_some_and(|owner| owner != worker_id) {
        return Resp::err("peer may register only its own task; omit --owner or use its worker_id");
    }
    if !matches!(priority.as_str(), "p0" | "p1" | "p2" | "p3" | "p4") {
        return Resp::err(format!(
            "invalid priority {}; must be p0, p1, p2, p3, or p4",
            priority
        ));
    }
    let task_owner = worker_id.clone();
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
    if let Some(existing) = st
        .tasks
        .values()
        .find(|task| {
            task_resource_active(&task.status)
                && (feature_id.is_some() && task.feature_id == feature_id
                    || worktree_path.is_some() && task.worktree_path == worktree_path)
        })
        .cloned()
    {
        let now = now_ms();
        let blocked_task = TaskRec {
            id: task_id.clone(),
            owner: worker_id.clone(),
            created_by: worker_id.clone(),
            feature_id: feature_id.clone(),
            worktree_path: worktree_path.clone(),
            branch: branch.clone(),
            base_commit: base_commit.clone(),
            priority: priority.clone(),
            status: "blocked".into(),
            next_step: Some(format!("RESOURCE_CONFLICT={}", existing.id)),
            wait: None,
            created_ms: now,
            updated_ms: now,
        };
        server.commit_locked(&mut st, &[Event::TaskCreated { task: blocked_task }]);
        return Resp::err_data(
            "TASK_RESOURCE_CONFLICT",
            json!({
                "requested_task": task_id,
                "blocking_task": existing.id,
                "responsible_actor": existing.owner,
                "status": "blocked",
                "notification": "none; use explicit sendmessage when coordination is needed",
            }),
        );
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
        status: "working".to_string(),
        next_step,
        wait: None,
        created_ms: now,
        updated_ms: now,
    };
    server.commit_locked(&mut st, &[Event::TaskCreated { task: task.clone() }]);
    Resp::data(json!({"task": task.id, "owner": task.owner, "status": task.status}))
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
    let Some(worker) = st.workers.get(&worker_id).cloned() else {
        return Resp::err(format!("worker {} not registered", worker_id));
    };
    if worker.token != token {
        return Resp::err("token mismatch: identity does not own this worker_id");
    }
    if let Err(e) = validate_worktree_path(&server.root, &worktree_path) {
        return Resp::err(e);
    }
    let Some(mut task) = st.tasks.get(&task_id).cloned() else {
        return Resp::err(format!("task {} not found", task_id));
    };
    if task.owner != worker_id {
        return Resp::err("only the task owner may relocate its worktree");
    }
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
    let st = server.state.lock().unwrap();
    if let Err(error) = verify(&st, &worker_id, &token) {
        return error;
    }
    Resp::err("central task dispatch is deprecated; each peer registers and owns its task")
}

fn handle_task_claim(server: &Server, worker_id: String, token: String, task_id: String) -> Resp {
    let st = server.state.lock().unwrap();
    if let Err(error) = verify(&st, &worker_id, &token) {
        return error;
    }
    Resp::err(format!(
        "task claim is deprecated; peer must self-register task {}",
        task_id
    ))
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
    if task.owner != worker_id {
        return Resp::err("only the task owner may update its lifecycle");
    }
    if let Some(new_status) = status {
        if !TASK_STATUSES.contains(&new_status.as_str()) {
            return Resp::err(format!(
                "invalid status {}; must be one of {:?}",
                new_status, TASK_STATUSES
            ));
        }
        if new_status == "closed" {
            return Resp::err("use collab task close after owner merge and cleanup verification");
        }
        if new_status == "delivered" {
            return Resp::err(
                "use collab task deliver to complete a claim; direct status mutation is rejected",
            );
        }
        if new_status == "waiting" {
            return Resp::err("use collab task wait so responsibility and deadline are durable");
        }
        if !task_transition_allowed(&task.status, &new_status) {
            return Resp::err(format!(
                "invalid task transition {} -> {}",
                task.status, new_status
            ));
        }
        task.status = new_status.clone();
        if new_status != "waiting" {
            task.wait = None;
        }
    }
    if next_step.is_some() {
        task.next_step = next_step;
    }
    task.updated_ms = now_ms();
    server.commit_locked(&mut st, &[Event::TaskUpdated { task: task.clone() }]);
    Resp::data(json!({
        "task": task.id,
        "status": task.status,
        "owner": task.owner,
        "notification": "none",
        "next_action": task.next_step,
    }))
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
                "pane": worker.pane,
                "active_tasks": active_tasks,
                "action": "peer owns cleanup; daemon operator may inspect during migration"
            })
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
    if task.owner != worker_id || task.status != "reviewed" {
        return Resp::err(format!(
            "task {} must be reviewed by its owner before delivery (current: {})",
            task_id, task.status
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
    task.status = "delivered".to_string();
    task.wait = None;
    task.next_step = Some(
        "sync latest main, verify the exact candidate, integrate to main, then mark merged"
            .to_string(),
    );
    task.updated_ms = now_ms();
    server.commit_locked(&mut st, &[Event::TaskUpdated { task: task.clone() }]);

    Resp::data(json!({
        "delivered": task.id,
        "status": task.status,
        "evidence": evidence,
        "worktree": worktree,
        "notification": "none",
        "next_action": task.next_step,
        "identity": {"worker_id": worker.id, "kind": "peer"},
    }))
}

fn handle_task_close(server: &Server, worker_id: String, token: String, task_id: String) -> Resp {
    let mut st = server.state.lock().unwrap();
    let Some(worker) = st.workers.get(&worker_id).cloned() else {
        return Resp::err(format!("worker {} not registered", worker_id));
    };
    if worker.token != token {
        return Resp::err("token mismatch: identity does not own this worker_id");
    }
    let Some(task) = st.tasks.get(&task_id).cloned() else {
        return Resp::err(format!("task {} not found", task_id));
    };
    if task.owner != worker_id {
        return Resp::err("only the task owner may close its lifecycle");
    }
    if task.status != "merged" {
        return Resp::err(format!(
            "task {} must be merged by its owner before close (current: {})",
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
    closed.next_step = Some("closed after owner merge and cleanup".to_string());
    closed.updated_ms = now_ms();
    server.commit_locked(
        &mut st,
        &[Event::TaskUpdated {
            task: closed.clone(),
        }],
    );

    let waiting: Vec<TaskRec> = st
        .tasks
        .values()
        .filter(|candidate| {
            candidate.status == "waiting"
                && candidate
                    .wait
                    .as_ref()
                    .is_some_and(|wait| wait.waiting_for == closed.id)
        })
        .cloned()
        .collect();
    let mut subscribed_notifications = Vec::new();
    for mut waiter_task in waiting {
        waiter_task.status = "blocked".into();
        waiter_task.wait = None;
        waiter_task.next_step = Some(format!(
            "RESOURCE_RELEASED={} recheck conflicts, then resume only after Server confirms free",
            closed.id
        ));
        waiter_task.updated_ms = now_ms();
        let waiter = waiter_task.owner.clone();
        let subscription = st
            .matching_subscription(&waiter, "resource-released", Some(&closed.id), now_ms())
            .cloned();
        let mut events = vec![Event::TaskUpdated { task: waiter_task }];
        if let Some(subscription) = subscription {
            let message_id = gen_msg_id();
            events.extend([
                Event::Sent {
                    msg: Message {
                        id: message_id.clone(),
                        from: "collab-server".into(),
                        to: waiter,
                        mtype: "notification".into(),
                        body: format!("RESOURCE_RELEASED subject={}", closed.id),
                        in_reply_to: None,
                        created_ms: now_ms(),
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
                    msg_id: message_id.clone(),
                    mode: "explicit-notification".into(),
                },
            ]);
            subscribed_notifications.push((message_id, subscription.id));
        }
        server.commit_locked(&mut st, &events);
    }

    let stale_workers = stale_worker_views(&st, &server.pane_alive_check);
    drop(st);
    for (message_id, subscription_id) in subscribed_notifications {
        attempt_notification(server, &message_id, &subscription_id);
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
        "stale_workers": stale_workers,
        "notification": "subscribed resource waiters only",
        "next_action": "lifecycle complete",
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
        "wait": task.wait,
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
        .filter(|task| task.owner == worker_id)
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
        "identity": {"worker_id": worker.id, "kind": "peer", "pane": worker.pane},
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
    if worker.token != token {
        return Resp::err("token mismatch: identity does not own this worker_id");
    }
    let Some(mut task) = st.tasks.get(&task_id).cloned() else {
        return Resp::err(format!("task {} not found", task_id));
    };
    if task.owner != worker_id || !task_claim_held(&task.status) {
        return Resp::err("only an owned active task may enter waiting");
    }
    if matches!(
        task.status.as_str(),
        "delivered" | "merged" | "closed" | "cancelled"
    ) {
        return Resp::err("terminal or delivered task may not enter waiting");
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
    if blocking.owner == worker_id || !st.workers.contains_key(&blocking.owner) {
        return Resp::err("WAIT_RESPONSIBLE_ACTOR_MISSING");
    }
    let responsible_actor = blocking.owner.clone();
    task.status = "waiting".into();
    task.next_step = Some(format!("WAITING_FOR={}", blocking_task_id));
    task.wait = Some(WaitSpec {
        waiter: worker_id.clone(),
        waiting_for: blocking_task_id.clone(),
        responsible_actor: responsible_actor.clone(),
        reason: "resource_conflict".into(),
        deadline_ms: now_ms() + 15 * 60 * 1000,
        resume_on: vec![
            "resource_released".into(),
            "rework".into(),
            "cancelled".into(),
        ],
        escalation: "resource_owner_and_waiter_recheck".into(),
    });
    task.updated_ms = now_ms();
    server.commit_locked(&mut st, &[Event::TaskUpdated { task: task.clone() }]);
    Resp::data(json!({
        "task": task.id,
        "status": task.status,
        "waiting_for": blocking_task_id,
        "responsible_actor": responsible_actor,
        "deadline_ms": task.wait.as_ref().map(|wait| wait.deadline_ms),
        "notification": "none; subscribe for release/deadline or use explicit sendmessage",
    }))
}

// ---------- dispatch ----------

fn mutation_blocked_during_migration(req: &Req) -> bool {
    match req {
        Req::Send { .. }
        | Req::NotificationSubscribe { .. }
        | Req::NotificationUnsubscribe { .. }
        | Req::Poll { .. }
        | Req::Ack { .. }
        | Req::TaskRegister { .. }
        | Req::TaskRelocate { .. }
        | Req::TaskUpdate { .. }
        | Req::TaskClaim { .. }
        | Req::TaskWait { .. }
        | Req::TaskDeliver { .. }
        | Req::TaskClose { .. }
        | Req::TaskDispatch { .. }
        | Req::MigrationPlan { .. }
        | Req::MigrationApply { .. }
        | Req::TransferMaster { .. }
        | Req::RemoveWorker { .. }
        | Req::ResetBindings { .. } => true,
        Req::Register { .. }
        | Req::NotificationMethods
        | Req::NotificationStatus { .. }
        | Req::Inbox { .. }
        | Req::Context { .. }
        | Req::MsgStatus { .. }
        | Req::TaskStatus { .. }
        | Req::TaskConflicts { .. }
        | Req::MigrationInspect { .. }
        | Req::MigrationVerify { .. }
        | Req::Role { .. }
        | Req::Workers
        | Req::MasterId
        | Req::MasterRecover { .. }
        | Req::Shutdown { .. }
        | Req::Ping => false,
    }
}

fn dispatch(server: &Arc<Server>, req: Req) -> Resp {
    if mutation_blocked_during_migration(&req) && server.state.lock().unwrap().admission_frozen() {
        return Resp::err(
            "MIGRATION_ADMISSION_FROZEN: only identity rebind, read queries, daemon restart, and migration verify are allowed",
        );
    }
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
        Req::NotificationMethods => Resp::data(json!({
            "methods": ["tmux"],
            "events": NOTIFICATION_EVENTS,
            "one_shot": true,
            "max_lifetime_attempts": MAX_WAKE_ATTEMPTS,
            "max_ttl_seconds": MAX_NOTIFICATION_TTL_SECONDS,
        })),
        Req::NotificationSubscribe {
            worker_id,
            token,
            event,
            subject,
            trigger_ms,
            ttl_seconds,
        } => handle_notification_subscribe(
            server,
            worker_id,
            token,
            event,
            subject,
            trigger_ms,
            ttl_seconds,
        ),
        Req::NotificationStatus { worker_id, token } => {
            handle_notification_status(server, worker_id, token)
        }
        Req::NotificationUnsubscribe {
            worker_id,
            token,
            subscription_id,
        } => handle_notification_unsubscribe(server, worker_id, token, subscription_id),
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
            drop(st);
            if owned.is_empty() {
                return Resp::err("no ackable messages (must address your own inbox)");
            }
            server.commit(&[Event::Acked { ids: owned.clone() }]);
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
                    "state": m.state, "wake_attempts": m.wake_attempt_count,
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
        Req::MigrationInspect { worker_id, token } => {
            handle_migration_inspect(server, worker_id, token)
        }
        Req::MigrationPlan { worker_id, token } => handle_migration_plan(server, worker_id, token),
        Req::MigrationApply { worker_id, token } => {
            handle_migration_apply(server, worker_id, token)
        }
        Req::MigrationVerify { worker_id, token } => {
            handle_migration_verify(server, worker_id, token)
        }
        Req::Role { worker_id: _ } => {
            Resp::err("declared roles are removed; use collab who/context for peer identity")
        }
        Req::Workers => {
            let st = server.state.lock().unwrap();
            let workers: Vec<serde_json::Value> = st
                .workers
                .values()
                .map(|w| {
                    let active = st.tasks.values().find(|task| {
                        task.owner == w.id
                            && !matches!(task.status.as_str(), "closed" | "cancelled")
                    });
                    json!({
                        "id": w.id,
                        "pane": w.pane,
                        "endpoint_live": w.pane.as_deref().is_some_and(pane_alive),
                        "active_task": active.map(|task| task.id.as_str()),
                        "active_status": active.map(|task| task.status.as_str()),
                    })
                })
                .collect();
            Resp::data(json!({
                "workers": workers,
                "count": workers.len()
            }))
        }
        Req::MasterId => Resp::err("permanent master role is deprecated; all identities are peers"),
        Req::MasterRecover {
            worker_id: _,
            token: _,
            session: _,
        } => Resp::err("master recovery is deprecated; re-register the peer identity"),
        Req::TransferMaster {
            worker_id: _,
            token: _,
            target_id: _,
        } => Resp::err("master transfer is deprecated; authority is task-scoped"),
        Req::RemoveWorker {
            worker_id: _,
            token: _,
            target_id: _,
            force: _,
        } => Resp::err("remove-worker is deprecated; use task-owner cleanup and migration verify"),
        Req::ResetBindings { confirm: _ } => Resp::err(
            "binding reset is deprecated; preserve journal/mailbox and use migration rebind",
        ),
        Req::Shutdown { operator } if operator => Resp::data(json!({
            "authorized": true,
            "capability": "daemon-operator",
        })),
        Req::Shutdown { .. } => Resp::err("shutdown requires an explicit daemon-operator action"),
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
    for (index, line) in content.lines().enumerate() {
        if line.trim().is_empty() {
            continue;
        }
        let event = serde_json::from_str::<Event>(line).map_err(|error| {
            anyhow::anyhow!(
                "journal replay failed at line {}: {}; manual journal edits are unsupported",
                index + 1,
                error
            )
        })?;
        st.apply(&event);
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
    std::fs::write(
        server_dir.join("server.pid"),
        std::process::id().to_string(),
    )?;
    record_activity(
        &scope.root,
        "daemon_start",
        json!({"pid": std::process::id()}),
    );

    // Background scheduler: bounded waits and explicitly registered notifications.
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
mod peer_tests;
