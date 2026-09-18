use std::path::Path;
use std::process::Command;

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

    let output = Command::new(env!("CARGO_BIN_EXE_collab"))
        .arg("context")
        .current_dir(&root)
        .env_remove("TMUX")
        .env_remove("TMUX_PANE")
        .output()
        .unwrap();

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
