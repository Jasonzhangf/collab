use std::path::Path;
use std::process::Command;

fn run_context(root: &Path) -> std::process::Output {
    Command::new(env!("CARGO_BIN_EXE_collab"))
        .arg("context")
        .current_dir(root)
        .env_remove("TMUX")
        .env_remove("TMUX_PANE")
        .env("CODEX_THREAD_ID", "context-test-thread")
        .output()
        .unwrap()
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
    assert!(!Path::new(&root).join(".agent-collab").exists());
    assert_eq!(std::fs::read_dir(&root).unwrap().count(), 0);

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
        std::fs::read_dir(root.join(".agent-collab/runs"))
            .unwrap()
            .count(),
        0
    );
    assert!(!root.join(".agent-collab/server").exists());

    std::fs::remove_dir_all(root).ok();
}
