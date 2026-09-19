use std::io::{BufRead, BufReader, Write};
use std::os::unix::net::UnixListener;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::atomic::{AtomicU64, Ordering};
use std::thread::JoinHandle;

static NEXT_STATE: AtomicU64 = AtomicU64::new(0);

fn start_route_not_found_daemon(state: &Path) -> JoinHandle<()> {
    std::fs::create_dir_all(state).unwrap();
    let socket = state.join("server.sock");
    let _ = std::fs::remove_file(&socket);
    let listener = UnixListener::bind(socket).unwrap();
    std::thread::spawn(move || {
        let (mut stream, _) = listener.accept().unwrap();
        let mut line = String::new();
        BufReader::new(&stream).read_line(&mut line).unwrap();
        let request: serde_json::Value = serde_json::from_str(&line).unwrap();
        assert_eq!(request["op"], "RouteResolve");
        assert!(request.get("project_context").is_none());
        stream
            .write_all(
                br#"{"ok":false,"error":"ROUTE_RESOLVE_NOT_FOUND: no registered route","data":null}
"#,
            )
            .unwrap();
    })
}

fn run_context(root: &Path) -> std::process::Output {
    run_context_with_args(root, &[])
}

fn run_context_with_args(root: &Path, args: &[&str]) -> std::process::Output {
    let state = PathBuf::from("/tmp").join(format!(
        "collab-u-{}-{}",
        std::process::id(),
        NEXT_STATE.fetch_add(1, Ordering::Relaxed)
    ));
    let responder = start_route_not_found_daemon(&state);
    let output = Command::new(env!("CARGO_BIN_EXE_collab"))
        .arg("context")
        .args(args)
        .current_dir(root)
        .env("COLLAB_STATE_DIR", &state)
        .env_remove("TMUX")
        .env_remove("TMUX_PANE")
        .env("CODEX_THREAD_ID", "context-test-thread")
        .output()
        .unwrap();
    responder.join().unwrap();
    std::fs::remove_dir_all(state).ok();
    output
}

#[test]
fn context_returns_structured_unregistered_without_side_effects() {
    let root = std::env::temp_dir().join(format!(
        "collab-context-unregistered-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    std::fs::create_dir_all(&root).unwrap();

    let output = run_context(&root);

    assert!(
        output.status.success(),
        "context failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let context: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    let expected_root = std::fs::canonicalize(&root).unwrap();
    assert_eq!(context["registered"], false);
    assert_eq!(
        context["project_root"],
        expected_root.to_string_lossy().as_ref()
    );
    assert_eq!(context["cwd"], expected_root.to_string_lossy().as_ref());
    assert_eq!(context["next_action"], "appsdk init .");
    assert_eq!(context["schema_version"], 1);
    assert_eq!(context["registration"]["status"], "unregistered");
    assert_eq!(
        context["next_actions"],
        serde_json::json!(["appsdk init ."])
    );
    assert_eq!(context["liveness"]["presence"], "unregistered");
    assert_eq!(context["liveness"]["live"], false);
    assert!(context["agent"].is_null());
    assert_eq!(context["tasks"], serde_json::json!([]));
    assert_eq!(context["peers"], serde_json::json!([]));
    assert_eq!(context["worktrees"], serde_json::json!([]));
    assert_eq!(context["subscriptions"], serde_json::json!([]));
    assert_eq!(context["inbox"]["unread"], 0);
    assert_eq!(context["master"]["status"], "unknown");
    assert_eq!(context["master"]["reason"], "peer_unregistered");
    assert_eq!(context["authority"]["managed_subagent"], false);
    assert_eq!(context["authority"]["must_obey_master"], false);
    assert_eq!(context["authority"]["may_decline_master_invite"], false);
    assert!(!Path::new(&root).join(".agent-collab").exists());
    assert_eq!(std::fs::read_dir(&root).unwrap().count(), 0);

    std::fs::remove_dir_all(root).ok();
}

#[test]
fn context_from_unregistered_worktree_points_directly_to_main_tree_initialization() {
    let root = std::env::temp_dir().join(format!(
        "collab-context-worktree-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    let worktree = root.join("playground/context-candidate");
    std::fs::create_dir_all(&worktree).unwrap();

    let output = run_context(&worktree);

    assert!(
        output.status.success(),
        "context failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let context: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(context["registered"], false);
    assert_eq!(
        context["next_action"],
        "return to the canonical project main checkout and run `appsdk init .`"
    );
    assert_eq!(
        context["next_actions"],
        serde_json::json!([
            "return to the canonical project main checkout and run `appsdk init .`"
        ])
    );
    assert_eq!(context["master"]["status"], "unknown");
    assert_eq!(context["master"]["reason"], "peer_unregistered");
    assert!(!worktree.join(".agent-collab").exists());

    std::fs::remove_dir_all(root).ok();
}

#[test]
fn explicit_worker_does_not_fall_back_to_another_thread_binding() {
    let root = std::env::temp_dir().join(format!(
        "collab-context-explicit-worker-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    let other = root.join("host-state/identities/worker-b");
    std::fs::create_dir_all(&other).unwrap();
    std::fs::write(
        other.join("identity.json"),
        r#"{
  "worker_id": "worker-b",
  "token": "token-b",
  "runtime": {
    "agent_id": "worker-b",
    "runtime_id": "runtime-b",
    "appserver_id": "appserver-cli",
    "endpoint_generation": 1,
    "binding_id": "binding-b",
    "native_thread_id": "context-test-thread"
  },
  "transport": {
    "kind": "appserver",
    "endpoint": "unix:///tmp/test.sock",
    "namespace": "codex_tui",
    "thread_id": "context-test-thread",
    "capabilities": [],
    "self_check": "test"
  }
}"#,
    )
    .unwrap();

    let output = run_context_with_args(&root, &["--worker", "worker-a"]);

    assert!(
        output.status.success(),
        "context failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let context: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(context["registered"], false);
    assert!(context["identity"].is_null());
    assert_ne!(context["identity"]["worker_id"], "worker-b");

    std::fs::remove_dir_all(root).ok();
}

#[test]
fn context_does_not_register_an_initialized_project_without_an_identity() {
    let root = std::env::temp_dir().join(format!(
        "collab-context-initialized-unregistered-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    std::fs::create_dir_all(root.join(".agent-collab/runs")).unwrap();

    let output = run_context(&root);

    assert!(
        output.status.success(),
        "context failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let context: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(context["registered"], false);
    assert_eq!(context["next_action"], "appsdk init .");
    assert_eq!(
        context["next_actions"],
        serde_json::json!(["appsdk init ."])
    );
    assert_eq!(context["liveness"]["presence"], "unregistered");
    assert_eq!(context["master"]["status"], "unknown");
    assert_eq!(
        std::fs::read_dir(root.join(".agent-collab/runs"))
            .unwrap()
            .count(),
        0
    );
    assert!(!root.join("host-state/identities").exists());
    assert!(!root.join(".agent-collab/server").exists());

    std::fs::remove_dir_all(root).ok();
}
