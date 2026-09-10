pub mod global_state;
pub(crate) mod keepalive;
pub mod knock;
pub mod mailbox;
pub mod notification_contract;
pub mod notification_state;
pub mod state;
pub mod timers;

pub use global_state::{GlobalState, ProjectRegistration, RuntimeBinding};

use crate::identity::{AgentId, AppServerId, BindingId, CommandId, OperationId, RuntimeId};
use crate::proto::{CommandEnvelope, ProjectContext, Req, RequestEnvelope, Resp, MSG_TYPES};
use crate::scope::{HostPaths, ProjectScopeId, RouteScope, Scope};
use crate::server::knock::{
    append_log, knock_or_log, pane_alive, pane_idle, pane_presence, PanePresence,
};
use mailbox::{
    batch_notification_text, compose_notification, default_direct_message_id,
    is_explicit_notification, missing_recipient_projection_messages, notification_text,
    read_recipient_mailbox, truncate_notification, DEFAULT_DIRECT_MESSAGE_TTL_SECONDS,
    MAX_ACTIVE_SUBSCRIPTIONS_PER_WORKER, MAX_NOTIFICATION_TTL_SECONDS, NOTIFICATION_EVENTS,
};
use serde_json::json;
use state::{
    goal_deadline_key, now_ms, runtime_for_pane, task_resource_active, wait_cycle, CleanupReceipt,
    Event, GlobalEvent, Message, MigrationRecord, NotificationSubscription, State, TaskRec,
    TypedCommand, TypedEnvelope, WaitSpec, WorkerRec, WorktreeBinding, MAX_WAKE_ATTEMPTS,
};
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::{Arc, Mutex};
use std::time::Duration;
use tokio::net::UnixListener;
use tokio::sync::Notify;

const MAX_POLL_MS: u64 = 3_600_000;
const TASK_STATUSES: [&str; 12] = [
    "assigned",
    "working",
    "blocked",
    "waiting",
    "verifying",
    "reviewed",
    "delivered",
    "accepted",
    "rework",
    "merged",
    "closed",
    "cancelled",
];
const MAX_WORKTREE_PATH_BYTES: usize = 80;
/// Lock used by releases before the host-scoped state directory existed.
/// A new daemon must fence this writer before it replays the project journal;
/// otherwise an old binary could append concurrently under the new socket.
const LEGACY_HOST_DAEMON_LOCK_PATH: &str = "/tmp/collab-host.lock";

fn sanitize_identifier(value: &str) -> String {
    value
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.') {
                c
            } else {
                '_'
            }
        })
        .collect()
}

#[cfg(test)]
#[derive(Clone, Copy)]
pub(crate) enum CommandJournalFault {
    StartAppend = 1,
    StartSync = 2,
    CompletionAppend = 3,
    CompletionSync = 4,
}

#[cfg(test)]
thread_local! {
    static COMMAND_JOURNAL_FAULT: std::cell::Cell<u8> = const { std::cell::Cell::new(0) };
}

#[cfg(test)]
pub(crate) fn inject_command_journal_fault(fault: CommandJournalFault) {
    COMMAND_JOURNAL_FAULT.with(|injected| injected.set(fault as u8));
}

#[cfg(test)]
#[derive(Clone, Copy, Debug)]
pub(crate) enum SubagentJournalFault {
    StartAppend = 11,
    StartSync = 12,
    CloseFirstAppend = 21,
    CloseFirstSync = 22,
    CloseFinalAppend = 31,
    CloseFinalSync = 32,
    WorkingAppend = 41,
    WorkingSync = 42,
}

#[cfg(test)]
thread_local! {
    static SUBAGENT_JOURNAL_FAULT: std::cell::Cell<u8> = const { std::cell::Cell::new(0) };
}

#[cfg(test)]
pub(crate) fn inject_subagent_journal_fault(fault: SubagentJournalFault) {
    SUBAGENT_JOURNAL_FAULT.with(|injected| injected.set(fault as u8));
}

#[derive(Clone, Copy)]
enum CommandJournalPhase {
    Start,
    Business,
    Completion,
}

#[cfg(test)]
const DIRECT_MESSAGE_WAKE_COOLDOWN_MS: i64 = 60_000;

#[derive(Debug)]
enum NotificationDeliveryError {
    Journal(notification_contract::JournalError),
}

impl std::fmt::Display for NotificationDeliveryError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Journal(error) => write!(f, "notification delivery commit failed: {error}"),
        }
    }
}

impl std::error::Error for NotificationDeliveryError {}

fn validate_worktree_path(root: &Path, raw: &str) -> Result<PathBuf, String> {
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
    let candidate = if path.is_absolute() {
        path.to_path_buf()
    } else {
        let relative = raw.strip_prefix("./").unwrap_or(raw);
        root.join(relative)
    };
    let canonical_root = root
        .canonicalize()
        .map_err(|error| format!("project root cannot be canonicalized: {error}"))?;
    let canonical_playground = canonical_root.join("playground");
    let mut existing = candidate.as_path();
    while !existing.exists() {
        existing = existing
            .parent()
            .ok_or_else(|| "worktree path has no existing parent".to_string())?;
    }
    let canonical_existing = existing
        .canonicalize()
        .map_err(|error| format!("worktree path cannot be canonicalized: {error}"))?;
    let suffix = candidate
        .strip_prefix(existing)
        .map_err(|_| "worktree path cannot be resolved under project root".to_string())?;
    let canonical_candidate = canonical_existing.join(suffix);
    if !canonical_candidate.starts_with(&canonical_playground) {
        return Err("worktree path must be inside ./playground".into());
    }
    if raw.as_bytes().len() > MAX_WORKTREE_PATH_BYTES {
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
    Ok(canonical_candidate)
}

fn task_claim_held(status: &str) -> bool {
    matches!(
        status,
        "working"
            | "blocked"
            | "verifying"
            | "reviewed"
            | "delivered"
            | "accepted"
            | "rework"
            | "merged"
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
                | ("delivered", "accepted" | "rework" | "cancelled")
                | ("accepted", "merged" | "rework" | "cancelled")
        )
}

pub struct Server {
    pub config: crate::config::Config,
    pub root: PathBuf,
    pub state: Mutex<State>,
    pub journal: Mutex<std::fs::File>,
    pub pane_alive_check: fn(&str) -> PanePresence,
    pub pane_owner_check: fn(&str, &str) -> Result<bool, ()>,
    pub pane_state_check: fn(&str) -> crate::server::knock::AgentState,
    pub mailbox_notify: Notify,
}

fn record_activity(root: &Path, kind: &str, detail: serde_json::Value) -> Result<(), String> {
    let path = root.join(".agent-collab/server/events.jsonl");
    let record = json!({
        "ts": now_ms(),
        "kind": kind,
        "detail": detail,
    });
    let mut file = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(path)
        .map_err(|error| format!("open events: {error}"))?;
    use std::io::Write;
    let mut line =
        serde_json::to_vec(&record).map_err(|error| format!("serialize events: {error}"))?;
    line.push(b'\n');
    file.write_all(&line)
        .map_err(|error| format!("append events: {error}"))
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
    pub(crate) fn log_path_for(root: &Path) -> PathBuf {
        root.join(".agent-collab").join("server").join("log.txt")
    }

    pub fn log_path(&self) -> PathBuf {
        Self::log_path_for(&self.root)
    }

    /// Build the R1 typed envelope that the compatible CLI registration path
    /// commits through the same daemon journal as legacy lifecycle events.
    pub fn typed_register_envelope(
        &self,
        worker_id: &str,
        token: &str,
        pane: &str,
        cwd: &str,
    ) -> Result<TypedEnvelope, String> {
        let project_scope = GlobalState::canonical_project_scope(Path::new(cwd))
            .map_err(|error| error.to_string())?;
        self.typed_register_envelope_for_scope(worker_id, token, pane, project_scope, cwd)
    }

    fn typed_register_envelope_for_scope(
        &self,
        worker_id: &str,
        token: &str,
        pane: &str,
        project_scope: ProjectScopeId,
        worker_cwd: &str,
    ) -> Result<TypedEnvelope, String> {
        let app_scope = AppServerId::new("tui-default").map_err(|error| error.to_string())?;
        let binding_text = sanitize_identifier(&format!("binding-{worker_id}"));
        let binding_id = BindingId::new(binding_text.clone()).map_err(|error| error.to_string())?;
        let route_scope = RouteScope {
            app_scope_id: app_scope.clone(),
            project_scope_id: project_scope.clone(),
        };
        let generation = {
            let st = self.state.lock().unwrap();
            match st.global.lookup_binding_for(&route_scope, &binding_id) {
                Some(existing) => existing
                    .endpoint_generation
                    .checked_add(1)
                    .ok_or_else(|| "endpoint generation overflow".to_string())?,
                None => 1,
            }
        };
        let (registration, registered_ms) = {
            let st = self.state.lock().unwrap();
            let registration = st
                .global
                .lookup_registration(&project_scope, &app_scope)
                .cloned()
                .map(Ok)
                .unwrap_or_else(|| {
                    global_state::ProjectRegistration::with_registered_at(
                        project_scope.clone(),
                        app_scope.clone(),
                        now_ms(),
                    )
                })
                .map_err(|error| error.to_string())?;
            let registered_ms = st
                .workers
                .get(worker_id)
                .map(|worker| worker.registered_ms)
                .unwrap_or_else(now_ms);
            (registration, registered_ms)
        };
        let agent_id = AgentId::new(worker_id.to_string()).map_err(|error| error.to_string())?;
        let runtime_id = RuntimeId::new(format!("runtime-{}", sanitize_identifier(pane)))
            .map_err(|error| error.to_string())?;
        let binding = RuntimeBinding::new(
            project_scope.clone(),
            app_scope,
            agent_id,
            runtime_id,
            binding_id.clone(),
            generation,
            None,
        )
        .map_err(|error| error.to_string())?;
        let command_id = CommandId::new(format!("register-{binding_text}-{generation}"))
            .map_err(|error| error.to_string())?;
        let operation_id = OperationId::new(format!("register-op-{binding_text}-{generation}"))
            .map_err(|error| error.to_string())?;
        let expected_revision = {
            let st = self.state.lock().unwrap();
            st.revision
        };
        let envelope = CommandEnvelope::new(
            command_id,
            operation_id,
            binding_id,
            generation,
            route_scope,
            Some(expected_revision),
            None,
            None,
            None,
        );
        let worker = WorkerRec {
            id: worker_id.to_string(),
            token: token.to_string(),
            pane: Some(pane.to_string()),
            cwd: worker_cwd.to_string(),
            registered_ms,
        };
        Ok(TypedEnvelope {
            command: TypedCommand::RegisterWorker {
                registration,
                binding,
                worker,
            },
            envelope,
        })
    }

    /// Dispatch one typed command through validation, journal append, flush
    /// and reducer apply. This is the production typed seam above the legacy
    /// CLI adapters; the legacy v1 call site routes through it.
    pub fn typed_dispatch(
        &self,
        typed: TypedEnvelope,
    ) -> Result<state::TypedOutcome, notification_contract::JournalError> {
        typed.envelope.validate().map_err(|error| {
            notification_contract::JournalError::InvalidCommand(error.to_string())
        })?;
        let mut st = self.state.lock().unwrap();
        self.validate_typed_register(&st, &typed)?;
        let mut events = Vec::new();
        for global_event in typed.command.global_events() {
            match global_event {
                GlobalEvent::ProjectRegistered { registration } => {
                    events.push(Event::GlobalProjectRegistered { registration })
                }
                GlobalEvent::RuntimeBound { binding } => {
                    events.push(Event::GlobalRuntimeBound { binding })
                }
            }
        }
        let TypedCommand::RegisterWorker { worker, .. } = &typed.command;
        events.push(Event::Registered {
            worker: worker.clone(),
        });
        if let Some(pane) = worker.pane.as_deref() {
            events.extend(default_direct_message_events(
                &st,
                &worker.id,
                pane,
                now_ms(),
            ));
        }
        let outcome = serde_json::json!({
            "command_id": typed.envelope.command_id.as_str(),
            "operation_id": typed.envelope.operation_id.as_str(),
            "scope": typed.envelope.scope,
        });
        let committed = self.commit_command_locked(
            &mut st,
            typed.envelope.command_id.as_str(),
            typed.envelope.operation_id.as_str(),
            &events,
            outcome,
            typed.envelope.expected_revision,
        )?;
        Ok(state::TypedOutcome {
            receipt: global_state::CommandReceipt {
                command_id: typed.envelope.command_id.clone(),
                operation_id: typed.envelope.operation_id.clone(),
                epoch: global_state::INITIAL_EPOCH,
                sequence: committed.receipt.sequence,
                revision: committed.receipt.revision,
                outcome: committed.outcome,
            },
            replayed: committed.replayed,
        })
    }

    fn validate_typed_register(
        &self,
        st: &State,
        typed: &TypedEnvelope,
    ) -> Result<(), notification_contract::JournalError> {
        let TypedCommand::RegisterWorker {
            registration,
            binding,
            worker,
        } = &typed.command;
        if binding.agent_id.as_str() != worker.id {
            return Err(notification_contract::JournalError::InvalidCommand(
                "runtime binding agent does not match worker identity".into(),
            ));
        }
        if registration.route_scope() != binding.route_scope() {
            return Err(notification_contract::JournalError::InvalidCommand(
                "project registration scope does not match runtime binding scope".into(),
            ));
        }
        if let Some(existing) = st.workers.get(&worker.id) {
            if existing.token != worker.token {
                let same_session = worker
                    .pane
                    .as_deref()
                    .and_then(tmux_session_for_pane)
                    .is_some_and(|session| session == worker.id)
                    && existing
                        .pane
                        .as_deref()
                        .and_then(tmux_session_for_pane)
                        .is_some_and(|session| session == worker.id);
                if !same_session {
                    return Err(notification_contract::JournalError::InvalidCommand(
                        "worker token does not belong to the registered runtime identity".into(),
                    ));
                }
            }
        }
        if typed.envelope.actor_binding_id != binding.binding_id {
            return Err(notification_contract::JournalError::InvalidCommand(
                format!(
                    "actor binding {} does not match command binding {}",
                    typed.envelope.actor_binding_id, binding.binding_id
                ),
            ));
        }
        if typed.envelope.endpoint_generation != binding.endpoint_generation {
            return Err(notification_contract::JournalError::InvalidCommand(
                format!(
                    "envelope generation {} does not match binding generation {}",
                    typed.envelope.endpoint_generation, binding.endpoint_generation
                ),
            ));
        }
        if typed.envelope.scope != binding.route_scope() {
            return Err(notification_contract::JournalError::InvalidCommand(
                "envelope scope does not match binding route scope".into(),
            ));
        }
        let mut next = st.global.clone();
        for event in typed.command.global_events() {
            event.apply(&mut next).map_err(|error| {
                notification_contract::JournalError::InvalidCommand(error.to_string())
            })?;
        }
        next.validate_binding(binding).map_err(|error| {
            notification_contract::JournalError::InvalidCommand(error.to_string())
        })?;
        Ok(())
    }

    /// Apply events to memory and persist them atomically-ordered in the journal.
    pub(crate) fn commit(&self, evs: &[Event]) {
        let mut st = self.state.lock().unwrap();
        self.commit_locked(&mut st, evs);
    }

    /// Fallible reducer entry point used by typed producers. Legacy v1 call
    /// sites still use `commit`; they retain the fail-closed panic boundary.
    pub fn commit_checked(
        &self,
        evs: &[Event],
    ) -> Result<notification_contract::CommitReceipt, notification_contract::JournalError> {
        let mut st = self.state.lock().unwrap();
        self.commit_locked_checked(&mut st, evs)
    }

    /// Compatibility entry point for callers that only need a string error.
    /// The checked reducer remains the single journal/state owner.
    pub(crate) fn try_commit(&self, evs: &[Event]) -> Result<(), String> {
        self.commit_checked(evs)
            .map(|_| ())
            .map_err(|error| error.to_string())
    }

    /// Compatibility entry point for callers holding the state lock.
    /// This delegates to the typed reducer and never applies state after a
    /// journal failure.
    pub(crate) fn try_commit_locked(&self, st: &mut State, evs: &[Event]) -> Result<(), String> {
        self.commit_locked_checked(st, evs)
            .map(|_| ())
            .map_err(|error| error.to_string())
    }

    /// Commit one command and its outcome atomically. A retry with the same
    /// command id returns the recorded outcome without appending another event.
    /// Reusing a command id for a different operation is rejected explicitly.
    pub fn commit_command(
        &self,
        command_id: &str,
        operation_id: &str,
        evs: &[Event],
        outcome: serde_json::Value,
    ) -> Result<notification_contract::CommandOutcome, notification_contract::JournalError> {
        validate_command_id(command_id)?;
        validate_command_id(operation_id)?;
        let mut st = self.state.lock().unwrap();
        self.commit_command_locked(&mut st, command_id, operation_id, evs, outcome, None)
    }

    fn commit_command_at_revision(
        &self,
        command_id: &str,
        operation_id: &str,
        evs: &[Event],
        outcome: serde_json::Value,
        expected_revision: u64,
    ) -> Result<notification_contract::CommandOutcome, notification_contract::JournalError> {
        validate_command_id(command_id)?;
        validate_command_id(operation_id)?;
        let mut st = self.state.lock().unwrap();
        self.commit_command_locked(
            &mut st,
            command_id,
            operation_id,
            evs,
            outcome,
            Some(expected_revision),
        )
    }

    fn commit_command_locked(
        &self,
        st: &mut State,
        command_id: &str,
        operation_id: &str,
        evs: &[Event],
        outcome: serde_json::Value,
        expected_revision: Option<u64>,
    ) -> Result<notification_contract::CommandOutcome, notification_contract::JournalError> {
        let typed_command_id = CommandId::new(command_id.to_owned()).map_err(|error| {
            notification_contract::JournalError::InvalidCommand(error.to_string())
        })?;
        if let Some(existing) = st.global.lookup_command_receipt(&typed_command_id) {
            if existing.operation_id.as_str() != operation_id {
                return Err(notification_contract::JournalError::InvalidCommand(
                    format!(
                        "command_id {command_id} already belongs to operation {}",
                        existing.operation_id
                    ),
                ));
            }
            return Ok(notification_contract::CommandOutcome {
                receipt: notification_contract::CommitReceipt {
                    sequence: existing.sequence,
                    revision: existing.revision,
                },
                operation_id: existing.operation_id.as_str().to_owned(),
                outcome: existing.outcome.clone(),
                replayed: true,
            });
        }
        if let Some(existing) = st.command_receipts.get(command_id) {
            if existing.operation_id != operation_id {
                return Err(notification_contract::JournalError::InvalidCommand(
                    format!(
                        "command_id {command_id} already belongs to operation {}",
                        existing.operation_id
                    ),
                ));
            }
            return Ok(notification_contract::CommandOutcome {
                receipt: notification_contract::CommitReceipt {
                    sequence: existing.sequence,
                    revision: existing.revision,
                },
                operation_id: existing.operation_id.clone(),
                outcome: existing.outcome.clone(),
                replayed: true,
            });
        }
        if let Some((existing_command_id, _)) =
            st.global
                .command_receipts
                .iter()
                .find(|(existing_command_id, receipt)| {
                    *existing_command_id != command_id
                        && receipt.operation_id.as_str() == operation_id
                })
        {
            return Err(notification_contract::JournalError::InvalidCommand(
                format!(
                    "operation_id {operation_id} already belongs to command_id {existing_command_id}"
                ),
            ));
        }
        if let Some((existing_command_id, _)) =
            st.command_receipts
                .iter()
                .find(|(existing_command_id, receipt)| {
                    *existing_command_id != command_id && receipt.operation_id == operation_id
                })
        {
            return Err(notification_contract::JournalError::InvalidCommand(
                format!(
                    "operation_id {operation_id} already belongs to command_id {existing_command_id}"
                ),
            ));
        }
        if let Some(expected_revision) = expected_revision {
            let observed_revision = st.revision;
            if observed_revision != expected_revision {
                return Err(notification_contract::JournalError::InvalidCommand(format!(
                    "compare-and-swap revision mismatch: expected {expected_revision}, observed {observed_revision}"
                )));
            }
        }
        let event_count = evs.len().checked_add(2).ok_or_else(|| {
            notification_contract::JournalError::InvalidCommand(
                "command event count overflow".into(),
            )
        })? as u64;
        let sequence = st.sequence.checked_add(event_count).ok_or_else(|| {
            notification_contract::JournalError::InvalidCommand("sequence counter overflow".into())
        })?;
        let revision = st.revision.checked_add(event_count).ok_or_else(|| {
            notification_contract::JournalError::InvalidCommand("revision counter overflow".into())
        })?;
        let receipt = state::CommandReceipt {
            operation_id: operation_id.to_owned(),
            outcome: outcome.clone(),
            sequence,
            revision,
        };
        let started = Event::CommandStarted {
            command_id: command_id.to_owned(),
            operation_id: operation_id.to_owned(),
        };
        let completed = Event::CommandCompleted {
            command_id: command_id.to_owned(),
            operation_id: operation_id.to_owned(),
            receipt: receipt.clone(),
        };
        self.append_command_phase_locked(
            st,
            std::slice::from_ref(&started),
            CommandJournalPhase::Start,
        )?;
        self.append_command_phase_locked(st, evs, CommandJournalPhase::Business)?;
        self.append_command_phase_locked(
            st,
            std::slice::from_ref(&completed),
            CommandJournalPhase::Completion,
        )?;
        let mut events = Vec::with_capacity(evs.len() + 2);
        events.push(started);
        events.extend_from_slice(evs);
        events.push(completed);
        self.apply_committed_events(st, &events)?;
        let has_pending_scheduler_admission = events.iter().any(|event| {
            matches!(
                event,
                Event::SchedulerAdmission { admission } if admission.status == "pending"
            )
        });
        let has_succeeded_scheduler_admission = events.iter().any(|event| {
            let Event::SchedulerAdmissionStatus {
                request_id, status, ..
            } = event
            else {
                return false;
            };
            status == "succeeded"
                && st
                    .scheduler_admissions
                    .get(request_id)
                    .is_some_and(|admission| {
                        admission.status == "succeeded"
                            && st.msgs.get(&admission.message_id).is_some_and(|message| {
                                message.state == "pending"
                                    && st.scheduler_message_deliverable(&message.id)
                            })
                    })
        });
        if (events
            .iter()
            .any(|event| matches!(event, Event::Sent { .. }))
            && !has_pending_scheduler_admission)
            || has_succeeded_scheduler_admission
        {
            self.mailbox_notify.notify_waiters();
        }
        Ok(notification_contract::CommandOutcome {
            receipt: notification_contract::CommitReceipt { sequence, revision },
            operation_id: operation_id.to_owned(),
            outcome,
            replayed: false,
        })
    }

    pub(crate) fn commit_locked(&self, st: &mut State, evs: &[Event]) {
        self.commit_locked_checked(st, evs)
            .unwrap_or_else(|error| panic!("collab journal commit failed: {error}"));
    }

    pub(crate) fn commit_locked_checked(
        &self,
        st: &mut State,
        evs: &[Event],
    ) -> Result<notification_contract::CommitReceipt, notification_contract::JournalError> {
        if let Some(error) = &st.journal_poison {
            return Err(notification_contract::JournalError::Append(error.clone()));
        }
        use std::io::Write;
        // Persist control truth before any state change or external notification.
        // A failed journal poisons this owner instead of silently resetting budgets.
        let mut buf = Vec::new();
        for ev in evs {
            let line = match serde_json::to_string(ev) {
                Ok(line) => line,
                Err(error) => {
                    let message = error.to_string();
                    st.journal_poison = Some(message.clone());
                    return Err(notification_contract::JournalError::Append(message));
                }
            };
            buf.extend_from_slice(line.as_bytes());
            buf.push(b'\n');
        }
        #[cfg(test)]
        let append_fault = SUBAGENT_JOURNAL_FAULT.with(|injected| {
            let injected_fault = injected.get();
            let close_final = injected_fault == SubagentJournalFault::CloseFinalAppend as u8
                && evs.iter().any(|event| {
                matches!(event, Event::SubagentUpdated { subagent } if subagent.status == "closed")
            });
            if close_final
                || injected_fault == SubagentJournalFault::StartAppend as u8
                || injected_fault == SubagentJournalFault::CloseFirstAppend as u8
                || injected_fault == SubagentJournalFault::WorkingAppend as u8
            {
                injected.set(0);
                true
            } else {
                false
            }
        });
        #[cfg(not(test))]
        let append_fault = false;
        if append_fault {
            let message = "injected subagent journal append failure".to_string();
            st.journal_poison = Some(message.clone());
            return Err(notification_contract::JournalError::Append(message));
        }
        let mut j = self.journal.lock().unwrap();
        if let Err(error) = j.write_all(&buf) {
            let message = error.to_string();
            st.journal_poison = Some(message.clone());
            return Err(notification_contract::JournalError::Append(message));
        }
        #[cfg(test)]
        let sync_fault = SUBAGENT_JOURNAL_FAULT.with(|injected| {
            let injected_fault = injected.get();
            let close_final = injected_fault == SubagentJournalFault::CloseFinalSync as u8
                && evs.iter().any(|event| {
                matches!(event, Event::SubagentUpdated { subagent } if subagent.status == "closed")
            });
            if close_final
                || injected_fault == SubagentJournalFault::StartSync as u8
                || injected_fault == SubagentJournalFault::CloseFirstSync as u8
                || injected_fault == SubagentJournalFault::WorkingSync as u8
            {
                injected.set(0);
                true
            } else {
                false
            }
        });
        #[cfg(not(test))]
        let sync_fault = false;
        if sync_fault {
            let message = "injected subagent journal sync failure".to_string();
            st.journal_poison = Some(message.clone());
            return Err(notification_contract::JournalError::Flush(message));
        }
        if let Err(error) = j.sync_data() {
            let message = error.to_string();
            st.journal_poison = Some(message.clone());
            return Err(notification_contract::JournalError::Flush(message));
        }
        self.apply_committed_events(st, evs)?;
        let has_pending_scheduler_admission = evs.iter().any(|event| {
            matches!(
                event,
                Event::SchedulerAdmission { admission } if admission.status == "pending"
            )
        });
        let has_succeeded_scheduler_admission = evs.iter().any(|event| {
            let Event::SchedulerAdmissionStatus {
                request_id, status, ..
            } = event
            else {
                return false;
            };
            status == "succeeded"
                && st
                    .scheduler_admissions
                    .get(request_id)
                    .is_some_and(|admission| {
                        admission.status == "succeeded"
                            && st.msgs.get(&admission.message_id).is_some_and(|message| {
                                message.state == "pending"
                                    && st.scheduler_message_deliverable(&message.id)
                            })
                    })
        });
        if (evs.iter().any(|event| matches!(event, Event::Sent { .. }))
            && !has_pending_scheduler_admission)
            || has_succeeded_scheduler_admission
        {
            self.mailbox_notify.notify_waiters();
        }
        Ok(notification_contract::CommitReceipt {
            sequence: st.sequence,
            revision: st.revision,
        })
    }

    fn append_command_phase_locked(
        &self,
        st: &mut State,
        evs: &[Event],
        phase: CommandJournalPhase,
    ) -> Result<(), notification_contract::JournalError> {
        if let Some(error) = &st.journal_poison {
            return Err(notification_contract::JournalError::Append(error.clone()));
        }
        let mut body = Vec::new();
        for ev in evs {
            let line = match serde_json::to_string(ev) {
                Ok(line) => line,
                Err(error) => {
                    let message = error.to_string();
                    st.journal_poison = Some(message.clone());
                    return Err(notification_contract::JournalError::Append(message));
                }
            };
            body.extend_from_slice(line.as_bytes());
            body.push(b'\n');
        }
        let mut journal = self.journal.lock().unwrap();
        #[cfg(test)]
        let append_fault = COMMAND_JOURNAL_FAULT.with(|injected| {
            let expected = match phase {
                CommandJournalPhase::Start => CommandJournalFault::StartAppend as u8,
                CommandJournalPhase::Completion => CommandJournalFault::CompletionAppend as u8,
                CommandJournalPhase::Business => 0,
            };
            if injected.get() == expected && expected != 0 {
                injected.set(0);
                true
            } else {
                false
            }
        });
        #[cfg(not(test))]
        let append_fault = false;
        #[cfg(not(test))]
        let _ = phase;
        if append_fault {
            let message = "injected command journal append failure".to_string();
            st.journal_poison = Some(message.clone());
            return Err(notification_contract::JournalError::Append(message));
        }
        if let Err(error) = std::io::Write::write_all(&mut *journal, &body) {
            let message = error.to_string();
            st.journal_poison = Some(message.clone());
            return Err(notification_contract::JournalError::Append(message));
        }
        #[cfg(test)]
        let sync_fault = COMMAND_JOURNAL_FAULT.with(|injected| {
            let expected = match phase {
                CommandJournalPhase::Start => CommandJournalFault::StartSync as u8,
                CommandJournalPhase::Completion => CommandJournalFault::CompletionSync as u8,
                CommandJournalPhase::Business => 0,
            };
            if injected.get() == expected && expected != 0 {
                injected.set(0);
                true
            } else {
                false
            }
        });
        #[cfg(not(test))]
        let sync_fault = false;
        if sync_fault {
            let message = "injected command journal sync failure".to_string();
            st.journal_poison = Some(message.clone());
            return Err(notification_contract::JournalError::Flush(message));
        }
        if let Err(error) = journal.sync_data() {
            let message = error.to_string();
            st.journal_poison = Some(message.clone());
            return Err(notification_contract::JournalError::Flush(message));
        }
        Ok(())
    }

    fn apply_committed_events(
        &self,
        st: &mut State,
        evs: &[Event],
    ) -> Result<(), notification_contract::JournalError> {
        for ev in evs {
            if let Err(error) = st.apply_checked(ev) {
                st.journal_poison.get_or_insert(error.clone());
                return Err(notification_contract::JournalError::Reducer(error));
            }
            if let Err(error) = st.advance_version() {
                st.journal_poison.get_or_insert(error.clone());
                return Err(notification_contract::JournalError::Reducer(error));
            }
            if let Event::Sent { msg } = ev {
                if let Err(error) = self.backup_message(msg) {
                    self.report_mailbox_projection_error(error);
                }
            }
            if let Event::Delivered { ids } = ev {
                for id in ids {
                    if let Some(msg) = st.msgs.get(id) {
                        if let Err(error) = self.backup_message(msg) {
                            self.report_mailbox_projection_error(error);
                        }
                    }
                }
            }
            if let Event::Acked { ids } = ev {
                for id in ids {
                    if let Some(msg) = st.msgs.get(id) {
                        if let Err(error) = self.backup_message(msg) {
                            self.report_mailbox_projection_error(error);
                        }
                    }
                }
            }
        }
        Ok(())
    }

    fn report_mailbox_projection_error(&self, error: String) {
        // Journal truth is already durable. Keep projection failure explicit
        // and queryable without turning it into a false delivery result.
        append_log(
            &self.log_path(),
            &format!("MAILBOX_JSONL_WRITE_FAILED: {error}"),
        );
        if let Err(activity_error) = record_activity(
            &self.root,
            "mailbox_projection_error",
            json!({"exact_error": error, "recoverable": true}),
        ) {
            append_log(
                &self.log_path(),
                &format!("MAILBOX_PROJECTION_ERROR_RECORD_FAILED: {activity_error}"),
            );
        }
    }

    fn backup_message(&self, msg: &Message) -> Result<(), String> {
        mailbox::backup_message(&self.root, msg)
    }

    fn rewrite_journal_locked(
        &self,
        st: &State,
    ) -> Result<(), notification_contract::JournalError> {
        let path = self.root.join(".agent-collab/server/journal.jsonl");
        let tmp = path.with_file_name("journal.jsonl.tmp");
        let mut body = String::new();
        let events = st.snapshot_events();
        for (index, event) in events.iter().enumerate() {
            let line = serde_json::to_string(event).map_err(|error| {
                notification_contract::JournalError::Append(format!("compact serialize: {error}"))
            })?;
            body.push_str(&line);
            let next_is_checkpoint = events
                .get(index + 1)
                .is_some_and(|next| matches!(next, Event::ReducerCheckpoint { .. }));
            if index + 1 != events.len() && !next_is_checkpoint {
                body.push('\n');
            }
        }
        if events
            .last()
            .is_some_and(|event| matches!(event, Event::ReducerCheckpoint { .. }))
        {
            body.push('\n');
        }
        std::fs::write(&tmp, body).map_err(|error| {
            notification_contract::JournalError::Append(format!("compact write: {error}"))
        })?;
        std::fs::rename(&tmp, &path).map_err(|error| {
            notification_contract::JournalError::Append(format!("compact rename: {error}"))
        })?;
        let file = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(&path)
            .map_err(|error| {
                notification_contract::JournalError::Append(format!("compact reopen: {error}"))
            })?;
        *self.journal.lock().unwrap() = file;
        Ok(())
    }
}

fn validate_command_id(value: &str) -> Result<(), notification_contract::JournalError> {
    if value.is_empty() || value.len() > 256 || value.chars().any(char::is_control) {
        return Err(notification_contract::JournalError::InvalidCommand(
            "command and operation ids must be non-empty, <=256 bytes, and control-free".into(),
        ));
    }
    Ok(())
}

pub(crate) fn purge_expired_storage(server: &Server, now: i64) -> usize {
    if server.state.lock().unwrap().admission_frozen() {
        return 0;
    }
    let cutoff = server.config.retention.cutoff_ms(now);
    let mut st = server.state.lock().unwrap();
    let expired: Vec<String> = st
        .msgs
        .values()
        .filter(|message| message.created_ms <= cutoff)
        .map(|message| message.id.clone())
        .collect();
    mailbox::purge_message_snapshot_files(&server.root, &expired, &st);
    if expired.is_empty() {
        return 0;
    }
    for id in &expired {
        st.drop_message(id);
    }
    if let Err(error) = server.rewrite_journal_locked(&st) {
        st.journal_poison = Some(error.to_string());
        append_log(
            &server.log_path(),
            &format!("JOURNAL_COMPACTION_FAILED: {error}"),
        );
    }
    expired.len()
}

pub fn gen_msg_id() -> String {
    use std::sync::atomic::{AtomicU64, Ordering};
    static SEQ: AtomicU64 = AtomicU64::new(1);
    let n = SEQ.fetch_add(1, Ordering::Relaxed);
    format!("m{}-{}", now_ms(), n)
}

fn worktree_binding_for_task(server: &Server, task: &TaskRec) -> Option<WorktreeBinding> {
    let worktree_root = task.worktree_path.as_ref()?.clone();
    let owning_project_scope = server.root.to_str()?.to_owned();
    Some(WorktreeBinding {
        worktree_root,
        owning_project_scope,
        task_id: task.id.clone(),
        owner_agent_id: task.owner.clone(),
        binding_id: format!("binding-task-{}", task.id),
        base_commit: task.base_commit.clone().unwrap_or_default(),
    })
}

fn iso(ms: i64) -> String {
    chrono::DateTime::from_timestamp_millis(ms)
        .map(|d| d.to_rfc3339())
        .unwrap_or_default()
}

fn default_direct_message_events(
    state: &State,
    worker_id: &str,
    pane: &str,
    now: i64,
) -> Vec<Event> {
    let mut events = Vec::new();
    let default_id = default_direct_message_id(worker_id);
    if state
        .notification_subscriptions
        .get(&default_id)
        .is_some_and(|subscription| subscription.status == "cancelled")
    {
        return events;
    }
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
            status_reason: None,
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

fn registered_peer_rebind_events(
    state: &State,
    pane_for_worker: &dyn Fn(&str) -> Option<String>,
    owns_pane: &dyn Fn(&str, &str) -> bool,
) -> Vec<Event> {
    let now = now_ms();
    state
        .workers
        .values()
        .filter_map(|worker| {
            if worker
                .pane
                .as_deref()
                .is_some_and(|pane| owns_pane(&worker.id, pane))
            {
                return None;
            }
            let pane = pane_for_worker(&worker.id)?;
            if !owns_pane(&worker.id, &pane) {
                return None;
            }
            let mut rebound = worker.clone();
            rebound.pane = Some(pane.clone());
            let mut events = vec![Event::Registered { worker: rebound }];
            events.extend(
                state
                    .notification_subscriptions
                    .values()
                    .filter(|subscription| {
                        subscription.worker_id == worker.id
                            && subscription.status == "armed"
                            && subscription.pane != pane
                    })
                    .map(|subscription| Event::NotificationRebound {
                        subscription_id: subscription.id.clone(),
                        pane: pane.clone(),
                        updated_ms: now,
                    }),
            );
            Some(events)
        })
        .flatten()
        .collect()
}

fn restore_registered_peer_default_leases(server: &Server) {
    let rebind_events = {
        let state = server.state.lock().unwrap();
        registered_peer_rebind_events(&state, &tmux_pane_for_session, &|worker_id, pane| {
            tmux_session_for_pane(pane).as_deref() == Some(worker_id)
        })
    };
    if !rebind_events.is_empty() {
        server.commit(&rebind_events);
    }
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
    owns_pane: &dyn Fn(&str, &str) -> Result<bool, ()>,
) -> bool {
    attempt_notification_with_at(
        server,
        message_id,
        subscription_id,
        can_receive,
        deliver,
        owns_pane,
        now_ms(),
    )
}

fn attempt_notification_with_at(
    server: &Server,
    message_id: &str,
    subscription_id: &str,
    can_receive: &dyn Fn(&str) -> bool,
    deliver: &dyn Fn(&str, &str) -> bool,
    owns_pane: &dyn Fn(&str, &str) -> Result<bool, ()>,
    now: i64,
) -> bool {
    if !server.config.notifications.enabled {
        return false;
    }
    let (recipient, pane, delay, worker_pane, explicit) = {
        let state = server.state.lock().unwrap();
        let Some(seed) = state.msgs.get(message_id) else {
            return false;
        };
        if !state.scheduler_message_deliverable(message_id) {
            return false;
        }
        let recipient = seed.to.clone();
        let Some(subscription) = state.notification_subscriptions.get(subscription_id) else {
            return false;
        };
        if subscription.worker_id != recipient {
            return false;
        }
        let pane = subscription.pane.clone();
        let delay = state
            .delivery_modes
            .get(message_id)
            .filter(|mode| mode.as_str() == "explicit-notification")
            .map(|_| 0)
            .unwrap_or_else(|| server.config.notifications.delay_ms(&subscription.event));
        let worker_pane = state.workers.get(&recipient).and_then(|w| w.pane.clone());
        let explicit = is_explicit_notification(&state, seed);
        (recipient, pane, delay, worker_pane, explicit)
    };

    let worker_pane_mismatch = worker_pane.as_deref() != Some(&pane);
    let presence = if worker_pane_mismatch {
        PanePresence::Missing
    } else {
        (server.pane_alive_check)(&pane)
    };
    if presence == PanePresence::Unknown {
        return false;
    }
    let alive = presence == PanePresence::Present;
    let owned = if alive {
        match owns_pane(&recipient, &pane) {
            Ok(owned) => owned,
            Err(()) => return false,
        }
    } else {
        false
    };
    let state_probe = if alive && owned {
        (server.pane_state_check)(&pane)
    } else {
        crate::server::knock::AgentState::Absent
    };

    let mut state = server.state.lock().unwrap();
    if worker_pane_mismatch
        || !alive
        || !owned
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
    if state_probe == crate::server::knock::AgentState::Unknown
        || (state_probe == crate::server::knock::AgentState::Working && !explicit)
    {
        crate::server::knock::append_log(
            &server.log_path(),
            &format!("knock deferred pane={pane} recipient={recipient} agent state is unknown"),
        );
        return false;
    }
    if !explicit
        && state
            .msgs
            .values()
            .filter(|message| message.to == recipient && message.state == "delivered")
            .count() as u32
            >= server.config.notifications.max_unacked
    {
        crate::server::knock::append_log(
            &server.log_path(),
            &format!("knock deferred pane={pane} recipient={recipient} unacked notification limit reached"),
        );
        return false;
    }
    let mut batch = state
        .msgs
        .values()
        .filter_map(|message| {
            let binding = state.wake_bindings.get(&message.id)?;
            let sub = state.notification_subscriptions.get(binding)?;
            let message_explicit = is_explicit_notification(&state, message);
            (message.to == recipient
                && state.scheduler_message_deliverable(&message.id)
                && message_explicit == explicit
                && state
                    .delivery_modes
                    .get(&message.id)
                    .filter(|mode| mode.as_str() == "explicit-notification")
                    .map(|_| 0)
                    .unwrap_or_else(|| server.config.notifications.delay_ms(&sub.event))
                    == delay
                && message.state == "pending"
                && message.wake_attempt_count < MAX_WAKE_ATTEMPTS
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
    let Some(window_start_ms) = batch.first().map(|candidate| candidate.0) else {
        return false;
    };
    let (batch, remaining) = mailbox::select_batch(batch, delay, window_start_ms);
    let Some(first) = batch.first() else {
        return false;
    };
    let last_attempt = state
        .msgs
        .values()
        .filter_map(|message| {
            let binding = state.wake_bindings.get(&message.id)?;
            let sub = state.notification_subscriptions.get(binding)?;
            let message_explicit = is_explicit_notification(&state, message);
            (message.to == recipient
                && message_explicit == explicit
                && (message_explicit || server.config.notifications.delay_ms(&sub.event) == delay)
                && sub.worker_id == recipient
                && sub.pane == pane)
                .then_some(message.last_wake_attempt_ms)
        })
        .max()
        .unwrap_or(0);
    if now.saturating_sub(window_start_ms) < delay || now.saturating_sub(last_attempt) < delay {
        return false;
    }
    let ids = batch.iter().map(|m| m.1.clone()).collect::<Vec<_>>();
    let attempted_ids = ids.clone();
    if !can_receive(&pane) {
        server.commit_locked(
            &mut state,
            &[Event::WakeAttempted {
                ids: attempted_ids.clone(),
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
            ids: attempted_ids,
            attempted_ms: now,
        }],
    );
    drop(state);
    let text = truncate_notification(compose_notification(
        &first.1,
        "notification-batch",
        &batch_notification_text(&batch, remaining),
    ));
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
    match notification_contract::NotificationSink::submit(server, &events) {
        Ok(_) => true,
        Err(error) => {
            let error = NotificationDeliveryError::Journal(error);
            append_log(&server.log_path(), &error.to_string());
            false
        }
    }
}

fn attempt_notification(server: &Server, message_id: &str, subscription_id: &str) -> bool {
    attempt_notification_with(
        server,
        message_id,
        subscription_id,
        &|_| true,
        &|pane, text| knock_or_log(&server.log_path(), pane, text),
        &|worker_id, pane| (server.pane_owner_check)(worker_id, pane),
    )
}

#[cfg(test)]
mod notification_batch_tests {
    use super::*;
    use crate::server::state::{Event, NotificationSubscription, State, WorkerRec};
    use std::sync::{Arc, Mutex};

    fn test_server() -> (Arc<Server>, std::path::PathBuf) {
        static SEQUENCE: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
        let sequence = SEQUENCE.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        let root = std::env::temp_dir().join(format!(
            "collab-notification-batch-{}-{sequence}",
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
                config: crate::config::Config::default(),
                root: root.clone(),
                state: Mutex::new(State::default()),
                journal: Mutex::new(journal),
                pane_alive_check: |_| crate::server::knock::PanePresence::Present,
                pane_owner_check: |_, _| Ok(true),
                pane_state_check: |_| crate::server::knock::AgentState::Waiting,
                mailbox_notify: tokio::sync::Notify::new(),
            }),
            root,
        )
    }

    fn register_and_subscribe(server: &Server, worker_id: &str) -> String {
        let now = now_ms();
        let pane = format!("%test-{worker_id}");
        let subscription_id = format!("sub-{worker_id}");
        server.commit(&[
            Event::Registered {
                worker: WorkerRec {
                    id: worker_id.into(),
                    token: format!("token-{worker_id}"),
                    pane: Some(pane.clone()),
                    cwd: "/tmp".into(),
                    registered_ms: now,
                },
            },
            Event::NotificationSubscribed {
                subscription: NotificationSubscription {
                    id: subscription_id.clone(),
                    worker_id: worker_id.into(),
                    event: "direct-message".into(),
                    subject: None,
                    pane,
                    method: "tmux".into(),
                    trigger_ms: None,
                    trigger_times_ms: Vec::new(),
                    interval_ms: None,
                    repeat_count: 1,
                    fired_count: 0,
                    expires_ms: now + 300_000,
                    status: "armed".into(),
                    created_ms: now,
                    updated_ms: now,
                    status_reason: None,
                },
            },
        ]);
        subscription_id
    }

    fn queue_message(
        server: &Server,
        worker_id: &str,
        subscription_id: &str,
        message_id: &str,
        created_ms: i64,
    ) {
        server.commit(&[
            Event::Sent {
                msg: Message {
                    id: message_id.into(),
                    from: "peer".into(),
                    to: worker_id.into(),
                    mtype: "notify".into(),
                    subject: Some(format!("topic-{message_id}")),
                    body: format!("DETAIL-{message_id}"),
                    in_reply_to: None,
                    created_ms,
                    state: "pending".into(),
                    wake_attempt_count: 0,
                    last_wake_attempt_ms: 0,
                },
            },
            Event::WakeBound {
                message_id: message_id.into(),
                subscription_id: subscription_id.into(),
            },
        ]);
    }

    #[test]
    fn automatic_batch_does_not_cross_the_first_notice_window() {
        let (server, root) = test_server();
        let subscription_id = register_and_subscribe(&server, "recipient");
        let now = now_ms();
        queue_message(
            &server,
            "recipient",
            &subscription_id,
            "old-notice",
            now - 120_001,
        );
        queue_message(&server, "recipient", &subscription_id, "late-notice", now);

        let delivered = Mutex::new(Vec::new());
        assert!(attempt_notification_with_at(
            &server,
            "old-notice",
            &subscription_id,
            &|_| true,
            &|_, text| {
                delivered.lock().unwrap().push(text.to_string());
                true
            },
            &|_, _| Ok(true),
            now,
        ));

        let text = delivered.lock().unwrap().join("\n");
        assert!(text.contains("old-notice"));
        assert!(
            !text.contains("late-notice"),
            "a notice arriving after the first 120-second window must remain pending"
        );
        let state = server.state.lock().unwrap();
        assert_eq!(state.msgs["old-notice"].state, "delivered");
        assert_eq!(state.msgs["late-notice"].state, "pending");
        drop(state);
        let mailbox =
            std::fs::read_to_string(root.join(".agent-collab/mailbox/recipient-recipient.jsonl"))
                .unwrap();
        assert!(mailbox.contains("DETAIL-old-notice"));
        assert!(mailbox.contains("DETAIL-late-notice"));
        assert!(
            !text.contains("DETAIL-old-notice") && !text.contains("DETAIL-late-notice"),
            "batch wake must carry task summary while full details stay in JSONL"
        );
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn four_notice_batch_uses_the_original_window_start_after_capping() {
        let (server, root) = test_server();
        let subscription_id = register_and_subscribe(&server, "recipient");
        let window_start = 10_000_000;
        for (index, offset) in [(0, 0), (1, 60_000), (2, 70_000), (3, 80_000)] {
            queue_message(
                &server,
                "recipient",
                &subscription_id,
                &format!("backlog-{index}"),
                window_start + offset,
            );
        }

        let delivered = Mutex::new(Vec::new());
        assert!(attempt_notification_with_at(
            &server,
            "backlog-0",
            &subscription_id,
            &|_| true,
            &|_, text| {
                delivered.lock().unwrap().push(text.to_string());
                true
            },
            &|_, _| Ok(true),
            window_start + 120_000,
        ));
        let text = delivered.lock().unwrap().join("\n");
        assert!(!text.contains("backlog-0"));
        assert!(text.contains("backlog-1"));
        assert!(text.contains("backlog-2"));
        assert!(text.contains("backlog-3"));
        let state = server.state.lock().unwrap();
        assert_eq!(state.msgs["backlog-0"].state, "pending");
        assert_eq!(state.msgs["backlog-1"].state, "delivered");
        assert_eq!(state.msgs["backlog-2"].state, "delivered");
        assert_eq!(state.msgs["backlog-3"].state, "delivered");
        drop(state);
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn explicit_notice_is_immediate_and_isolated_from_automatic_batching() {
        let (server, root) = test_server();
        let subscription_id = register_and_subscribe(&server, "recipient");
        let now = now_ms();
        queue_message(
            &server,
            "recipient",
            &subscription_id,
            "automatic-notice",
            now - 120_001,
        );
        queue_message(
            &server,
            "recipient",
            &subscription_id,
            "explicit-notice",
            now,
        );
        server.commit(&[Event::DeliveryMode {
            msg_id: "explicit-notice".into(),
            mode: "explicit-notification".into(),
        }]);

        let delivered = Mutex::new(Vec::new());
        assert!(attempt_notification_with_at(
            &server,
            "explicit-notice",
            &subscription_id,
            &|_| true,
            &|_, text| {
                delivered.lock().unwrap().push(text.to_string());
                true
            },
            &|_, _| Ok(true),
            now,
        ));
        let text = delivered.lock().unwrap().join("\n");
        assert!(text.contains("explicit-notice"));
        assert!(!text.contains("automatic-notice"));
        let state = server.state.lock().unwrap();
        assert_eq!(state.msgs["explicit-notice"].state, "delivered");
        assert_eq!(state.msgs["automatic-notice"].state, "pending");
        drop(state);
        assert!(attempt_notification_with_at(
            &server,
            "automatic-notice",
            &subscription_id,
            &|_| true,
            &|_, text| {
                delivered.lock().unwrap().push(text.to_string());
                true
            },
            &|_, _| Ok(true),
            now,
        ));
        assert!(delivered
            .lock()
            .unwrap()
            .iter()
            .any(|text| { text.contains("automatic-notice") }));
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn mailbox_status_reports_a_valid_but_incomplete_projection() {
        let (server, root) = test_server();
        let subscription_id = register_and_subscribe(&server, "recipient");
        queue_message(
            &server,
            "recipient",
            &subscription_id,
            "projection-gap",
            now_ms(),
        );
        std::fs::write(
            root.join(".agent-collab/mailbox/recipient-recipient.jsonl"),
            "",
        )
        .unwrap();

        let response = dispatch(
            &server,
            Req::MailboxRead {
                all: false,
                sort: Some("time-asc".into()),
                worker_id: Some("recipient".into()),
            },
        );
        assert!(response.ok);
        assert_eq!(response.data["recipient_jsonl"]["status"], "incomplete");
        assert_eq!(
            response.data["recipient_jsonl"]["missing_message_ids"],
            serde_json::json!(["projection-gap"])
        );
        assert!(response.data["recipient_jsonl"]["exact_error"]
            .as_str()
            .unwrap()
            .contains("projection-gap"));
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn failed_reservation_is_durable_and_is_never_replayed() {
        let (server, root) = test_server();
        let subscription_id = register_and_subscribe(&server, "recipient");
        let now = now_ms();
        queue_message(
            &server,
            "recipient",
            &subscription_id,
            "reserved-once",
            now - 120_001,
        );

        assert!(!attempt_notification_with_at(
            &server,
            "reserved-once",
            &subscription_id,
            &|_| false,
            &|_, _| panic!("a failed reservation must not send"),
            &|_, _| Ok(true),
            now,
        ));
        assert_eq!(
            server.state.lock().unwrap().msgs["reserved-once"].state,
            "pending"
        );
        assert_eq!(
            server.state.lock().unwrap().msgs["reserved-once"].wake_attempt_count,
            1
        );
        assert!(!attempt_notification_with_at(
            &server,
            "reserved-once",
            &subscription_id,
            &|_| true,
            &|_, _| panic!("a reserved message must never be replayed"),
            &|_, _| Ok(true),
            now + 300_000,
        ));
        std::fs::remove_dir_all(root).unwrap();
    }
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
        &|_, _| Ok(true),
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
    if event != "deadline"
        && event != "master-idle"
        && (trigger_ms.is_some()
            || !trigger_times_ms.is_empty()
            || interval_ms.is_some()
            || repeat_count != 1)
    {
        return Resp::err("schedule options are valid only for deadline subscriptions");
    }
    if event == "deadline" && trigger_ms.is_some() && !trigger_times_ms.is_empty() {
        return Resp::err("use at-ms or trigger-ms, not both");
    }
    let goal_deadline = subject
        .as_deref()
        .is_some_and(|value| value.starts_with("goal:"));
    if event == "deadline"
        && goal_deadline
        && (interval_ms.is_some()
            || repeat_count != 1
            || trigger_times_ms.len() > 1
            || (trigger_ms.is_none() && trigger_times_ms.is_empty()))
    {
        return Resp::err("goal deadline subscriptions are one-shot and require one at-ms trigger");
    }
    let now = now_ms();
    let expires_ms = now.saturating_add((ttl_seconds as i64).saturating_mul(1000));
    let mut state = server.state.lock().unwrap();
    if let Err(error) = verify(&state, &worker_id, &token) {
        return error;
    }
    if matches!(event.as_str(), "deadline" | "master-idle") {
        let live_master = match live_master_id(server, &state) {
            Ok(master) => master,
            Err(error) => return Resp::err(error),
        };
        if live_master.as_deref() != Some(worker_id.as_str()) {
            return Resp::err(if event == "master-idle" {
                "master-idle subscription requires the live registered master"
            } else if live_master.is_some() {
                "master authority required for deadline subscriptions"
            } else {
                "no live master; deadline subscriptions require an approved live master"
            });
        }
    }
    if goal_deadline {
        let requested_key = trigger_ms
            .or_else(|| trigger_times_ms.first().copied())
            .and_then(|trigger| {
                subject
                    .clone()
                    .map(|subject| (worker_id.clone(), subject, trigger))
            });
        if let Some(existing) = state
            .notification_subscriptions
            .values()
            .filter(|subscription| {
                goal_deadline_key(subscription).as_ref() == requested_key.as_ref()
                    && matches!(subscription.status.as_str(), "armed" | "consumed")
                    && subscription.expires_ms > now
            })
            .min_by_key(|subscription| (subscription.created_ms, subscription.id.clone()))
            .cloned()
        {
            return Resp::data(json!({
                "subscription": existing,
                "one_shot": true,
                "max_repeat_count": crate::server::state::MAX_NOTIFICATION_REPEATS,
                "deduplicated": true,
            }));
        }
    }
    let Some(pane) = state.worker_pane(&worker_id) else {
        return Resp::err("notification subscription requires a registered tmux pane");
    };
    if runtime_for_pane(Some(&pane)).is_none() {
        return Resp::err("notification subscription method tmux is unavailable for this pane");
    }
    let active = state
        .notification_subscriptions
        .values()
        .filter(|s| s.worker_id == worker_id && s.status == "armed")
        .count();
    if active >= MAX_ACTIVE_SUBSCRIPTIONS_PER_WORKER {
        return Resp::err("maximum 3 active subscriptions per agent");
    }
    if event == "master-idle" {
        if trigger_ms.is_some() || !trigger_times_ms.is_empty() {
            return Resp::err("master-idle requires a recurring interval, not an absolute trigger");
        }
        if !matches!(interval_ms, Some(900_000 | 3_600_000)) {
            return Resp::err("master-idle interval must be exactly 900000 or 3600000 ms");
        }
        if repeat_count == 0 || repeat_count > crate::server::state::MAX_NOTIFICATION_REPEATS {
            return Resp::err("repeat_count must be between 1 and 100");
        }
    }
    if event == "deadline" {
        if interval_ms.is_some() && (!trigger_times_ms.is_empty() || trigger_ms.is_some()) {
            return Resp::err("periodic schedule cannot include an absolute time list");
        }
        if interval_ms.is_none() && trigger_times_ms.is_empty() && trigger_ms.is_none() {
            return Resp::err("deadline requires at-ms or every-ms");
        }
        if repeat_count == 0 || repeat_count > crate::server::state::MAX_NOTIFICATION_REPEATS {
            return Resp::err("repeat_count must be between 1 and 100");
        }
        if interval_ms.is_some_and(|ms| ms <= 0) {
            return Resp::err("every-ms must be positive");
        }
        if interval_ms.is_some()
            && trigger_times_ms.is_empty()
            && trigger_ms.is_none()
            && repeat_count == 1
        {}
        if !trigger_times_ms.is_empty() && (interval_ms.is_some() || repeat_count != 1) {
            return Resp::err(
                "absolute schedule uses at-ms values and repeat_count is their length",
            );
        }
        let times = if trigger_times_ms.is_empty() {
            trigger_ms.into_iter().collect()
        } else {
            trigger_times_ms.clone()
        };
        if times.len() > crate::server::state::MAX_NOTIFICATION_REPEATS as usize {
            return Resp::err("absolute schedule supports at most 100 times");
        }
        if times
            .iter()
            .any(|trigger| *trigger <= now || *trigger >= expires_ms)
        {
            return Resp::err("absolute trigger times must be in the future and before expiry");
        }
        if interval_ms.is_some_and(|ms| now.saturating_add(ms) >= expires_ms) {
            return Resp::err("every-ms must fire before subscription expiry");
        }
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
        status_reason: None,
    };
    server.commit_locked(
        &mut state,
        &[Event::NotificationSubscribed {
            subscription: subscription.clone(),
        }],
    );
    Resp::data(
        json!({"subscription": subscription, "one_shot": goal_deadline, "max_repeat_count": crate::server::state::MAX_NOTIFICATION_REPEATS}),
    )
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
    let mut events = vec![Event::NotificationStatus {
        subscription_id: subscription_id.clone(),
        status: "cancelled".into(),
        updated_ms: now_ms(),
    }];
    let pending = state
        .wake_bindings
        .iter()
        .filter_map(|(message_id, bound_subscription)| {
            (bound_subscription == &subscription_id
                && state
                    .msgs
                    .get(message_id)
                    .is_some_and(|message| message.state == "pending"))
            .then_some(message_id.clone())
        })
        .collect::<Vec<_>>();
    if !pending.is_empty() {
        events.push(Event::Superseded { ids: pending });
    }
    server.commit_locked(&mut state, &events);
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
            Some(pane) if pane.starts_with('%') => match (server.pane_alive_check)(pane) {
                PanePresence::Present => {}
                PanePresence::Missing => {
                    issues.push(format!("worker {} tmux pane is offline", worker.id))
                }
                PanePresence::Unknown => issues.push(format!(
                    "worker {} tmux pane liveness is unknown",
                    worker.id
                )),
            },
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

fn register_typed(
    server: &Server,
    worker_id: &str,
    token: &str,
    pane: Option<&str>,
    cwd: &str,
    project_scope: Option<ProjectScopeId>,
) -> Resp {
    let Some(pane) = pane else {
        return Resp::err("collab registration requires a live tmux pane");
    };
    let typed = match project_scope {
        Some(project_scope) => {
            server.typed_register_envelope_for_scope(worker_id, token, pane, project_scope, cwd)
        }
        None => server.typed_register_envelope(worker_id, token, pane, cwd),
    };
    match typed {
        Ok(typed) => match server.typed_dispatch(typed.clone()) {
            Ok(outcome) => {
                let Some(runtime) = runtime_for_pane(Some(pane)) else {
                    return Resp::err("collab registration requires a live tmux pane");
                };
                let (role_brief, registered_at) = {
                    let st = server.state.lock().unwrap();
                    let registered_at = match &typed.command {
                        TypedCommand::RegisterWorker { worker, .. } => worker.registered_ms,
                    };
                    (role_brief(&st, worker_id), registered_at)
                };
                Resp::data(json!({
                    "worker_id": worker_id,
                    "identity_kind": "peer",
                    "runtime": runtime,
                    "registered_at": iso(registered_at),
                    "role_brief": role_brief,
                    "typed": true,
                    "command_id": outcome.receipt.command_id.as_str(),
                    "operation_id": outcome.receipt.operation_id.as_str(),
                    "sequence": outcome.receipt.sequence,
                    "revision": outcome.receipt.revision,
                    "replayed": outcome.replayed,
                    "command": typed.command,
                }))
            }
            Err(error) => Resp::err(format!("typed registrar rejected registration: {error}")),
        },
        Err(error) => Resp::err(format!("typed registrar failed to build command: {error}")),
    }
}

pub(crate) fn handle_register(
    server: &Server,
    worker_id: String,
    token: String,
    pane: Option<String>,
    cwd: String,
) -> Resp {
    let st = server.state.lock().unwrap();
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
        let existing_project_scope = match existing_project_scope(&st, &worker_id) {
            Ok(scope) => scope,
            Err(error) => return error,
        };
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
                drop(st);
                let mut resp = register_typed(
                    server,
                    &worker_id,
                    &token,
                    pane.as_deref(),
                    &cwd,
                    existing_project_scope,
                );
                if resp.ok {
                    resp.data["recovered"] = json!(true);
                    resp.data["identity_source"] = json!("tmux_session");
                }
                return resp;
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
        let refreshed_pane = pane.clone().or_else(|| existing.pane.clone());
        drop(st);
        let mut resp = register_typed(
            server,
            &worker_id,
            &token,
            refreshed_pane.as_deref(),
            &cwd,
            existing_project_scope,
        );
        if resp.ok {
            resp.data["reused"] = json!(true);
        }
        return resp;
    }
    drop(st);
    register_typed(server, &worker_id, &token, pane.as_deref(), &cwd, None)
}

fn existing_project_scope(state: &State, worker_id: &str) -> Result<Option<ProjectScopeId>, Resp> {
    let mut found = None;
    for project in state.global.projects.values() {
        for binding in project.runtime_bindings.values() {
            if binding.agent_id.as_str() != worker_id {
                continue;
            }
            if found
                .as_ref()
                .is_some_and(|scope| scope != &binding.project_scope)
            {
                return Err(Resp::err(format!(
                    "worker {} has ambiguous registered project scope",
                    worker_id
                )));
            }
            found = Some(binding.project_scope.clone());
        }
    }
    Ok(found)
}

fn role_brief(state: &State, worker_id: &str) -> serde_json::Value {
    if state.master_worker_id.as_deref() == Some(worker_id) {
        return json!({
            "role": "master",
            "role_task": "Orchestrate the project; implementation is not your primary job.",
            "responsibilities": [
                "Run `appsdk longhorizon show` to reconstruct goal, tasks, workers, blockers, and bugs.",
                "Split work into independent scopes; assign tasks and resources; keep useful worker capacity loaded.",
                "Own worker blockers: investigate, unblock, reassign, or close. Do not wait for someone else.",
                "Drive test, verification, commit, merge, worktree cleanup, and task closure.",
                "Continue under the standing goal without waiting for user input; hold wakes only for a true external approval or dependency gate."
            ],
            "notification_rule": "A notification is an interrupt, not completion. Do its P0/P1/P2 action, then resume scheduling; never stop on ACK/read/summary."
        });
    }
    if is_managed_subagent(state, worker_id) {
        return json!({
            "role": "managed-subagent",
            "role_task": "Execute the assigned independent task and return evidence to parent/master.",
            "responsibilities": [
                "Stay inside the assigned task, worktree, file scope, delivery conditions, and tests.",
                "Accept and execute master/parent instructions for this assignment; do not create a global schedule.",
                "On trouble, investigate first. Send root cause, attempted actions, proposed fix, and any required decision to the live master; copy parent when different.",
                "Complete implementation, tests, commit, delivery evidence, and resource cleanup; do not stop at code-written or ACK."
            ],
            "notification_rule": "Handle the named priority action, then resume your assigned task. Reading or ACK is never task progress."
        });
    }
    json!({
        "role": "worker",
        "role_task": "Own and complete your independent task; collaborate with the master without abandoning existing ownership.",
        "responsibilities": [
            "Execute your registered task end to end within its worktree and file scope: implement, test, commit, deliver evidence, and close resources.",
            "Evaluate master collaboration requests against current ownership and capacity. Accept ready non-conflicting work; decline or negotiate conflicts explicitly instead of silently ignoring them.",
            "On trouble, investigate first. Report root cause, attempted actions, proposed fix, and the exact decision needed to the live master.",
            "Do not wait passively and do not stop on ACK/read/summary; after handling a notification, resume your current task."
        ],
        "notification_rule": "P0 preempts P1, P1 preempts P2. Higher priority interrupts but does not cancel your owned task."
    })
}

fn worker_identity_presence(server: &Server, worker: &WorkerRec) -> PanePresence {
    let Some(pane) = worker.pane.as_deref() else {
        return PanePresence::Missing;
    };
    match (server.pane_alive_check)(pane) {
        PanePresence::Present => match (server.pane_owner_check)(&worker.id, pane) {
            Ok(true) => PanePresence::Present,
            Ok(false) => PanePresence::Missing,
            Err(()) => PanePresence::Unknown,
        },
        presence => presence,
    }
}

/// Decide whether a new managed child would starve an already registered peer.
/// The caller must use the returned peer for the scope before creating a child.
pub(crate) fn registered_idle_peer_for_admission(
    server: &Server,
    requester: &str,
) -> Option<(String, String)> {
    // Snapshot only state-owned data while holding the mutex. Pane probes may
    // invoke tmux and must never run while the scheduler state is locked.
    let candidates: Vec<WorkerRec> = {
        let state = server.state.lock().unwrap();
        let mut workers: Vec<_> = state
            .workers
            .values()
            .filter(|worker| worker.id != requester)
            .filter(|worker| !is_managed_subagent(&state, &worker.id))
            .filter(|worker| {
                !state
                    .tasks
                    .values()
                    .any(|task| task.owner == worker.id && task_resource_active(&task.status))
            })
            .cloned()
            .collect();
        workers.sort_by(|a, b| a.id.cmp(&b.id));
        workers
    };

    let probed: Vec<WorkerRec> = candidates
        .into_iter()
        .filter(|worker| {
            matches!(
                worker_identity_presence(server, worker),
                PanePresence::Present
            ) && worker
                .pane
                .as_deref()
                .map(|pane| (server.pane_state_check)(pane) == knock::AgentState::Waiting)
                == Some(true)
        })
        .collect();

    let state = server.state.lock().unwrap();
    for worker in probed {
        let Some(current) = state.workers.get(&worker.id) else {
            continue;
        };
        let unchanged = current.id == worker.id
            && current.token == worker.token
            && current.pane == worker.pane
            && current.cwd == worker.cwd
            && current.registered_ms == worker.registered_ms;
        if unchanged
            && !is_managed_subagent(&state, &worker.id)
            && !state
                .tasks
                .values()
                .any(|task| task.owner == worker.id && task_resource_active(&task.status))
        {
            return Some((
                worker.id,
                "live registered peer is idle, owned, and has no actionable task".into(),
            ));
        }
    }
    None
}

pub(crate) fn idle_managed_subagent_for_admission(
    server: &Server,
    requester: &str,
) -> Option<(String, String, String)> {
    let candidates: Vec<(crate::subagent::Record, WorkerRec)> = {
        let state = server.state.lock().unwrap();
        let mut candidates = state
            .subagents
            .values()
            .filter(|record| record.parent == requester && record.status == "idle")
            .filter_map(|record| {
                state
                    .workers
                    .get(&record.peer)
                    .cloned()
                    .map(|worker| (record.clone(), worker))
            })
            .filter(|(_, worker)| {
                !state
                    .tasks
                    .values()
                    .any(|task| task.owner == worker.id && task_resource_active(&task.status))
            })
            .collect::<Vec<_>>();
        candidates.sort_by(|left, right| left.0.id.cmp(&right.0.id));
        candidates
    };

    let probed: Vec<(crate::subagent::Record, WorkerRec)> = candidates
        .into_iter()
        .filter(|(_, worker)| {
            matches!(
                worker_identity_presence(server, worker),
                PanePresence::Present
            ) && worker
                .pane
                .as_deref()
                .map(|pane| (server.pane_state_check)(pane) == knock::AgentState::Waiting)
                == Some(true)
        })
        .collect();

    let state = server.state.lock().unwrap();
    for (record, worker) in probed {
        let Some(current) = state.subagents.get(&record.id) else {
            continue;
        };
        let Some(current_worker) = state.workers.get(&worker.id) else {
            continue;
        };
        if current.parent == requester
            && current.status == "idle"
            && current.peer == worker.id
            && current.pane == worker.pane
            && current_worker.id == worker.id
            && current_worker.token == worker.token
            && current_worker.pane == worker.pane
            && current_worker.cwd == worker.cwd
            && current_worker.registered_ms == worker.registered_ms
            && !state
                .tasks
                .values()
                .any(|task| task.owner == worker.id && task_resource_active(&task.status))
        {
            return Some((
                record.id,
                worker.id,
                "live managed subagent is idle, owned, and has no active task".into(),
            ));
        }
    }
    None
}

pub(crate) fn live_master_id(
    server: &Server,
    state: &State,
) -> Result<Option<String>, &'static str> {
    let Some(worker) = state
        .master_worker_id
        .as_ref()
        .and_then(|id| state.workers.get(id))
    else {
        return Ok(None);
    };
    match worker_identity_presence(server, worker) {
        PanePresence::Present => Ok(Some(worker.id.clone())),
        PanePresence::Missing => Ok(None),
        PanePresence::Unknown => {
            Err("master identity is unknown; defer authority changes until pane probes succeed")
        }
    }
}

fn is_managed_subagent(state: &State, worker_id: &str) -> bool {
    state
        .subagents
        .values()
        .any(|record| record.peer == worker_id)
}

/// Admit a Start request to an already registered idle peer when the caller is
/// the live master. This is shared by the daemon dispatch path and the direct
/// subagent handler so neither entry point can bypass scheduler admission.
pub(crate) fn scheduler_admit_subagent_start(
    server: &Server,
    worker_id: &str,
    token: &str,
    requested_id: Option<&str>,
    requested_runtime: Option<&str>,
) -> Result<Option<Resp>, Resp> {
    if requested_id.is_some_and(|id| !crate::subagent::valid_id(id))
        || requested_runtime.is_some_and(|runtime| !crate::subagent::valid_runtime(runtime))
    {
        return Ok(None);
    }
    let authenticated = {
        let state = server.state.lock().unwrap();
        verify(&state, worker_id, token).is_ok()
    };
    if !authenticated {
        return Ok(None);
    }

    if requested_id.is_some_and(|id| server.state.lock().unwrap().subagents.contains_key(id)) {
        return Ok(None);
    }

    // Snapshot the master identity, then probe outside the state mutex for the
    // same reason as registered_idle_peer_for_admission.
    let Some(master) = ({
        let state = server.state.lock().unwrap();
        state
            .master_worker_id
            .as_ref()
            .and_then(|id| state.workers.get(id))
            .cloned()
    }) else {
        return Ok(None);
    };
    if master.id != worker_id || worker_identity_presence(server, &master) != PanePresence::Present
    {
        return Ok(None);
    }

    let (decision, peer_id, managed_subagent_id, reason) =
        if let Some((peer_id, reason)) = registered_idle_peer_for_admission(server, worker_id) {
            ("use-registered-peer", peer_id, None, reason)
        } else if let Some((id, peer_id, reason)) =
            idle_managed_subagent_for_admission(server, worker_id)
        {
            ("reuse-idle-managed-subagent", peer_id, Some(id), reason)
        } else {
            let admission = json!({
                "decision": "create-managed-subagent",
                "managed_subagent_id": serde_json::Value::Null,
                "reason": "no eligible live registered peer or idle managed subagent capacity",
            });
            record_scheduler_admission(server, admission)?;
            return Ok(None);
        };
    let managed_subagent = managed_subagent_id
        .as_ref()
        .map(|id| json!({"id": id, "worker_id": peer_id}))
        .unwrap_or(serde_json::Value::Null);
    let admission = json!({
        "decision": decision,
        "worker_id": peer_id,
        "managed_subagent_id": managed_subagent_id,
        "reason": reason,
    });
    record_scheduler_admission(server, admission.clone())?;
    Ok(Some(Resp::data(json!({
        "admission": admission,
        "managed_subagent": managed_subagent,
    }))))
}

fn record_scheduler_admission(server: &Server, admission: serde_json::Value) -> Result<(), Resp> {
    ensure_scheduler_admission_audit(server, &admission).map(|_| ())
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum SchedulerAdmissionAuditState {
    Recorded,
    Failed(String),
}

fn scheduler_admission_audit_state(
    server: &Server,
    request_id: &str,
) -> Result<Option<SchedulerAdmissionAuditState>, Resp> {
    let path = server.root.join(".agent-collab/server/events.jsonl");
    let content = match std::fs::read_to_string(path) {
        Ok(content) => content,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(error) => {
            return Err(Resp::err(format!(
                "scheduler admission audit failed: lookup {error}"
            )))
        }
    };
    for line in content.lines() {
        let Ok(record) = serde_json::from_str::<serde_json::Value>(line) else {
            continue;
        };
        if record.get("kind").and_then(serde_json::Value::as_str) != Some("scheduler_admission")
            || record
                .get("detail")
                .and_then(|detail| detail.get("request_id"))
                .and_then(serde_json::Value::as_str)
                != Some(request_id)
        {
            continue;
        }
        let detail = record.get("detail").cloned().unwrap_or_else(|| json!({}));
        if detail.get("status").and_then(serde_json::Value::as_str) == Some("failed") {
            let error = detail
                .get("error")
                .and_then(serde_json::Value::as_str)
                .unwrap_or("scheduler admission audit failed")
                .to_string();
            return Ok(Some(SchedulerAdmissionAuditState::Failed(error)));
        }
        return Ok(Some(SchedulerAdmissionAuditState::Recorded));
    }
    Ok(None)
}

fn ensure_scheduler_admission_audit(
    server: &Server,
    admission: &serde_json::Value,
) -> Result<SchedulerAdmissionAuditState, Resp> {
    if let Some(request_id) = admission
        .get("request_id")
        .and_then(serde_json::Value::as_str)
    {
        if let Some(state) = scheduler_admission_audit_state(server, request_id)? {
            return Ok(state);
        }
    }
    if let Err(error) = record_activity(&server.root, "scheduler_admission", admission.clone()) {
        append_log(
            &server.log_path(),
            &format!("SCHEDULER_ADMISSION_RECORD_FAILED: {error}"),
        );
        return Err(Resp::err(format!(
            "scheduler admission audit failed: {error}"
        )));
    }
    Ok(SchedulerAdmissionAuditState::Recorded)
}

fn scheduler_admission_audit_error(
    result: Result<SchedulerAdmissionAuditState, Resp>,
) -> Option<String> {
    match result {
        Ok(SchedulerAdmissionAuditState::Recorded) => None,
        Ok(SchedulerAdmissionAuditState::Failed(error)) => Some(error),
        Err(error) => Some(
            error
                .error
                .unwrap_or_else(|| "scheduler admission audit failed".into()),
        ),
    }
}

fn scheduler_admission_failed_response(
    admission: &crate::server::state::SchedulerAdmissionRecord,
) -> Resp {
    let error = admission.error.clone().unwrap_or_else(|| {
        "scheduler admission audit failed; retry with the same request_id".into()
    });
    Resp::err_data(
        error,
        json!({
            "request_id": admission.request_id,
            "reservation": true,
            "decision": "audit-failed",
            "message_id": admission.message_id,
            "task_id": admission.task_id,
            "admission": admission,
        }),
    )
}

fn scheduler_dispatch_recover_pending(server: &Server, request_id: &str) -> Option<Resp> {
    let (admission, task, subscription) = {
        let mut state = server.state.lock().unwrap();
        let Some(pending) = state
            .scheduler_admissions
            .get(request_id)
            .filter(|admission| admission.status == "pending")
            .cloned()
        else {
            return None;
        };
        let Some(task) = state.tasks.get(&pending.task_id).cloned() else {
            return None;
        };
        let audit = json!({
            "request_id": pending.request_id,
            "decision": pending.decision,
            "worker_id": pending.worker_id,
            "managed_subagent_id": pending.managed_subagent_id,
            "message_id": pending.message_id,
            "task_id": pending.task_id,
            "status": "pending",
            "recovery": true,
        });
        if let Some(error) =
            scheduler_admission_audit_error(ensure_scheduler_admission_audit(server, &audit))
        {
            server.commit_locked(
                &mut state,
                &[Event::SchedulerAdmissionStatus {
                    request_id: request_id.into(),
                    status: "failed".into(),
                    error: Some(error.clone()),
                    updated_ms: now_ms(),
                }],
            );
            let mut failed = pending;
            failed.status = "failed".into();
            failed.error = Some(error);
            return Some(scheduler_admission_failed_response(&failed));
        }
        server.commit_locked(
            &mut state,
            &[Event::SchedulerAdmissionStatus {
                request_id: request_id.into(),
                status: "succeeded".into(),
                error: None,
                updated_ms: now_ms(),
            }],
        );
        let admission = state.scheduler_admissions.get(request_id).cloned()?;
        let subscription = state
            .matching_subscription(&admission.worker_id, "direct-message", None, now_ms())
            .cloned();
        (admission, task, subscription)
    };
    let notified = subscription.as_ref().is_some_and(|subscription| {
        attempt_notification(server, &admission.message_id, &subscription.id)
    });
    Some(Resp::data(json!({
        "request_id": admission.request_id,
        "decision": admission.decision,
        "admission": admission,
        "message_id": admission.message_id,
        "task_id": admission.task_id,
        "target": task.owner,
        "status": task.status,
        "managed_subagent_id": admission.managed_subagent_id,
        "managed_subagent": admission
            .managed_subagent_id
            .as_ref()
            .map(|id| json!({"id": id, "worker_id": task.owner}))
            .unwrap_or(serde_json::Value::Null),
        "notification": if subscription.is_none() {
            "mailbox-only-no-subscription"
        } else if notified {
            "sent"
        } else {
            "subscribed-not-sent"
        },
        "recovered": true,
    })))
}

fn scheduler_dispatch_deduplicated(
    server: &Server,
    worker_id: &str,
    request_id: &str,
) -> Option<Resp> {
    let message_id = format!("scheduler-{request_id}");
    let task_id = format!("task-{message_id}");
    let state = server.state.lock().unwrap();
    if let Some(admission) = state.scheduler_admissions.get(request_id) {
        if admission.status == "failed" {
            return Some(scheduler_admission_failed_response(admission));
        }
        if admission.status == "pending" {
            return None;
        }
    }
    let (Some(message), Some(task)) = (
        state.msgs.get(&message_id).cloned(),
        state.tasks.get(&task_id).cloned(),
    ) else {
        return None;
    };
    let managed_subagent_id = state
        .subagents
        .values()
        .find(|record| record.parent == worker_id && record.peer == task.owner)
        .map(|record| record.id.clone());
    Some(Resp::data(json!({
        "request_id": request_id,
        "decision": "deduplicated",
        "message_id": message.id,
        "task_id": task.id,
        "target": task.owner,
        "status": task.status,
        "managed_subagent_id": managed_subagent_id,
        "managed_subagent": managed_subagent_id
            .as_ref()
            .map(|id| json!({"id": id, "worker_id": task.owner}))
            .unwrap_or(serde_json::Value::Null),
        "deduplicated": true,
    })))
}

fn scheduler_assignment_events(
    message: Message,
    task: TaskRec,
    managed_child: Option<crate::subagent::Record>,
) -> Vec<Event> {
    let message_id = message.id.clone();
    let mut events = vec![Event::Sent { msg: message }];
    events.push(Event::TaskCreated { task });
    if let Some(mut child) = managed_child {
        child.status = "assigned".into();
        child.last_message = Some(message_id);
        events.push(Event::SubagentUpdated { subagent: child });
    }
    events
}

pub(crate) fn handle_scheduler_dispatch(
    server: &Server,
    worker_id: String,
    token: String,
    request_id: String,
    subject: String,
    body: String,
    feature_id: Option<String>,
    mut worktree_path: Option<String>,
    branch: Option<String>,
    base_commit: Option<String>,
    priority: String,
    next_step: Option<String>,
) -> Resp {
    if !crate::subagent::valid_id(&request_id) {
        return Resp::err("scheduler request_id must be a valid non-empty ID");
    }
    if subject.trim().is_empty() || body.trim().is_empty() {
        return Resp::err("scheduler dispatch requires a non-empty subject and body");
    }
    if !matches!(priority.as_str(), "p0" | "p1" | "p2" | "p3" | "p4") {
        return Resp::err(format!(
            "invalid priority {}; must be p0, p1, p2, p3, or p4",
            priority
        ));
    }
    if let Some(path) = &worktree_path {
        let canonical = match validate_worktree_path(&server.root, path) {
            Ok(path) => path,
            Err(error) => return Resp::err(error),
        };
        worktree_path = Some(canonical.display().to_string());
    }

    let authenticated = {
        let state = server.state.lock().unwrap();
        verify(&state, &worker_id, &token).is_ok()
    };
    if !authenticated {
        return Resp::err("scheduler dispatch authentication failed");
    }
    let Some(master) = ({
        let state = server.state.lock().unwrap();
        state
            .master_worker_id
            .as_ref()
            .and_then(|id| state.workers.get(id))
            .cloned()
    }) else {
        return Resp::err("scheduler dispatch requires a live master");
    };
    if master.id != worker_id || worker_identity_presence(server, &master) != PanePresence::Present
    {
        return Resp::err("scheduler dispatch requires the live registered master");
    }

    let message_id = format!("scheduler-{request_id}");
    let task_id = format!("task-{message_id}");
    for _ in 0..3 {
        if let Some(response) = scheduler_dispatch_recover_pending(server, &request_id) {
            return response;
        }
        if let Some(response) = scheduler_dispatch_deduplicated(server, &worker_id, &request_id) {
            return response;
        }
        let candidate = registered_idle_peer_for_admission(server, &worker_id)
            .map(|(peer, reason)| (peer, None, reason, "use-registered-peer"))
            .or_else(|| {
                idle_managed_subagent_for_admission(server, &worker_id).map(
                    |(managed_id, peer, reason)| {
                        (
                            peer,
                            Some(managed_id),
                            reason,
                            "reuse-idle-managed-subagent",
                        )
                    },
                )
            });
        let Some((peer_id, managed_id, reason, decision)) = candidate else {
            if let Some(response) = scheduler_dispatch_recover_pending(server, &request_id) {
                return response;
            }
            if let Some(response) = scheduler_dispatch_deduplicated(server, &worker_id, &request_id)
            {
                return response;
            }
            return Resp::err(
                "scheduler dispatch has no eligible live peer or idle managed subagent capacity",
            );
        };

        let mut state = server.state.lock().unwrap();
        let Some(worker) = state.workers.get(&peer_id).cloned() else {
            continue;
        };
        if state
            .tasks
            .values()
            .any(|task| task.owner == peer_id && task_resource_active(&task.status))
        {
            continue;
        }
        let mut managed_child = None;
        if let Some(managed_id) = &managed_id {
            let Some(child) = state.subagents.get(managed_id).cloned() else {
                continue;
            };
            if child.parent != worker_id
                || child.peer != peer_id
                || child.status != "idle"
                || child.pane != worker.pane
            {
                continue;
            }
            managed_child = Some(child);
        } else if is_managed_subagent(&state, &peer_id) {
            continue;
        }
        if state.msgs.contains_key(&message_id) || state.tasks.contains_key(&task_id) {
            continue;
        }

        let now = now_ms();
        let message = Message {
            id: message_id.clone(),
            from: worker_id.clone(),
            to: peer_id.clone(),
            mtype: "notify".into(),
            subject: Some(subject.clone()),
            body: body.clone(),
            in_reply_to: None,
            created_ms: now,
            state: "pending".into(),
            wake_attempt_count: 0,
            last_wake_attempt_ms: 0,
        };
        let task = TaskRec {
            id: task_id.clone(),
            owner: peer_id.clone(),
            created_by: worker_id.clone(),
            feature_id: feature_id.clone(),
            worktree_path: worktree_path.clone(),
            branch: branch.clone(),
            base_commit: base_commit.clone(),
            priority: priority.clone(),
            status: "assigned".into(),
            next_step: next_step.clone().or_else(|| {
                Some(format!(
                    "Read scheduler message {message_id}; mark task working before execution"
                ))
            }),
            wait: None,
            created_ms: now,
            updated_ms: now,
        };
        let admission_record = crate::server::state::SchedulerAdmissionRecord {
            request_id: request_id.clone(),
            decision: decision.into(),
            worker_id: peer_id.clone(),
            managed_subagent_id: managed_id.clone(),
            message_id: message_id.clone(),
            task_id: task_id.clone(),
            status: "pending".into(),
            error: None,
            created_ms: now,
            updated_ms: now,
        };
        let mut admission = json!({
            "request_id": request_id,
            "decision": decision,
            "worker_id": peer_id,
            "managed_subagent_id": managed_id,
            "message_id": message_id,
            "task_id": task_id,
            "reason": reason,
            "status": "pending",
        });
        let binding = worktree_binding_for_task(server, &task);
        let mut events = scheduler_assignment_events(message, task, managed_child);
        if let Some(binding) = binding {
            events.push(Event::WorktreeBound { binding });
        }
        let subscription = state
            .matching_subscription(&peer_id, "direct-message", None, now)
            .cloned();
        events.push(Event::DeliveryMode {
            msg_id: message_id.clone(),
            mode: "explicit-notification".into(),
        });
        if let Some(subscription) = &subscription {
            events.push(Event::WakeBound {
                message_id: message_id.clone(),
                subscription_id: subscription.id.clone(),
            });
        }
        events.push(Event::SchedulerAdmission {
            admission: admission_record,
        });
        server.commit_locked(&mut state, &events);
        if let Some(error) =
            scheduler_admission_audit_error(ensure_scheduler_admission_audit(server, &admission))
        {
            server.commit_locked(
                &mut state,
                &[Event::SchedulerAdmissionStatus {
                    request_id: request_id.clone(),
                    status: "failed".into(),
                    error: Some(error.clone()),
                    updated_ms: now_ms(),
                }],
            );
            drop(state);
            admission["status"] = json!("failed");
            admission["error"] = json!(error.clone());
            return Resp::err_data(
                error,
                json!({
                    "request_id": request_id,
                    "reservation": true,
                    "message_id": message_id,
                    "task_id": task_id,
                    "admission": admission,
                }),
            );
        }
        server.commit_locked(
            &mut state,
            &[Event::SchedulerAdmissionStatus {
                request_id: request_id.clone(),
                status: "succeeded".into(),
                error: None,
                updated_ms: now_ms(),
            }],
        );
        drop(state);
        let notified = subscription.as_ref().is_some_and(|subscription| {
            attempt_notification(server, &message_id, &subscription.id)
        });
        admission["status"] = json!("succeeded");
        return Resp::data(json!({
            "request_id": request_id,
            "decision": decision,
            "admission": admission,
            "message_id": message_id,
            "task_id": task_id,
            "target": peer_id,
            "status": "assigned",
            "managed_subagent_id": managed_id,
            "managed_subagent": managed_id
                .as_ref()
                .map(|id| json!({"id": id, "worker_id": peer_id}))
                .unwrap_or(serde_json::Value::Null),
            "notification": if subscription.is_none() {
                "mailbox-only-no-subscription"
            } else if notified {
                "sent"
            } else {
                "subscribed-not-sent"
            },
        }));
    }
    Resp::err(
        "scheduler dispatch capacity changed during admission; retry with the same request_id",
    )
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
        Ok(Some(master)) if master == worker_id => Ok(()),
        Ok(Some(_)) => Err(Resp::err(
            "master authority required; ask the registered master to delegate",
        )),
        Ok(None) => Err(Resp::err(
            "no live master; a peer may promote itself only with explicit user approval",
        )),
        Err(error) => Err(Resp::err(error)),
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
    match live_master_id(server, &state) {
        Ok(Some(_)) => {
            return Resp::err("master already exists; only the registered master may delegate")
        }
        Err(error) => return Resp::err(error),
        Ok(None) => {}
    }
    match worker_identity_presence(server, &worker) {
        PanePresence::Present => {}
        PanePresence::Missing => return Resp::err("master promotion requires a live tmux pane"),
        PanePresence::Unknown => return Resp::err(
            "promotion candidate identity is unknown; defer promotion until pane probes succeed",
        ),
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
    Resp::data(json!({
        "master": worker_id,
        "mode": "user_approved_self_promotion",
        "role_brief": role_brief(&state, &worker_id)
    }))
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
    match worker_identity_presence(server, target) {
        PanePresence::Present => {}
        PanePresence::Missing => {
            return Resp::err("master delegation requires a live target tmux pane")
        }
        PanePresence::Unknown => {
            return Resp::err(
                "delegation target identity is unknown; defer delegation until pane probes succeed",
            )
        }
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
    Resp::data(json!({
        "master": target_id,
        "delegated_by": worker_id,
        "role_brief": role_brief(&state, &target_id)
    }))
}

fn handle_worker_close(
    server: &Server,
    worker_id: String,
    token: String,
    target_id: String,
    reason: String,
    kill_session: bool,
) -> Resp {
    let mut state = server.state.lock().unwrap();
    if let Err(error) = verify_master_actor(server, &state, &worker_id, &token) {
        return error;
    }
    if reason.trim().is_empty() {
        return Resp::err("worker close requires a non-empty --reason");
    }
    if target_id == worker_id {
        return Resp::err("master cannot close itself; delegate first");
    }
    let Some(target) = state.workers.get(&target_id).cloned() else {
        return Resp::err(format!("target worker {} not registered", target_id));
    };

    // Closing a worker that still owns live work would strand the task and its
    // worktree. The task lifecycle must be resolved first.
    let owned: Vec<String> = state
        .tasks
        .values()
        .filter(|task| task.owner == target_id && keepalive::actionable(&task.status))
        .map(|task| task.id.clone())
        .collect();
    if !owned.is_empty() {
        return Resp::err(format!(
            "worker {} still owns {}; close or force-close the task first",
            target_id,
            owned.join(", ")
        ));
    }

    let mut killed_session = false;
    if kill_session {
        if let Some(pane) = target.pane.as_deref() {
            if (server.pane_alive_check)(pane) == PanePresence::Present {
                let output = std::process::Command::new("tmux")
                    .args(["display-message", "-p", "-t", pane, "#{session_id}"])
                    .output();
                match output {
                    Ok(output) if output.status.success() => {
                        let session = String::from_utf8_lossy(&output.stdout).trim().to_string();
                        if session.is_empty() {
                            return Resp::err("could not resolve the target tmux session");
                        }
                        // Kill only the resolved session, never a name guess.
                        match std::process::Command::new("tmux")
                            .args(["kill-session", "-t", &session])
                            .status()
                        {
                            Ok(status) if status.success() => killed_session = true,
                            _ => return Resp::err(format!("tmux kill-session {} failed", session)),
                        }
                    }
                    _ => return Resp::err("could not resolve the target tmux session"),
                }
            }
        }
    }

    let now = now_ms();
    server.commit_locked(
        &mut state,
        &[Event::WorkerClosed {
            worker_id: target_id.clone(),
            closed_by: worker_id.clone(),
            reason: reason.clone(),
            killed_session,
            at_ms: now,
        }],
    );
    Resp::data(json!({
        "closed": target_id,
        "closed_by": worker_id,
        "reason": reason,
        "killed_session": killed_session,
        "pane": target.pane,
    }))
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
        "master_wake": state.master_wake,
    })
}

fn handle_master_status(server: &Server) -> Resp {
    let state = server.state.lock().unwrap();
    let live = match live_master_id(server, &state) {
        Ok(master) => master,
        Err(error) => {
            return Resp::err_data(
                error,
                json!({"status": "unknown", "recorded_worker_id": state.master_worker_id}),
            )
        }
    };
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
        None,
    )
}

fn handle_authenticated_send(
    server: &Server,
    raw_from: String,
    worker_id: String,
    token: String,
    command: Option<crate::proto::CommandEnvelope>,
    to: String,
    mtype: String,
    subject: Option<String>,
    body: String,
    in_reply_to: Option<String>,
    delivery_mode: String,
) -> Resp {
    let st = server.state.lock().unwrap();
    let Some(worker) = st.workers.get(&worker_id).cloned() else {
        return Resp::err(format!("worker {} not registered", worker_id));
    };
    if worker.token != token {
        return Resp::err("token mismatch: identity does not own this worker_id");
    }
    let Some(command) = command else {
        return Resp::err(
            "LEGACY_SEND_REJECTED: authenticated sender binding and route scope are required",
        );
    };
    let appserver_id = match crate::identity::AppServerId::new("appserver-cli") {
        Ok(id) => id,
        Err(error) => return Resp::err(error.to_string()),
    };
    let agent_id = match crate::identity::AgentId::new(&worker.id) {
        Ok(agent_id) => agent_id,
        Err(error) => return Resp::err(error.to_string()),
    };
    let runtime_id = match crate::identity::RuntimeId::new(format!("runtime-{}", worker.id)) {
        Ok(runtime_id) => runtime_id,
        Err(error) => return Resp::err(error.to_string()),
    };
    let binding_id = match crate::identity::BindingId::new(format!("binding-{}", worker.id)) {
        Ok(binding_id) => binding_id,
        Err(error) => return Resp::err(error.to_string()),
    };
    let runtime = crate::identity::RuntimeIdentity {
        agent_id,
        runtime_id,
        appserver_id: appserver_id.clone(),
        endpoint_generation: 0,
        binding_id,
        native_thread_id: None,
    };
    let registered_scope = match crate::scope::RouteScope::for_registered_project(
        appserver_id,
        Path::new(&worker.cwd),
    ) {
        Ok(scope) => scope,
        Err(error) => return Resp::err(error.to_string()),
    };
    if let Err(error) = command.validate_for(&runtime, &registered_scope) {
        return Resp::err(format!("SEND_BINDING_REJECTED: {error}"));
    }
    if raw_from != worker_id {
        return Resp::err("sender identity is derived from the authenticated binding");
    }
    drop(st);
    handle_send_with_task(
        server,
        worker_id,
        to,
        mtype,
        subject,
        body,
        in_reply_to,
        delivery_mode,
        false,
        None,
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
    managed_subagent_id: Option<&str>,
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
    let managed_child = if assign_task {
        let Some(id) = managed_subagent_id else {
            return Resp::err("managed task requires an explicit subagent binding");
        };
        let Some(child) = st.subagents.get(id).cloned() else {
            return Resp::err(format!("unknown managed subagent {}", id));
        };
        if child.parent != from || child.peer != to {
            return Resp::err("managed subagent owner mismatch");
        }
        if child.status != "idle" {
            return Resp::err("subagent is not idle; query status instead of resending");
        }
        if st
            .tasks
            .values()
            .any(|task| task.owner == child.peer && task_resource_active(&task.status))
        {
            return Resp::err("managed subagent already has an active task");
        }
        Some(child)
    } else {
        if managed_subagent_id.is_some() {
            return Resp::err("unassigned peer message cannot bind a managed subagent");
        }
        None
    };
    if from.trim().is_empty() {
        return Resp::err("sender cannot be empty");
    }
    let Some(recipient) = st.workers.get(&to) else {
        return Resp::err(format!("recipient {} not registered", to));
    };
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
        let managed_duplicate = assign_task
            && managed_child.as_ref().is_some_and(|child| {
                child.last_message.as_deref() == Some(existing.id.as_str())
                    && st
                        .tasks
                        .get(&format!("task-{}", existing.id))
                        .is_some_and(|task| task.owner == child.peer && task.created_by == from)
            });
        if !assign_task || managed_duplicate {
            return Resp::data(json!({"msg_id": existing.id, "deduplicated": true}));
        }
    }
    let mid = msg.id.clone();
    let task_id = assign_task.then(|| format!("task-{mid}"));
    let subscription = st
        .matching_subscription(&to, "direct-message", None, now_ms())
        .cloned();
    let mut events = if let Some(task_id) = &task_id {
        let task = TaskRec {
            id: task_id.clone(),
            owner: to.clone(),
            created_by: from.clone(),
            feature_id: None,
            worktree_path: None,
            branch: None,
            base_commit: None,
            priority: "p2".into(),
            status: "assigned".into(),
            next_step: Some(format!(
                "Read collab msg {mid}; accept via subagent working; bind a worktree with task relocate before code edits."
            )),
            wait: None,
            created_ms: now_ms(),
            updated_ms: now_ms(),
        };
        scheduler_assignment_events(
            msg,
            task,
            Some(managed_child.expect("managed child validated above")),
        )
    } else {
        vec![Event::Sent { msg }]
    };
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
    if let Err(error) = server.commit_locked_checked(&mut st, &events) {
        return Resp::err(format!("SEND_DURABILITY_FAILED: {error}"));
    }
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
        return Resp::err(
            "cross-project send requires source project and master assignment evidence",
        );
    }
    if source_master_assigned_ms <= 0 {
        return Resp::err("cross-project send requires source master assignment timestamp");
    }
    if source_master_approval
        .as_deref()
        .is_none_or(|v| v.trim().is_empty())
        && source_master_assigned_by == from
    {
        return Resp::err(
            "cross-project send requires user approval evidence for self-promoted source master",
        );
    }
    let mut st = server.state.lock().unwrap();
    let live_master = match live_master_id(server, &st) {
        Ok(master) => master,
        Err(error) => return Resp::err(error),
    };
    if live_master.as_deref() != Some(to.as_str()) {
        return Resp::err("cross-project communication requires the target to be a live master");
    }
    let Some(recipient) = st.workers.get(&to) else {
        return Resp::err(format!("recipient {} not registered", to));
    };
    if recipient.pane.as_deref().is_none_or(|pane| {
        (server.pane_alive_check)(pane) != PanePresence::Present
            || (server.pane_owner_check)(&to, pane) != Ok(true)
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
    mut worktree_path: Option<String>,
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
        let canonical = match validate_worktree_path(&server.root, path) {
            Ok(path) => path,
            Err(error) => return Resp::err(error),
        };
        worktree_path = Some(canonical.display().to_string());
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
    let mut events = vec![Event::TaskCreated { task: task.clone() }];
    if let Some(binding) = worktree_binding_for_task(server, &task) {
        events.push(Event::WorktreeBound { binding });
    }
    server.commit_locked(&mut st, &events);
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
    let canonical_worktree = match validate_worktree_path(&server.root, &worktree_path) {
        Ok(path) => path,
        Err(error) => return Resp::err(error),
    };
    let worktree_path = canonical_worktree.display().to_string();
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
    let mut events = vec![Event::TaskUpdated { task: task.clone() }];
    if let Some(binding) = worktree_binding_for_task(server, &task) {
        events.push(Event::WorktreeBound { binding });
    }
    server.commit_locked(&mut st, &events);
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

fn handle_task_accept(server: &Server, worker_id: String, token: String, task_id: String) -> Resp {
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
        return Resp::err("only the task owner may accept its assignment");
    }
    let Some(admission) = st
        .scheduler_admissions
        .values()
        .find(|admission| admission.task_id == task.id && admission.status == "succeeded")
    else {
        return Resp::err(
            "task assignment provenance is missing; accept only scheduler assignments",
        );
    };
    if admission.managed_subagent_id.is_some() {
        return Resp::err("managed assignment must be accepted with collab subagent working");
    }
    if task.status == "working" {
        return Resp::data(json!({
            "task": task.id,
            "status": task.status,
            "owner": task.owner,
            "accepted": true,
            "idempotent": true,
            "notification": "none",
            "next_action": task.next_step,
        }));
    }
    if task.status != "assigned" {
        return Resp::err(format!(
            "task {} is not assigned; current status is {}",
            task.id, task.status
        ));
    }
    task.status = "working".into();
    task.wait = None;
    task.updated_ms = now_ms();
    server.commit_locked(&mut st, &[Event::TaskUpdated { task: task.clone() }]);
    Resp::data(json!({
        "task": task.id,
        "status": task.status,
        "owner": task.owner,
        "accepted": true,
        "notification": "none",
        "next_action": task.next_step,
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
        if new_status == "working" && task.status == "assigned" {
            return Resp::err("use collab task accept to accept an assigned task");
        }
        // Pre-review producers persisted accepted candidates without a lifecycle
        // record. Keep their owner-local merge transition replayable while new
        // review records continue through the evidence-bearing integrated path.
        let legacy_accepted_merge = new_status == "merged"
            && task.status == "accepted"
            && !st.task_lifecycle.contains_key(&task.id);
        if new_status == "accepted" || (new_status == "merged" && !legacy_accepted_merge) {
            return Resp::err(
                "use collab task review/integrated for integration-owned lifecycle transitions",
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

fn stale_worker_views(
    st: &State,
    presence: &dyn Fn(&str) -> PanePresence,
) -> Vec<serde_json::Value> {
    st.workers
        .values()
        .filter(|worker| {
            worker
                .pane
                .as_deref()
                .is_some_and(|pane| presence(pane) == PanePresence::Missing)
        })
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
    tmux_session_for_pane_with(pane, &knock::tmux_output).ok()
}

fn tmux_session_for_pane_with(
    pane: &str,
    run: &dyn Fn(&[&str]) -> std::io::Result<std::process::Output>,
) -> Result<String, ()> {
    let output = run(&["display-message", "-p", "-t", pane, "#S"]).map_err(|_| ())?;
    if !output.status.success() {
        return Err(());
    }
    let name = std::str::from_utf8(&output.stdout).map_err(|_| ())?.trim();
    if name.is_empty() || name.contains(['\r', '\n']) {
        return Err(());
    }
    Ok(name.to_string())
}

fn tmux_pane_for_session(session: &str) -> Option<String> {
    let output = Command::new("tmux")
        .args(["list-panes", "-a", "-F", "#{session_name}\t#{pane_id}"])
        .output()
        .ok()?;
    if !output.status.success() {
        return None;
    }
    let output = String::from_utf8_lossy(&output.stdout);
    let mut panes = output.lines().filter_map(|line| {
        let (name, pane) = line.split_once('\t')?;
        (name == session && pane.starts_with('%')).then(|| pane.to_owned())
    });
    let pane = panes.next()?;
    panes.next().is_none().then_some(pane)
}

/// A peer owns its tmux pane only when the live session name matches the
/// worker id. Non-tmux operators always pass. Stale or split bindings wake
/// the wrong agent, so keepalive and notification paths consult this guard
/// before touching a pane.
pub(crate) fn pane_owner_authoritative(worker_id: &str, pane: &str) -> Result<bool, ()> {
    if pane.starts_with('%') {
        tmux_session_for_pane_with(pane, &knock::tmux_output).map(|session| session == worker_id)
    } else {
        Ok(true)
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
    let now = now_ms();
    task.status = "delivered".to_string();
    task.wait = None;
    task.next_step = Some(
        "task owner or live master reviews delivery with collab task review --accept or --rework"
            .to_string(),
    );
    task.updated_ms = now;
    let mut lifecycle = st.task_lifecycle.get(&task.id).cloned().unwrap_or_default();
    lifecycle.delivery_evidence = Some(evidence.clone());
    lifecycle.delivered_ms = Some(now);
    server.commit_locked(
        &mut st,
        &[
            Event::TaskUpdated { task: task.clone() },
            Event::TaskLifecycleUpdated {
                task_id: task.id.clone(),
                record: lifecycle,
            },
        ],
    );

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

fn task_integration_authorized(
    server: &Server,
    state: &State,
    task: &TaskRec,
    worker_id: &str,
) -> bool {
    task.owner == worker_id
        || live_master_id(server, state).ok().flatten().as_deref() == Some(worker_id)
}

fn resolve_authoritative_main_head(root: &Path) -> Result<String, Resp> {
    let output = Command::new("git")
        .current_dir(root)
        .args(["rev-parse", "--verify", "refs/heads/main^{commit}"])
        .output();
    match output {
        Ok(output) if output.status.success() => {
            let head = String::from_utf8_lossy(&output.stdout).trim().to_string();
            if head.is_empty() {
                Err(Resp::err_data(
                    "TASK_INTEGRATION_MAIN_UNRESOLVED",
                    json!({"root": root, "ref": "refs/heads/main"}),
                ))
            } else {
                Ok(head)
            }
        }
        Ok(output) => {
            let dirty = Command::new("git")
                .current_dir(root)
                .args(["status", "--porcelain", "--untracked-files=all"])
                .output()
                .map(|status| status.status.success() && !status.stdout.is_empty())
                .unwrap_or(false);
            Err(Resp::err_data(
                "TASK_INTEGRATION_MAIN_UNRESOLVED",
                json!({
                    "root": root,
                    "dirty": dirty,
                    "ref": "refs/heads/main",
                    "detail": String::from_utf8_lossy(&output.stderr).trim(),
                }),
            ))
        }
        Err(error) => Err(Resp::err_data(
            "TASK_INTEGRATION_MAIN_UNRESOLVED",
            json!({"root": root, "ref": "refs/heads/main", "detail": error.to_string()}),
        )),
    }
}

fn handle_task_review(
    server: &Server,
    worker_id: String,
    token: String,
    task_id: String,
    accept: bool,
    rework: bool,
    evidence: String,
) -> Resp {
    let mut st = server.state.lock().unwrap();
    if let Err(error) = verify(&st, &worker_id, &token) {
        return error;
    }
    if accept == rework {
        return Resp::err("task review requires exactly one of --accept or --rework");
    }
    let evidence = evidence.trim();
    if evidence.is_empty() {
        return Resp::err("task review requires non-empty --evidence");
    }
    let Some(task) = st.tasks.get(&task_id).cloned() else {
        return Resp::err(format!("task {} not found", task_id));
    };
    if task.status != "delivered" {
        return Resp::err(format!(
            "task {} must be delivered before review (current: {})",
            task_id, task.status
        ));
    }
    if !task_integration_authorized(server, &st, &task, &worker_id) {
        return Resp::err("task review requires task owner or live master authority");
    }
    let now = now_ms();
    let mut reviewed = task;
    reviewed.status = if accept { "accepted" } else { "rework" }.into();
    reviewed.next_step = Some(if accept {
        "integrate the accepted candidate on refs/heads/main, then record collab task integrated"
            .into()
    } else {
        format!("address review evidence: {evidence}")
    });
    reviewed.updated_ms = now;
    let mut lifecycle = st.task_lifecycle.get(&task_id).cloned().unwrap_or_default();
    lifecycle.review_evidence = Some(evidence.to_owned());
    lifecycle.reviewer = Some(worker_id.clone());
    lifecycle.reviewed_ms = Some(now);
    server.commit_locked(
        &mut st,
        &[
            Event::TaskUpdated {
                task: reviewed.clone(),
            },
            Event::TaskLifecycleUpdated {
                task_id: task_id.clone(),
                record: lifecycle,
            },
        ],
    );
    Resp::data(json!({
        "task": task_id,
        "status": reviewed.status,
        "reviewer": worker_id,
        "evidence": evidence,
        "next_action": reviewed.next_step,
    }))
}

fn handle_task_integrated(
    server: &Server,
    worker_id: String,
    token: String,
    task_id: String,
    commit: String,
    evidence: String,
) -> Resp {
    let mut st = server.state.lock().unwrap();
    if let Err(error) = verify(&st, &worker_id, &token) {
        return error;
    }
    let commit = commit.trim();
    let evidence = evidence.trim();
    if commit.is_empty() || evidence.is_empty() {
        return Resp::err("task integrated requires non-empty --commit and --evidence");
    }
    let Some(task) = st.tasks.get(&task_id).cloned() else {
        return Resp::err(format!("task {} not found", task_id));
    };
    if task.status != "accepted" {
        return Resp::err(format!(
            "task {} must be accepted before integration (current: {})",
            task_id, task.status
        ));
    }
    if !task_integration_authorized(server, &st, &task, &worker_id) {
        return Resp::err("task integrated requires task owner or live master authority");
    }
    let head = match resolve_authoritative_main_head(&server.root) {
        Ok(head) => head,
        Err(error) => return error,
    };
    if commit != head {
        return Resp::err_data(
            "TASK_INTEGRATION_COMMIT_MISMATCH",
            json!({"provided": commit, "main_head": head}),
        );
    }
    let now = now_ms();
    let mut integrated = task;
    integrated.status = "merged".into();
    integrated.next_step = Some("owner cleans the worktree/branch and closes the task".into());
    integrated.updated_ms = now;
    let mut lifecycle = st.task_lifecycle.get(&task_id).cloned().unwrap_or_default();
    lifecycle.integration_commit = Some(commit.to_owned());
    lifecycle.integration_evidence = Some(evidence.to_owned());
    lifecycle.integrated_ms = Some(now);
    server.commit_locked(
        &mut st,
        &[
            Event::TaskUpdated {
                task: integrated.clone(),
            },
            Event::TaskLifecycleUpdated {
                task_id: task_id.clone(),
                record: lifecycle,
            },
        ],
    );
    Resp::data(json!({
        "task": task_id,
        "status": integrated.status,
        "commit": commit,
        "evidence": evidence,
        "next_action": integrated.next_step,
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
        let live_master = match live_master_id(server, &st) {
            Ok(master) => master,
            Err(error) => return Resp::err(error),
        };
        let owner_identity_live = st
            .workers
            .get(&task.owner)
            .and_then(|owner| owner.pane.as_deref())
            .is_some_and(|pane| {
                (server.pane_alive_check)(pane) != PanePresence::Missing
                    && (server.pane_owner_check)(&task.owner, pane) != Ok(false)
            });
        let authorized = live_master.as_deref() == Some(worker_id.as_str())
            || (live_master.is_none() && (task.owner == worker_id || !owner_identity_live));
        if !authorized {
            return Resp::err_data(
                "manual force close is not authorized for this caller",
                json!({
                    "live_master": live_master,
                    "task_owner": task.owner,
                    "requester": worker_id,
                    "rule": "live master may close any task; with no live master, the owner may close its task or a registered peer may close an orphaned task whose owner identity is no longer live",
                    "owner_identity_live": owner_identity_live,
                }),
            );
        }
        if task.status == "closed" {
            if let Some(receipt) = st.cleanup_receipts.get(&task.id).filter(|receipt| {
                receipt.task_id == task.id
                    && receipt.worktree_path == task.worktree_path
                    && receipt.branch == task.branch
            }) {
                return Resp::data(json!({
                    "task": task.id,
                    "status": task.status,
                    "owner": task.owner,
                    "manual": true,
                    "reason": receipt.manual_reason,
                    "receipt_id": receipt.id,
                    "superseded_pending_keepalives": [],
                    "stale_workers": stale_worker_views(&st, &server.pane_alive_check),
                    "idempotent": true,
                    "next_action": "lifecycle complete; keepalives for this task owner stopped",
                }));
            }
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
            .filter(|m| {
                m.to == closed.owner
                    && m.mtype == "keepalive"
                    && matches!(m.state.as_str(), "pending" | "delivered")
            })
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
        let other_actionable = st.tasks.values().any(|t| {
            t.id != closed.id
                && t.owner == closed.owner
                && crate::server::keepalive::actionable(&t.status)
        });
        if !other_actionable {
            if let Some(record) = st.keepalives.get(&closed.owner).cloned() {
                if record.unacked > 0 || record.last_notice_id.is_some() {
                    let mut updated = record;
                    updated.unacked = 0;
                    updated.last_notice_id = None;
                    events.push(Event::KeepaliveUpdated {
                        worker_id: closed.owner.clone(),
                        record: updated,
                    });
                }
            }
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
    let superseded: Vec<String> = st
        .msgs
        .values()
        .filter(|m| {
            m.to == closed.owner
                && m.mtype == "keepalive"
                && matches!(m.state.as_str(), "pending" | "delivered")
        })
        .map(|m| m.id.clone())
        .collect();
    let mut close_events: Vec<Event> = vec![
        Event::CleanupVerified {
            receipt: receipt.clone(),
        },
        Event::TaskUpdated {
            task: closed.clone(),
        },
    ];
    if !superseded.is_empty() {
        close_events.push(Event::Superseded { ids: superseded });
    }
    let other_actionable = st.tasks.values().any(|t| {
        t.id != closed.id
            && t.owner == closed.owner
            && crate::server::keepalive::actionable(&t.status)
    });
    if !other_actionable {
        if let Some(record) = st.keepalives.get(&closed.owner).cloned() {
            if record.unacked > 0 || record.last_notice_id.is_some() {
                let mut updated = record;
                updated.unacked = 0;
                updated.last_notice_id = None;
                close_events.push(Event::KeepaliveUpdated {
                    worker_id: closed.owner.clone(),
                    record: updated,
                });
            }
        }
    }
    server.commit_locked(&mut st, &close_events);

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
        let mut events = vec![
            Event::TaskUpdated { task: waiter_task },
            Event::MasterWakeSignal {
                signal: state::MasterWakeSignal::TaskFreed {
                    task_id: closed.id.clone(),
                },
                at_ms: now_ms(),
            },
        ];
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
    let lifecycle = state.task_lifecycle.get(&task.id);
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
        "delivery": {
            "evidence": lifecycle.and_then(|record| record.delivery_evidence.clone()),
            "at": lifecycle.and_then(|record| record.delivered_ms.map(iso)),
        },
        "review": {
            "evidence": lifecycle.and_then(|record| record.review_evidence.clone()),
            "reviewer": lifecycle.and_then(|record| record.reviewer.clone()),
            "at": lifecycle.and_then(|record| record.reviewed_ms.map(iso)),
        },
        "integration": {
            "commit": lifecycle.and_then(|record| record.integration_commit.clone()),
            "evidence": lifecycle.and_then(|record| record.integration_evidence.clone()),
            "at": lifecycle.and_then(|record| record.integrated_ms.map(iso)),
        },
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
        "master": match live_master_id(server, &st) {
            Ok(master) => master.map(|id| json!({"worker_id": id})),
            Err(error) => Some(json!({"status": "unknown", "error": error})),
        },
        "authority": {
            "managed_subagent": managed,
            "must_obey_master": managed,
            "may_decline_master_invite": !managed,
        },
        "truth": "server journal and mailbox; tmux is wake-only",
    }))
}

fn poll_messages(server: &Server, worker_id: &str) -> Option<Resp> {
    let ids: Vec<String>;
    let msgs: Vec<Message>;
    let mut st = server.state.lock().unwrap();
    let unread = st.inbox_of(worker_id);
    if unread.is_empty() {
        return None;
    }
    ids = unread.iter().map(|m| m.id.clone()).collect();
    msgs = unread.into_iter().cloned().collect();
    // recv is an explicit read operation: deliver and consume the same batch
    // atomically so a successful read cannot leave a new ACK obligation.
    let mut events = vec![Event::Delivered { ids: ids.clone() }, Event::Acked { ids }];
    if let Some(record) = st.keepalives.get(worker_id).cloned() {
        if record.unacked > 0 || record.last_notice_id.is_some() || record.suspected_offline {
            let mut updated = record;
            updated.unacked = 0;
            updated.last_notice_id = None;
            updated.suspected_offline = false;
            updated.activity_ms = now_ms();
            events.push(Event::KeepaliveUpdated {
                worker_id: worker_id.to_owned(),
                record: updated,
            });
        }
    }
    server.commit_locked(&mut st, &events);
    Some(Resp::data(json!({
        "messages": msgs,
        "count": msgs.len(),
        "fetched_at": iso(now_ms()),
    })))
}

async fn poll_messages_async(server: Arc<Server>, worker_id: &str) -> Option<Resp> {
    let worker_id = worker_id.to_owned();
    tokio::task::spawn_blocking(move || poll_messages(&server, &worker_id))
        .await
        .unwrap_or_else(|error| Some(Resp::err(format!("poll handler join error: {}", error))))
}

async fn handle_poll_async(server: Arc<Server>, worker_id: String, timeout_ms: u64) -> Resp {
    let timeout_ms = timeout_ms.min(MAX_POLL_MS);
    let mut notified = Box::pin(server.mailbox_notify.notified());
    let timeout = tokio::time::sleep(Duration::from_millis(timeout_ms));
    tokio::pin!(timeout);
    loop {
        notified.as_mut().enable();
        if let Some(response) = poll_messages_async(server.clone(), &worker_id).await {
            return response;
        }
        tokio::select! {
            _ = notified.as_mut() => {
                notified.set(server.mailbox_notify.notified());
            }
            _ = &mut timeout => {
                return Resp::data(json!({"messages": [], "count": 0, "timeout": true}));
            }
        }
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
        "delivered" | "accepted" | "merged" | "closed" | "cancelled"
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

fn worker_status_summary_with_maps(
    server: &Server,
    tasks: &std::collections::HashMap<String, TaskRec>,
    msgs: &std::collections::HashMap<String, Message>,
    keepalives: &std::collections::HashMap<String, crate::server::keepalive::Record>,
    w: &WorkerRec,
) -> serde_json::Value {
    let active = tasks
        .values()
        .find(|task| task.owner == w.id && !matches!(task.status.as_str(), "closed" | "cancelled"));
    let pane = w.pane.as_deref();
    let presence = pane
        .map(server.pane_alive_check)
        .unwrap_or(PanePresence::Missing);
    let endpoint_live = presence == PanePresence::Present;
    let ownership = if endpoint_live {
        pane.map(|p| (server.pane_owner_check)(&w.id, p))
    } else {
        None
    };
    let identity_valid = endpoint_live && ownership == Some(Ok(true));
    let agent_state = if endpoint_live && identity_valid {
        pane.map(|p| match (server.pane_state_check)(p) {
            crate::server::knock::AgentState::Waiting => "waiting",
            crate::server::knock::AgentState::Working => "working",
            crate::server::knock::AgentState::Absent => "absent",
            crate::server::knock::AgentState::Unknown => "unknown",
        })
        .unwrap_or("absent")
    } else if presence == PanePresence::Unknown || (endpoint_live && ownership == Some(Err(()))) {
        "unknown"
    } else {
        "absent"
    };
    let unacked_notifications = msgs
        .values()
        .filter(|m| m.to == w.id && m.state == "delivered")
        .count();
    let pending_notifications = msgs
        .values()
        .filter(|m| m.to == w.id && m.state == "pending")
        .count();
    let notifications_paused = false;
    let keepalive = keepalives.get(&w.id);
    let suspected_offline = keepalive.map(|k| k.suspected_offline).unwrap_or(false);
    let unacked_keepalives = keepalive.map(|k| k.unacked).unwrap_or(0);
    let status = if agent_state == "unknown" {
        "unknown"
    } else if !endpoint_live {
        "lost"
    } else if !identity_valid {
        "identity-mismatch"
    } else if suspected_offline {
        "offline"
    } else {
        agent_state
    };
    let diagnostic = if status == "unknown" {
        None
    } else if status == "lost" {
        Some("pane dead or not found; clean up task or restart pane")
    } else if status == "identity-mismatch" {
        Some("pane re-bound or owned by different process; verify pane ownership")
    } else if suspected_offline {
        Some("unresponsive; run snapshot: collab subagent snapshot <id> --lines 40")
    } else {
        None
    };
    json!({
        "id": w.id,
        "pane": w.pane,
        "status": status,
        "presence": match presence {
            PanePresence::Present => "present",
            PanePresence::Missing => "missing",
            PanePresence::Unknown => "unknown",
        },
        "endpoint_live": (presence != PanePresence::Unknown).then_some(endpoint_live),
        "identity_valid": (presence != PanePresence::Unknown && ownership != Some(Err(()))).then_some(identity_valid),
        "agent_state": agent_state,
        "unacked_notifications": unacked_notifications,
        "pending_notifications": pending_notifications,
        "notifications_paused": notifications_paused,
        "unacked_keepalives": unacked_keepalives,
        "suspected_offline": suspected_offline,
        "diagnostic": diagnostic,
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
        | Req::TaskAccept { .. }
        | Req::TaskClaim { .. }
        | Req::TaskWait { .. }
        | Req::TaskDeliver { .. }
        | Req::TaskReview { .. }
        | Req::TaskIntegrated { .. }
        | Req::TaskClose { .. }
        | Req::TaskDispatch { .. }
        | Req::MigrationPlan { .. }
        | Req::MigrationApply { .. }
        | Req::MasterPromote { .. }
        | Req::MasterDelegate { .. }
        | Req::TransferMaster { .. }
        | Req::RemoveWorker { .. }
        | Req::WorkerClose { .. }
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
        | Req::Ping
        | Req::StatusAll
        | Req::MailboxRead { .. } => false,
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
            worker_id,
            token,
            command,
            to,
            mtype,
            subject,
            body,
            in_reply_to,
            delivery,
        } => {
            let Some(worker_id) = worker_id else {
                return Resp::err(
                    "LEGACY_SEND_REJECTED: authenticated sender binding and route scope are required",
                );
            };
            let Some(token) = token else {
                return Resp::err(
                    "LEGACY_SEND_REJECTED: authenticated sender binding and route scope are required",
                );
            };
            handle_authenticated_send(
                server,
                from,
                worker_id,
                token,
                command,
                to,
                mtype,
                subject,
                body,
                in_reply_to,
                delivery,
            )
        }
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
        Req::Poll { .. } => {
            Resp::err("Poll is only handled by the async daemon connection path; use collab recv")
        }
        Req::Ack {
            worker_id,
            token,
            ids,
        } => {
            let mut st = server.state.lock().unwrap();
            if let Err(e) = verify(&st, &worker_id, &token) {
                return e;
            }
            let mut acked = Vec::new();
            let mut already_acked = Vec::new();
            let mut not_found = Vec::new();
            let mut restored_msgs = Vec::new();

            if ids.is_empty() {
                for m in st.msgs.values() {
                    if m.to == worker_id {
                        if m.state == "delivered" || m.state == "pending" {
                            acked.push(m.id.clone());
                        } else if m.state == "read" {
                            already_acked.push(m.id.clone());
                        }
                    }
                }
            } else {
                for id in ids {
                    let in_mem = st.msgs.get(&id).cloned();
                    let msg = in_mem.or_else(|| {
                        let path = server
                            .root
                            .join(".agent-collab")
                            .join("mailbox")
                            .join(format!("{}.json", id));
                        std::fs::read_to_string(&path)
                            .ok()
                            .and_then(|s| serde_json::from_str::<Message>(&s).ok())
                    });
                    match msg {
                        Some(m) if m.to == worker_id => {
                            if !st.msgs.contains_key(&id) {
                                restored_msgs.push(m.clone());
                            }
                            if m.state == "delivered" || m.state == "pending" {
                                acked.push(id);
                            } else {
                                already_acked.push(id);
                            }
                        }
                        _ => {
                            not_found.push(id);
                        }
                    }
                }
            }

            let mut events = Vec::new();
            for m in restored_msgs {
                events.push(Event::Sent { msg: m });
            }
            if !acked.is_empty() {
                events.push(Event::Acked { ids: acked.clone() });
            }

            // An explicit or bulk ACK from an authenticated worker proves
            // the worker is responsive and active. Clear keepalive unacked counter.
            if let Some(record) = st.keepalives.get(&worker_id).cloned() {
                if record.unacked > 0 || record.last_notice_id.is_some() || record.suspected_offline
                {
                    let mut updated = record;
                    let now = crate::server::state::now_ms();
                    updated.unacked = 0;
                    updated.last_notice_id = None;
                    updated.suspected_offline = false;
                    updated.activity_ms = now;
                    events.push(Event::KeepaliveUpdated {
                        worker_id: worker_id.clone(),
                        record: updated,
                    });
                }
            }

            if !events.is_empty() {
                server.commit_locked(&mut st, &events);
            }
            drop(st);
            Resp::data(json!({
                "acked": acked,
                "already_acked": already_acked,
                "not_found": not_found,
            }))
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
            let in_mem = st.msgs.get(&msg_id).cloned();
            let msg = in_mem.or_else(|| {
                let path = server
                    .root
                    .join(".agent-collab")
                    .join("mailbox")
                    .join(format!("{}.json", msg_id));
                std::fs::read_to_string(&path)
                    .ok()
                    .and_then(|s| serde_json::from_str::<Message>(&s).ok())
            });
            let answered = st.answered(&msg_id);
            drop(st);
            match msg {
                Some(m) => Resp::data(json!({
                    "id": m.id, "from": m.from, "to": m.to, "type": m.mtype,
                    "subject": m.subject, "body": m.body,
                    "state": m.state, "wake_attempts": m.wake_attempt_count,
                    "created_at": iso(m.created_ms), "answered": answered,
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
        Req::TaskAccept {
            worker_id,
            token,
            task_id,
        } => handle_task_accept(server, worker_id, token, task_id),
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
        Req::TaskReview {
            worker_id,
            token,
            task_id,
            accept,
            rework,
            evidence,
        } => handle_task_review(server, worker_id, token, task_id, accept, rework, evidence),
        Req::TaskIntegrated {
            worker_id,
            token,
            task_id,
            commit,
            evidence,
        } => handle_task_integrated(server, worker_id, token, task_id, commit, evidence),
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
            let (workers_rec, tasks_map, msgs_map, keepalives_map) = {
                let st = server.state.lock().unwrap();
                (
                    st.workers.values().cloned().collect::<Vec<_>>(),
                    st.tasks.clone(),
                    st.msgs.clone(),
                    st.keepalives.clone(),
                )
            };
            let mut workers: Vec<serde_json::Value> = workers_rec
                .iter()
                .map(|w| {
                    worker_status_summary_with_maps(
                        server,
                        &tasks_map,
                        &msgs_map,
                        &keepalives_map,
                        w,
                    )
                })
                .collect();
            workers.sort_by(|a, b| a["id"].as_str().cmp(&b["id"].as_str()));
            Resp::data(json!({
                "workers": workers,
                "count": workers.len()
            }))
        }
        Req::WorkerClose {
            worker_id,
            token,
            target_id,
            reason,
            kill_session,
        } => handle_worker_close(server, worker_id, token, target_id, reason, kill_session),
        Req::WorkerStatus { worker_id } => {
            let (workers_rec, tasks_map, msgs_map, keepalives_map) = {
                let st = server.state.lock().unwrap();
                (
                    st.workers.values().cloned().collect::<Vec<_>>(),
                    st.tasks.clone(),
                    st.msgs.clone(),
                    st.keepalives.clone(),
                )
            };
            let mut workers: Vec<serde_json::Value> = workers_rec
                .iter()
                .filter(|w| worker_id.as_ref().is_none_or(|id| id == &w.id))
                .map(|w| {
                    worker_status_summary_with_maps(
                        server,
                        &tasks_map,
                        &msgs_map,
                        &keepalives_map,
                        w,
                    )
                })
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
        Req::StatusAll => {
            let (
                workers_rec,
                tasks,
                subagents,
                msgs_len,
                tasks_map,
                msgs_map,
                keepalives_map,
                master_wake,
                now,
            ) = {
                let st = server.state.lock().unwrap();
                let workers_rec: Vec<WorkerRec> = st.workers.values().cloned().collect();
                let mut tasks: Vec<serde_json::Value> =
                    st.tasks.values().map(|task| task_view(&st, task)).collect();
                tasks.sort_by(|a, b| a["id"].as_str().cmp(&b["id"].as_str()));
                let mut subagents: Vec<crate::subagent::Record> =
                    st.subagents.values().cloned().collect();
                subagents.sort_by(|a, b| a.id.cmp(&b.id));
                let msgs_len = st.msgs.len();
                let now = now_ms();
                (
                    workers_rec,
                    tasks,
                    subagents,
                    msgs_len,
                    st.tasks.clone(),
                    st.msgs.clone(),
                    st.keepalives.clone(),
                    st.master_wake.clone(),
                    now,
                )
            };
            let mut workers: Vec<serde_json::Value> = workers_rec
                .iter()
                .map(|w| {
                    worker_status_summary_with_maps(
                        server,
                        &tasks_map,
                        &msgs_map,
                        &keepalives_map,
                        w,
                    )
                })
                .collect();
            workers.sort_by(|a, b| a["id"].as_str().cmp(&b["id"].as_str()));

            Resp::data(json!({
                "summary": {
                    "workers": workers.len(),
                    "messages": msgs_len,
                    "tasks": tasks.len(),
                    "subagents": subagents.len(),
                    "now": iso(now),
                },
                "master_wake": master_wake,
                "workers": workers,
                "tasks": tasks,
                "subagents": subagents,
            }))
        }
        Req::MailboxRead {
            all,
            sort,
            worker_id,
        } => {
            let st = server.state.lock().unwrap();
            let mut msgs: Vec<Message> = st
                .msgs
                .values()
                .filter(|m| {
                    if all {
                        true
                    } else if let Some(wid) = &worker_id {
                        &m.to == wid || &m.from == wid
                    } else {
                        true
                    }
                })
                .cloned()
                .collect();
            let recipient_messages = worker_id.as_deref().map(|recipient| {
                st.msgs
                    .values()
                    .filter(|message| message.to == recipient)
                    .cloned()
                    .collect::<Vec<_>>()
            });
            let sort_order = sort.as_deref().unwrap_or("time-asc");
            if sort_order == "time-desc" {
                msgs.sort_by(|a, b| b.created_ms.cmp(&a.created_ms));
            } else {
                msgs.sort_by(|a, b| a.created_ms.cmp(&b.created_ms));
            }
            let count = msgs.len();
            drop(st);
            let projection = worker_id.as_deref().and_then(|recipient| {
                let path = server
                    .root
                    .join(".agent-collab/mailbox")
                    .join(format!("recipient-{recipient}.jsonl"));
                match read_recipient_mailbox(&path, recipient) {
                    Ok(read) => {
                        let missing = missing_recipient_projection_messages(
                            recipient_messages.as_deref().unwrap_or_default(),
                            recipient,
                            &read,
                        );
                        let status = if read.partial_tail {
                            "partial-tail"
                        } else if !read.recoverable_errors.is_empty() {
                            "recoverable-error"
                        } else if missing.is_empty() {
                            "ok"
                        } else {
                            "incomplete"
                        };
                        let mut exact_errors = read.recoverable_errors.clone();
                        if !missing.is_empty() {
                            exact_errors.push(format!(
                                "recipient JSONL is missing message records: {}",
                                missing.join(",")
                            ));
                        }
                        let exact_error =
                            (!exact_errors.is_empty()).then(|| exact_errors.join(" | "));
                        Some(json!({
                            "status": status,
                            "partial_tail": read.partial_tail,
                            "recoverable_errors": read.recoverable_errors,
                            "missing_message_ids": missing,
                            "exact_error": exact_error,
                            "records": read.records,
                        }))
                    }
                    Err(error) => Some(json!({
                        "status": "error",
                        "exact_error": error,
                        "records": [],
                    })),
                }
            });
            let mut response = json!({
                "count": count,
                "sort": sort_order,
                "messages": msgs,
            });
            if let Some(projection) = projection {
                response["recipient_jsonl"] = projection;
            }
            Resp::data(response)
        }
    }
}

fn request_requires_project_context(req: &Req) -> bool {
    !matches!(req, Req::Ping)
}

/// Validate the route carried by a host-daemon request before the legacy
/// project reducer sees it.  The first host-routing seam intentionally admits
/// only the project loaded by this daemon instance; a different canonical
/// project receives an explicit rejection until a multi-project reducer can
/// preserve independent journals safely.
pub(crate) fn validate_request_context(
    server: &Server,
    req: &Req,
    project_context: Option<&ProjectContext>,
) -> Result<(), String> {
    let Some(project_context) = project_context else {
        if request_requires_project_context(req) {
            return Err(
                "PROJECT_CONTEXT_REQUIRED: canonical project root and scope are required".into(),
            );
        }
        return Ok(());
    };

    project_context
        .validate()
        .map_err(|error| format!("PROJECT_CONTEXT_INVALID: {error}"))?;
    let registered_scope = GlobalState::canonical_project_scope(&server.root)
        .map_err(|error| format!("PROJECT_SCOPE_UNKNOWN: {error}"))?;
    if project_context.canonical_root != registered_scope.as_str()
        || project_context.project_scope != registered_scope
    {
        return Err(format!(
            "PROJECT_SCOPE_UNKNOWN: host daemon is bound to {}, request selected {}",
            registered_scope.as_str(),
            project_context.canonical_root
        ));
    }

    if let Req::Register { cwd, .. } = req {
        let request_scope = GlobalState::canonical_project_scope(Path::new(cwd))
            .map_err(|error| format!("PROJECT_SCOPE_INVALID: {error}"))?;
        if request_scope != registered_scope {
            return Err(format!(
                "PROJECT_SCOPE_MISMATCH: register cwd {} is outside {}",
                cwd,
                registered_scope.as_str()
            ));
        }
    }
    Ok(())
}

fn parse_wire_request(line: &str) -> Result<(Option<ProjectContext>, Req), String> {
    match serde_json::from_str::<RequestEnvelope>(line) {
        Ok(envelope) => Ok(envelope.into_parts()),
        Err(envelope_error) => serde_json::from_str::<Req>(line)
            .map(|request| (None, request))
            .map_err(|request_error| {
                format!("bad request: {request_error}; wire envelope parse: {envelope_error}")
            }),
    }
}

async fn dispatch_wire(
    server: Arc<Server>,
    project_context: Option<ProjectContext>,
    req: Req,
) -> Resp {
    if let Err(error) = validate_request_context(&server, &req, project_context.as_ref()) {
        return Resp::err(error);
    }
    match req {
        Req::Poll {
            worker_id,
            token,
            timeout_ms,
        } => {
            let admission = {
                let check = server.state.lock().unwrap();
                if let Err(error) = verify(&check, &worker_id, &token) {
                    Some(error)
                } else if check.admission_frozen() {
                    Some(Resp::err("MIGRATION_ADMISSION_FROZEN: only identity rebind, read queries, daemon restart, and migration verify are allowed"))
                } else {
                    None
                }
            };
            match admission {
                Some(response) => response,
                None => handle_poll_async(server, worker_id, timeout_ms).await,
            }
        }
        req => tokio::task::spawn_blocking(move || dispatch(&server, req))
            .await
            .unwrap_or_else(|e| Resp::err(format!("handler join error: {}", e))),
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
        let resp = match parse_wire_request(&line) {
            Ok((project_context, req)) => {
                let activity_req = req.clone();
                let resp = dispatch_wire(server.clone(), project_context, req).await;
                let _ = record_activity(
                    &server.root,
                    "request",
                    request_activity(&activity_req, &resp),
                );
                resp
            }
            Err(error) => {
                let resp = Resp::err(error);
                let _ =
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

fn apply_replayed_event(st: &mut State, event: &Event, line: usize) -> anyhow::Result<()> {
    st.apply_checked(event)
        .map_err(|error| anyhow::anyhow!("journal replay failed at line {line}: {error}"))?;
    match event {
        Event::ReducerCheckpoint { sequence, revision } => st
            .set_checkpoint_version(*sequence, *revision)
            .map_err(|error| anyhow::anyhow!("journal replay failed at line {line}: {error}")),
        _ => st
            .advance_version()
            .map_err(|error| anyhow::anyhow!("journal replay failed at line {line}: {error}")),
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
    let mut pending_command: Option<(String, String, Vec<Event>)> = None;
    let mut seen_command_ids = std::collections::HashSet::new();
    let mut seen_operation_ids = std::collections::HashMap::new();
    let mut convert_root = false;
    for (index, line) in content.lines().enumerate() {
        let trimmed = line.trim();
        if trimmed.is_empty() {
            continue;
        }
        let line_events = decode_journal_line(trimmed).map_err(|error| {
            anyhow::anyhow!(
                "journal replay failed at line {}: {}; manual journal edits are unsupported",
                index + 1,
                error
            )
        })?;
        if line_events.len() > 1
            && !line_events
                .iter()
                .any(|event| matches!(event, Event::ReducerCheckpoint { .. }))
        {
            convert_root = true;
        }
        for event in line_events {
            if let Event::CommandStarted {
                command_id,
                operation_id,
            } = &event
            {
                if !seen_command_ids.insert(command_id.clone()) {
                    anyhow::bail!(
                        "journal replay failed at line {}: duplicate command {}",
                        index + 1,
                        command_id
                    );
                }
                if let Some(existing_command_id) =
                    seen_operation_ids.insert(operation_id.clone(), command_id.clone())
                {
                    anyhow::bail!(
                        "journal replay failed at line {}: operation {} already belongs to command {}",
                        index + 1,
                        operation_id,
                        existing_command_id
                    );
                }
                if pending_command.is_some() {
                    anyhow::bail!(
                        "journal replay failed at line {}: nested command {}",
                        index + 1,
                        command_id
                    );
                }
                pending_command = Some((
                    command_id.clone(),
                    operation_id.clone(),
                    vec![event.clone()],
                ));
                continue;
            }
            if let Some((command_id, operation_id, pending)) = pending_command.as_mut() {
                match &event {
                    Event::CommandCompleted {
                        command_id: completed_id,
                        operation_id: completed_operation,
                        receipt,
                    } => {
                        if completed_id != command_id || completed_operation != operation_id {
                            anyhow::bail!(
                                "journal replay failed at line {}: command completion does not match start",
                                index + 1
                            );
                        }
                        if receipt.operation_id != *operation_id {
                            anyhow::bail!(
                                "journal replay failed at line {}: command receipt operation does not match start",
                                index + 1
                            );
                        }
                        pending.push(event.clone());
                        let committed = std::mem::take(pending);
                        pending_command = None;
                        for event in committed {
                            apply_replayed_event(&mut st, &event, index + 1)?;
                            events.push(event);
                        }
                    }
                    Event::CommandStarted { .. } => unreachable!(),
                    _ => pending.push(event.clone()),
                }
                continue;
            }
            if matches!(event, Event::CommandCompleted { .. }) {
                anyhow::bail!(
                    "journal replay failed at line {}: command completion without start",
                    index + 1
                );
            }
            if matches!(event, Event::MasterAssigned { .. })
                && (line.contains("\"ev\":\"RootAssigned\"")
                    || line.contains("\"ev\": \"RootAssigned\""))
            {
                convert_root = true;
            }
            apply_replayed_event(&mut st, &event, index + 1)?;
            events.push(event);
        }
    }
    if let Some((command_id, _, _)) = pending_command {
        anyhow::bail!(
            "journal replay failed: incomplete command {command_id}; completion marker missing"
        );
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

fn decode_journal_line(line: &str) -> Result<Vec<Event>, notification_contract::JournalError> {
    let mut events = Vec::new();
    let mut stream = serde_json::Deserializer::from_str(line).into_iter::<Event>();
    while let Some(item) = stream.next() {
        events.push(
            item.map_err(|error| notification_contract::JournalError::Replay(error.to_string()))?,
        );
    }
    let offset = stream.byte_offset();
    if offset < line.len() && !line[offset..].trim().is_empty() {
        return Err(notification_contract::JournalError::Replay(
            "trailing characters".into(),
        ));
    }
    if events.is_empty() {
        return Err(notification_contract::JournalError::Replay(
            "empty event".into(),
        ));
    }
    Ok(events)
}

fn acquire_daemon_lock(lock_path: &Path, socket_path: &Path) -> anyhow::Result<std::fs::File> {
    let file = std::fs::OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .open(&lock_path)?;
    use std::os::unix::io::AsRawFd;
    let rc = unsafe { libc::flock(file.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) };
    if rc == 0 {
        return Ok(file);
    }
    let error = std::io::Error::last_os_error();
    match error.raw_os_error() {
        Some(code) if code == libc::EAGAIN || code == libc::EWOULDBLOCK => anyhow::bail!(
            "server already running at {}: {}",
            socket_path.display(),
            error
        ),
        Some(code) if code == libc::EPERM => anyhow::bail!(
            "cannot acquire daemon lock at {}: flock is unavailable or denied; refusing PID fallback: {}",
            lock_path.display(),
            error
        ),
        _ => Err(error.into()),
    }
}

fn probe_lock(lock_path: &Path) -> anyhow::Result<bool> {
    let file = match std::fs::OpenOptions::new()
        .read(true)
        .write(true)
        .open(lock_path)
    {
        Ok(file) => file,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(false),
        Err(error) => return Err(error.into()),
    };
    use std::os::unix::io::AsRawFd;
    let rc = unsafe { libc::flock(file.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) };
    if rc == 0 {
        let unlock = unsafe { libc::flock(file.as_raw_fd(), libc::LOCK_UN) };
        if unlock != 0 {
            return Err(std::io::Error::last_os_error().into());
        }
        return Ok(false);
    }
    let error = std::io::Error::last_os_error();
    if matches!(
        error.raw_os_error(),
        Some(code) if code == libc::EAGAIN || code == libc::EWOULDBLOCK
    ) {
        return Ok(true);
    }
    Err(error.into())
}

fn probe_legacy_socket(socket_path: &Path) -> anyhow::Result<bool> {
    match crate::client::connect(socket_path) {
        Ok(stream) => {
            drop(stream);
            Ok(true)
        }
        Err(error)
            if matches!(
                error.kind(),
                std::io::ErrorKind::NotFound | std::io::ErrorKind::ConnectionRefused
            ) =>
        {
            Ok(false)
        }
        Err(error) => Err(anyhow::Error::new(error)),
    }
}

/// Reject startup while a pre-host-endpoint daemon can still write the same
/// project journal.  Stale sockets are preserved and are safe to classify as
/// absent only when the connection probe returns `NotFound` or
/// `ConnectionRefused`; any other probe error is unknown and fails closed.
fn ensure_legacy_writer_absent(scope: &Scope, host_paths: &HostPaths) -> anyhow::Result<()> {
    let project_server_dir = scope.server_dir();
    let legacy_socket = project_server_dir.join("server.sock");
    if legacy_socket != host_paths.socket_path() && probe_legacy_socket(&legacy_socket)? {
        anyhow::bail!(
            "DAEMON_MIGRATION_REQUIRED: legacy project daemon is reachable at {}; stop or migrate it before starting the host daemon",
            legacy_socket.display()
        );
    }

    let legacy_project_lock = project_server_dir.join("daemon.lock");
    if legacy_project_lock != host_paths.lock_path() && probe_lock(&legacy_project_lock)? {
        anyhow::bail!(
            "DAEMON_MIGRATION_REQUIRED: legacy project daemon lock is held at {}; stop or migrate it before starting the host daemon",
            legacy_project_lock.display()
        );
    }

    let legacy_host_lock = Path::new(LEGACY_HOST_DAEMON_LOCK_PATH);
    if probe_lock(legacy_host_lock)? {
        anyhow::bail!(
            "DAEMON_MIGRATION_REQUIRED: legacy host daemon lock is held at {}; stop or migrate it before starting the host daemon",
            legacy_host_lock.display()
        );
    }
    Ok(())
}

fn same_inode(left: &std::fs::Metadata, right: &std::fs::Metadata) -> bool {
    use std::os::unix::fs::MetadataExt;
    left.dev() == right.dev() && left.ino() == right.ino()
}

fn prepare_socket_path(sock_path: &Path) -> anyhow::Result<()> {
    use std::os::unix::fs::FileTypeExt;

    let before = match std::fs::symlink_metadata(sock_path) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(()),
        Err(error) => return Err(error.into()),
    };
    if !before.file_type().is_socket() {
        anyhow::bail!(
            "server socket path is occupied by {}; refusing to remove it",
            sock_path.display()
        );
    }
    match crate::client::connect(sock_path) {
        Ok(_) => anyhow::bail!("server already running at {}", sock_path.display()),
        Err(error)
            if matches!(
                error.kind(),
                std::io::ErrorKind::ConnectionRefused | std::io::ErrorKind::NotFound
            ) => {}
        Err(error) => {
            return Err(anyhow::Error::new(error).context(format!(
                "cannot determine whether stale server socket {} can be removed",
                sock_path.display()
            )))
        }
    }
    let after = match std::fs::symlink_metadata(sock_path) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(()),
        Err(error) => return Err(error.into()),
    };
    if !same_inode(&before, &after) {
        anyhow::bail!(
            "server socket changed while checking {}; refusing to remove it",
            sock_path.display()
        );
    }
    std::fs::remove_file(sock_path)?;
    Ok(())
}

fn remove_listener_socket(sock_path: &Path, captured: &std::fs::Metadata) -> anyhow::Result<()> {
    let current = match std::fs::symlink_metadata(sock_path) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(()),
        Err(error) => return Err(error.into()),
    };
    if same_inode(captured, &current) {
        std::fs::remove_file(sock_path)?;
    }
    Ok(())
}

pub async fn run(scope: Scope) -> anyhow::Result<()> {
    let host_paths = scope.host_paths()?;
    run_with_host_paths(scope, host_paths).await
}

async fn run_with_host_paths(scope: Scope, host_paths: HostPaths) -> anyhow::Result<()> {
    host_paths.ensure_root()?;
    let sock_path = host_paths.socket_path();
    let project_server_dir = scope.server_dir();
    std::fs::create_dir_all(&project_server_dir)?;

    // The host lock is the only writable daemon admission gate.  Project
    // roots still select their own journal/reducer storage, but never another
    // socket or a second host writer.
    let _lock_file = acquire_daemon_lock(&host_paths.lock_path(), &sock_path)?;

    // Check legacy endpoints only after the current host lock is acquired. If
    // the compatibility fixture uses the legacy path as its host path, the
    // normal duplicate-daemon error remains authoritative; a real host-path
    // migration still reaches this fence because its lock is distinct.
    ensure_legacy_writer_absent(&scope, &host_paths)?;

    prepare_socket_path(&sock_path)?;

    let state = replay(&scope.root)?;
    let journal_file = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(project_server_dir.join("journal.jsonl"))?;

    append_log(&host_paths.log_path(), "server starting");

    let server = Arc::new(Server {
        config: crate::config::load(&scope.root)?,
        root: scope.root.clone(),
        state: Mutex::new(state),
        journal: Mutex::new(journal_file),
        pane_alive_check: pane_presence,
        pane_owner_check: pane_owner_authoritative,
        pane_state_check: knock::probe_agent_state,
        mailbox_notify: Notify::new(),
    });
    restore_registered_peer_default_leases(&server);
    purge_expired_storage(&server, now_ms());
    let listener = UnixListener::bind(&sock_path)?;
    let socket_metadata = std::fs::symlink_metadata(&sock_path)?;
    use std::os::unix::fs::PermissionsExt;
    if let Err(error) = std::fs::set_permissions(&sock_path, std::fs::Permissions::from_mode(0o600))
    {
        let cleanup = remove_listener_socket(&sock_path, &socket_metadata);
        return match cleanup {
            Ok(()) => Err(error.into()),
            Err(cleanup_error) => Err(anyhow::Error::new(error).context(format!(
                "failed to clean up startup socket: {cleanup_error}"
            ))),
        };
    }
    std::fs::write(host_paths.pid_path(), std::process::id().to_string()).map_err(|error| {
        match remove_listener_socket(&sock_path, &socket_metadata) {
            Ok(()) => anyhow::Error::new(error),
            Err(cleanup_error) => anyhow::Error::new(error).context(format!(
                "failed to clean up startup socket: {cleanup_error}"
            )),
        }
    })?;
    let _ = record_activity(
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
            let s = sched.clone();
            tokio::task::spawn_blocking(move || crate::server::timers::tick(&s))
                .await
                .ok();
        }
    });

    loop {
        match listener.accept().await {
            Ok((stream, _)) => {
                let srv = server.clone();
                tokio::spawn(conn_task(srv, stream));
            }
            Err(e) => append_log(&host_paths.log_path(), &format!("accept error: {}", e)),
        }
    }
}

#[cfg(test)]
mod reducer_binding_tests {
    use super::*;

    #[test]
    fn task_register_wires_worktree_binding_and_replays_it() {
        static SEQUENCE: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
        let root = std::path::PathBuf::from(format!(
            "/tmp/collab-r2-binding-{}-{}",
            std::process::id(),
            SEQUENCE.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
        ));
        let server_dir = root.join(".agent-collab/server");
        std::fs::create_dir_all(&server_dir).unwrap();
        let journal = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(server_dir.join("journal.jsonl"))
            .unwrap();
        let server = Server {
            config: crate::config::Config::default(),
            root: root.clone(),
            state: Mutex::new(State::default()),
            journal: Mutex::new(journal),
            pane_alive_check: |_| PanePresence::Present,
            pane_owner_check: |_, _| Ok(true),
            pane_state_check: |_| crate::server::knock::AgentState::Waiting,
            mailbox_notify: tokio::sync::Notify::new(),
        };
        peer_tests::register(&server, "worker", "%worker");
        let worktree = "playground/task-1".to_string();
        let registered = handle_task_register(
            &server,
            "worker".into(),
            "token-worker".into(),
            "task-1".into(),
            None,
            None,
            Some(worktree.clone()),
            Some("feature/branch".into()),
            Some("abc123".into()),
            "p2".into(),
        );
        assert!(registered.ok, "{registered:?}");
        let canonical_worktree = server
            .root
            .canonicalize()
            .unwrap()
            .join("playground/task-1")
            .to_string_lossy()
            .into_owned();
        {
            let state = server.state.lock().unwrap();
            assert_eq!(
                state.worktree_bindings["binding-task-task-1"].task_id,
                "task-1"
            );
            assert_eq!(
                state.worktree_bindings["binding-task-task-1"].worktree_root,
                canonical_worktree
            );
        }
        let replayed = replay(&root).unwrap();
        assert_eq!(
            replayed.worktree_bindings["binding-task-task-1"].task_id,
            "task-1"
        );
        assert_eq!(
            replayed.worktree_bindings["binding-task-task-1"].worktree_root,
            canonical_worktree
        );
        std::fs::remove_dir_all(root).unwrap();
    }
}

#[cfg(test)]
mod startup_tests {
    use super::*;
    use std::os::unix::net::UnixListener;
    use std::path::PathBuf;
    use std::sync::{Mutex, MutexGuard, OnceLock};
    use std::time::Duration;

    static STARTUP_TEST_LOCK: OnceLock<Mutex<()>> = OnceLock::new();

    fn startup_test_lock() -> MutexGuard<'static, ()> {
        STARTUP_TEST_LOCK
            .get_or_init(|| Mutex::new(()))
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }

    fn test_root(name: &str) -> PathBuf {
        let root = PathBuf::from(format!(
            "/tmp/collab-startup-{name}-{}-{}",
            std::process::id(),
            now_ms()
        ));
        std::fs::create_dir_all(root.join(".agent-collab/server"))
            .expect("create startup test root");
        root
    }

    #[tokio::test]
    async fn pid_publication_failure_removes_the_owned_socket() {
        let _startup_test_lock = startup_test_lock();
        let root = test_root("pid-failure");
        let host_paths = HostPaths::for_state_root(root.join("host-state")).unwrap();
        host_paths.ensure_root().unwrap();
        std::fs::create_dir(host_paths.pid_path()).expect("occupy pid path");

        let error = run_with_host_paths(Scope { root: root.clone() }, host_paths.clone())
            .await
            .expect_err("a directory at server.pid must fail startup");
        assert!(error.to_string().contains("directory"), "{error:#}");
        assert!(
            !host_paths.socket_path().exists(),
            "startup failure must remove the socket it just published"
        );

        std::fs::remove_dir_all(root).expect("remove startup test root");
    }

    #[tokio::test]
    async fn startup_rejects_a_reachable_legacy_project_daemon() {
        let _startup_test_lock = startup_test_lock();
        let root = test_root("legacy-socket");
        let host_paths = HostPaths::for_state_root(root.join("host-state")).unwrap();
        host_paths.ensure_root().unwrap();
        let legacy_socket = root.join(".agent-collab/server/server.sock");
        let listener = UnixListener::bind(&legacy_socket).expect("bind legacy socket");

        let error = run_with_host_paths(Scope { root: root.clone() }, host_paths.clone())
            .await
            .expect_err("a reachable legacy daemon must be fenced");
        assert!(error.to_string().contains("DAEMON_MIGRATION_REQUIRED"));
        assert!(error.to_string().contains("legacy project daemon"));
        assert!(!host_paths.socket_path().exists());
        assert!(legacy_socket.exists());

        drop(listener);
        std::fs::remove_dir_all(root).expect("remove startup test root");
    }

    #[tokio::test]
    async fn startup_rejects_a_held_legacy_project_lock() {
        let _startup_test_lock = startup_test_lock();
        let root = test_root("legacy-lock");
        let host_paths = HostPaths::for_state_root(root.join("host-state")).unwrap();
        host_paths.ensure_root().unwrap();
        let legacy_lock = root.join(".agent-collab/server/daemon.lock");
        let lock_file = std::fs::OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .open(&legacy_lock)
            .expect("open legacy lock");
        use std::os::unix::io::AsRawFd;
        let rc = unsafe { libc::flock(lock_file.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) };
        assert_eq!(rc, 0, "test must hold the legacy lock");

        let error = run_with_host_paths(Scope { root: root.clone() }, host_paths.clone())
            .await
            .expect_err("a held legacy writer lock must be fenced");
        assert!(error.to_string().contains("DAEMON_MIGRATION_REQUIRED"));
        assert!(error.to_string().contains("legacy project daemon lock"));
        assert!(!host_paths.socket_path().exists());

        drop(lock_file);
        std::fs::remove_dir_all(root).expect("remove startup test root");
    }

    #[test]
    fn cleanup_preserves_a_replacement_socket_path() {
        let root = test_root("replacement");
        let host_paths = HostPaths::for_state_root(root.join("host-state")).unwrap();
        host_paths.ensure_root().unwrap();
        let socket = host_paths.socket_path();
        let original = UnixListener::bind(&socket).expect("bind original socket");
        let captured = std::fs::symlink_metadata(&socket).expect("capture socket metadata");
        drop(original);
        std::fs::remove_file(&socket).expect("remove original socket path");
        let replacement = UnixListener::bind(&socket).expect("bind replacement socket");
        let current = std::fs::symlink_metadata(&socket).expect("read replacement metadata");
        assert!(!same_inode(&captured, &current));

        remove_listener_socket(&socket, &captured).expect("replacement cleanup check");
        assert!(
            socket.exists(),
            "cleanup must not unlink a replacement socket"
        );

        drop(replacement);
        std::fs::remove_file(&socket).ok();
        std::fs::remove_dir_all(root).expect("remove startup test root");
    }

    #[tokio::test]
    async fn retry_after_pid_failure_succeeds_once_the_path_is_fixed() {
        let _startup_test_lock = startup_test_lock();
        let root = test_root("retry");
        let host_paths = HostPaths::for_state_root(root.join("host-state")).unwrap();
        host_paths.ensure_root().unwrap();
        let pid_path = host_paths.pid_path();
        std::fs::create_dir(&pid_path).expect("occupy pid path");
        let first_error = run_with_host_paths(Scope { root: root.clone() }, host_paths.clone())
            .await
            .expect_err("first startup must fail");
        assert!(first_error.to_string().contains("directory"));
        assert!(!host_paths.socket_path().exists());
        std::fs::remove_dir(&pid_path).expect("remove pid directory");

        let socket = host_paths.socket_path();
        let running = tokio::spawn(run_with_host_paths(
            Scope { root: root.clone() },
            host_paths.clone(),
        ));
        let deadline = tokio::time::Instant::now() + Duration::from_secs(5);
        while tokio::time::Instant::now() < deadline {
            let socket = socket.clone();
            let status = tokio::task::spawn_blocking(move || crate::client::daemon_status(&socket))
                .await
                .expect("readiness probe task must complete");
            if status == crate::client::DaemonAvailability::Alive {
                break;
            }
            tokio::time::sleep(Duration::from_millis(25)).await;
        }
        let socket = socket.clone();
        let status = tokio::task::spawn_blocking(move || crate::client::daemon_status(&socket))
            .await
            .expect("readiness probe task must complete");
        assert_eq!(status, crate::client::DaemonAvailability::Alive);
        assert!(pid_path.is_file());

        running.abort();
        let _ = running.await;
        std::fs::remove_dir_all(root).expect("remove startup test root");
    }
}

#[cfg(test)]
pub(crate) mod peer_tests;

#[cfg(test)]
mod ownership_probe_tests {
    use super::*;

    #[test]
    fn unknown_worker_status_does_not_claim_lost_or_advise_cleanup() {
        for failure in ["presence", "ownership", "agent"] {
            let (mut server, root) = peer_tests::test_server();
            peer_tests::register(&server, "worker", "%worker");
            match failure {
                "presence" => server.pane_alive_check = |_| PanePresence::Unknown,
                "ownership" => server.pane_owner_check = |_, _| Err(()),
                _ => server.pane_state_check = |_| knock::AgentState::Unknown,
            }
            let mut state = server.state.lock().unwrap();
            state.keepalives.insert(
                "worker".into(),
                keepalive::Record {
                    suspected_offline: true,
                    ..Default::default()
                },
            );
            let view = worker_status_summary_with_maps(
                &server,
                &state.tasks,
                &state.msgs,
                &state.keepalives,
                &state.workers["worker"],
            );
            assert_eq!(view["status"], "unknown", "{failure}: {view}");
            assert!(view["diagnostic"].is_null(), "{failure}: {view}");
            if failure == "presence" {
                assert_eq!(view["presence"], "unknown");
                assert!(view["endpoint_live"].is_null());
            }
            drop(state);
            std::fs::remove_dir_all(root).unwrap();
        }
    }

    #[test]
    fn unknown_master_blocks_promotion_and_delegation_without_reassignment() {
        for failure in ["presence", "ownership"] {
            let (mut server, root) = peer_tests::test_server();
            peer_tests::register(&server, "master", "%master");
            peer_tests::register(&server, "peer", "%peer");
            assert!(
                handle_master_promote(
                    &server,
                    "master".into(),
                    "token-master".into(),
                    "approved".into()
                )
                .ok
            );
            match failure {
                "presence" => {
                    server.pane_alive_check = |pane| {
                        if pane == "%master" {
                            PanePresence::Unknown
                        } else {
                            PanePresence::Present
                        }
                    }
                }
                _ => {
                    server.pane_owner_check = |worker, _| {
                        if worker == "master" {
                            Err(())
                        } else {
                            Ok(true)
                        }
                    }
                }
            }
            let promoted = handle_master_promote(
                &server,
                "peer".into(),
                "token-peer".into(),
                "approved".into(),
            );
            assert!(!promoted.ok, "{failure}: {promoted:?}");
            assert!(promoted.error.unwrap().contains("unknown"));
            let delegated = handle_master_delegate(
                &server,
                "master".into(),
                "token-master".into(),
                "peer".into(),
            );
            assert!(!delegated.ok);
            assert!(delegated.error.unwrap().contains("unknown"));
            let status = handle_master_status(&server);
            assert!(!status.ok);
            assert_eq!(status.data["status"], "unknown");
            assert!(status.data.get("recorded_unusable").is_none());
            assert_eq!(
                server.state.lock().unwrap().master_worker_id.as_deref(),
                Some("master")
            );
            std::fs::remove_dir_all(root).unwrap();
        }
    }

    #[test]
    fn unknown_candidate_identity_blocks_promotion_and_delegation_explicitly() {
        for failure in ["presence", "ownership"] {
            let (mut server, root) = peer_tests::test_server();
            peer_tests::register(&server, "master", "%master");
            peer_tests::register(&server, "peer", "%peer");
            match failure {
                "presence" => {
                    server.pane_alive_check = |pane| {
                        if pane == "%peer" {
                            PanePresence::Unknown
                        } else {
                            PanePresence::Present
                        }
                    }
                }
                _ => {
                    server.pane_owner_check =
                        |worker, _| if worker == "peer" { Err(()) } else { Ok(true) }
                }
            }
            let promoted = handle_master_promote(
                &server,
                "peer".into(),
                "token-peer".into(),
                "approved".into(),
            );
            assert!(!promoted.ok, "{failure}: {promoted:?}");
            assert!(promoted.error.unwrap().contains("unknown"));
            assert!(server.state.lock().unwrap().master_worker_id.is_none());
            assert!(
                handle_master_promote(
                    &server,
                    "master".into(),
                    "token-master".into(),
                    "approved".into()
                )
                .ok
            );
            let delegated = handle_master_delegate(
                &server,
                "master".into(),
                "token-master".into(),
                "peer".into(),
            );
            assert!(!delegated.ok, "{failure}: {delegated:?}");
            assert!(delegated.error.unwrap().contains("unknown"));
            assert_eq!(
                server.state.lock().unwrap().master_worker_id.as_deref(),
                Some("master")
            );
            std::fs::remove_dir_all(root).unwrap();
        }
    }

    #[test]
    fn ownership_command_errors_are_unknown_and_success_is_parsed_strictly() {
        assert_eq!(
            tmux_session_for_pane_with("%42", &|_| Command::new("/dev/null/collab-missing-tmux")
                .output()),
            Err(())
        );
        assert_eq!(
            tmux_session_for_pane_with("%42", &|_| Command::new("/bin/sh")
                .args(["-c", "exit 7"])
                .output()),
            Err(())
        );
        assert_eq!(
            tmux_session_for_pane_with("%42", &|_| Command::new("/bin/sh")
                .args(["-c", "printf ''"])
                .output()),
            Err(())
        );
        assert_eq!(
            tmux_session_for_pane_with("%42", &|_| Command::new("/bin/sh")
                .args(["-c", "printf 'one\\ntwo\\n'"])
                .output()),
            Err(())
        );
        assert_eq!(
            tmux_session_for_pane_with("%42", &|_| Command::new("/bin/sh")
                .args(["-c", "printf 'worker\\n'"])
                .output()),
            Ok("worker".into())
        );
    }
}

#[cfg(test)]
mod scheduler_admission_tests {
    use super::*;
    use crate::server::peer_tests::{register, test_server};
    #[cfg(unix)]
    use std::os::unix::fs::PermissionsExt;
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::sync::{Arc, Barrier};
    use std::thread;
    use std::time::{Duration, Instant};

    static PROBE_STARTED: AtomicBool = AtomicBool::new(false);
    static PROBE_RELEASE: AtomicBool = AtomicBool::new(false);

    fn blocking_pane_probe(_pane: &str) -> PanePresence {
        PROBE_STARTED.store(true, Ordering::Release);
        while !PROBE_RELEASE.load(Ordering::Acquire) {
            thread::yield_now();
        }
        PanePresence::Present
    }

    #[test]
    fn start_admits_registered_idle_peer_before_managed_child_without_duplicates() {
        let (server, root) = test_server();
        register(&server, "master", "%master");
        register(&server, "idle-peer", "%idle-peer");
        server.commit(&[Event::MasterAssigned {
            worker_id: "master".into(),
            assigned_by: "operator".into(),
            approval: Some("scheduler test".into()),
            assigned_ms: now_ms(),
        }]);
        let server = Arc::new(server);

        let start = |id: &str, token: &str| {
            dispatch(
                &server,
                Req::Subagent {
                    worker_id: "master".into(),
                    token: token.into(),
                    command: crate::subagent::Action::Start {
                        id: Some(id.into()),
                        runtime: Some("cursor".into()),
                    },
                    launch_env: Default::default(),
                },
            )
        };
        let first = start("unneeded-child", "token-master");
        assert!(first.ok, "{first:?}");
        assert_eq!(first.data["admission"]["decision"], "use-registered-peer");
        assert_eq!(first.data["admission"]["worker_id"], "idle-peer");
        let second = start("unneeded-child-2", "token-master");
        assert!(second.ok, "{second:?}");
        assert_eq!(second.data["admission"], first.data["admission"]);
        let direct = crate::subagent::handle_with_env(
            &server,
            "master",
            "token-master",
            crate::subagent::Action::Start {
                id: Some("direct-child".into()),
                runtime: Some("cursor".into()),
            },
            Default::default(),
        );
        assert!(direct.ok, "{direct:?}");
        assert_eq!(direct.data["admission"], first.data["admission"]);
        let state = server.state.lock().unwrap();
        assert!(state.subagents.is_empty());
        assert!(state.tasks.is_empty());
        assert!(state.msgs.is_empty());
        drop(state);
        assert_eq!(
            std::fs::read_to_string(root.join(".agent-collab/server/events.jsonl"))
                .unwrap()
                .matches("scheduler_admission")
                .count(),
            3
        );

        let denied = start("denied-child", "wrong-token");
        assert!(!denied.ok);
        assert!(denied.error.unwrap().contains("authentication failed"));
        let state = server.state.lock().unwrap();
        assert!(state.subagents.is_empty());
        assert!(state.tasks.is_empty());
        assert!(state.msgs.is_empty());
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn admission_excludes_active_unknown_and_managed_capacity() {
        let (mut server, root) = test_server();
        register(&server, "master", "%master");
        register(&server, "peer", "%peer");
        server.commit(&[Event::TaskCreated {
            task: TaskRec {
                id: "active-peer-task".into(),
                owner: "peer".into(),
                created_by: "master".into(),
                feature_id: None,
                worktree_path: None,
                branch: None,
                base_commit: None,
                priority: "p1".into(),
                status: "assigned".into(),
                next_step: None,
                wait: None,
                created_ms: now_ms(),
                updated_ms: now_ms(),
            },
        }]);
        assert!(registered_idle_peer_for_admission(&server, "master").is_none());
        server.pane_alive_check = |_| PanePresence::Unknown;
        assert!(registered_idle_peer_for_admission(&server, "master").is_none());
        std::fs::remove_dir_all(root).unwrap();

        let (server, root) = test_server();
        register(&server, "master", "%master");
        register(&server, "managed-peer", "%managed-peer");
        server.commit(&[Event::SubagentUpdated {
            subagent: crate::subagent::Record {
                id: "existing-child".into(),
                parent: "master".into(),
                peer: "managed-peer".into(),
                status: "idle".into(),
                session: None,
                pane: Some("%managed-peer".into()),
                profile: None,
                created_ms: now_ms(),
                ready_deadline_ms: 0,
                last_message: None,
                error: None,
                probe_failures: Vec::new(),
                runtime: Some("cursor".into()),
            },
        }]);
        assert!(registered_idle_peer_for_admission(&server, "master").is_none());
        assert_eq!(
            idle_managed_subagent_for_admission(&server, "master")
                .map(|(id, _, _)| id)
                .as_deref(),
            Some("existing-child")
        );
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn managed_idle_capacity_is_reused_and_existing_start_semantics_are_preserved() {
        let (server, root) = test_server();
        register(&server, "master", "%master");
        register(&server, "managed-peer", "%managed-peer");
        server.commit(&[
            Event::MasterAssigned {
                worker_id: "master".into(),
                assigned_by: "operator".into(),
                approval: Some("scheduler test".into()),
                assigned_ms: now_ms(),
            },
            Event::SubagentUpdated {
                subagent: crate::subagent::Record {
                    id: "existing-child".into(),
                    parent: "master".into(),
                    peer: "managed-peer".into(),
                    status: "idle".into(),
                    session: Some("$managed-peer".into()),
                    pane: Some("%managed-peer".into()),
                    profile: None,
                    created_ms: now_ms(),
                    ready_deadline_ms: 0,
                    last_message: None,
                    error: None,
                    probe_failures: Vec::new(),
                    runtime: Some("cursor".into()),
                },
            },
        ]);
        let server = Arc::new(server);
        let reused = dispatch(
            &server,
            Req::Subagent {
                worker_id: "master".into(),
                token: "token-master".into(),
                command: crate::subagent::Action::Start {
                    id: Some("new-child".into()),
                    runtime: Some("cursor".into()),
                },
                launch_env: Default::default(),
            },
        );
        assert!(reused.ok, "{reused:?}");
        assert_eq!(
            reused.data["admission"]["decision"],
            "reuse-idle-managed-subagent"
        );
        assert_eq!(reused.data["managed_subagent"]["id"], "existing-child");

        let existing = crate::subagent::handle_with_env(
            &server,
            "master",
            "token-master",
            crate::subagent::Action::Start {
                id: Some("existing-child".into()),
                runtime: Some("codex".into()),
            },
            Default::default(),
        );
        assert!(existing.ok, "{existing:?}");
        assert_eq!(existing.data["reused"], true);

        let invalid_id = crate::subagent::handle_with_env(
            &server,
            "master",
            "token-master",
            crate::subagent::Action::Start {
                id: Some("invalid id".into()),
                runtime: Some("cursor".into()),
            },
            Default::default(),
        );
        assert!(!invalid_id.ok);
        assert!(invalid_id.error.unwrap().contains("invalid subagent ID"));
        let invalid_runtime = crate::subagent::handle_with_env(
            &server,
            "master",
            "token-master",
            crate::subagent::Action::Start {
                id: Some("existing-child".into()),
                runtime: Some("unknown".into()),
            },
            Default::default(),
        );
        assert!(!invalid_runtime.ok);
        assert!(invalid_runtime
            .error
            .unwrap()
            .contains("runtime must be cursor or codex"));
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn admission_probes_do_not_hold_state_lock() {
        PROBE_STARTED.store(false, Ordering::Release);
        PROBE_RELEASE.store(false, Ordering::Release);
        let (mut server, root) = test_server();
        register(&server, "master", "%master");
        register(&server, "idle-peer", "%idle-peer");
        server.pane_alive_check = blocking_pane_probe;
        let server = Arc::new(server);
        let probe_server = server.clone();
        let join =
            thread::spawn(move || registered_idle_peer_for_admission(&probe_server, "master"));

        let deadline = Instant::now() + Duration::from_secs(1);
        while !PROBE_STARTED.load(Ordering::Acquire) && Instant::now() < deadline {
            thread::yield_now();
        }
        let lock_available = server.state.try_lock().is_ok();
        PROBE_RELEASE.store(true, Ordering::Release);
        let selected = join.join().unwrap();

        assert!(PROBE_STARTED.load(Ordering::Acquire));
        assert!(lock_available, "pane probe held the scheduler state lock");
        assert_eq!(selected.map(|(id, _)| id).as_deref(), Some("idle-peer"));
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn admission_audit_failure_is_explicit_and_does_not_create_child() {
        let (server, root) = test_server();
        register(&server, "master", "%master");
        register(&server, "idle-peer", "%idle-peer");
        server.commit(&[Event::MasterAssigned {
            worker_id: "master".into(),
            assigned_by: "operator".into(),
            approval: Some("scheduler test".into()),
            assigned_ms: now_ms(),
        }]);
        let events = root.join(".agent-collab/server/events.jsonl");
        std::fs::create_dir(&events).unwrap();
        let server = Arc::new(server);
        let result = dispatch(
            &server,
            Req::Subagent {
                worker_id: "master".into(),
                token: "token-master".into(),
                command: crate::subagent::Action::Start {
                    id: Some("audit-failure-child".into()),
                    runtime: Some("cursor".into()),
                },
                launch_env: Default::default(),
            },
        );
        assert!(!result.ok);
        assert!(result
            .error
            .unwrap()
            .contains("scheduler admission audit failed"));
        assert!(server.state.lock().unwrap().subagents.is_empty());
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn no_capacity_records_explicit_create_admission() {
        let (server, root) = test_server();
        register(&server, "master", "%master");
        server.commit(&[Event::MasterAssigned {
            worker_id: "master".into(),
            assigned_by: "operator".into(),
            approval: Some("scheduler test".into()),
            assigned_ms: now_ms(),
        }]);
        let decision = scheduler_admit_subagent_start(
            &server,
            "master",
            "token-master",
            Some("new-child"),
            Some("cursor"),
        )
        .unwrap();
        assert!(decision.is_none());
        let audit =
            std::fs::read_to_string(root.join(".agent-collab/server/events.jsonl")).unwrap();
        assert!(audit.contains("create-managed-subagent"));
        assert!(audit.contains("no eligible live registered peer"));
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn scheduler_dispatch_assigns_ordinary_peer_and_deduplicates_request() {
        let (server, root) = test_server();
        register(&server, "master", "%master");
        register(&server, "peer", "%peer");
        server.commit(&[Event::MasterAssigned {
            worker_id: "master".into(),
            assigned_by: "operator".into(),
            approval: Some("scheduler test".into()),
            assigned_ms: now_ms(),
        }]);
        let server = Arc::new(server);
        let dispatch_request = |subject: &str, body: &str| {
            dispatch(
                &server,
                Req::Subagent {
                    worker_id: "master".into(),
                    token: "token-master".into(),
                    command: crate::subagent::Action::Dispatch {
                        request_id: "req-ordinary-1".into(),
                        subject: subject.into(),
                        body: body.into(),
                        feature_id: Some("feature-1".into()),
                        worktree_path: None,
                        branch: None,
                        base_commit: None,
                        priority: "p1".into(),
                        next_step: None,
                    },
                    launch_env: Default::default(),
                },
            )
        };
        assert_eq!(
            registered_idle_peer_for_admission(&server, "master")
                .map(|(id, _)| id)
                .as_deref(),
            Some("peer")
        );
        let first = dispatch_request("Implement feature", "Do the work");
        assert!(first.ok, "{first:?}");
        assert_eq!(first.data["decision"], "use-registered-peer");
        assert_eq!(first.data["target"], "peer");
        assert_eq!(first.data["status"], "assigned");
        let second = dispatch_request("different retry text", "ignored by request key");
        assert!(second.ok, "{second:?}");
        assert_eq!(second.data["decision"], "deduplicated");
        assert_eq!(second.data["task_id"], first.data["task_id"]);
        assert_eq!(second.data["message_id"], first.data["message_id"]);
        let state = server.state.lock().unwrap();
        assert_eq!(state.tasks.len(), 1);
        assert_eq!(state.msgs.len(), 1);
        assert_eq!(state.tasks["task-scheduler-req-ordinary-1"].owner, "peer");
        let message_id = first.data["message_id"].as_str().unwrap();
        let subscription_id = state.wake_bindings.get(message_id).unwrap();
        assert_eq!(state.delivery_modes[message_id], "explicit-notification");
        assert_eq!(
            state.notification_subscriptions[subscription_id].worker_id,
            "peer"
        );
        assert_eq!(
            state.notification_subscriptions[subscription_id].event,
            "direct-message"
        );
        drop(state);
        let rejected_update = dispatch(
            &server,
            Req::TaskUpdate {
                worker_id: "peer".into(),
                token: "token-peer".into(),
                task_id: "task-scheduler-req-ordinary-1".into(),
                status: Some("working".into()),
                next_step: None,
            },
        );
        assert!(!rejected_update.ok, "{rejected_update:?}");
        assert!(rejected_update.error.unwrap().contains("task accept"));
        let accepted = dispatch(
            &server,
            Req::TaskAccept {
                worker_id: "peer".into(),
                token: "token-peer".into(),
                task_id: "task-scheduler-req-ordinary-1".into(),
            },
        );
        assert!(accepted.ok, "{accepted:?}");
        assert_eq!(accepted.data["status"], "working");
        let retry = dispatch(
            &server,
            Req::TaskAccept {
                worker_id: "peer".into(),
                token: "token-peer".into(),
                task_id: "task-scheduler-req-ordinary-1".into(),
            },
        );
        assert!(retry.ok, "{retry:?}");
        assert_eq!(retry.data["idempotent"], true);
        let replayed = replay(&root).unwrap();
        assert_eq!(
            replayed.tasks["task-scheduler-req-ordinary-1"].status,
            "working"
        );
        assert_eq!(
            replayed.scheduler_admissions["req-ordinary-1"].status,
            "succeeded"
        );
        let replayed_subscription_id = replayed
            .wake_bindings
            .get(first.data["message_id"].as_str().unwrap())
            .unwrap();
        assert_eq!(
            replayed.notification_subscriptions[replayed_subscription_id].worker_id,
            "peer"
        );
        let audit_count = std::fs::read_to_string(root.join(".agent-collab/server/events.jsonl"))
            .unwrap()
            .lines()
            .filter(|line| line.contains("scheduler_admission") && line.contains("req-ordinary-1"))
            .count();
        assert_eq!(audit_count, 1);
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn scheduler_dispatch_reuses_managed_child_and_deduplicates_request() {
        let (server, root) = test_server();
        register(&server, "master", "%master");
        register(&server, "managed-peer", "%managed-peer");
        server.commit(&[
            Event::MasterAssigned {
                worker_id: "master".into(),
                assigned_by: "operator".into(),
                approval: Some("scheduler test".into()),
                assigned_ms: now_ms(),
            },
            Event::SubagentUpdated {
                subagent: crate::subagent::Record {
                    id: "managed-child".into(),
                    parent: "master".into(),
                    peer: "managed-peer".into(),
                    status: "idle".into(),
                    session: Some("$managed-peer".into()),
                    pane: Some("%managed-peer".into()),
                    profile: None,
                    created_ms: now_ms(),
                    ready_deadline_ms: 0,
                    last_message: None,
                    error: None,
                    probe_failures: Vec::new(),
                    runtime: Some("cursor".into()),
                },
            },
        ]);
        let server = Arc::new(server);
        let request = || {
            dispatch(
                &server,
                Req::Subagent {
                    worker_id: "master".into(),
                    token: "token-master".into(),
                    command: crate::subagent::Action::Dispatch {
                        request_id: "req-managed-1".into(),
                        subject: "Managed task".into(),
                        body: "Use existing child".into(),
                        feature_id: None,
                        worktree_path: None,
                        branch: None,
                        base_commit: None,
                        priority: "p2".into(),
                        next_step: None,
                    },
                    launch_env: Default::default(),
                },
            )
        };
        let first = request();
        assert!(first.ok, "{first:?}");
        assert_eq!(first.data["decision"], "reuse-idle-managed-subagent");
        assert_eq!(first.data["managed_subagent_id"], "managed-child");
        let second = request();
        assert!(second.ok, "{second:?}");
        assert_eq!(second.data["decision"], "deduplicated");
        let state = server.state.lock().unwrap();
        assert_eq!(state.tasks.len(), 1);
        assert_eq!(state.msgs.len(), 1);
        assert_eq!(state.subagents["managed-child"].status, "assigned");
        drop(state);
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn scheduler_dispatch_audit_failure_is_stable_on_request_retry() {
        let (server, root) = test_server();
        register(&server, "master", "%master");
        register(&server, "peer", "%peer");
        server.commit(&[Event::MasterAssigned {
            worker_id: "master".into(),
            assigned_by: "operator".into(),
            approval: Some("scheduler test".into()),
            assigned_ms: now_ms(),
        }]);
        let activity_path = root.join(".agent-collab/server/events.jsonl");
        std::fs::create_dir_all(activity_path.parent().unwrap()).unwrap();
        std::fs::create_dir(&activity_path).unwrap();
        let server = Arc::new(server);
        let request = |body: &str| {
            dispatch(
                &server,
                Req::Subagent {
                    worker_id: "master".into(),
                    token: "token-master".into(),
                    command: crate::subagent::Action::Dispatch {
                        request_id: "req-audit-failure-1".into(),
                        subject: "Audit failure task".into(),
                        body: body.into(),
                        feature_id: None,
                        worktree_path: None,
                        branch: None,
                        base_commit: None,
                        priority: "p2".into(),
                        next_step: None,
                    },
                    launch_env: Default::default(),
                },
            )
        };
        let first = request("first body");
        assert!(!first.ok, "{first:?}");
        let first_error = first.error.clone().unwrap();
        assert!(first_error.contains("scheduler admission audit failed"));
        assert_eq!(first.data["reservation"], true);
        let state = server.state.lock().unwrap();
        assert_eq!(state.msgs["scheduler-req-audit-failure-1"].state, "pending");
        assert_eq!(
            state.msgs["scheduler-req-audit-failure-1"].wake_attempt_count,
            0
        );
        drop(state);
        *server.state.lock().unwrap() = replay(&root).unwrap();
        let second = request("retry body is ignored");
        assert!(!second.ok, "{second:?}");
        assert_eq!(second.error.as_deref(), Some(first_error.as_str()));
        assert_eq!(second.data["reservation"], true);
        assert_eq!(second.data["message_id"], first.data["message_id"]);
        assert_eq!(second.data["task_id"], first.data["task_id"]);
        let state = server.state.lock().unwrap();
        assert_eq!(state.tasks.len(), 1);
        assert_eq!(state.msgs.len(), 1);
        assert_eq!(
            state.scheduler_admissions["req-audit-failure-1"].status,
            "failed"
        );
        drop(state);
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn scheduler_dispatch_audit_failure_cannot_be_accepted_by_managed_child() {
        let (server, root) = test_server();
        register(&server, "master", "%master");
        register(&server, "managed-peer", "%managed-peer");
        server.commit(&[
            Event::MasterAssigned {
                worker_id: "master".into(),
                assigned_by: "operator".into(),
                approval: Some("scheduler test".into()),
                assigned_ms: now_ms(),
            },
            Event::SubagentUpdated {
                subagent: crate::subagent::Record {
                    id: "managed-child-failed-audit".into(),
                    parent: "master".into(),
                    peer: "managed-peer".into(),
                    status: "idle".into(),
                    session: Some("$managed-peer".into()),
                    pane: Some("%managed-peer".into()),
                    profile: None,
                    created_ms: now_ms(),
                    ready_deadline_ms: 0,
                    last_message: None,
                    error: None,
                    probe_failures: Vec::new(),
                    runtime: Some("cursor".into()),
                },
            },
        ]);
        std::fs::create_dir(root.join(".agent-collab/server/events.jsonl")).unwrap();
        let server = Arc::new(server);
        let dispatch_result = dispatch(
            &server,
            Req::Subagent {
                worker_id: "master".into(),
                token: "token-master".into(),
                command: crate::subagent::Action::Dispatch {
                    request_id: "req-managed-failed-audit-1".into(),
                    subject: "Failed managed task".into(),
                    body: "Must not execute".into(),
                    feature_id: None,
                    worktree_path: None,
                    branch: None,
                    base_commit: None,
                    priority: "p2".into(),
                    next_step: None,
                },
                launch_env: Default::default(),
            },
        );
        assert!(!dispatch_result.ok, "{dispatch_result:?}");
        let working = crate::subagent::handle_with_env(
            &server,
            "managed-peer",
            "token-managed-peer",
            crate::subagent::Action::Working {
                id: "managed-child-failed-audit".into(),
            },
            Default::default(),
        );
        assert!(!working.ok, "{working:?}");
        assert!(working
            .error
            .unwrap_or_default()
            .contains("scheduler assignment admission is failed"));
        let state = server.state.lock().unwrap();
        assert_eq!(
            state.scheduler_admissions["req-managed-failed-audit-1"].status,
            "failed"
        );
        assert_eq!(
            state.subagents["managed-child-failed-audit"].status,
            "assigned"
        );
        assert_eq!(
            state.tasks["task-scheduler-req-managed-failed-audit-1"].status,
            "assigned"
        );
        drop(state);
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn scheduler_dispatch_audit_failure_does_not_wake_long_poll_or_recv() {
        let (server, root) = test_server();
        register(&server, "master", "%master");
        register(&server, "peer", "%peer");
        server.commit(&[Event::MasterAssigned {
            worker_id: "master".into(),
            assigned_by: "operator".into(),
            approval: Some("scheduler test".into()),
            assigned_ms: now_ms(),
        }]);
        std::fs::create_dir(root.join(".agent-collab/server/events.jsonl")).unwrap();
        let server = Arc::new(server);
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        let (poll_result, dispatch_result) = runtime.block_on(async {
            let poll = handle_poll_async(Arc::clone(&server), "peer".into(), 100);
            let dispatch_server = Arc::clone(&server);
            let dispatch = tokio::task::spawn_blocking(move || {
                std::thread::sleep(Duration::from_millis(10));
                dispatch(
                    &dispatch_server,
                    Req::Subagent {
                        worker_id: "master".into(),
                        token: "token-master".into(),
                        command: crate::subagent::Action::Dispatch {
                            request_id: "req-long-poll-failed-audit-1".into(),
                            subject: "Failed long poll task".into(),
                            body: "Must not be consumed".into(),
                            feature_id: None,
                            worktree_path: None,
                            branch: None,
                            base_commit: None,
                            priority: "p2".into(),
                            next_step: None,
                        },
                        launch_env: Default::default(),
                    },
                )
            });
            let (poll_result, dispatch_result) = tokio::join!(poll, dispatch);
            (poll_result, dispatch_result.unwrap())
        });
        assert!(poll_result.ok, "{poll_result:?}");
        assert_eq!(poll_result.data["count"], 0);
        assert_eq!(poll_result.data["timeout"], true);
        assert!(!dispatch_result.ok, "{dispatch_result:?}");
        let state = server.state.lock().unwrap();
        assert_eq!(
            state.msgs["scheduler-req-long-poll-failed-audit-1"].wake_attempt_count,
            0
        );
        assert_eq!(state.inbox_of("peer").len(), 0);
        assert_eq!(
            state.scheduler_admissions["req-long-poll-failed-audit-1"].status,
            "failed"
        );
        drop(state);
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn scheduler_dispatch_success_wakes_long_poll() {
        let (mut server, root) = test_server();
        register(&server, "master", "%master");
        register(&server, "peer", "%peer");
        server.config.notifications.enabled = false;
        server.commit(&[Event::MasterAssigned {
            worker_id: "master".into(),
            assigned_by: "operator".into(),
            approval: Some("scheduler test".into()),
            assigned_ms: now_ms(),
        }]);
        let server = Arc::new(server);
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        let (poll_result, dispatch_result) = runtime.block_on(async {
            let poll = handle_poll_async(Arc::clone(&server), "peer".into(), 1_000);
            let dispatch_server = Arc::clone(&server);
            let dispatch = tokio::task::spawn_blocking(move || {
                std::thread::sleep(Duration::from_millis(10));
                dispatch(
                    &dispatch_server,
                    Req::Subagent {
                        worker_id: "master".into(),
                        token: "token-master".into(),
                        command: crate::subagent::Action::Dispatch {
                            request_id: "req-long-poll-success-1".into(),
                            subject: "Successful long poll task".into(),
                            body: "Must wake recv".into(),
                            feature_id: None,
                            worktree_path: None,
                            branch: None,
                            base_commit: None,
                            priority: "p2".into(),
                            next_step: None,
                        },
                        launch_env: Default::default(),
                    },
                )
            });
            let (poll_result, dispatch_result) = tokio::join!(poll, dispatch);
            (poll_result, dispatch_result.unwrap())
        });
        assert!(poll_result.ok, "{poll_result:?}");
        assert_eq!(poll_result.data["count"], 1);
        assert!(!poll_result.data["timeout"].as_bool().unwrap_or(false));
        assert_eq!(
            poll_result.data["messages"][0]["id"],
            "scheduler-req-long-poll-success-1"
        );
        assert!(dispatch_result.ok, "{dispatch_result:?}");
        let state = server.state.lock().unwrap();
        assert_eq!(
            state.scheduler_admissions["req-long-poll-success-1"].status,
            "succeeded"
        );
        assert_eq!(
            state.msgs["scheduler-req-long-poll-success-1"].state,
            "read"
        );
        drop(state);
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn scheduler_dispatch_recovers_pending_reservation_without_duplicates() {
        // Simulate a process interruption after the reservation and audit write,
        // but before the durable succeeded status commit.
        let (mut server, root) = test_server();
        register(&server, "master", "%master");
        register(&server, "peer", "%peer");
        server.config.notifications.enabled = false;
        server.commit(&[
            Event::MasterAssigned {
                worker_id: "master".into(),
                assigned_by: "operator".into(),
                approval: Some("scheduler test".into()),
                assigned_ms: now_ms(),
            },
            Event::Sent {
                msg: Message {
                    id: "scheduler-req-pending-recovery-1".into(),
                    from: "master".into(),
                    to: "peer".into(),
                    mtype: "notify".into(),
                    subject: Some("Pending recovery".into()),
                    body: "Reuse reservation".into(),
                    in_reply_to: None,
                    created_ms: now_ms(),
                    state: "pending".into(),
                    wake_attempt_count: 0,
                    last_wake_attempt_ms: 0,
                },
            },
            Event::TaskCreated {
                task: TaskRec {
                    id: "task-scheduler-req-pending-recovery-1".into(),
                    owner: "peer".into(),
                    created_by: "master".into(),
                    feature_id: None,
                    worktree_path: None,
                    branch: None,
                    base_commit: None,
                    priority: "p2".into(),
                    status: "assigned".into(),
                    next_step: Some("accept".into()),
                    wait: None,
                    created_ms: now_ms(),
                    updated_ms: now_ms(),
                },
            },
            Event::DeliveryMode {
                msg_id: "scheduler-req-pending-recovery-1".into(),
                mode: "explicit-notification".into(),
            },
            Event::WakeBound {
                message_id: "scheduler-req-pending-recovery-1".into(),
                subscription_id: "sub-default-direct-message-peer".into(),
            },
            Event::SchedulerAdmission {
                admission: crate::server::state::SchedulerAdmissionRecord {
                    request_id: "req-pending-recovery-1".into(),
                    decision: "use-registered-peer".into(),
                    worker_id: "peer".into(),
                    managed_subagent_id: None,
                    message_id: "scheduler-req-pending-recovery-1".into(),
                    task_id: "task-scheduler-req-pending-recovery-1".into(),
                    status: "pending".into(),
                    error: None,
                    created_ms: now_ms(),
                    updated_ms: now_ms(),
                },
            },
        ]);
        record_scheduler_admission(
            &server,
            json!({
                "request_id": "req-pending-recovery-1",
                "decision": "use-registered-peer",
                "worker_id": "peer",
                "message_id": "scheduler-req-pending-recovery-1",
                "task_id": "task-scheduler-req-pending-recovery-1",
                "status": "pending",
            }),
        )
        .unwrap();
        let audit_path = root.join(".agent-collab/server/events.jsonl");
        let original_mode = std::fs::metadata(&audit_path).unwrap().permissions().mode();
        let mut read_only = std::fs::metadata(&audit_path).unwrap().permissions();
        read_only.set_mode(original_mode & !0o222);
        std::fs::set_permissions(&audit_path, read_only).unwrap();
        let server = Arc::new(server);
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        let (poll_result, recovered) = runtime.block_on(async {
            let poll = handle_poll_async(Arc::clone(&server), "peer".into(), 1_000);
            let recovery_server = Arc::clone(&server);
            let recovery = tokio::task::spawn_blocking(move || {
                dispatch(
                    &recovery_server,
                    Req::Subagent {
                        worker_id: "master".into(),
                        token: "token-master".into(),
                        command: crate::subagent::Action::Dispatch {
                            request_id: "req-pending-recovery-1".into(),
                            subject: "Changed subject is ignored".into(),
                            body: "Changed body is ignored".into(),
                            feature_id: None,
                            worktree_path: None,
                            branch: None,
                            base_commit: None,
                            priority: "p2".into(),
                            next_step: None,
                        },
                        launch_env: Default::default(),
                    },
                )
            });
            let (poll_result, recovered) = tokio::join!(poll, recovery);
            (poll_result, recovered.unwrap())
        });
        let mut restored = std::fs::metadata(&audit_path).unwrap().permissions();
        restored.set_mode(original_mode);
        std::fs::set_permissions(&audit_path, restored).unwrap();
        assert!(poll_result.ok, "{poll_result:?}");
        assert_eq!(poll_result.data["count"], 1);
        assert!(!poll_result.data["timeout"].as_bool().unwrap_or(false));
        assert_eq!(
            poll_result.data["messages"][0]["id"],
            "scheduler-req-pending-recovery-1"
        );
        assert!(recovered.ok, "{recovered:?}");
        assert_eq!(recovered.data["recovered"], true);
        assert_eq!(recovered.data["decision"], "use-registered-peer");
        let state = server.state.lock().unwrap();
        assert_eq!(state.msgs.len(), 1);
        assert_eq!(state.tasks.len(), 1);
        assert_eq!(
            state.scheduler_admissions["req-pending-recovery-1"].status,
            "succeeded"
        );
        drop(state);
        let audit_count = std::fs::read_to_string(root.join(".agent-collab/server/events.jsonl"))
            .unwrap()
            .lines()
            .filter(|line| {
                line.contains("scheduler_admission") && line.contains("req-pending-recovery-1")
            })
            .count();
        assert_eq!(audit_count, 1);
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn scheduler_dispatch_concurrent_same_request_reserves_once() {
        let (server, root) = test_server();
        register(&server, "master", "%master");
        register(&server, "peer", "%peer");
        server.commit(&[Event::MasterAssigned {
            worker_id: "master".into(),
            assigned_by: "operator".into(),
            approval: Some("scheduler test".into()),
            assigned_ms: now_ms(),
        }]);
        let server = Arc::new(server);
        let barrier = Arc::new(Barrier::new(2));
        let request = |server: Arc<Server>, barrier: Arc<Barrier>| {
            thread::spawn(move || {
                barrier.wait();
                dispatch(
                    &server,
                    Req::Subagent {
                        worker_id: "master".into(),
                        token: "token-master".into(),
                        command: crate::subagent::Action::Dispatch {
                            request_id: "req-concurrent-1".into(),
                            subject: "Concurrent task".into(),
                            body: "Reserve exactly once".into(),
                            feature_id: None,
                            worktree_path: None,
                            branch: None,
                            base_commit: None,
                            priority: "p2".into(),
                            next_step: None,
                        },
                        launch_env: Default::default(),
                    },
                )
            })
        };
        let first = request(Arc::clone(&server), Arc::clone(&barrier));
        let second = request(Arc::clone(&server), Arc::clone(&barrier));
        let first = first.join().unwrap();
        let second = second.join().unwrap();
        assert!(first.ok, "{first:?}");
        assert!(second.ok, "{second:?}");
        assert_eq!(first.data["task_id"], second.data["task_id"]);
        assert_eq!(first.data["message_id"], second.data["message_id"]);
        assert!(["use-registered-peer", "deduplicated"]
            .contains(&first.data["decision"].as_str().unwrap()));
        assert!(["use-registered-peer", "deduplicated"]
            .contains(&second.data["decision"].as_str().unwrap()));
        let state = server.state.lock().unwrap();
        assert_eq!(state.tasks.len(), 1);
        assert_eq!(state.msgs.len(), 1);
        assert_eq!(
            state.tasks["task-scheduler-req-concurrent-1"].status,
            "assigned"
        );
        assert_eq!(
            state.scheduler_admissions["req-concurrent-1"].status,
            "succeeded"
        );
        drop(state);
        std::fs::remove_dir_all(root).unwrap();
    }
}
