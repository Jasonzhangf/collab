//! Codex App Server transport over the host-owned Unix WebSocket endpoint.
//!
//! This module speaks the native JSON-RPC surface exposed by Codex TUI and
//! Desktop. It does not start an App Server, invent a namespace, or treat
//! `thread/queue/add` acceptance as delivery. Every operation is bounded and
//! preserves the exact native error on failure.

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
            "thread/queue/add",
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
            "accepted_semantics": "thread/queue/add accepted is not delivered, executed, replied, or read"
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
    // Item history is a diagnostic capability, not a registration or wake
    // requirement. Some App Server builds expose thread/read and queue/add but
    // return method-not-found for items/list; that must not block peer
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
    let send_message = method_exists(
        &mut client,
        "thread/queue/add",
        json!({"threadId": "", "input": []}),
    )?;
    if !send_message {
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
            "wait_reply".into(),
        ],
        self_check: "initialize, thread/read identity, and thread/queue/add method probe passed"
            .into(),
    })
}

/// Queue one notification through a server-selected App Server transport.
/// A successful result means the native App Server accepted the queue write;
/// it does not mean the target turn was executed, read, or answered.
pub fn queue_add(
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
                    "version": env!("CARGO_PKG_VERSION")
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
        assert!(selected.self_check.contains("thread/queue/add"));
    }

    fn read_client_frame(stream: &mut UnixStream) -> Vec<u8> {
        let mut header = [0_u8; 2];
        stream.read_exact(&mut header).unwrap();
        let masked = header[1] & 0x80 != 0;
        let mut length = (header[1] & 0x7f) as usize;
        if length == 126 {
            let mut bytes = [0_u8; 2];
            stream.read_exact(&mut bytes).unwrap();
            length = u16::from_be_bytes(bytes) as usize;
        }
        let mut mask = [0_u8; 4];
        if masked {
            stream.read_exact(&mut mask).unwrap();
        }
        let mut payload = vec![0_u8; length];
        stream.read_exact(&mut payload).unwrap();
        if masked {
            for (index, byte) in payload.iter_mut().enumerate() {
                *byte ^= mask[index % 4];
            }
        }
        payload
    }
}
