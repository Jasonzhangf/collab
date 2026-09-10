use super::*;
use crate::server::notification_contract::JournalError;
use crate::server::state::{default_priority, is_goal_deadline, TaskRec};
use std::process::Command;
use std::sync::atomic::{AtomicU64, Ordering};

pub(crate) fn test_server() -> (Server, PathBuf) {
    static SEQ: AtomicU64 = AtomicU64::new(0);
    let n = SEQ.fetch_add(1, Ordering::Relaxed);
    let root = std::env::temp_dir().join(format!("collab-peer-{}-{n}", std::process::id()));
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
            pane_alive_check: |_| PanePresence::Present,
            pane_owner_check: |_, _| Ok(true),
            pane_state_check: |_| crate::server::knock::AgentState::Waiting,
            mailbox_notify: tokio::sync::Notify::new(),
        },
        root,
    )
}

pub(super) fn register(server: &Server, id: &str, pane: &str) -> Resp {
    handle_register(
        server,
        id.into(),
        format!("token-{id}"),
        Some(pane.into()),
        server.root.display().to_string(),
    )
}

fn send_command(root: &Path, id: &str) -> crate::proto::CommandEnvelope {
    use crate::identity::{AppServerId, BindingId, CommandId, OperationId};
    use crate::proto::CommandEnvelope;
    let app = AppServerId::new("appserver-cli").unwrap();
    let scope = crate::scope::RouteScope::for_registered_project(app, root).unwrap();
    CommandEnvelope::new(
        CommandId::new(format!("command-{id}")).unwrap(),
        OperationId::new(format!("operation-{id}")).unwrap(),
        BindingId::new(format!("binding-{id}")).unwrap(),
        0,
        scope,
        None,
        None,
        None,
        None,
    )
}

fn authenticated_send(root: &Path, from: &str, to: &str, subject: &str) -> Req {
    Req::Send {
        from: from.into(),
        worker_id: Some(from.into()),
        token: Some(format!("token-{from}")),
        command: Some(send_command(root, from)),
        to: to.into(),
        mtype: "notify".into(),
        subject: Some(subject.into()),
        body: "body".into(),
        in_reply_to: None,
        delivery: "immediate".into(),
    }
}

#[test]
fn master_idle_subscription_is_restricted_to_the_live_master_and_supported_intervals() {
    let (server, root) = test_server();
    register(&server, "master", "%master");
    register(&server, "worker", "%worker");
    server.commit(&[Event::MasterAssigned {
        worker_id: "master".into(),
        assigned_by: "operator".into(),
        approval: Some("user-approved".into()),
        assigned_ms: now_ms(),
    }]);

    let worker = handle_notification_subscribe(
        &server,
        "worker".into(),
        "token-worker".into(),
        "master-idle".into(),
        Some("master-idle".into()),
        None,
        Vec::new(),
        Some(900_000),
        3,
        86_400,
    );
    assert!(!worker.ok);
    assert_eq!(
        worker.error.as_deref(),
        Some("master-idle subscription requires the live registered master")
    );

    let accepted = handle_notification_subscribe(
        &server,
        "master".into(),
        "token-master".into(),
        "master-idle".into(),
        Some("master-idle".into()),
        None,
        Vec::new(),
        Some(3_600_000),
        3,
        86_400,
    );
    assert!(accepted.ok, "{}", accepted.error.unwrap_or_default());

    let invalid = handle_notification_subscribe(
        &server,
        "master".into(),
        "token-master".into(),
        "master-idle".into(),
        Some("master-idle".into()),
        None,
        Vec::new(),
        Some(60_000),
        3,
        86_400,
    );
    assert!(!invalid.ok);
    assert_eq!(
        invalid.error.as_deref(),
        Some("master-idle interval must be exactly 900000 or 3600000 ms")
    );
    std::fs::remove_dir_all(root).ok();
}

#[test]
fn cancelling_master_idle_subscription_supersedes_pending_wake() {
    let (server, root) = test_server();
    register(&server, "master", "%master");
    server.commit(&[
        Event::MasterAssigned {
            worker_id: "master".into(),
            assigned_by: "operator".into(),
            approval: Some("user-approved".into()),
            assigned_ms: now_ms(),
        },
        Event::NotificationSubscribed {
            subscription: NotificationSubscription {
                id: "sub-master-idle".into(),
                worker_id: "master".into(),
                event: "master-idle".into(),
                subject: Some("master-idle".into()),
                pane: "%master".into(),
                method: "tmux".into(),
                trigger_ms: Some(now_ms() - 1),
                trigger_times_ms: Vec::new(),
                interval_ms: Some(900_000),
                repeat_count: 3,
                fired_count: 0,
                expires_ms: now_ms() + 86_400_000,
                status: "armed".into(),
                created_ms: now_ms() - 900_000,
                updated_ms: now_ms(),
                status_reason: None,
            },
        },
        Event::Sent {
            msg: Message {
                id: "pending-idle".into(),
                from: "collab-server".into(),
                to: "master".into(),
                mtype: "notification".into(),
                subject: Some("master-idle:master-idle".into()),
                body: "MASTER_IDLE_WAKE scheduling continues".into(),
                in_reply_to: None,
                created_ms: now_ms(),
                state: "pending".into(),
                wake_attempt_count: 0,
                last_wake_attempt_ms: 0,
            },
        },
        Event::WakeBound {
            message_id: "pending-idle".into(),
            subscription_id: "sub-master-idle".into(),
        },
    ]);
    let cancelled = handle_notification_unsubscribe(
        &server,
        "master".into(),
        "token-master".into(),
        "sub-master-idle".into(),
    );
    assert!(cancelled.ok, "{}", cancelled.error.unwrap_or_default());
    let state = server.state.lock().unwrap();
    assert_eq!(
        state.notification_subscriptions["sub-master-idle"].status,
        "cancelled"
    );
    assert_eq!(state.msgs["pending-idle"].state, "superseded");
    drop(state);
    std::fs::remove_dir_all(root).ok();
}

#[test]
fn deadline_subscription_requires_live_master_authority() {
    let (server, root) = test_server();
    register(&server, "worker", "%worker");
    register(&server, "master", "%master");

    let denied = handle_notification_subscribe(
        &server,
        "worker".into(),
        "token-worker".into(),
        "deadline".into(),
        Some("goal:test".into()),
        None,
        vec![now_ms() + 10_000],
        None,
        1,
        60,
    );
    assert!(!denied.ok);
    assert!(denied.error.unwrap().contains("deadline subscriptions"));

    server.commit(&[Event::MasterAssigned {
        worker_id: "master".into(),
        assigned_by: "master".into(),
        approval: Some("user approved test master".into()),
        assigned_ms: now_ms(),
    }]);
    let accepted = handle_notification_subscribe(
        &server,
        "master".into(),
        "token-master".into(),
        "deadline".into(),
        Some("goal:test".into()),
        None,
        vec![now_ms() + 10_000],
        None,
        1,
        60,
    );
    assert!(accepted.ok, "master should be allowed: {accepted:?}");
    std::fs::remove_dir_all(root).ok();
}

#[test]
fn goal_deadline_rejects_periodic_rearm_options() {
    let (server, root) = test_server();
    register(&server, "master", "%master");
    server.commit(&[Event::MasterAssigned {
        worker_id: "master".into(),
        assigned_by: "operator".into(),
        approval: Some("user-approved".into()),
        assigned_ms: now_ms(),
    }]);

    let response = handle_notification_subscribe(
        &server,
        "master".into(),
        "token-master".into(),
        "deadline".into(),
        Some("goal:inactive".into()),
        None,
        Vec::new(),
        Some(600_000),
        100,
        86_400,
    );
    assert!(!response.ok);
    assert_eq!(
        response.error.as_deref(),
        Some("goal deadline subscriptions are one-shot and require one at-ms trigger")
    );
    std::fs::remove_dir_all(root).ok();
}

#[test]
fn goal_deadline_registration_deduplicates_same_deadline() {
    let (server, root) = test_server();
    register(&server, "master", "%master");
    server.commit(&[Event::MasterAssigned {
        worker_id: "master".into(),
        assigned_by: "operator".into(),
        approval: Some("user-approved".into()),
        assigned_ms: now_ms(),
    }]);
    let trigger_ms = now_ms() + 10_000;
    let first = handle_notification_subscribe(
        &server,
        "master".into(),
        "token-master".into(),
        "deadline".into(),
        Some("goal:revision-7".into()),
        Some(trigger_ms),
        Vec::new(),
        None,
        1,
        86_400,
    );
    assert!(first.ok, "{}", first.error.unwrap_or_default());
    let second = handle_notification_subscribe(
        &server,
        "master".into(),
        "token-master".into(),
        "deadline".into(),
        Some("goal:revision-7".into()),
        Some(trigger_ms),
        Vec::new(),
        None,
        1,
        86_400,
    );
    assert!(second.ok, "{}", second.error.unwrap_or_default());
    assert_eq!(second.data["deduplicated"], true);
    assert_eq!(
        second.data["subscription"]["id"],
        first.data["subscription"]["id"]
    );
    assert_eq!(
        server
            .state
            .lock()
            .unwrap()
            .notification_subscriptions
            .values()
            .filter(|subscription| is_goal_deadline(subscription))
            .count(),
        1
    );

    let next_revision = handle_notification_subscribe(
        &server,
        "master".into(),
        "token-master".into(),
        "deadline".into(),
        Some("goal:revision-8".into()),
        Some(trigger_ms),
        Vec::new(),
        None,
        1,
        86_400,
    );
    assert!(
        next_revision.ok,
        "{}",
        next_revision.error.unwrap_or_default()
    );
    assert!(next_revision.data.get("deduplicated").is_none());
    assert_ne!(
        next_revision.data["subscription"]["id"],
        first.data["subscription"]["id"]
    );
    assert_eq!(
        server
            .state
            .lock()
            .unwrap()
            .notification_subscriptions
            .values()
            .filter(|subscription| is_goal_deadline(subscription))
            .count(),
        2
    );
    std::fs::remove_dir_all(root).ok();
}

fn create_task(server: &Server, owner: &str, id: &str, feature: &str) -> Resp {
    handle_task_register(
        server,
        owner.into(),
        format!("token-{owner}"),
        id.into(),
        None,
        Some(feature.into()),
        None,
        None,
        None,
        default_priority(),
    )
}

fn initialize_main(root: &Path) {
    for args in [
        ["init", "-q"].as_slice(),
        ["config", "user.email", "test@example.com"].as_slice(),
        ["config", "user.name", "Collab Test"].as_slice(),
        ["commit", "--allow-empty", "-q", "-m", "main"].as_slice(),
        ["branch", "-M", "main"].as_slice(),
    ] {
        assert!(Command::new("git")
            .current_dir(root)
            .args(args)
            .status()
            .unwrap()
            .success());
    }
}

fn current_head(root: &Path) -> String {
    String::from_utf8(
        Command::new("git")
            .current_dir(root)
            .args(["rev-parse", "HEAD"])
            .output()
            .unwrap()
            .stdout,
    )
    .unwrap()
    .trim()
    .to_owned()
}

#[test]
fn failed_journal_cannot_apply_a_keepalive_reservation() {
    let (server, root) = test_server();
    *server.journal.lock().unwrap() =
        std::fs::File::open(root.join(".agent-collab/server/journal.jsonl")).unwrap();
    let mut state = State::default();
    let result = server.commit_locked_checked(
        &mut state,
        &[Event::KeepaliveUpdated {
            worker_id: "worker".into(),
            record: crate::server::keepalive::Record::default(),
        }],
    );
    assert!(result.is_err());
    assert!(state.keepalives.is_empty());
    std::fs::remove_dir_all(root).unwrap();
}

#[test]
fn command_retry_returns_original_outcome_without_reapplying_events() {
    let (server, root) = test_server();
    let event = Event::KeepaliveUpdated {
        worker_id: "worker".into(),
        record: crate::server::keepalive::Record::default(),
    };
    let first = server
        .commit_command(
            "command-1",
            "operation-1",
            std::slice::from_ref(&event),
            json!({"accepted": true}),
        )
        .unwrap();
    assert!(!first.replayed);
    let second = server
        .commit_command(
            "command-1",
            "operation-1",
            &[event],
            json!({"accepted": false}),
        )
        .unwrap();
    assert!(second.replayed);
    assert_eq!(first.receipt, second.receipt);
    assert_eq!(first.operation_id, second.operation_id);
    assert_eq!(second.outcome, json!({"accepted": true}));
    assert!(server
        .state
        .lock()
        .unwrap()
        .global
        .command_receipts
        .contains_key("command-1"));
    assert_eq!(
        std::fs::read_to_string(root.join(".agent-collab/server/journal.jsonl"))
            .unwrap()
            .lines()
            .count(),
        3
    );
    std::fs::remove_dir_all(root).unwrap();
}

#[test]
fn typed_registration_uses_global_binding_and_host_idempotency() {
    let (server, root) = test_server();
    let cwd = root.to_str().unwrap();
    let typed = server
        .typed_register_envelope("worker", "token-worker", "%worker", cwd)
        .unwrap();
    let first = server.typed_dispatch(typed.clone()).unwrap();
    assert!(!first.replayed);
    let project_scope = crate::server::global_state::GlobalState::canonical_project_scope(
        Path::new(cwd),
    )
    .unwrap();
    let state = server.state.lock().unwrap();
    assert!(state
        .global
        .lookup_registration(
            &project_scope,
            &crate::identity::AppServerId::new("tui-default").unwrap(),
        )
        .is_some());
    assert_eq!(state.global.projects[project_scope.as_str()].runtime_bindings.len(), 1);
    assert!(state.global.command_receipts.contains_key(first.receipt.command_id.as_str()));
    drop(state);

    let replay = server.typed_dispatch(typed).unwrap();
    assert!(replay.replayed);
    assert_eq!(replay.receipt, first.receipt);
    assert_eq!(
        std::fs::read_to_string(root.join(".agent-collab/server/journal.jsonl"))
            .unwrap()
            .lines()
            .count(),
        6
    );
    std::fs::remove_dir_all(root).unwrap();
}

#[test]
fn typed_receipt_revision_is_the_next_cas_and_stale_after_another_mutation() {
    let (server, root) = test_server();
    let cwd = root.to_str().unwrap();

    let first = server
        .typed_dispatch(
            server
                .typed_register_envelope("worker", "token-worker", "%worker", cwd)
                .unwrap(),
        )
        .unwrap();
    let mut second = server
        .typed_register_envelope("worker", "token-worker", "%worker", cwd)
        .unwrap();
    second.envelope.expected_revision = Some(first.receipt.revision);
    let second = server
        .typed_dispatch(second)
        .expect("a receipt revision must be reusable as the next CAS revision");

    let mut stale = server
        .typed_register_envelope("worker", "token-worker", "%worker", cwd)
        .unwrap();
    stale.envelope.expected_revision = Some(second.receipt.revision);
    server
        .commit_checked(&[Event::KeepaliveUpdated {
            worker_id: "other-reducer".into(),
            record: crate::server::keepalive::Record::default(),
        }])
        .unwrap();
    let journal_before_stale =
        std::fs::read_to_string(root.join(".agent-collab/server/journal.jsonl")).unwrap();
    let error = server
        .typed_dispatch(stale)
        .expect_err("an intervening reducer mutation must reject the old CAS revision");
    assert!(error.to_string().contains("compare-and-swap revision mismatch"));
    assert_eq!(
        std::fs::read_to_string(root.join(".agent-collab/server/journal.jsonl")).unwrap(),
        journal_before_stale,
        "a stale CAS must not append a journal event"
    );
    std::fs::remove_dir_all(root).unwrap();
}

#[test]
fn typed_rebinds_survive_journal_rewrite_and_replay_with_one_version_axis() {
    let (server, root) = test_server();
    let cwd = root.to_str().unwrap();
    let mut previous_revision = 0;
    for _ in 0..3 {
        let mut typed = server
            .typed_register_envelope("worker", "token-worker", "%worker", cwd)
            .unwrap();
        typed.envelope.expected_revision = Some(previous_revision);
        let outcome = server.typed_dispatch(typed).unwrap();
        previous_revision = outcome.receipt.revision;
    }

    let (version, binding_generation, receipts) = {
        let state = server.state.lock().unwrap();
        (
            (state.sequence, state.revision, state.global.version()),
            state
                .global
                .projects
                .values()
                .next()
                .unwrap()
                .runtime_bindings["binding-worker"]
                .endpoint_generation,
            state.global.command_receipts.clone(),
        )
    };
    let state = server.state.lock().unwrap();
    state.global.validate().unwrap();
    server.rewrite_journal_locked(&state).unwrap();
    drop(state);

    let replayed = super::replay(&root).unwrap();
    replayed.global.validate().unwrap();
    assert_eq!(
        (replayed.sequence, replayed.revision),
        (version.0, version.1)
    );
    assert_eq!(replayed.global.version(), version.2);
    assert_eq!(
        replayed
            .global
            .projects
            .values()
            .next()
            .unwrap()
            .runtime_bindings["binding-worker"]
            .endpoint_generation,
        binding_generation
    );
    assert_eq!(replayed.global.command_receipts, receipts);
    std::fs::remove_dir_all(root).unwrap();
}

#[test]
fn legacy_command_record_rewrite_and_replay_preserve_checkpoint_version() {
    let (server, root) = test_server();
    let receipt = crate::server::state::CommandReceipt {
        operation_id: "legacy-operation".into(),
        outcome: json!({"accepted": true}),
        sequence: 1,
        revision: 1,
    };
    server
        .commit_checked(&[Event::CommandRecorded {
            command_id: "legacy-command".into(),
            receipt: receipt.clone(),
        }])
        .unwrap();

    let state = server.state.lock().unwrap();
    assert_eq!((state.sequence, state.revision), (1, 1));
    server.rewrite_journal_locked(&state).unwrap();
    drop(state);

    let compacted =
        std::fs::read_to_string(root.join(".agent-collab/server/journal.jsonl")).unwrap();
    assert!(compacted.contains("\"ev\":\"CommandRecorded\""));
    assert!(!compacted.contains("\"ev\":\"CommandStarted\""));
    assert!(!compacted.contains("\"ev\":\"CommandCompleted\""));
    let replayed =
        super::replay(&root).expect("a compacted legacy CommandRecorded must replay successfully");
    assert_eq!((replayed.sequence, replayed.revision), (1, 1));
    assert_eq!(replayed.command_receipts["legacy-command"], receipt);
    std::fs::remove_dir_all(root).unwrap();
}

#[test]
fn replay_rejects_a_checkpoint_that_regresses_real_history() {
    let (server, root) = test_server();
    let journal = root.join(".agent-collab/server/journal.jsonl");
    let events = [
        Event::Registered {
            worker: crate::server::state::WorkerRec {
                id: "checkpoint-worker".into(),
                token: "checkpoint-token".into(),
                pane: Some("%checkpoint-worker".into()),
                cwd: "/tmp".into(),
                registered_ms: 1,
            },
        },
        Event::ReducerCheckpoint {
            sequence: 0,
            revision: 0,
        },
    ];
    let body = events
        .iter()
        .map(|event| serde_json::to_string(event).unwrap())
        .collect::<Vec<_>>()
        .join("\n")
        + "\n";
    std::fs::write(&journal, &body).unwrap();

    let error = match super::replay(&root) {
        Ok(_) => panic!("a real checkpoint rollback must fail closed"),
        Err(error) => error,
    };
    assert!(
        error
            .to_string()
            .contains("reducer checkpoint regresses version"),
        "unexpected error: {error}"
    );
    assert_eq!(std::fs::read_to_string(&journal).unwrap(), body);
    drop(server);
    std::fs::remove_dir_all(root).unwrap();
}

#[test]
fn typed_registration_generation_reaches_max_then_fails_before_journaling_overflow() {
    let (server, root) = test_server();
    let cwd = root.to_str().unwrap();
    let mut first = server
        .typed_register_envelope("worker", "token-worker", "%worker", cwd)
        .unwrap();
    let crate::server::state::TypedCommand::RegisterWorker { binding, .. } = &mut first.command;
    binding.endpoint_generation = u64::MAX - 1;
    first.envelope.endpoint_generation = u64::MAX - 1;
    first.envelope.command_id = crate::identity::CommandId::new("register-max-minus-one").unwrap();
    first.envelope.operation_id =
        crate::identity::OperationId::new("register-op-max-minus-one").unwrap();
    let first = server
        .typed_dispatch(first)
        .expect("MAX-1 binding generation must be accepted");
    assert!(!first.replayed);

    let max = server
        .typed_register_envelope("worker", "token-worker", "%worker", cwd)
        .unwrap();
    assert_eq!(max.envelope.endpoint_generation, u64::MAX);
    let max = server
        .typed_dispatch(max)
        .expect("MAX binding generation must be accepted");
    assert!(!max.replayed);
    assert_eq!(
        server
            .state
            .lock()
            .unwrap()
            .global
            .projects
            .values()
            .next()
            .unwrap()
            .runtime_bindings["binding-worker"]
            .endpoint_generation,
        u64::MAX
    );

    let journal_before_overflow =
        std::fs::read_to_string(root.join(".agent-collab/server/journal.jsonl")).unwrap();
    let receipt_count_before_overflow = server.state.lock().unwrap().global.command_receipts.len();
    let error = server
        .typed_register_envelope("worker", "token-worker", "%worker", cwd)
        .expect_err("MAX binding generation must fail explicitly on increment overflow");
    assert!(
        error.contains("endpoint generation overflow"),
        "unexpected error: {error}"
    );
    assert_eq!(
        std::fs::read_to_string(root.join(".agent-collab/server/journal.jsonl")).unwrap(),
        journal_before_overflow,
        "generation overflow must happen before any journal append"
    );
    assert_eq!(
        server.state.lock().unwrap().global.command_receipts.len(),
        receipt_count_before_overflow,
        "generation overflow must not masquerade as a replayed receipt"
    );
    std::fs::remove_dir_all(root).unwrap();
}

#[test]
fn replay_rejects_invalid_command_receipt_without_rewriting_the_journal() {
    let (server, root) = test_server();
    let journal = root.join(".agent-collab/server/journal.jsonl");
    let event = Event::CommandRecorded {
        command_id: "invalid-receipt".into(),
        receipt: crate::server::state::CommandReceipt {
            operation_id: "invalid-receipt-operation".into(),
            outcome: json!({"accepted": true}),
            sequence: 0,
            revision: 0,
        },
    };
    let body = format!("{}\n", serde_json::to_string(&event).unwrap());
    std::fs::write(&journal, &body).unwrap();

    let error = match super::replay(&root) {
        Ok(_) => panic!("replay must apply command receipt validation before exposing state"),
        Err(error) => error,
    };
    assert!(error.to_string().contains("command receipt sequence"));
    assert_eq!(std::fs::read_to_string(&journal).unwrap(), body);
    drop(server);
    std::fs::remove_dir_all(root).unwrap();
}

#[test]
fn replay_rejects_invalid_command_completion_receipt_without_rewriting_the_journal() {
    let (server, root) = test_server();
    let journal = root.join(".agent-collab/server/journal.jsonl");
    let events = [
        Event::CommandStarted {
            command_id: "invalid-completion".into(),
            operation_id: "invalid-completion-operation".into(),
        },
        Event::CommandCompleted {
            command_id: "invalid-completion".into(),
            operation_id: "invalid-completion-operation".into(),
            receipt: crate::server::state::CommandReceipt {
                operation_id: "invalid-completion-operation".into(),
                outcome: json!({"accepted": true}),
                sequence: 1,
                revision: 0,
            },
        },
    ];
    let body = events
        .iter()
        .map(|event| serde_json::to_string(event).unwrap())
        .collect::<Vec<_>>()
        .join("\n")
        + "\n";
    std::fs::write(&journal, &body).unwrap();

    let error = match super::replay(&root) {
        Ok(_) => panic!("replay must validate a command completion receipt"),
        Err(error) => error,
    };
    assert!(error.to_string().contains("command receipt revision"));
    assert_eq!(std::fs::read_to_string(&journal).unwrap(), body);
    drop(server);
    std::fs::remove_dir_all(root).unwrap();
}

#[test]
fn typed_dispatch_rejects_stale_revision_without_appending() {
    let (server, root) = test_server();
    let cwd = root.to_str().unwrap();
    let first = server
        .typed_register_envelope("worker", "token-worker", "%worker", cwd)
        .unwrap();
    server.typed_dispatch(first).unwrap();
    let mut stale = server
        .typed_register_envelope("worker-2", "token-worker-2", "%worker-2", cwd)
        .unwrap();
    stale.envelope.command_id = crate::identity::CommandId::new("register-stale").unwrap();
    stale.envelope.operation_id = crate::identity::OperationId::new("register-stale-op").unwrap();
    stale.envelope.expected_revision = Some(0);
    let error = server.typed_dispatch(stale).unwrap_err();
    assert!(error.to_string().contains("compare-and-swap revision mismatch"));
    assert_eq!(
        std::fs::read_to_string(root.join(".agent-collab/server/journal.jsonl"))
            .unwrap()
            .lines()
            .count(),
        6
    );
    std::fs::remove_dir_all(root).unwrap();
}

#[test]
fn typed_dispatch_rejects_wrong_principal_and_scope() {
    let (server, root) = test_server();
    let cwd = root.to_str().unwrap();
    let mut wrong_principal = server
        .typed_register_envelope("worker", "token-worker", "%worker", cwd)
        .unwrap();
    if let crate::server::state::TypedCommand::RegisterWorker { binding, .. } =
        &mut wrong_principal.command
    {
        binding.agent_id = crate::identity::AgentId::new("other-worker").unwrap();
    }
    let principal_error = server.typed_dispatch(wrong_principal).unwrap_err();
    assert!(principal_error
        .to_string()
        .contains("runtime binding agent does not match worker identity"));

    let mut wrong_scope = server
        .typed_register_envelope("worker", "token-worker", "%worker", cwd)
        .unwrap();
    wrong_scope.envelope.scope.project_scope_id =
        crate::scope::ProjectScopeId::new("/other-project").unwrap();
    let scope_error = server.typed_dispatch(wrong_scope).unwrap_err();
    assert!(scope_error
        .to_string()
        .contains("envelope scope does not match binding route scope"));
    assert!(std::fs::read_to_string(root.join(".agent-collab/server/journal.jsonl"))
        .unwrap()
        .trim()
        .is_empty());
    std::fs::remove_dir_all(root).unwrap();
}

#[test]
fn global_reducer_failure_is_explicit_and_poisoned_after_journal_append() {
    let (server, root) = test_server();
    let binding = crate::server::global_state::RuntimeBinding::new(
        crate::scope::ProjectScopeId::new("/unregistered-project").unwrap(),
        crate::identity::AppServerId::new("tui-default").unwrap(),
        crate::identity::AgentId::new("worker").unwrap(),
        crate::identity::RuntimeId::new("runtime-worker").unwrap(),
        crate::identity::BindingId::new("binding-worker").unwrap(),
        1,
        None,
    )
    .unwrap();
    let error = server
        .commit_checked(&[Event::GlobalRuntimeBound { binding }])
        .unwrap_err();
    assert!(matches!(error, JournalError::Reducer(_)));
    assert!(server.state.lock().unwrap().journal_poison.is_some());
    assert_eq!(server.state.lock().unwrap().global.projects.len(), 0);
    let journal = std::fs::read_to_string(root.join(".agent-collab/server/journal.jsonl")).unwrap();
    assert!(journal.contains("GlobalRuntimeBound"));
    std::fs::remove_dir_all(root).unwrap();
}

#[test]
fn command_start_append_and_sync_failures_fail_closed_before_business_apply() {
    for fault in [
        CommandJournalFault::StartAppend,
        CommandJournalFault::StartSync,
    ] {
        let (server, root) = test_server();
        inject_command_journal_fault(fault);
        let result = server.commit_command(
            "command-start-fault",
            "operation-start-fault",
            &[Event::KeepaliveUpdated {
                worker_id: "worker".into(),
                record: crate::server::keepalive::Record::default(),
            }],
            json!({"accepted": true}),
        );
        assert!(result.is_err());
        assert!(server.state.lock().unwrap().keepalives.is_empty());
        let replay = super::replay(&root);
        match fault {
            CommandJournalFault::StartAppend => {
                assert!(replay.is_ok(), "failed first append must leave no command");
            }
            CommandJournalFault::StartSync => {
                assert!(
                    replay.is_err(),
                    "sync failure after start append must poison replay"
                );
            }
            _ => unreachable!(),
        }
        std::fs::remove_dir_all(root).unwrap();
    }
}

#[test]
fn command_completion_append_failure_leaves_incomplete_replay() {
    let (server, root) = test_server();
    inject_command_journal_fault(CommandJournalFault::CompletionAppend);
    let result = server.commit_command(
        "command-completion-append",
        "operation-completion-append",
        &[Event::KeepaliveUpdated {
            worker_id: "worker".into(),
            record: crate::server::keepalive::Record::default(),
        }],
        json!({"accepted": true}),
    );
    assert!(result.is_err());
    assert!(server.state.lock().unwrap().keepalives.is_empty());
    assert!(super::replay(&root).is_err());
    std::fs::remove_dir_all(root).unwrap();
}

#[test]
fn command_completion_sync_failure_is_explicit_and_replayable_if_marker_was_written() {
    let (server, root) = test_server();
    inject_command_journal_fault(CommandJournalFault::CompletionSync);
    let result = server.commit_command(
        "command-completion-sync",
        "operation-completion-sync",
        &[Event::KeepaliveUpdated {
            worker_id: "worker".into(),
            record: crate::server::keepalive::Record::default(),
        }],
        json!({"accepted": true}),
    );
    assert!(result.is_err());
    assert!(server.state.lock().unwrap().keepalives.is_empty());
    let replayed = super::replay(&root).expect("written completion marker must replay");
    assert!(replayed.keepalives.contains_key("worker"));
    assert_eq!(
        replayed.command_receipts["command-completion-sync"].operation_id,
        "operation-completion-sync"
    );
    std::fs::remove_dir_all(root).unwrap();
}

fn subagent_record(id: &str, status: &str, peer: &str) -> crate::subagent::Record {
    crate::subagent::Record {
        id: id.into(),
        parent: "parent".into(),
        peer: peer.into(),
        status: status.into(),
        session: Some("$session".into()),
        pane: Some("%child".into()),
        profile: None,
        created_ms: now_ms(),
        ready_deadline_ms: now_ms() + 90_000,
        last_message: None,
        error: None,
        probe_failures: Vec::new(),
        runtime: Some("cursor".into()),
    }
}

fn subagent_req(command: crate::subagent::Action) -> Req {
    Req::Subagent {
        worker_id: "parent".into(),
        token: "token-parent".into(),
        command,
        launch_env: Default::default(),
    }
}

#[test]
fn subagent_start_journal_failure_does_not_launch_or_write_success() {
    use crate::server::SubagentJournalFault::{StartAppend, StartSync};

    for fault in [StartAppend, StartSync] {
        let (mut server, root) = test_server();
        register(&server, "parent", "%parent");
        server.pane_alive_check = |_| PanePresence::Present;
        server.commit(&[Event::MasterAssigned {
            worker_id: "parent".into(),
            assigned_by: "operator".into(),
            approval: Some("start journal regression".into()),
            assigned_ms: now_ms(),
        }]);
        let server = Arc::new(server);
        crate::server::inject_subagent_journal_fault(fault);
        let result = dispatch(
            &server,
            subagent_req(crate::subagent::Action::Start {
                id: Some("start-journal-fault".into()),
                runtime: Some("cursor".into()),
            }),
        );
        assert!(!result.ok, "{fault:?}: {result:?}");
        let state = server.state.lock().unwrap();
        assert!(state.subagents.is_empty());
        assert!(state.journal_poison.is_some());
        drop(state);
        assert!(!root
            .join(".agent-collab/server/launch-start-journal-fault.json")
            .exists());
        let replayed = replay(&root).unwrap();
        match fault {
            StartAppend => assert!(replayed.subagents.is_empty()),
            StartSync => {
                assert!(replayed
                    .subagents
                    .values()
                    .all(|record| record.status != "starting" && record.status != "failed"));
            }
            _ => unreachable!(),
        }
        std::fs::remove_dir_all(root).unwrap();
    }
}

#[test]
fn subagent_working_journal_failure_preserves_task_and_subagent_and_poison_rejects_mutation() {
    use crate::server::SubagentJournalFault::{WorkingAppend, WorkingSync};

    for fault in [WorkingAppend, WorkingSync] {
        let (server, root) = test_server();
        register(&server, "parent", "%parent");
        register(&server, "child", "%child");
        let now = now_ms();
        let mut record = subagent_record("working-journal-fault", "assigned", "child");
        record.last_message = Some("working-journal-fault".into());
        server.commit(&[
            Event::SubagentUpdated { subagent: record },
            Event::TaskCreated {
                task: TaskRec {
                    id: "task-working-journal-fault".into(),
                    owner: "child".into(),
                    created_by: "parent".into(),
                    feature_id: None,
                    worktree_path: None,
                    branch: None,
                    base_commit: None,
                    priority: "p2".into(),
                    status: "assigned".into(),
                    next_step: None,
                    wait: None,
                    created_ms: now,
                    updated_ms: now,
                },
            },
        ]);
        let server = Arc::new(server);
        crate::server::inject_subagent_journal_fault(fault);
        let result = dispatch(
            &server,
            Req::Subagent {
                worker_id: "child".into(),
                token: "token-child".into(),
                command: crate::subagent::Action::Working {
                    id: "working-journal-fault".into(),
                },
                launch_env: Default::default(),
            },
        );
        assert!(!result.ok, "{fault:?}: {result:?}");
        let state = server.state.lock().unwrap();
        assert_eq!(state.subagents["working-journal-fault"].status, "assigned");
        assert_eq!(state.tasks["task-working-journal-fault"].status, "assigned");
        assert!(state.journal_poison.is_some());
        drop(state);
        let rejected = dispatch(
            &server,
            Req::Subagent {
                worker_id: "child".into(),
                token: "token-child".into(),
                command: crate::subagent::Action::Working {
                    id: "working-journal-fault".into(),
                },
                launch_env: Default::default(),
            },
        );
        assert!(!rejected.ok);
        let state = server.state.lock().unwrap();
        assert_eq!(state.subagents["working-journal-fault"].status, "assigned");
        assert_eq!(state.tasks["task-working-journal-fault"].status, "assigned");
        drop(state);
        assert!(replay(&root).is_ok());
        std::fs::remove_dir_all(root).unwrap();
    }
}

#[test]
fn subagent_close_first_journal_failure_does_not_remove_external_manifest() {
    use crate::server::SubagentJournalFault::{CloseFirstAppend, CloseFirstSync};

    for fault in [CloseFirstAppend, CloseFirstSync] {
        let (mut server, root) = test_server();
        register(&server, "parent", "%parent");
        server.pane_alive_check = |_| PanePresence::Missing;
        server.commit(&[Event::SubagentUpdated {
            subagent: subagent_record("close-first-fault", "idle", "child"),
        }]);
        let manifest = root.join(".agent-collab/server/launch-close-first-fault.json");
        std::fs::write(&manifest, b"test-only manifest").unwrap();
        let server = Arc::new(server);
        crate::server::inject_subagent_journal_fault(fault);
        let result = dispatch(
            &server,
            subagent_req(crate::subagent::Action::Close {
                id: "close-first-fault".into(),
            }),
        );
        assert!(!result.ok, "{fault:?}: {result:?}");
        let state = server.state.lock().unwrap();
        assert_eq!(state.subagents["close-first-fault"].status, "idle");
        assert!(state.journal_poison.is_some());
        drop(state);
        assert!(manifest.exists());
        std::fs::remove_dir_all(root).unwrap();
    }
}

#[test]
fn subagent_close_final_journal_failure_reports_unknown_and_stays_open() {
    use crate::server::SubagentJournalFault::{CloseFinalAppend, CloseFinalSync};

    for fault in [CloseFinalAppend, CloseFinalSync] {
        let (mut server, root) = test_server();
        register(&server, "parent", "%parent");
        server.pane_alive_check = |_| PanePresence::Missing;
        server.commit(&[Event::SubagentUpdated {
            subagent: subagent_record("close-final-fault", "idle", "child"),
        }]);
        let manifest = root.join(".agent-collab/server/launch-close-final-fault.json");
        std::fs::write(&manifest, b"test-only manifest").unwrap();
        let server = Arc::new(server);
        crate::server::inject_subagent_journal_fault(fault);
        let result = dispatch(
            &server,
            subagent_req(crate::subagent::Action::Close {
                id: "close-final-fault".into(),
            }),
        );
        assert!(!result.ok, "{fault:?}: {result:?}");
        assert!(result.error.unwrap().contains("outcome unknown"));
        let state = server.state.lock().unwrap();
        assert_eq!(state.subagents["close-final-fault"].status, "closing");
        assert!(state.journal_poison.is_some());
        drop(state);
        assert!(!manifest.exists());
        std::fs::remove_dir_all(root).unwrap();
    }
}

#[test]
fn replayed_command_is_idempotent_and_operation_conflict_fails_closed() {
    let (server, root) = test_server();
    server
        .commit_command(
            "command-replay",
            "operation-replay",
            &[Event::KeepaliveUpdated {
                worker_id: "worker".into(),
                record: crate::server::keepalive::Record::default(),
            }],
            json!({"accepted": true}),
        )
        .unwrap();
    let restarted = {
        let journal = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(root.join(".agent-collab/server/journal.jsonl"))
            .unwrap();
        Server {
            config: crate::config::Config::default(),
            root: root.clone(),
            state: Mutex::new(super::replay(&root).unwrap()),
            journal: Mutex::new(journal),
            pane_alive_check: |_| PanePresence::Present,
            pane_owner_check: |_, _| Ok(true),
            pane_state_check: |_| crate::server::knock::AgentState::Waiting,
            mailbox_notify: tokio::sync::Notify::new(),
        }
    };
    let retry = restarted
        .commit_command(
            "command-replay",
            "operation-replay",
            &[Event::KeepaliveUpdated {
                worker_id: "duplicate".into(),
                record: crate::server::keepalive::Record::default(),
            }],
            json!({"accepted": false}),
        )
        .unwrap();
    assert!(retry.replayed);
    assert_eq!(retry.outcome, json!({"accepted": true}));
    let conflict = restarted.commit_command(
        "command-other",
        "operation-replay",
        &[],
        json!({"accepted": true}),
    );
    assert!(matches!(conflict, Err(JournalError::InvalidCommand(_))));
    assert!(!restarted
        .state
        .lock()
        .unwrap()
        .keepalives
        .contains_key("duplicate"));
    std::fs::remove_dir_all(root).unwrap();
}

#[test]
fn replay_rejects_business_event_persisted_without_command_completion() {
    let (server, root) = test_server();
    let journal = root.join(".agent-collab/server/journal.jsonl");
    let events = [
        Event::CommandStarted {
            command_id: "incomplete".into(),
            operation_id: "operation-incomplete".into(),
        },
        Event::KeepaliveUpdated {
            worker_id: "worker".into(),
            record: crate::server::keepalive::Record::default(),
        },
    ];
    let body = events
        .iter()
        .map(|event| serde_json::to_string(event).unwrap())
        .collect::<Vec<_>>()
        .join("\n");
    std::fs::write(&journal, format!("{body}\n")).unwrap();
    let error = match super::replay(&root) {
        Ok(_) => panic!("incomplete command replay must fail"),
        Err(error) => error.to_string(),
    };
    assert!(error.contains("incomplete"), "unexpected error: {error}");
    drop(server);
    std::fs::remove_dir_all(root).unwrap();
}

#[test]
fn legacy_command_record_without_outcome_is_rejected() {
    let (server, root) = test_server();
    let journal = root.join(".agent-collab/server/journal.jsonl");
    std::fs::write(
        &journal,
        r#"{"ev":"CommandRecorded","command_id":"legacy","receipt":{"operation_id":"op"}}
"#,
    )
    .unwrap();
    let error = match super::replay(&root) {
        Ok(_) => panic!("legacy command record without outcome must be rejected"),
        Err(error) => error.to_string(),
    };
    assert!(error.contains("outcome"), "unexpected error: {error}");
    drop(server);
    std::fs::remove_dir_all(root).unwrap();
}

#[test]
fn activity_log_never_copies_launch_credentials() {
    let request = Req::Subagent {
        worker_id: "parent".into(),
        token: "token".into(),
        command: crate::subagent::Action::Start {
            id: None,
            runtime: None,
        },
        launch_env: std::collections::BTreeMap::from([("SECRET".into(), "do-not-log".into())]),
    };
    let log = request_activity(&request, &Resp::data(json!({})));
    assert!(log["request"].get("launch_env").is_none());
    assert!(!log.to_string().contains("do-not-log"));
}

#[test]
fn managed_subagent_is_authenticated_persistent_and_replayable() {
    use crate::subagent::{Action, Record};
    let (server, root) = test_server();
    register(&server, "parent", "%parent");
    register(&server, "child", "%child");
    register(&server, "other", "%other");
    let record = Record {
        id: "managed".into(),
        parent: "parent".into(),
        peer: "child".into(),
        status: "starting".into(),
        session: None,
        pane: Some("%child".into()),
        profile: None,
        created_ms: now_ms(),
        ready_deadline_ms: now_ms() + 90000,
        last_message: None,
        error: None,
        probe_failures: Vec::new(),
        runtime: None,
    };
    let event = Event::SubagentUpdated { subagent: record };
    let encoded = serde_json::to_string(&event).unwrap();
    let mut replay = State::default();
    replay.apply(&serde_json::from_str(&encoded).unwrap());
    assert_eq!(replay.subagents["managed"].status, "starting");
    server.commit(&[event]);
    let child_ctx = handle_context(&server, "child".into(), "token-child".into());
    assert_eq!(child_ctx.data["authority"]["must_obey_master"], true);
    assert_eq!(
        child_ctx.data["authority"]["may_decline_master_invite"],
        false
    );
    let parent_ctx = handle_context(&server, "parent".into(), "token-parent".into());
    assert_eq!(
        parent_ctx.data["authority"]["may_decline_master_invite"],
        true
    );
    assert!(
        !crate::subagent::handle(
            &server,
            "other",
            "token-other",
            Action::Close {
                id: "managed".into()
            }
        )
        .ok
    );
    assert!(!crate::subagent::handle(&server, "parent", "wrong", Action::List).ok);
    assert!(
        !crate::subagent::handle(
            &server,
            "parent",
            "token-parent",
            Action::Ready {
                id: "managed".into()
            }
        )
        .ok
    );
    assert!(
        crate::subagent::handle(
            &server,
            "child",
            "token-child",
            Action::Ready {
                id: "managed".into()
            }
        )
        .ok
    );
    let count = server.state.lock().unwrap().msgs.len();
    assert!(
        crate::subagent::handle(
            &server,
            "child",
            "token-child",
            Action::Ready {
                id: "managed".into()
            }
        )
        .ok
    );
    assert_eq!(server.state.lock().unwrap().msgs.len(), count);
    let no_assigned = crate::subagent::handle(
        &server,
        "child",
        "token-child",
        Action::Working {
            id: "managed".into(),
        },
    );
    assert!(!no_assigned.ok);
    assert_eq!(
        no_assigned.error.as_deref(),
        Some("no assigned task to accept")
    );
    let unknown = crate::subagent::handle(
        &server,
        "parent",
        "token-parent",
        Action::Status {
            id: "missing-managed".into(),
        },
    );
    assert!(!unknown.ok);
    assert!(
        crate::subagent::handle(
            &server,
            "parent",
            "token-parent",
            Action::Send {
                id: "managed".into(),
                subject: "test".into(),
                body: "task".into()
            }
        )
        .ok
    );
    let message_id = server.state.lock().unwrap().subagents["managed"]
        .last_message
        .clone()
        .unwrap();
    assert_eq!(
        server.state.lock().unwrap().tasks[&format!("task-{message_id}")].status,
        "assigned"
    );
    let server = Arc::new(server);
    let message = dispatch(&server, Req::MsgStatus { msg_id: message_id });
    assert_eq!(message.data["body"], "task");
    assert!(
        !crate::subagent::handle(
            &server,
            "parent",
            "token-parent",
            Action::Send {
                id: "managed".into(),
                subject: "test".into(),
                body: "task".into()
            }
        )
        .ok
    );
    let still_working = crate::subagent::handle(
        &server,
        "parent",
        "token-parent",
        Action::Send {
            id: "managed".into(),
            subject: "must-wait".into(),
            body: "active task still owns the child".into(),
        },
    );
    assert!(!still_working.ok);
    assert_eq!(
        still_working.error.as_deref(),
        Some("subagent is not idle; query status instead of resending")
    );
    assert!(
        crate::subagent::handle(
            &server,
            "child",
            "token-child",
            Action::Working {
                id: "managed".into()
            }
        )
        .ok
    );
    assert_eq!(
        server
            .state
            .lock()
            .unwrap()
            .tasks
            .values()
            .next()
            .unwrap()
            .status,
        "working"
    );
    let observed = dispatch(
        &server,
        Req::SubagentObserve {
            id: Some("managed".into()),
            snapshot_lines: None,
        },
    );
    assert!(observed.ok);
    assert_eq!(observed.data["notification_channel"], "none");
    assert!(observed.data.get("screen_tail").is_none());
    assert!(observed.data["tasks"].as_array().unwrap().len() == 1);
    assert!(
        crate::subagent::handle(
            &server,
            "child",
            "token-child",
            Action::Ready {
                id: "managed".into()
            }
        )
        .ok
    );
    assert_eq!(
        server.state.lock().unwrap().subagents["managed"].status,
        "idle"
    );
    assert!(
        crate::subagent::handle(
            &server,
            "parent",
            "token-parent",
            Action::Close {
                id: "managed".into()
            }
        )
        .ok
    );
    assert!(
        crate::subagent::handle(
            &server,
            "parent",
            "token-parent",
            Action::Close {
                id: "managed".into()
            }
        )
        .ok
    );
    std::fs::remove_dir_all(root).unwrap();
}

#[test]
fn managed_subagent_send_binds_the_selected_child_when_multiple_children_are_assigned() {
    use crate::subagent::{Action, Record};
    let (server, root) = test_server();
    register(&server, "parent", "%parent");
    register(&server, "child-a", "%child-a");
    register(&server, "child-b", "%child-b");
    let now = now_ms();
    server.commit(&[
        Event::SubagentUpdated {
            subagent: Record {
                id: "managed-a".into(),
                parent: "parent".into(),
                peer: "child-a".into(),
                status: "idle".into(),
                session: Some("$child-a".into()),
                pane: Some("%child-a".into()),
                profile: None,
                created_ms: now,
                ready_deadline_ms: now + 90_000,
                last_message: None,
                error: None,
                probe_failures: Vec::new(),
                runtime: None,
            },
        },
        Event::SubagentUpdated {
            subagent: Record {
                id: "managed-b".into(),
                parent: "parent".into(),
                peer: "child-b".into(),
                status: "idle".into(),
                session: Some("$child-b".into()),
                pane: Some("%child-b".into()),
                profile: None,
                created_ms: now,
                ready_deadline_ms: now + 90_000,
                last_message: None,
                error: None,
                probe_failures: Vec::new(),
                runtime: None,
            },
        },
    ]);

    let wrong_binding = handle_send_with_task(
        &server,
        "parent".into(),
        "child-a".into(),
        "notify".into(),
        Some("wrong-binding".into()),
        "must fail".into(),
        None,
        "immediate".into(),
        true,
        Some("managed-b"),
    );
    assert!(!wrong_binding.ok);
    assert_eq!(
        wrong_binding.error.as_deref(),
        Some("managed subagent owner mismatch")
    );
    let unknown_binding = handle_send_with_task(
        &server,
        "parent".into(),
        "child-a".into(),
        "notify".into(),
        Some("unknown-binding".into()),
        "must fail".into(),
        None,
        "immediate".into(),
        true,
        Some("missing"),
    );
    assert!(!unknown_binding.ok);
    assert_eq!(
        unknown_binding.error.as_deref(),
        Some("unknown managed subagent missing")
    );

    let first = crate::subagent::handle(
        &server,
        "parent",
        "token-parent",
        Action::Send {
            id: "managed-a".into(),
            subject: "same-task".into(),
            body: "same body".into(),
        },
    );
    assert!(first.ok, "{}", first.error.unwrap_or_default());
    let first_message = server.state.lock().unwrap().subagents["managed-a"]
        .last_message
        .clone()
        .unwrap();
    let second = crate::subagent::handle(
        &server,
        "parent",
        "token-parent",
        Action::Send {
            id: "managed-b".into(),
            subject: "same-task".into(),
            body: "same body".into(),
        },
    );
    assert!(second.ok, "{}", second.error.unwrap_or_default());
    let second_message = server.state.lock().unwrap().subagents["managed-b"]
        .last_message
        .clone()
        .unwrap();

    let journal: Vec<serde_json::Value> =
        std::fs::read_to_string(root.join(".agent-collab/server/journal.jsonl"))
            .unwrap()
            .lines()
            .map(|line| serde_json::from_str(line).unwrap())
            .collect();
    let first_sent_index = journal
        .iter()
        .position(|event| event["ev"] == "Sent" && event["msg"]["id"] == first_message)
        .unwrap();
    let second_sent_index = journal
        .iter()
        .position(|event| event["ev"] == "Sent" && event["msg"]["id"] == second_message)
        .unwrap();
    assert!(!journal[first_sent_index..second_sent_index]
        .iter()
        .any(|event| event["ev"] == "SubagentUpdated" && event["subagent"]["id"] == "managed-b"),
        "the selected child must be bound only in the message commit");

    let state = server.state.lock().unwrap();
    let first_message = state.subagents["managed-a"].last_message.clone().unwrap();
    let second_message = state.subagents["managed-b"].last_message.clone().unwrap();
    assert_ne!(first_message, second_message);
    assert_eq!(state.tasks.len(), 2);
    assert_eq!(
        state.tasks[&format!("task-{first_message}")].owner,
        "child-a"
    );
    assert_eq!(
        state.tasks[&format!("task-{second_message}")].owner,
        "child-b"
    );
    drop(state);

    assert!(
        crate::subagent::handle(
            &server,
            "child-a",
            "token-child-a",
            Action::Working {
                id: "managed-a".into(),
            },
        )
        .ok
    );
    assert!(
        crate::subagent::handle(
            &server,
            "child-b",
            "token-child-b",
            Action::Working {
                id: "managed-b".into(),
            },
        )
        .ok
    );
    let replayed = replay(&root).unwrap();
    assert_eq!(replayed.subagents["managed-a"].status, "working");
    assert_eq!(replayed.subagents["managed-b"].status, "working");
    assert_eq!(
        replayed.tasks[&format!("task-{first_message}")].status,
        "working"
    );
    assert_eq!(
        replayed.tasks[&format!("task-{second_message}")].status,
        "working"
    );
    std::fs::remove_dir_all(root).unwrap();
}

#[test]
fn managed_subagent_send_reclaims_working_child_without_an_owned_task() {
    use crate::subagent::{Action, Record};
    let (server, root) = test_server();
    register(&server, "parent", "%parent");
    register(&server, "child", "%child");
    let now = now_ms();
    server.commit(&[Event::SubagentUpdated {
        subagent: Record {
            id: "managed".into(),
            parent: "parent".into(),
            peer: "child".into(),
            // A keepalive pane observation can leave this stale after the
            // child has reported ready and consumed an empty recv cycle.
            status: "working".into(),
            session: Some("$child".into()),
            pane: Some("%child".into()),
            profile: None,
            created_ms: now,
            ready_deadline_ms: now + 90_000,
            last_message: None,
            error: None,
            probe_failures: Vec::new(),
            runtime: None,
        },
    }]);

    let assigned = crate::subagent::handle(
        &server,
        "parent",
        "token-parent",
        Action::Send {
            id: "managed".into(),
            subject: "next-task".into(),
            body: "dispatch after ready and recv".into(),
        },
    );
    assert!(assigned.ok, "{}", assigned.error.unwrap_or_default());
    let state = server.state.lock().unwrap();
    assert_eq!(state.subagents["managed"].status, "assigned");
    assert_eq!(state.tasks.len(), 1);
    assert_eq!(state.tasks.values().next().unwrap().owner, "child");
    drop(state);
    assert!(
        crate::subagent::handle(
            &server,
            "child",
            "token-child",
            Action::Working {
                id: "managed".into(),
            },
        )
        .ok
    );
    assert!(
        crate::subagent::handle(
            &server,
            "child",
            "token-child",
            Action::Ready {
                id: "managed".into(),
            },
        )
        .ok
    );
    let before = {
        let state = server.state.lock().unwrap();
        (
            state.tasks.len(),
            state.subagents["managed"].last_message.clone(),
            state.subagents["managed"].status.clone(),
        )
    };
    let idle_with_active_task = crate::subagent::handle(
        &server,
        "parent",
        "token-parent",
        Action::Send {
            id: "managed".into(),
            subject: "must-wait-idle".into(),
            body: "idle status still has an active task".into(),
        },
    );
    assert!(!idle_with_active_task.ok);
    assert_eq!(
        idle_with_active_task.error.as_deref(),
        Some("managed subagent already has an active task")
    );
    let state = server.state.lock().unwrap();
    assert_eq!(state.tasks.len(), before.0);
    assert_eq!(state.subagents["managed"].last_message, before.1);
    assert_eq!(state.subagents["managed"].status, before.2);
    drop(state);
    let direct_idle_with_active_task = handle_send_with_task(
        &server,
        "parent".into(),
        "child".into(),
        "notify".into(),
        Some("direct-must-wait".into()),
        "direct active task still owns the child".into(),
        None,
        "immediate".into(),
        true,
        Some("managed"),
    );
    assert!(!direct_idle_with_active_task.ok);
    assert_eq!(
        direct_idle_with_active_task.error.as_deref(),
        Some("managed subagent already has an active task")
    );
    std::fs::remove_dir_all(root).unwrap();
}

#[test]
fn managed_subagent_working_requires_existing_owned_assigned_task() {
    use crate::subagent::{Action, Record};

    {
        let (server, root) = test_server();
        register(&server, "parent", "%parent");
        register(&server, "child", "%child");
        let now = now_ms();
        server.commit(&[Event::SubagentUpdated {
            subagent: Record {
                id: "missing-task".into(),
                parent: "parent".into(),
                peer: "child".into(),
                status: "assigned".into(),
                session: Some("$child".into()),
                pane: Some("%child".into()),
                profile: None,
                created_ms: now,
                ready_deadline_ms: now + 90_000,
                last_message: Some("missing".into()),
                error: None,
                probe_failures: Vec::new(),
                runtime: None,
            },
        }]);
        let result = crate::subagent::handle(
            &server,
            "child",
            "token-child",
            Action::Working {
                id: "missing-task".into(),
            },
        );
        assert!(!result.ok);
        assert_eq!(
            result.error.as_deref(),
            Some("assigned task task-missing not found")
        );
        assert_eq!(
            server.state.lock().unwrap().subagents["missing-task"].status,
            "assigned"
        );
        std::fs::remove_dir_all(root).unwrap();
    }

    {
        let (server, root) = test_server();
        register(&server, "parent", "%parent");
        register(&server, "child", "%child");
        let now = now_ms();
        server.commit(&[
            Event::SubagentUpdated {
                subagent: Record {
                    id: "terminal-task".into(),
                    parent: "parent".into(),
                    peer: "child".into(),
                    status: "assigned".into(),
                    session: Some("$child".into()),
                    pane: Some("%child".into()),
                    profile: None,
                    created_ms: now,
                    ready_deadline_ms: now + 90_000,
                    last_message: Some("terminal".into()),
                    error: None,
                    probe_failures: Vec::new(),
                    runtime: None,
                },
            },
            Event::TaskCreated {
                task: TaskRec {
                    id: "task-terminal".into(),
                    owner: "child".into(),
                    created_by: "parent".into(),
                    feature_id: None,
                    worktree_path: None,
                    branch: None,
                    base_commit: None,
                    priority: "p2".into(),
                    status: "closed".into(),
                    next_step: None,
                    wait: None,
                    created_ms: now,
                    updated_ms: now,
                },
            },
        ]);
        let result = crate::subagent::handle(
            &server,
            "child",
            "token-child",
            Action::Working {
                id: "terminal-task".into(),
            },
        );
        assert!(!result.ok);
        assert_eq!(
            result.error.as_deref(),
            Some("assigned task task-terminal is not in assigned state (status=closed)")
        );
        let state = server.state.lock().unwrap();
        assert_eq!(state.subagents["terminal-task"].status, "assigned");
        assert_eq!(state.tasks["task-terminal"].status, "closed");
        drop(state);
        std::fs::remove_dir_all(root).unwrap();
    }

    {
        let (server, root) = test_server();
        register(&server, "parent", "%parent");
        register(&server, "child", "%child");
        let now = now_ms();
        server.commit(&[
            Event::SubagentUpdated {
                subagent: Record {
                    id: "owner-task".into(),
                    parent: "parent".into(),
                    peer: "child".into(),
                    status: "assigned".into(),
                    session: Some("$child".into()),
                    pane: Some("%child".into()),
                    profile: None,
                    created_ms: now,
                    ready_deadline_ms: now + 90_000,
                    last_message: Some("owner".into()),
                    error: None,
                    probe_failures: Vec::new(),
                    runtime: None,
                },
            },
            Event::TaskCreated {
                task: TaskRec {
                    id: "task-owner".into(),
                    owner: "other".into(),
                    created_by: "parent".into(),
                    feature_id: None,
                    worktree_path: None,
                    branch: None,
                    base_commit: None,
                    priority: "p2".into(),
                    status: "assigned".into(),
                    next_step: None,
                    wait: None,
                    created_ms: now,
                    updated_ms: now,
                },
            },
        ]);
        let result = crate::subagent::handle(
            &server,
            "child",
            "token-child",
            Action::Working {
                id: "owner-task".into(),
            },
        );
        assert!(!result.ok);
        assert_eq!(result.error.as_deref(), Some("task owner mismatch"));
        let state = server.state.lock().unwrap();
        assert_eq!(state.subagents["owner-task"].status, "assigned");
        assert_eq!(state.tasks["task-owner"].owner, "other");
        assert_eq!(state.tasks["task-owner"].status, "assigned");
        drop(state);
        std::fs::remove_dir_all(root).unwrap();
    }
}

#[test]
fn managed_subagent_working_accepts_assignment_after_probe_race_and_is_idempotent() {
    use crate::subagent::{Action, Record};

    let (server, root) = test_server();
    register(&server, "parent", "%parent");
    register(&server, "child", "%child");
    let now = now_ms();
    server.commit(&[Event::SubagentUpdated {
        subagent: Record {
            id: "managed".into(),
            parent: "parent".into(),
            peer: "child".into(),
            status: "idle".into(),
            session: Some("$child".into()),
            pane: Some("%child".into()),
            profile: None,
            created_ms: now,
            ready_deadline_ms: now + 90_000,
            last_message: None,
            error: None,
            probe_failures: Vec::new(),
            runtime: None,
        },
    }]);

    let sent = crate::subagent::handle(
        &server,
        "parent",
        "token-parent",
        Action::Send {
            id: "managed".into(),
            subject: "race-task".into(),
            body: "claim the task after the probe observes working".into(),
        },
    );
    assert!(sent.ok, "{}", sent.error.unwrap_or_default());
    let message_id = server.state.lock().unwrap().subagents["managed"]
        .last_message
        .clone()
        .unwrap();
    let task_id = format!("task-{message_id}");
    {
        let state = server.state.lock().unwrap();
        assert_eq!(state.subagents["managed"].status, "assigned");
        assert_eq!(state.tasks[&task_id].status, "assigned");
        assert_eq!(state.tasks.len(), 1);
    }

    // Model the keepalive pane probe winning the race: it observes the child
    // as working and persists that managed status while the task is assigned.
    let mut probed = server.state.lock().unwrap().subagents["managed"].clone();
    probed.status = "working".into();
    server.commit(&[Event::SubagentUpdated { subagent: probed }]);

    let first = crate::subagent::handle(
        &server,
        "child",
        "token-child",
        Action::Working {
            id: "managed".into(),
        },
    );
    assert!(first.ok, "{}", first.error.unwrap_or_default());
    {
        let state = server.state.lock().unwrap();
        assert_eq!(state.subagents["managed"].status, "working");
        assert_eq!(state.tasks[&task_id].status, "working");
        assert_eq!(state.tasks[&task_id].owner, "child");
        assert_eq!(state.tasks.len(), 1);
    }

    // Once both durable records are working, a repeated claim is a harmless
    // replay of the same transition and must not append or create anything.
    let journal_after_first =
        std::fs::read_to_string(root.join(".agent-collab/server/journal.jsonl"))
            .unwrap()
            .lines()
            .count();
    let second = crate::subagent::handle(
        &server,
        "child",
        "token-child",
        Action::Working {
            id: "managed".into(),
        },
    );
    assert!(second.ok, "{}", second.error.unwrap_or_default());
    let journal_after_second =
        std::fs::read_to_string(root.join(".agent-collab/server/journal.jsonl"))
            .unwrap()
            .lines()
            .count();
    assert_eq!(journal_after_second, journal_after_first);
    assert_eq!(server.state.lock().unwrap().tasks.len(), 1);

    let replayed = replay(&root).unwrap();
    assert_eq!(replayed.subagents["managed"].status, "working");
    assert_eq!(replayed.tasks[&task_id].status, "working");
    assert_eq!(replayed.tasks[&task_id].owner, "child");
    assert_eq!(replayed.tasks.len(), 1);
    std::fs::remove_dir_all(root).unwrap();
}

#[test]
fn worker_close_is_master_only_audited_and_refuses_to_strand_tasks() {
    let (server, root) = test_server();
    register(&server, "peer-a", "%a");
    register(&server, "peer-b", "%b");
    assert!(
        super::handle_master_promote(
            &server,
            "peer-a".into(),
            "token-peer-a".into(),
            "user approved peer-a as collab master".into(),
        )
        .ok
    );

    // A non-master peer cannot retire another peer.
    let outsider = super::handle_worker_close(
        &server,
        "peer-b".into(),
        "token-peer-b".into(),
        "peer-a".into(),
        "trying to close the master".into(),
        false,
    );
    assert!(!outsider.ok);

    // The audit reason is mandatory.
    let no_reason = super::handle_worker_close(
        &server,
        "peer-a".into(),
        "token-peer-a".into(),
        "peer-b".into(),
        "   ".into(),
        false,
    );
    assert!(!no_reason.ok);
    assert!(no_reason.error.unwrap().contains("--reason"));

    // Master may not close itself into a headless project.
    let self_close = super::handle_worker_close(
        &server,
        "peer-a".into(),
        "token-peer-a".into(),
        "peer-a".into(),
        "self".into(),
        false,
    );
    assert!(!self_close.ok);
    assert!(self_close.error.unwrap().contains("cannot close itself"));

    // A worker holding live work keeps its registration; the task lifecycle
    // has to be resolved first or the worktree is stranded.
    server.commit(&[Event::TaskCreated {
        task: crate::server::state::TaskRec {
            id: "task-b".into(),
            owner: "peer-b".into(),
            created_by: "peer-a".into(),
            feature_id: None,
            worktree_path: None,
            branch: None,
            base_commit: None,
            priority: "p2".into(),
            status: "working".into(),
            next_step: None,
            wait: None,
            created_ms: now_ms(),
            updated_ms: now_ms(),
        },
    }]);
    let owns_work = super::handle_worker_close(
        &server,
        "peer-a".into(),
        "token-peer-a".into(),
        "peer-b".into(),
        "pane looks dead".into(),
        false,
    );
    assert!(!owns_work.ok);
    assert!(owns_work.error.unwrap().contains("task-b"));

    server.commit(&[Event::TaskUpdated {
        task: crate::server::state::TaskRec {
            id: "task-b".into(),
            owner: "peer-b".into(),
            created_by: "peer-a".into(),
            feature_id: None,
            worktree_path: None,
            branch: None,
            base_commit: None,
            priority: "p2".into(),
            status: "closed".into(),
            next_step: None,
            wait: None,
            created_ms: now_ms(),
            updated_ms: now_ms(),
        },
    }]);
    let closed = super::handle_worker_close(
        &server,
        "peer-a".into(),
        "token-peer-a".into(),
        "peer-b".into(),
        "pane dead after snapshot".into(),
        false,
    );
    assert!(closed.ok, "{}", closed.error.clone().unwrap_or_default());
    assert_eq!(closed.data["closed"], "peer-b");
    assert_eq!(closed.data["reason"], "pane dead after snapshot");
    assert_eq!(closed.data["killed_session"], false);

    let state = server.state.lock().unwrap();
    assert!(!state.workers.contains_key("peer-b"));
    assert!(!state.keepalives.contains_key("peer-b"));
    drop(state);

    std::fs::remove_dir_all(root).unwrap();
}

#[test]
fn master_promotion_requires_user_approval_and_existing_master_delegates() {
    let (server, root) = test_server();
    let worker_registration = register(&server, "peer-a", "%a");
    register(&server, "peer-b", "%b");
    assert_eq!(worker_registration.data["role_brief"]["role"], "worker");
    assert!(worker_registration.data["role_brief"]["role_task"]
        .as_str()
        .unwrap()
        .contains("independent task"));

    let missing =
        super::handle_master_promote(&server, "peer-a".into(), "token-peer-a".into(), "".into());
    assert!(!missing.ok);
    assert!(missing.error.unwrap().contains("approval"));

    let promoted = super::handle_master_promote(
        &server,
        "peer-a".into(),
        "token-peer-a".into(),
        "user approved peer-a as collab master".into(),
    );
    assert!(promoted.ok, "{}", promoted.error.unwrap_or_default());
    assert_eq!(promoted.data["mode"], "user_approved_self_promotion");
    assert_eq!(promoted.data["role_brief"]["role"], "master");
    assert!(promoted.data["role_brief"]["responsibilities"]
        .as_array()
        .unwrap()
        .iter()
        .any(|line| line.as_str().unwrap().contains("assign tasks")));
    let context = handle_context(&server, "peer-a".into(), "token-peer-a".into());
    assert_eq!(context.data["master"]["worker_id"], "peer-a");
    let status = super::handle_master_status(&server);
    assert_eq!(status.data["master"]["worker_id"], "peer-a");
    assert_eq!(
        status.data["master"]["approval"],
        "user approved peer-a as collab master"
    );

    let rejected = super::handle_master_promote(
        &server,
        "peer-b".into(),
        "token-peer-b".into(),
        "user approved peer-b as collab master".into(),
    );
    assert!(!rejected.ok);
    assert!(rejected
        .error
        .unwrap()
        .contains("only the registered master"));

    let outsider = super::handle_master_delegate(
        &server,
        "peer-b".into(),
        "token-peer-b".into(),
        "peer-a".into(),
    );
    assert!(!outsider.ok);
    assert!(outsider.error.unwrap().contains("master authority"));

    let delegated = super::handle_master_delegate(
        &server,
        "peer-a".into(),
        "token-peer-a".into(),
        "peer-b".into(),
    );
    assert!(delegated.ok, "{}", delegated.error.unwrap_or_default());
    assert_eq!(
        server.state.lock().unwrap().master_worker_id.as_deref(),
        Some("peer-b")
    );
    std::fs::remove_dir_all(root).unwrap();
}

#[test]
fn dead_master_pane_is_not_claimable_and_allows_approved_self_promote() {
    fn only_b(pane: &str) -> PanePresence {
        if pane == "%b" {
            PanePresence::Present
        } else {
            PanePresence::Missing
        }
    }
    let (mut server, root) = test_server();
    register(&server, "peer-a", "%a");
    register(&server, "peer-b", "%b");
    assert!(
        super::handle_master_promote(
            &server,
            "peer-a".into(),
            "token-peer-a".into(),
            "user approved peer-a as collab master".into(),
        )
        .ok
    );
    server.pane_alive_check = only_b;
    let status = super::handle_master_status(&server);
    assert!(status.data["master"].is_null(), "{status:?}");
    assert_eq!(status.data["recorded_unusable"]["worker_id"], "peer-a");
    let promoted = super::handle_master_promote(
        &server,
        "peer-b".into(),
        "token-peer-b".into(),
        "user approved peer-b after the previous master pane died".into(),
    );
    assert!(promoted.ok, "{}", promoted.error.unwrap_or_default());
    assert_eq!(
        server.state.lock().unwrap().master_worker_id.as_deref(),
        Some("peer-b")
    );
    let live = super::handle_master_status(&server);
    assert_eq!(live.data["master"]["worker_id"], "peer-b");
    assert!(live.data["recorded_unusable"].is_null());
    std::fs::remove_dir_all(root).unwrap();
}

#[test]
fn cross_project_send_requires_master_endpoints_on_both_sides() {
    let (server, root) = test_server();
    register(&server, "target-master", "%target-master");
    register(&server, "target-peer", "%target-peer");
    let promoted = super::handle_master_promote(
        &server,
        "target-master".into(),
        "token-target-master".into(),
        "user approved target-master as collab master".into(),
    );
    assert!(promoted.ok, "{}", promoted.error.unwrap_or_default());

    let denied_peer = super::handle_cross_project_send(
        &server,
        "appsdk-master".into(),
        "/tmp/appsdk".into(),
        "appsdk-operator".into(),
        None,
        1,
        "target-peer".into(),
        "feature".into(),
        "must reject non-master target".into(),
        None,
    );
    assert!(!denied_peer.ok);
    assert!(denied_peer
        .error
        .unwrap()
        .contains("target to be a live master"));

    let delivered = super::handle_cross_project_send(
        &server,
        "appsdk-master".into(),
        "/tmp/appsdk".into(),
        "appsdk-operator".into(),
        None,
        1,
        "target-master".into(),
        "feature".into(),
        "master-to-master message".into(),
        None,
    );
    assert!(delivered.ok, "{}", delivered.error.unwrap_or_default());
    let msg_id = delivered.data["msg_id"].as_str().unwrap();
    let state = server.state.lock().unwrap();
    assert_eq!(state.msgs[msg_id].to, "target-master");
    assert_eq!(state.msgs[msg_id].from, "appsdk-master@/tmp/appsdk");
    drop(state);

    let missing_approval = super::handle_cross_project_send(
        &server,
        "self-promoted".into(),
        "/tmp/appsdk".into(),
        "self-promoted".into(),
        None,
        1,
        "target-master".into(),
        "feature".into(),
        "must reject missing approval".into(),
        None,
    );
    assert!(!missing_approval.ok);
    assert!(missing_approval.error.unwrap().contains("user approval"));
    std::fs::remove_dir_all(root).unwrap();
}

#[test]
fn master_promotion_requires_live_tmux_pane() {
    fn none_alive(_: &str) -> PanePresence {
        PanePresence::Missing
    }
    let (mut server, root) = test_server();
    register(&server, "peer-a", "%a");
    server.pane_alive_check = none_alive;
    let denied = super::handle_master_promote(
        &server,
        "peer-a".into(),
        "token-peer-a".into(),
        "user approved peer-a as collab master".into(),
    );
    assert!(!denied.ok);
    assert!(denied.error.unwrap().contains("live tmux pane"));
    std::fs::remove_dir_all(root).unwrap();
}

#[test]
fn master_assigned_replays_approval_and_live_identity() {
    let event = Event::MasterAssigned {
        worker_id: "peer-a".into(),
        assigned_by: "peer-a".into(),
        approval: Some("user approved peer-a as collab master".into()),
        assigned_ms: 1,
    };
    let encoded = serde_json::to_string(&event).unwrap();
    let mut replay = State::default();
    replay.apply(&serde_json::from_str(&encoded).unwrap());
    assert_eq!(replay.master_worker_id.as_deref(), Some("peer-a"));
    assert_eq!(replay.master_assigned_by.as_deref(), Some("peer-a"));
    assert_eq!(
        replay.master_approval.as_deref(),
        Some("user approved peer-a as collab master")
    );
    assert_eq!(replay.master_assigned_ms, Some(1));
    let legacy = r#"{"ev":"RootAssigned","worker_id":"peer-b","assigned_by":"peer-a","approval":null,"assigned_ms":2}"#;
    let mut legacy_replay = State::default();
    legacy_replay.apply(&serde_json::from_str(legacy).unwrap());
    assert_eq!(legacy_replay.master_worker_id.as_deref(), Some("peer-b"));
}

#[test]
fn journal_root_assigned_rewrites_to_master_on_replay() {
    let root = std::env::temp_dir().join(format!(
        "collab-root-to-master-{}-{}",
        std::process::id(),
        now_ms()
    ));
    let server_dir = root.join(".agent-collab/server");
    std::fs::create_dir_all(&server_dir).unwrap();
    let journal = server_dir.join("journal.jsonl");
    std::fs::write(
        &journal,
        r#"{"ev":"RootAssigned","worker_id":"peer-a","assigned_by":"peer-a","approval":"user approved","assigned_ms":1}
"#,
    )
    .unwrap();
    let state = replay(&root).unwrap();
    assert_eq!(state.master_worker_id.as_deref(), Some("peer-a"));
    let rewritten = std::fs::read_to_string(&journal).unwrap();
    assert!(rewritten.contains("MasterAssigned"), "{rewritten}");
    assert!(!rewritten.contains("RootAssigned"), "{rewritten}");
    let again = replay(&root).unwrap();
    assert_eq!(again.master_worker_id.as_deref(), Some("peer-a"));
    std::fs::remove_dir_all(root).unwrap();
}

#[test]
fn first_and_later_registration_are_equal_peers() {
    let (server, root) = test_server();
    assert_eq!(
        register(&server, "peer-a", "%peer-a").data["identity_kind"],
        "peer"
    );
    assert_eq!(
        register(&server, "peer-b", "%peer-b").data["identity_kind"],
        "peer"
    );
    let state = server.state.lock().unwrap();
    assert_eq!(state.workers.len(), 2);
    assert!(state.master_worker_id.is_none());
    drop(state);
    std::fs::remove_dir_all(root).ok();
}

#[test]
fn registration_creates_one_finite_default_direct_message_subscription() {
    let (server, root) = test_server();
    assert!(register(&server, "peer", "%peer").ok);
    assert!(register(&server, "peer", "%peer").ok);
    let state = server.state.lock().unwrap();
    let subscriptions: Vec<_> = state
        .notification_subscriptions
        .values()
        .filter(|subscription| subscription.worker_id == "peer")
        .collect();
    assert_eq!(subscriptions.len(), 1);
    assert_eq!(subscriptions[0].event, "direct-message");
    assert_eq!(subscriptions[0].method, "tmux");
    assert_eq!(subscriptions[0].status, "armed");
    assert!(subscriptions[0].expires_ms > now_ms());
    drop(state);
    std::fs::remove_dir_all(root).ok();
}

#[test]
fn cancelled_default_lease_stays_suppressed_until_explicit_subscribe() {
    let (server, root) = test_server();
    assert!(register(&server, "peer", "%peer").ok);

    let cancelled = handle_notification_unsubscribe(
        &server,
        "peer".into(),
        "token-peer".into(),
        "sub-default-direct-message-peer".into(),
    );
    assert!(cancelled.ok, "{}", cancelled.error.unwrap_or_default());
    assert!(register(&server, "peer", "%peer").ok);

    let state = server.state.lock().unwrap();
    assert_eq!(
        state.notification_subscriptions["sub-default-direct-message-peer"].status,
        "cancelled"
    );
    assert!(default_direct_message_events(&state, "peer", "%peer", now_ms()).is_empty());
    drop(state);

    let explicit = handle_notification_subscribe(
        &server,
        "peer".into(),
        "token-peer".into(),
        "direct-message".into(),
        None,
        None,
        Vec::new(),
        None,
        1,
        3_600,
    );
    assert!(explicit.ok, "{}", explicit.error.unwrap_or_default());
    let explicit_id = explicit.data["subscription"]["id"].as_str().unwrap();
    assert_ne!(explicit_id, "sub-default-direct-message-peer");
    assert_eq!(
        explicit.data["subscription"]["status"],
        serde_json::Value::String("armed".into())
    );

    std::fs::remove_dir_all(root).ok();
}

#[test]
fn registration_adds_default_lease_when_only_short_direct_message_lease_exists() {
    let mut state = State::default();
    let now = 10_000;
    state.apply(&Event::NotificationSubscribed {
        subscription: NotificationSubscription {
            id: "sub-short".into(),
            worker_id: "peer".into(),
            event: "direct-message".into(),
            subject: None,
            pane: "%peer".into(),
            method: "tmux".into(),
            trigger_ms: None,
            trigger_times_ms: Vec::new(),
            interval_ms: None,
            repeat_count: 1,
            fired_count: 0,
            expires_ms: now + 600_000,
            status: "armed".into(),
            created_ms: now,
            updated_ms: now,
            status_reason: None,
        },
    });

    let events = default_direct_message_events(&state, "peer", "%peer", now);
    let default = events
        .iter()
        .find_map(|event| match event {
            Event::NotificationSubscribed { subscription } => Some(subscription),
            _ => None,
        })
        .expect("a short explicit lease must not suppress the default peer lease");
    assert_eq!(default.id, "sub-default-direct-message-peer");
    assert_eq!(
        default.expires_ms,
        now + DEFAULT_DIRECT_MESSAGE_TTL_SECONDS as i64 * 1000
    );

    for event in &events {
        state.apply(event);
    }
    assert_eq!(
        state.notification_subscriptions["sub-short"].status,
        "rebound"
    );
    assert!(default_direct_message_events(&state, "peer", "%peer", now + 1).is_empty());
}

#[test]
fn daemon_replay_restores_default_lease_for_registered_peer() {
    let mut state = State::default();
    state.apply(&Event::Registered {
        worker: WorkerRec {
            id: "peer".into(),
            token: "token-peer".into(),
            pane: Some("%peer".into()),
            cwd: "/tmp".into(),
            registered_ms: 1,
        },
    });

    let events = registered_peer_default_events(&state, 10_000, &|worker, pane| {
        worker == "peer" && pane == "%peer"
    });
    assert!(events.iter().any(|event| matches!(
        event,
        Event::NotificationSubscribed { subscription }
            if subscription.id == "sub-default-direct-message-peer"
    )));
}

#[test]
fn daemon_restart_rebinds_stale_pane_before_restoring_default_lease() {
    let mut state = State::default();
    state.apply(&Event::Registered {
        worker: WorkerRec {
            id: "peer".into(),
            token: "token-peer".into(),
            pane: Some("%stale".into()),
            cwd: "/tmp".into(),
            registered_ms: 1,
        },
    });

    let events = registered_peer_rebind_events(
        &state,
        &|worker| (worker == "peer").then(|| "%current".into()),
        &|worker, pane| worker == "peer" && pane == "%current",
    );
    assert_eq!(events.len(), 1);
    for event in events {
        state.apply(&event);
    }
    assert_eq!(state.workers["peer"].pane.as_deref(), Some("%current"));

    let lease_events = registered_peer_default_events(&state, 10_000, &|worker, pane| {
        worker == "peer" && pane == "%current"
    });
    assert!(lease_events.iter().any(|event| matches!(
        event,
        Event::NotificationSubscribed { subscription }
            if subscription.worker_id == "peer" && subscription.pane == "%current"
    )));
}

#[test]
fn daemon_restart_rebinds_existing_deadline_without_recreating_it() {
    let mut state = State::default();
    state.apply(&Event::Registered {
        worker: WorkerRec {
            id: "peer".into(),
            token: "token-peer".into(),
            pane: Some("%stale".into()),
            cwd: "/tmp".into(),
            registered_ms: 1,
        },
    });
    let original = NotificationSubscription {
        id: "sub-goal".into(),
        worker_id: "peer".into(),
        event: "deadline".into(),
        subject: Some("goal:sha256:test".into()),
        pane: "%stale".into(),
        method: "tmux".into(),
        trigger_ms: Some(20_000),
        trigger_times_ms: Vec::new(),
        interval_ms: None,
        repeat_count: 1,
        fired_count: 0,
        expires_ms: 60_000,
        status: "armed".into(),
        created_ms: 1,
        updated_ms: 1,
        status_reason: None,
    };
    state.apply(&Event::NotificationSubscribed {
        subscription: original.clone(),
    });

    let events = registered_peer_rebind_events(
        &state,
        &|worker| (worker == "peer").then(|| "%current".into()),
        &|worker, pane| worker == "peer" && pane == "%current",
    );
    assert!(events.iter().any(|event| matches!(
        event,
        Event::NotificationRebound { subscription_id, pane, .. }
            if subscription_id == "sub-goal" && pane == "%current"
    )));
    assert!(!events.iter().any(|event| matches!(
        event,
        Event::NotificationSubscribed { subscription }
            if subscription.id == "sub-goal"
    )));
    for event in &events {
        state.apply(event);
    }
    let rebound = &state.notification_subscriptions["sub-goal"];
    assert_eq!(rebound.id, original.id);
    assert_eq!(rebound.pane, "%current");
    assert_eq!(rebound.trigger_ms, original.trigger_ms);
    assert_eq!(rebound.expires_ms, original.expires_ms);
    assert_eq!(rebound.status, "armed");
}

#[test]
fn daemon_restart_does_not_guess_between_multiple_or_unowned_panes() {
    let mut state = State::default();
    state.apply(&Event::Registered {
        worker: WorkerRec {
            id: "peer".into(),
            token: "token-peer".into(),
            pane: Some("%stale".into()),
            cwd: "/tmp".into(),
            registered_ms: 1,
        },
    });

    assert!(registered_peer_rebind_events(&state, &|_| None, &|_, _| true).is_empty());
    assert!(
        registered_peer_rebind_events(&state, &|_| Some("%foreign".into()), &|_, _| false,)
            .is_empty()
    );
    assert_eq!(state.workers["peer"].pane.as_deref(), Some("%stale"));
}

#[test]
fn expired_mailbox_and_journal_are_removed_and_do_not_replay() {
    let (server, root) = test_server();
    assert!(register(&server, "peer", "%peer").ok);
    let now = now_ms();
    let old_id = "m-old".to_string();
    let fresh_id = "m-fresh".to_string();
    server.commit(&[
        Event::Sent {
            msg: Message {
                id: old_id.clone(),
                from: "peer".into(),
                to: "peer".into(),
                mtype: "notify".into(),
                subject: Some("old".into()),
                body: "expired body".into(),
                in_reply_to: None,
                created_ms: now - 8 * 86_400_000,
                state: "read".into(),
                wake_attempt_count: 1,
                last_wake_attempt_ms: now - 8 * 86_400_000,
            },
        },
        Event::Sent {
            msg: Message {
                id: fresh_id.clone(),
                from: "peer".into(),
                to: "peer".into(),
                mtype: "notify".into(),
                subject: Some("new".into()),
                body: "fresh body".into(),
                in_reply_to: None,
                created_ms: now,
                state: "pending".into(),
                wake_attempt_count: 0,
                last_wake_attempt_ms: 0,
            },
        },
    ]);
    let mailbox = root.join(".agent-collab/mailbox");
    assert!(mailbox.join("m-old.json").exists());
    assert!(mailbox.join("m-fresh.json").exists());
    let journal_before = std::fs::read_to_string(root.join(".agent-collab/server/journal.jsonl"))
        .unwrap()
        .lines()
        .count();

    assert_eq!(purge_expired_storage(&server, now), 1);
    let state = server.state.lock().unwrap();
    assert!(!state.msgs.contains_key(&old_id));
    assert!(state.msgs.contains_key(&fresh_id));
    assert!(state.workers.contains_key("peer"));
    drop(state);
    assert!(!mailbox.join("m-old.json").exists());
    assert!(mailbox.join("m-fresh.json").exists());
    let journal = std::fs::read_to_string(root.join(".agent-collab/server/journal.jsonl")).unwrap();
    assert!(!journal.contains("m-old"));
    assert!(journal.contains("m-fresh"));
    assert!(journal.lines().count() < journal_before);

    let replayed = replay(&root).unwrap();
    assert!(!replayed.msgs.contains_key(&old_id));
    assert_eq!(replayed.msgs[&fresh_id].body, "fresh body");
    assert_eq!(replayed.workers["peer"].pane.as_deref(), Some("%peer"));
    std::fs::remove_dir_all(root).ok();
}

#[test]
fn retention_skips_fresh_messages_and_frozen_admission() {
    let (server, root) = test_server();
    assert!(register(&server, "peer", "%peer").ok);
    let now = now_ms();
    server.commit(&[Event::Sent {
        msg: Message {
            id: "m-keep".into(),
            from: "peer".into(),
            to: "peer".into(),
            mtype: "notify".into(),
            subject: Some("keep".into()),
            body: "keep".into(),
            in_reply_to: None,
            created_ms: now - 86_400_000,
            state: "pending".into(),
            wake_attempt_count: 0,
            last_wake_attempt_ms: 0,
        },
    }]);
    assert_eq!(purge_expired_storage(&server, now), 0);
    assert!(server.state.lock().unwrap().msgs.contains_key("m-keep"));

    server.commit(&[Event::Sent {
        msg: Message {
            id: "m-old-frozen".into(),
            from: "peer".into(),
            to: "peer".into(),
            mtype: "notify".into(),
            subject: Some("old".into()),
            body: "old".into(),
            in_reply_to: None,
            created_ms: now - 8 * 86_400_000,
            state: "read".into(),
            wake_attempt_count: 0,
            last_wake_attempt_ms: 0,
        },
    }]);
    server.commit(&[Event::MigrationUpdated {
        migration: MigrationRecord {
            id: "migration".into(),
            from_version: "v1".into(),
            to_version: "v1".into(),
            phase: "applied".into(),
            admission_frozen: true,
            snapshot_hash: None,
            worker_count: 1,
            task_count: 0,
            message_count: 2,
            operator: "peer".into(),
            issues: Vec::new(),
            created_ms: now,
            updated_ms: now,
        },
    }]);
    assert_eq!(purge_expired_storage(&server, now), 0);
    assert!(server
        .state
        .lock()
        .unwrap()
        .msgs
        .contains_key("m-old-frozen"));
    std::fs::remove_dir_all(root).ok();
}

#[test]
fn fresh_default_does_not_skip_legacy_duplicate_cleanup() {
    let mut state = State::default();
    for event in default_direct_message_events(&state, "peer", "%one", 1000) {
        state.apply(&event);
    }
    let mut old = state
        .notification_subscriptions
        .values()
        .next()
        .unwrap()
        .clone();
    old.id = "sub-legacy".into();
    state.apply(&Event::NotificationSubscribed { subscription: old });
    for event in default_direct_message_events(&state, "peer", "%one", 2000) {
        state.apply(&event);
    }
    assert_eq!(
        state
            .notification_subscriptions
            .values()
            .filter(|sub| sub.status == "armed")
            .count(),
        1
    );
    assert_eq!(
        state.notification_subscriptions["sub-legacy"].status,
        "rebound"
    );
    assert!(default_direct_message_events(&state, "peer", "%one", 2000).is_empty());
}

#[test]
fn default_subscription_renews_and_rebinds_without_new_ids() {
    let mut state = State::default();
    for event in default_direct_message_events(&state, "peer", "%one", 1000) {
        state.apply(&event);
    }
    let id = state
        .notification_subscriptions
        .keys()
        .next()
        .unwrap()
        .clone();
    let ttl = DEFAULT_DIRECT_MESSAGE_TTL_SECONDS as i64 * 1000;
    for (pane, time) in [("%one", ttl), ("%one", ttl * 3), ("%two", ttl * 4)] {
        for event in default_direct_message_events(&state, "peer", pane, time) {
            state.apply(&event);
        }
        assert_eq!(state.notification_subscriptions.len(), 1);
        let sub = &state.notification_subscriptions[&id];
        assert_eq!(sub.pane, pane);
        assert_eq!(sub.status, "armed");
        assert_eq!(sub.expires_ms, time + ttl);
    }
}

#[test]
fn replay_removes_legacy_declared_roles() {
    let mut state = State::default();
    for (id, role) in [("legacy-master", "master"), ("legacy-worker", "worker")] {
        let json = format!(
            r#"{{"ev":"Registered","worker":{{"id":"{id}","token":"token-{id}","pane":"%{id}","cwd":"/tmp","registered_ms":1,"role":"{role}"}}}}"#
        );
        let event: Event = serde_json::from_str(&json).unwrap();
        state.apply(&event);
    }
    assert!(state
        .workers
        .values()
        .all(|worker| { serde_json::to_value(worker).unwrap().get("role").is_none() }));
}

#[test]
fn only_owner_mutates_and_closes_task() {
    let (server, root) = test_server();
    register(&server, "peer-a", "%peer-a");
    register(&server, "peer-b", "%peer-b");
    assert!(create_task(&server, "peer-a", "task-a", "feature-a").ok);

    let update = handle_task_update(
        &server,
        "peer-b".into(),
        "token-peer-b".into(),
        "task-a".into(),
        Some("verifying".into()),
        None,
    );
    assert!(!update.ok);
    let close = handle_task_close(
        &server,
        "peer-b".into(),
        "token-peer-b".into(),
        "task-a".into(),
        false,
        None,
    );
    assert!(!close.ok);
    assert_eq!(
        server.state.lock().unwrap().tasks["task-a"].status,
        "working"
    );
    std::fs::remove_dir_all(root).ok();
}

#[test]
fn owner_completes_local_lifecycle_without_peer_reports() {
    let (server, root) = test_server();
    register(&server, "peer", "%peer");
    assert!(create_task(&server, "peer", "task", "feature").ok);
    for status in ["verifying", "reviewed"] {
        assert!(
            handle_task_update(
                &server,
                "peer".into(),
                "token-peer".into(),
                "task".into(),
                Some(status.into()),
                Some(format!("continue {status}")),
            )
            .ok
        );
    }
    let delivered = handle_task_deliver(
        &server,
        "peer".into(),
        "token-peer".into(),
        "task".into(),
        Some("tests and candidate commit verified".into()),
        Some("/tmp/task-worktree".into()),
    );
    assert!(delivered.ok);
    assert_eq!(delivered.data["notification"], "none");
    assert!(
        handle_task_review(
            &server,
            "peer".into(),
            "token-peer".into(),
            "task".into(),
            true,
            false,
            "reviewed candidate".into(),
        )
        .ok
    );
    initialize_main(&root);
    assert!(
        handle_task_integrated(
            &server,
            "peer".into(),
            "token-peer".into(),
            "task".into(),
            current_head(&root),
            "main verified".into(),
        )
        .ok
    );
    let closed = handle_task_close(
        &server,
        "peer".into(),
        "token-peer".into(),
        "task".into(),
        false,
        None,
    );
    assert!(closed.ok);
    let state = server.state.lock().unwrap();
    assert_eq!(state.tasks["task"].status, "closed");
    assert_eq!(state.cleanup_receipts["task"].task_id, "task");
    assert!(
        state.msgs.is_empty(),
        "normal lifecycle must not report to peers"
    );
    drop(state);
    std::fs::remove_dir_all(root).ok();
}

#[test]
fn master_force_close_skips_owner_and_cleanup_requirements() {
    let (server, root) = test_server();
    register(&server, "owner", "%owner");
    register(&server, "master", "%master");
    server.commit(&[Event::MasterAssigned {
        worker_id: "master".into(),
        assigned_by: "master".into(),
        approval: Some("user approved force-close test".into()),
        assigned_ms: now_ms(),
    }]);
    let now = now_ms();
    server.commit(&[Event::TaskCreated {
        task: TaskRec {
            id: "stuck".into(),
            owner: "owner".into(),
            created_by: "owner".into(),
            feature_id: None,
            worktree_path: Some("playground/stuck".into()),
            branch: Some("codex/stuck".into()),
            base_commit: Some("base".into()),
            priority: default_priority(),
            status: "blocked".into(),
            next_step: None,
            wait: None,
            created_ms: now,
            updated_ms: now,
        },
    }]);
    let resp = handle_task_close(
        &server,
        "master".into(),
        "token-master".into(),
        "stuck".into(),
        true,
        Some("worktree dirty and merge blocked; force closing per master".into()),
    );
    assert!(resp.ok, "{}", resp.error.unwrap_or_default());
    let state = server.state.lock().unwrap();
    assert_eq!(state.tasks["stuck"].status, "closed");
    assert_eq!(
        state.cleanup_receipts["stuck"].manual_reason.as_deref(),
        Some("worktree dirty and merge blocked; force closing per master"),
    );
    drop(state);
    std::fs::remove_dir_all(root).ok();
}

#[test]
fn non_master_force_close_is_rejected() {
    let (server, root) = test_server();
    register(&server, "owner", "%owner");
    register(&server, "peer", "%peer");
    let now = now_ms();
    server.commit(&[Event::TaskCreated {
        task: TaskRec {
            id: "stuck".into(),
            owner: "owner".into(),
            created_by: "owner".into(),
            feature_id: None,
            worktree_path: Some("playground/stuck".into()),
            branch: Some("codex/stuck".into()),
            base_commit: Some("base".into()),
            priority: default_priority(),
            status: "working".into(),
            next_step: None,
            wait: None,
            created_ms: now,
            updated_ms: now,
        },
    }]);
    let resp = handle_task_close(
        &server,
        "peer".into(),
        "token-peer".into(),
        "stuck".into(),
        true,
        Some("not authorized".into()),
    );
    assert!(!resp.ok);
    let state = server.state.lock().unwrap();
    assert_eq!(state.tasks["stuck"].status, "working");
    drop(state);
    std::fs::remove_dir_all(root).ok();
}

#[test]
fn orphan_force_close_defers_when_owner_pane_probe_is_unknown() {
    let (server, root) = test_server();
    register(&server, "owner", "%owner");
    register(&server, "peer", "%peer");
    assert!(create_task(&server, "owner", "working", "feature").ok);
    let mut server = server;
    server.pane_owner_check = |worker_id, pane| {
        if worker_id == "owner" && pane == "%owner" {
            Err(())
        } else {
            Ok(true)
        }
    };
    let resp = handle_task_close(
        &server,
        "peer".into(),
        "token-peer".into(),
        "working".into(),
        true,
        Some("owner pane probe is unknown; defer orphan close".into()),
    );
    assert!(!resp.ok);
    assert_eq!(
        resp.error.as_deref(),
        Some("manual force close is not authorized for this caller")
    );
    assert_eq!(
        server.state.lock().unwrap().tasks["working"].status,
        "working"
    );
    std::fs::remove_dir_all(root).ok();
}

#[test]
fn owner_force_close_when_no_live_master_is_allowed() {
    let (server, root) = test_server();
    register(&server, "owner", "%owner");
    let now = now_ms();
    server.commit(&[Event::TaskCreated {
        task: TaskRec {
            id: "orphan".into(),
            owner: "owner".into(),
            created_by: "owner".into(),
            feature_id: None,
            worktree_path: None,
            branch: None,
            base_commit: None,
            priority: default_priority(),
            status: "blocked".into(),
            next_step: None,
            wait: None,
            created_ms: now,
            updated_ms: now,
        },
    }]);
    let resp = handle_task_close(
        &server,
        "owner".into(),
        "token-owner".into(),
        "orphan".into(),
        true,
        Some("master unreachable; owner closes".into()),
    );
    assert!(resp.ok, "{}", resp.error.unwrap_or_default());
    let state = server.state.lock().unwrap();
    assert_eq!(state.tasks["orphan"].status, "closed");
    drop(state);
    std::fs::remove_dir_all(root).ok();
}

#[test]
fn registered_peer_force_closes_orphaned_owner_with_no_live_master() {
    let (server, root) = test_server();
    register(&server, "owner", "%owner");
    register(&server, "peer", "%peer");
    assert!(create_task(&server, "owner", "orphan", "feature").ok);
    let mut server = server;
    server.pane_alive_check = |pane| {
        if pane != "%owner" {
            PanePresence::Present
        } else {
            PanePresence::Missing
        }
    };
    let resp = handle_task_close(
        &server,
        "peer".into(),
        "token-peer".into(),
        "orphan".into(),
        true,
        Some("owner pane lost; no live master; peer closes orphan".into()),
    );
    assert!(resp.ok, "{}", resp.error.unwrap_or_default());
    let state = server.state.lock().unwrap();
    assert_eq!(state.tasks["orphan"].status, "closed");
    assert_eq!(
        state.cleanup_receipts["orphan"].manual_reason.as_deref(),
        Some("owner pane lost; no live master; peer closes orphan"),
    );
    drop(state);
    std::fs::remove_dir_all(root).ok();
}

#[test]
fn repeated_orphan_force_close_is_idempotent_after_journal_replay() {
    let (server, root) = test_server();
    register(&server, "owner", "%owner");
    register(&server, "peer", "%peer");
    assert!(create_task(&server, "owner", "orphan", "feature").ok);
    let mut server = server;
    server.pane_alive_check = |pane| {
        if pane == "%owner" {
            PanePresence::Missing
        } else {
            PanePresence::Present
        }
    };
    let reason = "owner pane lost; replay closes the same orphan";
    let first = handle_task_close(
        &server,
        "peer".into(),
        "token-peer".into(),
        "orphan".into(),
        true,
        Some(reason.into()),
    );
    assert!(first.ok, "{}", first.error.unwrap_or_default());
    let receipt_id = first.data["receipt_id"].as_str().unwrap().to_owned();
    let task_updated_ms = server.state.lock().unwrap().tasks["orphan"].updated_ms;
    let journal_path = root.join(".agent-collab/server/journal.jsonl");
    let journal_after_first = std::fs::read_to_string(&journal_path).unwrap();
    *server.state.lock().unwrap() = replay(&root).unwrap();

    let second = handle_task_close(
        &server,
        "peer".into(),
        "token-peer".into(),
        "orphan".into(),
        true,
        Some(reason.into()),
    );
    assert!(second.ok, "{}", second.error.unwrap_or_default());
    assert_eq!(second.data["idempotent"], true);
    assert_eq!(second.data["receipt_id"], receipt_id);
    assert_eq!(
        server.state.lock().unwrap().tasks["orphan"].updated_ms,
        task_updated_ms
    );
    assert_eq!(
        std::fs::read_to_string(&journal_path).unwrap(),
        journal_after_first
    );
    std::fs::remove_dir_all(root).ok();
}

#[test]
fn worktree_claim_requires_cleanup_and_cannot_cancel() {
    let (server, root) = test_server();
    register(&server, "peer", "%peer");
    let now = now_ms();
    server.commit(&[Event::TaskCreated {
        task: TaskRec {
            id: "task".into(),
            owner: "peer".into(),
            created_by: "peer".into(),
            feature_id: Some("feature".into()),
            worktree_path: Some("playground/task-wt".into()),
            branch: Some("codex/task-wt".into()),
            base_commit: Some("base".into()),
            priority: default_priority(),
            status: "working".into(),
            next_step: None,
            wait: None,
            created_ms: now,
            updated_ms: now,
        },
    }]);
    let cancelled = handle_task_update(
        &server,
        "peer".into(),
        "token-peer".into(),
        "task".into(),
        Some("cancelled".into()),
        None,
    );
    assert!(!cancelled.ok);
    assert_eq!(
        cancelled.error.as_deref(),
        Some(
            "CLEANUP_REQUIRED_BEFORE_CANCEL: task owns a worktree; close only after merged cleanup"
        )
    );
    std::fs::remove_dir_all(root).ok();
}

#[test]
fn merged_worktree_without_cleanup_receipt_fails_audit() {
    let (server, root) = test_server();
    let now = now_ms();
    server.commit(&[Event::TaskCreated {
        task: TaskRec {
            id: "merged-task".into(),
            owner: "peer".into(),
            created_by: "peer".into(),
            feature_id: None,
            worktree_path: Some("playground/merged-wt".into()),
            branch: Some("codex/merged-wt".into()),
            base_commit: None,
            priority: default_priority(),
            status: "merged".into(),
            next_step: None,
            wait: None,
            created_ms: now,
            updated_ms: now,
        },
    }]);
    let issues = migration_issues(&server, &server.state.lock().unwrap());
    assert!(issues
        .iter()
        .any(|issue| issue.starts_with("TASK_CLEANUP_INCOMPLETE:merged-task:")));
    std::fs::remove_dir_all(root).ok();
}

#[test]
fn conflict_is_durable_and_wait_targets_resource_holder() {
    let (server, root) = test_server();
    register(&server, "holder", "%holder");
    register(&server, "waiter", "%waiter");
    assert!(create_task(&server, "holder", "held", "shared-feature").ok);
    let conflict = create_task(&server, "waiter", "waiting", "shared-feature");
    assert!(!conflict.ok);
    assert_eq!(conflict.error.as_deref(), Some("TASK_RESOURCE_CONFLICT"));
    assert_eq!(conflict.data["responsible_actor"], "holder");
    assert_eq!(server.state.lock().unwrap().msgs.len(), 0);
    assert_eq!(
        conflict.data["notification"],
        "none; use explicit sendmessage when coordination is needed"
    );

    let waiting = handle_task_wait(
        &server,
        "waiter".into(),
        "token-waiter".into(),
        "waiting".into(),
        "held".into(),
    );
    assert!(waiting.ok);
    let state = server.state.lock().unwrap();
    let wait = state.tasks["waiting"].wait.as_ref().unwrap();
    assert_eq!(wait.waiter, "waiter");
    assert_eq!(wait.responsible_actor, "holder");
    assert!(wait.deadline_ms > now_ms());
    assert!(wait.resume_on.contains(&"resource_released".into()));
    assert_eq!(wait.escalation, "resource_owner_and_waiter_recheck");
    drop(state);
    std::fs::remove_dir_all(root).ok();
}

#[test]
fn holder_close_persists_release_only_for_waiter() {
    let (server, root) = test_server();
    register(&server, "holder", "%holder");
    register(&server, "waiter", "%waiter");
    assert!(create_task(&server, "holder", "held", "shared-feature").ok);
    assert!(!create_task(&server, "waiter", "waiting", "shared-feature").ok);
    assert!(
        handle_task_wait(
            &server,
            "waiter".into(),
            "token-waiter".into(),
            "waiting".into(),
            "held".into(),
        )
        .ok
    );
    assert!(
        handle_notification_subscribe(
            &server,
            "waiter".into(),
            "token-waiter".into(),
            "resource-released".into(),
            Some("held".into()),
            None,
            Vec::new(),
            None,
            1,
            60,
        )
        .ok
    );
    for status in ["verifying", "reviewed"] {
        assert!(
            handle_task_update(
                &server,
                "holder".into(),
                "token-holder".into(),
                "held".into(),
                Some(status.into()),
                Some(format!("continue {status}")),
            )
            .ok
        );
    }
    assert!(
        handle_task_deliver(
            &server,
            "holder".into(),
            "token-holder".into(),
            "held".into(),
            Some("candidate verified".into()),
            Some("/tmp/holder-worktree".into()),
        )
        .ok
    );
    assert!(
        handle_task_review(
            &server,
            "holder".into(),
            "token-holder".into(),
            "held".into(),
            true,
            false,
            "reviewed candidate".into(),
        )
        .ok
    );
    initialize_main(&root);
    assert!(
        handle_task_integrated(
            &server,
            "holder".into(),
            "token-holder".into(),
            "held".into(),
            current_head(&root),
            "main verified".into(),
        )
        .ok
    );
    assert!(
        handle_task_close(
            &server,
            "holder".into(),
            "token-holder".into(),
            "held".into(),
            false,
            None,
        )
        .ok
    );

    let state = server.state.lock().unwrap();
    assert_eq!(state.tasks["waiting"].status, "blocked");
    assert!(state.tasks["waiting"].wait.is_none());
    assert!(state.tasks["waiting"]
        .next_step
        .as_deref()
        .unwrap()
        .starts_with("RESOURCE_RELEASED=held"));
    let releases: Vec<&Message> = state
        .msgs
        .values()
        .filter(|message| message.body.starts_with("RESOURCE_RELEASED "))
        .collect();
    assert_eq!(releases.len(), 1);
    assert_eq!(releases[0].to, "waiter");
    assert!(state
        .msgs
        .values()
        .all(|message| !message.body.starts_with("TASK_CLOSED ")));
    drop(state);
    std::fs::remove_dir_all(root).ok();
}

#[test]
fn direct_two_peer_and_three_peer_wait_cycles_fail_closed() {
    let (server, root) = test_server();
    for (id, pane) in [("a", "%a"), ("b", "%b"), ("c", "%c")] {
        register(&server, id, pane);
    }
    assert!(create_task(&server, "a", "a-task", "a-feature").ok);
    let direct = handle_task_wait(
        &server,
        "a".into(),
        "token-a".into(),
        "a-task".into(),
        "a-task".into(),
    );
    assert!(!direct.ok);
    assert_eq!(direct.error.as_deref(), Some("WAIT_CYCLE_DETECTED"));

    let now = now_ms();
    let make = |id: &str, owner: &str, waiting_for: Option<&str>| TaskRec {
        id: id.into(),
        owner: owner.into(),
        created_by: owner.into(),
        feature_id: Some("shared".into()),
        worktree_path: None,
        branch: None,
        base_commit: None,
        priority: default_priority(),
        status: if waiting_for.is_some() {
            "waiting"
        } else {
            "blocked"
        }
        .into(),
        next_step: None,
        wait: waiting_for.map(|blocking| WaitSpec {
            waiter: owner.into(),
            waiting_for: blocking.into(),
            responsible_actor: "a".into(),
            reason: "resource_conflict".into(),
            deadline_ms: now + 60_000,
            resume_on: vec!["resource_released".into()],
            escalation: "resource_owner_and_waiter_recheck".into(),
        }),
        created_ms: now,
        updated_ms: now,
    };
    server.commit(&[
        Event::TaskUpdated {
            task: make("a-task", "a", None),
        },
        Event::TaskCreated {
            task: make("b-task", "b", Some("a-task")),
        },
    ]);
    let two = handle_task_wait(
        &server,
        "a".into(),
        "token-a".into(),
        "a-task".into(),
        "b-task".into(),
    );
    assert_eq!(two.error.as_deref(), Some("WAIT_CYCLE_DETECTED"));

    server.commit(&[
        Event::TaskUpdated {
            task: make("b-task", "b", Some("c-task")),
        },
        Event::TaskCreated {
            task: make("c-task", "c", Some("a-task")),
        },
    ]);
    let three = handle_task_wait(
        &server,
        "a".into(),
        "token-a".into(),
        "a-task".into(),
        "b-task".into(),
    );
    assert_eq!(three.error.as_deref(), Some("WAIT_CYCLE_DETECTED"));
    std::fs::remove_dir_all(root).ok();
}

#[test]
fn terminal_or_delivered_task_cannot_wait() {
    let (server, root) = test_server();
    register(&server, "a", "%a");
    register(&server, "b", "%b");
    assert!(create_task(&server, "a", "a-task", "a-feature").ok);
    assert!(create_task(&server, "b", "b-task", "b-feature").ok);
    server
        .state
        .lock()
        .unwrap()
        .tasks
        .get_mut("a-task")
        .unwrap()
        .status = "delivered".into();
    let response = handle_task_wait(
        &server,
        "a".into(),
        "token-a".into(),
        "a-task".into(),
        "b-task".into(),
    );
    assert!(!response.ok);
    std::fs::remove_dir_all(root).ok();
}

#[test]
fn peer_migration_freezes_snapshot_and_resumes_after_verify() {
    let (server, root) = test_server();
    register(&server, "peer", "%peer");
    assert!(handle_migration_inspect(&server, "peer".into(), "token-peer".into()).ok);
    assert!(handle_migration_plan(&server, "peer".into(), "token-peer".into()).ok);
    let applied = handle_migration_apply(&server, "peer".into(), "token-peer".into());
    assert!(applied.ok);
    assert!(applied.data["admission_frozen"].as_bool().unwrap());
    let verified = handle_migration_verify(&server, "peer".into(), "token-peer".into());
    assert!(verified.ok);
    assert!(verified.data["verified"].as_bool().unwrap());
    assert!(!server.state.lock().unwrap().admission_frozen());
    let repeated = handle_migration_verify(&server, "peer".into(), "token-peer".into());
    assert!(repeated.ok);
    assert_eq!(repeated.data["verified"], true);
    assert_eq!(repeated.data["idempotent"], true);
    assert_eq!(repeated.data["resumed"], false);
    assert!(repeated.data["next"]
        .as_str()
        .unwrap()
        .contains("do not rerun"));
    std::fs::remove_dir_all(root).ok();
}

#[test]
fn migration_transaction_lease_rejects_second_peer() {
    let (server, root) = test_server();
    register(&server, "peer-a", "%peer-a");
    register(&server, "peer-b", "%peer-b");
    assert!(handle_migration_plan(&server, "peer-a".into(), "token-peer-a".into()).ok);
    let second = handle_migration_plan(&server, "peer-b".into(), "token-peer-b".into());
    assert!(!second.ok);
    assert_eq!(
        second.error.as_deref(),
        Some("MIGRATION_TRANSACTION_HELD_BY_ANOTHER_PEER")
    );
    assert_eq!(second.data["holder"], "peer-a");
    assert_eq!(second.data["requester"], "peer-b");
    assert_eq!(second.data["retry_allowed"], false);
    assert!(second.data["next"]
        .as_str()
        .unwrap()
        .contains("do not retry"));
    std::fs::remove_dir_all(root).ok();
}

#[test]
fn migration_verify_rejection_exposes_current_state_and_stops_retry() {
    let (server, root) = test_server();
    register(&server, "peer", "%peer");
    let response = handle_migration_verify(&server, "peer".into(), "token-peer".into());
    assert!(!response.ok);
    assert_eq!(
        response.error.as_deref(),
        Some("no migration record to verify")
    );
    assert_eq!(response.data["retry_allowed"], false);
    assert!(response.data["next"].as_str().unwrap().contains("inspect"));
    std::fs::remove_dir_all(root).ok();
}

#[test]
fn migration_rejects_wait_without_matching_active_resource_holder() {
    let (server, root) = test_server();
    register(&server, "holder", "%holder");
    register(&server, "waiter", "%waiter");
    assert!(create_task(&server, "holder", "held", "shared-feature").ok);
    assert!(!create_task(&server, "waiter", "waiting", "shared-feature").ok);
    assert!(
        handle_task_wait(
            &server,
            "waiter".into(),
            "token-waiter".into(),
            "waiting".into(),
            "held".into(),
        )
        .ok
    );
    server
        .state
        .lock()
        .unwrap()
        .tasks
        .get_mut("held")
        .unwrap()
        .status = "closed".into();

    let inspected = handle_migration_inspect(&server, "waiter".into(), "token-waiter".into());
    assert!(inspected.ok);
    assert!(inspected.data["issues"]
        .as_array()
        .unwrap()
        .iter()
        .any(|issue| issue
            .as_str()
            .unwrap()
            .contains("inactive blocking task held")));

    std::fs::remove_dir_all(root).ok();
}

#[test]
fn changed_migration_snapshot_remains_frozen() {
    let (server, root) = test_server();
    register(&server, "peer", "%peer");
    assert!(handle_migration_plan(&server, "peer".into(), "token-peer".into()).ok);
    assert!(handle_migration_apply(&server, "peer".into(), "token-peer".into()).ok);

    let now = now_ms();
    server.commit(&[Event::TaskCreated {
        task: TaskRec {
            id: "tampered".into(),
            owner: "peer".into(),
            created_by: "peer".into(),
            feature_id: Some("tampered".into()),
            worktree_path: None,
            branch: None,
            base_commit: None,
            priority: default_priority(),
            status: "working".into(),
            next_step: None,
            wait: None,
            created_ms: now,
            updated_ms: now,
        },
    }]);
    let verified = handle_migration_verify(&server, "peer".into(), "token-peer".into());
    assert!(verified.ok);
    assert!(!verified.data["verified"].as_bool().unwrap());
    assert!(verified.data["issues"]
        .as_array()
        .unwrap()
        .iter()
        .any(|issue| issue.as_str().unwrap().contains("snapshot hash mismatch")));
    assert!(server.state.lock().unwrap().admission_frozen());
    std::fs::remove_dir_all(root).ok();
}

#[test]
fn migration_freeze_rejects_mutations_but_allows_rebind_and_reads() {
    let (server, root) = test_server();
    register(&server, "peer", "%peer");
    assert!(create_task(&server, "peer", "task", "feature").ok);
    assert!(handle_migration_plan(&server, "peer".into(), "token-peer".into()).ok);
    assert!(handle_migration_apply(&server, "peer".into(), "token-peer".into()).ok);
    let server = Arc::new(server);

    let mutations = vec![
        Req::Send {
            from: "peer".into(),
            worker_id: Some("peer".into()),
            token: Some("token-peer".into()),
            command: Some(send_command(&root, "peer-freeze")),
            to: "peer".into(),
            mtype: "notify".into(),
            subject: Some("release".into()),
            body: "RESOURCE_RELEASED feature".into(),
            in_reply_to: None,
            delivery: "immediate".into(),
        },
        Req::Poll {
            worker_id: "peer".into(),
            token: "token-peer".into(),
            timeout_ms: 1,
        },
        Req::Ack {
            worker_id: "peer".into(),
            token: "token-peer".into(),
            ids: vec!["message".into()],
        },
        Req::TaskUpdate {
            worker_id: "peer".into(),
            token: "token-peer".into(),
            task_id: "task".into(),
            status: Some("verifying".into()),
            next_step: None,
        },
        Req::MigrationPlan {
            worker_id: "peer".into(),
            token: "token-peer".into(),
        },
        Req::MigrationApply {
            worker_id: "peer".into(),
            token: "token-peer".into(),
        },
    ];
    for request in mutations {
        let response = dispatch(&server, request);
        assert_eq!(
            response.error.as_deref(),
            Some(
                "MIGRATION_ADMISSION_FROZEN: only identity rebind, read queries, daemon restart, and migration verify are allowed"
            )
        );
    }

    let read = dispatch(
        &server,
        Req::TaskStatus {
            task_id: Some("task".into()),
        },
    );
    assert!(read.ok);
    let rebound = dispatch(
        &server,
        Req::Register {
            worker_id: "peer".into(),
            token: "token-peer".into(),
            pane: Some("%peer".into()),
            cwd: "/tmp/rebound".into(),
        },
    );
    assert!(rebound.ok);
    let new_identity = dispatch(
        &server,
        Req::Register {
            worker_id: "new-peer".into(),
            token: "token-new-peer".into(),
            pane: Some("%new-peer".into()),
            cwd: "/tmp".into(),
        },
    );
    assert_eq!(
        new_identity.error.as_deref(),
        Some("MIGRATION_ADMISSION_FROZEN: only an existing tmux identity may rebind")
    );
    assert_eq!(server.state.lock().unwrap().workers.len(), 1);
    std::fs::remove_dir_all(root).ok();
}

#[test]
fn authenticated_send_uses_registered_cwd_for_authoritative_route_scope() {
    let (server, root) = test_server();
    let registered_cwd = root.join("registered");
    std::fs::create_dir_all(&registered_cwd).unwrap();
    assert!(
        handle_register(
            &server,
            "sender".into(),
            "token-sender".into(),
            Some("%sender".into()),
            registered_cwd.display().to_string(),
        )
        .ok
    );
    assert!(register(&server, "recipient", "%recipient").ok);

    let mut request = authenticated_send(&root, "sender", "recipient", "scope");
    if let Req::Send {
        command: Some(command),
        ..
    } = &mut request
    {
        command.scope = crate::scope::RouteScope::for_registered_project(
            crate::identity::AppServerId::new("appserver-cli").unwrap(),
            &root,
        )
        .unwrap();
    }
    let response = dispatch(&Arc::new(server), request);
    assert!(!response.ok);
    assert!(response
        .error
        .as_deref()
        .is_some_and(|error| error.starts_with("SEND_BINDING_REJECTED:")));
    std::fs::remove_dir_all(root).ok();
}

#[test]
fn authenticated_send_returns_typed_durability_failure_before_wake() {
    let (server, root) = test_server();
    assert!(register(&server, "sender", "%sender").ok);
    assert!(register(&server, "recipient", "%recipient").ok);
    *server.journal.lock().unwrap() =
        std::fs::File::open(root.join(".agent-collab/server/journal.jsonl")).unwrap();

    let response = dispatch(
        &Arc::new(server),
        authenticated_send(&root, "sender", "recipient", "durability"),
    );
    assert!(!response.ok);
    assert!(response
        .error
        .as_deref()
        .is_some_and(|error| error.starts_with("SEND_DURABILITY_FAILED:")));
    assert!(response.error.as_deref().unwrap().contains("journal"));
    std::fs::remove_dir_all(root).ok();
}

#[tokio::test]
async fn duplicate_daemon_rejection_preserves_authoritative_pid() {
    let root = PathBuf::from(format!(
        "/tmp/collab-sd-{}-{}",
        std::process::id(),
        now_ms()
    ));
    let scope = Scope { root: root.clone() };
    let first = tokio::spawn(run(Scope { root: root.clone() }));
    for _ in 0..100 {
        if scope.sock_path().exists() && scope.server_dir().join("server.pid").exists() {
            break;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    assert!(scope.sock_path().exists());
    let pid_path = scope.server_dir().join("server.pid");
    let authoritative_pid = std::fs::read_to_string(&pid_path).unwrap();

    let error = run(Scope { root: root.clone() })
        .await
        .err()
        .expect("second daemon must be rejected");
    assert!(error.to_string().contains("server already running"));
    assert_eq!(
        std::fs::read_to_string(&pid_path).unwrap(),
        authoritative_pid
    );

    first.abort();
    let _ = first.await;
    std::fs::remove_dir_all(root).ok();
}

#[test]
fn long_polls_do_not_starve_ping_on_the_blocking_pool() {
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .worker_threads(2)
        .max_blocking_threads(2)
        .enable_all()
        .build()
        .unwrap();
    runtime.block_on(async {
        use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
        use tokio::net::UnixStream;
        use tokio::time::{timeout, Duration};

        let (server, root) = test_server();
        register(&server, "peer", "%peer");
        let server = Arc::new(server);
        let mut poll_clients = Vec::new();
        let mut poll_tasks = Vec::new();

        for _ in 0..8 {
            let (mut client, server_stream) = UnixStream::pair().unwrap();
            poll_tasks.push(tokio::spawn(conn_task(server.clone(), server_stream)));
            let request = serde_json::to_string(&Req::Poll {
                worker_id: "peer".into(),
                token: "token-peer".into(),
                timeout_ms: 10_000,
            })
            .unwrap();
            client.write_all(request.as_bytes()).await.unwrap();
            client.write_all(b"\n").await.unwrap();
            poll_clients.push(client);
        }

        tokio::time::sleep(Duration::from_millis(50)).await;

        let (mut ping_client, server_stream) = UnixStream::pair().unwrap();
        let ping_task = tokio::spawn(conn_task(server.clone(), server_stream));
        let request = serde_json::to_string(&Req::Ping).unwrap();
        ping_client.write_all(request.as_bytes()).await.unwrap();
        ping_client.write_all(b"\n").await.unwrap();
        let mut response = String::new();
        timeout(
            Duration::from_secs(1),
            BufReader::new(&mut ping_client).read_line(&mut response),
        )
        .await
        .expect("Ping must not wait behind long Poll requests")
        .unwrap();
        let response: Resp = serde_json::from_str(response.trim()).unwrap();
        assert!(response.ok);

        ping_task.abort();
        for task in poll_tasks {
            task.abort();
        }
        drop(poll_clients);
        std::fs::remove_dir_all(root).ok();
    });
}

#[tokio::test]
async fn poll_wakes_when_a_message_is_committed() {
    let (server, root) = test_server();
    register(&server, "peer", "%peer");
    let server = Arc::new(server);
    let poll = tokio::spawn(handle_poll_async(server.clone(), "peer".into(), 5_000));

    tokio::time::sleep(Duration::from_millis(10)).await;
    server.commit(&[Event::Sent {
        msg: Message {
            id: "wake-message".into(),
            from: "sender".into(),
            to: "peer".into(),
            mtype: "notify".into(),
            subject: Some("wake".into()),
            body: "message".into(),
            in_reply_to: None,
            created_ms: now_ms(),
            state: "pending".into(),
            wake_attempt_count: 0,
            last_wake_attempt_ms: 0,
        },
    }]);

    let response = tokio::time::timeout(Duration::from_secs(1), poll)
        .await
        .expect("Poll must wake after a durable message commit")
        .unwrap();
    assert!(response.ok);
    assert_eq!(response.data["count"], 1);
    assert_eq!(response.data["messages"][0]["id"], "wake-message");
    assert_eq!(
        server.state.lock().unwrap().msgs["wake-message"].state,
        "read"
    );
    std::fs::remove_dir_all(root).ok();
}

#[tokio::test]
async fn recv_consumes_messages_without_a_follow_up_ack() {
    let (server, root) = test_server();
    register(&server, "peer", "%peer");
    server.commit(&[Event::Sent {
        msg: Message {
            id: "recv-message".into(),
            from: "sender".into(),
            to: "peer".into(),
            mtype: "notify".into(),
            subject: Some("recv".into()),
            body: "message".into(),
            in_reply_to: None,
            created_ms: now_ms(),
            state: "pending".into(),
            wake_attempt_count: 0,
            last_wake_attempt_ms: 0,
        },
    }]);
    let server = Arc::new(server);
    let response = handle_poll_async(server.clone(), "peer".into(), 100).await;
    assert!(response.ok);
    assert_eq!(response.data["count"], 1);
    assert_eq!(
        server.state.lock().unwrap().msgs["recv-message"].state,
        "read"
    );
    std::fs::remove_dir_all(root).ok();
}

#[test]
fn legacy_sent_event_replay_classifies_message_without_reply_reference() {
    let (_server, root) = test_server();
    let journal_path = root.join(".agent-collab/server/journal.jsonl");
    std::fs::write(
        &journal_path,
        r#"{"ev":"Sent","msg":{"id":"legacy-message","from":"peer-a","to":"peer-b","type":"request","subject":"legacy","body":"legacy body","created_ms":1,"state":"pending"}}
"#,
    )
    .unwrap();

    let state = replay(&root).expect("legacy Sent event must replay");
    let message = state
        .msgs
        .get("legacy-message")
        .expect("replay must retain the legacy message");
    assert_eq!(message.in_reply_to, None);
    assert_eq!(message.mtype, "request");
    assert_eq!(
        state
            .inbox_of("peer-b")
            .iter()
            .map(|message| message.id.as_str())
            .collect::<Vec<_>>(),
        vec!["legacy-message"]
    );
    assert!(!state.answered("legacy-message"));

    std::fs::remove_dir_all(root).ok();
}

#[test]
fn malformed_journal_replay_fails_fast() {
    let (_server, root) = test_server();
    std::fs::write(
        root.join(".agent-collab/server/journal.jsonl"),
        "{manual-edit\n",
    )
    .unwrap();
    let error = replay(&root).err().expect("malformed journal must fail");
    assert!(error
        .to_string()
        .contains("manual journal edits are unsupported"));
    std::fs::remove_dir_all(root).ok();
}

#[test]
fn concatenated_journal_events_replay_and_self_heal() {
    let (_server, root) = test_server();
    let journal_path = root.join(".agent-collab/server/journal.jsonl");
    let event1 = json!({"ev":"KeepaliveUpdated","worker_id":"w1","record":{"observed":"unknown","idle_since_ms":100,"activity_ms":50,"last_notice_ms":0,"last_notice_id":null,"unacked":0,"suspected_offline":false}}).to_string();
    let event2 = json!({"ev":"KeepaliveUpdated","worker_id":"w2","record":{"observed":"unknown","idle_since_ms":200,"activity_ms":150,"last_notice_ms":0,"last_notice_id":null,"unacked":0,"suspected_offline":false}}).to_string();
    std::fs::write(&journal_path, format!("{}{}\n", event1, event2)).unwrap();

    let state = replay(&root).expect("concatenated journal events must self-heal and replay");
    assert_eq!(state.keepalives.len(), 2);
    assert_eq!(state.keepalives["w1"].idle_since_ms, 100);
    assert_eq!(state.keepalives["w2"].idle_since_ms, 200);

    let content = std::fs::read_to_string(&journal_path).unwrap();
    let lines: Vec<_> = content.lines().filter(|l| !l.trim().is_empty()).collect();
    assert_eq!(lines.len(), 2);
    std::fs::remove_dir_all(root).ok();
}

#[test]
fn worktree_path_budget_accepts_short_slug_and_rejects_escape() {
    let root = std::env::temp_dir().join(format!(
        "collab-worktree-path-{}-{}",
        std::process::id(),
        now_ms()
    ));
    std::fs::create_dir_all(root.join("playground")).unwrap();
    assert!(validate_worktree_path(&root, "./playground/ar03-0828").is_ok());
    assert!(validate_worktree_path(
        &root,
        "./playground/v3-direct-sse-terminal-observability-20260827-long-run-id"
    )
    .is_err());
    assert!(validate_worktree_path(&root, "./playground/../outside").is_err());
    #[cfg(unix)]
    {
        std::os::unix::fs::symlink("/tmp", root.join("playground/link")).unwrap();
        assert!(validate_worktree_path(&root, "./playground/link/escape").is_err());
    }
    std::fs::remove_dir_all(root).unwrap();
}

#[test]
fn tmux_notification_contains_id_subject_and_original_body() {
    let text = notification_text(&Message {
        id: "message-id".into(),
        from: "sender".into(),
        to: "recipient".into(),
        mtype: "notify".into(),
        subject: Some("release".into()),
        body: "RESOURCE_RELEASED feature=shared".into(),
        in_reply_to: None,
        created_ms: now_ms(),
        state: "pending".into(),
        wake_attempt_count: 0,
        last_wake_attempt_ms: 0,
    })
    .unwrap();
    assert_eq!(
        text,
        "COLLAB_NOTIFY message-id [release] RESOURCE_RELEASED feature=shared | P1 ACTION: the resource is free; resume the task that waited on it. Details: collab msg message-id. | READ IS NOT DONE: never end your turn on an ACK, a read, or a summary. After handling, resume your current task; if you own none, run `appsdk longhorizon show` and take work."
    );
    assert!(!text.contains("ACK this notice"));
    assert!(text.contains("READ IS NOT DONE"));
}

#[test]
fn tmux_notification_classifies_priority_and_names_one_action() {
    let notify = |subject: &str| {
        notification_text(&Message {
            id: "m1".into(),
            from: "collab-server".into(),
            to: "master".into(),
            mtype: "notify".into(),
            subject: Some(subject.into()),
            body: "body".into(),
            in_reply_to: None,
            created_ms: now_ms(),
            state: "pending".into(),
            wake_attempt_count: 0,
            last_wake_attempt_ms: 0,
        })
        .unwrap()
    };

    assert!(notify("worker-idle: w1").contains("P1 ACTION: dispatch work to this idle capacity"));
    assert!(notify("master-idle: master").contains("P1 ACTION: run the scheduling pass"));
    assert!(notify("worker-unresponsive: w1").contains("P1 ACTION: snapshot the pane"));
    assert!(notify("task-keepalive 1/3").contains("P1 ACTION: continue your own task"));
    assert!(notify("goal:plan.md").contains("P0 ACTION: run the long-horizon briefing"));
    assert!(notify("Settings delivery recorded").contains("P2 ACTION: note it"));

    // Every class carries the resume protocol, not just the operational ones.
    for subject in [
        "worker-idle: w1",
        "goal:plan.md",
        "Settings delivery recorded",
    ] {
        assert!(notify(subject).contains("resume your current task"));
    }
}

#[test]
fn tmux_notification_truncates_body_without_dropping_the_action_contract() {
    let text = notification_text(&Message {
        id: "message-id".into(),
        from: "sender".into(),
        to: "recipient".into(),
        mtype: "notify".into(),
        subject: Some("release".into()),
        body: "x".repeat(4000),
        in_reply_to: None,
        created_ms: now_ms(),
        state: "pending".into(),
        wake_attempt_count: 0,
        last_wake_attempt_ms: 0,
    })
    .unwrap();

    assert!(text.chars().count() <= 1024);
    assert!(text.contains("P1 ACTION:"));
    assert!(text.contains("READ IS NOT DONE"));
    assert!(text.ends_with("run `appsdk longhorizon show` and take work."));
}

#[test]
fn tmux_notification_abbreviates_subject_and_escapes_body_controls() {
    let text = notification_text(&Message {
        id: "message-id".into(),
        from: "sender".into(),
        to: "recipient".into(),
        mtype: "notify".into(),
        subject: Some(
            "this subject is deliberately longer than forty eight visible characters".into(),
        ),
        body: "line one\nline two\t中文".into(),
        in_reply_to: None,
        created_ms: now_ms(),
        state: "pending".into(),
        wake_attempt_count: 0,
        last_wake_attempt_ms: 0,
    })
    .unwrap();
    assert_eq!(
        text,
        "COLLAB_NOTIFY message-id [this subject is deliberately longer than forty …] line one\\nline two\\t中文 | P1 ACTION: do the in-scope action the message asks for. Details: collab msg message-id. | READ IS NOT DONE: never end your turn on an ACK, a read, or a summary. After handling, resume your current task; if you own none, run `appsdk longhorizon show` and take work."
    );
}

#[test]
fn sendmessage_requires_subject_before_state_mutation() {
    let (server, root) = test_server();
    let response = handle_send(
        &server,
        "sender".into(),
        "recipient".into(),
        "notify".into(),
        None,
        "The candidate is ready.".into(),
        None,
        "immediate".into(),
    );
    assert!(!response.ok);
    assert_eq!(
        response.error.as_deref(),
        Some("MESSAGE_SUBJECT_REQUIRED: sendmessage requires --subject")
    );
    assert!(server.state.lock().unwrap().msgs.is_empty());
    std::fs::remove_dir_all(root).ok();
}

#[test]
fn send_without_subscription_is_mailbox_only_and_deduplicated() {
    let (server, root) = test_server();
    register(&server, "sender", "%collab-missing-sender");
    register(&server, "recipient", "%collab-missing-recipient");
    let subscription_id = server
        .state
        .lock()
        .unwrap()
        .notification_subscriptions
        .values()
        .find(|subscription| subscription.worker_id == "recipient")
        .unwrap()
        .id
        .clone();
    server.commit(&[Event::NotificationStatus {
        subscription_id,
        status: "cancelled".into(),
        updated_ms: now_ms(),
    }]);
    let first = handle_send(
        &server,
        "sender".into(),
        "recipient".into(),
        "notify".into(),
        Some("occupied".into()),
        "RESOURCE_OCCUPIED feature=shared".into(),
        None,
        "immediate".into(),
    );
    assert!(first.ok);
    let message_id = first.data["msg_id"].as_str().unwrap().to_owned();
    assert_eq!(server.state.lock().unwrap().msgs.len(), 1);
    assert!(root
        .join(".agent-collab/mailbox")
        .join(format!("{message_id}.json"))
        .exists());
    let jsonl = root.join(".agent-collab/mailbox/recipient-recipient.jsonl");
    assert!(std::fs::read_to_string(&jsonl)
        .unwrap()
        .lines()
        .any(|line| {
            serde_json::from_str::<serde_json::Value>(line)
                .map(|record| {
                    record["schema_version"] == 1
                        && record["record_type"] == "message"
                        && record["recipient"] == "recipient"
                        && record["category"] == "direct"
                        && record["task_ids"].is_array()
                        && record["created_ms"].is_i64()
                        && record["window_start_ms"].is_null()
                        && record["window_end_ms"].is_null()
                        && record["state"] == "pending"
                        && record["exact_error"].as_str().is_some_and(|error| {
                            error.starts_with("MAILBOX_SCOPE_BINDING_UNAVAILABLE:")
                        })
                        && record["message"]["id"] == message_id
                })
                .unwrap_or(false)
        }));
    assert_eq!(
        replay(&root).unwrap().msgs[&message_id].body,
        "RESOURCE_OCCUPIED feature=shared"
    );
    assert_eq!(first.data["notification"], "mailbox-only-no-subscription");
    assert_eq!(
        server.state.lock().unwrap().msgs[&message_id].wake_attempt_count,
        0
    );
    assert!(!server.log_path().exists());

    let duplicate = handle_send(
        &server,
        "sender".into(),
        "recipient".into(),
        "notify".into(),
        Some("occupied".into()),
        "RESOURCE_OCCUPIED feature=shared".into(),
        None,
        "immediate".into(),
    );
    assert!(duplicate.ok);
    assert_eq!(duplicate.data["msg_id"], message_id);
    assert_eq!(duplicate.data["deduplicated"], true);
    assert_eq!(server.state.lock().unwrap().msgs.len(), 1);
    std::fs::remove_dir_all(root).ok();
}

#[test]
fn explicit_peer_notification_accepts_arbitrary_durable_body() {
    let (server, root) = test_server();
    register(&server, "sender", "%sender");
    register(&server, "recipient", "%recipient");
    let response = handle_send(
        &server,
        "sender".into(),
        "recipient".into(),
        "notify".into(),
        Some("review".into()),
        "The candidate is ready for your review.".into(),
        None,
        "immediate".into(),
    );
    assert!(response.ok);
    let message_id = response.data["msg_id"].as_str().unwrap();
    let state = server.state.lock().unwrap();
    assert_eq!(
        state.msgs[message_id].body,
        "The candidate is ready for your review."
    );
    assert_eq!(state.msgs[message_id].subject.as_deref(), Some("review"));
    assert_eq!(state.msgs[message_id].wake_attempt_count, 1);
    assert_eq!(response.data["notification"], "subscribed-not-sent");
    drop(state);
    std::fs::remove_dir_all(root).ok();
}

#[test]
fn recipient_jsonl_records_latest_delivery_and_journal_replay() {
    let (server, root) = test_server();
    register(&server, "recipient", "%recipient");
    let id = "jsonl-message";
    server.commit(&[
        Event::Sent {
            msg: Message {
                id: id.into(),
                from: "sender".into(),
                to: "recipient".into(),
                mtype: "notify".into(),
                subject: Some("progress".into()),
                body: "task-jsonl progress".into(),
                in_reply_to: None,
                created_ms: now_ms(),
                state: "pending".into(),
                wake_attempt_count: 0,
                last_wake_attempt_ms: 0,
            },
        },
        Event::Delivered {
            ids: vec![id.into()],
        },
        Event::Acked {
            ids: vec![id.into()],
        },
    ]);
    let path = root.join(".agent-collab/mailbox/recipient-recipient.jsonl");
    let records = std::fs::read_to_string(path)
        .unwrap()
        .lines()
        .map(|line| serde_json::from_str::<serde_json::Value>(line).unwrap())
        .filter(|record| record["message"]["id"] == id)
        .collect::<Vec<_>>();
    assert_eq!(records.len(), 3, "Sent/Delivered/Acked append snapshots");
    for record in &records {
        assert_eq!(record["schema_version"], 1);
        assert_eq!(record["record_type"], "message");
        assert_eq!(record["recipient"], "recipient");
        assert_eq!(record["category"], "progress");
        assert!(record["task_ids"].is_array());
        assert!(record["created_ms"].is_i64());
        assert!(record["window_start_ms"].is_null());
        assert!(record["window_end_ms"].is_null());
        assert!(record["exact_error"]
            .as_str()
            .is_some_and(|error| { error.starts_with("MAILBOX_SCOPE_BINDING_UNAVAILABLE:") }));
    }
    assert_eq!(records.last().unwrap()["message"]["state"], "read");
    assert_eq!(replay(&root).unwrap().msgs[id].state, "read");
    let path = root.join(".agent-collab/mailbox/recipient-recipient.jsonl");
    std::fs::OpenOptions::new()
        .append(true)
        .open(&path)
        .unwrap();
    std::fs::OpenOptions::new()
        .append(true)
        .open(&path)
        .unwrap();
    std::fs::write(
        &path,
        format!("{}{{\"partial\":", std::fs::read_to_string(&path).unwrap()),
    )
    .unwrap();
    let projection = read_recipient_mailbox(&path, "recipient").unwrap();
    assert_eq!(projection.records.len(), 3);
    assert!(projection.partial_tail);
    assert!(projection.unterminated_tail);
    std::fs::write(&path, "{\"bad\":true}\nnot-json\n").unwrap();
    assert!(read_recipient_mailbox(&path, "recipient").is_err());
    std::fs::remove_dir_all(root).ok();
}

#[test]
fn recipient_jsonl_failure_is_logged_without_panicking() {
    let (server, root) = test_server();
    register(&server, "recipient", "%recipient");
    std::fs::create_dir_all(root.join(".agent-collab/mailbox/recipient-recipient.jsonl")).unwrap();
    server.commit(&[Event::Sent {
        msg: Message {
            id: "jsonl-failure".into(),
            from: "sender".into(),
            to: "recipient".into(),
            mtype: "notify".into(),
            subject: Some("failure".into()),
            body: "preserve journal truth".into(),
            in_reply_to: None,
            created_ms: now_ms(),
            state: "pending".into(),
            wake_attempt_count: 0,
            last_wake_attempt_ms: 0,
        },
    }]);
    assert!(
        std::fs::read_to_string(root.join(".agent-collab/server/log.txt"))
            .unwrap()
            .contains("MAILBOX_JSONL_WRITE_FAILED")
    );
    assert_eq!(
        replay(&root).unwrap().msgs["jsonl-failure"].state,
        "pending"
    );
    std::fs::remove_dir_all(root).ok();
}

#[test]
fn recipient_jsonl_accepts_legacy_bare_message_before_new_append() {
    let (server, root) = test_server();
    register(&server, "recipient", "%recipient");
    let path = root.join(".agent-collab/mailbox/recipient-recipient.jsonl");
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    let legacy = Message {
        id: "legacy-message".into(),
        from: "sender".into(),
        to: "recipient".into(),
        mtype: "notify".into(),
        subject: Some("progress".into()),
        body: "legacy record".into(),
        in_reply_to: None,
        created_ms: now_ms(),
        state: "pending".into(),
        wake_attempt_count: 0,
        last_wake_attempt_ms: 0,
    };
    std::fs::write(
        &path,
        format!("{}\n", serde_json::to_string(&legacy).unwrap()),
    )
    .unwrap();
    server.commit(&[Event::Sent {
        msg: Message {
            id: "new-message".into(),
            from: "sender".into(),
            to: "recipient".into(),
            mtype: "notify".into(),
            subject: Some("progress".into()),
            body: "new record".into(),
            in_reply_to: None,
            created_ms: now_ms(),
            state: "pending".into(),
            wake_attempt_count: 0,
            last_wake_attempt_ms: 0,
        },
    }]);
    let projection = read_recipient_mailbox(&path, "recipient").unwrap();
    assert_eq!(projection.records.len(), 2);
    assert_eq!(projection.records[0]["schema_version"], 1);
    assert_eq!(projection.records[0]["window_source"], "legacy-message");
    assert_eq!(projection.records[1]["message"]["id"], "new-message");
    assert_eq!(replay(&root).unwrap().msgs["new-message"].state, "pending");
    std::fs::remove_dir_all(root).ok();
}

#[test]
fn recipient_jsonl_separates_complete_unterminated_legacy_record() {
    let (server, root) = test_server();
    register(&server, "recipient", "%recipient");
    let path = root.join(".agent-collab/mailbox/recipient-recipient.jsonl");
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    let legacy = Message {
        id: "legacy-without-newline".into(),
        from: "sender".into(),
        to: "recipient".into(),
        mtype: "notify".into(),
        subject: Some("progress".into()),
        body: "complete record without separator".into(),
        in_reply_to: None,
        created_ms: now_ms(),
        state: "pending".into(),
        wake_attempt_count: 0,
        last_wake_attempt_ms: 0,
    };
    std::fs::write(&path, serde_json::to_string(&legacy).unwrap()).unwrap();
    server.commit(&[Event::Sent {
        msg: Message {
            id: "after-legacy-without-newline".into(),
            from: "sender".into(),
            to: "recipient".into(),
            mtype: "notify".into(),
            subject: Some("progress".into()),
            body: "new record".into(),
            in_reply_to: None,
            created_ms: now_ms(),
            state: "pending".into(),
            wake_attempt_count: 0,
            last_wake_attempt_ms: 0,
        },
    }]);
    let projection = read_recipient_mailbox(&path, "recipient").unwrap();
    assert_eq!(projection.records.len(), 2);
    assert!(!projection.partial_tail);
    assert!(!projection.unterminated_tail);
    assert_eq!(
        projection.records[0]["message"]["id"],
        "legacy-without-newline"
    );
    assert_eq!(
        projection.records[1]["message"]["id"],
        "after-legacy-without-newline"
    );
    std::fs::remove_dir_all(root).ok();
}

#[test]
fn partial_recipient_tail_is_repaired_before_append_and_replay_preserves_assignment() {
    let (server, root) = test_server();
    register(&server, "recipient", "%recipient");
    server.commit(&[Event::TaskCreated {
        task: TaskRec {
            id: "partial-assigned-task".into(),
            owner: "recipient".into(),
            created_by: "sender".into(),
            feature_id: Some("partial-mailbox".into()),
            worktree_path: None,
            branch: None,
            base_commit: Some("partial-base".into()),
            priority: "p0".into(),
            status: "working".into(),
            next_step: Some("replay after restart".into()),
            wait: None,
            created_ms: now_ms(),
            updated_ms: now_ms(),
        },
    }]);
    let path = root.join(".agent-collab/mailbox/recipient-recipient.jsonl");
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    server.commit(&[Event::Sent {
        msg: Message {
            id: "before-partial".into(),
            from: "sender".into(),
            to: "recipient".into(),
            mtype: "notify".into(),
            subject: Some("progress".into()),
            body: "before partial tail".into(),
            in_reply_to: None,
            created_ms: now_ms(),
            state: "pending".into(),
            wake_attempt_count: 0,
            last_wake_attempt_ms: 0,
        },
    }]);
    let mut content = std::fs::read_to_string(&path).unwrap();
    content.push_str("{\"partial\":");
    std::fs::write(&path, content).unwrap();
    server.commit(&[Event::Sent {
        msg: Message {
            id: "after-partial".into(),
            from: "sender".into(),
            to: "recipient".into(),
            mtype: "notify".into(),
            subject: Some("progress".into()),
            body: "after partial tail".into(),
            in_reply_to: None,
            created_ms: now_ms(),
            state: "pending".into(),
            wake_attempt_count: 0,
            last_wake_attempt_ms: 0,
        },
    }]);
    let projection = read_recipient_mailbox(&path, "recipient").unwrap();
    assert_eq!(projection.records.len(), 2);
    assert!(!projection.partial_tail);
    assert!(!projection.unterminated_tail);
    assert_eq!(projection.records[0]["message"]["id"], "before-partial");
    assert_eq!(projection.records[1]["message"]["id"], "after-partial");
    drop(server);
    let replayed = replay(&root).unwrap();
    assert_eq!(replayed.msgs["before-partial"].state, "pending");
    assert_eq!(replayed.msgs["after-partial"].state, "pending");
    assert_eq!(replayed.tasks["partial-assigned-task"].owner, "recipient");
    assert_eq!(
        replayed.tasks["partial-assigned-task"]
            .feature_id
            .as_deref(),
        Some("partial-mailbox")
    );
    assert_eq!(
        replayed.tasks["partial-assigned-task"].next_step.as_deref(),
        Some("replay after restart")
    );
    std::fs::remove_dir_all(root).ok();
}

#[test]
fn malformed_recipient_jsonl_does_not_block_future_append_or_journal_replay() {
    let (server, root) = test_server();
    register(&server, "recipient", "%recipient");
    server.commit(&[Event::TaskCreated {
        task: TaskRec {
            id: "assigned-task".into(),
            owner: "recipient".into(),
            created_by: "sender".into(),
            feature_id: Some("mailbox-envelope".into()),
            worktree_path: None,
            branch: None,
            base_commit: Some("base-commit".into()),
            priority: "p0".into(),
            status: "working".into(),
            next_step: Some("consume mailbox".into()),
            wait: None,
            created_ms: now_ms(),
            updated_ms: now_ms(),
        },
    }]);
    let path = root.join(".agent-collab/mailbox/recipient-recipient.jsonl");
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    std::fs::write(&path, "{\"bad\":true}\n").unwrap();
    server.commit(&[Event::Sent {
        msg: Message {
            id: "after-malformed".into(),
            from: "sender".into(),
            to: "recipient".into(),
            mtype: "notify".into(),
            subject: Some("progress".into()),
            body: "journal remains authoritative".into(),
            in_reply_to: None,
            created_ms: now_ms(),
            state: "pending".into(),
            wake_attempt_count: 0,
            last_wake_attempt_ms: 0,
        },
    }]);
    let content = std::fs::read_to_string(&path).unwrap();
    assert!(content.lines().any(|line| line.contains("after-malformed")));
    assert_eq!(content.matches("{\"bad\":true}").count(), 1);
    let projection = read_recipient_mailbox(&path, "recipient").unwrap();
    assert_eq!(projection.records.len(), 1);
    assert_eq!(projection.records[0]["message"]["id"], "after-malformed");
    assert!(
        std::fs::read_to_string(root.join(".agent-collab/server/log.txt"))
            .unwrap()
            .contains("MAILBOX_JSONL_RECOVERABLE")
    );
    drop(server);
    let replayed = replay(&root).unwrap();
    assert_eq!(replayed.msgs["after-malformed"].state, "pending");
    let assignment = &replayed.tasks["assigned-task"];
    assert_eq!(assignment.owner, "recipient");
    assert_eq!(assignment.feature_id.as_deref(), Some("mailbox-envelope"));
    assert_eq!(assignment.base_commit.as_deref(), Some("base-commit"));
    assert_eq!(assignment.status, "working");
    assert_eq!(assignment.next_step.as_deref(), Some("consume mailbox"));
    std::fs::remove_dir_all(root).ok();
}

#[test]
fn interior_malformed_recipient_jsonl_preserves_bad_line_and_appends_later_message() {
    let (server, root) = test_server();
    register(&server, "recipient", "%recipient");
    let path = root.join(".agent-collab/mailbox/recipient-recipient.jsonl");
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    server.commit(&[Event::TaskCreated {
        task: TaskRec {
            id: "interior-assigned-task".into(),
            owner: "recipient".into(),
            created_by: "sender".into(),
            feature_id: Some("interior-mailbox-recovery".into()),
            worktree_path: Some("./playground/interior-mailbox-recovery".into()),
            branch: Some("codex/interior-mailbox-recovery".into()),
            base_commit: Some("interior-base".into()),
            priority: "p1".into(),
            status: "working".into(),
            next_step: Some("consume preserved mailbox".into()),
            wait: None,
            created_ms: now_ms(),
            updated_ms: now_ms(),
        },
    }]);

    server.commit(&[Event::Sent {
        msg: Message {
            id: "before-interior-malformed".into(),
            from: "sender".into(),
            to: "recipient".into(),
            mtype: "notify".into(),
            subject: Some("progress".into()),
            body: "before malformed interior record".into(),
            in_reply_to: None,
            created_ms: now_ms(),
            state: "pending".into(),
            wake_attempt_count: 0,
            last_wake_attempt_ms: 0,
        },
    }]);
    let malformed_line = "not-json-interior-record-preserve-this-line";
    server.commit(&[Event::Sent {
        msg: Message {
            id: "later-valid-record".into(),
            from: "sender".into(),
            to: "recipient".into(),
            mtype: "notify".into(),
            subject: Some("progress".into()),
            body: "later valid record".into(),
            in_reply_to: None,
            created_ms: now_ms(),
            state: "pending".into(),
            wake_attempt_count: 0,
            last_wake_attempt_ms: 0,
        },
    }]);

    let content = std::fs::read_to_string(&path).unwrap();
    let (first, remaining) = content.split_once('\n').unwrap();
    std::fs::write(&path, format!("{first}\n{malformed_line}\n{remaining}")).unwrap();

    server.commit(&[Event::Sent {
        msg: Message {
            id: "after-interior-malformed".into(),
            from: "sender".into(),
            to: "recipient".into(),
            mtype: "notify".into(),
            subject: Some("progress".into()),
            body: "new append after malformed interior record".into(),
            in_reply_to: None,
            created_ms: now_ms(),
            state: "pending".into(),
            wake_attempt_count: 0,
            last_wake_attempt_ms: 0,
        },
    }]);

    let content = std::fs::read_to_string(&path).unwrap();
    assert!(content.lines().any(|line| line == malformed_line));
    let projection = read_recipient_mailbox(&path, "recipient").unwrap();
    assert_eq!(
        projection
            .records
            .iter()
            .filter_map(|record| record["message"]["id"].as_str())
            .collect::<Vec<_>>(),
        vec![
            "before-interior-malformed",
            "later-valid-record",
            "after-interior-malformed"
        ]
    );
    assert_eq!(projection.recoverable_errors.len(), 1);
    assert!(projection.recoverable_errors[0].contains("record 2"));

    let server = Arc::new(server);
    let response = dispatch(
        &server,
        Req::MailboxRead {
            all: false,
            sort: Some("time-asc".into()),
            worker_id: Some("recipient".into()),
        },
    );
    assert!(response.ok);
    assert_eq!(
        response.data["recipient_jsonl"]["status"],
        "recoverable-error"
    );
    assert!(response.data["recipient_jsonl"]["exact_error"]
        .as_str()
        .unwrap()
        .contains("record 2"));
    assert_eq!(
        response.data["recipient_jsonl"]["records"]
            .as_array()
            .unwrap()
            .len(),
        3
    );

    drop(server);
    let replayed = replay(&root).unwrap();
    assert_eq!(
        replayed.msgs["later-valid-record"].body,
        "later valid record"
    );
    assert_eq!(
        replayed.msgs["after-interior-malformed"].body,
        "new append after malformed interior record"
    );
    let assignment = &replayed.tasks["interior-assigned-task"];
    assert_eq!(assignment.owner, "recipient");
    assert_eq!(
        assignment.feature_id.as_deref(),
        Some("interior-mailbox-recovery")
    );
    assert_eq!(
        assignment.worktree_path.as_deref(),
        Some("./playground/interior-mailbox-recovery")
    );
    assert_eq!(
        assignment.branch.as_deref(),
        Some("codex/interior-mailbox-recovery")
    );
    assert_eq!(assignment.base_commit.as_deref(), Some("interior-base"));
    assert_eq!(assignment.status, "working");
    assert_eq!(
        assignment.next_step.as_deref(),
        Some("consume preserved mailbox")
    );
    std::fs::remove_dir_all(root).ok();
}

#[test]
fn removed_role_and_dispatch_commands_fail_fast() {
    let (server, root) = test_server();
    register(&server, "peer", "%peer");
    assert!(
        handle_task_dispatch(&server, "peer".into(), "token-peer".into())
            .error
            .unwrap()
            .contains("deprecated")
    );
    assert!(handle_task_claim(
        &server,
        "peer".into(),
        "token-peer".into(),
        "legacy-task".into(),
    )
    .error
    .unwrap()
    .contains("deprecated"));
    let server = Arc::new(server);
    for request in [
        Req::Role {
            worker_id: "peer".into(),
        },
        Req::TransferMaster {
            worker_id: "peer".into(),
            token: "token-peer".into(),
            target_id: "peer".into(),
        },
    ] {
        assert!(!dispatch(&server, request).ok);
    }
    std::fs::remove_dir_all(root).ok();
}

#[test]
fn lifecycle_cannot_bypass_review_or_delivery() {
    let (server, root) = test_server();
    register(&server, "peer", "%peer");
    assert!(create_task(&server, "peer", "task", "feature").ok);
    let early_delivery = handle_task_deliver(
        &server,
        "peer".into(),
        "token-peer".into(),
        "task".into(),
        Some("not reviewed".into()),
        Some("/tmp/worktree".into()),
    );
    assert!(!early_delivery.ok);
    assert!(
        handle_task_update(
            &server,
            "peer".into(),
            "token-peer".into(),
            "task".into(),
            Some("verifying".into()),
            None,
        )
        .ok
    );
    assert!(
        handle_task_update(
            &server,
            "peer".into(),
            "token-peer".into(),
            "task".into(),
            Some("reviewed".into()),
            None,
        )
        .ok
    );
    let skipped_delivery = handle_task_update(
        &server,
        "peer".into(),
        "token-peer".into(),
        "task".into(),
        Some("merged".into()),
        None,
    );
    assert_eq!(
        skipped_delivery.error.as_deref(),
        Some("use collab task review/integrated for integration-owned lifecycle transitions")
    );
    assert_eq!(
        server.state.lock().unwrap().tasks["task"].status,
        "reviewed"
    );
    std::fs::remove_dir_all(root).ok();
}

#[test]
fn legacy_accepted_candidate_can_record_merge() {
    let (server, root) = test_server();
    register(&server, "peer", "%peer");
    assert!(create_task(&server, "peer", "accepted-task", "feature").ok);
    {
        let mut state = server.state.lock().unwrap();
        let mut task = state.tasks.remove("accepted-task").unwrap();
        task.status = "accepted".into();
        state.tasks.insert(task.id.clone(), task);
    }
    let merged = handle_task_update(
        &server,
        "peer".into(),
        "token-peer".into(),
        "accepted-task".into(),
        Some("merged".into()),
        Some("integration recorded".into()),
    );
    assert!(
        merged.ok,
        "legacy accepted task must be mergeable: {merged:?}"
    );
    assert_eq!(
        server.state.lock().unwrap().tasks["accepted-task"].status,
        "merged"
    );
    assert_eq!(
        replay(&root).unwrap().tasks["accepted-task"].status,
        "merged"
    );
    std::fs::remove_dir_all(root).ok();
}

#[test]
fn delivery_review_and_exact_main_integration_are_durable() {
    let (server, root) = test_server();
    register(&server, "owner", "%owner");
    register(&server, "outsider", "%outsider");
    assert!(create_task(&server, "owner", "task", "feature").ok);
    assert!(
        handle_task_update(
            &server,
            "owner".into(),
            "token-owner".into(),
            "task".into(),
            Some("verifying".into()),
            None,
        )
        .ok
    );
    assert!(
        handle_task_update(
            &server,
            "owner".into(),
            "token-owner".into(),
            "task".into(),
            Some("reviewed".into()),
            None,
        )
        .ok
    );
    assert!(
        handle_task_deliver(
            &server,
            "owner".into(),
            "token-owner".into(),
            "task".into(),
            Some("candidate commit and gates passed".into()),
            Some("candidate".into()),
        )
        .ok
    );
    let denied = handle_task_review(
        &server,
        "outsider".into(),
        "token-outsider".into(),
        "task".into(),
        true,
        false,
        "outsider review".into(),
    );
    assert!(!denied.ok);
    assert!(
        handle_task_review(
            &server,
            "owner".into(),
            "token-owner".into(),
            "task".into(),
            true,
            false,
            "review gates passed".into(),
        )
        .ok
    );

    for args in [
        ["init", "-q"].as_slice(),
        ["config", "user.email", "test@example.com"].as_slice(),
        ["config", "user.name", "Collab Test"].as_slice(),
        ["commit", "--allow-empty", "-q", "-m", "main"].as_slice(),
        ["branch", "-M", "main"].as_slice(),
    ] {
        assert!(Command::new("git")
            .current_dir(&root)
            .args(args)
            .status()
            .unwrap()
            .success());
    }
    let head = String::from_utf8(
        Command::new("git")
            .current_dir(&root)
            .args(["rev-parse", "HEAD"])
            .output()
            .unwrap()
            .stdout,
    )
    .unwrap()
    .trim()
    .to_owned();
    assert!(
        handle_task_integrated(
            &server,
            "owner".into(),
            "token-owner".into(),
            "task".into(),
            head,
            "main integration verified".into(),
        )
        .ok
    );
    let state = server.state.lock().unwrap();
    assert_eq!(state.tasks["task"].status, "merged");
    let lifecycle = &state.task_lifecycle["task"];
    assert_eq!(
        lifecycle.delivery_evidence.as_deref(),
        Some("candidate commit and gates passed")
    );
    assert_eq!(lifecycle.reviewer.as_deref(), Some("owner"));
    assert_eq!(
        lifecycle.integration_evidence.as_deref(),
        Some("main integration verified")
    );
    drop(state);
    let replayed = replay(&root).unwrap();
    assert_eq!(replayed.tasks["task"].status, "merged");
    assert_eq!(
        replayed.task_lifecycle["task"]
            .integration_evidence
            .as_deref(),
        Some("main integration verified")
    );
    std::fs::remove_dir_all(root).ok();
}

#[test]
fn accepted_task_can_return_to_rework_and_redeliver() {
    let (server, root) = test_server();
    register(&server, "owner", "%owner");
    assert!(create_task(&server, "owner", "task", "feature").ok);
    for status in ["verifying", "reviewed"] {
        assert!(
            handle_task_update(
                &server,
                "owner".into(),
                "token-owner".into(),
                "task".into(),
                Some(status.into()),
                None,
            )
            .ok
        );
    }
    assert!(
        handle_task_deliver(
            &server,
            "owner".into(),
            "token-owner".into(),
            "task".into(),
            Some("first candidate".into()),
            Some("candidate".into()),
        )
        .ok
    );
    assert!(
        handle_task_review(
            &server,
            "owner".into(),
            "token-owner".into(),
            "task".into(),
            true,
            false,
            "accepted".into(),
        )
        .ok
    );
    let direct_verifying = handle_task_update(
        &server,
        "owner".into(),
        "token-owner".into(),
        "task".into(),
        Some("verifying".into()),
        None,
    );
    assert_eq!(
        direct_verifying.error.as_deref(),
        Some("invalid task transition accepted -> verifying")
    );
    let direct_merge = handle_task_update(
        &server,
        "owner".into(),
        "token-owner".into(),
        "task".into(),
        Some("merged".into()),
        None,
    );
    assert_eq!(
        direct_merge.error.as_deref(),
        Some("use collab task review/integrated for integration-owned lifecycle transitions")
    );
    assert!(
        handle_task_update(
            &server,
            "owner".into(),
            "token-owner".into(),
            "task".into(),
            Some("rework".into()),
            Some("address review findings".into()),
        )
        .ok
    );
    for status in ["working", "verifying", "reviewed"] {
        assert!(
            handle_task_update(
                &server,
                "owner".into(),
                "token-owner".into(),
                "task".into(),
                Some(status.into()),
                None,
            )
            .ok
        );
    }
    let redelivery = handle_task_deliver(
        &server,
        "owner".into(),
        "token-owner".into(),
        "task".into(),
        Some("corrected candidate".into()),
        Some("candidate".into()),
    );
    assert!(redelivery.ok, "{}", redelivery.error.unwrap_or_default());
    assert_eq!(
        server.state.lock().unwrap().task_lifecycle["task"]
            .delivery_evidence
            .as_deref(),
        Some("corrected candidate")
    );
    std::fs::remove_dir_all(root).ok();
}

#[test]
fn context_is_read_only_and_does_not_consume_notifications() {
    let (server, root) = test_server();
    register(&server, "peer", "%peer");
    assert!(create_task(&server, "peer", "task", "feature").ok);
    let message_id = "notification".to_string();
    server.commit(&[Event::Sent {
        msg: Message {
            id: message_id.clone(),
            from: "peer-two".into(),
            to: "peer".into(),
            mtype: "notify".into(),
            subject: Some("released:task".into()),
            body: "RESOURCE_RELEASED task=task".into(),
            in_reply_to: None,
            created_ms: now_ms(),
            state: "pending".into(),
            wake_attempt_count: 0,
            last_wake_attempt_ms: 0,
        },
    }]);

    let context = handle_context(&server, "peer".into(), "token-peer".into());
    assert!(context.ok);
    assert!(context.data["master"].is_null());
    assert_eq!(context.data["authority"]["must_obey_master"], false);
    assert_eq!(context.data["authority"]["may_decline_master_invite"], true);
    assert_eq!(context.data["inbox"]["unread"], 1);
    let state = server.state.lock().unwrap();
    assert_eq!(state.msgs[&message_id].state, "pending");
    drop(state);
    std::fs::remove_dir_all(root).ok();
}

#[test]
fn architecture_source_has_no_live_declared_role_or_dispatch_owner() {
    let server_source = include_str!("mod.rs");
    let state_source = include_str!("state.rs");
    let mcp_source = include_str!("../bin/collab-mcp.rs");
    for removed in [
        "#[cfg(any())]",
        "fn default_role",
        "fn handle_transfer_master",
        "fn idle_worker_ids",
        "TASK_OFFER",
        "TASK_DELIVERED",
        "master_notified",
    ] {
        assert!(
            !server_source.contains(removed),
            "removed runtime semantic remains: {removed}"
        );
    }
    assert!(!state_source.contains("pub role:"));
    assert!(!state_source.contains("pub goal_prompt:"));
    assert!(!state_source.contains("pub goal_busy:"));
    assert!(!state_source.contains("pub nudge_count:"));
    assert!(!state_source.contains("pub last_nudge_ms:"));
    for removed_tool in [
        "\"collab_role\"",
        "\"collab_root\"",
        "\"collab_task_claim\"",
        "\"collab_task_dispatch\"",
        "\"project_root\"",
        "args.get(\"pane\")",
    ] {
        assert!(
            !mcp_source.contains(removed_tool),
            "removed MCP tool remains: {removed_tool}"
        );
    }
}

#[test]
fn active_lifecycle_manifest_binds_every_registered_call_edge_to_source() {
    let manifest: serde_json::Value =
        serde_json::from_str(include_str!("../../docs/collab-v1-lifecycle.manifest.json")).unwrap();
    let call_map: serde_json::Value =
        serde_json::from_str(include_str!("../../docs/mainline-call-map.json")).unwrap();
    assert_eq!(manifest["status"], "active");
    assert_eq!(call_map["status"], "active");
    assert_eq!(manifest["lifecycle_id"], call_map["lifecycle_id"]);

    let project_root = Path::new(env!("CARGO_MANIFEST_DIR"));
    for edge in call_map["edges"].as_array().unwrap() {
        let path = edge["path"].as_str().unwrap();
        let source = std::fs::read_to_string(project_root.join(path)).unwrap();
        for field in ["caller", "callee"] {
            let symbol = edge[field].as_str().unwrap();
            assert!(
                source.contains(symbol),
                "{field} {symbol} is not bound in {path}"
            );
        }
    }
    for path in manifest["canonical_docs"].as_array().unwrap() {
        assert!(project_root.join(path.as_str().unwrap()).is_file());
    }
}

#[test]
fn cleanup_rejects_unmerged_then_removes_only_merged_clean_worktree() {
    let root = std::env::temp_dir().join(format!(
        "collab-close-git-{}-{}",
        std::process::id(),
        now_ms()
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
    assert!(refused.unwrap_err().contains("not merged"));
    assert!(playground.join("wt").is_dir());

    assert!(git(&["merge", "-q", "feature"]).status.success());
    assert!(close_task_resources(&root, Some("playground/wt"), Some("feature")).is_ok());
    assert!(!playground.join("wt").exists());
    assert!(!git(&["rev-parse", "--verify", "feature"]).status.success());
    std::fs::remove_dir_all(root).ok();
}

#[test]
fn cursor_and_codex_subagents_exchange_messages() {
    use crate::subagent::{Action, Record};
    let (server, root) = test_server();
    register(&server, "parent", "%parent");
    register(&server, "cursor-peer", "%cursor");
    register(&server, "codex-peer", "%codex");
    let now = now_ms();
    let cursor = Record {
        id: "cursor-rt".into(),
        parent: "parent".into(),
        peer: "cursor-peer".into(),
        status: "idle".into(),
        session: Some("$cursor".into()),
        pane: Some("%cursor".into()),
        profile: None,
        created_ms: now,
        ready_deadline_ms: now + 90_000,
        last_message: None,
        error: None,
        probe_failures: Vec::new(),
        runtime: Some("cursor".into()),
    };
    let codex = Record {
        id: "codex-rt".into(),
        parent: "parent".into(),
        peer: "codex-peer".into(),
        status: "idle".into(),
        session: Some("$codex".into()),
        pane: Some("%codex".into()),
        profile: None,
        created_ms: now,
        ready_deadline_ms: now + 90_000,
        last_message: None,
        error: None,
        probe_failures: Vec::new(),
        runtime: Some("codex".into()),
    };
    server.commit(&[
        Event::SubagentUpdated {
            subagent: cursor.clone(),
        },
        Event::SubagentUpdated {
            subagent: codex.clone(),
        },
    ]);
    let to_codex = handle_send(
        &server,
        "cursor-peer".into(),
        "codex-peer".into(),
        "notify".into(),
        Some("cursor-to-codex".into()),
        "ping from cursor runtime".into(),
        None,
        "immediate".into(),
    );
    assert!(to_codex.ok, "{}", to_codex.error.unwrap_or_default());
    let to_cursor = handle_send(
        &server,
        "codex-peer".into(),
        "cursor-peer".into(),
        "notify".into(),
        Some("codex-to-cursor".into()),
        "pong from codex runtime".into(),
        None,
        "immediate".into(),
    );
    assert!(to_cursor.ok, "{}", to_cursor.error.unwrap_or_default());
    let to_parent = handle_send(
        &server,
        "cursor-peer".into(),
        "parent".into(),
        "notify".into(),
        Some("cursor-result".into()),
        "cursor finished".into(),
        None,
        "immediate".into(),
    );
    assert!(to_parent.ok, "{}", to_parent.error.unwrap_or_default());
    let from_parent_cursor = crate::subagent::handle(
        &server,
        "parent",
        "token-parent",
        Action::Send {
            id: "cursor-rt".into(),
            subject: "assign-cursor".into(),
            body: "task for cursor".into(),
        },
    );
    assert!(
        from_parent_cursor.ok,
        "{}",
        from_parent_cursor.error.unwrap_or_default()
    );
    assert!(
        crate::subagent::handle(
            &server,
            "cursor-peer",
            "token-cursor-peer",
            Action::Working {
                id: "cursor-rt".into()
            }
        )
        .ok
    );
    assert!(
        crate::subagent::handle(
            &server,
            "cursor-peer",
            "token-cursor-peer",
            Action::Ready {
                id: "cursor-rt".into()
            }
        )
        .ok
    );
    let from_parent_codex = crate::subagent::handle(
        &server,
        "parent",
        "token-parent",
        Action::Send {
            id: "codex-rt".into(),
            subject: "assign-codex".into(),
            body: "task for codex".into(),
        },
    );
    assert!(
        from_parent_codex.ok,
        "{}",
        from_parent_codex.error.unwrap_or_default()
    );
    let cursor_status = crate::subagent::handle(
        &server,
        "parent",
        "token-parent",
        Action::Status {
            id: "cursor-rt".into(),
        },
    );
    let codex_status = crate::subagent::handle(
        &server,
        "parent",
        "token-parent",
        Action::Status {
            id: "codex-rt".into(),
        },
    );
    assert!(cursor_status.ok);
    assert!(codex_status.ok);
    assert_eq!(cursor_status.data["subagent"]["runtime"], "cursor");
    assert_eq!(codex_status.data["subagent"]["runtime"], "codex");
    assert_eq!(cursor_status.data["next_check"], "status");
    assert_eq!(cursor_status.data["progress"], "snapshot");
    assert_eq!(cursor_status.data["close_required"], false);
    let msgs: Vec<_> = server
        .state
        .lock()
        .unwrap()
        .msgs
        .values()
        .cloned()
        .collect();
    assert!(
        msgs.iter().any(|m| m.from == "cursor-peer"
            && m.to == "codex-peer"
            && m.subject.as_deref() == Some("cursor-to-codex")),
        "{msgs:?}"
    );
    assert!(
        msgs.iter().any(|m| m.from == "codex-peer"
            && m.to == "cursor-peer"
            && m.subject.as_deref() == Some("codex-to-cursor")),
        "{msgs:?}"
    );
    assert!(
        msgs.iter().any(|m| m.from == "cursor-peer"
            && m.to == "parent"
            && m.subject.as_deref() == Some("cursor-result")),
        "{msgs:?}"
    );
    std::fs::remove_dir_all(root).unwrap();
}

#[test]
fn worker_status_query_exposes_liveness_identity_and_notification_pressure() {
    let (server, root) = test_server();
    register(&server, "status-worker", "%test-status-worker");
    let resp = dispatch(&Arc::new(server), Req::WorkerStatus { worker_id: None });
    assert!(resp.ok);
    let workers = resp.data["workers"].as_array().unwrap();
    assert_eq!(workers.len(), 1);
    let w = &workers[0];
    assert_eq!(w["id"], "status-worker");
    assert_eq!(w["pane"], "%test-status-worker");
    assert_eq!(w["endpoint_live"], true);
    assert_eq!(w["identity_valid"], true);
    assert_eq!(w["agent_state"], "waiting");
    assert_eq!(w["status"], "waiting");
    assert_eq!(w["unacked_notifications"], 0);
    assert_eq!(w["notifications_paused"], false);
    std::fs::remove_dir_all(root).unwrap();
}

#[test]
fn bulk_ack_with_empty_ids_acknowledges_all_inbox_messages() {
    let (server, root) = test_server();
    register(&server, "sender-worker", "%test-sender-worker");
    register(&server, "bulk-worker", "%test-bulk-worker");
    let server_arc = Arc::new(server);

    // Send 2 messages to bulk-worker
    for i in 1..=2 {
        dispatch(
            &server_arc,
            Req::Send {
                from: "sender-worker".into(),
                worker_id: Some("sender-worker".into()),
                token: Some("token-sender-worker".into()),
                command: Some(send_command(&root, "sender-worker")),
                to: "bulk-worker".into(),
                mtype: "notify".into(),
                subject: Some(format!("test-{i}")),
                body: format!("body {i}"),
                in_reply_to: None,
                delivery: "immediate".into(),
            },
        );
    }

    // Deliver them
    let ids: Vec<String> = server_arc
        .state
        .lock()
        .unwrap()
        .msgs
        .values()
        .map(|m| m.id.clone())
        .collect();
    server_arc.commit(&[Event::Delivered { ids: ids.clone() }]);

    // Bulk ack with empty ids
    let resp = dispatch(
        &server_arc,
        Req::Ack {
            worker_id: "bulk-worker".into(),
            token: "token-bulk-worker".into(),
            ids: vec![],
        },
    );
    assert!(resp.ok);
    assert_eq!(resp.data["acked"].as_array().unwrap().len(), 2);

    // Verify all messages are read
    let state = server_arc.state.lock().unwrap();
    assert!(state.msgs.values().all(|m| m.state == "read"));
    drop(state);
    std::fs::remove_dir_all(root).unwrap();
}

#[test]
fn external_or_operator_sender_can_send_without_registration_or_pane() {
    let (server, root) = test_server();
    register(&server, "recipient-worker", "%recipient");
    let resp = handle_send(
        &server,
        "external-operator".into(),
        "recipient-worker".into(),
        "notify".into(),
        Some("test-topic".into()),
        "hello from outside tmux".into(),
        None,
        "immediate".into(),
    );
    assert!(resp.ok);
    assert_eq!(resp.data["durable"].as_bool(), Some(true));
    let msg_id = resp.data["msg_id"].as_str().unwrap();

    let state = server.state.lock().unwrap();
    let msg = state.msgs.get(msg_id).unwrap();
    assert_eq!(msg.from, "external-operator");
    assert_eq!(msg.to, "recipient-worker");
    assert_eq!(msg.body, "hello from outside tmux");
    drop(state);
    std::fs::remove_dir_all(root).unwrap();
}

#[test]
fn worker_freed_transitions_notify_live_master() {
    let (server, root) = test_server();
    register(&server, "master-worker", "%master");
    register(&server, "task-worker", "%worker");
    let server_arc = std::sync::Arc::new(server);
    let promote_resp = dispatch(
        &server_arc,
        Req::MasterPromote {
            worker_id: "master-worker".into(),
            token: "token-master-worker".into(),
            approval: "approved".into(),
        },
    );
    assert!(promote_resp.ok);

    let mut initial_rec = crate::server::keepalive::Record::default();
    initial_rec.observed = "working".into();
    initial_rec.idle_since_ms = 1000;
    server_arc.commit(&[Event::KeepaliveUpdated {
        worker_id: "task-worker".into(),
        record: initial_rec,
    }]);

    crate::server::keepalive::tick_with(
        &server_arc,
        2000,
        &|_pane| crate::server::knock::AgentState::Waiting,
        &|_pane, _text| true,
        &|_worker_id, _pane| Ok(true),
    );

    let state = server_arc.state.lock().unwrap();
    let idle_alert = state
        .msgs
        .values()
        .find(|m| m.to == "master-worker" && m.subject == Some("worker-idle: task-worker".into()));
    assert!(
        idle_alert.is_some(),
        "expected worker-idle alert sent to master"
    );
    let alert = idle_alert.unwrap();
    assert!(alert.body.contains("now idle with no active task"));
    drop(state);
    std::fs::remove_dir_all(root).unwrap();
}

#[test]
fn master_working_to_idle_notifies_itself_once_with_scheduling_contract() {
    let (server, root) = test_server();
    register(&server, "master-worker", "%master");
    let server_arc = std::sync::Arc::new(server);
    let promote_resp = dispatch(
        &server_arc,
        Req::MasterPromote {
            worker_id: "master-worker".into(),
            token: "token-master-worker".into(),
            approval: "approved".into(),
        },
    );
    assert!(promote_resp.ok);

    let mut initial_rec = crate::server::keepalive::Record::default();
    initial_rec.observed = "working".into();
    initial_rec.idle_since_ms = 1000;
    server_arc.commit(&[Event::KeepaliveUpdated {
        worker_id: "master-worker".into(),
        record: initial_rec,
    }]);

    crate::server::keepalive::tick_with(
        &server_arc,
        2000,
        &|_pane| crate::server::knock::AgentState::Waiting,
        &|_pane, _text| true,
        &|_worker_id, _pane| Ok(true),
    );

    let state = server_arc.state.lock().unwrap();
    let idle_alerts: Vec<_> = state
        .msgs
        .values()
        .filter(|m| {
            m.to == "master-worker" && m.subject == Some("master-idle: master-worker".into())
        })
        .collect();
    assert_eq!(idle_alerts.len(), 1, "one scheduling wake per transition");
    let alert = idle_alerts[0];
    assert!(alert.body.contains("task graph"));
    assert!(alert.body.contains("saturation"));
    assert!(alert.body.contains("Scheduling continues"));
    assert!(alert.body.contains(
        "no actionable task, dependency, resolvable blocker, or authorized open bug remains"
    ));
    assert!(alert
        .body
        .contains("collab notify unsubscribe sub-default-direct-message-master-worker"));
    assert!(alert.body.contains("record the receipt"));
    let subscription_id = state
        .wake_bindings
        .get(&alert.id)
        .expect("master wake must bind once");
    let subscription = state
        .notification_subscriptions
        .get(subscription_id)
        .unwrap();
    assert_eq!(subscription.worker_id, "master-worker");
    assert_eq!(subscription.event, "direct-message");
    assert_eq!(subscription.status, "armed");
    drop(state);

    crate::server::keepalive::tick_with(
        &server_arc,
        3000,
        &|_pane| crate::server::knock::AgentState::Waiting,
        &|_pane, _text| true,
        &|_worker_id, _pane| Ok(true),
    );
    let state = server_arc.state.lock().unwrap();
    assert_eq!(
        state
            .msgs
            .values()
            .filter(|m| {
                m.to == "master-worker" && m.subject == Some("master-idle: master-worker".into())
            })
            .count(),
        1,
        "duplicate idle observation must not wake again"
    );
    drop(state);

    let mut new_reason = crate::server::keepalive::Record::default();
    new_reason.observed = "idle".into();
    new_reason.idle_episode_reason = "different-reason".into();
    new_reason.idle_episode_notices = 1;
    server_arc.commit(&[Event::KeepaliveUpdated {
        worker_id: "master-worker".into(),
        record: new_reason,
    }]);
    crate::server::keepalive::tick_with(
        &server_arc,
        122000,
        &|_pane| crate::server::knock::AgentState::Waiting,
        &|_pane, _text| true,
        &|_worker_id, _pane| Ok(true),
    );
    let state = server_arc.state.lock().unwrap();
    assert_eq!(
        state
            .msgs
            .values()
            .filter(|m| {
                m.to == "master-worker" && m.subject == Some("master-idle: master-worker".into())
            })
            .count(),
        2,
        "same episode reminder is windowed and new reason does not reset budget"
    );
    drop(state);

    for now in [242000, 362000] {
        crate::server::keepalive::tick_with(
            &server_arc,
            now,
            &|_pane| crate::server::knock::AgentState::Waiting,
            &|_pane, _text| true,
            &|_worker_id, _pane| Ok(true),
        );
    }
    let state = server_arc.state.lock().unwrap();
    assert_eq!(
        state
            .msgs
            .values()
            .filter(|m| {
                m.to == "master-worker" && m.subject == Some("master-idle: master-worker".into())
            })
            .count(),
        3,
        "idle episode suppresses after three reminders"
    );
    drop(state);
    std::fs::remove_dir_all(root).unwrap();
}

#[test]
fn master_idle_requires_live_master_armed_subscription_and_empty_backlog() {
    let (server, root) = test_server();
    register(&server, "unpromoted", "%unpromoted");
    let server_arc = std::sync::Arc::new(server);
    let mut initial_rec = crate::server::keepalive::Record::default();
    initial_rec.observed = "working".into();
    server_arc.commit(&[Event::KeepaliveUpdated {
        worker_id: "unpromoted".into(),
        record: initial_rec.clone(),
    }]);
    crate::server::keepalive::tick_with(
        &server_arc,
        2000,
        &|_pane| crate::server::knock::AgentState::Waiting,
        &|_pane, _text| true,
        &|_worker_id, _pane| Ok(true),
    );
    assert!(!server_arc
        .state
        .lock()
        .unwrap()
        .msgs
        .values()
        .any(|m| m.subject == Some("master-idle: unpromoted".into())));

    let promote_resp = dispatch(
        &server_arc,
        Req::MasterPromote {
            worker_id: "unpromoted".into(),
            token: "token-unpromoted".into(),
            approval: "approved".into(),
        },
    );
    assert!(promote_resp.ok);
    server_arc.commit(&[
        Event::KeepaliveUpdated {
            worker_id: "unpromoted".into(),
            record: initial_rec.clone(),
        },
        Event::NotificationStatus {
            subscription_id: "sub-default-direct-message-unpromoted".into(),
            status: "consumed".into(),
            updated_ms: 2001,
        },
    ]);
    crate::server::keepalive::tick_with(
        &server_arc,
        3000,
        &|_pane| crate::server::knock::AgentState::Waiting,
        &|_pane, _text| true,
        &|_worker_id, _pane| Ok(true),
    );
    assert!(!server_arc
        .state
        .lock()
        .unwrap()
        .msgs
        .values()
        .any(|m| m.subject == Some("master-idle: unpromoted".into())));
    std::fs::remove_dir_all(root).unwrap();

    let (server, root) = test_server();
    register(&server, "busy-master", "%master");
    let server_arc = std::sync::Arc::new(server);
    assert!(
        dispatch(
            &server_arc,
            Req::MasterPromote {
                worker_id: "busy-master".into(),
                token: "token-busy-master".into(),
                approval: "approved".into(),
            }
        )
        .ok
    );
    create_task(&server_arc, "busy-master", "task-actionable", "feature");
    server_arc.commit(&[Event::KeepaliveUpdated {
        worker_id: "busy-master".into(),
        record: initial_rec,
    }]);
    crate::server::keepalive::tick_with(
        &server_arc,
        4000,
        &|_pane| crate::server::knock::AgentState::Waiting,
        &|_pane, _text| true,
        &|_worker_id, _pane| Ok(true),
    );
    assert!(!server_arc
        .state
        .lock()
        .unwrap()
        .msgs
        .values()
        .any(|m| m.subject == Some("master-idle: busy-master".into())));
    std::fs::remove_dir_all(root).unwrap();
}

#[test]
fn worker_unresponsive_notifies_live_master_with_snapshot_advice() {
    let (server, root) = test_server();
    register(&server, "master-worker", "%master");
    register(&server, "stuck-worker", "%stuck");
    let server_arc = std::sync::Arc::new(server);
    let promote_resp = dispatch(
        &server_arc,
        Req::MasterPromote {
            worker_id: "master-worker".into(),
            token: "token-master-worker".into(),
            approval: "approved".into(),
        },
    );
    assert!(promote_resp.ok);

    crate::server::keepalive::tick_with(
        &server_arc,
        2000,
        &|_pane| crate::server::knock::AgentState::Absent,
        &|_pane, _text| true,
        &|_worker_id, _pane| Ok(true),
    );

    let state = server_arc.state.lock().unwrap();
    let alert = state.msgs.values().find(|m| {
        m.to == "master-worker" && m.subject == Some("worker-unresponsive: stuck-worker".into())
    });
    assert!(
        alert.is_some(),
        "expected worker-unresponsive alert sent to master"
    );
    let alert = alert.unwrap();
    assert!(alert
        .body
        .contains("subagent snapshot stuck-worker --lines 40"));
    drop(state);
    std::fs::remove_dir_all(root).unwrap();
}

#[test]
fn closed_and_delivered_tasks_do_not_trigger_keepalives() {
    let (server, root) = test_server();
    register(&server, "worker-a", "%pane-a");
    let server_arc = std::sync::Arc::new(server);
    let base = now_ms();

    // Create a task that is delivered
    server_arc.commit(&[Event::TaskCreated {
        task: TaskRec {
            id: "task-delivered".into(),
            owner: "worker-a".into(),
            created_by: "worker-a".into(),
            feature_id: None,
            worktree_path: None,
            branch: None,
            base_commit: None,
            priority: "p2".into(),
            status: "delivered".into(),
            next_step: None,
            wait: None,
            created_ms: base,
            updated_ms: base,
        },
    }]);

    // Tick scheduler - delivered task must NOT generate keepalive!
    let sends = std::cell::Cell::new(0);
    crate::server::keepalive::tick_with(
        &server_arc,
        base + 900_000,
        &|_| crate::server::knock::AgentState::Waiting,
        &|_, _| {
            sends.set(sends.get() + 1);
            true
        },
        &|_, _| Ok(true),
    );
    assert_eq!(sends.get(), 0, "delivered task must not trigger keepalive");

    // Now update task to closed
    server_arc.commit(&[Event::TaskUpdated {
        task: TaskRec {
            id: "task-delivered".into(),
            owner: "worker-a".into(),
            created_by: "worker-a".into(),
            feature_id: None,
            worktree_path: None,
            branch: None,
            base_commit: None,
            priority: "p2".into(),
            status: "closed".into(),
            next_step: None,
            wait: None,
            created_ms: base,
            updated_ms: base,
        },
    }]);

    // Tick scheduler - closed task must NOT generate keepalive!
    crate::server::keepalive::tick_with(
        &server_arc,
        base + 1_800_000,
        &|_| crate::server::knock::AgentState::Waiting,
        &|_, _| {
            sends.set(sends.get() + 1);
            true
        },
        &|_, _| Ok(true),
    );
    assert_eq!(sends.get(), 0, "closed task must not trigger keepalive");
    std::fs::remove_dir_all(root).unwrap();
}

#[test]
fn status_all_aggregates_workers_tasks_subagents_and_summary() {
    let (server, root) = test_server();
    let server_arc = Arc::new(server);

    // Register a worker
    server_arc.commit(&[Event::Registered {
        worker: WorkerRec {
            id: "worker-1".into(),
            token: "tok-1".into(),
            pane: Some("%1".into()),
            cwd: root.display().to_string(),
            registered_ms: 1000,
        },
    }]);

    // Create a task
    server_arc.commit(&[Event::TaskCreated {
        task: TaskRec {
            id: "task-1".into(),
            owner: "worker-1".into(),
            created_by: "master".into(),
            feature_id: None,
            worktree_path: None,
            branch: None,
            base_commit: None,
            priority: "normal".into(),
            status: "working".into(),
            next_step: Some("implementing".into()),
            wait: None,
            created_ms: 1000,
            updated_ms: 1000,
        },
    }]);

    let resp = dispatch(&server_arc, Req::StatusAll);
    assert!(resp.ok);
    assert_eq!(resp.data["summary"]["workers"], 1);
    assert_eq!(resp.data["summary"]["tasks"], 1);
    assert_eq!(resp.data["workers"].as_array().unwrap().len(), 1);
    assert_eq!(resp.data["workers"][0]["id"], "worker-1");
    assert_eq!(resp.data["tasks"].as_array().unwrap().len(), 1);
    assert_eq!(resp.data["tasks"][0]["id"], "task-1");

    std::fs::remove_dir_all(root).unwrap();
}

#[test]
fn mailbox_read_all_chronological_sort_asc_and_desc() {
    let (server, root) = test_server();
    let server_arc = Arc::new(server);

    // Send messages with different timestamps
    server_arc.commit(&[
        Event::Sent {
            msg: Message {
                id: "m-1".into(),
                from: "alice".into(),
                to: "bob".into(),
                mtype: "notify".into(),
                subject: Some("first".into()),
                body: "first body".into(),
                in_reply_to: None,
                created_ms: 1000,
                state: "delivered".into(),
                wake_attempt_count: 0,
                last_wake_attempt_ms: 0,
            },
        },
        Event::Sent {
            msg: Message {
                id: "m-2".into(),
                from: "bob".into(),
                to: "alice".into(),
                mtype: "notify".into(),
                subject: Some("second".into()),
                body: "second body".into(),
                in_reply_to: None,
                created_ms: 2000,
                state: "pending".into(),
                wake_attempt_count: 0,
                last_wake_attempt_ms: 0,
            },
        },
        Event::Sent {
            msg: Message {
                id: "m-3".into(),
                from: "charlie".into(),
                to: "bob".into(),
                mtype: "notify".into(),
                subject: Some("third".into()),
                body: "third body".into(),
                in_reply_to: None,
                created_ms: 3000,
                state: "pending".into(),
                wake_attempt_count: 0,
                last_wake_attempt_ms: 0,
            },
        },
    ]);

    // Test time-asc (default)
    let asc_resp = dispatch(
        &server_arc,
        Req::MailboxRead {
            all: true,
            sort: Some("time-asc".into()),
            worker_id: None,
        },
    );
    assert!(asc_resp.ok);
    assert_eq!(asc_resp.data["count"], 3);
    let asc_msgs = asc_resp.data["messages"].as_array().unwrap();
    assert_eq!(asc_msgs[0]["id"], "m-1");
    assert_eq!(asc_msgs[1]["id"], "m-2");
    assert_eq!(asc_msgs[2]["id"], "m-3");

    // Test time-desc
    let desc_resp = dispatch(
        &server_arc,
        Req::MailboxRead {
            all: true,
            sort: Some("time-desc".into()),
            worker_id: None,
        },
    );
    assert!(desc_resp.ok);
    let desc_msgs = desc_resp.data["messages"].as_array().unwrap();
    assert_eq!(desc_msgs[0]["id"], "m-3");
    assert_eq!(desc_msgs[1]["id"], "m-2");
    assert_eq!(desc_msgs[2]["id"], "m-1");

    // Test worker filter
    let worker_resp = dispatch(
        &server_arc,
        Req::MailboxRead {
            all: false,
            sort: Some("time-asc".into()),
            worker_id: Some("charlie".into()),
        },
    );
    assert!(worker_resp.ok);
    assert_eq!(worker_resp.data["count"], 1);
    assert_eq!(worker_resp.data["messages"][0]["id"], "m-3");

    std::fs::remove_dir_all(root).unwrap();
}
#[tokio::test]
async fn recv_clears_keepalive_unacked_counter() {
    let (server, root) = test_server();
    register(&server, "peer", "%peer");
    server.commit(&[Event::Sent {
        msg: Message {
            id: "wake-message".into(),
            from: "sender".into(),
            to: "peer".into(),
            mtype: "notify".into(),
            subject: Some("wake".into()),
            body: "message".into(),
            in_reply_to: None,
            created_ms: now_ms(),
            state: "pending".into(),
            wake_attempt_count: 0,
            last_wake_attempt_ms: 0,
        },
    }]);
    let mut keepalive = crate::server::keepalive::Record::default();
    keepalive.unacked = 3;
    keepalive.last_notice_id = Some("stale-notice".into());
    keepalive.suspected_offline = true;
    server.commit(&[Event::KeepaliveUpdated {
        worker_id: "peer".into(),
        record: keepalive,
    }]);
    let server = Arc::new(server);
    let response = handle_poll_async(server.clone(), "peer".into(), 100).await;
    assert!(response.ok);
    let record = server.state.lock().unwrap().keepalives["peer"].clone();
    assert_eq!(record.unacked, 0);
    assert_eq!(record.last_notice_id, None);
    assert!(!record.suspected_offline);
    std::fs::remove_dir_all(root).ok();
}
