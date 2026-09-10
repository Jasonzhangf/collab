//! Notification projection and batching helpers.
//!
//! This is the single physical owner for mailbox JSONL append/read/repair and
//! the batched preview selection.  It must not own a second journal writer;
//! mutations are still committed by `crate::server::Server`.

use super::state::{Message, State, MAX_WAKE_ATTEMPTS};
use serde_json::json;
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

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

#[derive(Debug)]
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
    validate_mailbox_recipient(recipient)?;
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
    validate_mailbox_recipient(recipient)?;
    let content = std::fs::read_to_string(path)
        .map_err(|error| format!("read malformed mailbox: {error}"))?;
    // Only remove a syntactically malformed run at EOF. Interior malformed
    // records remain part of the append-only projection so later valid
    // records are retained. Valid JSON with invalid fields is never truncated.
    let mut statuses = Vec::new();
    let mut offset = 0usize;
    for segment in content.split_inclusive('\n') {
        let line = segment.strip_suffix('\n').unwrap_or(segment);
        let line = line.strip_suffix('\r').unwrap_or(line);
        let json_result = serde_json::from_str::<serde_json::Value>(line);
        statuses.push((offset, json_result.is_ok()));
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
        validate_mailbox_record_fields(&record, index)?;
        return Ok(record);
    }

    if record["schema_version"] == 2
        && record["record_type"] == "message-event"
        && record["recipient"] == recipient
        && record["to_agent_id"] == recipient
    {
        validate_mailbox_record_fields(&record, index)?;
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
    if !mailbox_event_metadata_available(&message) {
        return Err(format!(
            "invalid mailbox envelope at record {index}: MAILBOX_RAW_RECORD_METADATA_MISSING"
        ));
    }
    validate_message_event_state(&message.state)
        .map_err(|error| format!("invalid mailbox envelope at record {index}: {error}"))?;
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
        "from_agent_id": message.from,
        "to_agent_id": message.to,
        "route_scope": serde_json::Value::Null,
        "binding_id": serde_json::Value::Null,
        "entity_key": mailbox_entity_key(&message),
        "projection_key": mailbox_projection_key(&message),
        "raw_body": message.body,
        "raw_reference": message.id,
        "exact_error": "MAILBOX_SCOPE_BINDING_UNAVAILABLE: legacy mailbox record has no authoritative route_scope or binding".to_string(),
        "message": message,
    }))
}

pub fn missing_recipient_projection_messages(
    messages: &[Message],
    recipient: &str,
    projection: &RecipientMailboxRead,
) -> Vec<String> {
    let recorded = projection
        .records
        .iter()
        .filter_map(|record| record["message"]["id"].as_str())
        .collect::<std::collections::HashSet<_>>();
    let mut missing = messages
        .iter()
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

fn validate_mailbox_recipient(recipient: &str) -> Result<(), String> {
    if recipient.is_empty() {
        return Err("mailbox recipient must not be empty".into());
    }
    if recipient == "." || recipient == ".." {
        return Err("mailbox recipient must be a basename".into());
    }
    if recipient.chars().any(char::is_control) {
        return Err("mailbox recipient must not contain control characters".into());
    }
    if recipient.contains(['/', '\\']) {
        return Err("mailbox recipient must be a basename".into());
    }
    Ok(())
}

fn validate_mailbox_string<'a>(
    value: &'a serde_json::Value,
    field: &str,
) -> Result<&'a str, String> {
    value
        .as_str()
        .ok_or_else(|| format!("mailbox record field {field} must be a string"))
}

fn validate_mailbox_i64(value: &serde_json::Value, field: &str) -> Result<i64, String> {
    value
        .as_i64()
        .ok_or_else(|| format!("mailbox record field {field} must be an integer"))
}

fn validate_message_event_state(state: &str) -> Result<(), String> {
    if matches!(state, "pending" | "delivered" | "read" | "superseded") {
        Ok(())
    } else {
        Err(format!(
            "mailbox record state {state} is not a known event state"
        ))
    }
}

fn validate_mailbox_record_metadata(
    record: &serde_json::Value,
    expected_recipient: &str,
    required_event_fields: &[&str],
) -> Result<(), String> {
    let schema_version = validate_mailbox_i64(&record["schema_version"], "schema_version")?;
    let record_type = validate_mailbox_string(&record["record_type"], "record_type")?;
    let valid_schema = matches!(
        (record_type, schema_version),
        ("message", 1) | ("message-event", 2) | ("mailbox-latest", 2)
    );
    if !valid_schema {
        return Err(format!(
            "mailbox record schema_version {schema_version} does not match record_type {record_type}"
        ));
    }
    let from = validate_mailbox_string(&record["from_agent_id"], "from_agent_id")?;
    let to = validate_mailbox_string(&record["to_agent_id"], "to_agent_id")?;
    if to != expected_recipient {
        return Err("mailbox record to_agent_id does not match recipient".into());
    }
    if from.is_empty() || to.is_empty() {
        return Err("mailbox record from_agent_id/to_agent_id must not be empty".into());
    }
    let recipient = validate_mailbox_string(&record["recipient"], "recipient")?;
    if recipient != expected_recipient {
        return Err("mailbox record recipient does not match recipient".into());
    }
    let event_kind = validate_mailbox_string(&record["event_kind"], "event_kind")?;
    let state = validate_mailbox_string(&record["state"], "state")?;
    validate_message_event_state(state)?;
    let expected_kind = match state {
        "pending" => "sent",
        "delivered" => "delivered",
        "read" => "acked",
        "superseded" => "superseded",
        _ => unreachable!(),
    };
    if event_kind != expected_kind {
        return Err(format!(
            "mailbox record state {state} must have event_kind {expected_kind}, got {event_kind}"
        ));
    }
    let entity_key = validate_mailbox_string(&record["entity_key"], "entity_key")?;
    if entity_key.is_empty() {
        return Err("mailbox record entity_key must not be empty".into());
    }
    let raw_reference = validate_mailbox_string(&record["raw_reference"], "raw_reference")?;
    let raw_body = validate_mailbox_string(&record["raw_body"], "raw_body")?;
    if raw_reference.is_empty() {
        return Err("mailbox record raw_reference must not be empty".into());
    }
    if raw_body.is_empty() {
        return Err("mailbox record raw_body must not be empty".into());
    }
    validate_mailbox_i64(&record["created_ms"], "created_ms")?;
    let message = &record["message"];
    let message_id = validate_mailbox_string(&message["id"], "message.id")?;
    let message_from = validate_mailbox_string(&message["from"], "message.from")?;
    let message_to = validate_mailbox_string(&message["to"], "message.to")?;
    let message_state = validate_mailbox_string(&message["state"], "message.state")?;
    validate_message_event_state(message_state)?;
    if message_id != raw_reference {
        return Err("mailbox record raw_reference must match message.id".into());
    }
    if message_from != from || message_to != to {
        return Err("mailbox record message.from/to must match from_agent_id/to_agent_id".into());
    }
    if message_state != state {
        return Err("mailbox record state must match message.state".into());
    }
    validate_mailbox_i64(&message["created_ms"], "message.created_ms")?;
    let message_body = validate_mailbox_string(&message["body"], "message.body")?;
    if message_body != raw_body {
        return Err("mailbox record raw_body must match message.body".into());
    }
    let projection_key = validate_mailbox_string(&record["projection_key"], "projection_key")?;
    let expected_projection_key = format!("{from}|{to}|{entity_key}");
    if projection_key != expected_projection_key {
        return Err("mailbox record projection_key does not match identity/entity".into());
    }
    let exact_error = validate_mailbox_string(&record["exact_error"], "exact_error")?;
    if exact_error.is_empty() {
        return Err("mailbox record exact_error must not be empty".into());
    }
    if !exact_error.starts_with("MAILBOX_SCOPE_BINDING_UNAVAILABLE:") {
        return Err("mailbox record exact_error must identify unavailable scope binding".into());
    }
    if !record
        .get("route_scope")
        .is_some_and(serde_json::Value::is_null)
    {
        return Err(
            "mailbox record route_scope must be null when scope binding is unavailable".into(),
        );
    }
    if !record
        .get("binding_id")
        .is_some_and(serde_json::Value::is_null)
    {
        return Err(
            "mailbox record binding_id must be null when scope binding is unavailable".into(),
        );
    }
    for field in required_event_fields {
        validate_mailbox_string(record.get(field).unwrap_or(&serde_json::Value::Null), field)?;
    }
    Ok(())
}

fn validate_mailbox_record_fields(record: &serde_json::Value, index: usize) -> Result<(), String> {
    let record_type = validate_mailbox_string(&record["record_type"], "record_type")?;
    match record_type {
        "message" | "message-event" => validate_mailbox_record_metadata(
            record,
            validate_mailbox_string(&record["recipient"], "recipient")?,
            &["projection_key"],
        )
        .map_err(|error| format!("invalid mailbox record at record {index}: {error}")),
        "mailbox-latest" => {
            let recipient = validate_mailbox_string(&record["recipient"], "recipient")?;
            validate_mailbox_record_metadata(record, recipient, &["projection_key"]).map_err(
                |error| format!("invalid latest mailbox record at record {index}: {error}"),
            )?;
            let message_id = validate_mailbox_string(&record["message_id"], "message_id")?;
            if message_id != validate_mailbox_string(&record["raw_reference"], "raw_reference")? {
                return Err("mailbox-latest message_id must match raw_reference".into());
            }
            let message = &record["message"];
            if message_id != validate_mailbox_string(&message["id"], "message.id")? {
                return Err("mailbox-latest message_id must match message.id".into());
            }
            validate_mailbox_i64(&record["occurred_ms"], "occurred_ms")?;
            Ok(())
        }
        other => Err(format!("mailbox record_type {other} is not supported")),
    }
}

fn mailbox_event_kind(msg: &Message) -> &'static str {
    match msg.state.as_str() {
        "pending" => "sent",
        "delivered" => "delivered",
        "read" => "acked",
        "superseded" => "superseded",
        _ => "state-updated",
    }
}

fn mailbox_event_metadata_available(msg: &Message) -> bool {
    !msg.from.is_empty() && !msg.to.is_empty() && !msg.body.is_empty() && !msg.id.is_empty()
}

pub fn mailbox_entity_key(msg: &Message) -> String {
    if let Some(reply_to) = msg.in_reply_to.as_deref().filter(|value| !value.is_empty()) {
        return format!("thread:{reply_to}");
    }
    if let Some(subject) = msg.subject.as_deref().filter(|value| !value.is_empty()) {
        return format!("subject:{subject}");
    }
    format!("message:{}", msg.id)
}

fn mailbox_projection_key(msg: &Message) -> String {
    format!("{}|{}|{}", msg.from, msg.to, mailbox_entity_key(msg))
}

fn mailbox_event_envelope(msg: &Message) -> serde_json::Value {
    let metadata_available = mailbox_event_metadata_available(msg);
    let exact_error = if metadata_available {
        "MAILBOX_SCOPE_BINDING_UNAVAILABLE: message carries no authoritative route_scope or binding in owned mailbox scope; projection is raw-only"
            .to_string()
    } else {
        "MAILBOX_RAW_RECORD_METADATA_MISSING".to_string()
    };
    let subject = msg.subject.as_deref().unwrap_or("notice");
    let (priority, action) = notification_class(subject);
    let entity_key = mailbox_entity_key(msg);
    let projection_key = mailbox_projection_key(msg);
    json!({
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
        "event_kind": mailbox_event_kind(msg),
        "from_agent_id": msg.from,
        "to_agent_id": msg.to,
        "route_scope": serde_json::Value::Null,
        "binding_id": serde_json::Value::Null,
        "entity_key": entity_key,
        "projection_key": projection_key,
        "raw_body": msg.body,
        "raw_reference": msg.id,
        "exact_error": exact_error,
        "message": msg,
    })
}

fn mailbox_latest_envelope(msg: &Message) -> serde_json::Value {
    let entity_key = mailbox_entity_key(msg);
    let projection_key = mailbox_projection_key(msg);
    json!({
        "schema_version": 2,
        "record_type": "mailbox-latest",
        "projection_key": projection_key,
        "from_agent_id": msg.from,
        "to_agent_id": msg.to,
        "recipient": msg.to,
        "entity_key": entity_key,
        "event_kind": mailbox_event_kind(msg),
        "message_id": msg.id,
        "route_scope": serde_json::Value::Null,
        "binding_id": serde_json::Value::Null,
        "state": msg.state,
        "created_ms": msg.created_ms,
        "occurred_ms": msg.created_ms,
        "raw_body": msg.body,
        "raw_reference": msg.id,
        "exact_error": "MAILBOX_SCOPE_BINDING_UNAVAILABLE: raw event has no authoritative route_scope or binding"
            .to_string(),
        "message": msg,
    })
}

fn latest_mailbox_path(root: &Path, recipient: &str) -> PathBuf {
    root.join(".agent-collab")
        .join("mailbox")
        .join(format!("recipient-{recipient}.latest.jsonl"))
}

#[derive(Debug)]
pub struct LatestMailboxProjection {
    pub records: Vec<serde_json::Value>,
    pub partial_tail: bool,
    pub unterminated_tail: bool,
    pub recoverable_errors: Vec<String>,
}

fn normalize_latest_mailbox_record(
    record: serde_json::Value,
    recipient: &str,
    index: usize,
) -> Result<serde_json::Value, String> {
    if record["schema_version"] == 2
        && record["record_type"] == "mailbox-latest"
        && record["recipient"] == recipient
        && record["to_agent_id"] == recipient
    {
        validate_mailbox_record_fields(&record, index)?;
        return Ok(record);
    }
    Err(format!(
        "invalid latest mailbox projection at record {index}"
    ))
}

pub fn read_latest_mailbox_projection(
    path: &Path,
    recipient: &str,
) -> Result<LatestMailboxProjection, String> {
    validate_mailbox_recipient(recipient)?;
    let content = std::fs::read_to_string(path).map_err(|error| format!("read latest: {error}"))?;
    let lines = content.lines().collect::<Vec<_>>();
    let has_unterminated_tail = !content.is_empty() && !content.ends_with('\n');
    let mut partial_tail = false;
    let mut records = Vec::new();
    let mut recoverable_errors = Vec::new();
    for (index, line) in lines.iter().enumerate() {
        match serde_json::from_str::<serde_json::Value>(line) {
            Ok(record) => match normalize_latest_mailbox_record(record, recipient, index + 1) {
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
                let error = format!("malformed latest JSONL record {}: {error}", index + 1);
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
    Ok(LatestMailboxProjection {
        records,
        partial_tail,
        unterminated_tail: has_unterminated_tail,
        recoverable_errors,
    })
}

fn write_latest_projection(path: &Path, records: &[serde_json::Value]) -> Result<(), String> {
    let tmp = path.with_file_name(format!(
        "{}.tmp",
        path.file_name()
            .and_then(|name| name.to_str())
            .unwrap_or("mailbox-latest")
    ));
    let mut body = String::new();
    for record in records {
        let line =
            serde_json::to_string(record).map_err(|error| format!("latest serialize: {error}"))?;
        body.push_str(&line);
        body.push('\n');
    }
    std::fs::write(&tmp, body).map_err(|error| format!("latest write: {error}"))?;
    std::fs::rename(&tmp, path).map_err(|error| format!("latest rename: {error}"))?;
    Ok(())
}

pub fn rebuild_latest_projection(
    root: &Path,
    recipient: &str,
) -> Result<LatestMailboxProjection, String> {
    validate_mailbox_recipient(recipient)?;
    let dir = root.join(".agent-collab").join("mailbox");
    let raw_path = dir.join(format!("recipient-{recipient}.jsonl"));
    let raw = read_recipient_mailbox(&raw_path, recipient)?;
    if raw.partial_tail || raw.unterminated_tail || !raw.recoverable_errors.is_empty() {
        return Err(format!(
            "MAILBOX_RAW_RECORDS_NOT_FULLY_VALIDATED: partial_tail={} unterminated_tail={} errors={}",
            raw.partial_tail,
            raw.unterminated_tail,
            raw.recoverable_errors.join(" | ")
        ));
    }
    let mut latest = BTreeMap::<(String, String, String), serde_json::Value>::new();
    for record in raw.records {
        let message = serde_json::from_value::<Message>(record["message"].clone())
            .map_err(|error| format!("latest rebuild message: {error}"))?;
        latest.insert(
            (
                message.from.clone(),
                message.to.clone(),
                mailbox_entity_key(&message),
            ),
            mailbox_latest_envelope(&message),
        );
    }
    let mut records = latest.values().cloned().collect::<Vec<_>>();
    records.sort_by(|a, b| {
        (
            a["from_agent_id"].as_str().unwrap_or_default(),
            a["to_agent_id"].as_str().unwrap_or_default(),
            a["entity_key"].as_str().unwrap_or_default(),
        )
            .cmp(&(
                b["from_agent_id"].as_str().unwrap_or_default(),
                b["to_agent_id"].as_str().unwrap_or_default(),
                b["entity_key"].as_str().unwrap_or_default(),
            ))
    });
    let latest_path = latest_mailbox_path(root, recipient);
    std::fs::create_dir_all(latest_path.parent().unwrap_or(root))
        .map_err(|error| format!("latest directory: {error}"))?;
    write_latest_projection(&latest_path, &records)?;
    Ok(LatestMailboxProjection {
        records,
        partial_tail: false,
        unterminated_tail: false,
        recoverable_errors: raw.recoverable_errors,
    })
}

fn update_latest_mailbox_projection(root: &Path, msg: &Message) -> Result<(), String> {
    let recipient = &msg.to;
    validate_mailbox_recipient(recipient)?;
    let dir = root.join(".agent-collab").join("mailbox");
    std::fs::create_dir_all(&dir).map_err(|error| format!("latest directory: {error}"))?;
    let path = latest_mailbox_path(root, recipient);
    let latest = mailbox_latest_envelope(msg);
    if !path.exists() {
        return rebuild_latest_projection(root, recipient).map(|_| ());
    }
    match read_latest_mailbox_projection(&path, recipient) {
        Ok(read) => {
            let mut errors = read.recoverable_errors.clone();
            if read.partial_tail {
                errors.push("partial latest projection tail".into());
            }
            if read.unterminated_tail {
                errors.push("unterminated latest projection tail".into());
            }
            if !errors.is_empty() {
                super::knock::append_log(
                    &super::Server::log_path_for(root),
                    &format!("MAILBOX_LATEST_RECOVERABLE: {}", errors.join(" | ")),
                );
                let rebuilt = rebuild_latest_projection(root, recipient)?;
                if !rebuilt.recoverable_errors.is_empty() {
                    super::knock::append_log(
                        &super::Server::log_path_for(root),
                        &format!(
                            "MAILBOX_LATEST_RAW_RECOVERABLE: {}",
                            rebuilt.recoverable_errors.join(" | ")
                        ),
                    );
                }
                return Ok(());
            }
            let mut recoverable_errors = Vec::<String>::new();
            let mut projection = BTreeMap::<(String, String, String), serde_json::Value>::new();
            for record in &read.records {
                let Some(from) = record["from_agent_id"].as_str() else {
                    recoverable_errors
                        .push("latest projection record missing from_agent_id".into());
                    continue;
                };
                let Some(to) = record["to_agent_id"].as_str() else {
                    recoverable_errors.push("latest projection record missing to_agent_id".into());
                    continue;
                };
                let Some(entity_key) = record["entity_key"].as_str() else {
                    recoverable_errors.push("latest projection record missing entity_key".into());
                    continue;
                };
                projection.insert(
                    (from.to_string(), to.to_string(), entity_key.to_string()),
                    record.clone(),
                );
            }
            if !recoverable_errors.is_empty() {
                super::knock::append_log(
                    &super::Server::log_path_for(root),
                    &format!(
                        "MAILBOX_LATEST_RECOVERABLE: {}",
                        recoverable_errors.join(" | ")
                    ),
                );
                let rebuilt = rebuild_latest_projection(root, recipient)?;
                if !rebuilt.recoverable_errors.is_empty() {
                    super::knock::append_log(
                        &super::Server::log_path_for(root),
                        &format!(
                            "MAILBOX_LATEST_RAW_RECOVERABLE: {}",
                            rebuilt.recoverable_errors.join(" | ")
                        ),
                    );
                }
                return Ok(());
            }
            projection.insert(
                (msg.from.clone(), msg.to.clone(), mailbox_entity_key(msg)),
                latest,
            );
            let mut records = projection.into_values().collect::<Vec<_>>();
            records.sort_by(|a, b| {
                (
                    a["from_agent_id"].as_str().unwrap_or_default(),
                    a["to_agent_id"].as_str().unwrap_or_default(),
                    a["entity_key"].as_str().unwrap_or_default(),
                )
                    .cmp(&(
                        b["from_agent_id"].as_str().unwrap_or_default(),
                        b["to_agent_id"].as_str().unwrap_or_default(),
                        b["entity_key"].as_str().unwrap_or_default(),
                    ))
            });
            write_latest_projection(&path, &records)
        }
        Err(error) => {
            super::knock::append_log(
                &super::Server::log_path_for(root),
                &format!("MAILBOX_LATEST_RECOVERABLE: {error}"),
            );
            let rebuilt = rebuild_latest_projection(root, recipient)?;
            if !rebuilt.recoverable_errors.is_empty() {
                super::knock::append_log(
                    &super::Server::log_path_for(root),
                    &format!(
                        "MAILBOX_LATEST_RAW_RECOVERABLE: {}",
                        rebuilt.recoverable_errors.join(" | ")
                    ),
                );
            }
            Ok(())
        }
    }
}

pub fn backup_message(root: &Path, msg: &Message) -> Result<(), String> {
    validate_mailbox_recipient(&msg.to)?;
    if !mailbox_event_metadata_available(msg) {
        return Err(
            "MAILBOX_RAW_RECORD_METADATA_MISSING: id/from/to/body are required for mailbox retention"
                .into(),
        );
    }
    validate_message_event_state(&msg.state)?;
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
    let record = mailbox_event_envelope(msg);
    let data = serde_json::to_string(&record).map_err(|error| format!("serialize: {error}"))?;
    file.write_all(data.as_bytes())
        .map_err(|error| format!("append: {error}"))?;
    file.write_all(b"\n")
        .map_err(|error| format!("newline: {error}"))?;
    file.sync_data().map_err(|error| format!("sync: {error}"))?;
    update_latest_mailbox_projection(root, msg)?;
    Ok(())
}

pub fn purge_message_snapshot_files(root: &Path, expired_ids: &[String], live: &State) {
    let mailbox = root.join(".agent-collab").join("mailbox");
    match std::fs::read_dir(&mailbox) {
        Ok(entries) => {
            for entry in entries {
                let entry = match entry {
                    Ok(entry) => entry,
                    Err(error) => {
                        super::knock::append_log(
                            &super::Server::log_path_for(root),
                            &format!("MAILBOX_SNAPSHOT_READ_FAILED: {error}"),
                        );
                        continue;
                    }
                };
                let name = entry.file_name();
                let Some(name) = name.to_str() else {
                    continue;
                };
                let Some(id) = name.strip_suffix(".json") else {
                    continue;
                };
                if expired_ids.iter().any(|expired_id| expired_id == id)
                    || !live.msgs.contains_key(id)
                {
                    if let Err(error) = std::fs::remove_file(entry.path()) {
                        super::knock::append_log(
                            &super::Server::log_path_for(root),
                            &format!("MAILBOX_SNAPSHOT_REMOVE_FAILED: {error}"),
                        );
                    }
                }
            }
        }
        Err(error) if error.kind() != std::io::ErrorKind::NotFound => {
            super::knock::append_log(
                &super::Server::log_path_for(root),
                &format!("MAILBOX_SNAPSHOT_READ_FAILED: {error}"),
            );
        }
        Err(_) => {}
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
    use crate::server::state::{now_ms, Event, Message};
    use std::sync::Mutex;

    fn test_server() -> (crate::server::Server, PathBuf) {
        static SEQUENCE: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
        let sequence = SEQUENCE.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        let root = std::env::temp_dir().join(format!(
            "collab-mailbox-projection-{}-{sequence}",
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
            crate::server::Server {
                config: crate::config::Config::default(),
                root: root.clone(),
                state: Mutex::new(crate::server::state::State::default()),
                journal: Mutex::new(journal),
                pane_alive_check: |_| crate::server::knock::PanePresence::Present,
                pane_owner_check: |_, _| Ok(true),
                pane_state_check: |_| crate::server::knock::AgentState::Waiting,
                mailbox_notify: tokio::sync::Notify::new(),
            },
            root,
        )
    }

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

    #[test]
    fn mailbox_repair_preserves_syntactically_valid_invalid_schema_tail() {
        let dir = std::env::temp_dir().join(format!(
            "collab-mailbox-valid-json-invalid-schema-{}-{}",
            std::process::id(),
            now_ms()
        ));
        let mailbox = dir.join(".agent-collab/mailbox");
        std::fs::create_dir_all(&mailbox).unwrap();
        let path = mailbox.join("recipient-master.jsonl");
        let raw = b"{\"schema_version\":999,\"record_type\":\"unknown\"}\n";
        std::fs::write(&path, raw).unwrap();

        assert!(!recover_malformed_mailbox_tail(&path, "master").unwrap());
        assert_eq!(std::fs::read(&path).unwrap(), raw);
        assert!(read_recipient_mailbox(&path, "master").is_err());

        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn mailbox_repair_truncates_only_syntactically_malformed_tail() {
        let dir = std::env::temp_dir().join(format!(
            "collab-mailbox-malformed-tail-{}-{}",
            std::process::id(),
            now_ms()
        ));
        let mailbox = dir.join(".agent-collab/mailbox");
        std::fs::create_dir_all(&mailbox).unwrap();
        let path = mailbox.join("recipient-master.jsonl");
        let prefix = b"{\"schema_version\":999,\"record_type\":\"unknown\"}\n";
        let mut raw = prefix.to_vec();
        raw.extend_from_slice(b"{\"unterminated\":");
        std::fs::write(&path, &raw).unwrap();

        assert!(recover_malformed_mailbox_tail(&path, "master").unwrap());
        assert_eq!(std::fs::read(&path).unwrap(), prefix);

        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn mailbox_rejects_non_basename_recipients_before_writing() {
        for recipient in ["", ".", "..", "/", "../", "worker\n"] {
            let dir = std::env::temp_dir().join(format!(
                "collab-mailbox-invalid-recipient-{}-{}",
                std::process::id(),
                now_ms()
            ));
            let mut message = msg("invalid-recipient");
            message.to = recipient.into();

            let error = backup_message(&dir, &message).unwrap_err();
            assert!(error.contains("mailbox recipient"), "{error}");
            assert!(!dir.exists(), "invalid recipient created {dir:?}");
        }
    }

    #[test]
    fn mailbox_keeps_valid_recipient_projection_path() {
        let dir = std::env::temp_dir().join(format!(
            "collab-mailbox-valid-recipient-{}-{}",
            std::process::id(),
            now_ms()
        ));
        let message = msg("valid-recipient");
        backup_message(&dir, &message).unwrap();

        assert!(dir
            .join(".agent-collab/mailbox/recipient-master.jsonl")
            .is_file());
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn raw_mailbox_retains_event_kind_identity_raw_entity_and_unavailable_scope_error() {
        let dir = std::env::temp_dir().join(format!(
            "collab-mailbox-raw-retention-{}-{}",
            std::process::id(),
            now_ms()
        ));
        let mut message = msg("raw-retention");
        message.in_reply_to = Some("task-raw".into());
        backup_message(&dir, &message).unwrap();

        message.state = "delivered".into();
        backup_message(&dir, &message).unwrap();

        message.state = "read".into();
        backup_message(&dir, &message).unwrap();

        let raw_path = dir.join(".agent-collab/mailbox/recipient-master.jsonl");
        let read = read_recipient_mailbox(&raw_path, "master").unwrap();
        assert_eq!(read.records.len(), 3);
        let event_kinds = read
            .records
            .iter()
            .filter_map(|record| record["event_kind"].as_str())
            .collect::<Vec<_>>();
        assert_eq!(event_kinds, vec!["sent", "delivered", "acked"]);
        for record in &read.records {
            assert_eq!(record["from_agent_id"], "server");
            assert_eq!(record["to_agent_id"], "master");
            assert!(record["route_scope"].is_null());
            assert!(record["binding_id"].is_null());
            assert_eq!(record["entity_key"], "thread:task-raw");
            assert!(record["exact_error"]
                .as_str()
                .is_some_and(|error| error.starts_with("MAILBOX_SCOPE_BINDING_UNAVAILABLE:")));
            assert_eq!(record["raw_body"], "body raw-retention");
            assert_eq!(record["raw_reference"], "raw-retention");
            assert_eq!(record["message"]["id"], "raw-retention");
        }
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn raw_mailbox_cross_field_inconsistency_fails_closed_and_blocks_rebuild() {
        let dir = std::env::temp_dir().join(format!(
            "collab-mailbox-raw-reject-inconsistent-{}-{}",
            std::process::id(),
            now_ms()
        ));
        let message = msg("raw-cross-field");
        backup_message(&dir, &message).unwrap();

        let raw_path = dir.join(".agent-collab/mailbox/recipient-master.jsonl");
        let mut records = std::fs::read_to_string(&raw_path)
            .unwrap()
            .lines()
            .map(|line| serde_json::from_str::<serde_json::Value>(line).unwrap())
            .collect::<Vec<_>>();
        records[0]["raw_reference"] = serde_json::json!("different-id");
        let body = records
            .iter()
            .map(|record| serde_json::to_string(record).unwrap())
            .collect::<Vec<_>>()
            .join("\n");
        std::fs::write(&raw_path, format!("{body}\n")).unwrap();

        let read_error = read_recipient_mailbox(&raw_path, "master").unwrap_err();
        assert!(read_error.contains("raw_reference"), "{read_error}");
        let rebuild_error = rebuild_latest_projection(&dir, "master").unwrap_err();
        assert!(rebuild_error.contains("raw_reference"), "{rebuild_error}");
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn latest_projection_is_idempotent_per_projection_key() {
        let dir = std::env::temp_dir().join(format!(
            "collab-mailbox-latest-idempotent-{}-{}",
            std::process::id(),
            now_ms()
        ));
        let mut message = msg("latest-idempotent");
        backup_message(&dir, &message).unwrap();
        message.state = "delivered".into();
        backup_message(&dir, &message).unwrap();
        message.state = "read".into();
        backup_message(&dir, &message).unwrap();
        backup_message(&dir, &message).unwrap();

        let latest_path = latest_mailbox_path(&dir, "master");
        let latest = read_latest_mailbox_projection(&latest_path, "master").unwrap();
        assert_eq!(latest.records.len(), 1);
        assert_eq!(latest.records[0]["state"], "read");
        assert_eq!(latest.records[0]["event_kind"], "acked");
        assert_eq!(
            std::fs::read_to_string(&latest_path)
                .unwrap()
                .lines()
                .count(),
            1
        );
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn malformed_latest_projection_tail_is_rebuilt_from_raw() {
        let dir = std::env::temp_dir().join(format!(
            "collab-mailbox-latest-repair-{}-{}",
            std::process::id(),
            now_ms()
        ));
        let mut message = msg("latest-repair");
        backup_message(&dir, &message).unwrap();

        let latest_path = latest_mailbox_path(&dir, "master");
        let mut content = std::fs::read_to_string(&latest_path).unwrap();
        content.push_str("{\"partial\":");
        std::fs::write(&latest_path, content).unwrap();

        message.state = "delivered".into();
        backup_message(&dir, &message).unwrap();

        let latest = read_latest_mailbox_projection(&latest_path, "master").unwrap();
        assert_eq!(latest.records.len(), 1);
        assert_eq!(latest.records[0]["state"], "delivered");
        assert_eq!(latest.records[0]["event_kind"], "delivered");
        let raw = read_recipient_mailbox(
            &dir.join(".agent-collab/mailbox/recipient-master.jsonl"),
            "master",
        )
        .unwrap();
        assert_eq!(raw.records.len(), 2);
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn mailbox_events_survive_journal_replay_with_latest_state() {
        let (server, root) = test_server();
        let id = "replay-latest";
        let message = Message {
            id: id.into(),
            from: "sender".into(),
            to: "master".into(),
            mtype: "notify".into(),
            subject: Some("progress".into()),
            body: "replay raw body".into(),
            in_reply_to: None,
            created_ms: now_ms(),
            state: "pending".into(),
            wake_attempt_count: 0,
            last_wake_attempt_ms: 0,
        };
        server.commit(&[
            Event::Sent {
                msg: message.clone(),
            },
            Event::Delivered {
                ids: vec![id.into()],
            },
            Event::Acked {
                ids: vec![id.into()],
            },
        ]);

        let latest_path = latest_mailbox_path(&root, "master");
        let latest = read_latest_mailbox_projection(&latest_path, "master").unwrap();
        assert_eq!(latest.records.len(), 1);
        assert_eq!(latest.records[0]["state"], "read");
        drop(server);

        let replayed = crate::server::replay(&root).unwrap();
        assert_eq!(replayed.msgs[id].state, "read");
        assert_eq!(replayed.msgs[id].body, "replay raw body");
        std::fs::remove_dir_all(root).unwrap();
    }
}
