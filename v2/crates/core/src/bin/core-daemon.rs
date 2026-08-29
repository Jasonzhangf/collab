use collab_v2_core::{CoreError, Identity, TaskState};
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
    CreateTask {
        actor: String,
        task_id: String,
    },
    Claim {
        actor: String,
        task_id: String,
    },
    Transition {
        actor: String,
        task_id: String,
        state: TaskState,
    },
    Snapshot,
}

fn error_value(error: CoreError) -> Value {
    json!({"ok": false, "error": format!("{error:?}")})
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
        let response = match serde_json::from_str::<Command>(&line) {
            Ok(Command::Register { identity }) => {
                state.register(identity).map(|_| json!({"ok": true}))
            }
            Ok(Command::CreateTask { actor, task_id }) => state
                .create_task(&actor, task_id)
                .map(|_| json!({"ok": true})),
            Ok(Command::Claim { actor, task_id }) => {
                state.claim(&actor, &task_id).map(|_| json!({"ok": true}))
            }
            Ok(Command::Transition {
                actor,
                task_id,
                state: next,
            }) => state
                .transition(&actor, &task_id, next)
                .map(|_| json!({"ok": true})),
            Ok(Command::Snapshot) => Ok(json!({"ok": true, "state": state})),
            Err(error) => Err(CoreError::InvalidTransition)
                .map(|_: ()| json!({"ok": true, "parse_error": error.to_string()})),
        }
        .unwrap_or_else(error_value);
        if let Some(path) = state_path.as_ref() {
            if let Ok(raw) = serde_json::to_string(&state) {
                let _ = std::fs::write(path, raw);
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
