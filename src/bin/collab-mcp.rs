use serde_json::{json, Value};
use std::io::{self, BufRead, Read, Write};
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
        tool("collab_msg", "Read a durable notification by ID.", json!({"id":{"type":"string"}}), &["id"]),
        tool("collab_subagent", "Parent manages children; child uses ready/working and sends results via collab_sendmessage. status includes mailbox, keepalive and notification history. snapshot is explicit screen-tail read only, not a health probe. Cursor health is official `agent status --format json`. Non-tmux observers get no push channel and must check status/mailbox themselves. rearm requires an explicit operator request after exhaustion. start accepts optional runtime=cursor|codex to override ~/.appsdk/config.toml. dispatch assigns a real task through the live master scheduler and is idempotent by request_id.", json!({"action":{"type":"string","enum":["start","dispatch","list","status","snapshot","rearm","send","ready","working","close"]},"id":{"type":"string"},"request_id":{"type":"string"},"runtime":{"type":"string","enum":["cursor","codex"]},"lines":{"type":"integer","minimum":1,"maximum":200},"subject":{"type":"string"},"body":{"type":"string"},"feature_id":{"type":"string"},"worktree_path":{"type":"string"},"branch":{"type":"string"},"base_commit":{"type":"string"},"priority":{"type":"string","enum":["p0","p1","p2","p3","p4"]},"next_step":{"type":"string"}}), &["action"]),
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
            "Register one owner-scoped finite subscription; deadline uses either absolute at_ms values or a periodic interval, master-idle uses a recurring 15- or 60-minute interval for the live master, and at most three active subscriptions are allowed per Agent.",
            json!({"event":{"type":"string","enum":["direct-message","resource-released","deadline","async-result","master-idle"]},"subject":{"type":"string"},"at_ms":{"type":"array","items":{"type":"integer"}},"every_ms":{"type":"integer","minimum":1},"trigger_ms":{"type":"integer"},"repeat_count":{"type":"integer","minimum":1,"maximum":100},"ttl_seconds":{"type":"integer","minimum":1}}),
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
            "collab_task_accept",
            "Owner-authenticated acceptance of an assigned scheduler task; atomically records assigned to working.",
            json!({"id":{"type":"string"}}),
            &["id"]
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
            "collab_task_review",
            "Accept a delivered task or return it for rework with durable evidence.",
            json!({"id":{"type":"string"},"accept":{"type":"boolean"},"rework":{"type":"boolean"},"evidence":{"type":"string"}}),
            &["id", "evidence"]
        ),
        tool(
            "collab_task_integrated",
            "Record exact integration of an accepted task on refs/heads/main.",
            json!({"id":{"type":"string"},"commit":{"type":"string"},"evidence":{"type":"string"}}),
            &["id", "commit", "evidence"]
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
        ),
        tool(
            "collab_master",
            "Inspect the live Collab master, self-promote after explicit user approval when no live master exists, or delegate as the current live master. Codex/Cursor root is unrelated. Init and register never create master. Independent peers may decline a master collaboration invite; managed subagents must obey the master.",
            json!({"action":{"type":"string","enum":["status","promote","delegate"]},"approval":{"type":"string"},"target":{"type":"string"}}),
            &["action"]
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
        "collab_msg" => argv.extend(["msg".into(), required(args, "id")?]),
        "collab_subagent" => {
            let action = required(args, "action")?;
            if ![
                "start", "dispatch", "list", "status", "snapshot", "rearm", "send", "ready",
                "working", "close",
            ]
            .contains(&action.as_str())
            {
                return Err("invalid subagent action".into());
            }
            argv.extend(["subagent".into(), action.clone()]);
            if action == "start" {
                optional_flag(&mut argv, args, "id", "--id")?;
                optional_flag(&mut argv, args, "runtime", "--runtime")?;
            } else if action == "dispatch" {
                argv.extend([
                    "--request-id".into(),
                    required(args, "request_id")?,
                    "--subject".into(),
                    required(args, "subject")?,
                    required(args, "body")?,
                ]);
                optional_flag(&mut argv, args, "feature_id", "--feature-id")?;
                optional_flag(&mut argv, args, "worktree_path", "--worktree-path")?;
                optional_flag(&mut argv, args, "branch", "--branch")?;
                optional_flag(&mut argv, args, "base_commit", "--base-commit")?;
                optional_flag(&mut argv, args, "priority", "--priority")?;
                optional_flag(&mut argv, args, "next_step", "--next-step")?;
            } else if action != "list" {
                argv.push(required(args, "id")?);
            }
            if action == "send" {
                argv.extend([
                    "--subject".into(),
                    required(args, "subject")?,
                    required(args, "body")?,
                ]);
            }
            if action == "snapshot" {
                optional_integer_flag(&mut argv, args, "lines", "--lines")?;
            }
        }
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
            if let Some(values) = args.get("at_ms").and_then(Value::as_array) { for value in values { argv.extend(["--at-ms".into(), value.as_i64().ok_or("at_ms must contain integers")?.to_string()]); } }
            optional_integer_flag(&mut argv, args, "every_ms", "--every-ms")?;
            optional_integer_flag(&mut argv, args, "repeat_count", "--repeat-count")?;
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
        "collab_task_accept" => {
            argv.extend(["task".into(), "accept".into(), required(args, "id")?]);
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
        "collab_task_review" => {
            argv.extend(["task".into(), "review".into(), required(args, "id")?]);
            if args.get("accept").and_then(Value::as_bool).unwrap_or(false) {
                argv.push("--accept".into());
            }
            if args.get("rework").and_then(Value::as_bool).unwrap_or(false) {
                argv.push("--rework".into());
            }
            argv.extend(["--evidence".into(), required(args, "evidence")?]);
        }
        "collab_task_integrated" => {
            argv.extend(["task".into(), "integrated".into(), required(args, "id")?]);
            argv.extend(["--commit".into(), required(args, "commit")?]);
            argv.extend(["--evidence".into(), required(args, "evidence")?]);
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
        "collab_master" => {
            let action = required(args, "action")?;
            match action.as_str() {
                "status" => argv.extend(["master".into(), "status".into()]),
                "promote" => argv.extend([
                    "master".into(),
                    "promote".into(),
                    "--approval".into(),
                    required(args, "approval")?,
                ]),
                "delegate" => argv.extend([
                    "master".into(),
                    "delegate".into(),
                    required(args, "target")?,
                ]),
                _ => return Err("invalid master action".into()),
            }
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

#[derive(Clone, Copy)]
enum Frame {
    Line,
    ContentLength,
}

fn read_message(stdin: &mut impl BufRead) -> io::Result<Option<(Frame, Value)>> {
    let mut first = String::new();
    if stdin.read_line(&mut first)? == 0 {
        return Ok(None);
    }
    let header = first.trim_end_matches(['\r', '\n']);
    if header.is_empty() {
        return read_message(stdin);
    }
    if header.to_ascii_lowercase().starts_with("content-length:") {
        let Some(length) = header
            .split_once(':')
            .and_then(|(_, value)| value.trim().parse::<usize>().ok())
        else {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "invalid Content-Length",
            ));
        };
        loop {
            let mut next = String::new();
            if stdin.read_line(&mut next)? == 0 {
                break;
            }
            if next == "\n" || next == "\r\n" {
                break;
            }
        }
        let mut body = vec![0; length];
        Read::read_exact(stdin, &mut body)?;
        let req = serde_json::from_slice(&body).map_err(io::Error::other)?;
        return Ok(Some((Frame::ContentLength, req)));
    }
    let req = serde_json::from_str(header).map_err(io::Error::other)?;
    Ok(Some((Frame::Line, req)))
}

fn write_message(out: &mut impl Write, frame: Frame, message: &Value) -> io::Result<()> {
    let body = serde_json::to_string(message)?;
    match frame {
        Frame::Line => writeln!(out, "{body}")?,
        Frame::ContentLength => write!(out, "Content-Length: {}\r\n\r\n{body}", body.len())?,
    }
    out.flush()
}

fn handle(req: &Value) -> Option<Value> {
    let method = req.get("method").and_then(Value::as_str).unwrap_or("");
    if method.starts_with("notifications/") {
        return None;
    }
    let id = req.get("id").cloned().unwrap_or(Value::Null);
    Some(match method {
        "initialize" => {
            let version = req
                .pointer("/params/protocolVersion")
                .and_then(Value::as_str)
                .unwrap_or("2024-11-05");
            response(
                &id,
                json!({"protocolVersion":version,"capabilities":{"tools":{"listChanged":false}},"serverInfo":{"name":"collab","version":env!("CARGO_PKG_VERSION")}}),
            )
        }
        "ping" => response(&id, json!({})),
        "tools/list" => response(&id, json!({"tools":tools()})),
        "resources/list" => response(&id, json!({"resources":[]})),
        "prompts/list" => response(&id, json!({"prompts":[]})),
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
    })
}

fn main() {
    let mut stdin = io::stdin().lock();
    let mut out = io::stdout();
    while let Ok(Some((frame, req))) = read_message(&mut stdin) {
        if let Some(reply) = handle(&req) {
            let _ = write_message(&mut out, frame, &reply);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn content_length_and_newline_frames_round_trip() {
        let body = json!({"jsonrpc":"2.0","id":1,"method":"initialize","params":{"protocolVersion":"2025-03-26"}});
        let encoded = serde_json::to_string(&body).unwrap();
        let framed = format!("Content-Length: {}\r\n\r\n{encoded}", encoded.len());
        let (frame, req) = read_message(&mut framed.as_bytes()).unwrap().unwrap();
        assert!(matches!(frame, Frame::ContentLength));
        assert_eq!(
            handle(&req).unwrap()["result"]["protocolVersion"],
            "2025-03-26"
        );
        let line = format!("{encoded}\n");
        let (frame, req) = read_message(&mut line.as_bytes()).unwrap().unwrap();
        assert!(matches!(frame, Frame::Line));
        assert_eq!(
            handle(&req).unwrap()["result"]["serverInfo"]["name"],
            "collab"
        );
        assert_eq!(
            handle(&json!({"method":"resources/list","id":2})).unwrap()["result"]["resources"],
            json!([])
        );
    }

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

    #[test]
    fn subagent_dispatch_schema_exposes_stable_request_and_task_fields() {
        let definitions = tools();
        let subagent = definitions
            .as_array()
            .unwrap()
            .iter()
            .find(|tool| tool["name"] == "collab_subagent")
            .unwrap();
        assert!(subagent["inputSchema"]["properties"]["action"]["enum"]
            .as_array()
            .unwrap()
            .contains(&json!("dispatch")));
        let properties = subagent["inputSchema"]["properties"].as_object().unwrap();
        for field in ["request_id", "subject", "body", "feature_id", "priority"] {
            assert!(properties.contains_key(field), "missing MCP field {field}");
        }
    }

    #[test]
    fn task_accept_schema_requires_owner_task_id() {
        let definitions = tools();
        let accept = definitions
            .as_array()
            .unwrap()
            .iter()
            .find(|tool| tool["name"] == "collab_task_accept")
            .unwrap();
        assert_eq!(accept["inputSchema"]["required"], json!(["id"]));
    }

    #[test]
    fn master_idle_event_is_public_in_mcp_schema() {
        let definitions = tools();
        let subscribe = definitions
            .as_array()
            .unwrap()
            .iter()
            .find(|tool| tool["name"] == "collab_notify_subscribe")
            .unwrap();
        assert!(subscribe["inputSchema"]["properties"]["event"]["enum"]
            .as_array()
            .unwrap()
            .contains(&json!("master-idle")));
    }

    #[test]
    fn notification_schedule_fields_match_mcp_call_arguments() {
        let definitions = tools();
        let subscribe = definitions
            .as_array()
            .unwrap()
            .iter()
            .find(|tool| tool["name"] == "collab_notify_subscribe")
            .unwrap();
        let properties = subscribe["inputSchema"]["properties"].as_object().unwrap();
        for field in [
            "at_ms",
            "every_ms",
            "trigger_ms",
            "repeat_count",
            "ttl_seconds",
        ] {
            assert!(properties.contains_key(field), "missing MCP field {field}");
        }
    }

    #[test]
    fn master_tool_exposes_status_promote_and_delegate() {
        let definitions = tools();
        let master = definitions
            .as_array()
            .unwrap()
            .iter()
            .find(|tool| tool["name"] == "collab_master")
            .unwrap();
        assert_eq!(master["inputSchema"]["required"], json!(["action"]));
        assert_eq!(
            master["inputSchema"]["properties"]["action"]["enum"],
            json!(["status", "promote", "delegate"])
        );
    }

    #[test]
    fn lifecycle_review_and_integration_tools_are_exposed() {
        let definitions = tools();
        for (name, required) in [
            ("collab_task_review", json!(["id", "evidence"])),
            (
                "collab_task_integrated",
                json!(["id", "commit", "evidence"]),
            ),
        ] {
            let tool = definitions
                .as_array()
                .unwrap()
                .iter()
                .find(|tool| tool["name"] == name)
                .unwrap_or_else(|| panic!("missing MCP tool {name}"));
            assert_eq!(tool["inputSchema"]["required"], required);
        }
    }
}
