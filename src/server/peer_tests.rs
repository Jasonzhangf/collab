use super::*;
use crate::server::state::default_priority;
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
            pane_alive_check: |_| true,
            pane_owner_check: |_, _| true,
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
fn failed_journal_cannot_apply_a_keepalive_reservation() {
    let (server, root) = test_server();
    *server.journal.lock().unwrap() =
        std::fs::File::open(root.join(".agent-collab/server/journal.jsonl")).unwrap();
    let mut state = State::default();
    let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        server.commit_locked(
            &mut state,
            &[Event::KeepaliveUpdated {
                worker_id: "worker".into(),
                record: crate::server::keepalive::Record::default(),
            }],
        );
    }));
    assert!(result.is_err());
    assert!(state.keepalives.is_empty());
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
fn master_promotion_requires_user_approval_and_existing_master_delegates() {
    let (server, root) = test_server();
    register(&server, "peer-a", "%a");
    register(&server, "peer-b", "%b");

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
    fn only_b(pane: &str) -> bool {
        pane == "%b"
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
fn master_promotion_requires_live_tmux_pane() {
    fn none_alive(_: &str) -> bool {
        false
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
            expires_ms: now + 600_000,
            status: "armed".into(),
            created_ms: now,
            updated_ms: now,
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
        "COLLAB_NOTIFY message-id [release] RESOURCE_RELEASED feature=shared | ACTION: weigh priority from the ID and subject. When selected, run collab msg message-id, then execute the actionable in-scope request; do not stop at ACK or waiting."
    );
    assert!(text.contains("execute the actionable in-scope request"));
    assert!(text.contains("do not stop at ACK or waiting"));
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
        "COLLAB_NOTIFY message-id [this subject is deliberately longer than forty …] line one\\nline two\\t中文 | ACTION: weigh priority from the ID and subject. When selected, run collab msg message-id, then execute the actionable in-scope request; do not stop at ACK or waiting."
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
    assert_eq!(state.msgs[message_id].wake_attempt_count, 0);
    assert_eq!(response.data["notification"], "subscribed-not-sent");
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
