use crate::{
    config,
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
    pub session: Option<String>,
    pub pane: Option<String>,
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
    let state = server.state.lock().unwrap();
    let Some(id) = id else {
        return Ok(json!({"subagents":state.subagents.values().collect::<Vec<_>>()}));
    };
    let record = state.subagents.get(id).context("unknown subagent")?;
    if let Some(lines) = lines {
        if !(1..=200).contains(&lines) {
            bail!("snapshot lines must be 1..200");
        }
        if record.pane.is_none() {
            let mut value = json!({
                "subagent_id": record.id,
                "captured_ms": now_ms(),
                "pane": serde_json::Value::Null,
                "screen_tail": ""
            });
            merge_follow_up(&mut value);
            return Ok(value);
        }
        let pane = record.pane.as_deref().context("subagent has no pane")?;
        if !crate::server::knock::pane_alive(pane) {
            bail!("subagent pane exited");
        }
        let binding = Command::new("tmux")
            .args([
                "display-message",
                "-p",
                "-t",
                pane,
                "#{session_id} #{session_name}",
            ])
            .output()?;
        if !binding.status.success()
            || String::from_utf8_lossy(&binding.stdout).trim()
                != format!(
                    "{} {}",
                    record.session.as_deref().unwrap_or_default(),
                    record.peer
                )
        {
            bail!("subagent session identity changed");
        }
        let output = Command::new("tmux")
            .args(["capture-pane", "-p", "-t", pane, "-S", &format!("-{lines}")])
            .output()?;
        if !output.status.success() {
            bail!("tmux snapshot failed");
        }
        let text = String::from_utf8_lossy(&output.stdout);
        let tail: Vec<_> = text.lines().rev().take(lines).collect();
        let mut value = json!({"subagent_id":record.id,"captured_ms":now_ms(),"pane":pane,
            "screen_tail":tail.into_iter().rev().collect::<Vec<_>>().join("\n")});
        merge_follow_up(&mut value);
        return Ok(value);
    }
    let observed = match record.pane.as_deref() {
        None => "unknown",
        Some(pane) if !crate::server::knock::pane_alive(pane) => "exited",
        Some(pane) => match crate::server::knock::probe_agent_state(pane) {
            crate::server::knock::AgentState::Absent => "agent_absent",
            crate::server::knock::AgentState::Unknown => "unknown",
            crate::server::knock::AgentState::Working => "working",
            crate::server::knock::AgentState::Waiting => "idle",
        },
    };
    let mut mailbox: Vec<_> = state
        .msgs
        .values()
        .filter(|m| m.from == record.peer && m.to == record.parent)
        .collect();
    mailbox.sort_by_key(|m| m.created_ms);
    let mut value = json!({"subagent":record,"observed_status":observed,"observed_ms":now_ms(),
        "keepalive":crate::server::keepalive::view(&state,&record.peer),"mailbox":mailbox,
        "tasks":state.tasks.values().filter(|t|t.owner==record.peer).collect::<Vec<_>>()});
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
    matches!(runtime, "cursor" | "codex")
}
fn is_cursor(runtime: &str) -> bool {
    runtime == "cursor"
}

fn child_prompt(record: &Record) -> String {
    format!(
        "You are a persistent AppSDK subagent. Your managed ID is {}. Your parent peer is {}. Your Collab identity is already registered as this tmux session. Do not self-register, recover a worker, or ask the user to grant identity. First report ready {}. Wait quietly for Collab messages. When assigned a task, read it, report working {}, and use the project's task/worktree workflow. Preserve others' files; code changes require your own worktree. Report progress through collab task records and send results to the parent with collab sendmessage --to {} --subject <topic> <body>. After completing a task report ready {} and remain available. Do not close this session automatically, repeatedly poll, send ACK loops, or create other subagents without a user request. Collab master is project arbitration, not Codex/Cursor root; you must follow master and parent direction and may not decline a master collaboration request.\n\
Collab master owns the final outcome for every dispatched task in this project. If master is unreachable within one escalation cycle, the master -- not you -- has the authority and the obligation to force-close with collab task close <task-id> --force --reason \"<text>\". You do not get to block, idle, or keep the task actionable. When you report a blocker, also report the concrete fix or the conditions the master must satisfy. Sending \"I'm blocked\" without a proposed solution is a master failure, not yours to ignore; do not let the master defer it back to you.\n\
 collab-mcp is the shared Collab MCP for every agent. Use collab_* tools when this session lists them. The collab CLI in this cwd is also valid. If MCP is missing, unsupported, aborted, or unknown, use the CLI. Missing MCP is not a reason to skip receive, ready, or send.\n\
CLI: collab subagent ready {}; collab subagent working {}; collab recv; collab ack <message-id>; collab msg <message-id>; collab inbox; collab sendmessage --to {} --subject <topic> \"<body>\"; collab task relocate <task-id> --worktree ./playground/<slug>.\n\
Each dispatched message has a canonical task named task-<message-id>. working claims that task; do not register a duplicate. Bind a clean worktree before code edits. ready only means session idle. Use collab recv to read and consume a notification; use explicit ack only for legacy or already-delivered recovery. Never ACK an ACK or request automatic rearm after exhaustion.",
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
    if is_cursor(runtime) {
        let mut args = vec![
            "--yolo".into(),
            "--trust".into(),
            "--approve-mcps".into(),
            "--sandbox".into(),
            "disabled".into(),
            "--workspace".into(),
            workspace.to_string_lossy().into_owned(),
        ];
        if let Some(model) = &profile.model {
            args.extend(["--model".into(), model.clone()]);
        } else {
            args.extend(["--model".into(), "auto".into()]);
        }
        args.push(prompt.into());
        if args.iter().any(|a| a == "--worktree" || a == "persist") {
            bail!("cursor launch must not use persist or --worktree");
        }
        return Ok(("agent".into(), args));
    }
    let mut args = vec!["--profile".into(), profile.codex_profile.clone()];
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
        "mcp_servers.appsdk-subagent.env_vars=[\"TMUX\",\"TMUX_PANE\",\"PATH\",\"HOME\"]".into(),
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

fn probe_cursor_status(
    executable: &std::path::Path,
    settings: &config::Health,
    environment: &std::collections::BTreeMap<String, String>,
) -> Result<()> {
    let mut command = Command::new(executable);
    command.env_clear().envs(environment);
    command.args(["status", "--format", "json"]);
    command
        .env_remove("TMUX_PANE")
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null());
    use std::os::unix::process::CommandExt;
    command.process_group(0);
    let mut child = command.spawn().context("cannot start health probe")?;
    finish_probe(&mut child, Duration::from_secs(settings.timeout_seconds))?;
    let mut text = String::new();
    if let Some(mut stdout) = child.stdout.take() {
        std::io::Read::read_to_string(&mut stdout, &mut text)?;
    }
    let value: serde_json::Value =
        serde_json::from_str(text.trim()).context("cursor status did not return JSON")?;
    if value.get("loggedIn") != Some(&json!(true))
        && value.get("isAuthenticated") != Some(&json!(true))
        && value.get("status") != Some(&json!("authenticated"))
    {
        bail!("cursor is not logged in");
    }
    Ok(())
}

fn probe_with(
    executable: &std::path::Path,
    runtime: &str,
    profile: &config::Profile,
    settings: &config::Health,
    environment: &std::collections::BTreeMap<String, String>,
) -> Result<()> {
    if is_cursor(runtime) {
        return probe_cursor_status(executable, settings, environment);
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
    for key in [
        "TMUX",
        "TMUX_PANE",
        "TERM",
        "TERM_PROGRAM",
        "TERM_PROGRAM_VERSION",
        "COLORTERM",
    ] {
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
) -> Result<()> {
    crate::scope::init(&server.root).context("cannot write project MCP and CLI permissions")?;
    record.runtime = Some(settings.runtime.clone());
    let mut errors = Vec::new();
    let executable = if is_cursor(&settings.runtime) {
        std::path::Path::new("agent")
    } else {
        std::path::Path::new("codex")
    };
    let names: Vec<String> = if is_cursor(&settings.runtime) {
        settings
            .profile_priority
            .first()
            .cloned()
            .into_iter()
            .collect()
    } else {
        settings.profile_priority.clone()
    };
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
    let prompt = child_prompt(record);
    let mcp = std::env::current_exe()
        .ok()
        .and_then(|p| p.parent().map(|d| d.join("collab-mcp")))
        .unwrap_or_else(|| std::path::PathBuf::from("collab-mcp"));
    let (executable, args) = launch_args(&settings.runtime, profile, &server.root, &prompt, &mcp)?;
    use std::io::Write;
    use std::os::unix::fs::OpenOptionsExt;
    let manifest = server
        .root
        .join(".agent-collab/server")
        .join(format!("launch-{}.json", record.id));
    let mut file = std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(&manifest)?;
    let mut environment = environment;
    environment.remove("TMUX");
    environment.remove("TMUX_PANE");
    environment.insert("COLLAB_WORKER".into(), record.peer.clone());
    file.write_all(&serde_json::to_vec(&LaunchSpec {
        executable,
        args,
        env: environment,
    })?)?;
    file.sync_all()?;
    let mut command = Command::new("tmux");
    command
        .args([
            "new-session",
            "-d",
            "-P",
            "-F",
            "#{session_id} #{pane_id}",
            "-s",
            &record.peer,
            "-c",
        ])
        .arg(&server.root)
        .arg(std::env::current_exe()?)
        .arg("subagent-exec")
        .arg(&manifest);
    let output = match command.output() {
        Ok(output) => output,
        Err(error) => {
            std::fs::remove_file(&manifest)?;
            return Err(error.into());
        }
    };
    if !output.status.success() {
        std::fs::remove_file(&manifest)?;
        bail!(
            "tmux start failed: {}",
            String::from_utf8_lossy(&output.stderr)
        );
    }
    let binding = String::from_utf8(output.stdout)?;
    let mut parts = binding.split_whitespace();
    record.session = Some(parts.next().context("missing session ID")?.into());
    record.pane = Some(parts.next().context("missing pane ID")?.into());
    let ident = crate::identity::provision(
        &crate::scope::Scope {
            root: server.root.clone(),
        },
        &record.peer,
        record.pane.as_deref().context("missing pane")?,
        &record.peer,
    )?;
    let registered = crate::server::handle_register(
        server,
        ident.worker_id,
        ident.token,
        ident.pane,
        server.root.display().to_string(),
    );
    if !registered.ok {
        bail!(
            "cannot register child identity: {}",
            registered.error.unwrap_or_default()
        );
    }
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
    match run(server, actor, token, action, environment) {
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
                bail!("subagent.runtime must be cursor or codex");
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
            .tmux
            .name_template
            .replace("{cwd_name}", &prefix)
            .replace("{short_id}", &id);
        if !valid_id(&peer) {
            bail!("tmux name must contain only ASCII letters, digits, dash or underscore and be <=80 characters");
        }
        let mut record = Record {
            id: id.clone(),
            parent: actor.into(),
            peer,
            status: "probing".into(),
            session: None,
            pane: None,
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
                if existing.pane.is_some() {
                    let existing = existing.clone();
                    return Ok(follow_up(&existing, true));
                }
                record = existing.clone();
                record.runtime = Some(config.subagent.runtime.clone());
            } else {
                server.commit_locked(
                    &mut state,
                    &[Event::SubagentUpdated {
                        subagent: record.clone(),
                    }],
                );
            }
        }
        if let Err(e) = launch(server, &mut record, &config.subagent, environment) {
            let msg = e.to_string();
            record.error = Some(msg.clone());
            if msg.contains("timed out") {
                record.status = "probing".into();
                server.commit(&[Event::SubagentUpdated {
                    subagent: record.clone(),
                }]);
                return Ok(follow_up(&record, false));
            }
            record.status = "failed".into();
        }
        server.commit(&[Event::SubagentUpdated {
            subagent: record.clone(),
        }]);
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
        if record.peer != actor || state.workers[actor].pane != record.pane {
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
            server.commit_locked(
                &mut state,
                &[Event::KeepaliveUpdated {
                    worker_id: record.peer.clone(),
                    record: crate::server::keepalive::Record::default(),
                }],
            );
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
                // A keepalive pane probe may persist `working` before the
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
                server.commit_locked(&mut state, &events);
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
            // A keepalive pane observation can race with the child's ready
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
                server.commit_locked(
                    &mut state,
                    &[Event::SubagentUpdated {
                        subagent: record.clone(),
                    }],
                );
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
                            server.commit_locked(
                                &mut state,
                                &[Event::SubagentUpdated { subagent: current }],
                            );
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
            server.commit_locked(
                &mut state,
                &[Event::SubagentUpdated {
                    subagent: record.clone(),
                }],
            );
            drop(state);
            if let (Some(session), Some(pane)) = (&record.session, &record.pane) {
                if crate::server::knock::pane_alive(pane) {
                    let output = Command::new("tmux")
                        .args([
                            "display-message",
                            "-p",
                            "-t",
                            pane,
                            "#{session_id} #{session_name}",
                        ])
                        .output()?;
                    if !output.status.success()
                        || String::from_utf8_lossy(&output.stdout).trim()
                            != format!("{} {}", session, record.peer)
                    {
                        bail!("session identity changed; refusing to close");
                    }
                    if !Command::new("tmux")
                        .args(["kill-session", "-t", session])
                        .status()?
                        .success()
                    {
                        bail!("tmux close failed");
                    }
                }
            }
            let manifest = server
                .root
                .join(".agent-collab/server")
                .join(format!("launch-{}.json", record.id));
            match std::fs::remove_file(&manifest) {
                Ok(()) => {}
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
                Err(error) => return Err(error.into()),
            }
            record.status = "closed".into();
            server.commit(&[Event::SubagentUpdated {
                subagent: record.clone(),
            }]);
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
    #[ignore = "requires tmux and node; creates only a disposable session"]
    fn snapshot_is_explicit_bounded_and_checks_session_binding() {
        let (server, root) = crate::server::peer_tests::test_server();
        let name = format!("collab-snapshot-test-{}", std::process::id());
        let output = Command::new("tmux").args(["new-session","-d","-P","-F","#{session_id} #{pane_id}","-s",&name,
            "node -e 'process.stdout.write(\"snapshot-marker\\n\".repeat(100));setInterval(()=>{},1000)'"
        ]).output().unwrap();
        assert!(output.status.success());
        struct Cleanup(String);
        impl Drop for Cleanup {
            fn drop(&mut self) {
                let _ = Command::new("tmux")
                    .args(["kill-session", "-t", &self.0])
                    .status();
            }
        }
        let _cleanup = Cleanup(name.clone());
        let text = String::from_utf8(output.stdout).unwrap();
        let binding: Vec<_> = text.split_whitespace().collect();
        let mut record = Record {
            id: "snapshot".into(),
            parent: "parent".into(),
            peer: name,
            status: "idle".into(),
            session: Some(binding[0].into()),
            pane: Some(binding[1].into()),
            profile: None,
            created_ms: now_ms(),
            ready_deadline_ms: 0,
            last_message: None,
            error: None,
            probe_failures: vec![],
            runtime: None,
        };
        server.commit(&[Event::SubagentUpdated {
            subagent: record.clone(),
        }]);
        std::thread::sleep(Duration::from_millis(300));
        assert!(observe(&server, Some("snapshot"), None)
            .unwrap()
            .get("screen_tail")
            .is_none());
        let snap = observe(&server, Some("snapshot"), Some(5)).unwrap();
        assert!(snap["screen_tail"]
            .as_str()
            .unwrap()
            .contains("snapshot-marker"));
        assert!(snap["screen_tail"].as_str().unwrap().lines().count() <= 5);
        assert!(observe(&server, Some("snapshot"), Some(201)).is_err());
        record.session = Some("$not-ours".into());
        server.commit(&[Event::SubagentUpdated { subagent: record }]);
        assert!(observe(&server, Some("snapshot"), Some(5)).is_err());
        assert!(server.state.lock().unwrap().msgs.is_empty());
        std::fs::remove_dir_all(root).unwrap();
    }
    #[test]
    fn probes_are_bounded_and_require_exact_success() {
        let directory =
            std::env::temp_dir().join(format!("collab-probe-test-{:016x}", rand::random::<u64>()));
        std::fs::create_dir(&directory).unwrap();
        let executable = directory.join("codex-fixture");
        std::fs::write(&executable, "#!/bin/sh\nwhile [ $# -gt 0 ]; do\n case \"$1\" in\n --profile) shift; profile=$1;;\n --output-last-message) shift; output=$1;;\n esac\n shift\ndone\ncase \"$profile\" in\n good) printf OK > \"$output\";;\n wrong) printf NOT_OK > \"$output\";;\n fail) exit 3;;\n slow) exec sleep 20;;\nesac\n").unwrap();
        std::fs::set_permissions(&executable, std::fs::Permissions::from_mode(0o700)).unwrap();
        let settings = config::Health {
            timeout_seconds: 1,
            ..Default::default()
        };
        for (name, expected) in [
            ("good", true),
            ("wrong", false),
            ("fail", false),
            ("slow", false),
        ] {
            let start = Instant::now();
            assert_eq!(
                probe_with(
                    &executable,
                    "codex",
                    &config::Profile {
                        codex_profile: name.into(),
                        model: None
                    },
                    &settings,
                    &std::env::vars().collect()
                )
                .is_ok(),
                expected
            );
            assert!(start.elapsed() < Duration::from_secs(3));
        }
        std::fs::remove_dir_all(directory).unwrap();
    }
    #[test]
    fn cursor_probe_uses_official_status_json() {
        let directory = std::env::temp_dir().join(format!(
            "collab-cursor-probe-{:016x}",
            rand::random::<u64>()
        ));
        std::fs::create_dir(&directory).unwrap();
        let executable = directory.join("agent-fixture");
        std::fs::write(
            &executable,
            "#!/bin/sh\ncase \"$1\" in\n status)\n  if [ \"$2\" = --format ] && [ \"$3\" = json ]; then\n    printf '{\"loggedIn\":true,\"authMethod\":\"test\"}\\n'\n    exit 0\n  fi\n  exit 2\n  ;;\n esac\n exit 1\n",
        )
        .unwrap();
        std::fs::set_permissions(&executable, std::fs::Permissions::from_mode(0o700)).unwrap();
        let settings = config::Health {
            timeout_seconds: 2,
            ..Default::default()
        };
        assert!(probe_with(
            &executable,
            "cursor",
            &config::Profile {
                codex_profile: String::new(),
                model: None
            },
            &settings,
            &std::env::vars().collect()
        )
        .is_ok());
        let logged_out = directory.join("agent-logged-out");
        std::fs::write(&logged_out, "#!/bin/sh\nprintf '{\"loggedIn\":false}\\n'\n").unwrap();
        std::fs::set_permissions(&logged_out, std::fs::Permissions::from_mode(0o700)).unwrap();
        assert!(probe_with(
            &logged_out,
            "cursor",
            &config::Profile {
                codex_profile: String::new(),
                model: None
            },
            &settings,
            &std::env::vars().collect()
        )
        .is_err());
        let slow = directory.join("agent-slow");
        std::fs::write(&slow, "#!/bin/sh\nexec sleep 20\n").unwrap();
        std::fs::set_permissions(&slow, std::fs::Permissions::from_mode(0o700)).unwrap();
        let start = Instant::now();
        let err = probe_with(
            &slow,
            "cursor",
            &config::Profile {
                codex_profile: String::new(),
                model: None,
            },
            &config::Health {
                timeout_seconds: 1,
                ..Default::default()
            },
            &std::env::vars().collect(),
        )
        .unwrap_err()
        .to_string();
        assert!(err.contains("timed out"), "{err}");
        assert!(start.elapsed() < Duration::from_secs(3));
        let mcp = std::path::Path::new("/tmp/collab-mcp");
        let (exe, args) = launch_args(
            "cursor",
            &config::Profile {
                codex_profile: String::new(),
                model: Some("test-model".into()),
            },
            std::path::Path::new("/tmp/project"),
            "hello",
            mcp,
        )
        .unwrap();
        assert_eq!(exe, "agent");
        assert!(args
            .windows(2)
            .any(|w| w == ["--workspace", "/tmp/project"]));
        for flag in ["--yolo", "--trust", "--approve-mcps"] {
            assert!(args.contains(&flag.to_string()), "{flag}");
        }
        assert!(args.windows(2).any(|w| w == ["--sandbox", "disabled"]));
        assert!(args.windows(2).any(|w| w == ["--model", "test-model"]));
        assert!(!args
            .iter()
            .any(|a| a == "--worktree" || a == "persist" || a == "-c"));
        assert_eq!(args.last().unwrap(), "hello");
        let (_, default_args) = launch_args(
            "cursor",
            &config::Profile {
                codex_profile: String::new(),
                model: None,
            },
            std::path::Path::new("/tmp/project"),
            "hello",
            mcp,
        )
        .unwrap();
        assert!(default_args.windows(2).any(|w| w == ["--model", "auto"]));
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
        assert_eq!(args[..2], ["--profile", "oauth"]);
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
            session: None,
            pane: None,
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
        std::fs::remove_dir_all(directory).unwrap();
    }
}
