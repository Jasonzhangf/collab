use serde_json::{json, Value};
use std::io::{self, BufRead, Write};
use std::process::Command;

fn tool(name: &str, description: &str, properties: Value, required: &[&str]) -> Value {
    json!({
        "name": name,
        "description": description,
        "inputSchema": {"type":"object", "properties": properties, "required": required}
    })
}

fn tools() -> Value {
    json!([
        tool(
            "collab_init",
            "Initialize/register this live project identity.",
            json!({}),
            &[]
        ),
        tool(
            "collab_whoami",
            "Return the authenticated Collab identity.",
            json!({}),
            &[]
        ),
        tool(
            "collab_who",
            "List registered workers and active tasks.",
            json!({}),
            &[]
        ),
        tool(
            "collab_sendmessage",
            "Persist an explicit peer notification with a required short subject and original body preview; the recipient is woken only through its own active direct-message subscription.",
            json!({"to":{"type":"string"},"subject":{"type":"string"},"body":{"type":"string"}}),
            &["to", "subject", "body"]
        ),
        tool(
            "collab_notify_methods",
            "List supported opt-in notification methods and event types.",
            json!({}),
            &[]
        ),
        tool(
            "collab_notify_subscribe",
            "Register one finite notification subscription owned by the calling Agent; direct-message is reusable until expiry and other event subscriptions are one-shot.",
            json!({"event":{"type":"string","enum":["direct-message","resource-released","deadline","async-result"]},"subject":{"type":"string"},"trigger_ms":{"type":"integer"},"ttl_seconds":{"type":"integer","minimum":1}}),
            &["event", "ttl_seconds"]
        ),
        tool(
            "collab_notify_status",
            "List the calling Agent's notification subscriptions.",
            json!({}),
            &[]
        ),
        tool(
            "collab_notify_unsubscribe",
            "Cancel one calling-Agent-owned notification subscription.",
            json!({"subscription_id":{"type":"string"}}),
            &["subscription_id"]
        ),
        tool(
            "collab_task_status",
            "Read the authoritative task registry.",
            json!({"id":{"type":"string"}}),
            &[]
        ),
        tool(
            "collab_task_register",
            "Register a task owned by the calling peer; /goal delegation is deferred.",
            json!({"id":{"type":"string"},"feature":{"type":"string"},"worktree":{"type":"string"},"branch":{"type":"string"},"base_commit":{"type":"string"},"priority":{"type":"string"},"next":{"type":"string"}}),
            &["id"]
        ),
        tool(
            "collab_task_wait",
            "Record a bounded resource wait against the blocking task owner.",
            json!({"id":{"type":"string"},"blocking_task":{"type":"string"}}),
            &["id", "blocking_task"]
        ),
        tool(
            "collab_task_deliver",
            "Deliver a claimed task through the Server.",
            json!({"id":{"type":"string"},"evidence":{"type":"string"},"worktree":{"type":"string"}}),
            &["id", "evidence", "worktree"]
        ),
        tool(
            "collab_task_relocate",
            "Relocate the calling peer's task to a short ./playground worktree.",
            json!({"id":{"type":"string"},"worktree":{"type":"string"},"branch":{"type":"string"},"base_commit":{"type":"string"}}),
            &["id", "worktree"]
        ),
        tool(
            "collab_task_block",
            "Mark an owned task blocked without notifying unrelated peers.",
            json!({"id":{"type":"string"},"next":{"type":"string"}}),
            &["id"]
        ),
        tool(
            "collab_task_update",
            "Update an authorized task state through the Server.",
            json!({"id":{"type":"string"},"status":{"type":"string"},"next":{"type":"string"}}),
            &["id"]
        ),
        tool(
            "collab_task_close",
            "Close the owner's merged task and safely clean its declared resources.",
            json!({"id":{"type":"string"}}),
            &["id"]
        ),
        tool(
            "collab_migrate",
            "Run peer-authorized migration inspect, plan, apply, or verify.",
            json!({"action":{"type":"string","enum":["inspect","plan","apply","verify"]}}),
            &["action"]
        ),
        tool(
            "collab_inbox",
            "Read the durable Collab inbox.",
            json!({}),
            &[]
        ),
        tool(
            "collab_context",
            "Return one read-only authoritative snapshot after a notification or restart.",
            json!({}),
            &[]
        ),
        tool(
            "collab_ack",
            "Acknowledge owned mailbox messages.",
            json!({"ids":{"type":"array","items":{"type":"string"}}}),
            &["ids"]
        )
    ])
}

fn collab_bin() -> std::path::PathBuf {
    if let Ok(path) = std::env::var("COLLAB_BIN") {
        return path.into();
    }
    std::env::current_exe()
        .ok()
        .and_then(|p| p.parent().map(|d| d.join("collab")))
        .unwrap_or_else(|| "collab".into())
}

fn call(name: &str, args: &Value) -> Result<String, String> {
    let mut argv = Vec::<String>::new();
    match name {
        "collab_init" => argv.push("init".into()),
        "collab_whoami" => argv.push("whoami".into()),
        "collab_who" => argv.push("who".into()),
        "collab_sendmessage" => {
            argv.extend([
                "sendmessage".into(),
                "--to".into(),
                required(args, "to")?,
                "--subject".into(),
                required(args, "subject")?,
                required(args, "body")?,
            ]);
        }
        "collab_notify_methods" => argv.extend(["notify".into(), "methods".into()]),
        "collab_notify_subscribe" => {
            argv.extend([
                "notify".into(),
                "subscribe".into(),
                "--event".into(),
                required(args, "event")?,
            ]);
            optional_flag(&mut argv, args, "subject", "--subject")?;
            optional_integer_flag(&mut argv, args, "trigger_ms", "--trigger-ms")?;
            optional_integer_flag(&mut argv, args, "ttl_seconds", "--ttl-seconds")?;
        }
        "collab_notify_status" => argv.extend(["notify".into(), "status".into()]),
        "collab_notify_unsubscribe" => argv.extend([
            "notify".into(),
            "unsubscribe".into(),
            required(args, "subscription_id")?,
        ]),
        "collab_inbox" => argv.push("inbox".into()),
        "collab_context" => argv.push("context".into()),
        "collab_ack" => {
            argv.push("ack".into());
            for id in args
                .get("ids")
                .and_then(Value::as_array)
                .ok_or("ids must be an array")?
            {
                argv.push(id.as_str().ok_or("ids must contain strings")?.into());
            }
        }
        "collab_task_status" => {
            argv.extend(["task".into(), "status".into()]);
            if let Some(id) = args.get("id").and_then(Value::as_str) {
                argv.push(id.into());
            }
        }
        "collab_task_register" => {
            argv.extend(["task".into(), "register".into(), required(args, "id")?]);
            optional_flag(&mut argv, args, "feature", "--feature")?;
            optional_flag(&mut argv, args, "worktree", "--worktree")?;
            optional_flag(&mut argv, args, "branch", "--branch")?;
            optional_flag(&mut argv, args, "base_commit", "--base-commit")?;
            optional_flag(&mut argv, args, "priority", "--priority")?;
            optional_flag(&mut argv, args, "next", "--next")?;
        }
        "collab_task_wait" => {
            argv.extend(["task".into(), "wait".into(), required(args, "id")?]);
            argv.extend(["--for".into(), required(args, "blocking_task")?]);
        }
        "collab_task_deliver" => {
            argv.extend(["task".into(), "deliver".into(), required(args, "id")?]);
            argv.extend(["--evidence".into(), required(args, "evidence")?]);
            argv.extend(["--worktree".into(), required(args, "worktree")?]);
        }
        "collab_task_relocate" => {
            argv.extend(["task".into(), "relocate".into(), required(args, "id")?]);
            argv.extend(["--worktree".into(), required(args, "worktree")?]);
            optional_flag(&mut argv, args, "branch", "--branch")?;
            optional_flag(&mut argv, args, "base_commit", "--base-commit")?;
        }
        "collab_task_block" => {
            argv.extend(["task".into(), "block".into(), required(args, "id")?]);
            optional_flag(&mut argv, args, "next", "--next")?;
        }
        "collab_task_update" => {
            argv.extend(["task".into(), "update".into(), required(args, "id")?]);
            optional_flag(&mut argv, args, "status", "--status")?;
            optional_flag(&mut argv, args, "next", "--next")?;
        }
        "collab_task_close" => argv.extend(["task".into(), "close".into(), required(args, "id")?]),
        "collab_migrate" => {
            argv.extend(["migrate".into(), required(args, "action")?]);
        }
        _ => return Err(format!("unknown tool {name}")),
    }
    let mut command = Command::new(collab_bin());
    command.args(argv);
    let output = command.output().map_err(|e| e.to_string())?;
    let stdout = String::from_utf8_lossy(&output.stdout).trim().to_string();
    let stderr = String::from_utf8_lossy(&output.stderr).trim().to_string();
    if !output.status.success() {
        return Err(if stderr.is_empty() { stdout } else { stderr });
    }
    Ok(stdout)
}

fn required(args: &Value, key: &str) -> Result<String, String> {
    args.get(key)
        .and_then(Value::as_str)
        .map(str::to_owned)
        .ok_or_else(|| format!("missing required argument {key}"))
}

fn optional_flag(
    argv: &mut Vec<String>,
    args: &Value,
    key: &str,
    flag: &str,
) -> Result<(), String> {
    if let Some(value) = args.get(key) {
        argv.extend([
            flag.into(),
            value
                .as_str()
                .ok_or_else(|| format!("{key} must be a string"))?
                .into(),
        ]);
    }
    Ok(())
}

fn optional_integer_flag(
    argv: &mut Vec<String>,
    args: &Value,
    key: &str,
    flag: &str,
) -> Result<(), String> {
    if let Some(value) = args.get(key) {
        argv.extend([
            flag.into(),
            value
                .as_i64()
                .ok_or_else(|| format!("{key} must be an integer"))?
                .to_string(),
        ]);
    }
    Ok(())
}

fn response(id: &Value, result: Value) -> Value {
    json!({"jsonrpc":"2.0", "id":id, "result":result})
}

fn main() {
    let stdin = io::stdin();
    let mut out = io::stdout();
    for line in stdin.lock().lines() {
        let Ok(line) = line else { break };
        let Ok(req) = serde_json::from_str::<Value>(&line) else {
            continue;
        };
        let id = req.get("id").cloned().unwrap_or(Value::Null);
        let method = req.get("method").and_then(Value::as_str).unwrap_or("");
        let result = match method {
            "initialize" => response(
                &id,
                json!({"protocolVersion":"2024-11-05","capabilities":{"tools":{}},"serverInfo":{"name":"collab","version":env!("CARGO_PKG_VERSION")}}),
            ),
            "notifications/initialized" => continue,
            "ping" => response(&id, json!({})),
            "tools/list" => response(&id, json!({"tools":tools()})),
            "tools/call" => {
                let params = req.get("params").cloned().unwrap_or_default();
                let name = params.get("name").and_then(Value::as_str).unwrap_or("");
                let args = params
                    .get("arguments")
                    .cloned()
                    .unwrap_or_else(|| json!({}));
                match call(name, &args) {
                    Ok(text) => response(
                        &id,
                        json!({"content":[{"type":"text","text":text}],"isError":false}),
                    ),
                    Err(error) => response(
                        &id,
                        json!({"content":[{"type":"text","text":error}],"isError":true}),
                    ),
                }
            }
            _ => {
                json!({"jsonrpc":"2.0","id":id,"error":{"code":-32601,"message":format!("method not found: {method}")}})
            }
        };
        let _ = writeln!(out, "{}", result);
        let _ = out.flush();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sendmessage_schema_requires_subject_and_body() {
        let definitions = tools();
        let send = definitions
            .as_array()
            .unwrap()
            .iter()
            .find(|tool| tool["name"] == "collab_sendmessage")
            .unwrap();
        assert_eq!(
            send["inputSchema"]["required"],
            json!(["to", "subject", "body"])
        );
        assert!(send["inputSchema"]["properties"]["subject"].is_object());
    }
}
