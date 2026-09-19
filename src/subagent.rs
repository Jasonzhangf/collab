use crate::{
    config,
    identity::AppServerId,
    proto::Resp,
    server::{
        state::{now_ms, Event},
        Server,
    },
};
use anyhow::{bail, Context, Result};
use clap::Subcommand;
use serde::{Deserialize, Serialize};
use serde_json::json;
use std::{
    process::{Command, Stdio},
    time::{Duration, Instant},
};

#[derive(Clone, Debug, Serialize, Deserialize, Subcommand)]
pub enum Action {
    Start {
        #[arg(long)]
        id: Option<String>,
        /// Override ~/.appsdk/config.toml [subagent].runtime for this child only
        #[arg(long)]
        #[serde(default)]
        runtime: Option<String>,
    },
    /// Dispatch a task through the live master scheduler.
    Dispatch {
        #[arg(long)]
        request_id: String,
        #[arg(long)]
        subject: String,
        body: String,
        #[arg(long)]
        feature_id: Option<String>,
        #[arg(long)]
        worktree_path: Option<String>,
        #[arg(long)]
        branch: Option<String>,
        #[arg(long)]
        base_commit: Option<String>,
        #[arg(long, default_value = "p2")]
        priority: String,
        #[arg(long)]
        next_step: Option<String>,
    },
    List,
    Status {
        id: String,
    },
    Snapshot {
        id: String,
        #[arg(long, default_value_t = 40)]
        lines: usize,
    },
    Rearm {
        id: String,
    },
    Send {
        id: String,
        #[arg(long)]
        subject: String,
        body: String,
    },
    Ready {
        id: String,
    },
    Working {
        id: String,
    },
    Close {
        id: String,
    },
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Record {
    pub id: String,
    pub parent: String,
    pub peer: String,
    pub status: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub thread_id: Option<String>,
    pub profile: Option<config::Profile>,
    pub created_ms: i64,
    pub ready_deadline_ms: i64,
    pub last_message: Option<String>,
    pub error: Option<String>,
    #[serde(default)]
    pub probe_failures: Vec<String>,
    #[serde(default)]
    pub runtime: Option<String>,
}
pub(crate) fn observe(
    server: &Server,
    id: Option<&str>,
    lines: Option<usize>,
) -> Result<serde_json::Value> {
    let (record, transport, mailbox, tasks, keepalive) = {
        let state = server.state.lock().unwrap();
        let Some(id) = id else {
            return Ok(json!({"subagents":state.subagents.values().collect::<Vec<_>>()}));
        };
        let record = state.subagents.get(id).context("unknown subagent")?.clone();
        let transport = state
            .workers
            .get(&record.peer)
            .and_then(|worker| worker.transport.clone());
        let mut mailbox: Vec<_> = state
            .msgs
            .values()
            .filter(|message| message.from == record.peer && message.to == record.parent)
            .cloned()
            .collect();
        mailbox.sort_by_key(|message| message.created_ms);
        let tasks = state
            .tasks
            .values()
            .filter(|task| task.owner == record.peer)
            .cloned()
            .collect::<Vec<_>>();
        let keepalive = crate::server::keepalive::view(&state, &record.peer);
        (record, transport, mailbox, tasks, keepalive)
    };
    if let Some(lines) = lines {
        if !(1..=200).contains(&lines) {
            bail!("snapshot lines must be 1..200");
        }
        let thread_id = record
            .thread_id
            .as_deref()
            .context("subagent has no App Server thread binding")?;
        let transport = transport.context("subagent has no registered App Server transport")?;
        let items = crate::client::adapters::codex_app_server::read_thread_items(
            &transport, thread_id, lines,
        )
        .map_err(|error| anyhow::anyhow!("{error}"))?;
        let text = serde_json::to_string_pretty(&items)?;
        let tail: Vec<_> = text.lines().rev().take(lines).collect();
        let mut value = json!({
            "subagent_id": record.id,
            "captured_ms": now_ms(),
            "thread_id": thread_id,
            "items": items,
            "text_tail": tail.into_iter().rev().collect::<Vec<_>>().join("\n")
        });
        merge_follow_up(&mut value);
        return Ok(value);
    }
    let thread_status = match (record.thread_id.as_deref(), transport.as_ref()) {
        (Some(thread_id), Some(transport)) => {
            (server.appserver_thread_status)(transport, thread_id)
                .map_err(|error| anyhow::anyhow!("{error}"))?
        }
        _ => serde_json::Value::Null,
    };
    let observed = if thread_status.is_null() {
        "unknown"
    } else {
        record.status.as_str()
    };
    let mut value = json!({
        "subagent": record,
        "observed_status": observed,
        "observed_ms": now_ms(),
        "thread_status": thread_status,
        "keepalive": keepalive,
        "mailbox": mailbox,
        "tasks": tasks
    });
    merge_follow_up(&mut value);
    Ok(value)
}

fn merge_follow_up(value: &mut serde_json::Value) {
    value["retry_allowed"] = json!(true);
    value["close_required"] = json!(false);
    value["next_check"] = json!("status");
    value["progress"] = json!("snapshot");
}

fn follow_up(record: &Record, reused: bool) -> serde_json::Value {
    let mut value = json!({"subagent": record, "reused": reused});
    merge_follow_up(&mut value);
    value
}

pub(crate) fn valid_id(id: &str) -> bool {
    !id.is_empty()
        && id.len() <= 80
        && id
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_')
}
pub(crate) fn valid_runtime(runtime: &str) -> bool {
    runtime == "codex"
}

fn child_prompt(record: &Record) -> String {
    format!(
        "You are a persistent AppSDK subagent. Your managed ID is {}. Your parent peer is {}. Your Collab identity is already registered on this Codex App Server thread. Do not self-register, recover a worker, or ask the user to grant identity. First report ready {}. Wait quietly for Collab messages. When assigned a task, read it, report working {}, and use the project's task/worktree workflow. Preserve others' files; code changes require your own worktree. Report progress through collab task records and send results to the parent with collab sendmessage --to {} --subject <topic> <body>. After completing a task report ready {} and remain available. Do not close this thread automatically, repeatedly poll, send ACK loops, or create other subagents without a user request. Collab master is project arbitration, not Codex root; you must follow master and parent direction and may not decline a master collaboration request.\n\
Collab master owns the final outcome for every dispatched task in this project. If master is unreachable within one escalation cycle, the master -- not you -- has the authority and the obligation to force-close with collab task close <task-id> --force --reason \"<text>\". You do not get to block, idle, or keep the task actionable. When you report a blocker, also report the concrete fix or the conditions the master must satisfy. Sending \"I'm blocked\" without a proposed solution is a master failure, not yours to ignore; do not let the master defer it back to you.\n\
 collab-mcp is the shared Collab MCP for every agent. Use collab_* tools when this session lists them. The collab CLI in this cwd is also valid. If MCP is missing, unsupported, aborted, or unknown, use the CLI. Missing MCP is not a reason to skip receive, ready, or send.\n\
CLI: collab subagent ready {}; collab subagent working {}; collab recv; collab ack <message-id>; collab msg <message-id>; collab inbox; collab sendmessage --to {} --subject <topic> \"<body>\"; collab task relocate <task-id> --worktree ./playground/<slug>.\n\
Each dispatched message has a canonical task named task-<message-id>. working claims that task; do not register a duplicate. Bind a clean worktree before code edits. ready only means thread idle. Use collab recv to read and consume a notification; use explicit ack only for legacy or already-delivered recovery. Never ACK an ACK or request automatic rearm after exhaustion.",
        record.id,
        record.parent,
        record.id,
        record.id,
        record.parent,
        record.id,
        record.id,
        record.id,
        record.parent
    )
}

fn launch_args(
    runtime: &str,
    profile: &config::Profile,
    workspace: &std::path::Path,
    prompt: &str,
    mcp: &std::path::Path,
) -> Result<(String, Vec<String>)> {
    if runtime != "codex" {
        bail!("subagent.runtime must be codex");
    }
    let _ = workspace;
    let mut args = vec![
        "--profile".into(),
        profile.codex_profile.clone(),
        "--approve-for-me".into(),
    ];
    if let Some(model) = &profile.model {
        args.extend(["--model".into(), model.clone()]);
    }
    args.extend([
        "-c".into(),
        format!(
            "mcp_servers.appsdk-subagent.command={}",
            serde_json::to_string(&mcp.to_string_lossy())?
        ),
        "-c".into(),
        "mcp_servers.appsdk-subagent.env_vars=[\"CODEX_THREAD_ID\",\"PATH\",\"HOME\"]".into(),
    ]);
    for tool in [
        "collab_init",
        "collab_subagent",
        "collab_msg",
        "collab_inbox",
        "collab_ack",
        "collab_context",
        "collab_sendmessage",
        "collab_notify_status",
        "collab_task_status",
        "collab_task_register",
        "collab_task_relocate",
        "collab_task_update",
        "collab_task_block",
        "collab_task_deliver",
        "collab_task_close",
        "collab_master",
    ] {
        args.extend([
            "-c".into(),
            format!("mcp_servers.appsdk-subagent.tools.{tool}.approval_mode=\"approve\""),
        ]);
    }
    args.push(format!(
        "{prompt}\nThis session may list the shared collab-mcp tools as appsdk-subagent. Use those tools when present. If collab_ack, collab_msg, or collab_init is missing, unsupported, or aborted, use the collab CLI in this cwd. That is protocol, not a bypass."
    ));
    Ok(("codex".into(), args))
}

fn finish_probe(child: &mut std::process::Child, timeout: Duration) -> Result<()> {
    let started = Instant::now();
    loop {
        if let Some(status) = child.try_wait()? {
            if !status.success() {
                bail!("probe exited {status}");
            }
            return Ok(());
        }
        if started.elapsed() >= timeout {
            // The probe owns this newly-created process group, including
            // its MCP children. Never signal unrelated named processes.
            let result = unsafe { libc::kill(-(child.id() as i32), libc::SIGKILL) };
            if result != 0 && child.try_wait()?.is_none() {
                return Err(std::io::Error::last_os_error().into());
            }
            child.wait()?;
            bail!("probe timed out");
        }
        std::thread::sleep(Duration::from_millis(100));
    }
}

fn probe_with(
    executable: &std::path::Path,
    runtime: &str,
    profile: &config::Profile,
    settings: &config::Health,
    environment: &std::collections::BTreeMap<String, String>,
) -> Result<()> {
    if runtime != "codex" {
        bail!("subagent.runtime must be codex");
    }
    let directory =
        std::env::temp_dir().join(format!("appsdk-probe-{:016x}", rand::random::<u64>()));
    std::fs::create_dir(&directory)?;
    let result = (|| {
        let output = directory.join("result.txt");
        let prompt = format!(
            "Connectivity probe only. Do not use tools or read files. Reply exactly: {}",
            settings.expected_response
        );
        let mut command = Command::new(executable);
        command.env_clear().envs(environment);
        command
            .args([
                "exec",
                "--profile",
                &profile.codex_profile,
                "--ephemeral",
                "--skip-git-repo-check",
                "--sandbox",
                "read-only",
                "--output-last-message",
            ])
            .arg(&output)
            .arg(&prompt);
        if let Some(model) = &profile.model {
            command.args(["--model", model]);
        }
        command
            .current_dir(&directory)
            .env_remove("TMUX")
            .env_remove("TMUX_PANE")
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null());
        use std::os::unix::process::CommandExt;
        command.process_group(0);
        let mut child = command.spawn().context("cannot start health probe")?;
        finish_probe(&mut child, Duration::from_secs(settings.timeout_seconds))?;
        let body = std::fs::read_to_string(output)?;
        if body.trim() != settings.expected_response {
            bail!(
                "probe response did not match expected response: {:?}",
                body.trim()
            );
        }
        Ok(())
    })();
    let cleanup = std::fs::remove_dir_all(&directory);
    result.and_then(|_| {
        cleanup?;
        Ok(())
    })
}
fn notify(
    server: &Server,
    from: &str,
    to: &str,
    subject: &str,
    body: String,
    assign_task: bool,
    managed_subagent_id: Option<&str>,
) -> Result<serde_json::Value> {
    let response = crate::server::handle_send_with_task(
        server,
        from.into(),
        to.into(),
        "notify".into(),
        Some(subject.into()),
        body,
        None,
        "immediate".into(),
        assign_task,
        managed_subagent_id,
    );
    if !response.ok {
        bail!("{}", response.error.unwrap_or_default());
    }
    Ok(response.data)
}
#[derive(Serialize, Deserialize)]
struct LaunchSpec {
    executable: String,
    args: Vec<String>,
    env: std::collections::BTreeMap<String, String>,
}

pub fn exec_launch(file: &std::path::Path) -> Result<()> {
    use std::os::unix::{fs::PermissionsExt, process::CommandExt};
    let metadata = std::fs::symlink_metadata(file)?;
    if !metadata.is_file() || metadata.permissions().mode() & 0o077 != 0 {
        bail!("unsafe launch manifest");
    }
    let spec: LaunchSpec = serde_json::from_slice(&std::fs::read(file)?)?;
    std::fs::remove_file(file)?;
    if spec.executable.is_empty() || spec.executable.starts_with('-') {
        bail!("unsafe launch executable");
    }
    let mut command = Command::new(&spec.executable);
    command.env_clear().envs(spec.env).args(spec.args);
    for key in ["TERM", "TERM_PROGRAM", "TERM_PROGRAM_VERSION", "COLORTERM"] {
        if let Some(value) = std::env::var_os(key) {
            command.env(key, value);
        }
    }
    Err(command.exec().into())
}

fn launch(
    server: &Server,
    record: &mut Record,
    settings: &config::Subagent,
    environment: std::collections::BTreeMap<String, String>,
    app_scope: Option<&AppServerId>,
) -> Result<()> {
    crate::scope::init(&server.root).context("cannot write project MCP and CLI permissions")?;
    record.runtime = Some(settings.runtime.clone());
    let mut errors = Vec::new();
    let executable = std::path::Path::new("codex");
    let names = settings.profile_priority.clone();
    for name in &names {
        let profile = &settings.profiles[name];
        match probe_with(
            executable,
            &settings.runtime,
            profile,
            &settings.health,
            &environment,
        ) {
            Ok(()) => {
                record.profile = Some(profile.clone());
                break;
            }
            Err(error) => errors.push(format!("{name}: {error}")),
        }
    }
    record.probe_failures = errors.clone();
    let profile = record
        .profile
        .as_ref()
        .context(format!("no healthy profile: {}", errors.join("; ")))?;
    let parent_transport = {
        let state = server.state.lock().unwrap();
        state
            .workers
            .get(&record.parent)
            .and_then(|worker| worker.transport.clone())
            .context("parent has no registered App Server transport")?
    };
    let thread_id = crate::client::adapters::codex_app_server::start_thread(
        &parent_transport,
        &server.root,
        profile.model.as_deref(),
    )
    .map_err(|error| anyhow::anyhow!("{error}"))?;
    record.thread_id = Some(thread_id.to_string());
    let prompt = child_prompt(record);
    let candidate = crate::proto::AppServerCandidate {
        endpoint: parent_transport
            .endpoint
            .clone()
            .context("parent App Server transport has no endpoint")?,
        namespace: parent_transport
            .namespace
            .clone()
            .context("parent App Server transport has no namespace")?,
        thread_id: thread_id.to_string(),
    };
    let scope = crate::scope::Scope {
        root: server.root.clone(),
    };
    let mut ident = crate::identity::load_or_create(&scope, Some(record.peer.clone()), None)?;
    let registered = crate::server::handle_register_with_app_scope(
        server,
        ident.worker_id.clone(),
        ident.token.clone(),
        server.root.display().to_string(),
        app_scope.cloned(),
        Some(crate::proto::TransportCandidates {
            appserver: Some(candidate),
        }),
    );
    if !registered.ok {
        let _ = crate::client::adapters::codex_app_server::archive_thread(
            &parent_transport,
            thread_id.as_str(),
        );
        bail!(
            "cannot register child identity: {}",
            registered.error.unwrap_or_default()
        );
    }
    let (runtime, transport) =
        crate::identity::registration_from_receipt(&registered.data, &ident.worker_id, &scope.root)
            .context("child registration receipt did not contain its runtime binding")?;
    crate::identity::persist_registration(&scope, &mut ident, runtime, transport.clone())
        .context("cannot persist child runtime binding")?;
    crate::client::adapters::codex_app_server::immediate_notify(
        &transport,
        &prompt,
        &format!("collab-subagent-start-{}", record.id),
    )
    .map_err(|error| anyhow::anyhow!("{error}"))?;
    let _ = environment;
    record.status = "starting".into();
    record.ready_deadline_ms = now_ms() + settings.startup.ready_timeout_seconds as i64 * 1000;
    Ok(())
}
#[cfg(test)]
pub fn handle(server: &Server, actor: &str, token: &str, action: Action) -> Resp {
    handle_with_env(server, actor, token, action, std::env::vars().collect())
}
pub fn handle_with_env(
    server: &Server,
    actor: &str,
    token: &str,
    action: Action,
    environment: std::collections::BTreeMap<String, String>,
) -> Resp {
    handle_with_env_route(server, actor, token, action, None, environment)
}

/// Production wire entry point. The app scope was admitted from the parent's
/// validated ProjectContext and is carried into child registration explicitly.
pub(crate) fn handle_with_env_for_app_scope(
    server: &Server,
    actor: &str,
    token: &str,
    action: Action,
    app_scope: AppServerId,
    environment: std::collections::BTreeMap<String, String>,
) -> Resp {
    handle_with_env_route(server, actor, token, action, Some(app_scope), environment)
}

fn handle_with_env_route(
    server: &Server,
    actor: &str,
    token: &str,
    action: Action,
    app_scope: Option<AppServerId>,
    environment: std::collections::BTreeMap<String, String>,
) -> Resp {
    if let Action::Dispatch {
        request_id,
        subject,
        body,
        feature_id,
        worktree_path,
        branch,
        base_commit,
        priority,
        next_step,
    } = action
    {
        return crate::server::handle_scheduler_dispatch(
            server,
            actor.into(),
            token.into(),
            request_id,
            subject,
            body,
            feature_id,
            worktree_path,
            branch,
            base_commit,
            priority,
            next_step,
        );
    }
    if let Action::Start {
        ref id,
        ref runtime,
    } = action
    {
        match crate::server::scheduler_admit_subagent_start(
            server,
            actor,
            token,
            id.as_deref(),
            runtime.as_deref(),
        ) {
            Ok(Some(response)) => return response,
            Ok(None) => {}
            Err(response) => return response,
        }
    }
    match run(server, actor, token, action, environment, app_scope) {
        Ok(value) => Resp::data(value),
        Err(e) => Resp::err(e.to_string()),
    }
}
fn run(
    server: &Server,
    actor: &str,
    token: &str,
    action: Action,
    environment: std::collections::BTreeMap<String, String>,
    app_scope: Option<AppServerId>,
) -> Result<serde_json::Value> {
    {
        let state = server.state.lock().unwrap();
        if !state.workers.get(actor).is_some_and(|w| w.token == token) {
            bail!("subagent authentication failed");
        }
    }
    if let Action::Start { id, runtime } = action {
        config::ensure_written()?;
        let mut config = config::load(&server.root)?;
        if let Some(runtime) = runtime {
            if !valid_runtime(runtime.as_str()) {
                bail!("subagent.runtime must be codex");
            }
            config.subagent.runtime = runtime;
        }
        let id = id.unwrap_or_else(|| format!("sa-{:016x}", rand::random::<u64>()));
        if !valid_id(&id) {
            bail!("invalid subagent ID");
        }
        let cwd_name = server
            .root
            .file_name()
            .unwrap_or_default()
            .to_string_lossy();
        let prefix: String = cwd_name
            .chars()
            .map(|c| {
                if c.is_ascii_alphanumeric() || c == '-' {
                    c
                } else {
                    '-'
                }
            })
            .take(32)
            .collect();
        let peer = config
            .subagent
            .name_template
            .replace("{cwd_name}", &prefix)
            .replace("{short_id}", &id);
        if !valid_id(&peer) {
            bail!("subagent name must contain only ASCII letters, digits, dash or underscore and be <=80 characters");
        }
        let mut record = Record {
            id: id.clone(),
            parent: actor.into(),
            peer,
            status: "probing".into(),
            thread_id: None,
            profile: None,
            created_ms: now_ms(),
            ready_deadline_ms: 0,
            last_message: None,
            error: None,
            probe_failures: Vec::new(),
            runtime: Some(config.subagent.runtime.clone()),
        };
        {
            let mut state = server.state.lock().unwrap();
            if let Some(existing) = state.subagents.get(&id) {
                if existing.parent != actor {
                    bail!("subagent belongs to another parent");
                }
                if existing.thread_id.is_some() {
                    let existing = existing.clone();
                    return Ok(follow_up(&existing, true));
                }
                record = existing.clone();
                record.runtime = Some(config.subagent.runtime.clone());
            } else {
                server
                    .commit_locked_checked(
                        &mut state,
                        &[Event::SubagentUpdated {
                            subagent: record.clone(),
                        }],
                    )
                    .map_err(|error| anyhow::anyhow!("subagent start journal failure: {error}"))?;
            }
        }
        if let Err(e) = launch(
            server,
            &mut record,
            &config.subagent,
            environment,
            app_scope.as_ref(),
        ) {
            let msg = e.to_string();
            record.error = Some(msg.clone());
            if msg.contains("timed out") {
                record.status = "probing".into();
                server
                    .commit_checked(&[Event::SubagentUpdated {
                        subagent: record.clone(),
                    }])
                    .map_err(|error| {
                        anyhow::anyhow!(
                            "subagent start outcome unknown: probe timed out and journal commit failed: {error}"
                        )
                    })?;
                return Ok(follow_up(&record, false));
            }
            record.status = "failed".into();
        }
        server
            .commit_checked(&[Event::SubagentUpdated {
                subagent: record.clone(),
            }])
            .map_err(|error| {
                anyhow::anyhow!("subagent start outcome unknown: journal commit failed: {error}")
            })?;
        return Ok(follow_up(&record, false));
    }
    if matches!(action, Action::List) {
        let state = server.state.lock().unwrap();
        return Ok(
            json!({"subagents": state.subagents.values().filter(|s| s.parent == actor).collect::<Vec<_>>() }),
        );
    }
    let id = match &action {
        Action::Status { id }
        | Action::Snapshot { id, .. }
        | Action::Rearm { id }
        | Action::Send { id, .. }
        | Action::Ready { id }
        | Action::Working { id }
        | Action::Close { id } => id,
        _ => unreachable!(),
    };
    let mut state = server.state.lock().unwrap();
    let mut record = state.subagents.get(id).context("unknown subagent")?.clone();
    let child_action = matches!(action, Action::Ready { .. } | Action::Working { .. });
    if child_action {
        let bound_thread = state
            .workers
            .get(actor)
            .and_then(|worker| worker.transport.as_ref())
            .and_then(|transport| transport.thread_id.as_deref());
        if record.peer != actor || record.thread_id.as_deref() != bound_thread {
            bail!("only the bound subagent may report readiness or work");
        }
    } else if record.parent != actor
        && crate::server::live_master_id(server, &state)
            .map_err(anyhow::Error::msg)?
            .as_deref()
            != Some(actor)
    {
        bail!("only the creating parent or live master may manage this subagent");
    }
    match action {
        Action::Snapshot { lines, .. } => {
            drop(state);
            return observe(server, Some(&record.id), Some(lines));
        }
        Action::Rearm { .. } => {
            server
                .commit_locked_checked(
                    &mut state,
                    &[Event::KeepaliveUpdated {
                        worker_id: record.peer.clone(),
                        record: crate::server::keepalive::Record::default(),
                    }],
                )
                .map_err(|error| anyhow::anyhow!("subagent rearm journal failure: {error}"))?;
            return Ok(
                json!({"subagent_id":record.id,"keepalive_rearmed":true,"notification":"none"}),
            );
        }
        Action::Status { .. } => {
            drop(state);
            return observe(server, Some(&record.id), None);
        }
        Action::Ready { .. } | Action::Working { .. } => {
            let ready = matches!(action, Action::Ready { .. });
            if matches!(
                record.status.as_str(),
                "closing" | "closed" | "failed" | "probing"
            ) {
                bail!("subagent is not running");
            }
            if ready && record.status == "idle" {
                return Ok(json!({"subagent": record, "reused": true}));
            }
            if ready && record.status == "assigned" {
                bail!("accept the assigned task before reporting completion");
            }
            let assigned_task = if !ready {
                // A keepalive thread probe may persist `working` before the
                // child gets a chance to claim its still-assigned task. Keep
                // the task binding as the source of truth and accept both
                // sides of that short race. A working task is accepted only
                // when the managed record is already working, which makes a
                // repeated claim idempotent without reopening other states.
                if !matches!(record.status.as_str(), "assigned" | "working") {
                    bail!("no assigned task to accept");
                }
                let task_id = record
                    .last_message
                    .as_ref()
                    .map(|id| format!("task-{id}"))
                    .ok_or_else(|| anyhow::anyhow!("assigned task message binding is missing"))?;
                let mut task = state
                    .tasks
                    .get(&task_id)
                    .cloned()
                    .ok_or_else(|| anyhow::anyhow!("assigned task {task_id} not found"))?;
                if task.owner != actor {
                    bail!("task owner mismatch");
                }
                if let Some(admission) = state
                    .scheduler_admissions
                    .values()
                    .find(|admission| admission.task_id == task.id)
                {
                    if admission.status != "succeeded" {
                        bail!(
                            "scheduler assignment admission is {}; cannot accept task {}",
                            admission.status,
                            task_id
                        );
                    }
                    if admission.managed_subagent_id.as_deref() != Some(record.id.as_str()) {
                        bail!("managed subagent does not own scheduler task {}", task_id);
                    }
                }
                if task.status != "assigned"
                    && !(record.status == "working" && task.status == "working")
                {
                    bail!(
                        "assigned task {task_id} is not in assigned state (status={})",
                        task.status
                    );
                }
                let task_was_assigned = task.status == "assigned";
                if task_was_assigned {
                    task.status = "working".into();
                    task.updated_ms = now_ms();
                }
                Some((task, task_was_assigned))
            } else {
                None
            };
            let next_status = if ready { "idle" } else { "working" };
            let mut events = Vec::new();
            if let Some((task, task_was_assigned)) = assigned_task {
                if task_was_assigned {
                    events.push(Event::TaskUpdated { task });
                }
            }
            if record.status != next_status {
                record.status = next_status.into();
                events.push(Event::SubagentUpdated {
                    subagent: record.clone(),
                });
            }
            if !events.is_empty() {
                // Persist the task and managed-record transition together so
                // replay cannot observe a half-claimed assignment.
                server
                    .commit_locked_checked(&mut state, &events)
                    .map_err(|error| {
                        anyhow::anyhow!("subagent working journal failure: {error}")
                    })?;
            }
            drop(state);
            if ready {
                notify(
                    server,
                    actor,
                    &record.parent,
                    "subagent-idle",
                    format!("subagent={} is idle and available", record.id),
                    false,
                    None,
                )?;
            }
        }
        Action::Send { subject, body, .. } => {
            if subject.trim().is_empty() || body.trim().is_empty() {
                bail!("subject and task body are required");
            }
            // A keepalive thread observation can race with the child's ready
            // report and leave the durable managed status at working even
            // though the child owns no actionable task. Reconcile that stale
            // state before asking the task sender to bind the next dispatch.
            let has_active_owned_task = state.tasks.values().any(|task| {
                task.owner == record.peer
                    && crate::server::state::task_resource_active(&task.status)
            });
            let stale_working_without_task = record.status == "working" && !has_active_owned_task;
            if record.status == "idle" && has_active_owned_task {
                bail!("managed subagent already has an active task");
            }
            if record.status != "idle" && !stale_working_without_task {
                bail!("subagent is not idle; query status instead of resending");
            }
            if stale_working_without_task {
                record.status = "idle".into();
                server
                    .commit_locked_checked(
                        &mut state,
                        &[Event::SubagentUpdated {
                            subagent: record.clone(),
                        }],
                    )
                    .map_err(|error| anyhow::anyhow!("subagent send journal failure: {error}"))?;
            }
            drop(state);
            let result = match notify(
                server,
                actor,
                &record.peer,
                &subject,
                body,
                true,
                Some(&record.id),
            ) {
                Ok(value) => value,
                Err(error) => {
                    let mut state = server.state.lock().unwrap();
                    if let Some(mut current) = state.subagents.get(&record.id).cloned() {
                        if current.status == "idle" {
                            current.error = Some(error.to_string());
                            if let Err(journal_error) = server.commit_locked_checked(
                                &mut state,
                                &[Event::SubagentUpdated { subagent: current }],
                            ) {
                                return Err(anyhow::anyhow!(
                                    "subagent send outcome unknown: notification failed and journal commit failed: {journal_error}"
                                ));
                            }
                        }
                    }
                    return Err(error);
                }
            };
            return Ok(json!({"subagent_id": record.id, "message": result}));
        }
        Action::Close { .. } => {
            if record.status == "closed" {
                return Ok(json!({"subagent": record, "reused": true}));
            }
            if record.status == "probing" && record.error.is_none() {
                bail!("startup probe is in progress; check status, or close after its bounded completion");
            }
            record.status = "closing".into();
            server
                .commit_locked_checked(
                    &mut state,
                    &[Event::SubagentUpdated {
                        subagent: record.clone(),
                    }],
                )
                .map_err(|error| anyhow::anyhow!("subagent close journal failure: {error}"))?;
            drop(state);
            if let Some(thread_id) = record.thread_id.as_deref() {
                let transport = {
                    let state = server.state.lock().unwrap();
                    state
                        .workers
                        .get(&record.peer)
                        .and_then(|worker| worker.transport.clone())
                }
                .context("subagent has no registered App Server transport")?;
                (server.appserver_thread_archive)(&transport, thread_id)
                    .map_err(|error| anyhow::anyhow!("{error}"))?;
            }
            record.status = "closed".into();
            server
                .commit_checked(&[Event::SubagentUpdated {
                    subagent: record.clone(),
                }])
                .map_err(|error| {
                    anyhow::anyhow!("subagent close outcome unknown: external close completed but journal commit failed: {error}")
                })?;
        }
        _ => unreachable!(),
    }
    Ok(json!({"subagent": record}))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::fs::PermissionsExt;
    #[test]
    fn probes_are_bounded_and_require_exact_success() {
        let directory =
            std::env::temp_dir().join(format!("collab-probe-test-{:016x}", rand::random::<u64>()));
        std::fs::create_dir(&directory).unwrap();
        let executable = directory.join("codex-fixture");
        std::fs::write(
            &executable,
            "#!/bin/sh\nprofile=\noutput=\nwhile [ $# -gt 0 ]; do\n case \"$1\" in\n --profile) shift; profile=$1;;\n --output-last-message) shift; output=$1;;\n esac\n shift\ndone\ncase \"$profile\" in\n good) printf OK > \"$output\";;\n env) if [ -z \"${TMUX+x}\" ] && [ -z \"${TMUX_PANE+x}\" ]; then printf OK > \"$output\"; else printf INHERITED > \"$output\"; fi;;\n wrong) printf NOT_OK > \"$output\";;\n fail) exit 3;;\n slow) exec sleep 2;;\nesac\n",
        )
        .unwrap();
        std::fs::set_permissions(&executable, std::fs::Permissions::from_mode(0o700)).unwrap();
        let settings = config::Health {
            timeout_seconds: 1,
            ..Default::default()
        };
        for (name, expected) in [
            ("good", true),
            ("env", true),
            ("wrong", false),
            ("fail", false),
            ("slow", false),
        ] {
            let mut environment: std::collections::BTreeMap<_, _> = std::env::vars().collect();
            environment.insert("TMUX".into(), "/tmp/legacy-tmux".into());
            environment.insert("TMUX_PANE".into(), "%42".into());
            let start = Instant::now();
            let result = probe_with(
                &executable,
                "codex",
                &config::Profile {
                    codex_profile: name.into(),
                    model: None,
                },
                &settings,
                &environment,
            );
            assert_eq!(result.is_ok(), expected, "profile={name} result={result:?}");
            if name == "slow" {
                assert_eq!(result.unwrap_err().to_string(), "probe timed out");
            }
            assert!(start.elapsed() < Duration::from_secs(3));
        }
    }
    #[test]
    fn cursor_runtime_is_rejected() {
        let mcp = std::path::Path::new("/tmp/collab-mcp");
        let error = launch_args(
            "cursor",
            &config::Profile {
                codex_profile: "oauth".into(),
                model: None,
            },
            std::path::Path::new("/tmp/project"),
            "hello",
            mcp,
        )
        .unwrap_err()
        .to_string();
        assert!(error.contains("must be codex"), "{error}");
        let (exe, args) = launch_args(
            "codex",
            &config::Profile {
                codex_profile: "oauth".into(),
                model: None,
            },
            std::path::Path::new("/tmp/project"),
            "hello",
            mcp,
        )
        .unwrap();
        assert_eq!(exe, "codex");
        assert_eq!(args[..3], ["--profile", "oauth", "--approve-for-me"]);
        assert!(args.contains(&"--approve-for-me".to_string()));
        assert!(!args.contains(&"dangerously-bypass-approvals-and-sandbox".to_string()));
        assert!(!args
            .iter()
            .any(|a| a == "danger-full-access" || a == "--ask-for-approval"));
        assert!(args
            .iter()
            .any(|a| a.contains("mcp_servers.appsdk-subagent")));
        assert!(args
            .iter()
            .any(|a| a.contains("collab_ack") && a.contains("approve")));
        assert!(args.last().unwrap().contains("collab CLI"));
        let prompt = child_prompt(&Record {
            id: "child-1".into(),
            parent: "parent-1".into(),
            peer: "peer-1".into(),
            status: "starting".into(),
            thread_id: None,
            profile: None,
            created_ms: 0,
            ready_deadline_ms: 0,
            last_message: None,
            error: None,
            probe_failures: vec![],
            runtime: None,
        });
        assert!(prompt.contains("collab ack <message-id>"));
        assert!(prompt.contains("already registered"));
        assert!(!prompt.contains("collab init"));
        assert!(!prompt.contains("worker recover"));
        assert!(prompt.contains("shared Collab MCP"));
        assert!(prompt.contains("collab CLI in this cwd is also valid"));
        assert!(!prompt.contains("NOT sandboxed shell"));
    }
}
