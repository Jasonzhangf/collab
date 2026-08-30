mod client;
mod config;
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
    /// Show your current role (master if first registered, otherwise worker)
    Role,
    /// List all registered workers with roles; shows who is master
    Who,
    /// Show the current master worker id, or recover it from this tmux session
    Master {
        #[command(subcommand)]
        cmd: Option<MasterCmd>,
    },
    /// Refresh this worker's tmux pane/session registration
    Worker {
        #[command(subcommand)]
        cmd: WorkerCmd,
    },
    /// Transfer master role to another registered worker
    TransferMaster { target: String },
    /// Remove a stale registered worker (master only); --force requeues its active tasks
    RemoveWorker {
        target: String,
        #[arg(long)]
        force: bool,
    },
    /// Clear all runtime worker/master bindings and requeue active tasks
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
    Send {
        #[arg(long)]
        to: String,
        #[arg(long, default_value = "notify")]
        r#type: String,
        #[arg(long)]
        in_reply_to: Option<String>,
        #[arg(long, default_value = "immediate", value_parser = ["immediate", "idle"])]
        delivery: String,
        #[arg(trailing_var_arg = true)]
        body: Vec<String>,
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
    /// Return one authoritative snapshot for continuation after a wake/restart
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
    /// Query message status (nudges, answered)
    Msg { msg_id: String },
    /// Task registration and lifecycle (task owner owns feature/worktree)
    Task {
        #[command(subcommand)]
        cmd: TaskCmd,
    },
    /// Show or update project-local .agent-collab/collab.json
    Config {
        /// New heartbeat interval in minutes
        #[arg(long)]
        heartbeat_minutes: Option<i64>,
    },
}

#[derive(Subcommand)]
enum TaskCmd {
    /// Register a task; the caller becomes owner unless master passes --owner
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
        /// Dispatch priority: p0 (highest) through p4
        #[arg(long)]
        priority: Option<String>,
        /// Next step hint for the assigned worker
        #[arg(long)]
        next: Option<String>,
        /// Complete /goal prompt; must begin with /goal and contains no wrapper text
        #[arg(long)]
        goal: Option<String>,
    },
    /// Relocate an existing task to a short playground worktree (master only)
    Relocate {
        id: String,
        #[arg(long)]
        worktree: String,
        #[arg(long)]
        branch: Option<String>,
        #[arg(long)]
        base_commit: Option<String>,
    },
    /// Update task status/next step by the task owner or master
    Update {
        id: String,
        #[arg(long)]
        status: Option<String>,
        #[arg(long)]
        next: Option<String>,
    },
    /// Claim an available task as the calling agent (transfers ownership)
    Claim { id: String },
    /// Put an owned task into resource-waiting state until another task releases
    Wait {
        id: String,
        #[arg(long = "for")]
        blocking_task: String,
    },
    /// Complete a claim atomically and notify master; close releases the claim
    Deliver {
        id: String,
        #[arg(long)]
        evidence: String,
        #[arg(long)]
        worktree: String,
    },
    /// Mark a claim blocked and notify master through the Server
    Block {
        id: String,
        #[arg(long)]
        next: Option<String>,
    },
    /// Close a merged task and clean up its declared worktree/branch
    Close { id: String },
    /// Dispatch available tasks to idle workers in priority order (master only)
    Dispatch,
    /// Show task registry
    Status { id: Option<String> },
}

#[derive(Subcommand)]
enum MasterCmd {
    /// Transfer master to this pane after the previous master endpoint is stale
    Recover,
}

#[derive(Subcommand)]
enum WorkerCmd {
    /// Re-register the current tmux pane without changing task ownership
    Recover,
}

fn out<T: serde::Serialize>(v: &T) {
    println!("{}", serde_json::to_string_pretty(v).unwrap());
}

/// Register an identity with the server (idempotent for the same token).
fn register(scope: &Scope, ident: &Identity) -> anyhow::Result<()> {
    let cwd = std::env::current_dir()?.display().to_string();
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
            let cwd = std::env::current_dir()?;
            let in_tmux = std::env::var_os("TMUX_PANE").is_some();
            if !in_tmux {
                anyhow::bail!("collab init requires a live tmux pane (Herdr runtime is disabled)");
            }
            if cwd.ancestors().skip(1).any(|ancestor| {
                ancestor
                    .file_name()
                    .is_some_and(|name| name == "playground")
            }) {
                anyhow::bail!(
                    "collab init must run from the project main tree, not a ./playground worktree"
                );
            }
            let project_root = scope::find_root(&cwd).unwrap_or_else(|| cwd.clone());
            let _base = scope::init(&project_root)?;
            let scope = Scope::resolve()?;
            let started = !client::alive(&scope.sock_path());
            client::ensure_server(&scope.sock_path())?;
            let ident = me(&scope, None)?;
            let role_resp: serde_json::Value = client::call(
                &scope.sock_path(),
                &Req::Role {
                    worker_id: ident.worker_id.clone(),
                },
            )?;
            let task_board: serde_json::Value =
                client::call(&scope.sock_path(), &Req::TaskStatus { task_id: None })?;
            out(&json!({
                "ok": true,
                "root": cwd,
                "worker_id": ident.worker_id,
                "role": role_resp["role"],
                "daemon_started": started,
                "task_board": task_board["tasks"],
                "recovery_action": "inspect task_board and collab inbox; master reviews delivered tasks, workers claim available tasks"
            }));
            Ok(())
        }
        Cmd::Serve => {
            let scope = Scope::resolve()?;
            let rt = tokio::runtime::Runtime::new()?;
            rt.block_on(server::run(scope))
        }
        Cmd::Up => {
            let cwd = std::env::current_dir()?;
            if scope::find_root(&cwd).is_none() {
                scope::init(&cwd)?;
            }
            let scope = Scope::resolve()?;
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
            if std::env::var_os("TMUX_PANE").is_some() {
                if !client::alive(&scope.sock_path()) {
                    anyhow::bail!(
                        "collab down from a tmux pane requires a live daemon and authenticated master"
                    );
                }
                let ident = me(&scope, None)?;
                let _: serde_json::Value = client::call(
                    &scope.sock_path(),
                    &Req::Shutdown {
                        worker_id: ident.worker_id,
                    },
                )?;
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
            let scope = Scope::resolve()?;
            let ident = me(&scope, None)?;
            let v: serde_json::Value = client::call(
                &scope.sock_path(),
                &Req::Role {
                    worker_id: ident.worker_id,
                },
            )?;
            out(&v);
            Ok(())
        }
        Cmd::Who => {
            let scope = Scope::resolve()?;
            let v: serde_json::Value = client::call(&scope.sock_path(), &Req::Workers)?;
            out(&v);
            Ok(())
        }
        Cmd::Master { cmd } => {
            let scope = Scope::resolve()?;
            let v: serde_json::Value = match cmd {
                None => client::call(&scope.sock_path(), &Req::MasterId)?,
                Some(MasterCmd::Recover) => {
                    let ident = me(&scope, None)?;
                    let pane = ident
                        .pane
                        .as_deref()
                        .filter(|p| p.starts_with('%'))
                        .ok_or_else(|| {
                            anyhow::anyhow!("collab master recover requires a tmux pane")
                        })?;
                    let session = std::process::Command::new("tmux")
                        .args(["display-message", "-p", "-t", pane, "#S"])
                        .output()?;
                    if !session.status.success() {
                        anyhow::bail!("cannot resolve tmux session for pane {}", pane);
                    }
                    let session = String::from_utf8_lossy(&session.stdout).trim().to_string();
                    if session.is_empty() {
                        anyhow::bail!("tmux returned an empty session name for pane {}", pane);
                    }
                    client::call(
                        &scope.sock_path(),
                        &Req::MasterRecover {
                            worker_id: ident.worker_id,
                            token: ident.token,
                            session,
                        },
                    )?
                }
            };
            out(&v);
            Ok(())
        }
        Cmd::Worker {
            cmd: WorkerCmd::Recover,
        } => {
            let scope = Scope::resolve()?;
            let ident = me(&scope, None)?;
            let v: serde_json::Value = client::call(
                &scope.sock_path(),
                &Req::Role {
                    worker_id: ident.worker_id.clone(),
                },
            )?;
            out(&json!({
                "recovered": true,
                "worker_id": ident.worker_id,
                "pane": ident.pane,
                "session": ident.session,
                "role": v["role"],
                "next": "run collab who and collab task status; task ownership is unchanged"
            }));
            Ok(())
        }
        Cmd::TransferMaster { target } => {
            let scope = Scope::resolve()?;
            let ident = me(&scope, None)?;
            let v: serde_json::Value = client::call(
                &scope.sock_path(),
                &Req::TransferMaster {
                    worker_id: ident.worker_id,
                    token: ident.token,
                    target_id: target,
                },
            )?;
            out(&v);
            Ok(())
        }
        Cmd::RemoveWorker { target, force } => {
            let scope = Scope::resolve()?;
            let ident = me(&scope, None)?;
            let v: serde_json::Value = client::call(
                &scope.sock_path(),
                &Req::RemoveWorker {
                    worker_id: ident.worker_id,
                    token: ident.token,
                    target_id: target,
                    force,
                },
            )?;
            out(&v);
            Ok(())
        }
        Cmd::Reset { force } => {
            if !force {
                anyhow::bail!("collab reset requires --force (clears worker/master bindings)");
            }
            let scope = Scope::resolve()?;
            let v: serde_json::Value =
                client::call(&scope.sock_path(), &Req::ResetBindings { confirm: true })?;
            out(&v);
            Ok(())
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
        Cmd::Config { heartbeat_minutes } => {
            let scope = Scope::resolve()?;
            let mut config = config::load(&scope.root)?;
            if let Some(minutes) = heartbeat_minutes {
                config.heartbeat_minutes = minutes;
                config::save(&scope.root, &config)?;
            }
            out(&json!({
                "path": scope.root.join(".agent-collab").join("collab.json"),
                "config": config,
            }));
            Ok(())
        }
    }
}

// keep Resp referenced so the type stays part of the public surface for tests
#[allow(dead_code)]
fn _unused(_r: Resp) {}
