use crate::scope::Scope;
use rand::Rng;
use serde::{Deserialize, Serialize};
use std::fmt;
use std::path::PathBuf;

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

impl RuntimeIdentity {
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
    let ident = read_identity(&identity_path(scope, worker_id))?.unwrap_or_else(|| Identity {
        worker_id: worker_id.into(),
        token: hex(16),
        pane: Some(pane.into()),
        session: Some(session.into()),
        runtime: None,
    });
    let ident = Identity {
        pane: Some(pane.into()),
        session: Some(session.into()),
        ..ident
    };
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
    let pane = pane_override
        .or_else(|| std::env::var("TMUX_PANE").ok())
        .filter(|pane| pane.starts_with('%'))
        .ok_or_else(|| anyhow::anyhow!("collab identity requires a live tmux pane"))?;
    let requested = worker_id.or_else(|| std::env::var("COLLAB_WORKER").ok());
    if let Some(ident) = read_identity(&pane_identity_path(scope, &pane))? {
        return Ok(Identity {
            pane: Some(pane),
            ..ident
        });
    }
    if let Some(worker_id) = requested.clone() {
        if let Some(ident) = read_identity(&identity_path(scope, &worker_id))? {
            return Ok(Identity {
                pane: Some(pane),
                ..ident
            });
        }
    }
    let session = tmux_session(&pane)
        .ok_or_else(|| anyhow::anyhow!("cannot resolve tmux session for pane {}", pane))?;
    if let Some(ident) = read_identity(&session_identity_path(scope, &session))? {
        let ident = Identity {
            pane: Some(pane),
            session: Some(session),
            ..ident
        };
        persist_identity(scope, &ident)?;
        return Ok(ident);
    }
    let ident = Identity {
        worker_id: requested.unwrap_or_else(|| session.clone()),
        token: hex(16),
        pane: Some(pane),
        session: Some(session),
        runtime: None,
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
