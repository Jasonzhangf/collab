use crate::proto::{SelectedTransport, TransportKind};
use crate::scope::Scope;
use anyhow::Context;
use rand::Rng;
use serde::{Deserialize, Serialize};
use std::fmt;
use std::path::{Path, PathBuf};

macro_rules! string_id {
    ($name:ident) => {
        #[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
        #[serde(transparent)]
        pub struct $name(String);

        impl $name {
            pub fn new(value: impl Into<String>) -> anyhow::Result<Self> {
                let value = value.into();
                validate_id(&value)?;
                Ok(Self(value))
            }

            pub fn as_str(&self) -> &str {
                &self.0
            }
        }

        impl fmt::Display for $name {
            fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                f.write_str(&self.0)
            }
        }
    };
}

const MAX_ID_LENGTH: usize = 256;

fn validate_id(value: &str) -> anyhow::Result<()> {
    if value.is_empty() {
        anyhow::bail!("identifier must not be empty");
    }
    if value.len() > MAX_ID_LENGTH {
        anyhow::bail!("identifier exceeds {MAX_ID_LENGTH} bytes");
    }
    if value.chars().any(char::is_control) {
        anyhow::bail!("identifier must not contain control characters");
    }
    Ok(())
}

string_id!(AgentId);
string_id!(RuntimeId);
string_id!(AppServerId);
string_id!(BindingId);
string_id!(NativeThreadId);
string_id!(TurnId);
string_id!(MessageId);
string_id!(DispatchId);
string_id!(CommandId);
string_id!(OperationId);

pub(crate) fn validate_id_for_protocol(value: &str) -> anyhow::Result<()> {
    validate_id(value)
}

/// The stable AppServer route owned by the CLI adapter. A first registration
/// has no persisted runtime binding yet, so it may use this contract only to
/// construct the request context. Existing bindings retain the AppServer
/// scope established by their native endpoint.
pub const CLI_APP_SERVER_ID: &str = "appserver-cli";

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RuntimeIdentity {
    pub agent_id: AgentId,
    pub runtime_id: RuntimeId,
    pub appserver_id: AppServerId,
    pub endpoint_generation: u64,
    pub binding_id: BindingId,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub native_thread_id: Option<NativeThreadId>,
}

fn validate_registration_transport(
    transport: &SelectedTransport,
    runtime: &RuntimeIdentity,
) -> anyhow::Result<()> {
    if transport.kind != TransportKind::AppServer {
        anyhow::bail!("only the App Server transport is supported");
    }
    let endpoint = transport
        .endpoint
        .as_deref()
        .filter(|value| !value.is_empty())
        .ok_or_else(|| anyhow::anyhow!("selected App Server transport has no endpoint"))?;
    let namespace = transport
        .namespace
        .as_deref()
        .filter(|value| !value.is_empty())
        .ok_or_else(|| anyhow::anyhow!("selected App Server transport has no namespace"))?;
    let thread_id = transport
        .thread_id
        .as_deref()
        .filter(|value| !value.is_empty())
        .ok_or_else(|| anyhow::anyhow!("selected App Server transport has no thread_id"))?;
    if runtime
        .native_thread_id
        .as_ref()
        .map(NativeThreadId::as_str)
        != Some(thread_id)
    {
        anyhow::bail!(
            "selected App Server thread_id does not match typed binding native_thread_id"
        );
    }
    if !matches!(namespace, "codex_tui" | "codex_app") {
        anyhow::bail!("selected App Server transport has unsupported namespace {namespace}");
    }
    if !endpoint.starts_with("unix://") {
        anyhow::bail!("selected App Server transport endpoint is not unix://");
    }
    if transport.self_check.trim().is_empty() {
        anyhow::bail!("selected transport is missing its server self-check");
    }
    Ok(())
}

impl RuntimeIdentity {
    /// Construct the only provisional identity permitted before registration.
    /// Its binding and generation are never treated as a registered runtime;
    /// they exist solely so the first Register request can carry a validated
    /// app/project context.
    pub fn cli_adapter(worker_id: &str) -> anyhow::Result<Self> {
        let identity = Self {
            agent_id: AgentId::new(worker_id.to_owned())?,
            runtime_id: RuntimeId::new(format!("runtime-{worker_id}"))?,
            appserver_id: AppServerId::new(CLI_APP_SERVER_ID)?,
            endpoint_generation: 0,
            binding_id: BindingId::new(format!("binding-{worker_id}"))?,
            native_thread_id: None,
        };
        identity.validate()?;
        Ok(identity)
    }

    pub fn validate(&self) -> anyhow::Result<()> {
        validate_id(self.agent_id.as_str())?;
        validate_id(self.runtime_id.as_str())?;
        validate_id(self.appserver_id.as_str())?;
        validate_id(self.binding_id.as_str())?;
        if let Some(native_thread_id) = &self.native_thread_id {
            validate_id(native_thread_id.as_str())?;
        }
        Ok(())
    }
}

#[derive(Debug, Deserialize)]
struct RegistrationBinding {
    project_scope: String,
    app_scope_id: AppServerId,
    agent_id: AgentId,
    runtime_id: RuntimeId,
    binding_id: BindingId,
    endpoint_generation: u64,
    #[serde(default)]
    native_thread_id: Option<NativeThreadId>,
}

/// Recover the current runtime identity from the typed Register response.
///
/// The daemon may include a human-readable runtime channel beside the typed
/// command. That channel is deliberately ignored: only
/// `typed.command.binding` contains the runtime/binding fields that authorize
/// later commands. The project root and worker are checked before the
/// identity can be persisted; the caller compares the returned app scope with
/// the scope used for its request.
pub fn runtime_from_registration_receipt(
    receipt: &serde_json::Value,
    expected_worker_id: &str,
    expected_root: &Path,
) -> anyhow::Result<RuntimeIdentity> {
    registration_from_receipt(receipt, expected_worker_id, expected_root)
        .map(|(runtime, _)| runtime)
}

pub fn registration_from_receipt(
    receipt: &serde_json::Value,
    expected_worker_id: &str,
    expected_root: &Path,
) -> anyhow::Result<(RuntimeIdentity, SelectedTransport)> {
    if receipt.get("typed").and_then(serde_json::Value::as_bool) != Some(true) {
        anyhow::bail!("registration receipt is missing typed=true");
    }
    let worker_id = receipt
        .get("worker_id")
        .and_then(serde_json::Value::as_str)
        .ok_or_else(|| anyhow::anyhow!("registration receipt is missing worker_id"))?;
    if worker_id != expected_worker_id {
        anyhow::bail!(
            "registration receipt worker_id mismatch: expected {expected_worker_id}, observed {worker_id}"
        );
    }

    let binding_value = receipt
        .get("command")
        .and_then(serde_json::Value::as_object)
        .and_then(|command| command.get("binding"))
        .cloned()
        .ok_or_else(|| anyhow::anyhow!("registration receipt is missing typed command.binding"))?;
    let binding: RegistrationBinding = serde_json::from_value(binding_value)
        .context("registration receipt command.binding has an invalid typed shape")?;

    let canonical_root = std::fs::canonicalize(expected_root)?;
    let canonical_root = canonical_root.to_str().ok_or_else(|| {
        anyhow::anyhow!("registered project root must be valid UTF-8 for the wire context")
    })?;
    if binding.project_scope != canonical_root {
        anyhow::bail!(
            "registration receipt project scope mismatch: expected {canonical_root}, observed {}",
            binding.project_scope
        );
    }
    let runtime = RuntimeIdentity {
        agent_id: binding.agent_id,
        runtime_id: binding.runtime_id,
        appserver_id: binding.app_scope_id,
        endpoint_generation: binding.endpoint_generation,
        binding_id: binding.binding_id,
        native_thread_id: binding.native_thread_id,
    };
    runtime.validate()?;
    if runtime.agent_id.as_str() != expected_worker_id {
        anyhow::bail!(
            "registration receipt binding agent mismatch: expected {expected_worker_id}, observed {}",
            runtime.agent_id
        );
    }
    let selected_value = receipt
        .get("transport_selected")
        .ok_or_else(|| anyhow::anyhow!("registration receipt is missing transport_selected"))?;
    if !selected_value.is_object() {
        anyhow::bail!("registration receipt transport_selected must be a JSON object");
    }
    let selected: SelectedTransport = serde_json::from_value(selected_value.clone())
        .context("registration receipt transport_selected has an invalid typed shape")?;
    validate_registration_transport(&selected, &runtime)?;
    Ok((runtime, selected))
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum BindingValidationError {
    StaleGeneration {
        expected: u64,
        observed: u64,
    },
    Mismatch {
        field: &'static str,
        expected: String,
        observed: String,
    },
}

impl fmt::Display for BindingValidationError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::StaleGeneration { expected, observed } => {
                write!(
                    f,
                    "stale endpoint generation: expected {expected}, observed {observed}"
                )
            }
            Self::Mismatch {
                field,
                expected,
                observed,
            } => write!(
                f,
                "runtime binding mismatch for {field}: expected {expected}, observed {observed}"
            ),
        }
    }
}

impl std::error::Error for BindingValidationError {}

/// Compare an incoming runtime binding with the currently registered binding.
/// This is intentionally pure: callers decide whether a failed command is
/// rejected or whether a separately authorized reconnect should rebind.
pub fn validate_binding(
    registered: &RuntimeIdentity,
    incoming: &RuntimeIdentity,
) -> Result<(), BindingValidationError> {
    if registered.endpoint_generation != incoming.endpoint_generation {
        return Err(BindingValidationError::StaleGeneration {
            expected: registered.endpoint_generation,
            observed: incoming.endpoint_generation,
        });
    }
    for (field, expected, observed) in [
        (
            "agent_id",
            registered.agent_id.as_str(),
            incoming.agent_id.as_str(),
        ),
        (
            "runtime_id",
            registered.runtime_id.as_str(),
            incoming.runtime_id.as_str(),
        ),
        (
            "appserver_id",
            registered.appserver_id.as_str(),
            incoming.appserver_id.as_str(),
        ),
        (
            "binding_id",
            registered.binding_id.as_str(),
            incoming.binding_id.as_str(),
        ),
    ] {
        if expected != observed {
            return Err(BindingValidationError::Mismatch {
                field,
                expected: expected.into(),
                observed: observed.into(),
            });
        }
    }
    if registered.native_thread_id != incoming.native_thread_id {
        return Err(BindingValidationError::Mismatch {
            field: "native_thread_id",
            expected: registered
                .native_thread_id
                .as_ref()
                .map(ToString::to_string)
                .unwrap_or_default(),
            observed: incoming
                .native_thread_id
                .as_ref()
                .map(ToString::to_string)
                .unwrap_or_default(),
        });
    }
    Ok(())
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Identity {
    pub worker_id: String,
    pub token: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub runtime: Option<RuntimeIdentity>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub transport: Option<SelectedTransport>,
}

fn hex(n: usize) -> String {
    (0..n)
        .map(|_| format!("{:02x}", rand::thread_rng().gen::<u8>()))
        .collect()
}

fn identity_path(scope: &Scope, worker_id: &str) -> PathBuf {
    scope
        .root
        .join(".agent-collab")
        .join("runs")
        .join(worker_id)
        .join("identity.json")
}

fn write_identity(path: &std::path::Path, ident: &Identity) -> anyhow::Result<()> {
    let dir = path.parent().unwrap();
    std::fs::create_dir_all(dir)?;
    let tmp = dir.join("identity.json.tmp");
    std::fs::write(&tmp, serde_json::to_string_pretty(ident)?)?;
    std::fs::rename(&tmp, path)?;
    Ok(())
}

fn persist_identity(scope: &Scope, ident: &Identity) -> anyhow::Result<()> {
    write_identity(&identity_path(scope, &ident.worker_id), ident)?;
    Ok(())
}

/// Persist a runtime binding recovered from a successful typed registration.
/// Update the in-memory identity only after every mirrored identity file has
/// been written successfully.
pub fn persist_runtime(
    scope: &Scope,
    ident: &mut Identity,
    runtime: RuntimeIdentity,
) -> anyhow::Result<()> {
    runtime.validate()?;
    if runtime.agent_id.as_str() != ident.worker_id {
        anyhow::bail!(
            "runtime binding agent does not match identity worker: expected {}, observed {}",
            ident.worker_id,
            runtime.agent_id
        );
    }
    let mut updated = ident.clone();
    updated.runtime = Some(runtime);
    persist_identity(scope, &updated)?;
    *ident = updated;
    Ok(())
}

/// Persist the server-selected transport alongside the typed runtime binding.
/// The selection is an output of server admission, never a client preference.
pub fn persist_registration(
    scope: &Scope,
    ident: &mut Identity,
    runtime: RuntimeIdentity,
    transport: SelectedTransport,
) -> anyhow::Result<()> {
    validate_registration_transport(&transport, &runtime)?;
    let mut updated = ident.clone();
    updated.runtime = Some(runtime);
    updated.transport = Some(transport);
    persist_identity(scope, &updated)?;
    *ident = updated;
    Ok(())
}

fn read_identity(path: &std::path::Path) -> anyhow::Result<Option<Identity>> {
    if !path.exists() {
        return Ok(None);
    }
    Ok(Some(serde_json::from_str(&std::fs::read_to_string(path)?)?))
}

/// Load or create one Codex thread identity.
pub fn load_or_create(
    scope: &Scope,
    worker_id: Option<String>,
    _endpoint_override: Option<String>,
) -> anyhow::Result<Identity> {
    let _ = _endpoint_override;
    load_or_create_resolved(scope, worker_id)
}

/// Load or create the identity used by `collab init`. Initialization binds to
/// the process cwd and the Codex thread. The server remains the sole owner of
/// channel assignment, so init always re-registers instead of reusing a
/// persisted binding.
pub fn load_or_create_for_init(scope: &Scope) -> anyhow::Result<Identity> {
    let mut ident = load_or_create_resolved(scope, None)?;
    if ident.runtime.is_some() || ident.transport.is_some() {
        // A persisted binding may describe a channel the server has not
        // re-admitted in this environment. Preserve the worker identity and
        // token, clear the binding, and let registration run the server-owned
        // self-check and channel selection again.
        ident.runtime = None;
        ident.transport = None;
        persist_identity(scope, &ident)?;
    }
    Ok(ident)
}

fn load_or_create_resolved(scope: &Scope, worker_id: Option<String>) -> anyhow::Result<Identity> {
    let requested = worker_id
        .or_else(|| std::env::var("COLLAB_WORKER").ok())
        .or_else(|| {
            std::env::var("CODEX_THREAD_ID")
                .ok()
                .filter(|value| !value.trim().is_empty())
                .map(|thread_id| format!("codex-{thread_id}"))
        });
    let worker_id = requested.ok_or_else(|| {
        anyhow::anyhow!("collab identity requires CODEX_THREAD_ID or an explicit worker id")
    })?;
    if let Some(ident) = read_identity(&identity_path(scope, &worker_id))? {
        return Ok(ident);
    }
    let ident = Identity {
        worker_id,
        token: hex(16),
        runtime: None,
        transport: None,
    };
    persist_identity(scope, &ident)?;
    Ok(ident)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn identity_requires_a_codex_thread_when_no_worker_is_given() {
        let root = std::env::temp_dir().join(format!(
            "collab-identity-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(root.join(".agent-collab/runs")).unwrap();
        let scope = Scope { root: root.clone() };
        let previous_thread = std::env::var_os("CODEX_THREAD_ID");
        let previous_worker = std::env::var_os("COLLAB_WORKER");
        std::env::remove_var("CODEX_THREAD_ID");
        std::env::remove_var("COLLAB_WORKER");
        let result = load_or_create(&scope, None, None);
        if let Some(value) = previous_thread {
            std::env::set_var("CODEX_THREAD_ID", value);
        }
        if let Some(value) = previous_worker {
            std::env::set_var("COLLAB_WORKER", value);
        }
        assert!(result.is_err());
        assert_eq!(
            std::fs::read_dir(root.join(".agent-collab/runs"))
                .unwrap()
                .count(),
            0
        );
        std::fs::remove_dir_all(root).ok();
    }

    fn runtime_identity(generation: u64, binding: &str) -> RuntimeIdentity {
        RuntimeIdentity {
            agent_id: AgentId::new("agent-1").unwrap(),
            runtime_id: RuntimeId::new("runtime-1").unwrap(),
            appserver_id: AppServerId::new("appserver-1").unwrap(),
            endpoint_generation: generation,
            binding_id: BindingId::new(binding).unwrap(),
            native_thread_id: Some(NativeThreadId::new("thread-1").unwrap()),
        }
    }

    #[test]
    fn runtime_identity_serializes_typed_fields() {
        let identity = runtime_identity(3, "binding-1");
        let encoded = serde_json::to_value(&identity).unwrap();
        assert_eq!(
            encoded,
            serde_json::json!({
                "agent_id": "agent-1",
                "runtime_id": "runtime-1",
                "appserver_id": "appserver-1",
                "endpoint_generation": 3,
                "binding_id": "binding-1",
                "native_thread_id": "thread-1"
            })
        );
        assert_eq!(
            serde_json::from_value::<RuntimeIdentity>(encoded).unwrap(),
            identity
        );
    }

    #[test]
    fn cli_adapter_identity_has_one_stable_app_scope() {
        let first = RuntimeIdentity::cli_adapter("worker-1").unwrap();
        let second = RuntimeIdentity::cli_adapter("worker-1").unwrap();
        assert_eq!(first, second);
        assert_eq!(first.appserver_id.as_str(), CLI_APP_SERVER_ID);
        assert_eq!(first.endpoint_generation, 0);
    }

    #[test]
    fn registration_receipt_recovers_typed_command_binding() {
        let root = std::env::temp_dir().join(format!(
            "collab-registration-receipt-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(&root).unwrap();
        let canonical_root = std::fs::canonicalize(&root).unwrap();
        let receipt = serde_json::json!({
            "typed": true,
            "worker_id": "worker-1",
            "runtime": "untrusted-channel-label",
            "transport_selected": {
                "kind": "appserver",
                "endpoint": "unix:///tmp/codex.sock",
                "namespace": "codex_tui",
                "thread_id": "thread-1",
                "capabilities": ["session_status", "read_thread", "send_message"],
                "self_check": "server verified"
            },
            "command": {
                "cmd": "RegisterWorker",
                "binding": {
                    "project_scope": canonical_root.to_str().unwrap(),
                    "app_scope_id": CLI_APP_SERVER_ID,
                    "agent_id": "worker-1",
                    "runtime_id": "runtime-thread-1",
                    "binding_id": "binding-1",
                    "endpoint_generation": 4,
                    "native_thread_id": "thread-1"
                }
            }
        });

        let runtime = runtime_from_registration_receipt(&receipt, "worker-1", &root).unwrap();
        assert_eq!(runtime.agent_id.as_str(), "worker-1");
        assert_eq!(runtime.runtime_id.as_str(), "runtime-thread-1");
        assert_eq!(runtime.appserver_id.as_str(), CLI_APP_SERVER_ID);
        assert_eq!(runtime.endpoint_generation, 4);
        assert_eq!(runtime.binding_id.as_str(), "binding-1");
        assert_eq!(
            runtime.native_thread_id.as_ref().unwrap().as_str(),
            "thread-1"
        );
        std::fs::remove_dir_all(root).ok();
    }

    #[test]
    fn registration_receipt_rejects_missing_runtime_binding() {
        let root = std::env::temp_dir().join(format!(
            "collab-registration-missing-binding-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(&root).unwrap();
        let error = runtime_from_registration_receipt(
            &serde_json::json!({"typed": true, "worker_id": "worker-1"}),
            "worker-1",
            &root,
        )
        .unwrap_err();
        assert!(error.to_string().contains("typed command.binding"));
        std::fs::remove_dir_all(root).ok();
    }

    #[test]
    fn registration_receipt_rejects_transport_binding_mismatch() {
        let root = std::env::temp_dir().join(format!(
            "collab-registration-mismatch-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(&root).unwrap();
        let canonical_root = std::fs::canonicalize(&root).unwrap();
        let receipt = serde_json::json!({
            "typed": true,
            "worker_id": "worker-1",
            "transport_selected": {
                "kind": "appserver",
                "endpoint": "unix:///tmp/codex.sock",
                "namespace": "codex_tui",
                "thread_id": "thread-selected",
                "capabilities": ["send_message"],
                "self_check": "server verified"
            },
            "command": {
                "cmd": "RegisterWorker",
                "binding": {
                    "project_scope": canonical_root.to_str().unwrap(),
                    "app_scope_id": "app-1",
                    "agent_id": "worker-1",
                    "runtime_id": "runtime-1",
                    "binding_id": "binding-1",
                    "endpoint_generation": 1,
                    "native_thread_id": "thread-other"
                }
            }
        });
        let error = runtime_from_registration_receipt(&receipt, "worker-1", &root).unwrap_err();
        assert!(error.to_string().contains("native_thread_id"));
        std::fs::remove_dir_all(root).ok();
    }

    #[test]
    fn appserver_identity_uses_codex_thread() {
        let root = std::env::temp_dir().join(format!(
            "collab-identity-appserver-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(root.join(".agent-collab")).unwrap();
        let scope = Scope { root: root.clone() };
        let identity = load_or_create_resolved(&scope, Some("thread-worker".into())).unwrap();
        assert_eq!(identity.worker_id, "thread-worker");
        assert_eq!(identity.runtime, None);
        assert_eq!(identity.transport, None);
        std::fs::remove_dir_all(root).ok();
    }

    #[test]
    fn init_clears_a_persisted_binding_so_the_server_reassigns_the_channel() {
        let root = std::env::temp_dir().join(format!(
            "collab-init-priority-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(root.join(".agent-collab")).unwrap();
        let scope = Scope { root: root.clone() };
        let mut ident = load_or_create_resolved(&scope, Some("codex-thread-1".into())).unwrap();
        persist_registration(
            &scope,
            &mut ident,
            runtime_identity(4, "binding-appserver"),
            SelectedTransport {
                kind: TransportKind::AppServer,
                endpoint: Some("unix:///tmp/codex.sock".into()),
                namespace: Some("codex_tui".into()),
                thread_id: Some("thread-1".into()),
                capabilities: vec!["send_message".into()],
                self_check: "ok".into(),
            },
        )
        .unwrap();

        let previous_worker = std::env::var_os("COLLAB_WORKER");
        let previous_thread = std::env::var_os("CODEX_THREAD_ID");
        std::env::remove_var("CODEX_THREAD_ID");
        std::env::set_var("COLLAB_WORKER", "codex-thread-1");
        let resolved = load_or_create_for_init(&scope).unwrap();
        assert_eq!(resolved.worker_id, "codex-thread-1");
        assert_eq!(resolved.token, ident.token);
        assert_eq!(resolved.runtime, None);
        assert_eq!(resolved.transport, None);
        match previous_worker {
            Some(value) => std::env::set_var("COLLAB_WORKER", value),
            None => std::env::remove_var("COLLAB_WORKER"),
        }
        match previous_thread {
            Some(value) => std::env::set_var("CODEX_THREAD_ID", value),
            None => std::env::remove_var("CODEX_THREAD_ID"),
        }
        std::fs::remove_dir_all(root).ok();
    }

    #[test]
    fn persist_runtime_updates_all_identity_state_after_validation() {
        let root = std::env::temp_dir().join(format!(
            "collab-persist-runtime-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(root.join(".agent-collab")).unwrap();
        let scope = Scope { root: root.clone() };
        let mut identity = Identity {
            worker_id: "agent-1".into(),
            token: "token-1".into(),
            runtime: None,
            transport: None,
        };
        let runtime = runtime_identity(7, "binding-7");
        persist_registration(
            &scope,
            &mut identity,
            runtime.clone(),
            SelectedTransport {
                kind: TransportKind::AppServer,
                endpoint: Some("unix:///tmp/codex.sock".into()),
                namespace: Some("codex_tui".into()),
                thread_id: Some("thread-1".into()),
                capabilities: vec!["send_message".into()],
                self_check: "server verified".into(),
            },
        )
        .unwrap();
        assert_eq!(identity.runtime, Some(runtime.clone()));
        assert_eq!(
            identity.transport.as_ref().unwrap().kind,
            TransportKind::AppServer
        );
        let persisted = read_identity(&identity_path(&scope, "agent-1"))
            .unwrap()
            .unwrap();
        assert_eq!(persisted.runtime, Some(runtime));
        std::fs::remove_dir_all(root).ok();
    }

    #[test]
    fn binding_validation_rejects_stale_generation_without_mutating_inputs() {
        let registered = runtime_identity(4, "binding-1");
        let incoming = runtime_identity(3, "binding-1");
        let registered_before = registered.clone();
        let incoming_before = incoming.clone();

        assert!(matches!(
            validate_binding(&registered, &incoming),
            Err(BindingValidationError::StaleGeneration {
                expected: 4,
                observed: 3
            })
        ));
        assert_eq!(registered, registered_before);
        assert_eq!(incoming, incoming_before);
    }

    #[test]
    fn binding_validation_rejects_wrong_binding_without_mutating_inputs() {
        let registered = runtime_identity(4, "binding-1");
        let incoming = runtime_identity(4, "binding-2");
        let registered_before = registered.clone();
        let incoming_before = incoming.clone();

        assert!(matches!(
            validate_binding(&registered, &incoming),
            Err(BindingValidationError::Mismatch {
                field: "binding_id",
                ..
            })
        ));
        assert_eq!(registered, registered_before);
        assert_eq!(incoming, incoming_before);
    }

    #[test]
    fn identifier_validation_rejects_empty_and_control_values() {
        assert!(AgentId::new("").is_err());
        assert!(RuntimeId::new("runtime\n1").is_err());
        assert!(DispatchId::new("d".repeat(MAX_ID_LENGTH + 1)).is_err());
    }
}
