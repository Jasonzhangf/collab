//! Typed producer/consumer boundary for daemon notifications.
//!
//! This module deliberately contains no journal or JSONL writer.  `Server` is
//! the only `NotificationSink` implementation; consumers receive a snapshot
//! through `NotificationStateView` and cannot mutate reducer state.

use super::notification_state::MasterWakeAccumulator;
use super::state::{Event, Message, NotificationSubscription, State};
use super::Server;

#[derive(Debug, Clone, PartialEq)]
pub struct CommitReceipt {
    pub sequence: u64,
    pub revision: u64,
}

#[derive(Debug, Clone, PartialEq)]
pub struct CommandOutcome {
    pub receipt: CommitReceipt,
    pub operation_id: String,
    pub outcome: serde_json::Value,
    pub replayed: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum JournalError {
    Append(String),
    Flush(String),
    Replay(String),
    Reducer(String),
    InvalidCommand(String),
}

impl std::fmt::Display for JournalError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Append(error) => write!(f, "journal append failed: {error}"),
            Self::Flush(error) => write!(f, "journal flush failed: {error}"),
            Self::Replay(error) => write!(f, "journal replay failed: {error}"),
            Self::Reducer(error) => write!(f, "journal reducer failed: {error}"),
            Self::InvalidCommand(error) => write!(f, "invalid command: {error}"),
        }
    }
}

impl std::error::Error for JournalError {}

#[derive(Debug, Clone)]
pub struct NotificationStateView {
    pub revision: u64,
    pub sequence: u64,
    pub master_wake: MasterWakeAccumulator,
    pub messages: Vec<Message>,
    pub subscriptions: Vec<NotificationSubscription>,
}

impl NotificationStateView {
    pub fn from_state(state: &State) -> Self {
        let mut messages = state.msgs.values().cloned().collect::<Vec<_>>();
        messages.sort_by(|a, b| (a.created_ms, &a.id).cmp(&(b.created_ms, &b.id)));
        let mut subscriptions = state
            .notification_subscriptions
            .values()
            .cloned()
            .collect::<Vec<_>>();
        subscriptions.sort_by(|a, b| (&a.created_ms, &a.id).cmp(&(&b.created_ms, &b.id)));
        Self {
            revision: state.revision,
            sequence: state.sequence,
            master_wake: state.master_wake.clone(),
            messages,
            subscriptions,
        }
    }
}

pub trait NotificationStateSource {
    fn notification_state(&self) -> NotificationStateView;
}

pub trait NotificationSink {
    fn submit(&self, events: &[Event]) -> Result<CommitReceipt, JournalError>;
}

impl NotificationStateSource for Server {
    fn notification_state(&self) -> NotificationStateView {
        let state = self.state.lock().unwrap();
        NotificationStateView::from_state(&state)
    }
}

impl NotificationSink for Server {
    fn submit(&self, events: &[Event]) -> Result<CommitReceipt, JournalError> {
        self.commit_checked(events)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::server::state::WorkerRec;
    use std::sync::Mutex;

    fn test_server() -> (Server, std::path::PathBuf) {
        static SEQUENCE: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
        let sequence = SEQUENCE.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        let root = std::env::temp_dir().join(format!(
            "collab-notification-contract-{}-{}-{sequence}",
            std::process::id(),
            crate::server::state::now_ms()
        ));
        let server_dir = root.join(".agent-collab/server");
        std::fs::create_dir_all(&server_dir).unwrap();
        let journal = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(server_dir.join("journal.jsonl"))
            .unwrap();
        (
            Server {
                config: crate::config::Config::default(),
                root: root.clone(),
                state: Mutex::new(State::default()),
                journal: Mutex::new(journal),
                pane_alive_check: |_| crate::server::knock::PanePresence::Present,
                pane_owner_check: |_, _| Ok(true),
                pane_state_check: |_| crate::server::knock::AgentState::Waiting,
                mailbox_notify: tokio::sync::Notify::new(),
            },
            root,
        )
    }

    #[test]
    fn server_notification_sink_commits_and_source_reads_the_same_reducer_state() {
        let (server, root) = test_server();
        let receipt = server
            .submit(&[Event::Registered {
                worker: WorkerRec {
                    id: "worker".into(),
                    token: "token".into(),
                    pane: Some("%worker".into()),
                    cwd: "/project".into(),
                    registered_ms: 1,
                },
            }])
            .unwrap();
        assert_eq!(receipt.sequence, 1);
        assert_eq!(receipt.revision, 1);
        let view = server.notification_state();
        assert_eq!(view.sequence, 1);
        assert_eq!(view.revision, 1);
        assert_eq!(view.messages.len(), 0);
        assert_eq!(server.state.lock().unwrap().workers["worker"].id, "worker");
        assert_eq!(
            std::fs::read_to_string(root.join(".agent-collab/server/journal.jsonl"))
                .unwrap()
                .lines()
                .count(),
            1
        );
        let replayed = crate::server::replay(&root).unwrap();
        assert_eq!(replayed.sequence, 1);
        assert_eq!(replayed.revision, 1);
        assert_eq!(replayed.workers["worker"].id, "worker");
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn command_idempotency_returns_recorded_outcome_and_conflict_fails_closed() {
        let (server, root) = test_server();
        let first = server
            .commit_command(
                "command-1",
                "operation-1",
                &[Event::Registered {
                    worker: WorkerRec {
                        id: "worker".into(),
                        token: "token".into(),
                        pane: Some("%worker".into()),
                        cwd: "/project".into(),
                        registered_ms: 1,
                    },
                }],
                serde_json::json!({"accepted": true}),
            )
            .unwrap();
        assert!(!first.replayed);
        assert_eq!(first.receipt.sequence, 3);
        assert_eq!(first.receipt.revision, 3);

        let retried = server
            .commit_command(
                "command-1",
                "operation-1",
                &[Event::Registered {
                    worker: WorkerRec {
                        id: "duplicate".into(),
                        token: "token".into(),
                        pane: Some("%duplicate".into()),
                        cwd: "/project".into(),
                        registered_ms: 1,
                    },
                }],
                serde_json::json!({"accepted": false, "ignored": true}),
            )
            .unwrap();
        assert!(retried.replayed);
        assert_eq!(retried.outcome, serde_json::json!({"accepted": true}));
        assert_eq!(retried.receipt, first.receipt);
        assert_eq!(server.state.lock().unwrap().workers["worker"].id, "worker");
        assert!(
            server
                .state
                .lock()
                .unwrap()
                .workers
                .get("duplicate")
                .is_none(),
            "idempotent retry must not apply a second event"
        );

        let conflict = server.commit_command(
            "command-1",
            "operation-2",
            &[],
            serde_json::json!({"accepted": true}),
        );
        assert!(matches!(conflict, Err(JournalError::InvalidCommand(_))));
        assert_eq!(
            std::fs::read_to_string(root.join(".agent-collab/server/journal.jsonl"))
                .unwrap()
                .lines()
                .count(),
            3
        );
        std::fs::remove_dir_all(root).unwrap();
    }
}
