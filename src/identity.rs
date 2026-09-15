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
    match transport.kind {
        TransportKind::AppServer => {
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
            if transport.pane.is_some() {
                anyhow::bail!("selected App Server transport unexpectedly carries a tmux pane");
            }
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
                anyhow::bail!(
                    "selected App Server transport has unsupported namespace {namespace}"
                );
            }
            if !endpoint.starts_with("unix://") {
                anyhow::bail!("selected App Server transport endpoint is not unix://");
            }
        }
        TransportKind::Tmux => {
            let pane = transport
                .pane
                .as_deref()
                .filter(|value| value.starts_with('%'))
                .ok_or_else(|| anyhow::anyhow!("selected tmux transport has no live pane"))?;
            if transport.endpoint.is_some()
                || transport.namespace.is_some()
                || transport.thread_id.is_some()
            {
                anyhow::bail!("selected tmux transport carries App Server fields");
            }
            if runtime.native_thread_id.is_some() {
                anyhow::bail!("selected tmux transport unexpectedly carries a native thread");
            }
            let _ = pane;
        }
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
    pub pane: Option<String>,
    #[serde(default)]
    pub session: Option<String>,
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

fn pane_identity_path(scope: &Scope, pane: &str) -> PathBuf {
    scope
        .root
        .join(".agent-collab")
        .join("panes")
        .join(format!("{}.json", pane_file_name(pane)))
}

fn session_identity_path(scope: &Scope, session: &str) -> PathBuf {
    scope
        .root
        .join(".agent-collab")
        .join("runs")
        .join("by-pane")
        .join(format!("tmux-{}.json", pane_file_name(session)))
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
    if let Some(session) = ident.session.as_deref() {
        write_identity(&session_identity_path(scope, session), ident)?;
    }
    if let Some(pane) = ident.pane.as_deref() {
        write_identity(&pane_identity_path(scope, pane), ident)?;
    }
    Ok(())
}

/// Update the live endpoint from an explicit pane/session binding.  A changed
/// endpoint is a new runtime registration boundary; the old typed binding must
/// not be reused for a later command envelope.
fn refresh_endpoint(ident: &mut Identity, pane: &str, session: Option<&str>) -> bool {
    let changed = ident.pane.as_deref() != Some(pane)
        || session.is_some_and(|session| ident.session.as_deref() != Some(session));
    ident.pane = Some(pane.to_owned());
    if let Some(session) = session {
        ident.session = Some(session.to_owned());
    }
    if changed {
        ident.runtime = None;
    }
    changed
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

fn pane_file_name(pane: &str) -> String {
    pane.chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.') {
                c
            } else {
                '_'
            }
        })
        .collect()
}

fn tmux_session(pane: &str) -> Option<String> {
    let output = std::process::Command::new("tmux")
        .args(["display-message", "-p", "-t", pane, "#S"])
        .output()
        .ok()?;
    if !output.status.success() {
        return None;
    }
    let session = String::from_utf8_lossy(&output.stdout).trim().to_string();
    (!session.is_empty()).then_some(session)
}

/// Parent/daemon binds a live child pane so the agent does not self-register.
pub fn provision(
    scope: &Scope,
    worker_id: &str,
    pane: &str,
    session: &str,
) -> anyhow::Result<Identity> {
    if !pane.starts_with('%') {
        anyhow::bail!("collab identity requires a live tmux pane");
    }
    if worker_id.is_empty() || session.is_empty() {
        anyhow::bail!("collab identity requires a session name");
    }
    let mut ident = read_identity(&identity_path(scope, worker_id))?.unwrap_or_else(|| Identity {
        worker_id: worker_id.into(),
        token: hex(16),
        pane: Some(pane.into()),
        session: Some(session.into()),
        runtime: None,
        transport: None,
    });
    refresh_endpoint(&mut ident, pane, Some(session));
    persist_identity(scope, &ident)?;
    Ok(ident)
}

/// Load or create one tmux-session identity. Commands outside tmux fail before
/// writing a run identity, so diagnostics cannot accidentally declare a peer.
/// A parent-provisioned pane binding is enough; tmux lookup is not required.
pub fn load_or_create(
    scope: &Scope,
    worker_id: Option<String>,
    pane_override: Option<String>,
) -> anyhow::Result<Identity> {
    let pane_override_explicit = pane_override.is_some();
    let pane = pane_override.or_else(|| std::env::var("TMUX_PANE").ok());
    load_or_create_resolved(scope, worker_id, pane, pane_override_explicit)
}

/// Load or create the identity used by `collab init`. Initialization binds to
/// the process cwd and never depends on tmux: an advertised pane is only
/// offered as a capability candidate when it is actually live, and the server
/// still assigns the channel with App Server preferred.
pub fn load_or_create_for_init(
    scope: &Scope,
    appserver_available: bool,
) -> anyhow::Result<Identity> {
    load_or_create_for_init_with(
        scope,
        appserver_available,
        std::env::var("TMUX_PANE").ok(),
        None,
        |pane| tmux_session(pane).is_some(),
    )
}

fn load_or_create_for_init_with<F>(
    scope: &Scope,
    appserver_available: bool,
    tmux_pane: Option<String>,
    requested_worker: Option<String>,
    pane_is_live: F,
) -> anyhow::Result<Identity>
where
    F: FnOnce(&str) -> bool,
{
    let live_pane = init_pane_candidate(tmux_pane, appserver_available, pane_is_live);
    let mut ident = load_or_create_resolved(scope, requested_worker, live_pane.clone(), false)?;
    if !persisted_binding_matches_current_candidates(
        &ident,
        appserver_available,
        live_pane.as_deref(),
    ) {
        // A persisted binding may only be reused when it matches a currently
        // verified candidate: App Server when a thread is available, or the
        // exact live pane. A stale/dead pane or a now-available App Server
        // must not silently keep the old channel. Preserve the worker identity
        // and token, clear only the endpoint binding, and let registration
        // re-run with the server still choosing the channel.
        ident.runtime = None;
        ident.transport = None;
        persist_identity(scope, &ident)?;
    }
    Ok(ident)
}

/// Init reuses a persisted endpoint only when it is one of the candidates the
/// server could admit right now. App Server is preferred; tmux is accepted
/// only for the exact pane proven live in this run.
fn persisted_binding_matches_current_candidates(
    ident: &Identity,
    appserver_available: bool,
    live_pane: Option<&str>,
) -> bool {
    match ident.transport.as_ref().map(|transport| &transport.kind) {
        Some(TransportKind::AppServer) => appserver_available,
        Some(TransportKind::Tmux) => {
            !appserver_available && live_pane.is_some() && ident.pane.as_deref() == live_pane
        }
        None => true,
    }
}

/// App Server outranks tmux, so a pane is never probed when an App Server
/// thread is available. Otherwise a pane is a candidate only when it is
/// advertised and resolves live; a stale or missing pane is ignored rather
/// than blocking initialization.
fn init_pane_candidate<F>(
    tmux_pane: Option<String>,
    appserver_available: bool,
    pane_is_live: F,
) -> Option<String>
where
    F: FnOnce(&str) -> bool,
{
    if appserver_available {
        return None;
    }
    let pane = tmux_pane.filter(|pane| pane.starts_with('%'))?;
    pane_is_live(&pane).then_some(pane)
}

fn load_or_create_resolved(
    scope: &Scope,
    worker_id: Option<String>,
    pane: Option<String>,
    pane_override_explicit: bool,
) -> anyhow::Result<Identity> {
    let requested = worker_id
        .or_else(|| std::env::var("COLLAB_WORKER").ok())
        .or_else(|| {
            std::env::var("CODEX_THREAD_ID")
                .ok()
                .filter(|value| !value.trim().is_empty())
                .map(|thread_id| format!("codex-{thread_id}"))
        });
    if let Some(pane) = pane.filter(|pane| pane.starts_with('%')) {
        if let Some(mut ident) = read_identity(&pane_identity_path(scope, &pane))? {
            if refresh_endpoint(&mut ident, &pane, None) {
                persist_identity(scope, &ident)?;
            }
            return Ok(ident);
        }
        if let Some(worker_id) = requested.clone() {
            if let Some(mut ident) = read_identity(&identity_path(scope, &worker_id))? {
                if refresh_endpoint(&mut ident, &pane, None) {
                    persist_identity(scope, &ident)?;
                }
                return Ok(ident);
            }
        }
        let session = tmux_session(&pane)
            .ok_or_else(|| anyhow::anyhow!("cannot resolve tmux session for pane {}", pane))?;
        if let Some(mut ident) = read_identity(&session_identity_path(scope, &session))? {
            refresh_endpoint(&mut ident, &pane, Some(&session));
            persist_identity(scope, &ident)?;
            return Ok(ident);
        }
        let ident = Identity {
            worker_id: requested.unwrap_or_else(|| session.clone()),
            token: hex(16),
            pane: Some(pane),
            session: Some(session),
            runtime: None,
            transport: None,
        };
        persist_identity(scope, &ident)?;
        return Ok(ident);
    }
    if pane_override_explicit {
        anyhow::bail!("collab identity requires a live tmux pane");
    }
    let worker_id = requested.ok_or_else(|| {
        anyhow::anyhow!("collab identity requires COLLAB_WORKER or CODEX_THREAD_ID outside tmux")
    })?;
    if let Some(ident) = read_identity(&identity_path(scope, &worker_id))? {
        return Ok(ident);
    }
    let ident = Identity {
        worker_id,
        token: hex(16),
        pane: None,
        session: None,
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
    fn non_tmux_diagnostic_cannot_declare_identity() {
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
        let result = load_or_create(&scope, None, Some("not-tmux".into()));
        assert!(result.is_err());
        assert_eq!(
            std::fs::read_dir(root.join(".agent-collab/runs"))
                .unwrap()
                .count(),
            0
        );
        std::fs::remove_dir_all(root).ok();
    }

    #[test]
    fn provisioned_pane_identity_does_not_need_tmux() {
        let root = std::env::temp_dir().join(format!(
            "collab-identity-provision-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(root.join(".agent-collab")).unwrap();
        let scope = Scope { root: root.clone() };
        let provisioned = provision(&scope, "child-peer", "%743", "child-peer").unwrap();
        let loaded = load_or_create(&scope, None, Some("%743".into())).unwrap();
        assert_eq!(loaded.worker_id, "child-peer");
        assert_eq!(loaded.token, provisioned.token);
        assert_eq!(loaded.pane.as_deref(), Some("%743"));
        std::fs::remove_dir_all(root).ok();
    }

    #[test]
    fn endpoint_rebind_clears_runtime_and_updates_current_mirrors() {
        let root = std::env::temp_dir().join(format!(
            "collab-identity-rebind-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(root.join(".agent-collab")).unwrap();
        let scope = Scope { root: root.clone() };

        let mut registered = provision(&scope, "agent-1", "%743", "session-1").unwrap();
        let runtime = RuntimeIdentity {
            native_thread_id: None,
            ..runtime_identity(7, "binding-7")
        };
        persist_runtime(&scope, &mut registered, runtime.clone()).unwrap();

        let same_endpoint =
            load_or_create(&scope, Some("agent-1".into()), Some("%743".into())).unwrap();
        assert_eq!(same_endpoint.runtime, Some(runtime));

        let rebound = load_or_create(&scope, Some("agent-1".into()), Some("%744".into())).unwrap();
        assert_eq!(rebound.pane.as_deref(), Some("%744"));
        assert_eq!(rebound.session.as_deref(), Some("session-1"));
        assert_eq!(rebound.runtime, None);

        for path in [
            identity_path(&scope, "agent-1"),
            session_identity_path(&scope, "session-1"),
            pane_identity_path(&scope, "%744"),
        ] {
            let mirror = read_identity(&path).unwrap().unwrap();
            assert_eq!(mirror.worker_id, rebound.worker_id);
            assert_eq!(mirror.token, rebound.token);
            assert_eq!(mirror.pane, rebound.pane);
            assert_eq!(mirror.session, rebound.session);
            assert_eq!(mirror.runtime, None);
        }

        let mut session_rebound = rebound;
        let session_runtime = runtime_identity(8, "binding-8");
        persist_runtime(&scope, &mut session_rebound, session_runtime).unwrap();
        let session_changed = provision(&scope, "agent-1", "%744", "session-2").unwrap();
        assert_eq!(session_changed.runtime, None);
        assert_eq!(session_changed.session.as_deref(), Some("session-2"));
        assert_eq!(
            read_identity(&session_identity_path(&scope, "session-2"))
                .unwrap()
                .unwrap()
                .runtime,
            None
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
                    "runtime_id": "runtime-pane-1",
                    "binding_id": "binding-1",
                    "endpoint_generation": 4,
                    "native_thread_id": "thread-1"
                }
            }
        });

        let runtime = runtime_from_registration_receipt(&receipt, "worker-1", &root).unwrap();
        assert_eq!(runtime.agent_id.as_str(), "worker-1");
        assert_eq!(runtime.runtime_id.as_str(), "runtime-pane-1");
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
    fn registration_receipt_recovers_a_non_cli_app_scope() {
        let root = std::env::temp_dir().join(format!(
            "collab-registration-tui-receipt-{}-{}",
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
                "kind": "tmux",
                "pane": "%1",
                "capabilities": ["send_message"],
                "self_check": "server verified"
            },
            "command": {
                "cmd": "RegisterWorker",
                "binding": {
                    "project_scope": canonical_root.to_str().unwrap(),
                    "app_scope_id": "tui-default",
                    "agent_id": "worker-1",
                    "runtime_id": "runtime-tui",
                    "binding_id": "binding-tui",
                    "endpoint_generation": 4
                }
            }
        });

        let runtime = runtime_from_registration_receipt(&receipt, "worker-1", &root).unwrap();
        assert_eq!(runtime.appserver_id.as_str(), "tui-default");
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
    fn appserver_identity_can_be_created_without_tmux() {
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
        let identity =
            load_or_create_resolved(&scope, Some("thread-worker".into()), None, false).unwrap();
        assert_eq!(identity.worker_id, "thread-worker");
        assert_eq!(identity.pane, None);
        assert_eq!(identity.runtime, None);
        assert_eq!(identity.transport, None);
        std::fs::remove_dir_all(root).ok();
    }

    #[test]
    fn init_prefers_appserver_over_an_advertised_tmux_pane() {
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
        let mut ident = provision(&scope, "codex-thread-1", "%743", "session-1").unwrap();
        persist_registration(
            &scope,
            &mut ident,
            RuntimeIdentity {
                native_thread_id: None,
                ..runtime_identity(4, "binding-tmux")
            },
            SelectedTransport {
                kind: TransportKind::Tmux,
                endpoint: None,
                namespace: None,
                thread_id: None,
                pane: Some("%743".into()),
                capabilities: vec!["send_message_to_thread".into()],
                self_check: "ok".into(),
            },
        )
        .unwrap();

        let resolved = load_or_create_for_init_with(
            &scope,
            true,
            Some("%743".into()),
            Some("codex-thread-1".into()),
            |_| panic!("tmux must not be probed when App Server is available"),
        )
        .unwrap();
        assert_eq!(resolved.worker_id, "codex-thread-1");
        assert_eq!(resolved.token, ident.token);
        assert_eq!(resolved.runtime, None);
        assert_eq!(resolved.transport, None);
        std::fs::remove_dir_all(root).ok();
    }

    #[test]
    fn init_uses_a_live_tmux_pane_when_appserver_is_unavailable() {
        let selected = init_pane_candidate(Some("%7".into()), false, |pane| pane == "%7");
        assert_eq!(selected.as_deref(), Some("%7"));
    }

    #[test]
    fn init_ignores_a_stale_tmux_pane_without_appserver() {
        let selected = init_pane_candidate(Some("%7".into()), false, |_| false);
        assert_eq!(selected, None);
    }

    #[test]
    fn init_does_not_probe_tmux_when_appserver_is_available() {
        let selected = init_pane_candidate(Some("%7".into()), true, |_| {
            panic!("tmux must not be probed when App Server is available")
        });
        assert_eq!(selected, None);
    }

    #[test]
    fn init_clears_a_persisted_tmux_binding_when_the_pane_is_stale() {
        let root = std::env::temp_dir().join(format!(
            "collab-init-stale-pane-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(root.join(".agent-collab")).unwrap();
        let scope = Scope { root: root.clone() };
        let mut ident = provision(&scope, "codex-thread-2", "%743", "session-1").unwrap();
        persist_registration(
            &scope,
            &mut ident,
            RuntimeIdentity {
                native_thread_id: None,
                ..runtime_identity(5, "binding-tmux-stale")
            },
            SelectedTransport {
                kind: TransportKind::Tmux,
                endpoint: None,
                namespace: None,
                thread_id: None,
                pane: Some("%743".into()),
                capabilities: vec!["send_message_to_thread".into()],
                self_check: "ok".into(),
            },
        )
        .unwrap();

        let resolved = load_or_create_for_init_with(
            &scope,
            false,
            Some("%743".into()),
            Some("codex-thread-2".into()),
            |_| false,
        )
        .unwrap();
        assert_eq!(resolved.worker_id, "codex-thread-2");
        assert_eq!(resolved.token, ident.token);
        assert_eq!(resolved.runtime, None);
        assert_eq!(resolved.transport, None);
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
            pane: None,
            session: None,
            runtime: None,
            transport: None,
        };
        let runtime = RuntimeIdentity {
            native_thread_id: None,
            ..runtime_identity(7, "binding-7")
        };
        persist_registration(
            &scope,
            &mut identity,
            runtime.clone(),
            SelectedTransport {
                kind: TransportKind::Tmux,
                endpoint: None,
                namespace: None,
                thread_id: None,
                pane: Some("%7".into()),
                capabilities: vec!["send_message".into()],
                self_check: "server verified".into(),
            },
        )
        .unwrap();
        assert_eq!(identity.runtime, Some(runtime.clone()));
        assert_eq!(
            identity.transport.as_ref().unwrap().kind,
            TransportKind::Tmux
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
