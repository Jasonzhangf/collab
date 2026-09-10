mod client;
mod config;
mod identity;
mod install_skills;
pub(crate) mod migration;
mod proto;
mod scope;
mod server;
mod subagent;

use clap::{Parser, Subcommand};
use identity::{AppServerId, CommandId, Identity, OperationId, RuntimeIdentity};
use proto::{ProjectContext, Req, Resp};
use scope::Scope;
use serde::de::DeserializeOwned;
use serde_json::json;
use std::time::{SystemTime, UNIX_EPOCH};

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
    #[command(hide = true)]
    SubagentExec { file: std::path::PathBuf },
    /// Managed persistent agent peers (current project only)
    Subagent {
        #[command(subcommand)]
        command: subagent::Action,
    },
    /// Show the effective policy from ~/.appsdk/config.toml
    Config,
    /// Create .agent-collab skeleton in the current directory
    Init,
    /// Hidden: daemon entrypoint (spawned by `up`)
    #[command(hide = true)]
    Serve,
    /// Start the coordination daemon (idempotent)
    Up,
    /// Explicitly stop the daemon and disable automatic restart
    Down,
    /// Show server summary (pass --all to aggregate all workers, tasks, subagents)
    Status {
        #[arg(long)]
        all: bool,
    },
    /// Inspect or read messages from durable mailbox
    Mailbox {
        #[command(subcommand)]
        cmd: MailboxCmd,
    },
    /// Deprecated: declared roles were removed
    Role,
    /// List registered peers and their local activity projection
    Who,
    /// Inspect or explicitly assign collab master authority
    Master {
        #[command(subcommand)]
        command: MasterCmd,
    },
    /// Hidden alias: previous collab root commands are collab master
    #[command(hide = true)]
    Root {
        #[command(subcommand)]
        command: MasterCmd,
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
        /// Sender identity; defaults to current collab identity, COLLAB_WORKER, or 'operator'
        #[arg(long)]
        from: Option<String>,
        #[arg(long)]
        to: String,
        /// Short topic shown in the tmux notification preview
        #[arg(long)]
        subject: String,
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
        /// Acknowledge all pending and delivered messages in inbox
        #[arg(long)]
        all: bool,
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
    /// Install the embedded collab skill bundle into a global skills
    /// directory. Default target is `~/.agents/skills/collab`; pass
    /// `--target` to override. Existing files are skipped unless
    /// `--force` is given.
    InstallSkills {
        /// Destination directory for the collab skill bundle.
        /// Defaults to `~/.agents/skills/collab`.
        #[arg(long)]
        target: Option<std::path::PathBuf>,
        /// Overwrite existing files in the target instead of skipping them.
        #[arg(long)]
        force: bool,
    },
}

#[derive(Subcommand)]
enum NotifyCmd {
    /// List supported notification methods and events
    Methods,
    /// Register one finite notification subscription (direct-message is reusable)
    Subscribe {
        #[arg(long)]
        event: String,
        #[arg(long)]
        subject: Option<String>,
        /// Absolute UTC epoch milliseconds; repeat to define multiple fire times
        #[arg(long = "at-ms")]
        at_ms: Vec<i64>,
        /// Period in milliseconds; mutually exclusive with --at-ms
        #[arg(long = "every-ms")]
        every_ms: Option<i64>,
        /// Total number of notifications for a periodic subscription (1..=100)
        #[arg(long, default_value_t = 1)]
        repeat_count: u32,
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
    /// Accept an assigned task and atomically begin owner execution
    Accept { id: String },
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
    /// Accept a delivered task or return it for rework
    Review {
        id: String,
        #[arg(long, conflicts_with = "rework", required_unless_present = "rework")]
        accept: bool,
        #[arg(long, conflicts_with = "accept", required_unless_present = "accept")]
        rework: bool,
        #[arg(long)]
        evidence: String,
    },
    /// Record exact integration of an accepted task on main
    Integrated {
        id: String,
        #[arg(long)]
        commit: String,
        #[arg(long)]
        evidence: String,
    },
    /// Mark the caller's task blocked without notifying unrelated peers
    Block {
        id: String,
        #[arg(long)]
        next: Option<String>,
    },
    /// Close a merged task and clean up its declared worktree/branch.
    /// With --force the live master may close any task. With no live master,
    /// the owner may close its task, or a registered peer may close an
    /// orphaned task after the owner's tmux identity is lost. Force close
    /// stops keepalives without deleting the worktree or branch and requires
    /// a non-empty --reason.
    Close {
        id: String,
        #[arg(long)]
        force: bool,
        #[arg(long = "reason")]
        reason: Option<String>,
    },
    /// Deprecated: peers self-register tasks; no central dispatch
    Dispatch,
    /// Show task registry
    Status { id: Option<String> },
}

#[derive(Subcommand, Debug, Clone)]
pub enum MailboxCmd {
    /// Read messages in chronological order
    Read {
        /// Include all messages across the project mailbox
        #[arg(long)]
        all: bool,
        /// Sorting order: time-asc (default) or time-desc
        #[arg(long, default_value = "time-asc")]
        sort: String,
        /// Filter messages by specific worker ID
        #[arg(long)]
        worker: Option<String>,
    },
}

#[derive(Subcommand)]
enum MasterCmd {
    /// Promote this peer when no live master exists; requires the user's approval text
    Promote {
        #[arg(long)]
        approval: String,
    },
    /// Delegate master authority to another registered peer (live master only)
    Delegate { target: String },
    /// Send a durable message to the master of another explicit project
    Send {
        #[arg(long)]
        project: std::path::PathBuf,
        #[arg(long)]
        to: String,
        #[arg(long)]
        subject: String,
        #[arg(trailing_var_arg = true)]
        body: Vec<String>,
    },
    /// Show the current live master, if any
    Status,
    /// Deprecated: permanent master recovery was removed
    Recover,
}

#[derive(Subcommand)]
enum WorkerCmd {
    /// Re-register the current tmux pane without changing task ownership
    Recover,
    /// Inspect worker status (liveness, identity, agent state, unacked notifications)
    Status {
        /// Optional worker ID to inspect (defaults to all registered workers)
        id: Option<String>,
    },
    /// Live master retires a worker registration; optionally kills its tmux session
    Close {
        /// Worker ID to close
        id: String,
        /// Why this worker is being closed; recorded for audit
        #[arg(long)]
        reason: String,
        /// Also kill the worker's tmux session
        #[arg(long)]
        kill_session: bool,
    },
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
fn register(scope: &Scope, ident: &mut Identity) -> anyhow::Result<serde_json::Value> {
    let context_runtime = match ident.runtime.as_ref() {
        Some(runtime) => {
            runtime.validate()?;
            runtime.clone()
        }
        None => RuntimeIdentity::cli_adapter(&ident.worker_id)?,
    };
    if context_runtime.appserver_id.as_str() != identity::CLI_APP_SERVER_ID {
        anyhow::bail!(
            "CLI identity app scope must be {}; observed {}",
            identity::CLI_APP_SERVER_ID,
            context_runtime.appserver_id
        );
    }
    let cwd = scope.root.display().to_string();
    let response: serde_json::Value = client::call_with_runtime_identity_at_root(
        &scope.sock_path(),
        &Req::Register {
            worker_id: ident.worker_id.clone(),
            token: ident.token.clone(),
            pane: ident.pane.clone(),
            cwd,
        },
        &scope.root,
        &context_runtime,
    )?;
    let runtime =
        identity::runtime_from_registration_receipt(&response, &ident.worker_id, &scope.root)?;
    identity::persist_runtime(scope, ident, runtime)?;
    Ok(response)
}

/// Identity bootstrap used by every command that acts as a worker.
fn me(scope: &Scope, worker: Option<String>) -> anyhow::Result<Identity> {
    let worker = worker.or_else(|| std::env::var("COLLAB_WORKER").ok());
    let mut ident = identity::load_or_create(scope, worker, None)?;
    if ident.runtime.is_none() {
        let _ = register(scope, &mut ident)?;
    } else {
        runtime_for_request(&ident)?;
    }
    Ok(ident)
}

fn ensure_registration(scope: &Scope, ident: &mut Identity) -> anyhow::Result<serde_json::Value> {
    if ident.runtime.is_none() {
        register(scope, ident)
    } else {
        runtime_for_request(ident)?;
        Ok(json!({"reused": true}))
    }
}

fn runtime_for_request<'a>(ident: &'a Identity) -> anyhow::Result<&'a RuntimeIdentity> {
    let runtime = ident.runtime.as_ref().ok_or_else(|| {
        anyhow::anyhow!(
            "identity has no registered runtime binding; register the current peer before making a project request"
        )
    })?;
    runtime.validate()?;
    if runtime.agent_id.as_str() != ident.worker_id {
        anyhow::bail!(
            "runtime binding agent does not match identity worker: expected {}, observed {}",
            ident.worker_id,
            runtime.agent_id
        );
    }
    if runtime.appserver_id.as_str() != identity::CLI_APP_SERVER_ID {
        anyhow::bail!(
            "CLI identity app scope must be {}; observed {}",
            identity::CLI_APP_SERVER_ID,
            runtime.appserver_id
        );
    }
    Ok(runtime)
}

fn call_project<T: DeserializeOwned>(
    scope: &Scope,
    ident: &Identity,
    request: &Req,
) -> anyhow::Result<T> {
    let runtime = runtime_for_request(ident)?;
    client::call_with_runtime_identity_at_root(&scope.sock_path(), request, &scope.root, runtime)
}

fn cli_project_context(root: &std::path::Path) -> anyhow::Result<ProjectContext> {
    ProjectContext::for_registered_root_with_app(
        root,
        AppServerId::new(identity::CLI_APP_SERVER_ID)?,
    )
}

fn command_envelope(scope: &Scope, ident: &Identity) -> anyhow::Result<proto::CommandEnvelope> {
    let runtime = runtime_for_request(ident)?;
    let route = scope.route_scope(runtime.appserver_id.clone())?;
    let nonce = SystemTime::now().duration_since(UNIX_EPOCH)?.as_nanos();
    Ok(proto::CommandEnvelope::new(
        CommandId::new(format!("command-{}-{nonce}", std::process::id()))?,
        OperationId::new(format!("operation-{}-{nonce}", std::process::id()))?,
        runtime.binding_id.clone(),
        runtime.endpoint_generation,
        route,
        None,
        None,
        None,
        None,
    ))
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
        Cmd::SubagentExec { file } => subagent::exec_launch(&file),
        Cmd::Config => {
            crate::config::ensure_written()?;
            out(&crate::config::load(&scope::project_root()?)?);
            Ok(())
        }
        Cmd::Subagent { command } => {
            let scope = Scope::resolve()?;
            if std::env::var_os("TMUX_PANE").is_none() {
                let query = match &command {
                    subagent::Action::List => Some((None, None)),
                    subagent::Action::Status { id } => Some((Some(id.clone()), None)),
                    subagent::Action::Snapshot { id, lines } => {
                        Some((Some(id.clone()), Some(*lines)))
                    }
                    _ => None,
                };
                if let Some((id, snapshot_lines)) = query {
                    let value: serde_json::Value = client::call_with_context(
                        &scope.sock_path(),
                        &Req::SubagentObserve { id, snapshot_lines },
                        Some(cli_project_context(&scope.root)?),
                    )?;
                    out(&value);
                    return Ok(());
                }
            }
            let ident = me(&scope, None)?;
            let launch_env = if matches!(command, subagent::Action::Start { .. }) {
                std::env::vars().collect()
            } else {
                Default::default()
            };
            let value: serde_json::Value = call_project(
                &scope,
                &ident,
                &Req::Subagent {
                    worker_id: ident.worker_id.clone(),
                    token: ident.token.clone(),
                    command,
                    launch_env,
                },
            )?;
            out(&value);
            Ok(())
        }
        Cmd::Init => {
            let project_root = scope::project_root()?;
            let in_tmux = std::env::var_os("TMUX_PANE").is_some();
            if !in_tmux {
                anyhow::bail!(
                    "NOTIFICATION_CHANNEL_NONE: collab init requires a live tmux pane for peer registration; no push notifications are available here. Independent work can continue. Check subagent status (includes parent mailbox) yourself; use subagent snapshot explicitly for screen diagnostics. No subscription was created."
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
            let mut ident = identity::load_or_create(&scope, None, None)?;
            let registration = ensure_registration(&scope, &mut ident)?;
            let task_board: serde_json::Value =
                call_project(&scope, &ident, &Req::TaskStatus { task_id: None })?;
            out(&json!({
                "ok": true,
                "root": scope.root,
                "worker_id": ident.worker_id,
                "identity_kind": "peer",
                "daemon_started": started,
                "role_brief": registration["role_brief"],
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
            let host_server_dir = scope.host_server_dir();
            std::fs::create_dir_all(&host_server_dir)?;
            std::fs::remove_file(host_server_dir.join("DOWN")).ok();
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
                let _: serde_json::Value = client::call_with_context(
                    &scope.sock_path(),
                    &Req::Shutdown { operator: true },
                    Some(cli_project_context(&scope.root)?),
                )?;
            }
            let server_dir = scope.host_server_dir();
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
        Cmd::Status { all } => {
            let scope = Scope::resolve()?;
            let v: serde_json::Value = if all {
                client::call_with_context(
                    &scope.sock_path(),
                    &Req::StatusAll,
                    Some(cli_project_context(&scope.root)?),
                )?
            } else {
                client::call(&scope.sock_path(), &Req::Ping)?
            };
            out(&v);
            Ok(())
        }
        Cmd::Mailbox { cmd } => {
            let scope = Scope::resolve()?;
            match cmd {
                MailboxCmd::Read { all, sort, worker } => {
                    let explicit_worker = worker.is_some();
                    let actorless = all || explicit_worker;
                    let mut identity = None;
                    let worker_id = if actorless {
                        worker
                    } else if std::env::var_os("TMUX_PANE").is_none() {
                        anyhow::bail!(
                            "collab mailbox read outside tmux requires --all or --worker <id>"
                        );
                    } else {
                        let ident = me(&scope, None)?;
                        let worker_id = ident.worker_id.clone();
                        identity = Some(ident);
                        Some(worker_id)
                    };
                    let request = Req::MailboxRead {
                        all,
                        sort: Some(sort),
                        worker_id,
                    };
                    let v: serde_json::Value = if actorless {
                        client::call_with_context(
                            &scope.sock_path(),
                            &request,
                            Some(cli_project_context(&scope.root)?),
                        )?
                    } else {
                        let ident = identity
                            .as_ref()
                            .ok_or_else(|| anyhow::anyhow!("mailbox identity was not resolved"))?;
                        call_project(&scope, ident, &request)?
                    };
                    out(&v);
                    Ok(())
                }
            }
        }
        Cmd::Role => {
            anyhow::bail!("collab role is deprecated; declared roles were removed")
        }
        Cmd::Who => {
            let scope = Scope::resolve()?;
            let v: serde_json::Value = client::call_with_context(
                &scope.sock_path(),
                &Req::Workers,
                Some(cli_project_context(&scope.root)?),
            )?;
            out(&v);
            Ok(())
        }
        Cmd::Root { command } | Cmd::Master { command } => {
            let scope = Scope::resolve()?;
            let ident = me(&scope, None)?;
            let req = match command {
                MasterCmd::Recover => {
                    anyhow::bail!(
                        "collab master recover is deprecated; use collab master promote or delegate"
                    )
                }
                MasterCmd::Promote { approval } => Req::MasterPromote {
                    worker_id: ident.worker_id.clone(),
                    token: ident.token.clone(),
                    approval,
                },
                MasterCmd::Delegate { target } => Req::MasterDelegate {
                    worker_id: ident.worker_id.clone(),
                    token: ident.token.clone(),
                    target_id: target,
                },
                MasterCmd::Send {
                    project,
                    to,
                    subject,
                    body,
                } => {
                    let local: serde_json::Value =
                        call_project(&scope, &ident, &Req::MasterStatus)?;
                    let Some(master) = local.get("master") else {
                        anyhow::bail!("cross-project send requires this peer to be a live master")
                    };
                    if master.get("worker_id").and_then(|v| v.as_str())
                        != Some(ident.worker_id.as_str())
                        || master.get("endpoint_live").and_then(|v| v.as_bool()) != Some(true)
                    {
                        anyhow::bail!("cross-project send requires this peer to be the live master")
                    }
                    let target = project.canonicalize()?;
                    if target == scope.root.canonicalize()? {
                        anyhow::bail!("cross-project send requires a different project")
                    }
                    if !target.join(".agent-collab").is_dir() {
                        anyhow::bail!("target project has no .agent-collab: {}", target.display())
                    }
                    let target_scope = Scope { root: target };
                    let value: serde_json::Value = client::call_with_context(
                        &target_scope.sock_path(),
                        &Req::CrossProjectSend {
                            from: ident.worker_id.clone(),
                            from_project: scope.root.display().to_string(),
                            source_master_assigned_by: master
                                .get("assigned_by")
                                .and_then(|v| v.as_str())
                                .unwrap_or_default()
                                .to_string(),
                            source_master_approval: master
                                .get("approval")
                                .and_then(|v| v.as_str())
                                .map(str::to_owned),
                            source_master_assigned_ms: master
                                .get("assigned_ms")
                                .and_then(|v| v.as_i64())
                                .unwrap_or_default(),
                            to,
                            subject,
                            body: body.join(" "),
                            in_reply_to: None,
                        },
                        Some(cli_project_context(&target_scope.root)?),
                    )?;
                    out(&value);
                    return Ok(());
                }
                MasterCmd::Status => Req::MasterStatus,
            };
            let v: serde_json::Value = call_project(&scope, &ident, &req)?;
            out(&v);
            Ok(())
        }
        Cmd::Worker { cmd } => {
            let scope = Scope::resolve()?;
            match cmd {
                WorkerCmd::Recover => {
                    let mut ident = identity::load_or_create(&scope, None, None)?;
                    let _ = register(&scope, &mut ident)?;
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
                WorkerCmd::Status { id } => {
                    let ident = me(&scope, None)?;
                    let v: serde_json::Value =
                        call_project(&scope, &ident, &Req::WorkerStatus { worker_id: id })?;
                    out(&v);
                    Ok(())
                }
                WorkerCmd::Close {
                    id,
                    reason,
                    kill_session,
                } => {
                    let ident = me(&scope, None)?;
                    let v: serde_json::Value = call_project(
                        &scope,
                        &ident,
                        &Req::WorkerClose {
                            worker_id: ident.worker_id.clone(),
                            token: ident.token.clone(),
                            target_id: id,
                            reason,
                            kill_session,
                        },
                    )?;
                    out(&v);
                    Ok(())
                }
            }
        }
        Cmd::TransferMaster { target } => {
            let _ = target;
            anyhow::bail!("collab transfer-master is deprecated; use collab master delegate")
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
            let mut ident = identity::load_or_create(&scope, worker, pane)?;
            let registration = ensure_registration(&scope, &mut ident)?;
            let mut response = serde_json::to_value(&ident)?;
            response["role_brief"] = registration["role_brief"].clone();
            out(&response);
            Ok(())
        }
        Cmd::Send {
            from,
            to,
            subject,
            r#type,
            in_reply_to,
            delivery,
            body,
        } => {
            let scope = Scope::resolve()?;
            let ident = me(&scope, None)?;
            if from
                .as_deref()
                .is_some_and(|requested| requested != ident.worker_id)
            {
                anyhow::bail!("--from must match the authenticated worker identity");
            }
            let body = body.join(" ");
            if body.is_empty() {
                anyhow::bail!("empty message body");
            }
            let command = command_envelope(&scope, &ident)?;
            let v: serde_json::Value = call_project(
                &scope,
                &ident,
                &Req::Send {
                    from: ident.worker_id.clone(),
                    worker_id: Some(ident.worker_id.clone()),
                    token: Some(ident.token.clone()),
                    command: Some(command),
                    to,
                    mtype: r#type,
                    subject: Some(subject),
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
            let ident = me(&scope, None)?;
            let request = match cmd {
                NotifyCmd::Methods => Req::NotificationMethods,
                NotifyCmd::Subscribe {
                    event,
                    subject,
                    at_ms,
                    every_ms,
                    repeat_count,
                    trigger_ms,
                    ttl_seconds,
                } => Req::NotificationSubscribe {
                    worker_id: ident.worker_id.clone(),
                    token: ident.token.clone(),
                    event,
                    subject,
                    trigger_ms,
                    trigger_times_ms: at_ms,
                    interval_ms: every_ms,
                    repeat_count,
                    ttl_seconds,
                },
                NotifyCmd::Status => Req::NotificationStatus {
                    worker_id: ident.worker_id.clone(),
                    token: ident.token.clone(),
                },
                NotifyCmd::Unsubscribe { subscription_id } => Req::NotificationUnsubscribe {
                    worker_id: ident.worker_id.clone(),
                    token: ident.token.clone(),
                    subscription_id,
                },
            };
            let value: serde_json::Value = call_project(&scope, &ident, &request)?;
            out(&value);
            Ok(())
        }
        Cmd::Recv { timeout, worker } => {
            let scope = Scope::resolve()?;
            let ident = me(&scope, worker)?;
            let v: serde_json::Value = call_project(
                &scope,
                &ident,
                &Req::Poll {
                    worker_id: ident.worker_id.clone(),
                    token: ident.token.clone(),
                    timeout_ms: timeout.saturating_mul(1000),
                },
            )?;
            out(&v);
            Ok(())
        }
        Cmd::Inbox { worker } => {
            let scope = Scope::resolve()?;
            let ident = me(&scope, worker)?;
            let v: serde_json::Value = call_project(
                &scope,
                &ident,
                &Req::Inbox {
                    worker_id: ident.worker_id.clone(),
                    token: ident.token.clone(),
                },
            )?;
            out(&v);
            Ok(())
        }
        Cmd::Context { worker } => {
            let scope = Scope::resolve()?;
            let ident = me(&scope, worker)?;
            let v: serde_json::Value = call_project(
                &scope,
                &ident,
                &Req::Context {
                    worker_id: ident.worker_id.clone(),
                    token: ident.token.clone(),
                },
            )?;
            out(&v);
            Ok(())
        }
        Cmd::Ack { ids, worker, all } => {
            if ids.is_empty() && !all {
                anyhow::bail!("usage: collab ack <msg_id>... [--all] [--worker <id>]");
            }
            let scope = Scope::resolve()?;
            let ident = me(&scope, worker)?;
            let v: serde_json::Value = call_project(
                &scope,
                &ident,
                &Req::Ack {
                    worker_id: ident.worker_id.clone(),
                    token: ident.token.clone(),
                    ids,
                },
            )?;
            out(&v);
            Ok(())
        }
        Cmd::Msg { msg_id } => {
            let scope = Scope::resolve()?;
            let ident = me(&scope, None)?;
            let v: serde_json::Value = call_project(&scope, &ident, &Req::MsgStatus { msg_id })?;
            out(&v);
            Ok(())
        }
        Cmd::Task { cmd } => {
            let scope = Scope::resolve()?;
            let ident = me(&scope, None)?;
            let worker_id = ident.worker_id.clone();
            let token = ident.token.clone();
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
                    worker_id: worker_id.clone(),
                    token: token.clone(),
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
                    worker_id: worker_id.clone(),
                    token: token.clone(),
                    task_id: id,
                    status,
                    next_step: next,
                },
                TaskCmd::Accept { id } => Req::TaskAccept {
                    worker_id: worker_id.clone(),
                    token: token.clone(),
                    task_id: id,
                },
                TaskCmd::Relocate {
                    id,
                    worktree,
                    branch,
                    base_commit,
                } => Req::TaskRelocate {
                    worker_id: worker_id.clone(),
                    token: token.clone(),
                    task_id: id,
                    worktree_path: worktree,
                    branch,
                    base_commit,
                },
                TaskCmd::Claim { id } => Req::TaskClaim {
                    worker_id: worker_id.clone(),
                    token: token.clone(),
                    task_id: id,
                },
                TaskCmd::Wait { id, blocking_task } => Req::TaskWait {
                    worker_id: worker_id.clone(),
                    token: token.clone(),
                    task_id: id,
                    blocking_task_id: blocking_task,
                },
                TaskCmd::Deliver {
                    id,
                    evidence,
                    worktree,
                } => Req::TaskDeliver {
                    worker_id: worker_id.clone(),
                    token: token.clone(),
                    task_id: id,
                    evidence: Some(evidence),
                    worktree: Some(worktree),
                },
                TaskCmd::Review {
                    id,
                    accept,
                    rework,
                    evidence,
                } => Req::TaskReview {
                    worker_id: worker_id.clone(),
                    token: token.clone(),
                    task_id: id,
                    accept,
                    rework,
                    evidence,
                },
                TaskCmd::Integrated {
                    id,
                    commit,
                    evidence,
                } => Req::TaskIntegrated {
                    worker_id: worker_id.clone(),
                    token: token.clone(),
                    task_id: id,
                    commit,
                    evidence,
                },
                TaskCmd::Block { id, next } => Req::TaskUpdate {
                    worker_id: worker_id.clone(),
                    token: token.clone(),
                    task_id: id,
                    status: Some("blocked".into()),
                    next_step: next,
                },
                TaskCmd::Close { id, force, reason } => Req::TaskClose {
                    worker_id: worker_id.clone(),
                    token: token.clone(),
                    task_id: id,
                    force,
                    reason,
                },
                TaskCmd::Dispatch => Req::TaskDispatch {
                    worker_id: worker_id.clone(),
                    token: token.clone(),
                },
                TaskCmd::Status { id } => Req::TaskStatus { task_id: id },
            };
            let v: serde_json::Value = call_project(&scope, &ident, &req)?;
            out(&v);
            Ok(())
        }
        Cmd::Migrate { cmd } => {
            let scope = Scope::resolve()?;
            let ident = me(&scope, None)?;
            let worker_id = ident.worker_id.clone();
            let token = ident.token.clone();
            let req = match cmd {
                MigrateCmd::Inspect => Req::MigrationInspect {
                    worker_id: worker_id.clone(),
                    token: token.clone(),
                },
                MigrateCmd::Plan => Req::MigrationPlan {
                    worker_id: worker_id.clone(),
                    token: token.clone(),
                },
                MigrateCmd::Apply => Req::MigrationApply {
                    worker_id: worker_id.clone(),
                    token: token.clone(),
                },
                MigrateCmd::Verify => Req::MigrationVerify {
                    worker_id: worker_id.clone(),
                    token: token.clone(),
                },
            };
            let v: serde_json::Value = call_project(&scope, &ident, &req)?;
            out(&v);
            Ok(())
        }
        Cmd::InstallSkills { target, force } => {
            let target = match target {
                Some(path) => path,
                None => match std::env::var_os("HOME") {
                    Some(home) => std::path::PathBuf::from(home)
                        .join(".agents")
                        .join("skills")
                        .join("collab"),
                    None => {
                        anyhow::bail!("install-skills default target requires $HOME; pass --target")
                    }
                },
            };
            let (outcomes, bytes, count) =
                install_skills::install(&target, force).map_err(|error| anyhow::anyhow!(error))?;
            let written = outcomes
                .iter()
                .filter(|(_, o)| *o == install_skills::InstallOutcome::Written)
                .count();
            let skipped = count - written;
            let files: Vec<&str> = outcomes.iter().map(|(r, _)| *r).collect();
            out(&serde_json::json!({
                "target": target,
                "files": files,
                "written": written,
                "skipped": skipped,
                "bytes": bytes,
                "force": force,
                "next": "the collab skill is now visible to any agent that loads ~/.agents/skills; restart the agent or rerun its skill discovery to pick up the bundle",
            }));
            Ok(())
        }
    }
}

// keep Resp referenced so the type stays part of the public surface for tests
#[allow(dead_code)]
fn _unused(_r: Resp) {}

#[cfg(test)]
mod tests {
    use super::*;

    fn test_root(name: &str) -> std::path::PathBuf {
        let root = std::env::temp_dir().join(format!(
            "collab-main-{name}-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(&root).unwrap();
        root
    }

    fn identity_with_runtime(runtime: Option<RuntimeIdentity>) -> Identity {
        Identity {
            worker_id: "worker-1".into(),
            token: "token-1".into(),
            pane: None,
            session: None,
            runtime,
        }
    }

    #[test]
    fn runtime_for_request_rejects_an_unregistered_identity() {
        let identity = identity_with_runtime(None);
        let error = runtime_for_request(&identity).unwrap_err();
        assert!(error
            .to_string()
            .contains("identity has no registered runtime binding"));
    }

    #[test]
    fn cli_project_context_uses_the_cli_app_and_exact_root() {
        let root = test_root("project-context");
        let context = cli_project_context(&root).unwrap();
        let canonical = root.canonicalize().unwrap();
        assert_eq!(context.app_scope_id.as_str(), identity::CLI_APP_SERVER_ID);
        assert_eq!(context.canonical_root, canonical.to_string_lossy());
        assert_eq!(context.project_scope.as_str(), canonical.to_string_lossy());
        std::fs::remove_dir_all(root).ok();
    }

    #[test]
    fn ensure_registration_reuses_a_valid_runtime_without_rebinding() {
        let root = test_root("registration-reuse");
        let runtime = RuntimeIdentity::cli_adapter("worker-1").unwrap();
        let mut identity = identity_with_runtime(Some(runtime));
        let before = serde_json::to_value(&identity).unwrap();
        let response = ensure_registration(&Scope { root: root.clone() }, &mut identity).unwrap();
        assert_eq!(response, json!({"reused": true}));
        assert_eq!(serde_json::to_value(&identity).unwrap(), before);
        std::fs::remove_dir_all(root).ok();
    }

    #[test]
    fn command_envelope_uses_the_registered_binding_and_generation() {
        let root = test_root("command-envelope");
        let runtime = RuntimeIdentity {
            agent_id: identity::AgentId::new("worker-1").unwrap(),
            runtime_id: identity::RuntimeId::new("runtime-live").unwrap(),
            appserver_id: identity::AppServerId::new(identity::CLI_APP_SERVER_ID).unwrap(),
            endpoint_generation: 9,
            binding_id: identity::BindingId::new("binding-live").unwrap(),
            native_thread_id: None,
        };
        let identity = identity_with_runtime(Some(runtime));
        let envelope = command_envelope(&Scope { root: root.clone() }, &identity).unwrap();
        assert_eq!(envelope.actor_binding_id.as_str(), "binding-live");
        assert_eq!(envelope.endpoint_generation, 9);
        assert_eq!(
            envelope.scope.app_scope_id.as_str(),
            identity::CLI_APP_SERVER_ID
        );
        assert_eq!(
            envelope.scope.project_scope_id.as_str(),
            root.canonicalize().unwrap().to_string_lossy()
        );
        std::fs::remove_dir_all(root).ok();
    }
}
