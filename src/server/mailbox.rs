//! Notification projection and batching helpers.
//!
//! This is the single physical owner for mailbox JSONL append/read/repair and
//! the batched preview selection.  It must not own a second journal writer;
//! mutations are still committed by `crate::server::Server`.

use super::state::{Message, State, MAX_WAKE_ATTEMPTS};
use serde_json::json;
use std::path::Path;

const MAX_NOTIFICATION_SUBJECT_CHARS: usize = 48;
const MAX_NOTIFICATION_CHARS: usize = 1024;

pub const MAX_NOTIFICATION_TTL_SECONDS: u64 = 7 * 24 * 60 * 60;
pub const MAX_ACTIVE_SUBSCRIPTIONS_PER_WORKER: usize = 3;
pub const NOTIFICATION_EVENTS: [&str; 5] = [
    "direct-message",
    "resource-released",
    "deadline",
    "async-result",
    "master-idle",
];
pub const DEFAULT_DIRECT_MESSAGE_TTL_SECONDS: u64 = MAX_NOTIFICATION_TTL_SECONDS;

pub fn default_direct_message_id(worker_id: &str) -> String {
    format!("sub-default-direct-message-{worker_id}")
}

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

pub fn visible_body(body: &str) -> String {
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

pub fn notification_class(subject: &str) -> (&'static str, &'static str) {
    if subject.starts_with("worker-unresponsive") {
        return ("P1", "snapshot the pane, then recover or close it");
    }
    if subject.starts_with("worker-idle") {
        return ("P1", "dispatch work to this idle capacity");
    }
    if subject.starts_with("master-idle") {
        return ("P1", "run the scheduling pass: inspect graph/load/liveness, dispatch authorized work, and resolve blockers");
    }
    if subject.starts_with("subagent-status") {
        return ("P1", "re-dispatch, close, or leave the child idle");
    }
    if subject.starts_with("task-keepalive") {
        return ("P1", "continue your own task or record a real blocker");
    }
    if subject.starts_with("goal") || subject.starts_with("deadline") {
        return ("P0", "run the long-horizon briefing and schedule");
    }
    if subject.contains("blocker") || subject.contains("unblock") {
        return ("P0", "resolve the blocker; you own it");
    }
    if subject.starts_with("release") || subject.contains("released") {
        return (
            "P1",
            "the resource is free; resume the task that waited on it",
        );
    }
    if subject.contains("recorded") || subject.contains("receipt") || subject.contains("delivered")
    {
        return ("P2", "note it and go straight back to your current task");
    }
    ("P1", "do the in-scope action the message asks for")
}

pub fn semantic_notification_category(subject: &str, mtype: &str) -> &'static str {
    if subject.starts_with("master-idle") || subject.starts_with("worker-idle") {
        return "idle";
    }
    if subject.starts_with("task-keepalive")
        || subject.starts_with("subagent-status")
        || subject.starts_with("progress")
        || subject.starts_with("delivery")
    {
        return "progress";
    }
    if subject.starts_with("resource")
        || subject.starts_with("deadline")
        || subject.starts_with("worker-unresponsive")
    {
        return "system";
    }
    if mtype == "notify" {
        return "direct";
    }
    "system"
}

const NOTIFY_PROTOCOL: &str =
    "READ IS NOT DONE: never end your turn on an ACK, a read, or a summary. \
     After handling, resume your current task; if you own none, run \
     `appsdk longhorizon show` and take work.";

pub fn notification_text(message: &Message) -> Option<String> {
    let subject_raw = message.subject.as_deref()?;
    Some(compose_notification(
        &message.id,
        subject_raw,
        &visible_body(&message.body),
    ))
}

pub fn is_explicit_notification(state: &State, message: &Message) -> bool {
    message.mtype == "notify"
        && state
            .delivery_modes
            .get(&message.id)
            .is_some_and(|mode| mode == "explicit-notification")
}

pub fn batch_notification_text(
    batch: &[(i64, String, String, String, String)],
    remaining: usize,
) -> String {
    let message_ids = batch
        .iter()
        .map(|(_, id, _, _, _)| id.as_str())
        .collect::<Vec<_>>()
        .join(",");
    let actions = batch
        .iter()
        .map(|(_, _, _, _, text)| {
            text.split_once('[')
                .and_then(|(_, rest)| rest.split_once(']'))
                .map(|(subject, _)| subject)
                .unwrap_or("notification")
        })
        .collect::<Vec<_>>()
        .join(",");
    // Message does not carry a typed task association. Do not infer one from
    // preview text; the durable inbox remains the source for task details.
    let task_ids = "none";
    let older = (remaining > 0)
        .then(|| format!(" older_messages={remaining}; run collab inbox"))
        .unwrap_or_default();
    format!("Batch wake: message_ids={message_ids} task_ids={task_ids} action_categories={actions}. Read full durable details from collab inbox; execute the actions, do not ACK-only.{older}")
}

pub struct RecipientMailboxRead {
    pub records: Vec<serde_json::Value>,
    pub partial_tail: bool,
    pub unterminated_tail: bool,
    pub recoverable_errors: Vec<String>,
}

pub fn read_recipient_mailbox(
    path: &Path,
    recipient: &str,
) -> Result<RecipientMailboxRead, String> {
    let content = std::fs::read_to_string(path).map_err(|error| format!("read: {error}"))?;
    let lines = content.lines().collect::<Vec<_>>();
    let has_unterminated_tail = !content.is_empty() && !content.ends_with('\n');
    let mut partial_tail = false;
    let mut records = Vec::new();
    let mut recoverable_errors = Vec::new();
    for (index, line) in lines.iter().enumerate() {
        match serde_json::from_str::<serde_json::Value>(line) {
            Ok(record) => match normalize_mailbox_record(record, recipient, index + 1) {
                Ok(record) => records.push(record),
                Err(error) => {
                    if has_unterminated_tail && index + 1 == lines.len() {
                        return Err(error);
                    }
                    if lines
                        .iter()
                        .skip(index + 1)
                        .any(|later| !later.trim().is_empty())
                    {
                        recoverable_errors.push(error);
                    } else {
                        return Err(error);
                    }
                }
            },
            Err(error) => {
                let error = format!("malformed JSONL record {}: {error}", index + 1);
                if has_unterminated_tail && index + 1 == lines.len() {
                    partial_tail = true;
                    break;
                }
                if lines
                    .iter()
                    .skip(index + 1)
                    .any(|later| !later.trim().is_empty())
                {
                    recoverable_errors.push(error);
                } else {
                    return Err(error);
                }
            }
        }
    }
    Ok(RecipientMailboxRead {
        records,
        partial_tail,
        unterminated_tail: has_unterminated_tail,
        recoverable_errors,
    })
}

pub fn recover_malformed_mailbox_tail(path: &Path, recipient: &str) -> Result<bool, String> {
    let content = std::fs::read_to_string(path)
        .map_err(|error| format!("read malformed mailbox: {error}"))?;
    // Only remove a malformed run at EOF. Interior malformed records remain
    // part of the append-only projection so later valid records are retained.
    let mut statuses = Vec::new();
    let mut offset = 0usize;
    for segment in content.split_inclusive('\n') {
        let line = segment.strip_suffix('\n').unwrap_or(segment);
        let line = line.strip_suffix('\r').unwrap_or(line);
        let record_result = serde_json::from_str::<serde_json::Value>(line)
            .map_err(|error| format!("malformed JSONL record: {error}"))
            .and_then(|record| normalize_mailbox_record(record, recipient, 0));
        statuses.push((offset, record_result.is_ok()));
        offset += segment.len();
    }

    let mut trailing_malformed_start = None;
    for (start, valid) in statuses.into_iter().rev() {
        if valid {
            break;
        }
        trailing_malformed_start = Some(start);
    }
    if let Some(valid_len) = trailing_malformed_start {
        let file = std::fs::OpenOptions::new()
            .write(true)
            .open(path)
            .map_err(|error| format!("open malformed mailbox for truncation: {error}"))?;
        file.set_len(valid_len as u64)
            .map_err(|error| format!("truncate malformed mailbox: {error}"))?;
        return Ok(true);
    }
    Ok(false)
}

pub fn normalize_mailbox_record(
    record: serde_json::Value,
    recipient: &str,
    index: usize,
) -> Result<serde_json::Value, String> {
    if record["schema_version"] == 1
        && record["record_type"] == "message"
        && record["recipient"] == recipient
    {
        return Ok(record);
    }

    // Before schema_version=1, recipient JSONL stored the bare Message. Keep
    // those durable records readable and project them into the current shape
    // in memory. New writes remain append-only v1 envelopes.
    let message: Message = serde_json::from_value(record)
        .map_err(|error| format!("invalid mailbox envelope at record {index}: {error}"))?;
    if message.to != recipient {
        return Err(format!("invalid mailbox envelope at record {index}"));
    }
    let subject = message.subject.as_deref().unwrap_or("notice");
    let (priority, action) = notification_class(subject);
    Ok(json!({
        "schema_version": 1,
        "record_type": "message",
        "recipient": recipient,
        "category": semantic_notification_category(subject, &message.mtype),
        "priority": priority,
        "action": action,
        "task_ids": [],
        "created_ms": message.created_ms,
        "window_start_ms": serde_json::Value::Null,
        "window_end_ms": serde_json::Value::Null,
        "window_source": "legacy-message",
        "state": message.state,
        "exact_error": serde_json::Value::Null,
        "message": message,
    }))
}

pub fn missing_recipient_projection_messages(
    state: &State,
    recipient: &str,
    projection: &RecipientMailboxRead,
) -> Vec<String> {
    let recorded = projection
        .records
        .iter()
        .filter_map(|record| record["message"]["id"].as_str())
        .collect::<std::collections::HashSet<_>>();
    let mut missing = state
        .msgs
        .values()
        .filter(|message| message.to == recipient && !recorded.contains(message.id.as_str()))
        .map(|message| message.id.clone())
        .collect::<Vec<_>>();
    missing.sort();
    missing
}

pub fn compose_notification(id: &str, subject_raw: &str, body: &str) -> String {
    let subject = abbreviated_subject(subject_raw).unwrap_or_else(|| "notice".to_string());
    let (priority, action) = notification_class(subject_raw);

    let head = format!("COLLAB_NOTIFY {} [{}] ", id, subject);
    let tail = format!(
        " | {} ACTION: {}. Details: collab msg {}. | {}",
        priority, action, id, NOTIFY_PROTOCOL
    );

    // The protocol and action must survive a long body, so the body absorbs
    // the truncation instead of the instructions being cut off the end.
    let fixed = head.chars().count() + tail.chars().count();
    let budget = MAX_NOTIFICATION_CHARS.saturating_sub(fixed);
    let body = if body.chars().count() > budget {
        const ELLIPSIS: &str = "… [collab inbox]";
        let keep = budget.saturating_sub(ELLIPSIS.chars().count());
        body.chars().take(keep).collect::<String>() + ELLIPSIS
    } else {
        body.to_string()
    };

    format!("{head}{body}{tail}")
}

pub fn truncate_notification(text: String) -> String {
    if text.chars().count() <= MAX_NOTIFICATION_CHARS {
        return text;
    }
    const SUFFIX: &str = "… [truncated; run collab inbox]";
    let keep = MAX_NOTIFICATION_CHARS.saturating_sub(SUFFIX.chars().count());
    let mut truncated = text.chars().take(keep).collect::<String>();
    truncated.push_str(SUFFIX);
    truncated
}

pub fn backup_message(root: &Path, msg: &Message) -> Result<(), String> {
    let dir = root.join(".agent-collab").join("mailbox");
    std::fs::create_dir_all(&dir).map_err(|error| format!("create directory: {error}"))?;
    let path = dir.join(format!("{}.json", msg.id));
    let data =
        serde_json::to_string_pretty(msg).map_err(|error| format!("message serialize: {error}"))?;
    std::fs::write(&path, data).map_err(|error| format!("message snapshot: {error}"))?;
    let jsonl_path = dir.join(format!("recipient-{}.jsonl", msg.to));
    if jsonl_path.exists() {
        match read_recipient_mailbox(&jsonl_path, &msg.to) {
            Ok(projection) => {
                if !projection.recoverable_errors.is_empty() {
                    super::knock::append_log(
                        &super::Server::log_path_for(root),
                        &format!(
                            "MAILBOX_JSONL_RECOVERABLE: {}",
                            projection.recoverable_errors.join(" | ")
                        ),
                    );
                }
                if projection.partial_tail {
                    let content = std::fs::read(&jsonl_path)
                        .map_err(|error| format!("read partial mailbox: {error}"))?;
                    let valid_len = content
                        .iter()
                        .rposition(|byte| *byte == b'\n')
                        .map(|index| index + 1)
                        .unwrap_or(0);
                    let file = std::fs::OpenOptions::new()
                        .write(true)
                        .open(&jsonl_path)
                        .map_err(|error| format!("open partial mailbox: {error}"))?;
                    file.set_len(valid_len as u64)
                        .map_err(|error| format!("truncate partial mailbox: {error}"))?;
                } else if projection.unterminated_tail {
                    let mut file = std::fs::OpenOptions::new()
                        .append(true)
                        .open(&jsonl_path)
                        .map_err(|error| format!("open unterminated mailbox: {error}"))?;
                    use std::io::Write;
                    file.write_all(b"\n")
                        .map_err(|error| format!("terminate mailbox record: {error}"))?;
                    file.sync_data()
                        .map_err(|error| format!("sync mailbox separator: {error}"))?;
                }
            }
            Err(error) => {
                // Journal state is authoritative. A projection error is
                // queryable and recoverable; it must not prevent a later
                // durable message from being appended to the mailbox.
                let truncated = recover_malformed_mailbox_tail(&jsonl_path, &msg.to)?;
                super::knock::append_log(
                    &super::Server::log_path_for(root),
                    &format!(
                        "MAILBOX_JSONL_RECOVERABLE: {error}; malformed_tail_truncated={truncated}"
                    ),
                );
            }
        }
    }
    let mut file = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(&jsonl_path)
        .map_err(|error| format!("open: {error}"))?;
    use std::io::Write;
    let subject = msg.subject.as_deref().unwrap_or("notice");
    let (priority, action) = notification_class(subject);
    let record = json!({
        "schema_version": 1,
        "record_type": "message",
        "recipient": msg.to,
        "category": semantic_notification_category(subject, &msg.mtype),
        "priority": priority,
        "action": action,
        "task_ids": [],
        "created_ms": msg.created_ms,
        "window_start_ms": serde_json::Value::Null,
        "window_end_ms": serde_json::Value::Null,
        "window_source": "not-attached-to-message-event",
        "state": msg.state,
        "exact_error": serde_json::Value::Null,
        "message": msg,
    });
    let data = serde_json::to_string(&record).map_err(|error| format!("serialize: {error}"))?;
    file.write_all(data.as_bytes())
        .map_err(|error| format!("append: {error}"))?;
    file.write_all(b"\n")
        .map_err(|error| format!("newline: {error}"))?;
    file.sync_data().map_err(|error| format!("sync: {error}"))?;
    Ok(())
}

pub fn purge_message_snapshot_files(root: &Path, expired_ids: &[String], live: &State) {
    let mailbox = root.join(".agent-collab").join("mailbox");
    if let Ok(entries) = std::fs::read_dir(&mailbox) {
        for entry in entries.flatten() {
            let name = entry.file_name();
            let Some(name) = name.to_str() else {
                continue;
            };
            let Some(id) = name.strip_suffix(".json") else {
                continue;
            };
            if expired_ids.iter().any(|expired_id| expired_id == id) || !live.msgs.contains_key(id)
            {
                let _ = std::fs::remove_file(entry.path());
            }
        }
    }
}

pub fn select_batch(
    candidates: Vec<(i64, String, String, String, String)>,
    delay: i64,
    window_start_ms: i64,
) -> (Vec<(i64, String, String, String, String)>, usize) {
    const MAX_BATCH_DELIVERY: usize = 3;
    let window_end = window_start_ms.saturating_add(delay);
    let mut batch: Vec<_> = candidates
        .into_iter()
        .filter(|candidate| candidate.0 <= window_end)
        .collect();
    let total_pending = batch.len();
    let remaining = total_pending.saturating_sub(MAX_BATCH_DELIVERY);
    if remaining > 0 {
        batch.drain(..remaining);
    }
    (batch, remaining)
}

pub fn max_wake_attempts() -> u32 {
    MAX_WAKE_ATTEMPTS
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::server::state::{now_ms, Message};

    fn msg(id: &str) -> Message {
        Message {
            id: id.into(),
            from: "server".into(),
            to: "master".into(),
            mtype: "notify".into(),
            subject: Some(format!("worker-idle:{id}")),
            body: format!("body {id}"),
            in_reply_to: None,
            created_ms: now_ms(),
            state: "pending".into(),
            wake_attempt_count: 0,
            last_wake_attempt_ms: 0,
        }
    }

    #[test]
    fn batch_selection_is_bounded_and_returns_overflow_count() {
        let candidates = (0..5)
            .map(|index| {
                let id = format!("m{index}");
                let text = notification_text(&msg(&id)).unwrap();
                (index, id, "sub".into(), "direct-message".into(), text)
            })
            .collect::<Vec<_>>();
        let (batch, remaining) = select_batch(candidates, 120_000, 0);
        assert_eq!(batch.len(), 3);
        assert_eq!(remaining, 2);
        assert!(batch.first().is_some_and(|item| item.1 == "m2"));
    }

    #[test]
    fn mailbox_repair_removes_only_trailing_malformed_records() {
        let dir = std::env::temp_dir().join(format!(
            "collab-mailbox-test-{}-{}",
            std::process::id(),
            now_ms()
        ));
        let mailbox = dir.join(".agent-collab/mailbox");
        std::fs::create_dir_all(&mailbox).unwrap();
        let path = mailbox.join("recipient-master.jsonl");
        let message = msg("m1");
        std::fs::write(&path, "not-json\n").unwrap();
        assert!(recover_malformed_mailbox_tail(&path, "master").is_ok());
        assert!(recover_malformed_mailbox_tail(&path, "master").is_ok());
        backup_message(&dir, &message).unwrap();
        let read = read_recipient_mailbox(&path, "master").unwrap();
        assert_eq!(read.records.len(), 1);
        std::fs::remove_dir_all(dir).unwrap();
    }
}
