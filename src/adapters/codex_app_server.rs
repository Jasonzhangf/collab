//! Codex App Server transport over the host-owned Unix WebSocket endpoint.
//!
//! This module speaks the native JSON-RPC surface exposed by Codex TUI and
//! Desktop. It does not start an App Server, invent a namespace, or treat
//! queue acceptance as delivery. Explicit coordination uses `turn/start`,
//! while background wakeups may use `thread/queue/add`. Every operation is
//! bounded and preserves the exact native error on failure.

use super::{AdapterCapabilities, AdapterError, EndpointKind, WakeMode};
use crate::identity::NativeThreadId;
use crate::proto::{AppServerCandidate, SelectedTransport, TransportKind};
use serde_json::{json, Value};
use std::io::{BufRead, BufReader, Read, Write};
use std::os::unix::net::UnixStream;
use std::path::{Path, PathBuf};
use std::time::Duration;

pub const APPSERVER_SOCKET_ENV: &str = "COLLAB_APPSERVER_SOCKET";
pub const APPSERVER_NAMESPACE_ENV: &str = "COLLAB_APPSERVER_NAMESPACE";
pub const APPSERVER_TIMEOUT_MS_ENV: &str = "COLLAB_APPSERVER_TIMEOUT_MS";
pub const DEFAULT_TIMEOUT_MS: u64 = 5_000;
const MAX_FRAME_BYTES: usize = 8 * 1024 * 1024;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AppServerCapabilities {
    pub discover_sessions: bool,
    pub session_status: bool,
    pub send_message: bool,
    pub read_thread: bool,
    pub wait_reply: bool,
    pub ack: bool,
}

impl AppServerCapabilities {
    pub fn native() -> Self {
        Self {
            discover_sessions: true,
            session_status: true,
            send_message: true,
            read_thread: true,
            wait_reply: true,
            ack: false,
        }
    }

    pub fn adapter(&self) -> AdapterCapabilities {
        AdapterCapabilities {
            endpoint: EndpointKind::Tui,
            submit: self.send_message,
            interrupt: false,
            wake: WakeMode::Native,
        }
    }

    pub fn names(&self) -> Vec<&'static str> {
        let mut values = Vec::new();
        if self.discover_sessions {
            values.push("discover_sessions");
        }
        if self.session_status {
            values.push("session_status");
        }
        if self.send_message {
            values.push("send_message_to_thread");
        }
        if self.read_thread {
            values.push("read_thread");
        }
        if self.wait_reply {
            values.push("wait_reply");
        }
        if self.ack {
            values.push("ack");
        }
        values
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LiveAppServer {
    socket_path: PathBuf,
    namespace: String,
    thread_id: NativeThreadId,
    timeout: Duration,
    capabilities: AppServerCapabilities,
}

impl LiveAppServer {
    pub fn detect() -> Result<Option<Self>, AdapterError> {
        let Some(thread_id) = std::env::var("CODEX_THREAD_ID")
            .ok()
            .filter(|value| !value.trim().is_empty())
        else {
            return Ok(None);
        };
        let thread_id =
            NativeThreadId::new(thread_id).map_err(|error| AdapterError::InvalidBinding {
                detail: format!("CODEX_THREAD_ID is invalid: {error}"),
            })?;
        let Some(socket_path) = socket_candidate() else {
            return Ok(None);
        };
        if !socket_path.is_absolute() || !socket_path.exists() {
            return Ok(None);
        }
        let namespace = std::env::var(APPSERVER_NAMESPACE_ENV)
            .ok()
            .filter(|value| !value.trim().is_empty())
            .unwrap_or_else(|| "codex_tui".into());
        if !matches!(namespace.as_str(), "codex_tui" | "codex_app") {
            return Err(AdapterError::Unknown {
                operation: "detect",
                detail: format!("unsupported App Server namespace {namespace}"),
            });
        }
        let timeout = match std::env::var(APPSERVER_TIMEOUT_MS_ENV) {
            Ok(value) => Duration::from_millis(value.trim().parse::<u64>().map_err(|error| {
                AdapterError::Unknown {
                    operation: "detect",
                    detail: format!(
                        "{APPSERVER_TIMEOUT_MS_ENV} must be a positive integer: {error}"
                    ),
                }
            })?),
            Err(std::env::VarError::NotPresent) => Duration::from_millis(DEFAULT_TIMEOUT_MS),
            Err(error) => {
                return Err(AdapterError::Unknown {
                    operation: "detect",
                    detail: format!("cannot read {APPSERVER_TIMEOUT_MS_ENV}: {error}"),
                })
            }
        };
        if timeout.is_zero() {
            return Err(AdapterError::Unknown {
                operation: "detect",
                detail: format!("{APPSERVER_TIMEOUT_MS_ENV} must be greater than zero"),
            });
        }
        let mut client = Client::connect(&socket_path, timeout)?;
        client.initialize()?;
        let response = client.call("thread/read", json!({"threadId": thread_id.as_str()}))?;
        let observed = response
            .pointer("/thread/id")
            .and_then(Value::as_str)
            .ok_or_else(|| AdapterError::Unknown {
                operation: "thread/read",
                detail: "response is missing thread.id".into(),
            })?;
        if observed != thread_id.as_str() {
            return Err(AdapterError::Unknown {
                operation: "thread/read",
                detail: format!(
                    "thread identity mismatch: expected {}, observed {}",
                    thread_id, observed
                ),
            });
        }
        Ok(Some(Self {
            socket_path,
            namespace,
            thread_id,
            timeout,
            capabilities: AppServerCapabilities::native(),
        }))
    }

    pub fn socket_path(&self) -> &Path {
        &self.socket_path
    }

    pub fn namespace(&self) -> &str {
        &self.namespace
    }

    pub fn thread_id(&self) -> &NativeThreadId {
        &self.thread_id
    }

    pub fn capabilities(&self) -> &AppServerCapabilities {
        &self.capabilities
    }

    pub fn send(
        &self,
        thread_id: &NativeThreadId,
        body: &str,
        client_user_message_id: &str,
    ) -> Result<Value, AdapterError> {
        if !self.capabilities.send_message {
            return Err(AdapterError::CapabilityUnavailable {
                endpoint: EndpointKind::Tui,
                operation: "send_message_to_thread",
            });
        }
        let mut client = Client::connect(&self.socket_path, self.timeout)?;
        client.initialize()?;
        client.call(
            "turn/start",
            json!({
                "threadId": thread_id.as_str(),
                "input": [{"type": "text", "text": body}],
                "clientUserMessageId": client_user_message_id,
            }),
        )
    }

    pub fn status(&self, thread_id: &NativeThreadId) -> Result<Value, AdapterError> {
        let mut client = Client::connect(&self.socket_path, self.timeout)?;
        client.initialize()?;
        client.call("thread/read", json!({"threadId": thread_id.as_str()}))
    }

    pub fn read_items(
        &self,
        thread_id: &NativeThreadId,
        cursor: Option<&str>,
    ) -> Result<Value, AdapterError> {
        let mut client = Client::connect(&self.socket_path, self.timeout)?;
        client.initialize()?;
        client.call(
            "thread/items/list",
            json!({
                "threadId": thread_id.as_str(),
                "limit": 100,
                "cursor": cursor,
                "sortDirection": "desc",
            }),
        )
    }

    pub fn as_json(&self) -> Value {
        json!({
            "selected": "appserver",
            "priority": 100,
            "endpoint": format!("unix://{}", self.socket_path.display()),
            "namespace": self.namespace,
            "thread_id": self.thread_id.as_str(),
            "capabilities": self.capabilities.names(),
            "accepted_semantics": "turn/start accepted the immediate notification; execution and reply are observed separately"
        })
    }
}

/// Collect an App Server endpoint/thread candidate from the current process
/// environment without probing it. The daemon owns candidate self-check and
/// transport selection; a client-side probe must not be able to suppress a
/// candidate that the server could otherwise validate.
pub fn candidate_from_env() -> Result<Option<AppServerCandidate>, AdapterError> {
    let Some(thread_id) = std::env::var("CODEX_THREAD_ID")
        .ok()
        .filter(|value| !value.trim().is_empty())
    else {
        return Ok(None);
    };
    NativeThreadId::new(thread_id.clone()).map_err(|error| AdapterError::InvalidBinding {
        detail: format!("CODEX_THREAD_ID is invalid: {error}"),
    })?;
    let Some(socket_path) = socket_candidate() else {
        return Ok(None);
    };
    let namespace = std::env::var(APPSERVER_NAMESPACE_ENV)
        .ok()
        .filter(|value| !value.trim().is_empty())
        .unwrap_or_else(|| "codex_tui".into());
    Ok(Some(AppServerCandidate {
        endpoint: format!("unix://{}", socket_path.display()),
        namespace,
        thread_id,
    }))
}

/// Independently verify one worker-proposed App Server endpoint. The worker
/// only supplies a candidate; this function is the daemon-owned admission
/// check that establishes the native thread identity and required methods.
pub fn verify_candidate(candidate: &AppServerCandidate) -> Result<SelectedTransport, AdapterError> {
    let socket_path = endpoint_path(&candidate.endpoint)?;
    if !socket_path.is_absolute() {
        return Err(AdapterError::Unknown {
            operation: "verify_candidate",
            detail: "App Server endpoint must be an absolute unix socket path".into(),
        });
    }
    if !matches!(candidate.namespace.as_str(), "codex_tui" | "codex_app") {
        return Err(AdapterError::Unknown {
            operation: "verify_candidate",
            detail: format!("unsupported App Server namespace {}", candidate.namespace),
        });
    }
    let thread_id = NativeThreadId::new(candidate.thread_id.clone()).map_err(|error| {
        AdapterError::InvalidBinding {
            detail: format!("candidate thread_id is invalid: {error}"),
        }
    })?;
    let timeout = Duration::from_millis(DEFAULT_TIMEOUT_MS);
    let mut client = Client::connect(&socket_path, timeout)?;
    client.initialize()?;
    let response = client
        .call("thread/read", json!({"threadId": thread_id.as_str()}))
        .map_err(|error| match error {
            AdapterError::Unknown { detail, .. } if detail.contains("thread not found") => {
                AdapterError::RouteUnavailable { detail }
            }
            error => error,
        })?;
    let observed = response
        .pointer("/thread/id")
        .and_then(Value::as_str)
        .ok_or_else(|| AdapterError::Unknown {
            operation: "thread/read",
            detail: "response is missing thread.id".into(),
        })?;
    if observed != thread_id.as_str() {
        return Err(AdapterError::Unknown {
            operation: "thread/read",
            detail: format!(
                "thread identity mismatch: expected {}, observed {}",
                thread_id, observed
            ),
        });
    }
    // Item history is a diagnostic capability, not a registration or wake
    // requirement. Some App Server builds expose thread/read and notification
    // methods but return method-not-found for items/list; that must not block peer
    // registration. Snapshot calls still fail explicitly if the method is
    // unavailable.
    let _items_available = method_exists(
        &mut client,
        "thread/items/list",
        json!({
            "threadId": thread_id.as_str(),
            "limit": 1,
            "sortDirection": "desc",
        }),
    )?;
    let immediate_notify = method_exists(
        &mut client,
        "turn/start",
        json!({"threadId": "", "input": []}),
    )?;
    if !immediate_notify {
        return Err(AdapterError::CapabilityUnavailable {
            endpoint: EndpointKind::Tui,
            operation: "turn/start",
        });
    }
    let steer = method_exists(
        &mut client,
        "turn/steer",
        json!({
            "threadId": "",
            "expectedTurnId": "",
            "input": [],
        }),
    )?;
    if !steer {
        return Err(AdapterError::CapabilityUnavailable {
            endpoint: EndpointKind::Tui,
            operation: "turn/steer",
        });
    }
    let turns_list = method_exists(
        &mut client,
        "thread/turns/list",
        json!({
            "threadId": thread_id.as_str(),
            "limit": 1,
            "sortDirection": "desc",
        }),
    )?;
    if !turns_list {
        return Err(AdapterError::CapabilityUnavailable {
            endpoint: EndpointKind::Tui,
            operation: "thread/turns/list",
        });
    }
    let queue_wakeup = method_exists(
        &mut client,
        "thread/queue/add",
        json!({"threadId": "", "input": []}),
    )?;
    if !queue_wakeup {
        return Err(AdapterError::CapabilityUnavailable {
            endpoint: EndpointKind::Tui,
            operation: "thread/queue/add",
        });
    }
    Ok(SelectedTransport {
        kind: TransportKind::AppServer,
        endpoint: Some(format!("unix://{}", socket_path.display())),
        namespace: Some(candidate.namespace.clone()),
        thread_id: Some(thread_id.to_string()),
        capabilities: vec![
            "session_status".into(),
            "read_thread".into(),
            "send_message_to_thread".into(),
            "queue_wakeup".into(),
            "wait_reply".into(),
        ],
        self_check:
            "initialize, thread/read identity, turn/start, turn/steer, thread/turns/list, and thread/queue/add method probes passed"
                .into(),
    })
}

/// Start or steer one immediate notification through a server-selected App
/// Server transport. A successful result means the native App Server accepted
/// the turn; execution and reply are observed separately.
pub fn immediate_notify(
    transport: &SelectedTransport,
    body: &str,
    client_user_message_id: &str,
) -> Result<Value, AdapterError> {
    if transport.kind != TransportKind::AppServer {
        return Err(AdapterError::CapabilityUnavailable {
            endpoint: EndpointKind::Tui,
            operation: "turn/start",
        });
    }
    let endpoint = transport
        .endpoint
        .as_deref()
        .ok_or_else(|| AdapterError::InvalidBinding {
            detail: "selected App Server transport has no endpoint".into(),
        })?;
    let thread_id = transport
        .thread_id
        .as_deref()
        .ok_or_else(|| AdapterError::InvalidBinding {
            detail: "selected App Server transport has no thread_id".into(),
        })?;
    let socket_path = endpoint_path(endpoint)?;
    let thread_id = NativeThreadId::new(thread_id.to_owned()).map_err(|error| {
        AdapterError::InvalidBinding {
            detail: format!("selected App Server thread_id is invalid: {error}"),
        }
    })?;
    let mut client = Client::connect(&socket_path, Duration::from_millis(DEFAULT_TIMEOUT_MS))?;
    client.initialize()?;
    let status = thread_status(&mut client, thread_id.as_str())?;
    let active_turn_id = match status.as_str() {
        "active" => active_turn_id(&mut client, thread_id.as_str())?,
        _ => None,
    };
    match notification_action(&status, active_turn_id)? {
        NotificationAction::Steer(expected_turn_id) => {
            let receipt = client.call(
                "turn/steer",
                json!({
                    "threadId": thread_id.as_str(),
                    "expectedTurnId": expected_turn_id,
                    "input": [{"type": "text", "text": body, "text_elements": []}],
                    "clientUserMessageId": client_user_message_id,
                }),
            )?;
            validate_steer_receipt(&receipt, &expected_turn_id)?;
            Ok(receipt)
        }
        NotificationAction::Start => {
            let receipt = client.call(
                "turn/start",
                json!({
                    "threadId": thread_id.as_str(),
                    "input": [{"type": "text", "text": body}],
                    "clientUserMessageId": client_user_message_id,
                }),
            )?;
            validate_immediate_receipt(&receipt)?;
            Ok(receipt)
        }
    }
}

/// Queue one background wake through a server-selected App Server transport.
/// This is reserved for wakeup/long-horizon paths; ordinary sendmessage must
/// use `immediate_notify`.
pub fn queue_wakeup(
    transport: &SelectedTransport,
    body: &str,
    client_user_message_id: &str,
) -> Result<Value, AdapterError> {
    if transport.kind != TransportKind::AppServer {
        return Err(AdapterError::CapabilityUnavailable {
            endpoint: EndpointKind::Tui,
            operation: "thread/queue/add",
        });
    }
    let endpoint = transport
        .endpoint
        .as_deref()
        .ok_or_else(|| AdapterError::InvalidBinding {
            detail: "selected App Server transport has no endpoint".into(),
        })?;
    let thread_id = transport
        .thread_id
        .as_deref()
        .ok_or_else(|| AdapterError::InvalidBinding {
            detail: "selected App Server transport has no thread_id".into(),
        })?;
    let socket_path = endpoint_path(endpoint)?;
    let thread_id = NativeThreadId::new(thread_id.to_owned()).map_err(|error| {
        AdapterError::InvalidBinding {
            detail: format!("selected App Server thread_id is invalid: {error}"),
        }
    })?;
    let mut client = Client::connect(&socket_path, Duration::from_millis(DEFAULT_TIMEOUT_MS))?;
    client.initialize()?;
    client.call(
        "thread/queue/add",
        json!({
            "threadId": thread_id.as_str(),
            "input": [{"type": "text", "text": body}],
            "clientUserMessageId": client_user_message_id,
        }),
    )
}

fn transport_client(transport: &SelectedTransport) -> Result<Client, AdapterError> {
    if transport.kind != TransportKind::AppServer {
        return Err(AdapterError::CapabilityUnavailable {
            endpoint: EndpointKind::Tui,
            operation: "App Server transport",
        });
    }
    let endpoint = transport
        .endpoint
        .as_deref()
        .ok_or_else(|| AdapterError::InvalidBinding {
            detail: "selected App Server transport has no endpoint".into(),
        })?;
    let socket_path = endpoint_path(endpoint)?;
    let mut client = Client::connect(&socket_path, Duration::from_millis(DEFAULT_TIMEOUT_MS))?;
    client.initialize()?;
    Ok(client)
}

pub fn start_thread(
    transport: &SelectedTransport,
    cwd: &Path,
    model: Option<&str>,
) -> Result<NativeThreadId, AdapterError> {
    let mut client = transport_client(transport)?;
    let mut params = json!({
        "cwd": cwd,
        "approvalPolicy": "never",
        "sandbox": "danger-full-access",
        "sessionStartSource": "startup"
    });
    if let Some(model) = model {
        params["model"] = json!(model);
    }
    let response = client.call("thread/start", params)?;
    let thread_id = response
        .pointer("/thread/id")
        .and_then(Value::as_str)
        .ok_or_else(|| AdapterError::Unknown {
            operation: "thread/start",
            detail: "response is missing thread.id".into(),
        })?;
    NativeThreadId::new(thread_id.to_owned()).map_err(|error| AdapterError::InvalidBinding {
        detail: format!("thread/start returned an invalid thread id: {error}"),
    })
}

pub fn archive_thread(
    transport: &SelectedTransport,
    thread_id: &str,
) -> Result<Value, AdapterError> {
    let mut client = transport_client(transport)?;
    client.call("thread/archive", json!({"threadId": thread_id}))
}

pub fn read_thread_items(
    transport: &SelectedTransport,
    thread_id: &str,
    limit: usize,
) -> Result<Value, AdapterError> {
    let mut client = transport_client(transport)?;
    client.call(
        "thread/items/list",
        json!({
            "threadId": thread_id,
            "limit": limit,
            "sortDirection": "desc",
        }),
    )
}

pub fn read_thread_status(
    transport: &SelectedTransport,
    thread_id: &str,
) -> Result<Value, AdapterError> {
    let mut client = transport_client(transport)?;
    client.call("thread/read", json!({"threadId": thread_id}))
}

pub fn read_latest_turn_status(
    transport: &SelectedTransport,
    thread_id: &str,
) -> Result<Value, AdapterError> {
    let mut client = transport_client(transport)?;
    client.call(
        "thread/turns/list",
        json!({
            "threadId": thread_id,
            "limit": 1,
            "sortDirection": "desc",
        }),
    )
}

fn endpoint_path(endpoint: &str) -> Result<PathBuf, AdapterError> {
    let path = endpoint
        .strip_prefix("unix://")
        .ok_or_else(|| AdapterError::Unknown {
            operation: "verify_candidate",
            detail: "only unix:// App Server endpoints are supported".into(),
        })?;
    if path.is_empty() {
        return Err(AdapterError::Unknown {
            operation: "verify_candidate",
            detail: "App Server endpoint has no socket path".into(),
        });
    }
    Ok(PathBuf::from(path))
}

fn thread_status(client: &mut Client, thread_id: &str) -> Result<String, AdapterError> {
    let receipt = client.call("thread/read", json!({"threadId": thread_id}))?;
    let observed = receipt
        .pointer("/thread/id")
        .and_then(Value::as_str)
        .filter(|value| !value.trim().is_empty())
        .ok_or_else(|| AdapterError::Unknown {
            operation: "thread/read",
            detail: "response is missing thread.id".into(),
        })?;
    if observed != thread_id {
        return Err(AdapterError::Unknown {
            operation: "thread/read",
            detail: format!("thread identity mismatch: expected {thread_id}, observed {observed}"),
        });
    }
    receipt
        .pointer("/thread/status/type")
        .and_then(Value::as_str)
        .filter(|value| !value.trim().is_empty())
        .map(str::to_owned)
        .ok_or_else(|| AdapterError::Unknown {
            operation: "thread/read",
            detail: "response is missing thread.status.type".into(),
        })
}

fn active_turn_id(client: &mut Client, thread_id: &str) -> Result<Option<String>, AdapterError> {
    let page = client.call(
        "thread/turns/list",
        json!({
            "threadId": thread_id,
            "limit": 100,
            "sortDirection": "desc",
        }),
    )?;
    active_turn_id_from_page(&page)
}

fn active_turn_id_from_page(page: &Value) -> Result<Option<String>, AdapterError> {
    let turns =
        page.get("data")
            .and_then(Value::as_array)
            .ok_or_else(|| AdapterError::Unknown {
                operation: "thread/turns/list",
                detail: "response is missing data array".into(),
            })?;
    let mut active = Vec::new();
    for (index, turn) in turns.iter().enumerate() {
        if turn.get("status").and_then(Value::as_str) != Some("inProgress") {
            continue;
        }
        let turn_id = turn
            .get("id")
            .ok_or_else(|| AdapterError::Unknown {
                operation: "turn/steer",
                detail: format!("inProgress turn at data[{index}] is missing id"),
            })?
            .as_str()
            .ok_or_else(|| AdapterError::Unknown {
                operation: "turn/steer",
                detail: format!("inProgress turn at data[{index}] id must be a JSON string"),
            })?;
        if turn_id.trim().is_empty() {
            return Err(AdapterError::Unknown {
                operation: "turn/steer",
                detail: format!("inProgress turn at data[{index}] id must be non-empty after trim"),
            });
        }
        if turn_id.chars().any(char::is_whitespace) {
            return Err(AdapterError::Unknown {
                operation: "turn/steer",
                detail: format!("inProgress turn at data[{index}] id must not contain whitespace"),
            });
        }
        active.push(turn_id);
    }
    match active.len() {
        0 => Ok(None),
        1 => {
            let turn_id = active.remove(0);
            Ok(Some(turn_id.to_owned()))
        }
        _ => Err(AdapterError::Unknown {
            operation: "turn/steer",
            detail: format!(
                "STEER_ACTIVE_TURN_AMBIGUOUS: recipient has {} inProgress turns",
                active.len()
            ),
        }),
    }
}

#[derive(Debug, PartialEq, Eq)]
enum NotificationAction {
    Start,
    Steer(String),
}

fn notification_action(
    thread_status: &str,
    active_turn_id: Option<String>,
) -> Result<NotificationAction, AdapterError> {
    match thread_status {
        "active" => Ok(match active_turn_id {
            Some(turn_id) => NotificationAction::Steer(turn_id),
            None => NotificationAction::Start,
        }),
        "idle" | "notLoaded" => Ok(NotificationAction::Start),
        status => Err(AdapterError::Unknown {
            operation: "thread/read",
            detail: format!(
                "AUTO_NOTIFY_UNSUPPORTED_THREAD_STATUS: cannot deliver to thread status {status}"
            ),
        }),
    }
}

fn validate_steer_receipt(receipt: &Value, expected_turn_id: &str) -> Result<(), AdapterError> {
    let turn_id = receipt
        .get("turnId")
        .and_then(Value::as_str)
        .filter(|value| !value.trim().is_empty())
        .ok_or_else(|| AdapterError::Unknown {
            operation: "turn/steer",
            detail: "response is missing turnId".into(),
        })?;
    if turn_id != expected_turn_id {
        return Err(AdapterError::Unknown {
            operation: "turn/steer",
            detail: format!(
                "turn identity mismatch: expected {expected_turn_id}, observed {turn_id}"
            ),
        });
    }
    Ok(())
}

fn validate_immediate_receipt(receipt: &Value) -> Result<(), AdapterError> {
    let turn_id = receipt
        .pointer("/turn/id")
        .and_then(Value::as_str)
        .filter(|value| !value.trim().is_empty())
        .ok_or_else(|| AdapterError::Unknown {
            operation: "turn/start",
            detail: "response is missing turn.id".into(),
        })?;
    let status = receipt
        .pointer("/turn/status")
        .and_then(Value::as_str)
        .ok_or_else(|| AdapterError::Unknown {
            operation: "turn/start",
            detail: "response is missing turn.status".into(),
        })?;
    if !matches!(
        status,
        "inProgress" | "completed" | "interrupted" | "failed"
    ) {
        return Err(AdapterError::Unknown {
            operation: "turn/start",
            detail: format!("response returned unsupported turn status {status}"),
        });
    }
    if turn_id.chars().any(char::is_whitespace) {
        return Err(AdapterError::Unknown {
            operation: "turn/start",
            detail: "response returned an invalid turn.id".into(),
        });
    }
    Ok(())
}

fn method_exists(client: &mut Client, method: &str, params: Value) -> Result<bool, AdapterError> {
    match client.call_raw(method, params)? {
        Ok(_) => Ok(true),
        Err(error) => Ok(error.code != -32601),
    }
}

fn socket_candidate() -> Option<PathBuf> {
    if let Some(value) = std::env::var_os(APPSERVER_SOCKET_ENV).filter(|value| !value.is_empty()) {
        return Some(PathBuf::from(value));
    }
    if let Some(value) =
        std::env::var_os("CODEX_APP_SERVER_SOCKET").filter(|value| !value.is_empty())
    {
        return Some(PathBuf::from(value));
    }
    let codex_home = std::env::var_os("CODEX_HOME")
        .filter(|value| !value.is_empty())
        .map(PathBuf::from)
        .or_else(|| {
            std::env::var_os("HOME")
                .filter(|value| !value.is_empty())
                .map(|home| PathBuf::from(home).join(".codex"))
        })?;
    Some(codex_home.join("app-server-control/app-server-control.sock"))
}

struct Client {
    stream: UnixStream,
    timeout: Duration,
    next_id: u64,
}

#[derive(Debug)]
struct RpcError {
    code: i64,
    message: String,
}

impl Client {
    fn connect(path: &Path, timeout: Duration) -> Result<Self, AdapterError> {
        let stream = UnixStream::connect(path).map_err(|error| {
            if matches!(
                error.kind(),
                std::io::ErrorKind::NotFound
                    | std::io::ErrorKind::ConnectionRefused
                    | std::io::ErrorKind::ConnectionReset
                    | std::io::ErrorKind::ConnectionAborted
            ) {
                AdapterError::RouteUnavailable {
                    detail: format!("{}: {error}", path.display()),
                }
            } else {
                AdapterError::Unknown {
                    operation: "connect",
                    detail: format!("{}: {error}", path.display()),
                }
            }
        })?;
        stream
            .set_read_timeout(Some(timeout))
            .map_err(|error| AdapterError::Unknown {
                operation: "connect",
                detail: format!("set read timeout: {error}"),
            })?;
        stream
            .set_write_timeout(Some(timeout))
            .map_err(|error| AdapterError::Unknown {
                operation: "connect",
                detail: format!("set write timeout: {error}"),
            })?;
        let mut client = Self {
            stream,
            timeout,
            next_id: 1,
        };
        client.handshake()?;
        Ok(client)
    }

    fn handshake(&mut self) -> Result<(), AdapterError> {
        let key = websocket_key();
        let request = format!(
            "GET / HTTP/1.1\r\nHost: localhost\r\nUpgrade: websocket\r\nConnection: Upgrade\r\nSec-WebSocket-Key: {key}\r\nSec-WebSocket-Version: 13\r\n\r\n"
        );
        self.stream
            .write_all(request.as_bytes())
            .map_err(|error| transport("websocket handshake write", error))?;
        let mut reader = BufReader::new(
            self.stream
                .try_clone()
                .map_err(|error| transport("websocket handshake clone", error))?,
        );
        let mut status = String::new();
        reader
            .read_line(&mut status)
            .map_err(|error| transport("websocket handshake status", error))?;
        if !status.starts_with("HTTP/1.1 101") && !status.starts_with("HTTP/1.0 101") {
            return Err(AdapterError::Unknown {
                operation: "websocket handshake",
                detail: format!("upgrade rejected: {}", status.trim()),
            });
        }
        loop {
            let mut header = String::new();
            reader
                .read_line(&mut header)
                .map_err(|error| transport("websocket handshake header", error))?;
            if header == "\r\n" || header == "\n" || header.is_empty() {
                break;
            }
        }
        Ok(())
    }

    fn initialize(&mut self) -> Result<Value, AdapterError> {
        let result = self.call(
            "initialize",
            json!({
                "clientInfo": {
                    "name": "collab",
                    "title": "Collab",
                    "version": env!("COLLAB_VERSION")
                },
                "capabilities": {"experimentalApi": true}
            }),
        )?;
        self.notify("initialized", json!({}))?;
        Ok(result)
    }

    fn call(&mut self, method: &str, params: Value) -> Result<Value, AdapterError> {
        match self.call_raw(method, params)? {
            Ok(value) => Ok(value),
            Err(error) => Err(AdapterError::Unknown {
                operation: "rpc",
                detail: error.message,
            }),
        }
    }

    fn call_raw(
        &mut self,
        method: &str,
        params: Value,
    ) -> Result<Result<Value, RpcError>, AdapterError> {
        let id = self.next_id;
        self.next_id = self.next_id.saturating_add(1);
        self.write_json(&json!({"method": method, "id": id, "params": params}))?;
        loop {
            let value = self.read_json()?;
            if value.get("id").and_then(Value::as_u64) != Some(id) {
                continue;
            }
            if let Some(error) = value.get("error") {
                return Ok(Err(RpcError {
                    code: error.get("code").and_then(Value::as_i64).unwrap_or(0),
                    message: error
                        .get("message")
                        .and_then(Value::as_str)
                        .unwrap_or("native JSON-RPC error")
                        .to_owned(),
                }));
            }
            return Ok(Ok(value.get("result").cloned().ok_or_else(|| {
                AdapterError::Unknown {
                    operation: "rpc",
                    detail: "native response is missing result".into(),
                }
            })?));
        }
    }

    fn notify(&mut self, method: &str, params: Value) -> Result<(), AdapterError> {
        self.write_json(&json!({"method": method, "params": params}))
    }

    fn write_json(&mut self, value: &Value) -> Result<(), AdapterError> {
        let payload = serde_json::to_vec(value).map_err(|error| AdapterError::Unknown {
            operation: "encode",
            detail: error.to_string(),
        })?;
        if payload.len() > MAX_FRAME_BYTES {
            return Err(AdapterError::Unknown {
                operation: "encode",
                detail: "native frame exceeds maximum size".into(),
            });
        }
        self.stream
            .write_all(&encode_frame(0x1, &payload))
            .map_err(|error| transport("websocket write", error))?;
        self.stream
            .flush()
            .map_err(|error| transport("websocket flush", error))
    }

    fn read_json(&mut self) -> Result<Value, AdapterError> {
        loop {
            let payload = self.read_frame()?;
            let value: Value =
                serde_json::from_slice(&payload).map_err(|error| AdapterError::Unknown {
                    operation: "decode",
                    detail: error.to_string(),
                })?;
            if value.get("id").is_some() {
                return Ok(value);
            }
        }
    }

    fn read_frame(&mut self) -> Result<Vec<u8>, AdapterError> {
        let mut header = [0_u8; 2];
        self.stream
            .read_exact(&mut header)
            .map_err(|error| transport("websocket header", error))?;
        let opcode = header[0] & 0x0f;
        let masked = header[1] & 0x80 != 0;
        let mut length = (header[1] & 0x7f) as usize;
        if length == 126 {
            let mut bytes = [0_u8; 2];
            self.stream
                .read_exact(&mut bytes)
                .map_err(|error| transport("websocket length", error))?;
            length = u16::from_be_bytes(bytes) as usize;
        } else if length == 127 {
            let mut bytes = [0_u8; 8];
            self.stream
                .read_exact(&mut bytes)
                .map_err(|error| transport("websocket length", error))?;
            let value = u64::from_be_bytes(bytes);
            if value > MAX_FRAME_BYTES as u64 {
                return Err(AdapterError::Unknown {
                    operation: "websocket frame",
                    detail: "native frame exceeds maximum size".into(),
                });
            }
            length = value as usize;
        }
        let mut mask = [0_u8; 4];
        if masked {
            self.stream
                .read_exact(&mut mask)
                .map_err(|error| transport("websocket mask", error))?;
        }
        let mut payload = vec![0_u8; length];
        self.stream
            .read_exact(&mut payload)
            .map_err(|error| transport("websocket payload", error))?;
        if masked {
            for (index, byte) in payload.iter_mut().enumerate() {
                *byte ^= mask[index % 4];
            }
        }
        match opcode {
            0x1 => Ok(payload),
            0x8 => Err(AdapterError::Unknown {
                operation: "websocket",
                detail: "native App Server closed the connection".into(),
            }),
            0x9 => {
                self.stream
                    .write_all(&encode_frame(0xA, &payload))
                    .map_err(|error| transport("websocket pong", error))?;
                self.read_frame()
            }
            _ => self.read_frame(),
        }
    }
}

fn transport(operation: &'static str, error: std::io::Error) -> AdapterError {
    if matches!(
        error.kind(),
        std::io::ErrorKind::TimedOut | std::io::ErrorKind::WouldBlock
    ) {
        AdapterError::Timeout { operation }
    } else {
        AdapterError::Unknown {
            operation,
            detail: error.to_string(),
        }
    }
}

fn encode_frame(opcode: u8, payload: &[u8]) -> Vec<u8> {
    let mut frame = Vec::with_capacity(payload.len() + 14);
    frame.push(0x80 | opcode);
    let mask = [
        rand::random::<u8>(),
        rand::random::<u8>(),
        rand::random::<u8>(),
        rand::random::<u8>(),
    ];
    match payload.len() {
        length if length < 126 => frame.push(0x80 | length as u8),
        length if length <= u16::MAX as usize => {
            frame.push(0x80 | 126);
            frame.extend_from_slice(&(length as u16).to_be_bytes());
        }
        length => {
            frame.push(0x80 | 127);
            frame.extend_from_slice(&(length as u64).to_be_bytes());
        }
    }
    frame.extend_from_slice(&mask);
    frame.extend(
        payload
            .iter()
            .enumerate()
            .map(|(index, byte)| byte ^ mask[index % 4]),
    );
    frame
}

fn websocket_key() -> String {
    const ALPHABET: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let bytes: [u8; 16] = rand::random();
    let mut output = String::with_capacity(24);
    for chunk in bytes.chunks(3) {
        let b0 = chunk[0] as u32;
        let b1 = chunk.get(1).copied().unwrap_or(0) as u32;
        let b2 = chunk.get(2).copied().unwrap_or(0) as u32;
        let value = (b0 << 16) | (b1 << 8) | b2;
        output.push(ALPHABET[((value >> 18) & 0x3f) as usize] as char);
        output.push(ALPHABET[((value >> 12) & 0x3f) as usize] as char);
        output.push(if chunk.len() > 1 {
            ALPHABET[((value >> 6) & 0x3f) as usize] as char
        } else {
            '='
        });
        output.push(if chunk.len() > 2 {
            ALPHABET[(value & 0x3f) as usize] as char
        } else {
            '='
        });
    }
    output
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::net::Shutdown;
    use std::os::unix::net::UnixListener;
    use std::thread;

    fn assert_malformed_active_turn(page: Value, expected_detail: &str) {
        match active_turn_id_from_page(&page).unwrap_err() {
            AdapterError::Unknown { operation, detail } => {
                assert_eq!(operation, "turn/steer");
                assert!(detail.contains(expected_detail), "{detail}");
            }
            error => panic!("expected AdapterError::Unknown, got {error:?}"),
        }
    }

    #[test]
    fn frame_round_trip_uses_masked_client_frames() {
        let frame = encode_frame(0x1, b"hello");
        assert_eq!(frame[0], 0x81);
        assert_eq!(frame[1] & 0x80, 0x80);
        assert_eq!(frame[1] & 0x7f, 5);
        let mask = &frame[2..6];
        let decoded = frame[6..]
            .iter()
            .enumerate()
            .map(|(index, byte)| byte ^ mask[index % 4])
            .collect::<Vec<_>>();
        assert_eq!(decoded, b"hello");
    }

    #[test]
    fn client_handshake_and_rpc_round_trip() {
        let socket = std::env::temp_dir().join(format!(
            "collab-appserver-{}-{}.sock",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        let listener = UnixListener::bind(&socket).unwrap();
        let server = thread::spawn(move || {
            let (mut stream, _) = listener.accept().unwrap();
            let mut request = Vec::new();
            let mut byte = [0_u8; 1];
            while !request.ends_with(b"\r\n\r\n") {
                stream.read_exact(&mut byte).unwrap();
                request.push(byte[0]);
            }
            stream
                .write_all(b"HTTP/1.1 101 Switching Protocols\r\nUpgrade: websocket\r\nConnection: Upgrade\r\n\r\n")
                .unwrap();
            let payload = read_client_frame(&mut stream);
            let request: Value = serde_json::from_slice(&payload).unwrap();
            let response = json!({"id": request["id"], "result": {"ok": true}});
            stream
                .write_all(&encode_frame(0x1, &serde_json::to_vec(&response).unwrap()))
                .unwrap();
            stream.shutdown(Shutdown::Both).ok();
        });

        let mut client = Client::connect(&socket, Duration::from_secs(2)).unwrap();
        let value = client.call("initialize", json!({})).unwrap();
        assert_eq!(value["ok"], true);
        server.join().unwrap();
        std::fs::remove_file(socket).ok();
    }

    #[test]
    fn live_appserver_candidate_passes_full_server_self_check_when_available() {
        let Some(candidate) = candidate_from_env().unwrap() else {
            return;
        };
        let selected = verify_candidate(&candidate).expect("live App Server self-check");
        assert_eq!(selected.kind, TransportKind::AppServer);
        assert_eq!(
            selected.endpoint.as_deref(),
            Some(candidate.endpoint.as_str())
        );
        assert_eq!(
            selected.namespace.as_deref(),
            Some(candidate.namespace.as_str())
        );
        assert_eq!(
            selected.thread_id.as_deref(),
            Some(candidate.thread_id.as_str())
        );
        assert!(selected
            .capabilities
            .iter()
            .any(|capability| capability == "send_message_to_thread"));
        assert!(selected
            .capabilities
            .iter()
            .any(|capability| capability == "queue_wakeup"));
        assert!(selected.self_check.contains("turn/start"));
        assert!(selected.self_check.contains("turn/steer"));
        assert!(selected.self_check.contains("thread/turns/list"));
        assert!(selected.self_check.contains("thread/queue/add"));
    }

    #[test]
    fn candidate_rejects_appserver_without_queue_wakeup_method() {
        let socket =
            std::env::temp_dir().join(format!("collab-queue-probe-{}.sock", std::process::id()));
        let Some(listener) = bind_test_socket(&socket) else {
            return;
        };
        let server = thread::spawn(move || {
            let (mut stream, _) = listener.accept().unwrap();
            let mut request = Vec::new();
            let mut byte = [0_u8; 1];
            while !request.ends_with(b"\r\n\r\n") {
                stream.read_exact(&mut byte).unwrap();
                request.push(byte[0]);
            }
            stream
                .write_all(b"HTTP/1.1 101 Switching Protocols\r\nUpgrade: websocket\r\nConnection: Upgrade\r\n\r\n")
                .unwrap();
            loop {
                let payload = read_client_frame(&mut stream);
                let request: Value = serde_json::from_slice(&payload).unwrap();
                let Some(id) = request.get("id").cloned() else {
                    continue;
                };
                let method = request["method"].as_str().unwrap();
                let response = match method {
                    "initialize" => json!({"id": id, "result": {}}),
                    "thread/read" => {
                        json!({"id": id, "result": {"thread": {"id": "thread-1"}}})
                    }
                    "thread/items/list" => {
                        json!({"id": id, "error": {"code": -32601, "message": "unsupported"}})
                    }
                    "turn/start" => {
                        json!({"id": id, "error": {"code": -32600, "message": "invalid params"}})
                    }
                    "turn/steer" => {
                        json!({"id": id, "error": {"code": -32600, "message": "invalid params"}})
                    }
                    "thread/turns/list" => {
                        json!({"id": id, "result": {"data": []}})
                    }
                    "thread/queue/add" => {
                        let response = json!({
                            "id": id,
                            "error": {"code": -32601, "message": "unsupported"}
                        });
                        stream
                            .write_all(&encode_frame(0x1, &serde_json::to_vec(&response).unwrap()))
                            .unwrap();
                        break;
                    }
                    _ => unreachable!("{method}"),
                };
                stream
                    .write_all(&encode_frame(0x1, &serde_json::to_vec(&response).unwrap()))
                    .unwrap();
            }
            stream.shutdown(Shutdown::Both).ok();
        });

        let candidate = AppServerCandidate {
            endpoint: format!("unix://{}", socket.display()),
            namespace: "codex_tui".into(),
            thread_id: "thread-1".into(),
        };
        let error = verify_candidate(&candidate).unwrap_err();
        assert!(matches!(
            error,
            AdapterError::CapabilityUnavailable {
                operation: "thread/queue/add",
                ..
            }
        ));
        server.join().unwrap();
        std::fs::remove_file(socket).ok();
    }

    #[test]
    fn candidate_rejects_missing_appserver_thread_as_route_unavailable() {
        let socket = std::env::temp_dir().join(format!(
            "collab-missing-thread-{}-{}.sock",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        let listener = UnixListener::bind(&socket).unwrap();
        let server = thread::spawn(move || {
            let (mut stream, _) = listener.accept().unwrap();
            let mut request = Vec::new();
            let mut byte = [0_u8; 1];
            while !request.ends_with(b"\r\n\r\n") {
                stream.read_exact(&mut byte).unwrap();
                request.push(byte[0]);
            }
            stream
                .write_all(b"HTTP/1.1 101 Switching Protocols\r\nUpgrade: websocket\r\nConnection: Upgrade\r\n\r\n")
                .unwrap();
            loop {
                let payload = read_client_frame(&mut stream);
                let request: Value = serde_json::from_slice(&payload).unwrap();
                let Some(id) = request.get("id").cloned() else {
                    continue;
                };
                let response = match request["method"].as_str().unwrap() {
                    "initialize" => json!({"id": id, "result": {}}),
                    "thread/read" => {
                        json!({"id": id, "error": {"code": -32602, "message": "thread not found"}})
                    }
                    method => panic!("unexpected method after missing thread: {method}"),
                };
                stream
                    .write_all(&encode_frame(0x1, &serde_json::to_vec(&response).unwrap()))
                    .unwrap();
                if request["method"] == "thread/read" {
                    break;
                }
            }
            stream.shutdown(Shutdown::Both).ok();
        });

        let candidate = AppServerCandidate {
            endpoint: format!("unix://{}", socket.display()),
            namespace: "codex_tui".into(),
            thread_id: "missing-thread".into(),
        };
        let error = verify_candidate(&candidate).unwrap_err();
        assert!(matches!(error, AdapterError::RouteUnavailable { .. }));
        assert!(error.to_string().contains("ADAPTER_ROUTE_UNAVAILABLE"));
        server.join().unwrap();
        std::fs::remove_file(socket).ok();
    }

    #[test]
    fn candidate_rejects_appserver_without_steer_or_turns_list_methods() {
        for missing_method in ["turn/steer", "thread/turns/list"] {
            let socket = std::env::temp_dir().join(format!(
                "collab-candidate-method-{}-{}.sock",
                std::process::id(),
                std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .unwrap()
                    .as_nanos()
            ));
            let Some(listener) = bind_test_socket(&socket) else {
                return;
            };
            let missing_method = missing_method.to_string();
            let server_missing_method = missing_method.clone();
            let server = thread::spawn(move || {
                let (mut stream, _) = listener.accept().unwrap();
                handshake(&mut stream);
                loop {
                    let payload = read_client_frame(&mut stream);
                    let request: Value = serde_json::from_slice(&payload).unwrap();
                    let Some(id) = request.get("id").cloned() else {
                        continue;
                    };
                    let method = request["method"].as_str().unwrap();
                    let response = if method == server_missing_method {
                        json!({"id": id, "error": {"code": -32601, "message": "unsupported"}})
                    } else {
                        match method {
                            "initialize" => json!({"id": id, "result": {}}),
                            "thread/read" => {
                                json!({"id": id, "result": {"thread": {"id": "thread-1"}}})
                            }
                            "thread/items/list" => {
                                json!({"id": id, "error": {"code": -32601, "message": "unsupported"}})
                            }
                            "turn/start" | "turn/steer" | "thread/queue/add" => {
                                json!({"id": id, "error": {"code": -32600, "message": "invalid params"}})
                            }
                            "thread/turns/list" => {
                                json!({"id": id, "result": {"data": []}})
                            }
                            _ => unreachable!("{method}"),
                        }
                    };
                    stream
                        .write_all(&encode_frame(0x1, &serde_json::to_vec(&response).unwrap()))
                        .unwrap();
                    if method == server_missing_method {
                        break;
                    }
                }
                stream.shutdown(Shutdown::Both).ok();
            });

            let candidate = AppServerCandidate {
                endpoint: format!("unix://{}", socket.display()),
                namespace: "codex_tui".into(),
                thread_id: "thread-1".into(),
            };
            let error = verify_candidate(&candidate).unwrap_err();
            assert!(
                matches!(
                    &error,
                    AdapterError::CapabilityUnavailable {
                        operation,
                        ..
                    } if *operation == missing_method
                ),
                "{missing_method}: {error}"
            );
            server.join().unwrap();
            std::fs::remove_file(socket).ok();
        }
    }

    #[test]
    fn immediate_receipt_requires_turn_identity_and_protocol_status() {
        validate_immediate_receipt(&json!({
            "turn": {"id": "turn-1", "status": "inProgress"}
        }))
        .unwrap();

        for malformed in [
            json!({}),
            json!({"turn": {"status": "inProgress"}}),
            json!({"turn": {"id": "turn-1"}}),
            json!({"turn": {"id": "turn-1", "status": "queued"}}),
            json!({"turn": {"id": "bad turn", "status": "completed"}}),
        ] {
            assert!(
                validate_immediate_receipt(&malformed).is_err(),
                "{malformed}"
            );
        }
    }

    #[test]
    fn steer_receipt_requires_matching_turn_identity() {
        validate_steer_receipt(&json!({"turnId": "turn-1"}), "turn-1").unwrap();

        for malformed in [
            json!({}),
            json!({"turnId": ""}),
            json!({"turnId": "turn-2"}),
            json!({"turnId": "bad turn"}),
        ] {
            assert!(
                validate_steer_receipt(&malformed, "turn-1").is_err(),
                "{malformed}"
            );
        }
    }

    #[test]
    fn active_turn_selection_only_accepts_in_progress_turns() {
        assert_eq!(
            active_turn_id_from_page(&json!({
                "data": [{"id": "turn-active", "status": "inProgress"}]
            }))
            .unwrap()
            .as_deref(),
            Some("turn-active")
        );
        assert_eq!(
            active_turn_id_from_page(&json!({
                "data": [{"id": "turn-interrupted", "status": "interrupted"}]
            }))
            .unwrap(),
            None
        );
        assert_eq!(
            active_turn_id_from_page(&json!({
                "data": [
                    {"id": "turn-interrupted", "status": "interrupted"},
                    {"id": "turn-completed", "status": "completed"}
                ]
            }))
            .unwrap(),
            None
        );
    }

    #[test]
    fn active_turn_selection_rejects_missing_id() {
        assert_malformed_active_turn(
            json!({"data": [{"status": "inProgress"}]}),
            "data[0] is missing id",
        );
    }

    #[test]
    fn active_turn_selection_rejects_non_string_id() {
        assert_malformed_active_turn(
            json!({"data": [{"id": 7, "status": "inProgress"}]}),
            "data[0] id must be a JSON string",
        );
    }

    #[test]
    fn active_turn_selection_rejects_empty_id() {
        for id in ["", "   "] {
            assert_malformed_active_turn(
                json!({"data": [{"id": id, "status": "inProgress"}]}),
                "data[0] id must be non-empty after trim",
            );
        }
    }

    #[test]
    fn active_turn_selection_rejects_whitespace_containing_id() {
        assert_malformed_active_turn(
            json!({"data": [{"id": "turn active", "status": "inProgress"}]}),
            "data[0] id must not contain whitespace",
        );
    }

    #[test]
    fn active_turn_selection_handles_valid_zero_and_multiple_turns() {
        assert_eq!(
            active_turn_id_from_page(&json!({"data": []})).unwrap(),
            None
        );
        assert_eq!(
            active_turn_id_from_page(&json!({
                "data": [{"id": "turn-active", "status": "inProgress"}]
            }))
            .unwrap()
            .as_deref(),
            Some("turn-active")
        );
        let error = active_turn_id_from_page(&json!({
            "data": [
                {"id": "turn-1", "status": "inProgress"},
                {"id": "turn-2", "status": "inProgress"}
            ]
        }))
        .unwrap_err();
        assert!(
            error.to_string().contains("STEER_ACTIVE_TURN_AMBIGUOUS"),
            "{error}"
        );
    }

    #[test]
    fn notification_action_uses_turn_start_when_active_has_no_in_progress_turn() {
        assert_eq!(
            notification_action("active", None).unwrap(),
            NotificationAction::Start
        );
        assert_eq!(
            notification_action("active", Some("turn-active".into())).unwrap(),
            NotificationAction::Steer("turn-active".into())
        );
        assert_eq!(
            notification_action("idle", None).unwrap(),
            NotificationAction::Start
        );
        assert_eq!(
            notification_action("notLoaded", None).unwrap(),
            NotificationAction::Start
        );
        for status in ["systemError", "unknown", ""] {
            let error = notification_action(status, None).unwrap_err();
            assert!(
                error
                    .to_string()
                    .contains("AUTO_NOTIFY_UNSUPPORTED_THREAD_STATUS"),
                "{error}"
            );
        }
    }

    #[test]
    fn active_interrupted_only_and_in_progress_turn_actions_are_explicit() {
        let interrupted_only = active_turn_id_from_page(&json!({
            "data": [{"id": "turn-interrupted", "status": "interrupted"}]
        }))
        .unwrap();
        assert_eq!(
            notification_action("active", interrupted_only).unwrap(),
            NotificationAction::Start
        );

        let active = active_turn_id_from_page(&json!({
            "data": [{"id": "turn-active", "status": "inProgress"}]
        }))
        .unwrap();
        assert_eq!(
            notification_action("active", active).unwrap(),
            NotificationAction::Steer("turn-active".into())
        );
    }

    #[test]
    fn immediate_notify_routes_active_thread_to_steer() {
        let socket = temp_socket("notify-active");
        let Some(listener) = bind_test_socket(&socket) else {
            return;
        };
        let server = thread::spawn(move || {
            let (mut stream, _) = listener.accept().unwrap();
            handshake(&mut stream);
            initialize(&mut stream);
            let read_id = next_request_id(&mut stream);
            respond(
                &mut stream,
                json!({
                    "id": read_id,
                    "result": {
                        "thread": {
                            "id": "thread-1",
                            "status": {"type": "active", "activeFlags": []}
                        }
                    }
                }),
            );
            let turns_id = next_request_id(&mut stream);
            respond(
                &mut stream,
                json!({
                    "id": turns_id,
                    "result": {
                        "data": [
                            {"id": "turn-active", "status": "inProgress", "items": []}
                        ]
                    }
                }),
            );
            let request = next_request(&mut stream);
            assert_eq!(request["method"], "turn/steer");
            assert_eq!(request["params"]["threadId"], "thread-1");
            assert_eq!(request["params"]["expectedTurnId"], "turn-active");
            assert_eq!(request["params"]["clientUserMessageId"], "message-active");
            respond(
                &mut stream,
                json!({"id": request["id"], "result": {"turnId": "turn-active"}}),
            );
            stream.shutdown(Shutdown::Both).ok();
        });

        let receipt = immediate_notify(
            &selected_transport(&socket),
            "notify body",
            "message-active",
        )
        .unwrap();
        assert_eq!(receipt["turnId"], "turn-active");
        server.join().unwrap();
        std::fs::remove_file(socket).ok();
    }

    #[test]
    fn immediate_notify_starts_active_thread_with_only_interrupted_turn() {
        let socket = temp_socket("notify-interrupted");
        let Some(listener) = bind_test_socket(&socket) else {
            return;
        };
        let server = thread::spawn(move || {
            let (mut stream, _) = listener.accept().unwrap();
            handshake(&mut stream);
            initialize(&mut stream);
            let read_id = next_request_id(&mut stream);
            respond(
                &mut stream,
                json!({
                    "id": read_id,
                    "result": {
                        "thread": {
                            "id": "thread-1",
                            "status": {"type": "active", "activeFlags": []}
                        }
                    }
                }),
            );
            let turns_id = next_request_id(&mut stream);
            respond(
                &mut stream,
                json!({
                    "id": turns_id,
                    "result": {
                        "data": [
                            {"id": "turn-interrupted", "status": "interrupted", "items": []}
                        ]
                    }
                }),
            );
            let request = next_request(&mut stream);
            assert_eq!(request["method"], "turn/start");
            assert_eq!(request["params"]["threadId"], "thread-1");
            assert_eq!(
                request["params"]["clientUserMessageId"],
                "message-interrupted"
            );
            respond(
                &mut stream,
                json!({
                    "id": request["id"],
                    "result": {
                        "turn": {"id": "turn-started", "status": "inProgress", "items": []}
                    }
                }),
            );
            stream.shutdown(Shutdown::Both).ok();
        });

        let receipt = immediate_notify(
            &selected_transport(&socket),
            "notify body",
            "message-interrupted",
        )
        .unwrap();
        assert_eq!(receipt["turn"]["id"], "turn-started");
        server.join().unwrap();
        std::fs::remove_file(socket).ok();
    }

    #[test]
    fn immediate_notify_rejects_multiple_in_progress_turns() {
        let socket = temp_socket("notify-multiple-active");
        let Some(listener) = bind_test_socket(&socket) else {
            return;
        };
        let server = thread::spawn(move || {
            let (mut stream, _) = listener.accept().unwrap();
            handshake(&mut stream);
            initialize(&mut stream);
            let read_id = next_request_id(&mut stream);
            respond(
                &mut stream,
                json!({
                    "id": read_id,
                    "result": {
                        "thread": {
                            "id": "thread-1",
                            "status": {"type": "active", "activeFlags": []}
                        }
                    }
                }),
            );
            let turns_id = next_request_id(&mut stream);
            respond(
                &mut stream,
                json!({
                    "id": turns_id,
                    "result": {
                        "data": [
                            {"id": "turn-1", "status": "inProgress", "items": []},
                            {"id": "turn-2", "status": "inProgress", "items": []}
                        ]
                    }
                }),
            );
            stream.shutdown(Shutdown::Both).ok();
        });

        let error = immediate_notify(
            &selected_transport(&socket),
            "notify body",
            "message-multiple-active",
        )
        .unwrap_err();
        assert!(
            error.to_string().contains("STEER_ACTIVE_TURN_AMBIGUOUS"),
            "{error}"
        );
        server.join().unwrap();
        std::fs::remove_file(socket).ok();
    }

    #[test]
    fn immediate_notify_routes_idle_and_not_loaded_threads_to_turn_start() {
        for status in ["idle", "notLoaded"] {
            let socket = temp_socket("notify-start");
            let Some(listener) = bind_test_socket(&socket) else {
                return;
            };
            let status = status.to_string();
            let server = thread::spawn(move || {
                let (mut stream, _) = listener.accept().unwrap();
                handshake(&mut stream);
                initialize(&mut stream);
                let read_id = next_request_id(&mut stream);
                respond(
                    &mut stream,
                    json!({
                        "id": read_id,
                        "result": {
                            "thread": {
                                "id": "thread-1",
                                "status": {"type": status}
                            }
                        }
                    }),
                );
                let request = next_request(&mut stream);
                assert_eq!(request["method"], "turn/start");
                assert_eq!(request["params"]["threadId"], "thread-1");
                respond(
                    &mut stream,
                    json!({
                        "id": request["id"],
                        "result": {
                            "turn": {"id": "turn-started", "status": "inProgress", "items": []}
                        }
                    }),
                );
                stream.shutdown(Shutdown::Both).ok();
            });

            immediate_notify(&selected_transport(&socket), "notify body", "message-start").unwrap();
            server.join().unwrap();
            std::fs::remove_file(socket).ok();
        }
    }

    #[test]
    fn immediate_notify_rejects_queued_submission_as_success() {
        let socket = temp_socket("notify-queued");
        let Some(listener) = bind_test_socket(&socket) else {
            return;
        };
        let server = thread::spawn(move || {
            let (mut stream, _) = listener.accept().unwrap();
            handshake(&mut stream);
            initialize(&mut stream);
            let read_id = next_request_id(&mut stream);
            respond(
                &mut stream,
                json!({
                    "id": read_id,
                    "result": {
                        "thread": {
                            "id": "thread-1",
                            "status": {"type": "idle"}
                        }
                    }
                }),
            );
            let request = next_request(&mut stream);
            assert_eq!(request["method"], "turn/start");
            respond(
                &mut stream,
                json!({
                    "id": request["id"],
                    "result": {
                        "queuedSubmission": {"id": "queue-1"}
                    }
                }),
            );
            stream.shutdown(Shutdown::Both).ok();
        });

        assert!(immediate_notify(
            &selected_transport(&socket),
            "notify body",
            "message-queued"
        )
        .is_err());
        server.join().unwrap();
        std::fs::remove_file(socket).ok();
    }

    fn selected_transport(socket: &Path) -> SelectedTransport {
        SelectedTransport {
            kind: TransportKind::AppServer,
            endpoint: Some(format!("unix://{}", socket.display())),
            namespace: Some("codex_tui".into()),
            thread_id: Some("thread-1".into()),
            capabilities: vec!["send_message_to_thread".into()],
            self_check: "test".into(),
        }
    }

    fn temp_socket(tag: &str) -> PathBuf {
        PathBuf::from(format!(
            "collab-{tag}-{}-{}.sock",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ))
    }

    fn bind_test_socket(socket: &Path) -> Option<UnixListener> {
        match UnixListener::bind(socket) {
            Ok(listener) => Some(listener),
            Err(error) if error.kind() == std::io::ErrorKind::PermissionDenied => {
                eprintln!(
                    "SKIP socket integration assertion: sandbox denied unix socket bind at {}",
                    socket.display()
                );
                None
            }
            Err(error) => panic!("bind {}: {error}", socket.display()),
        }
    }

    fn handshake(stream: &mut UnixStream) {
        let mut request = Vec::new();
        let mut byte = [0_u8; 1];
        while !request.ends_with(b"\r\n\r\n") {
            stream.read_exact(&mut byte).unwrap();
            request.push(byte[0]);
        }
        stream
            .write_all(
                b"HTTP/1.1 101 Switching Protocols\r\nUpgrade: websocket\r\nConnection: Upgrade\r\n\r\n",
            )
            .unwrap();
    }

    fn initialize(stream: &mut UnixStream) {
        let request = next_request(stream);
        assert_eq!(request["method"], "initialize");
        respond(stream, json!({"id": request["id"], "result": {}}));
        let initialized = read_client_frame(stream);
        let initialized: Value = serde_json::from_slice(&initialized).unwrap();
        assert_eq!(initialized["method"], "initialized");
    }

    fn next_request(stream: &mut UnixStream) -> Value {
        serde_json::from_slice(&read_client_frame(stream)).unwrap()
    }

    fn next_request_id(stream: &mut UnixStream) -> Value {
        next_request(stream)["id"].clone()
    }

    fn respond(stream: &mut UnixStream, value: Value) {
        stream
            .write_all(&encode_frame(0x1, &serde_json::to_vec(&value).unwrap()))
            .unwrap();
    }

    fn read_client_frame(stream: &mut UnixStream) -> Vec<u8> {
        try_read_client_frame(stream).unwrap()
    }

    fn try_read_client_frame(stream: &mut UnixStream) -> std::io::Result<Vec<u8>> {
        let mut header = [0_u8; 2];
        stream.read_exact(&mut header)?;
        let masked = header[1] & 0x80 != 0;
        let mut length = (header[1] & 0x7f) as usize;
        if length == 126 {
            let mut bytes = [0_u8; 2];
            stream.read_exact(&mut bytes)?;
            length = u16::from_be_bytes(bytes) as usize;
        }
        let mut mask = [0_u8; 4];
        if masked {
            stream.read_exact(&mut mask)?;
        }
        let mut payload = vec![0_u8; length];
        stream.read_exact(&mut payload)?;
        if masked {
            for (index, byte) in payload.iter_mut().enumerate() {
                *byte ^= mask[index % 4];
            }
        }
        Ok(payload)
    }
}
