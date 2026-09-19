use std::io::{BufRead, BufReader, Write};
use std::os::unix::net::UnixListener;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};
use std::sync::atomic::{AtomicU64, Ordering};

static NEXT_FIXTURE: AtomicU64 = AtomicU64::new(0);

struct Fixture {
    root: PathBuf,
    state: PathBuf,
}

impl Fixture {
    fn new(name: &str) -> Self {
        let id = NEXT_FIXTURE.fetch_add(1, Ordering::Relaxed);
        let root =
            PathBuf::from("/tmp").join(format!("collab-r-{name}-{}-{id}", std::process::id()));
        let state = root.join("host-state");
        std::fs::create_dir_all(&state).unwrap();
        Self { root, state }
    }

    fn socket(&self) -> PathBuf {
        self.state.join("server.sock")
    }
}

impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.root);
    }
}

fn run_route(fixture: &Fixture, cwd: &Path, thread_id: &str, tmux: bool) -> Output {
    let mut command = Command::new(env!("CARGO_BIN_EXE_collab"));
    command
        .args(["route", "resolve"])
        .current_dir(cwd)
        .env("COLLAB_STATE_DIR", &fixture.state)
        .env("CODEX_THREAD_ID", thread_id)
        .env_remove("COLLAB_SOCKET_PATH")
        .env_remove("COLLAB_HOST_SOCKET");
    if tmux {
        command.env("TMUX", "/tmp/tmux-test,1,0");
        command.env("TMUX_PANE", "%1");
    } else {
        command.env_remove("TMUX");
        command.env_remove("TMUX_PANE");
    }
    command.output().unwrap()
}

fn run_up(fixture: &Fixture, cwd: &Path, thread_id: &str) -> Output {
    Command::new(env!("CARGO_BIN_EXE_collab"))
        .arg("up")
        .current_dir(cwd)
        .env("COLLAB_STATE_DIR", &fixture.state)
        .env("CODEX_THREAD_ID", thread_id)
        .env_remove("COLLAB_SOCKET_PATH")
        .env_remove("COLLAB_HOST_SOCKET")
        .output()
        .unwrap()
}

fn start_route_daemon(
    fixture: &Fixture,
    thread_id: &str,
    canonical_root: &Path,
) -> std::thread::JoinHandle<()> {
    let _ = std::fs::remove_file(fixture.socket());
    let listener = UnixListener::bind(fixture.socket()).unwrap();
    let canonical_root = canonical_root.canonicalize().unwrap();
    let thread_id = thread_id.to_owned();
    std::thread::spawn(move || {
        let (mut stream, _) = listener.accept().unwrap();
        let mut line = String::new();
        BufReader::new(&stream).read_line(&mut line).unwrap();
        let request: serde_json::Value = serde_json::from_str(&line).unwrap();
        assert_eq!(request["op"], "RouteResolve");
        assert_eq!(request["native_thread_id"], thread_id);
        assert!(request.get("project_context").is_none());
        let response = serde_json::json!({
            "ok": true,
            "app_scope_id": "appserver-global",
            "project_scope": canonical_root,
            "canonical_root": canonical_root,
            "storage_root": canonical_root,
            "agent_id": "agent-global",
            "binding_id": "binding-global",
            "endpoint_generation": 4,
            "native_thread_id": thread_id,
        });
        stream
            .write_all(format!("{response}\n").as_bytes())
            .unwrap();
    })
}

#[test]
fn route_resolve_is_identical_across_cwd_worktree_and_tmux_context() {
    let fixture = Fixture::new("cross-cwd");
    let canonical = fixture.root.join("appsdk-main");
    let worktree = fixture.root.join("other-repo/playground/task");
    std::fs::create_dir_all(&canonical).unwrap();
    std::fs::create_dir_all(&worktree).unwrap();
    let responder = start_route_daemon(&fixture, "thread-global", &canonical);

    let main = run_route(&fixture, &canonical, "thread-global", false);
    assert!(
        main.status.success(),
        "main route failed: {}",
        String::from_utf8_lossy(&main.stderr)
    );
    let route: serde_json::Value = serde_json::from_slice(&main.stdout).unwrap();
    assert_eq!(
        route["canonical_root"],
        canonical.canonicalize().unwrap().to_string_lossy().as_ref()
    );
    assert_eq!(route["native_thread_id"], "thread-global");
    responder.join().unwrap();

    let responder = start_route_daemon(&fixture, "thread-global", &canonical);
    let worktree_output = run_route(&fixture, &worktree, "thread-global", true);
    assert!(
        worktree_output.status.success(),
        "worktree route failed: {}",
        String::from_utf8_lossy(&worktree_output.stderr)
    );
    assert_eq!(
        serde_json::from_slice::<serde_json::Value>(&worktree_output.stdout).unwrap(),
        route
    );
    responder.join().unwrap();
}

#[test]
fn route_resolve_without_thread_identity_fails_explicitly() {
    let fixture = Fixture::new("missing-thread");
    let cwd = fixture.root.join("cwd");
    std::fs::create_dir_all(&cwd).unwrap();
    let output = Command::new(env!("CARGO_BIN_EXE_collab"))
        .args(["route", "resolve"])
        .current_dir(&cwd)
        .env("COLLAB_STATE_DIR", &fixture.state)
        .env_remove("CODEX_THREAD_ID")
        .output()
        .unwrap();
    assert!(!output.status.success());
    assert!(String::from_utf8_lossy(&output.stderr)
        .contains("route resolve requires --native-thread-id or CODEX_THREAD_ID"));
}

#[test]
fn lifecycle_up_initializes_the_exact_cwd_instead_of_following_a_thread_route() {
    let fixture = Fixture::new("up-cwd");
    let canonical = fixture.root.join("appsdk-main");
    let current = fixture.root.join("uninitialized-project");
    std::fs::create_dir_all(&canonical).unwrap();
    std::fs::create_dir_all(&current).unwrap();

    let output = run_up(&fixture, &current, "thread-global");

    assert!(
        output.status.success(),
        "up failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let receipt: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(receipt["ok"], true);
    assert_eq!(
        receipt["server"],
        fixture.state.join("server.sock").to_string_lossy().as_ref()
    );
    assert!(current.join(".agent-collab").is_dir());
    assert!(!canonical.join(".agent-collab").exists());

    let down = Command::new(env!("CARGO_BIN_EXE_collab"))
        .arg("down")
        .current_dir(&current)
        .env("COLLAB_STATE_DIR", &fixture.state)
        .env("CODEX_THREAD_ID", "thread-global")
        .env_remove("COLLAB_SOCKET_PATH")
        .env_remove("COLLAB_HOST_SOCKET")
        .output()
        .unwrap();
    assert!(
        down.status.success(),
        "down failed: {}",
        String::from_utf8_lossy(&down.stderr)
    );
}
