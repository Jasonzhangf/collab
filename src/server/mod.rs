pub(crate) mod keepalive;
pub mod knock;
pub mod state;
pub mod timers;

use crate::proto::{Req, Resp, MSG_TYPES};
use crate::scope::Scope;
use crate::server::knock::{append_log, knock_or_log, pane_alive, pane_idle};
use serde_json::json;
use state::{
    now_ms, runtime_for_pane, task_resource_active, wait_cycle, CleanupReceipt, Event, Message,
    MigrationRecord, NotificationSubscription, State, TaskRec, WaitSpec, WorkerRec,
    MAX_WAKE_ATTEMPTS,
};
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};
use tokio::net::UnixListener;

const MAX_POLL_MS: u64 = 3_600_000;
const POLL_TICK_MS: u64 = 250;
const TASK_STATUSES: [&str; 11] = [
    "assigned",
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
    pub config: crate::config::Config,
    pub root: PathBuf,
    pub state: Mutex<State>,
    pub journal: Mutex<std::fs::File>,
    pub pane_alive_check: fn(&str) -> bool,
    pub pane_owner_check: fn(&str, &str) -> bool,
    pub pane_state_check: fn(&str) -> crate::server::knock::AgentState,
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
        obj.remove("launch_env");
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
    pub(crate) fn commit(&self, evs: &[Event]) {
        let mut st = self.state.lock().unwrap();
        self.commit_locked(&mut st, evs);
    }

    pub(crate) fn commit_locked(&self, st: &mut State, evs: &[Event]) {
        let mut j = self.journal.lock().unwrap();
        use std::io::Write;
        // Persist control truth before any state change or external notification.
        // A failed journal poisons this owner instead of silently resetting budgets.
        for ev in evs {
            let line = serde_json::to_string(ev).expect("serialize event");
            writeln!(j, "{}", line).expect("journal append failed; refusing state mutation");
        }
        j.sync_data()
            .expect("journal sync failed; refusing state mutation");
        for ev in evs {
            st.apply(ev);
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

const MAX_NOTIFICATION_SUBJECT_CHARS: usize = 48;
#[cfg(test)]
const DIRECT_MESSAGE_WAKE_COOLDOWN_MS: i64 = 60_000;

fn abbreviated_subject(subject: &str) -> Option<String> {
    let normalized = subject.split_whitespace().collect::<Vec<_>>().join(" ");
    if normalized.is_empty() {
        return None;
    }
    if normalized.chars().count() <= MAX_NOTIFICATION_SUBJECT_CHARS {
        return Some(normalized);
    }
    let mut abbreviated = normalized
        .chars()
        .take(MAX_NOTIFICATION_SUBJECT_CHARS - 1)
        .collect::<String>();
    abbreviated.push('…');
    Some(abbreviated)
}

fn visible_body(body: &str) -> String {
    let mut visible = String::with_capacity(body.len());
    for ch in body.chars() {
        match ch {
            '\n' => visible.push_str("\\n"),
            '\r' => visible.push_str("\\r"),
            '\t' => visible.push_str("\\t"),
            ch if ch.is_control() => visible.push_str(&format!("\\u{{{:x}}}", ch as u32)),
            ch => visible.push(ch),
        }
    }
    visible
}

fn notification_text(message: &Message) -> Option<String> {
    let subject = abbreviated_subject(message.subject.as_deref()?)?;
    Some(format!(
        "COLLAB_NOTIFY {} [{}] {} | ACTION: weigh priority from the ID and subject. When selected, run collab msg {}, then execute the actionable in-scope request; do not stop at ACK or waiting.",
        message.id,
        subject,
        visible_body(&message.body),
        message.id
    ))
}

fn iso(ms: i64) -> String {
    chrono::DateTime::from_timestamp_millis(ms)
        .map(|d| d.to_rfc3339())
        .unwrap_or_default()
}

const MAX_NOTIFICATION_TTL_SECONDS: u64 = 7 * 24 * 60 * 60;
const MAX_ACTIVE_SUBSCRIPTIONS_PER_WORKER: usize = 3;
const NOTIFICATION_EVENTS: [&str; 4] = [
    "direct-message",
    "resource-released",
    "deadline",
    "async-result",
];
const DEFAULT_DIRECT_MESSAGE_TTL_SECONDS: u64 = MAX_NOTIFICATION_TTL_SECONDS;

fn default_direct_message_id(worker_id: &str) -> String {
    format!("sub-default-direct-message-{worker_id}")
}

fn default_direct_message_events(
    state: &State,
    worker_id: &str,
    pane: &str,
    now: i64,
) -> Vec<Event> {
    let mut events = Vec::new();
    let default_id = default_direct_message_id(worker_id);
    let refresh_after_ms = DEFAULT_DIRECT_MESSAGE_TTL_SECONDS as i64 * 1000 / 2;
    let mut current_is_fresh = false;
    for subscription in state.notification_subscriptions.values().filter(|sub| {
        sub.worker_id == worker_id && sub.event == "direct-message" && sub.status == "armed"
    }) {
        if subscription.id == default_id
            && subscription.pane == pane
            && subscription.expires_ms - now >= refresh_after_ms
        {
            current_is_fresh = true;
            continue;
        }
        if subscription.id != default_id {
            events.push(Event::NotificationStatus {
                subscription_id: subscription.id.clone(),
                status: "rebound".into(),
                updated_ms: now,
            });
        }
    }
    if current_is_fresh {
        return events;
    }
    events.push(Event::NotificationSubscribed {
        subscription: NotificationSubscription {
            id: default_id,
            worker_id: worker_id.into(),
            event: "direct-message".into(),
            subject: None,
            pane: pane.into(),
            method: "tmux".into(),
            trigger_ms: None,
            trigger_times_ms: Vec::new(),
            interval_ms: None,
            repeat_count: 1,
            fired_count: 0,
            expires_ms: now.saturating_add(DEFAULT_DIRECT_MESSAGE_TTL_SECONDS as i64 * 1000),
            status: "armed".into(),
            created_ms: now,
            updated_ms: now,
        },
    });
    events
}

fn registered_peer_default_events(
    state: &State,
    now: i64,
    owns_pane: &dyn Fn(&str, &str) -> bool,
) -> Vec<Event> {
    let mut workers = state.workers.values().collect::<Vec<_>>();
    workers.sort_by_key(|worker| worker.id.as_str());
    workers
        .into_iter()
        .filter_map(|worker| {
            let pane = worker.pane.as_deref()?;
            owns_pane(&worker.id, pane).then_some((worker.id.as_str(), pane))
        })
        .flat_map(|(worker_id, pane)| default_direct_message_events(state, worker_id, pane, now))
        .collect()
}

fn restore_registered_peer_default_leases(server: &Server) {
    let events = {
        let state = server.state.lock().unwrap();
        registered_peer_default_events(&state, now_ms(), &|worker_id, pane| {
            tmux_session_for_pane(pane).as_deref() == Some(worker_id)
        })
    };
    if !events.is_empty() {
        server.commit(&events);
    }
}

fn attempt_notification_with(
    server: &Server,
    message_id: &str,
    subscription_id: &str,
    can_receive: &dyn Fn(&str) -> bool,
    deliver: &dyn Fn(&str, &str) -> bool,
    owns_pane: &dyn Fn(&str, &str) -> bool,
) -> bool {
    let now = now_ms();
    if !server.config.notifications.enabled {
        return false;
    }
    let mut state = server.state.lock().unwrap();
    let Some(seed) = state.msgs.get(message_id) else {
        return false;
    };
    let recipient = seed.to.clone();
    let Some(subscription) = state.notification_subscriptions.get(subscription_id) else {
        return false;
    };
    let pane = subscription.pane.clone();
    let delay = server.config.notifications.delay_ms(&subscription.event);
    if subscription.worker_id != recipient {
        return false;
    }
    let worker_opt = state.workers.get(&recipient);
    let worker_pane_mismatch = worker_opt.and_then(|w| w.pane.as_deref()) != Some(&pane);
    let state_probe = (server.pane_state_check)(&pane);
    if worker_opt.is_none()
        || worker_pane_mismatch
        || !(server.pane_alive_check)(&pane)
        || !owns_pane(&recipient, &pane)
        || state_probe == crate::server::knock::AgentState::Absent
    {
        server.commit_locked(
            &mut state,
            &[Event::NotificationStatus {
                subscription_id: subscription_id.to_string(),
                status: "pane-lost".into(),
                updated_ms: now,
            }],
        );
        crate::server::knock::append_log(
            &server.log_path(),
            &format!("notification cancelled pane={pane} recipient={recipient} worker lost, identity mismatch or agent absent"),
        );
        return false;
    }
    let unacked_count = state
        .msgs
        .values()
        .filter(|m| m.to == recipient && m.state == "delivered")
        .count() as u32;
    if unacked_count >= server.config.notifications.max_unacked {
        crate::server::knock::append_log(
            &server.log_path(),
            &format!(
                "knock paused pane={pane} recipient={recipient} unacked={unacked_count}>={} awaiting ack",
                server.config.notifications.max_unacked
            ),
        );
        return false;
    }
    let mut batch = state
        .msgs
        .values()
        .filter_map(|message| {
            let binding = state.wake_bindings.get(&message.id)?;
            let sub = state.notification_subscriptions.get(binding)?;
            (message.to == recipient
                && server.config.notifications.delay_ms(&sub.event) == delay
                && message.state == "pending"
                && message.wake_attempt_count == 0
                && sub.worker_id == recipient
                && sub.pane == pane
                && sub.status == "armed"
                && sub.expires_ms > now
                && state
                    .workers
                    .get(&recipient)
                    .and_then(|w| w.pane.as_deref())
                    == Some(pane.as_str()))
            .then(|| {
                notification_text(message).map(|text| {
                    (
                        message.created_ms,
                        message.id.clone(),
                        binding.clone(),
                        sub.event.clone(),
                        text,
                    )
                })
            })
            .flatten()
        })
        .collect::<Vec<_>>();
    batch.sort_by(|a, b| (a.0, &a.1).cmp(&(b.0, &b.1)));
    let Some(first) = batch.first() else {
        return false;
    };
    let last_attempt = state
        .msgs
        .values()
        .filter(|m| m.to == recipient)
        .map(|m| m.last_wake_attempt_ms)
        .max()
        .unwrap_or(0);
    if now - first.0 < delay || now - last_attempt < delay {
        return false;
    }
    if state_probe == crate::server::knock::AgentState::Working {
        crate::server::knock::append_log(
            &server.log_path(),
            &format!("knock deferred pane={pane} recipient={recipient} agent is working"),
        );
        return false;
    }
    const MAX_BATCH_DELIVERY: usize = 3;
    let total_pending = batch.len();
    let remaining = if total_pending > MAX_BATCH_DELIVERY {
        total_pending - MAX_BATCH_DELIVERY
    } else {
        0
    };
    if total_pending > MAX_BATCH_DELIVERY {
        batch.truncate(MAX_BATCH_DELIVERY);
    }
    let ids = batch.iter().map(|m| m.1.clone()).collect::<Vec<_>>();
    if !can_receive(&pane) {
        server.commit_locked(
            &mut state,
            &[Event::WakeAttempted {
                ids: ids.clone(),
                attempted_ms: now,
            }],
        );
        crate::server::knock::append_log(
            &server.log_path(),
            &format!("knock skipped pane={pane} state={state_probe:?}"),
        );
        return false;
    }
    server.commit_locked(
        &mut state,
        &[Event::WakeAttempted {
            ids: ids.clone(),
            attempted_ms: now,
        }],
    );
    drop(state);
    let mut text_parts = batch
        .iter()
        .map(|m| m.4.clone())
        .collect::<Vec<_>>();
    if remaining > 0 {
        text_parts.push(format!("[+{} more pending in inbox; run collab ack / collab inbox]", remaining));
    }
    if unacked_count + (batch.len() as u32) >= server.config.notifications.max_unacked {
        text_parts.push(format!(
            "[ACK REQUIRED: {} unacked; run collab ack <id> to keep push notifications active]",
            unacked_count + (batch.len() as u32)
        ));
    }
    let text = text_parts.join(" | ");
    if !deliver(&pane, &text) {
        return false;
    }
    let mut events = vec![Event::Delivered { ids }];
    for (_, id, binding, event, _) in batch {
        if event != "direct-message" {
            events.push(Event::NotificationConsumed {
                subscription_id: binding,
                message_id: id,
                consumed_ms: now,
            });
        }
    }
    server.commit(&events);
    true
}

fn attempt_notification(server: &Server, message_id: &str, subscription_id: &str) -> bool {
    attempt_notification_with(
        server,
        message_id,
        subscription_id,
        &knock::pane_idle,
        &|pane, text| knock_or_log(&server.log_path(), pane, text),
        &|worker_id, pane| (server.pane_owner_check)(worker_id, pane),
    )
}

/// Test and migration helper: assume every pane is authoritative so
/// legacy fixtures keep working without threading the owner check through.
#[cfg(test)]
pub(crate) fn attempt_notification_with_default(
    server: &Server,
    message_id: &str,
    subscription_id: &str,
    can_receive: &dyn Fn(&str) -> bool,
    deliver: &dyn Fn(&str, &str) -> bool,
) -> bool {
    attempt_notification_with(
        server,
        message_id,
        subscription_id,
        can_receive,
        deliver,
        &|_, _| true,
    )
}

fn handle_notification_subscribe(
    server: &Server,
    worker_id: String,
    token: String,
    event: String,
    subject: Option<String>,
    trigger_ms: Option<i64>,
    trigger_times_ms: Vec<i64>,
    interval_ms: Option<i64>,
    repeat_count: u32,
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
    if event != "deadline" && (trigger_ms.is_some() || !trigger_times_ms.is_empty() || interval_ms.is_some() || repeat_count != 1) { return Resp::err("schedule options are valid only for deadline subscriptions"); }
    if event == "deadline" && trigger_ms.is_some() && !trigger_times_ms.is_empty() { return Resp::err("use at-ms or trigger-ms, not both"); }
    let now = now_ms();
    let expires_ms = now.saturating_add((ttl_seconds as i64).saturating_mul(1000));
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
    let active = state.notification_subscriptions.values().filter(|s| s.worker_id == worker_id && s.status == "armed").count();
    if active >= MAX_ACTIVE_SUBSCRIPTIONS_PER_WORKER { return Resp::err("maximum 3 active subscriptions per agent"); }
    if event == "deadline" {
        if interval_ms.is_some() && (!trigger_times_ms.is_empty() || trigger_ms.is_some()) { return Resp::err("periodic schedule cannot include an absolute time list"); }
        if interval_ms.is_none() && trigger_times_ms.is_empty() && trigger_ms.is_none() { return Resp::err("deadline requires at-ms or every-ms"); }
        if repeat_count == 0 || repeat_count > crate::server::state::MAX_NOTIFICATION_REPEATS { return Resp::err("repeat_count must be between 1 and 100"); }
        if interval_ms.is_some_and(|ms| ms <= 0) { return Resp::err("every-ms must be positive"); }
        if interval_ms.is_some() && trigger_times_ms.is_empty() && trigger_ms.is_none() && repeat_count == 1 { }
        if !trigger_times_ms.is_empty() && (interval_ms.is_some() || repeat_count != 1) { return Resp::err("absolute schedule uses at-ms values and repeat_count is their length"); }
        let times = if trigger_times_ms.is_empty() { trigger_ms.into_iter().collect() } else { trigger_times_ms.clone() };
        if times.len() > crate::server::state::MAX_NOTIFICATION_REPEATS as usize { return Resp::err("absolute schedule supports at most 100 times"); }
        if times.iter().any(|trigger| *trigger <= now || *trigger >= expires_ms) { return Resp::err("absolute trigger times must be in the future and before expiry"); }
        if interval_ms.is_some_and(|ms| now.saturating_add(ms) >= expires_ms) { return Resp::err("every-ms must fire before subscription expiry"); }
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
        trigger_times_ms,
        interval_ms,
        repeat_count,
        fired_count: 0,
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
    Resp::data(json!({"subscription": subscription, "one_shot": false, "max_repeat_count": crate::server::state::MAX_NOTIFICATION_REPEATS}))
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
        if task.worktree_path.is_some()
            && matches!(task.status.as_str(), "merged" | "closed" | "cancelled")
            && !state.cleanup_receipts.contains_key(&task.id)
        {
            issues.push(format!(
                "TASK_CLEANUP_INCOMPLETE:{}:{}",
                task.id,
                task.worktree_path.as_deref().unwrap_or("unknown")
            ));
        }
        if let Some(receipt) = state.cleanup_receipts.get(&task.id) {
            if receipt.task_id != task.id
                || receipt.worktree_path != task.worktree_path
                || receipt.branch != task.branch
            {
                issues.push(format!("TASK_CLEANUP_RECEIPT_MISMATCH:{}", task.id));
            }
            if let Some(path) = task.worktree_path.as_deref() {
                let worktree = Path::new(path);
                let worktree = if worktree.is_absolute() {
                    worktree.to_path_buf()
                } else {
                    server
                        .root
                        .join(worktree.strip_prefix("./").unwrap_or(worktree))
                };
                if worktree.exists() {
                    issues.push(format!("TASK_CLEANUP_INCOMPLETE:{}:{}", task.id, path));
                }
            }
        }
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

pub(crate) fn handle_register(
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
    if let Some(pane) = pane.as_deref() {
        if let Some(session) = tmux_session_for_pane(pane) {
            if session != worker_id {
                return Resp::err(format!(
                    "pane {} belongs to tmux session {}; worker {} cannot bind it",
                    pane, session, worker_id
                ));
            }
        }
    }
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
                let refreshed_pane = pane.clone();
                let refreshed = WorkerRec {
                    id: worker_id.clone(),
                    token,
                    pane,
                    cwd,
                    registered_ms: existing.registered_ms,
                };
                let mut events = vec![Event::Registered { worker: refreshed }];
                if let Some(pane) = refreshed_pane.as_deref() {
                    events.extend(default_direct_message_events(
                        &st,
                        &worker_id,
                        pane,
                        now_ms(),
                    ));
                }
                server.commit_locked(&mut st, &events);
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
        let mut events = vec![Event::Registered {
            worker: refreshed.clone(),
        }];
        if let Some(pane) = refreshed.pane.as_deref() {
            events.extend(default_direct_message_events(
                &st,
                &worker_id,
                pane,
                now_ms(),
            ));
        }
        server.commit_locked(&mut st, &events);
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
    let mut events = vec![Event::Registered {
        worker: rec.clone(),
    }];
    if let Some(pane) = rec.pane.as_deref() {
        events.extend(default_direct_message_events(
            &st,
            &worker_id,
            pane,
            now_ms(),
        ));
    }
    server.commit_locked(&mut st, &events);
    Resp::data(json!({
        "worker_id": worker_id,
        "identity_kind": "peer",
        "runtime": runtime,
        "registered_at": iso(rec.registered_ms)
    }))
}

fn live_master_id(server: &Server, state: &State) -> Option<String> {
    let worker_id = state.master_worker_id.as_deref()?;
    let worker = state.workers.get(worker_id)?;
    let pane = worker.pane.as_deref()?;
    ((server.pane_alive_check)(pane) && (server.pane_owner_check)(worker_id, pane))
        .then(|| worker_id.to_string())
}

fn is_managed_subagent(state: &State, worker_id: &str) -> bool {
    state
        .subagents
        .values()
        .any(|record| record.peer == worker_id)
}

fn verify_master_actor(
    server: &Server,
    state: &State,
    worker_id: &str,
    token: &str,
) -> Result<(), Resp> {
    let Some(worker) = state.workers.get(worker_id) else {
        return Err(Resp::err(format!("worker {} not registered", worker_id)));
    };
    if worker.token != token {
        return Err(Resp::err(
            "token mismatch: identity does not own this worker_id",
        ));
    }
    match live_master_id(server, state) {
        Some(master) if master == worker_id => Ok(()),
        Some(_) => Err(Resp::err(
            "master authority required; ask the registered master to delegate",
        )),
        None => Err(Resp::err(
            "no live master; a peer may promote itself only with explicit user approval",
        )),
    }
}

fn handle_master_promote(
    server: &Server,
    worker_id: String,
    token: String,
    approval: String,
) -> Resp {
    let mut state = server.state.lock().unwrap();
    let Some(worker) = state.workers.get(&worker_id).cloned() else {
        return Resp::err(format!("worker {} not registered", worker_id));
    };
    if worker.token != token {
        return Resp::err("token mismatch: identity does not own this worker_id");
    }
    if approval.trim().is_empty() {
        return Resp::err("master promotion requires explicit user approval");
    }
    if live_master_id(server, &state).is_some() {
        return Resp::err("master already exists; only the registered master may delegate");
    }
    if worker
        .pane
        .as_deref()
        .is_none_or(|pane| !(server.pane_alive_check)(pane))
    {
        return Resp::err("master promotion requires a live tmux pane");
    }
    server.commit_locked(
        &mut state,
        &[Event::MasterAssigned {
            worker_id: worker_id.clone(),
            assigned_by: worker_id.clone(),
            approval: Some(approval),
            assigned_ms: now_ms(),
        }],
    );
    Resp::data(json!({"master": worker_id, "mode": "user_approved_self_promotion"}))
}

fn handle_master_delegate(
    server: &Server,
    worker_id: String,
    token: String,
    target_id: String,
) -> Resp {
    let mut state = server.state.lock().unwrap();
    if let Err(error) = verify_master_actor(server, &state, &worker_id, &token) {
        return error;
    }
    let Some(target) = state.workers.get(&target_id) else {
        return Resp::err(format!("target worker {} not registered", target_id));
    };
    if target
        .pane
        .as_deref()
        .is_none_or(|pane| !(server.pane_alive_check)(pane))
    {
        return Resp::err("master delegation requires a live target tmux pane");
    }
    server.commit_locked(
        &mut state,
        &[Event::MasterAssigned {
            worker_id: target_id.clone(),
            assigned_by: worker_id.clone(),
            approval: None,
            assigned_ms: now_ms(),
        }],
    );
    Resp::data(json!({"master": target_id, "delegated_by": worker_id}))
}

fn master_assignment_view(
    state: &State,
    worker_id: &str,
    endpoint_live: bool,
) -> serde_json::Value {
    let pane = state
        .workers
        .get(worker_id)
        .and_then(|worker| worker.pane.clone());
    json!({
        "worker_id": worker_id,
        "pane": pane,
        "endpoint_live": endpoint_live,
        "assigned_by": state.master_assigned_by,
        "approval": state.master_approval,
        "assigned_ms": state.master_assigned_ms,
    })
}

fn handle_master_status(server: &Server) -> Resp {
    let state = server.state.lock().unwrap();
    let live = live_master_id(server, &state);
    let master = live
        .as_ref()
        .map(|id| master_assignment_view(&state, id, true));
    let recorded = state
        .master_worker_id
        .as_ref()
        .filter(|id| live.as_deref() != Some(id.as_str()))
        .map(|id| master_assignment_view(&state, id, false));
    Resp::data(json!({"master": master, "recorded_unusable": recorded}))
}

pub(crate) fn handle_send(
    server: &Server,
    from: String,
    to: String,
    mtype: String,
    subject: Option<String>,
    body: String,
    in_reply_to: Option<String>,
    delivery_mode: String,
) -> Resp {
    handle_send_with_task(
        server,
        from,
        to,
        mtype,
        subject,
        body,
        in_reply_to,
        delivery_mode,
        false,
    )
}

pub(crate) fn handle_send_with_task(
    server: &Server,
    from: String,
    to: String,
    mtype: String,
    subject: Option<String>,
    body: String,
    in_reply_to: Option<String>,
    delivery_mode: String,
    assign_task: bool,
) -> Resp {
    if mtype != "notify" {
        return Resp::err("peer messaging requires type notify");
    }
    if delivery_mode != "immediate" {
        return Resp::err(
            "implicit idle delivery is removed; use an explicit notification subscription",
        );
    }
    let Some(subject) = subject.filter(|subject| !subject.trim().is_empty()) else {
        return Resp::err("MESSAGE_SUBJECT_REQUIRED: sendmessage requires --subject");
    };
    if !MSG_TYPES.contains(&mtype.as_str()) {
        return Resp::err(format!(
            "invalid type {}; must be one of {:?}",
            mtype, MSG_TYPES
        ));
    }
    let mut st = server.state.lock().unwrap();
    if assign_task
        && !st
            .subagents
            .values()
            .any(|s| s.parent == from && s.peer == to && s.status == "assigned")
    {
        return Resp::err("managed task requires an authorized assigned subagent");
    }
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
        subject: Some(subject),
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
            && m.subject == msg.subject
            && m.body == msg.body
            && m.state == "pending"
    }) {
        return Resp::data(json!({"msg_id": existing.id, "deduplicated": true}));
    }
    let mid = msg.id.clone();
    let task_id = assign_task.then(|| format!("task-{mid}"));
    let subscription = st
        .matching_subscription(&to, "direct-message", None, now_ms())
        .cloned();
    let mut events = vec![Event::Sent { msg }];
    if let Some(task_id) = &task_id {
        events.push(Event::TaskCreated { task: TaskRec {
            id:task_id.clone(),owner:to.clone(),created_by:from.clone(),feature_id:None,
            worktree_path:None,branch:None,base_commit:None,priority:"p2".into(),status:"assigned".into(),
            next_step:Some(format!("Read collab msg {mid}; accept via subagent working; bind a worktree with task relocate before code edits.")),
            wait:None,created_ms:now_ms(),updated_ms:now_ms(),
        }});
        let mut child = st
            .subagents
            .values()
            .find(|s| s.parent == from && s.peer == to && s.status == "assigned")
            .unwrap()
            .clone();
        child.last_message = Some(mid.clone());
        events.push(Event::SubagentUpdated { subagent: child });
    }
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
        "task_id": task_id,
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

fn handle_cross_project_send(
    server: &Server,
    from: String,
    from_project: String,
    source_master_assigned_by: String,
    source_master_approval: Option<String>,
    source_master_assigned_ms: i64,
    to: String,
    subject: String,
    body: String,
    in_reply_to: Option<String>,
) -> Resp {
    if from_project.trim().is_empty() || source_master_assigned_by.trim().is_empty() {
        return Resp::err("cross-project send requires source project and master assignment evidence");
    }
    if source_master_assigned_ms <= 0 {
        return Resp::err("cross-project send requires source master assignment timestamp");
    }
    if source_master_approval.as_deref().is_none_or(|v| v.trim().is_empty())
        && source_master_assigned_by == from
    {
        return Resp::err("cross-project send requires user approval evidence for self-promoted source master");
    }
    let mut st = server.state.lock().unwrap();
    if live_master_id(server, &st).as_deref() != Some(to.as_str()) {
        return Resp::err("cross-project communication requires the target to be a live master");
    }
    let Some(recipient) = st.workers.get(&to) else {
        return Resp::err(format!("recipient {} not registered", to));
    };
    if recipient.pane.as_deref().is_none_or(|pane| {
        !(server.pane_alive_check)(pane) || !(server.pane_owner_check)(&to, pane)
    }) {
        return Resp::err("cross-project communication requires a live target tmux identity");
    }
    if subject.trim().is_empty() {
        return Resp::err("MESSAGE_SUBJECT_REQUIRED: cross-project send requires --subject");
    }
    let msg = Message {
        id: gen_msg_id(),
        from: format!("{}@{}", from, from_project),
        to: to.clone(),
        mtype: "notify".into(),
        subject: Some(subject),
        body,
        in_reply_to,
        created_ms: now_ms(),
        state: "pending".into(),
        wake_attempt_count: 0,
        last_wake_attempt_ms: 0,
    };
    let mid = msg.id.clone();
    let subscription = st
        .matching_subscription(&to, "direct-message", None, now_ms())
        .cloned();
    let mut events = vec![
        Event::Sent { msg },
        Event::DeliveryMode {
            msg_id: mid.clone(),
            mode: "explicit-notification".into(),
        },
    ];
    if let Some(subscription) = &subscription {
        events.push(Event::WakeBound {
            message_id: mid.clone(),
            subscription_id: subscription.id.clone(),
        });
    }
    server.commit_locked(&mut st, &events);
    drop(st);
    let notified = subscription
        .as_ref()
        .is_some_and(|sub| attempt_notification(server, &mid, &sub.id));
    Resp::data(json!({
        "msg_id": mid,
        "durable": true,
        "cross_project": true,
        "source_master": from,
        "target_master": to,
        "notification": if notified { "sent" } else { "mailbox-only-no-subscription" }
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
    Resp::data(json!({
        "task": task.id,
        "owner": task.owner,
        "status": task.status,
        "cleanup": if task.worktree_path.is_some() { "pending" } else { "not_required" },
    }))
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
        if new_status == "cancelled" && task.worktree_path.is_some() {
            return Resp::err(
                "CLEANUP_REQUIRED_BEFORE_CANCEL: task owns a worktree; close only after merged cleanup",
            );
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

/// A peer owns its tmux pane only when the live session name matches the
/// worker id. Non-tmux operators always pass. Stale or split bindings wake
/// the wrong agent, so keepalive and notification paths consult this guard
/// before touching a pane.
pub(crate) fn pane_owner_authoritative(worker_id: &str, pane: &str) -> bool {
    if pane.starts_with('%') {
        tmux_session_for_pane(pane).as_deref() == Some(worker_id)
    } else {
        true
    }
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

fn cleanup_receipt_is_reusable(root: &Path, receipt: &CleanupReceipt) -> bool {
    if let Some(path) = receipt.worktree_path.as_deref() {
        let worktree = Path::new(path);
        let worktree = if worktree.is_absolute() {
            worktree.to_path_buf()
        } else {
            root.join(worktree.strip_prefix("./").unwrap_or(worktree))
        };
        if worktree.exists() {
            return false;
        }
    }
    if let Some(branch) = receipt.branch.as_deref() {
        let branch_exists = Command::new("git")
            .current_dir(root)
            .args(["rev-parse", "--verify", branch])
            .output()
            .is_ok_and(|output| output.status.success());
        if branch_exists {
            return false;
        }
    }
    true
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

fn handle_task_close(
    server: &Server,
    worker_id: String,
    token: String,
    task_id: String,
    force: bool,
    reason: Option<String>,
) -> Resp {
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
    if force {
        let reason = reason
            .as_deref()
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .map(str::to_owned);
        let Some(reason) = reason else {
            return Resp::err("force close requires a non-empty --reason");
        };
        let live_master = live_master_id(server, &st);
        let authorized = live_master.as_deref() == Some(worker_id.as_str())
            || (live_master.is_none() && task.owner == worker_id);
        if !authorized {
            return Resp::err_data(
                "manual force close is not authorized for this caller",
                json!({
                    "live_master": live_master,
                    "task_owner": task.owner,
                    "requester": worker_id,
                    "rule": "live master may close any task; the owner may close its own task when no live master exists",
                }),
            );
        }
        let mut closed = task;
        closed.status = "closed".into();
        closed.wait = None;
        closed.next_step = Some(format!("manual close: {reason}"));
        closed.updated_ms = now_ms();
        let receipt = CleanupReceipt {
            id: format!("cleanup-manual-{}-{}", closed.id, closed.updated_ms),
            task_id: closed.id.clone(),
            worktree_path: closed.worktree_path.clone(),
            branch: closed.branch.clone(),
            verified_ms: closed.updated_ms,
            manual_reason: Some(reason.clone()),
        };
        let superseded: Vec<String> = st
            .msgs
            .values()
            .filter(|m| m.to == closed.owner && m.state == "pending" && m.mtype == "keepalive")
            .map(|m| m.id.clone())
            .collect();
        let mut events: Vec<Event> = vec![
            Event::CleanupVerified {
                receipt: receipt.clone(),
            },
            Event::TaskUpdated {
                task: closed.clone(),
            },
        ];
        if !superseded.is_empty() {
            events.push(Event::Superseded {
                ids: superseded.clone(),
            });
        }
        server.commit_locked(&mut st, &events);
        let stale_workers = stale_worker_views(&st, &server.pane_alive_check);
        drop(st);
        return Resp::data(json!({
            "task": closed.id,
            "status": closed.status,
            "owner": closed.owner,
            "manual": true,
            "reason": reason,
            "receipt_id": receipt.id,
            "superseded_pending_keepalives": superseded,
            "stale_workers": stale_workers,
            "next_action": "lifecycle complete; keepalives for this task owner stopped",
        }));
    }
    if task.owner != worker_id {
        return Resp::err("only the task owner may close its lifecycle");
    }
    if task.status != "merged" {
        return Resp::err(format!(
            "task {} must be merged by its owner before close (current: {})",
            task_id, task.status
        ));
    }
    let receipt_reusable = st
        .cleanup_receipts
        .get(&task.id)
        .filter(|receipt| {
            receipt.task_id == task.id
                && receipt.worktree_path == task.worktree_path
                && receipt.branch == task.branch
        })
        .is_some_and(|receipt| cleanup_receipt_is_reusable(&server.root, receipt));
    if !receipt_reusable {
        if let Err(e) = close_task_resources(
            &server.root,
            task.worktree_path.as_deref(),
            task.branch.as_deref(),
        ) {
            return Resp::err(e);
        }
    }
    let mut closed = task;
    closed.status = "closed".to_string();
    closed.wait = None;
    closed.next_step = Some("closed after owner merge and cleanup".to_string());
    closed.updated_ms = now_ms();
    let receipt = CleanupReceipt {
        id: format!("cleanup-{}-{}", closed.id, closed.updated_ms),
        task_id: closed.id.clone(),
        worktree_path: closed.worktree_path.clone(),
        branch: closed.branch.clone(),
        verified_ms: closed.updated_ms,
        manual_reason: None,
    };
    server.commit_locked(
        &mut st,
        &[
            Event::CleanupVerified {
                receipt: receipt.clone(),
            },
            Event::TaskUpdated {
                task: closed.clone(),
            },
        ],
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
                        subject: Some(format!("released:{}", closed.id)),
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
            "result": "verified",
            "receipt_id": receipt.id,
        },
        "stale_workers": stale_workers,
        "notification": "subscribed resource waiters only",
        "next_action": "lifecycle complete",
    }))
}

fn task_view(state: &State, task: &TaskRec) -> serde_json::Value {
    let cleanup_required = task.worktree_path.is_some();
    let cleanup_receipt = state.cleanup_receipts.get(&task.id);
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
        "cleanup": {
            "required": cleanup_required,
            "status": if !cleanup_required {
                "not_required"
            } else if cleanup_receipt.is_some() {
                "verified"
            } else {
                "pending"
            },
            "receipt_id": cleanup_receipt.map(|receipt| receipt.id.clone()),
        },
        "updated_at": iso(task.updated_ms),
        "keepalive": keepalive::view(state, &task.owner),
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
        .map(|task| task_view(&st, task))
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
    let managed = is_managed_subagent(&st, &worker_id);
    Resp::data(json!({
        "identity": {"worker_id": worker.id, "kind": "peer", "pane": worker.pane},
        "liveness": {
            "pane_alive": worker.pane.as_deref().is_some_and(pane_alive),
            "pane_idle": worker.pane.as_deref().is_some_and(pane_idle),
        },
        "tasks": tasks,
        "inbox": {"unread": unread.len()},
        "next_actions": next_actions,
        "master": live_master_id(server, &st).map(|id| json!({"worker_id": id})),
        "authority": {
            "managed_subagent": managed,
            "must_obey_master": managed,
            "may_decline_master_invite": !managed,
        },
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
        .map(|task| task_view(&st, task))
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

fn worker_status_summary(server: &Server, st: &State, w: &WorkerRec) -> serde_json::Value {
    let active = st.tasks.values().find(|task| {
        task.owner == w.id
            && !matches!(task.status.as_str(), "closed" | "cancelled")
    });
    let pane = w.pane.as_deref();
    let endpoint_live = pane.is_some_and(server.pane_alive_check);
    let identity_valid = pane.is_some_and(|p| (server.pane_owner_check)(&w.id, p));
    let agent_state = pane
        .map(|p| match (server.pane_state_check)(p) {
            crate::server::knock::AgentState::Waiting => "waiting",
            crate::server::knock::AgentState::Working => "working",
            crate::server::knock::AgentState::Absent => "absent",
            crate::server::knock::AgentState::Unknown => "unknown",
        })
        .unwrap_or("absent");
    let unacked_notifications = st
        .msgs
        .values()
        .filter(|m| m.to == w.id && m.state == "delivered")
        .count();
    let pending_notifications = st
        .msgs
        .values()
        .filter(|m| m.to == w.id && m.state == "pending")
        .count();
    let notifications_paused = (unacked_notifications as u32) >= server.config.notifications.max_unacked;
    let keepalive = st.keepalives.get(&w.id);
    let suspected_offline = keepalive.map(|k| k.suspected_offline).unwrap_or(false);
    let unacked_keepalives = keepalive.map(|k| k.unacked).unwrap_or(0);
    let status = if !endpoint_live {
        "lost"
    } else if !identity_valid {
        "identity-mismatch"
    } else if suspected_offline {
        "offline"
    } else if notifications_paused {
        "ack-required"
    } else {
        agent_state
    };
    json!({
        "id": w.id,
        "pane": w.pane,
        "status": status,
        "endpoint_live": endpoint_live,
        "identity_valid": identity_valid,
        "agent_state": agent_state,
        "unacked_notifications": unacked_notifications,
        "pending_notifications": pending_notifications,
        "notifications_paused": notifications_paused,
        "unacked_keepalives": unacked_keepalives,
        "suspected_offline": suspected_offline,
        "active_task": active.map(|task| task.id.as_str()),
        "active_status": active.map(|task| task.status.as_str()),
    })
}

// ---------- dispatch ----------

fn mutation_blocked_during_migration(req: &Req) -> bool {
    match req {
        Req::SubagentObserve { .. } => false,
        Req::Subagent { command, .. } => !matches!(
            command,
            crate::subagent::Action::List | crate::subagent::Action::Status { .. }
        ),
        Req::Send { .. }
        | Req::CrossProjectSend { .. }
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
        | Req::MasterPromote { .. }
        | Req::MasterDelegate { .. }
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
        | Req::MasterStatus
        | Req::Role { .. }
        | Req::Workers
        | Req::WorkerStatus { .. }
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
        Req::SubagentObserve { id, snapshot_lines } => {
            match crate::subagent::observe(server, id.as_deref(), snapshot_lines) {
                Ok(mut value) => {
                    value["notification_channel"] = json!("none");
                    value["next_action"] = json!("No push channel for a non-tmux agent. Check subagent status/mailbox yourself; request snapshot explicitly when useful.");
                    Resp::data(value)
                }
                Err(error) => Resp::err(error.to_string()),
            }
        }
        Req::Subagent {
            worker_id,
            token,
            command,
            launch_env,
        } => crate::subagent::handle_with_env(server, &worker_id, &token, command, launch_env),
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
            subject,
            body,
            in_reply_to,
            delivery,
        } => handle_send(
            server,
            from,
            to,
            mtype,
            subject,
            body,
            in_reply_to,
            delivery,
        ),
        Req::CrossProjectSend {
            from,
            from_project,
            source_master_assigned_by,
            source_master_approval,
            source_master_assigned_ms,
            to,
            subject,
            body,
            in_reply_to,
        } => handle_cross_project_send(
            server,
            from,
            from_project,
            source_master_assigned_by,
            source_master_approval,
            source_master_assigned_ms,
            to,
            subject,
            body,
            in_reply_to,
        ),
        Req::NotificationMethods => Resp::data(json!({
            "methods": ["tmux"],
            "events": NOTIFICATION_EVENTS,
            "one_shot": false,
            "max_lifetime_attempts": MAX_WAKE_ATTEMPTS,
            "max_repeat_count": crate::server::state::MAX_NOTIFICATION_REPEATS,
            "max_active_subscriptions_per_agent": MAX_ACTIVE_SUBSCRIPTIONS_PER_WORKER,
            "max_ttl_seconds": MAX_NOTIFICATION_TTL_SECONDS,
        })),
        Req::NotificationSubscribe {
            worker_id,
            token,
            event,
            subject,
            trigger_ms,
            trigger_times_ms,
            interval_ms,
            repeat_count,
            ttl_seconds,
        } => handle_notification_subscribe(
            server,
            worker_id,
            token,
            event,
            subject,
            trigger_ms,
            trigger_times_ms,
            interval_ms,
            repeat_count,
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
            let owned: Vec<String> = if ids.is_empty() {
                st.msgs
                    .values()
                    .filter(|m| m.to == worker_id && (m.state == "delivered" || m.state == "pending"))
                    .map(|m| m.id.clone())
                    .collect()
            } else {
                ids.into_iter()
                    .filter(|id| st.msgs.get(id).map(|m| m.to == worker_id).unwrap_or(false))
                    .collect()
            };
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
                        "subject": m.subject,
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
                    "subject": m.subject, "body": m.body,
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
            force,
            reason,
        } => handle_task_close(server, worker_id, token, task_id, force, reason),
        Req::TaskDispatch { worker_id, token } => handle_task_dispatch(server, worker_id, token),
        Req::TaskStatus { task_id } => {
            let st = server.state.lock().unwrap();
            match task_id {
                Some(id) => st
                    .tasks
                    .get(&id)
                    .map(|task| task_view(&st, task))
                    .map(Resp::data)
                    .unwrap_or_else(|| Resp::err(format!("task {} not found", id))),
                None => Resp::data(
                    json!({"tasks": st.tasks.values().map(|task| task_view(&st, task)).collect::<Vec<_>>() }),
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
        Req::MasterPromote {
            worker_id,
            token,
            approval,
        } => handle_master_promote(server, worker_id, token, approval),
        Req::MasterDelegate {
            worker_id,
            token,
            target_id,
        } => handle_master_delegate(server, worker_id, token, target_id),
        Req::MasterStatus => handle_master_status(server),
        Req::Role { worker_id: _ } => {
            Resp::err("declared roles are removed; use collab who/context for peer identity")
        }
        Req::Workers => {
            let st = server.state.lock().unwrap();
            let mut workers: Vec<serde_json::Value> = st
                .workers
                .values()
                .map(|w| worker_status_summary(server, &st, w))
                .collect();
            workers.sort_by(|a, b| a["id"].as_str().cmp(&b["id"].as_str()));
            Resp::data(json!({
                "workers": workers,
                "count": workers.len()
            }))
        }
        Req::WorkerStatus { worker_id } => {
            let st = server.state.lock().unwrap();
            let mut workers: Vec<serde_json::Value> = st
                .workers
                .values()
                .filter(|w| worker_id.as_ref().is_none_or(|id| id == &w.id))
                .map(|w| worker_status_summary(server, &st, w))
                .collect();
            workers.sort_by(|a, b| a["id"].as_str().cmp(&b["id"].as_str()));
            Resp::data(json!({
                "workers": workers,
                "count": workers.len()
            }))
        }
        Req::MasterId => handle_master_status(server),
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
    let content = std::fs::read_to_string(&journal)?;
    let mut events = Vec::new();
    let mut convert_root = false;
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
        if matches!(event, Event::MasterAssigned { .. })
            && (line.contains("\"ev\":\"RootAssigned\"")
                || line.contains("\"ev\": \"RootAssigned\""))
        {
            convert_root = true;
        }
        st.apply(&event);
        events.push(event);
    }
    if convert_root {
        let mut body = String::new();
        for event in events {
            body.push_str(&serde_json::to_string(&event)?);
            body.push('\n');
        }
        let tmp = journal.with_file_name("journal.jsonl.tmp");
        std::fs::write(&tmp, body)?;
        std::fs::rename(&tmp, &journal)?;
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

    let state = replay(&scope.root)?;
    let journal_file = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(server_dir.join("journal.jsonl"))?;

    append_log(&server_dir.join("log.txt"), "server starting");

    let server = Arc::new(Server {
        config: crate::config::load(&scope.root)?,
        root: scope.root.clone(),
        state: Mutex::new(state),
        journal: Mutex::new(journal_file),
        pane_alive_check: pane_alive,
        pane_owner_check: pane_owner_authoritative,
        pane_state_check: knock::probe_agent_state,
    });
    restore_registered_peer_default_leases(&server);
    let listener = UnixListener::bind(&sock_path)?;
    use std::os::unix::fs::PermissionsExt;
    std::fs::set_permissions(&sock_path, std::fs::Permissions::from_mode(0o600))?;
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
        let mut interval =
            tokio::time::interval(Duration::from_millis(sched.config.timers.tick_interval_ms));
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
pub(crate) mod peer_tests;
