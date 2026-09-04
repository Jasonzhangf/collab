use super::*;
use crate::server::state::default_priority;
use std::process::Command;
use std::sync::atomic::{AtomicU64, Ordering};

fn test_server() -> (Server, PathBuf) {
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
            root: root.clone(),
            state: Mutex::new(State::default()),
            journal: Mutex::new(journal),
            pane_alive_check: |_| true,
        },
        root,
    )
}

fn register(server: &Server, id: &str, pane: &str) -> Resp {
    handle_register(
        server,
        id.into(),
        format!("token-{id}"),
        Some(pane.into()),
        "/tmp".into(),
    )
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
    drop(state);
    std::fs::remove_dir_all(root).ok();
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
        handle_task_update(
            &server,
            "peer".into(),
            "token-peer".into(),
            "task".into(),
            Some("merged".into()),
            Some("main verified".into()),
        )
        .ok
    );
    let closed = handle_task_close(&server, "peer".into(), "token-peer".into(), "task".into());
    assert!(closed.ok);
    let state = server.state.lock().unwrap();
    assert_eq!(state.tasks["task"].status, "closed");
    assert!(
        state.msgs.is_empty(),
        "normal lifecycle must not report to peers"
    );
    drop(state);
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
        handle_task_update(
            &server,
            "holder".into(),
            "token-holder".into(),
            "held".into(),
            Some("merged".into()),
            Some("main verified".into()),
        )
        .ok
    );
    assert!(
        handle_task_close(
            &server,
            "holder".into(),
            "token-holder".into(),
            "held".into(),
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
            to: "peer".into(),
            mtype: "notify".into(),
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
fn worktree_path_budget_accepts_short_slug_and_rejects_escape() {
    let root = PathBuf::from("/tmp/project");
    assert!(validate_worktree_path(&root, "./playground/ar03-0828").is_ok());
    assert!(validate_worktree_path(
        &root,
        "./playground/v3-direct-sse-terminal-observability-20260827-long-run-id"
    )
    .is_err());
    assert!(validate_worktree_path(&root, "./playground/../outside").is_err());
}

#[test]
fn tmux_notification_is_short_and_contains_no_mailbox_body() {
    let text = notification_text("message-id");
    assert_eq!(text, "COLLAB_NOTIFY message-id");
    assert!(!text.contains("RESOURCE_"));
    assert!(!text.contains("CONTINUE_TASK"));
}

#[test]
fn send_without_subscription_is_mailbox_only_and_deduplicated() {
    let (server, root) = test_server();
    register(&server, "sender", "%collab-missing-sender");
    register(&server, "recipient", "%collab-missing-recipient");
    let first = handle_send(
        &server,
        "sender".into(),
        "recipient".into(),
        "notify".into(),
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
    assert_eq!(state.msgs[message_id].wake_attempt_count, 0);
    assert_eq!(
        response.data["notification"],
        "mailbox-only-no-subscription"
    );
    drop(state);
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
        Req::MasterId,
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
        Some("invalid task transition reviewed -> merged")
    );
    assert_eq!(
        server.state.lock().unwrap().tasks["task"].status,
        "reviewed"
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
        "\"collab_master\"",
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
