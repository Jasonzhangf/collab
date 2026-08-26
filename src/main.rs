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
    /// Show server summary
    Status,
    /// Show your current role (master if first registered, otherwise worker)
    Role,
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
    },
    /// Update task status/next step by the task owner or master
    Update {
        id: String,
        #[arg(long)]
        status: Option<String>,
        #[arg(long)]
        next: Option<String>,
    },
    /// Show task registry
    Status { id: Option<String> },
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
            if scope::find_root(&cwd).is_some() && cwd.join(".agent-collab").is_dir() {
                out(&json!({"ok": true, "root": cwd, "note": "already initialized"}));
                return Ok(());
            }
            let base = scope::init(&cwd)?;
            out(&json!({"ok": true, "created": base}));
            Ok(())
        }
        Cmd::Serve => {
            let scope = Scope::resolve()?;
            let rt = tokio::runtime::Runtime::new()?;
            rt.block_on(server::run(scope))
        }
        Cmd::Up => {
            let scope = Scope::resolve()?;
            let sock = scope.sock_path();
            if client::alive(&sock) {
                out(&json!({"ok": true, "server": sock, "note": "already running"}));
                return Ok(());
            }
            let exe = std::env::current_exe()?;
            use std::os::unix::process::CommandExt;
            let log = std::fs::OpenOptions::new()
                .create(true)
                .append(true)
                .open(scope.server_dir().join("log.txt"))?;
            let err = log.try_clone()?;
            std::process::Command::new(exe)
                .arg("serve")
                .stdin(std::process::Stdio::null())
                .stdout(log)
                .stderr(err)
                .process_group(0)
                .spawn()?;
            for _ in 0..40 {
                if client::alive(&sock) {
                    out(&json!({"ok": true, "server": sock, "started": true}));
                    return Ok(());
                }
                std::thread::sleep(std::time::Duration::from_millis(100));
            }
            anyhow::bail!("server did not come up within 4s; check .agent-collab/server/log.txt")
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
                } => Req::TaskRegister {
                    worker_id: ident.worker_id,
                    token: ident.token,
                    task_id: id,
                    owner,
                    feature_id: feature,
                    worktree_path: worktree,
                    branch,
                    base_commit,
                },
                TaskCmd::Update { id, status, next } => Req::TaskUpdate {
                    worker_id: ident.worker_id,
                    token: ident.token,
                    task_id: id,
                    status,
                    next_step: next,
                },
                TaskCmd::Status { id } => Req::TaskStatus { task_id: id },
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
