mod client;
mod identity;
mod proto;
mod scope;
mod server;

use clap::{Parser, Subcommand};
use identity::Identity;
use proto::{Req, Resp};
use scope::Scope;
use serde_json::json;

#[derive(Parser)]
#[command(
    name = "collab",
    version,
    about = "Project-local coordination for multi-agent work"
)]
struct Cli {
    #[command(subcommand)]
    cmd: Cmd,
}

#[derive(Subcommand)]
enum Cmd {
    /// Create .agent-collab skeleton in the current directory
    Init,
    /// Hidden: daemon entrypoint (spawned by `up`)
    #[command(hide = true)]
    Serve,
    /// Start the coordination daemon (idempotent)
    Up,
    /// Explicitly stop the daemon and disable automatic restart
    Down,
    /// Show server summary
    Status,
    /// Deprecated: declared roles were removed
    Role,
    /// List registered peers and their local activity projection
    Who,
    /// Deprecated: permanent master role was removed
    Master {
        #[command(subcommand)]
        cmd: Option<MasterCmd>,
    },
    /// Refresh this worker's tmux pane/session registration
    Worker {
        #[command(subcommand)]
        cmd: WorkerCmd,
    },
    /// Deprecated: permanent master role was removed
    TransferMaster { target: String },
    /// Deprecated: use explicit lifecycle cleanup or daemon migration tooling
    RemoveWorker {
        target: String,
        #[arg(long)]
        force: bool,
    },
    /// Deprecated: destructive binding reset was removed
    Reset {
        #[arg(long)]
        force: bool,
    },
    /// Get or create your worker identity and announce your tmux pane
    Whoami {
        #[arg(long)]
        worker: Option<String>,
        #[arg(long)]
        pane: Option<String>,
    },
    /// Send a message to another worker
    #[command(alias = "sendmessage")]
    Send {
        #[arg(long)]
        to: String,
        #[arg(long, default_value = "notify")]
        r#type: String,
        #[arg(long)]
        in_reply_to: Option<String>,
        #[arg(long, default_value = "immediate", hide = true)]
        delivery: String,
        #[arg(trailing_var_arg = true)]
        body: Vec<String>,
    },
    /// Discover and explicitly subscribe to finite notifications
    Notify {
        #[command(subcommand)]
        cmd: NotifyCmd,
    },
    /// Block until messages arrive (long-poll)
    Recv {
        #[arg(long, default_value_t = 600)]
        timeout: u64,
        /// act as another registered worker (testing / delegated runs)
        #[arg(long)]
        worker: Option<String>,
    },
    /// List unread inbox
    Inbox {
        #[arg(long)]
        worker: Option<String>,
    },
    /// Return one read-only authoritative snapshot after a notification/restart
    Context {
        #[arg(long)]
        worker: Option<String>,
    },
    /// Mark messages as read
    Ack {
        ids: Vec<String>,
        #[arg(long)]
        worker: Option<String>,
    },
    /// Query message status (wake attempts, answered)
    Msg { msg_id: String },
    /// Task registration and lifecycle (task owner owns feature/worktree)
    Task {
        #[command(subcommand)]
        cmd: TaskCmd,
    },
    /// Inspect, plan, apply, and verify an existing-project migration
    Migrate {
        #[command(subcommand)]
        cmd: MigrateCmd,
    },
}

#[derive(Subcommand)]
enum NotifyCmd {
    /// List supported notification methods and events
    Methods,
    /// Register one finite, one-shot notification subscription
    Subscribe {
        #[arg(long)]
        event: String,
        #[arg(long)]
        subject: Option<String>,
        #[arg(long)]
        trigger_ms: Option<i64>,
        #[arg(long)]
        ttl_seconds: u64,
    },
    /// List the caller's notification subscriptions
    Status,
    /// Cancel one caller-owned notification subscription
    Unsubscribe { subscription_id: String },
}

#[derive(Subcommand)]
enum TaskCmd {
    /// Register a task owned by the calling peer
    Register {
        id: String,
        #[arg(long)]
        owner: Option<String>,
        #[arg(long)]
        feature: Option<String>,
        #[arg(long)]
        worktree: Option<String>,
        #[arg(long)]
        branch: Option<String>,
        #[arg(long)]
        base_commit: Option<String>,
        /// Owner-local priority: p0 (highest) through p4
        #[arg(long)]
        priority: Option<String>,
        /// Next lifecycle step for the task owner
        #[arg(long)]
        next: Option<String>,
        /// Complete /goal prompt; must begin with /goal and contains no wrapper text
        #[arg(long)]
        goal: Option<String>,
    },
    /// Relocate the caller's task to a short playground worktree
    Relocate {
        id: String,
        #[arg(long)]
        worktree: String,
        #[arg(long)]
        branch: Option<String>,
        #[arg(long)]
        base_commit: Option<String>,
    },
    /// Update task status/next step by its owner
    Update {
        id: String,
        #[arg(long)]
        status: Option<String>,
        #[arg(long)]
        next: Option<String>,
    },
    /// Deprecated: peers self-register tasks; no central available queue
    Claim { id: String },
    /// Put an owned task into resource-waiting state until another task releases
    Wait {
        id: String,
        #[arg(long = "for")]
        blocking_task: String,
    },
    /// Record owner-local delivery evidence before integration
    Deliver {
        id: String,
        #[arg(long)]
        evidence: String,
        #[arg(long)]
        worktree: String,
    },
    /// Mark the caller's task blocked without notifying unrelated peers
    Block {
        id: String,
        #[arg(long)]
        next: Option<String>,
    },
    /// Close a merged task and clean up its declared worktree/branch
    Close { id: String },
    /// Deprecated: peers self-register tasks; no central dispatch
    Dispatch,
    /// Show task registry
    Status { id: Option<String> },
}

#[derive(Subcommand)]
enum MasterCmd {
    /// Deprecated: permanent master recovery was removed
    Recover,
}

#[derive(Subcommand)]
enum WorkerCmd {
    /// Re-register the current tmux pane without changing task ownership
    Recover,
}

#[derive(Subcommand)]
enum MigrateCmd {
    /// Inspect current durable state and migration blockers
    Inspect,
    /// Create a migration plan; does not freeze admission
    Plan,
    /// Freeze task admission and persist a deterministic snapshot
    Apply,
    /// Verify replayed state and resume task admission
    Verify,
}

fn out<T: serde::Serialize>(v: &T) {
    println!("{}", serde_json::to_string_pretty(v).unwrap());
}

/// Register an identity with the server (idempotent for the same token).
fn register(scope: &Scope, ident: &Identity) -> anyhow::Result<()> {
    let cwd = scope.root.display().to_string();
    let _: serde_json::Value = client::call(
        &scope.sock_path(),
        &Req::Register {
            worker_id: ident.worker_id.clone(),
            token: ident.token.clone(),
            pane: ident.pane.clone(),
            cwd,
        },
    )?;
    Ok(())
}

/// Identity bootstrap used by every command that acts as a worker.
fn me(scope: &Scope, worker: Option<String>) -> anyhow::Result<Identity> {
    let ident = identity::load_or_create(scope, worker, None)?;
    register(scope, &ident)?;
    Ok(ident)
}

fn main() {
    let cli = Cli::parse();
    if let Err(e) = run(cli.cmd) {
        eprintln!("collab: {}", e);
        std::process::exit(1);
    }
}

fn run(cmd: Cmd) -> anyhow::Result<()> {
    match cmd {
        Cmd::Init => {
            let project_root = scope::project_root()?;
            let in_tmux = std::env::var_os("TMUX_PANE").is_some();
            if !in_tmux {
                anyhow::bail!(
                    "collab init requires a live tmux pane; tmux is the only wake channel"
                );
            }
            if project_root.ancestors().skip(1).any(|ancestor| {
                ancestor
                    .file_name()
                    .is_some_and(|name| name == "playground")
            }) {
                anyhow::bail!(
                    "collab init must run from the project main tree, not a ./playground worktree"
                );
            }
            let _base = scope::init(&project_root)?;
            let scope = Scope { root: project_root };
            let started = !client::alive(&scope.sock_path());
            client::ensure_server(&scope.sock_path())?;
            let ident = me(&scope, None)?;
            let task_board: serde_json::Value =
                client::call(&scope.sock_path(), &Req::TaskStatus { task_id: None })?;
            out(&json!({
                "ok": true,
                "root": scope.root,
                "worker_id": ident.worker_id,
                "identity_kind": "peer",
                "daemon_started": started,
                "task_board": task_board["tasks"],
                "recovery_action": "inspect your own tasks, conflicts, and inbox through collab context"
            }));
            Ok(())
        }
        Cmd::Serve => {
            let scope = Scope::resolve()?;
            let rt = tokio::runtime::Runtime::new()?;
            rt.block_on(server::run(scope))
        }
        Cmd::Up => {
            let project_root = scope::project_root()?;
            if !project_root.join(".agent-collab").is_dir() {
                scope::init(&project_root)?;
            }
            let scope = Scope { root: project_root };
            std::fs::create_dir_all(scope.server_dir())?;
            std::fs::remove_file(scope.server_dir().join("DOWN")).ok();
            client::record_event(
                &scope.sock_path(),
                "daemon_up_requested",
                json!({"pid": std::process::id()}),
            );
            let sock = scope.sock_path();
            let was_running = client::alive(&sock);
            client::ensure_server(&sock)?;
            out(&json!({"ok": true, "server": sock, "started": !was_running}));
            Ok(())
        }
        Cmd::Down => {
            let scope = Scope::resolve()?;
            if client::alive(&scope.sock_path()) {
                let _: serde_json::Value =
                    client::call(&scope.sock_path(), &Req::Shutdown { operator: true })?;
            }
            let server_dir = scope.server_dir();
            std::fs::create_dir_all(&server_dir)?;
            std::fs::write(server_dir.join("DOWN"), b"explicitly stopped\n")?;
            client::record_event(
                &scope.sock_path(),
                "daemon_down_requested",
                json!({"pid": std::process::id()}),
            );
            let pid_path = server_dir.join("server.pid");
            if client::alive(&scope.sock_path()) {
                let mut pids = Vec::new();
                let output = std::process::Command::new("lsof")
                    .args(["-t", scope.sock_path().to_str().unwrap_or_default()])
                    .output()?;
                for line in String::from_utf8_lossy(&output.stdout).lines() {
                    if let Ok(pid) = line.trim().parse::<i32>() {
                        pids.push(pid);
                    }
                }
                if pids.is_empty() {
                    if let Ok(pid_text) = std::fs::read_to_string(&pid_path) {
                        if let Ok(pid) = pid_text.trim().parse::<i32>() {
                            pids.push(pid);
                        }
                    }
                }
                pids.sort_unstable();
                pids.dedup();
                for pid in pids {
                    if pid > 1 && pid != std::process::id() as i32 {
                        let status = std::process::Command::new("kill")
                            .args(["-TERM", &pid.to_string()])
                            .status()?;
                        if !status.success() {
                            anyhow::bail!("failed to stop collab daemon pid {}", pid);
                        }
                    }
                }
                for _ in 0..40 {
                    if !client::alive(&scope.sock_path()) {
                        break;
                    }
                    std::thread::sleep(std::time::Duration::from_millis(50));
                }
                if client::alive(&scope.sock_path()) {
                    anyhow::bail!(
                        "collab daemon did not stop at {}",
                        scope.sock_path().display()
                    );
                }
            }
            out(&json!({"ok": true, "down": true, "server": scope.sock_path()}));
            Ok(())
        }
        Cmd::Status => {
            let scope = Scope::resolve()?;
            let v: serde_json::Value = client::call(&scope.sock_path(), &Req::Ping)?;
            out(&v);
            Ok(())
        }
        Cmd::Role => {
            anyhow::bail!("collab role is deprecated; declared roles were removed")
        }
        Cmd::Who => {
            let scope = Scope::resolve()?;
            let v: serde_json::Value = client::call(&scope.sock_path(), &Req::Workers)?;
            out(&v);
            Ok(())
        }
        Cmd::Master { cmd } => {
            let _ = cmd;
            anyhow::bail!("collab master is deprecated; all registered identities are peers")
        }
        Cmd::Worker {
            cmd: WorkerCmd::Recover,
        } => {
            let scope = Scope::resolve()?;
            let ident = me(&scope, None)?;
            out(&json!({
                "recovered": true,
                "worker_id": ident.worker_id,
                "pane": ident.pane,
                "session": ident.session,
                "identity_kind": "peer",
                "next": "run collab who and collab task status; task ownership is unchanged"
            }));
            Ok(())
        }
        Cmd::TransferMaster { target } => {
            let _ = target;
            anyhow::bail!("collab transfer-master is deprecated; peer authority is task-scoped")
        }
        Cmd::RemoveWorker { target, force } => {
            let _ = (target, force);
            anyhow::bail!(
                "collab remove-worker is deprecated; use owner cleanup and migration verify"
            )
        }
        Cmd::Reset { force } => {
            let _ = force;
            anyhow::bail!(
                "collab reset is deprecated; preserve journal/mailbox and use migration rebind"
            )
        }
        Cmd::Whoami { worker, pane } => {
            let scope = Scope::resolve()?;
            let ident = identity::load_or_create(&scope, worker, pane)?;
            register(&scope, &ident)?;
            out(&ident);
            Ok(())
        }
        Cmd::Send {
            to,
            r#type,
            in_reply_to,
            delivery,
            body,
        } => {
            let scope = Scope::resolve()?;
            let ident = me(&scope, None)?;
            let body = body.join(" ");
            if body.is_empty() {
                anyhow::bail!("empty message body");
            }
            let v: serde_json::Value = client::call(
                &scope.sock_path(),
                &Req::Send {
                    from: ident.worker_id,
                    to,
                    mtype: r#type,
                    body,
                    in_reply_to,
                    delivery,
                },
            )?;
            out(&v);
            Ok(())
        }
        Cmd::Notify { cmd } => {
            let scope = Scope::resolve()?;
            let request = match cmd {
                NotifyCmd::Methods => Req::NotificationMethods,
                NotifyCmd::Subscribe {
                    event,
                    subject,
                    trigger_ms,
                    ttl_seconds,
                } => {
                    let ident = me(&scope, None)?;
                    Req::NotificationSubscribe {
                        worker_id: ident.worker_id,
                        token: ident.token,
                        event,
                        subject,
                        trigger_ms,
                        ttl_seconds,
                    }
                }
                NotifyCmd::Status => {
                    let ident = me(&scope, None)?;
                    Req::NotificationStatus {
                        worker_id: ident.worker_id,
                        token: ident.token,
                    }
                }
                NotifyCmd::Unsubscribe { subscription_id } => {
                    let ident = me(&scope, None)?;
                    Req::NotificationUnsubscribe {
                        worker_id: ident.worker_id,
                        token: ident.token,
                        subscription_id,
                    }
                }
            };
            let value: serde_json::Value = client::call(&scope.sock_path(), &request)?;
            out(&value);
            Ok(())
        }
        Cmd::Recv { timeout, worker } => {
            let scope = Scope::resolve()?;
            let ident = me(&scope, worker)?;
            let v: serde_json::Value = client::call(
                &scope.sock_path(),
                &Req::Poll {
                    worker_id: ident.worker_id,
                    token: ident.token,
                    timeout_ms: timeout.saturating_mul(1000),
                },
            )?;
            out(&v);
            Ok(())
        }
        Cmd::Inbox { worker } => {
            let scope = Scope::resolve()?;
            let ident = me(&scope, worker)?;
            let v: serde_json::Value = client::call(
                &scope.sock_path(),
                &Req::Inbox {
                    worker_id: ident.worker_id,
                    token: ident.token,
                },
            )?;
            out(&v);
            Ok(())
        }
        Cmd::Context { worker } => {
            let scope = Scope::resolve()?;
            let ident = me(&scope, worker)?;
            let v: serde_json::Value = client::call(
                &scope.sock_path(),
                &Req::Context {
                    worker_id: ident.worker_id,
                    token: ident.token,
                },
            )?;
            out(&v);
            Ok(())
        }
        Cmd::Ack { ids, worker } => {
            if ids.is_empty() {
                anyhow::bail!("usage: collab ack <msg_id>... [--worker <id>]");
            }
            let scope = Scope::resolve()?;
            let ident = me(&scope, worker)?;
            let v: serde_json::Value = client::call(
                &scope.sock_path(),
                &Req::Ack {
                    worker_id: ident.worker_id,
                    token: ident.token,
                    ids,
                },
            )?;
            out(&v);
            Ok(())
        }
        Cmd::Msg { msg_id } => {
            let scope = Scope::resolve()?;
            let v: serde_json::Value =
                client::call(&scope.sock_path(), &Req::MsgStatus { msg_id })?;
            out(&v);
            Ok(())
        }
        Cmd::Task { cmd } => {
            let scope = Scope::resolve()?;
            let ident = me(&scope, None)?;
            let req = match cmd {
                TaskCmd::Register {
                    id,
                    owner,
                    feature,
                    worktree,
                    branch,
                    base_commit,
                    priority,
                    next,
                    goal,
                } => Req::TaskRegister {
                    worker_id: ident.worker_id,
                    token: ident.token,
                    task_id: id,
                    owner,
                    feature_id: feature,
                    worktree_path: worktree,
                    branch,
                    base_commit,
                    priority: priority.unwrap_or_else(crate::server::state::default_priority),
                    next_step: next,
                    goal_prompt: goal,
                },
                TaskCmd::Update { id, status, next } => Req::TaskUpdate {
                    worker_id: ident.worker_id,
                    token: ident.token,
                    task_id: id,
                    status,
                    next_step: next,
                },
                TaskCmd::Relocate {
                    id,
                    worktree,
                    branch,
                    base_commit,
                } => Req::TaskRelocate {
                    worker_id: ident.worker_id,
                    token: ident.token,
                    task_id: id,
                    worktree_path: worktree,
                    branch,
                    base_commit,
                },
                TaskCmd::Claim { id } => Req::TaskClaim {
                    worker_id: ident.worker_id,
                    token: ident.token,
                    task_id: id,
                },
                TaskCmd::Wait { id, blocking_task } => Req::TaskWait {
                    worker_id: ident.worker_id,
                    token: ident.token,
                    task_id: id,
                    blocking_task_id: blocking_task,
                },
                TaskCmd::Deliver {
                    id,
                    evidence,
                    worktree,
                } => Req::TaskDeliver {
                    worker_id: ident.worker_id,
                    token: ident.token,
                    task_id: id,
                    evidence: Some(evidence),
                    worktree: Some(worktree),
                },
                TaskCmd::Block { id, next } => Req::TaskUpdate {
                    worker_id: ident.worker_id,
                    token: ident.token,
                    task_id: id,
                    status: Some("blocked".into()),
                    next_step: next,
                },
                TaskCmd::Close { id } => Req::TaskClose {
                    worker_id: ident.worker_id,
                    token: ident.token,
                    task_id: id,
                },
                TaskCmd::Dispatch => Req::TaskDispatch {
                    worker_id: ident.worker_id,
                    token: ident.token,
                },
                TaskCmd::Status { id } => Req::TaskStatus { task_id: id },
            };
            let v: serde_json::Value = client::call(&scope.sock_path(), &req)?;
            out(&v);
            Ok(())
        }
        Cmd::Migrate { cmd } => {
            let scope = Scope::resolve()?;
            let ident = me(&scope, None)?;
            let req = match cmd {
                MigrateCmd::Inspect => Req::MigrationInspect {
                    worker_id: ident.worker_id,
                    token: ident.token,
                },
                MigrateCmd::Plan => Req::MigrationPlan {
                    worker_id: ident.worker_id,
                    token: ident.token,
                },
                MigrateCmd::Apply => Req::MigrationApply {
                    worker_id: ident.worker_id,
                    token: ident.token,
                },
                MigrateCmd::Verify => Req::MigrationVerify {
                    worker_id: ident.worker_id,
                    token: ident.token,
                },
            };
            let v: serde_json::Value = client::call(&scope.sock_path(), &req)?;
            out(&v);
            Ok(())
        }
    }
}

// keep Resp referenced so the type stays part of the public surface for tests
#[allow(dead_code)]
fn _unused(_r: Resp) {}
