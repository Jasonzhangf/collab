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
    },
    List,
    Status {
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
}
fn valid_id(id: &str) -> bool {
    !id.is_empty()
        && id.len() <= 80
        && id
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_')
}
fn probe_with(
    executable: &std::path::Path,
    profile: &config::Profile,
    settings: &config::Health,
    environment: &std::collections::BTreeMap<String, String>,
) -> Result<()> {
    let directory =
        std::env::temp_dir().join(format!("appsdk-probe-{:016x}", rand::random::<u64>()));
    std::fs::create_dir(&directory)?;
    let result = (|| {
        let output = directory.join("result.txt");
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
            .arg(format!(
                "Connectivity probe only. Do not use tools or read files. Reply exactly: {}",
                settings.expected_response
            ))
            .current_dir(&directory)
            .env_remove("TMUX_PANE")
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null());
        if let Some(model) = &profile.model {
            command.args(["--model", model]);
        }
        use std::os::unix::process::CommandExt;
        command.process_group(0);
        let mut child = command.spawn().context("cannot start codex health probe")?;
        let started = Instant::now();
        loop {
            if let Some(status) = child.try_wait()? {
                if !status.success() {
                    bail!("probe exited {status}");
                }
                let body = std::fs::read_to_string(output)?;
                if body.trim() != settings.expected_response {
                    bail!("probe response did not match expected response");
                }
                return Ok(());
            }
            if started.elapsed() >= Duration::from_secs(settings.timeout_seconds) {
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
) -> Result<serde_json::Value> {
    let response = crate::server::handle_send(
        server,
        from.into(),
        to.into(),
        "notify".into(),
        Some(subject.into()),
        body,
        None,
        "immediate".into(),
    );
    if !response.ok {
        bail!("{}", response.error.unwrap_or_default());
    }
    Ok(response.data)
}
#[derive(Serialize, Deserialize)]
struct LaunchSpec {
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
    let mut command = Command::new("codex");
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
    let mut errors = Vec::new();
    for name in &settings.profile_priority {
        let profile = &settings.profiles[name];
        match probe_with(
            std::path::Path::new("codex"),
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
    let prompt = format!("You are a persistent AppSDK subagent. Your managed ID is {}. Your parent peer is {}. First run collab init in this inherited project cwd, then collab subagent ready {}. Do not claim ready until registration succeeds. Wait quietly for Collab messages. When assigned a task, read it with collab msg, run collab subagent working {}, and use the project's task/worktree workflow. Preserve others' files; code changes require your own worktree. Report progress through collab task records and send results to the parent with collab sendmessage --to {} --subject <topic> <body>. After completing a task run collab subagent ready {} and remain available. Do not close this session automatically, repeatedly poll, send ACK loops, or create other subagents without a user request.", record.id, record.parent, record.id, record.id, record.parent, record.id);
    let mut args = vec!["--profile".to_string(), profile.codex_profile.clone()];
    if let Some(model) = &profile.model {
        args.extend(["--model".into(), model.clone()]);
    }
    let mcp = std::env::current_exe()?.with_file_name("collab-mcp");
    if !mcp.is_file() {
        bail!("collab-mcp is missing beside the managed collab executable");
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
    // This managed bridge may perform the explicitly authorized peer/task
    // lifecycle without repeated prompts. Other MCP servers retain user policy.
    for tool in [
        "collab_init",
        "collab_subagent",
        "collab_msg",
        "collab_inbox",
        "collab_context",
        "collab_sendmessage",
        "collab_notify_status",
        "collab_task_status",
        "collab_task_register",
        "collab_task_update",
        "collab_task_block",
        "collab_task_deliver",
        "collab_task_close",
    ] {
        args.extend([
            "-c".into(),
            format!("mcp_servers.appsdk-subagent.tools.{tool}.approval_mode=\"approve\""),
        ]);
    }
    args.push(format!("{prompt}\nUse the appsdk-subagent MCP server for Collab operations, NOT sandboxed shell commands. First call collab_init, then collab_subagent with action=ready and id={}. Accept a task using action=working. Read messages using collab_msg or collab_inbox. These MCP operations inherit the live tmux environment and do not require shell access to its socket.", record.id));
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
    file.write_all(&serde_json::to_vec(&LaunchSpec {
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
    if let Action::Start { id } = action {
        let config = config::load(&server.root)?;
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
        };
        {
            let mut state = server.state.lock().unwrap();
            if let Some(existing) = state.subagents.get(&id) {
                if existing.parent != actor {
                    bail!("subagent belongs to another parent");
                }
                return Ok(json!({"subagent": existing, "reused": true}));
            }
            server.commit_locked(
                &mut state,
                &[Event::SubagentUpdated {
                    subagent: record.clone(),
                }],
            );
        }
        if let Err(e) = launch(server, &mut record, &config.subagent, environment) {
            record.status = "failed".into();
            record.error = Some(e.to_string());
        }
        server.commit(&[Event::SubagentUpdated {
            subagent: record.clone(),
        }]);
        return Ok(json!({"subagent": record, "retry_allowed": false}));
    }
    if matches!(action, Action::List) {
        let state = server.state.lock().unwrap();
        return Ok(
            json!({"subagents": state.subagents.values().filter(|s| s.parent == actor).collect::<Vec<_>>() }),
        );
    }
    let id = match &action {
        Action::Status { id }
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
    } else if record.parent != actor {
        bail!("only the creating parent may manage this subagent");
    }
    match action {
        Action::Status { .. } => {
            let observed = if let Some(pane) = &record.pane {
                if crate::server::knock::pane_alive(pane) {
                    let agent = crate::server::knock::probe_agent_state(pane);
                    if agent == crate::server::knock::AgentState::Absent {
                        "agent_absent"
                    } else if agent == crate::server::knock::AgentState::Unknown {
                        "unknown"
                    } else if record.status == "starting" && now_ms() > record.ready_deadline_ms {
                        "ready_timeout"
                    } else {
                        &record.status
                    }
                } else {
                    "exited"
                }
            } else {
                &record.status
            };
            return Ok(
                json!({"subagent": record, "observed_status": observed, "tasks": state.tasks.values().filter(|t| t.owner == record.peer).collect::<Vec<_>>() }),
            );
        }
        Action::Ready { .. } | Action::Working { .. } => {
            let ready = matches!(action, Action::Ready { .. });
            if matches!(
                record.status.as_str(),
                "closing" | "closed" | "failed" | "probing"
            ) {
                bail!("subagent is not running");
            }
            if !ready && record.status != "assigned" {
                bail!("no assigned task to accept");
            }
            if ready && record.status == "idle" {
                return Ok(json!({"subagent": record, "reused": true}));
            }
            if ready && record.status == "assigned" {
                bail!("accept the assigned task before reporting completion");
            }
            record.status = if ready { "idle" } else { "working" }.into();
            server.commit_locked(
                &mut state,
                &[Event::SubagentUpdated {
                    subagent: record.clone(),
                }],
            );
            drop(state);
            if ready {
                notify(
                    server,
                    actor,
                    &record.parent,
                    "subagent-idle",
                    format!("subagent={} is idle and available", record.id),
                )?;
            }
        }
        Action::Send { subject, body, .. } => {
            if record.status != "idle" {
                bail!("subagent is not idle; query status instead of resending");
            }
            if subject.trim().is_empty() || body.trim().is_empty() {
                bail!("subject and task body are required");
            }
            record.status = "assigned".into();
            server.commit_locked(
                &mut state,
                &[Event::SubagentUpdated {
                    subagent: record.clone(),
                }],
            );
            drop(state);
            let result = match notify(server, actor, &record.peer, &subject, body) {
                Ok(value) => value,
                Err(error) => {
                    let mut state = server.state.lock().unwrap();
                    if state.subagents[&record.id].status == "assigned" {
                        record.status = "idle".into();
                        record.error = Some(error.to_string());
                        server.commit_locked(
                            &mut state,
                            &[Event::SubagentUpdated { subagent: record }],
                        );
                    }
                    return Err(error);
                }
            };
            let mut state = server.state.lock().unwrap();
            let mut current = state.subagents[&record.id].clone();
            current.last_message = result
                .get("msg_id")
                .and_then(|v| v.as_str())
                .map(str::to_owned);
            server.commit_locked(&mut state, &[Event::SubagentUpdated { subagent: current }]);
            // Message history is the task request truth; do not create a second queue.
            return Ok(json!({"subagent_id": record.id, "message": result}));
        }
        Action::Close { .. } => {
            if record.status == "closed" {
                return Ok(json!({"subagent": record, "reused": true}));
            }
            if record.status == "probing" {
                bail!("startup probe is in progress; close after its bounded completion");
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
}
