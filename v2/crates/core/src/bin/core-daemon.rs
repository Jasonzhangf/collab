use collab_v2_core::{
    AgentState, CoreError, Identity, NotificationEvent, ResourceNotice, TaskState,
};
use serde::Deserialize;
use serde_json::{json, Value};
use std::io::{self, BufRead, Write};
use std::path::PathBuf;

#[derive(Deserialize)]
#[serde(tag = "op", rename_all = "snake_case")]
enum Command {
    Register {
        identity: Identity,
    },
    RegisterTask {
        actor: String,
        task_id: String,
        feature_id: String,
        resource_id: String,
    },
    TransitionTask {
        actor: String,
        task_id: String,
        state: TaskState,
    },
    WaitTask {
        actor: String,
        task_id: String,
        blocking_task_id: String,
        deadline_ms: u64,
        now_ms: u64,
    },
    Subscribe {
        owner: String,
        subscription_id: String,
        event: NotificationEvent,
        subject: Option<String>,
        expires_at_ms: u64,
        now_ms: u64,
    },
    SendResourceNotice {
        message_id: String,
        from: String,
        to: String,
        notice: ResourceNotice,
        subject: String,
    },
    WakeAttempt {
        message_id: String,
        agent_state: AgentState,
        succeeded: bool,
        now_ms: u64,
    },
    Snapshot,
}

fn error_value(error: CoreError) -> Value {
    json!({"ok": false, "error": format!("{error:?}")})
}

fn persist_state(path: Option<&PathBuf>, state: &collab_v2_core::CoreState) -> Result<(), String> {
    let Some(path) = path else {
        return Ok(());
    };
    let raw = serde_json::to_string(state).map_err(|error| error.to_string())?;
    std::fs::write(path, raw).map_err(|error| error.to_string())
}

fn main() {
    let state_path = std::env::args()
        .position(|arg| arg == "--state")
        .and_then(|index| std::env::args().nth(index + 1))
        .map(PathBuf::from);
    let stdin = io::stdin();
    let mut state: collab_v2_core::CoreState = state_path
        .as_ref()
        .and_then(|path| std::fs::read_to_string(path).ok())
        .and_then(|raw| serde_json::from_str(&raw).ok())
        .unwrap_or_default();
    for line in stdin.lock().lines() {
        let line = match line {
            Ok(line) => line,
            Err(error) => {
                eprintln!("read error: {error}");
                break;
            }
        };
        let mut response = match serde_json::from_str::<Command>(&line) {
            Ok(Command::Register { identity }) => {
                state.register(identity).map(|_| json!({"ok": true}))
            }
            Ok(Command::RegisterTask {
                actor,
                task_id,
                feature_id,
                resource_id,
            }) => state
                .register_task(&actor, task_id, feature_id, resource_id)
                .map(|_| json!({"ok": true})),
            Ok(Command::TransitionTask {
                actor,
                task_id,
                state: next,
            }) => state
                .transition_task(&actor, &task_id, next)
                .map(|_| json!({"ok": true})),
            Ok(Command::WaitTask {
                actor,
                task_id,
                blocking_task_id,
                deadline_ms,
                now_ms,
            }) => state
                .wait_task(&actor, &task_id, &blocking_task_id, deadline_ms, now_ms)
                .map(|_| json!({"ok": true})),
            Ok(Command::Subscribe {
                owner,
                subscription_id,
                event,
                subject,
                expires_at_ms,
                now_ms,
            }) => state
                .subscribe(
                    &owner,
                    subscription_id,
                    event,
                    subject,
                    expires_at_ms,
                    now_ms,
                )
                .map(|_| json!({"ok": true})),
            Ok(Command::SendResourceNotice {
                message_id,
                from,
                to,
                notice,
                subject,
            }) => state
                .send_resource_notice(message_id, &from, &to, notice, &subject)
                .map(|_| json!({"ok": true})),
            Ok(Command::WakeAttempt {
                message_id,
                agent_state,
                succeeded,
                now_ms,
            }) => state
                .record_wake_attempt(&message_id, agent_state, succeeded, now_ms)
                .map(|disposition| json!({"ok": true, "disposition": disposition})),
            Ok(Command::Snapshot) => Ok(json!({"ok": true, "state": state})),
            Err(error) => {
                Ok(json!({"ok": false, "error": "InvalidCommand", "message": error.to_string()}))
            }
        }
        .unwrap_or_else(error_value);
        if response.get("ok") == Some(&Value::Bool(true)) {
            if let Err(error) = persist_state(state_path.as_ref(), &state) {
                response = json!({"ok": false, "error": "PersistenceFailed", "message": error});
            }
        }
        let mut stdout = io::stdout().lock();
        if serde_json::to_writer(&mut stdout, &response).is_err() {
            break;
        }
        if stdout.write_all(b"\n").is_err() {
            break;
        }
        if stdout.flush().is_err() {
            break;
        }
    }
}
