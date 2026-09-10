//! R3 AppServer adapter seam.
//!
//! This is deliberately a thin typed boundary, not a second identity or scope
//! registry. It owns endpoint kind/capability selection, fail-closed command
//! submission checks, and P0 interrupt mapping. Runtime/binding/generation
//! truth remains in `RuntimeIdentity`, and request turn truth remains in
//! `RequestEnvelope`; this module never keeps a mutable peer map or derives
//! scope/role from business payloads.

use crate::identity::{BindingId, RuntimeIdentity, TurnId};
use crate::proto::{CommandEnvelope, Req, RequestEnvelope};
use crate::scope::RouteScope;
use serde::{Deserialize, Serialize};
use std::fmt;

pub const APPSERVER_ENV: &str = "COLLAB_APPSERVER";

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum EndpointKind {
    Tui,
    Desktop,
}

impl EndpointKind {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Tui => "tui",
            Self::Desktop => "desktop",
        }
    }

    pub fn parse(value: &str) -> Result<Self, AdapterError> {
        match value.to_ascii_lowercase().as_str() {
            "tui" | "tui-default" => Ok(Self::Tui),
            "desktop" | "desktop-appserver" => Ok(Self::Desktop),
            _ => Err(AdapterError::UnknownEndpoint {
                observed: Some(value.to_owned()),
            }),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum WakeMode {
    None,
    TmuxOptional,
    Native,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AdapterCapabilities {
    pub endpoint: EndpointKind,
    pub submit: bool,
    pub interrupt: bool,
    pub wake: WakeMode,
}

impl AdapterCapabilities {
    /// Describe the endpoint contract. This does not assert that a native
    /// transport is connected; the current registry returns an unavailable
    /// adapter until one is supplied by the runtime integration.
    pub fn for_endpoint(endpoint: EndpointKind) -> Self {
        match endpoint {
            EndpointKind::Tui => Self {
                endpoint,
                submit: true,
                interrupt: true,
                wake: WakeMode::TmuxOptional,
            },
            EndpointKind::Desktop => Self {
                endpoint,
                submit: true,
                interrupt: true,
                wake: WakeMode::Native,
            },
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AdapterSelection {
    pub kind: EndpointKind,
    pub capabilities: AdapterCapabilities,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct EndpointBinding<'a> {
    pub endpoint: EndpointKind,
    pub identity: &'a RuntimeIdentity,
}

impl<'a> EndpointBinding<'a> {
    pub fn new(endpoint: EndpointKind, identity: &'a RuntimeIdentity) -> Self {
        Self { endpoint, identity }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SubmissionReceipt {
    pub endpoint: EndpointKind,
    pub generation: u64,
    pub binding_id: BindingId,
    pub turn: Option<TurnId>,
    pub response: serde_json::Value,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum InterruptStatus {
    Accepted,
    Failed(String),
    Unknown(String),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InterruptReceipt {
    pub endpoint: EndpointKind,
    pub generation: u64,
    pub target_turn: TurnId,
    pub status: InterruptStatus,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AdapterError {
    UnknownEndpoint {
        observed: Option<String>,
    },
    EndpointUnavailable {
        endpoint: EndpointKind,
    },
    CapabilityUnavailable {
        endpoint: EndpointKind,
        operation: &'static str,
    },
    InvalidBinding {
        detail: String,
    },
    StaleBinding {
        expected: u64,
        observed: u64,
    },
    WrongTurn {
        expected: Option<String>,
        observed: Option<String>,
    },
    Timeout {
        operation: &'static str,
    },
    Unknown {
        operation: &'static str,
        detail: String,
    },
}

impl fmt::Display for AdapterError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::UnknownEndpoint { observed } => write!(
                f,
                "ADAPTER_UNKNOWN_ENDPOINT: no registered AppServer endpoint was detected{}",
                observed
                    .as_deref()
                    .map(|value| format!(" (observed {value})"))
                    .unwrap_or_default()
            ),
            Self::EndpointUnavailable { endpoint } => write!(
                f,
                "ADAPTER_ENDPOINT_UNAVAILABLE: {} AppServer endpoint has no live native transport",
                endpoint.as_str()
            ),
            Self::CapabilityUnavailable {
                endpoint,
                operation,
            } => write!(
                f,
                "ADAPTER_CAPABILITY_UNAVAILABLE: {} endpoint does not support {operation}",
                endpoint.as_str()
            ),
            Self::InvalidBinding { detail } => {
                write!(f, "ADAPTER_INVALID_BINDING: {detail}")
            }
            Self::StaleBinding { expected, observed } => write!(
                f,
                "ADAPTER_STALE_BINDING: expected endpoint generation {expected}, observed {observed}"
            ),
            Self::WrongTurn { expected, observed } => write!(
                f,
                "ADAPTER_WRONG_TURN: expected turn {}, observed {}",
                expected.as_deref().unwrap_or("none"),
                observed.as_deref().unwrap_or("none")
            ),
            Self::Timeout { operation } => {
                write!(f, "ADAPTER_TIMEOUT: {operation} timed out")
            }
            Self::Unknown { operation, detail } => {
                write!(f, "ADAPTER_UNKNOWN: {operation} unknown: {detail}")
            }
        }
    }
}

impl std::error::Error for AdapterError {}

pub trait AppServerAdapter {
    fn capabilities(&self) -> AdapterCapabilities;

    fn submit(
        &self,
        binding: &EndpointBinding<'_>,
        envelope: &RequestEnvelope,
    ) -> Result<SubmissionReceipt, AdapterError>;

    fn interrupt(
        &self,
        binding: &EndpointBinding<'_>,
        target_turn: &TurnId,
    ) -> Result<InterruptReceipt, AdapterError>;
}

#[derive(Debug, Clone)]
pub struct UnavailableAdapter {
    kind: EndpointKind,
}

/// Placeholder used until the host supplies a verified native transport.
/// Every operation fails explicitly so callers cannot silently switch to the
/// daemon or a tmux wake path.
impl AppServerAdapter for UnavailableAdapter {
    fn capabilities(&self) -> AdapterCapabilities {
        AdapterCapabilities::for_endpoint(self.kind)
    }

    fn submit(
        &self,
        _binding: &EndpointBinding<'_>,
        _envelope: &RequestEnvelope,
    ) -> Result<SubmissionReceipt, AdapterError> {
        Err(AdapterError::EndpointUnavailable {
            endpoint: self.kind,
        })
    }

    fn interrupt(
        &self,
        _binding: &EndpointBinding<'_>,
        _target_turn: &TurnId,
    ) -> Result<InterruptReceipt, AdapterError> {
        Err(AdapterError::EndpointUnavailable {
            endpoint: self.kind,
        })
    }
}

fn require_capability(
    adapter: &dyn AppServerAdapter,
    endpoint: EndpointKind,
    operation: &'static str,
) -> Result<(), AdapterError> {
    let capabilities = adapter.capabilities();
    if capabilities.endpoint != endpoint {
        return Err(AdapterError::InvalidBinding {
            detail: format!(
                "adapter endpoint mismatch: expected {}, observed {}",
                endpoint.as_str(),
                capabilities.endpoint.as_str()
            ),
        });
    }
    let supported = match operation {
        "submit" => capabilities.submit,
        "interrupt" => capabilities.interrupt,
        _ => true,
    };
    if supported {
        Ok(())
    } else {
        Err(AdapterError::CapabilityUnavailable {
            endpoint,
            operation,
        })
    }
}

#[derive(Debug, Clone, Default)]
pub struct AdapterRegistry;

impl AdapterRegistry {
    pub fn new() -> Self {
        Self
    }

    pub fn detect(explicit: Option<EndpointKind>) -> Result<AdapterSelection, AdapterError> {
        let kind = match explicit {
            Some(kind) => kind,
            None => match std::env::var(APPSERVER_ENV) {
                Ok(value) => EndpointKind::parse(&value)?,
                Err(_) => {
                    return Err(AdapterError::UnknownEndpoint { observed: None });
                }
            },
        };
        Ok(AdapterSelection {
            kind,
            capabilities: AdapterCapabilities::for_endpoint(kind),
        })
    }

    pub fn detect_optional_env() -> Result<Option<AdapterSelection>, AdapterError> {
        match std::env::var(APPSERVER_ENV) {
            Ok(value) => Self::detect(Some(EndpointKind::parse(&value)?)).map(Some),
            Err(std::env::VarError::NotPresent) => Ok(None),
            Err(_) => Err(AdapterError::UnknownEndpoint { observed: None }),
        }
    }

    pub fn adapter(&self, kind: EndpointKind) -> UnavailableAdapter {
        UnavailableAdapter { kind }
    }
}

pub fn binding_for_request<'a>(
    identity: &'a RuntimeIdentity,
    _envelope: &RequestEnvelope,
) -> Result<Option<EndpointBinding<'a>>, AdapterError> {
    let selection = AdapterRegistry::detect_optional_env()?;
    Ok(selection.map(|selection| EndpointBinding::new(selection.kind, identity)))
}

pub fn binding_for_explicit_env<'a>(
    identity: &'a RuntimeIdentity,
    env_value: Option<&str>,
) -> Result<Option<EndpointBinding<'a>>, AdapterError> {
    let Some(value) = env_value else {
        return Ok(None);
    };
    let selection = AdapterRegistry::detect(Some(EndpointKind::parse(value)?))?;
    Ok(Some(EndpointBinding::new(selection.kind, identity)))
}

pub fn validate_endpoint_binding(binding: &EndpointBinding<'_>) -> Result<(), AdapterError> {
    if let Err(error) = binding.identity.validate() {
        return Err(AdapterError::InvalidBinding {
            detail: error.to_string(),
        });
    }
    Ok(())
}

pub fn validate_turn_matches(
    expected: Option<&TurnId>,
    observed: Option<&TurnId>,
) -> Result<(), AdapterError> {
    match (expected, observed) {
        (None, None) => Ok(()),
        (Some(expected), Some(observed)) if expected == observed => Ok(()),
        (Some(expected), observed) => Err(AdapterError::WrongTurn {
            expected: Some(expected.to_string()),
            observed: observed.map(ToString::to_string),
        }),
        (None, Some(observed)) => Err(AdapterError::WrongTurn {
            expected: None,
            observed: Some(observed.to_string()),
        }),
    }
}

pub fn submit_registered(
    adapter: &dyn AppServerAdapter,
    binding: &EndpointBinding<'_>,
    envelope: &RequestEnvelope,
) -> Result<SubmissionReceipt, AdapterError> {
    validate_endpoint_binding(binding)?;
    validate_registered_envelope(binding, envelope)?;
    if let Some(command) = request_command(envelope) {
        validate_command(binding, envelope, command)?;
    }
    require_capability(adapter, binding.endpoint, "submit")?;
    let receipt = adapter.submit(binding, envelope)?;
    validate_submission_receipt(binding, envelope, &receipt)?;
    Ok(receipt)
}

fn validate_registered_envelope(
    binding: &EndpointBinding<'_>,
    envelope: &RequestEnvelope,
) -> Result<(), AdapterError> {
    envelope
        .validate()
        .map_err(|error| AdapterError::InvalidBinding {
            detail: error.to_string(),
        })?;
    let context =
        envelope
            .project_context
            .as_ref()
            .ok_or_else(|| AdapterError::InvalidBinding {
                detail: "registered adapter request is missing project context".into(),
            })?;
    if context.app_scope_id != binding.identity.appserver_id {
        return Err(AdapterError::InvalidBinding {
            detail: format!(
                "project context app scope mismatch: expected {}, observed {}",
                binding.identity.appserver_id, context.app_scope_id
            ),
        });
    }
    Ok(())
}

fn request_command(envelope: &RequestEnvelope) -> Option<&CommandEnvelope> {
    match &envelope.request {
        Req::Send {
            command: Some(command),
            ..
        } => Some(command),
        _ => None,
    }
}

fn validate_command(
    binding: &EndpointBinding<'_>,
    envelope: &RequestEnvelope,
    command: &CommandEnvelope,
) -> Result<(), AdapterError> {
    let registered_scope = route_scope(envelope)?;
    if let Err(error) = command.validate_for(binding.identity, &registered_scope) {
        if let Some(crate::identity::BindingValidationError::StaleGeneration {
            expected,
            observed,
        }) = error.downcast_ref::<crate::identity::BindingValidationError>()
        {
            return Err(AdapterError::StaleBinding {
                expected: *expected,
                observed: *observed,
            });
        }
        return Err(AdapterError::InvalidBinding {
            detail: error.to_string(),
        });
    }
    Ok(())
}

fn route_scope(envelope: &RequestEnvelope) -> Result<RouteScope, AdapterError> {
    let context =
        envelope
            .project_context
            .as_ref()
            .ok_or_else(|| AdapterError::InvalidBinding {
                detail: "registered adapter request is missing project context".into(),
            })?;
    Ok(RouteScope {
        app_scope_id: context.app_scope_id.clone(),
        project_scope_id: context.project_scope.clone(),
    })
}

fn request_turn(envelope: &RequestEnvelope) -> Option<&TurnId> {
    request_command(envelope).and_then(|command| command.turn_id.as_ref())
}

fn validate_submission_receipt(
    binding: &EndpointBinding<'_>,
    envelope: &RequestEnvelope,
    receipt: &SubmissionReceipt,
) -> Result<(), AdapterError> {
    if receipt.endpoint != binding.endpoint {
        return Err(AdapterError::InvalidBinding {
            detail: format!(
                "adapter receipt endpoint mismatch: expected {}, observed {}",
                binding.endpoint.as_str(),
                receipt.endpoint.as_str()
            ),
        });
    }
    if receipt.generation != binding.identity.endpoint_generation {
        return Err(AdapterError::StaleBinding {
            expected: binding.identity.endpoint_generation,
            observed: receipt.generation,
        });
    }
    if receipt.binding_id != binding.identity.binding_id {
        return Err(AdapterError::InvalidBinding {
            detail: format!(
                "adapter receipt binding mismatch: expected {}, observed {}",
                binding.identity.binding_id, receipt.binding_id
            ),
        });
    }
    validate_turn_matches(request_turn(envelope), receipt.turn.as_ref())
}

pub fn interrupt_registered(
    adapter: &dyn AppServerAdapter,
    binding: &EndpointBinding<'_>,
    target_turn: &TurnId,
) -> Result<InterruptReceipt, AdapterError> {
    validate_endpoint_binding(binding)?;
    require_capability(adapter, binding.endpoint, "interrupt")?;
    let receipt = adapter.interrupt(binding, target_turn)?;
    if receipt.endpoint != binding.endpoint {
        return Err(AdapterError::InvalidBinding {
            detail: format!(
                "adapter interrupt endpoint mismatch: expected {}, observed {}",
                binding.endpoint.as_str(),
                receipt.endpoint.as_str()
            ),
        });
    }
    if receipt.generation != binding.identity.endpoint_generation {
        return Err(AdapterError::StaleBinding {
            expected: binding.identity.endpoint_generation,
            observed: receipt.generation,
        });
    }
    validate_turn_matches(Some(target_turn), Some(&receipt.target_turn))?;
    Ok(receipt)
}

pub fn timeout(operation: &'static str) -> AdapterError {
    AdapterError::Timeout { operation }
}

pub fn unknown(operation: &'static str, detail: impl Into<String>) -> AdapterError {
    AdapterError::Unknown {
        operation,
        detail: detail.into(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::identity::{AgentId, AppServerId, BindingId, CommandId, OperationId, RuntimeId};
    use crate::scope::RouteScope;
    use std::os::unix::net::UnixListener;
    use std::path::PathBuf;

    fn registered_identity(appserver: &str, generation: u64, binding: &str) -> RuntimeIdentity {
        RuntimeIdentity {
            agent_id: AgentId::new("agent-1").unwrap(),
            runtime_id: RuntimeId::new("runtime-1").unwrap(),
            appserver_id: AppServerId::new(appserver).unwrap(),
            endpoint_generation: generation,
            binding_id: BindingId::new(binding).unwrap(),
            native_thread_id: None,
        }
    }

    fn turn(value: &str) -> TurnId {
        TurnId::new(value).unwrap()
    }

    fn request(
        binding: &EndpointBinding,
        turn: Option<&TurnId>,
        generation: u64,
        root: &PathBuf,
    ) -> RequestEnvelope {
        let command = CommandEnvelope::new(
            CommandId::new("command-1").unwrap(),
            OperationId::new("operation-1").unwrap(),
            binding.identity.binding_id.clone(),
            generation,
            RouteScope::for_registered_project(binding.identity.appserver_id.clone(), root)
                .unwrap(),
            None,
            turn.cloned(),
            None,
            None,
        );
        RequestEnvelope::new(
            Req::Send {
                from: "agent-1".into(),
                worker_id: Some("agent-1".into()),
                token: Some("token-1".into()),
                command: Some(command),
                to: "agent-2".into(),
                mtype: "notify".into(),
                subject: Some("subject".into()),
                body: "body".into(),
                in_reply_to: None,
                delivery: "immediate".into(),
            },
            Some(
                crate::proto::ProjectContext::for_registered_route(root, &binding.identity)
                    .unwrap(),
            ),
        )
    }

    #[derive(Debug)]
    struct FakeAdapter {
        endpoint: EndpointKind,
        submit: Result<SubmissionReceipt, AdapterError>,
        interrupt: Result<InterruptReceipt, AdapterError>,
    }

    impl AppServerAdapter for FakeAdapter {
        fn capabilities(&self) -> AdapterCapabilities {
            AdapterCapabilities::for_endpoint(self.endpoint)
        }

        fn submit(
            &self,
            binding: &EndpointBinding,
            envelope: &RequestEnvelope,
        ) -> Result<SubmissionReceipt, AdapterError> {
            match &self.submit {
                Ok(receipt) => Ok(receipt.clone()),
                Err(error) => {
                    let _ = (binding, envelope);
                    Err(error.clone())
                }
            }
        }

        fn interrupt(
            &self,
            binding: &EndpointBinding,
            target_turn: &TurnId,
        ) -> Result<InterruptReceipt, AdapterError> {
            match &self.interrupt {
                Ok(receipt) => Ok(receipt.clone()),
                Err(error) => {
                    let _ = (binding, target_turn);
                    Err(error.clone())
                }
            }
        }
    }

    #[derive(Debug)]
    struct UnsupportedAdapter;

    impl AppServerAdapter for UnsupportedAdapter {
        fn capabilities(&self) -> AdapterCapabilities {
            AdapterCapabilities {
                endpoint: EndpointKind::Desktop,
                submit: false,
                interrupt: false,
                wake: WakeMode::None,
            }
        }

        fn submit(
            &self,
            _binding: &EndpointBinding,
            _envelope: &RequestEnvelope,
        ) -> Result<SubmissionReceipt, AdapterError> {
            panic!("unsupported submit must be rejected before adapter execution")
        }

        fn interrupt(
            &self,
            _binding: &EndpointBinding,
            _target_turn: &TurnId,
        ) -> Result<InterruptReceipt, AdapterError> {
            panic!("unsupported interrupt must be rejected before adapter execution")
        }
    }

    struct EndpointMismatchAdapter {
        submit_calls: std::cell::Cell<usize>,
        interrupt_calls: std::cell::Cell<usize>,
    }

    impl EndpointMismatchAdapter {
        fn new() -> Self {
            Self {
                submit_calls: std::cell::Cell::new(0),
                interrupt_calls: std::cell::Cell::new(0),
            }
        }
    }

    impl AppServerAdapter for EndpointMismatchAdapter {
        fn capabilities(&self) -> AdapterCapabilities {
            AdapterCapabilities::for_endpoint(EndpointKind::Tui)
        }

        fn submit(
            &self,
            _binding: &EndpointBinding,
            _envelope: &RequestEnvelope,
        ) -> Result<SubmissionReceipt, AdapterError> {
            self.submit_calls.set(self.submit_calls.get() + 1);
            Err(AdapterError::Unknown {
                operation: "submit",
                detail: "endpoint-mismatched adapter was executed".into(),
            })
        }

        fn interrupt(
            &self,
            _binding: &EndpointBinding,
            _target_turn: &TurnId,
        ) -> Result<InterruptReceipt, AdapterError> {
            self.interrupt_calls.set(self.interrupt_calls.get() + 1);
            Err(AdapterError::Unknown {
                operation: "interrupt",
                detail: "endpoint-mismatched adapter was executed".into(),
            })
        }
    }

    fn receipt(
        kind: EndpointKind,
        binding: &EndpointBinding,
        turn: Option<TurnId>,
    ) -> SubmissionReceipt {
        SubmissionReceipt {
            endpoint: kind,
            generation: binding.identity.endpoint_generation,
            binding_id: binding.identity.binding_id.clone(),
            turn,
            response: serde_json::json!({"accepted": true}),
        }
    }

    fn temp_root(label: &str) -> PathBuf {
        let root = std::env::temp_dir().join(format!(
            "collab-adapter-{label}-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(&root).unwrap();
        root
    }

    #[test]
    fn tui_and_desktop_endpoints_have_distinct_capability_surfaces() {
        let tui = AdapterCapabilities::for_endpoint(EndpointKind::Tui);
        let desktop = AdapterCapabilities::for_endpoint(EndpointKind::Desktop);

        assert_eq!(tui.endpoint, EndpointKind::Tui);
        assert_eq!(tui.wake, WakeMode::TmuxOptional);
        assert_eq!(desktop.endpoint, EndpointKind::Desktop);
        assert_eq!(desktop.wake, WakeMode::Native);
        assert!(tui.submit && desktop.submit);
        assert!(tui.interrupt && desktop.interrupt);
    }

    #[test]
    fn explicit_tui_and_desktop_detection_does_not_guess_tmux_identity() {
        let tui = AdapterRegistry::detect(Some(EndpointKind::Tui)).unwrap();
        assert_eq!(tui.kind, EndpointKind::Tui);

        let desktop = AdapterRegistry::detect(Some(EndpointKind::Desktop)).unwrap();
        assert_eq!(desktop.kind, EndpointKind::Desktop);

        let error = EndpointKind::parse("unknown").unwrap_err();
        assert!(error.to_string().contains("ADAPTER_UNKNOWN_ENDPOINT"));
    }

    #[test]
    fn registered_submit_accepts_matching_runtime_binding_and_turn() {
        let root = std::env::temp_dir().join(format!(
            "collab-adapter-submit-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(&root).unwrap();
        let identity = registered_identity("appserver-desktop", 7, "binding-7");
        let binding = EndpointBinding::new(EndpointKind::Desktop, &identity);
        let envelope = request(&binding, Some(&turn("turn-1")), 7, &root);
        let expected = receipt(EndpointKind::Desktop, &binding, Some(turn("turn-1")));
        let adapter = FakeAdapter {
            endpoint: EndpointKind::Desktop,
            submit: Ok(expected.clone()),
            interrupt: Ok(InterruptReceipt {
                endpoint: EndpointKind::Desktop,
                generation: 7,
                target_turn: turn("turn-1"),
                status: InterruptStatus::Accepted,
            }),
        };

        assert_eq!(adapter.capabilities().wake, WakeMode::Native);
        assert_eq!(
            submit_registered(&adapter, &binding, &envelope).unwrap(),
            expected
        );
        std::fs::remove_dir_all(root).ok();
    }

    #[test]
    fn registered_submit_rejects_stale_generation_before_adapter_call() {
        let root = std::env::temp_dir().join(format!(
            "collab-adapter-stale-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(&root).unwrap();
        let identity = registered_identity("appserver-desktop", 9, "binding-9");
        let binding = EndpointBinding::new(EndpointKind::Desktop, &identity);
        let envelope = request(&binding, Some(&turn("turn-1")), 8, &root);
        let adapter = FakeAdapter {
            endpoint: EndpointKind::Desktop,
            submit: Ok(receipt(
                EndpointKind::Desktop,
                &binding,
                Some(turn("turn-1")),
            )),
            interrupt: Ok(InterruptReceipt {
                endpoint: EndpointKind::Desktop,
                generation: 9,
                target_turn: turn("turn-1"),
                status: InterruptStatus::Accepted,
            }),
        };

        let error = submit_registered(&adapter, &binding, &envelope).unwrap_err();
        assert_eq!(
            error,
            AdapterError::StaleBinding {
                expected: 9,
                observed: 8
            }
        );
        std::fs::remove_dir_all(root).ok();
    }

    #[test]
    fn registered_submit_rejects_wrong_binding_before_adapter_call() {
        let root = std::env::temp_dir().join(format!(
            "collab-adapter-wrong-binding-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(&root).unwrap();
        let identity = registered_identity("appserver-tui", 3, "binding-3");
        let binding = EndpointBinding::new(EndpointKind::Tui, &identity);
        let mut envelope = request(&binding, Some(&turn("turn-1")), 3, &root);
        if let Req::Send {
            command: Some(command),
            ..
        } = &mut envelope.request
        {
            command.actor_binding_id = BindingId::new("binding-other").unwrap();
        }
        let adapter = FakeAdapter {
            endpoint: EndpointKind::Tui,
            submit: Ok(receipt(EndpointKind::Tui, &binding, Some(turn("turn-1")))),
            interrupt: Ok(InterruptReceipt {
                endpoint: EndpointKind::Tui,
                generation: 3,
                target_turn: turn("turn-1"),
                status: InterruptStatus::Accepted,
            }),
        };

        let error = submit_registered(&adapter, &binding, &envelope).unwrap_err();
        assert!(matches!(error, AdapterError::InvalidBinding { .. }));
        assert!(error.to_string().contains("ADAPTER_INVALID_BINDING"));
        std::fs::remove_dir_all(root).ok();
    }

    #[test]
    fn registered_submit_rejects_context_for_a_different_appserver() {
        let root = temp_root("wrong-context");
        let identity = registered_identity("appserver-desktop", 4, "binding-4");
        let binding = EndpointBinding::new(EndpointKind::Desktop, &identity);
        let mut envelope = request(&binding, Some(&turn("turn-1")), 4, &root);
        envelope.project_context.as_mut().unwrap().app_scope_id =
            AppServerId::new("appserver-other").unwrap();
        let adapter = FakeAdapter {
            endpoint: EndpointKind::Desktop,
            submit: Ok(receipt(
                EndpointKind::Desktop,
                &binding,
                Some(turn("turn-1")),
            )),
            interrupt: Err(AdapterError::EndpointUnavailable {
                endpoint: EndpointKind::Desktop,
            }),
        };

        let error = submit_registered(&adapter, &binding, &envelope).unwrap_err();
        assert!(matches!(error, AdapterError::InvalidBinding { .. }));
        assert!(error.to_string().contains("app scope mismatch"));
        std::fs::remove_dir_all(root).ok();
    }

    #[test]
    fn registered_submit_rejects_receipt_with_wrong_turn_after_adapter_call() {
        let root = std::env::temp_dir().join(format!(
            "collab-adapter-receipt-wrong-turn-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(&root).unwrap();
        let identity = registered_identity("appserver-tui", 3, "binding-3");
        let binding = EndpointBinding::new(EndpointKind::Tui, &identity);
        let envelope = request(&binding, Some(&turn("turn-1")), 3, &root);
        let mut wrong_receipt = receipt(EndpointKind::Tui, &binding, Some(turn("turn-1")));
        wrong_receipt.turn = Some(turn("turn-2"));
        let adapter = FakeAdapter {
            endpoint: EndpointKind::Tui,
            submit: Ok(wrong_receipt),
            interrupt: Ok(InterruptReceipt {
                endpoint: EndpointKind::Tui,
                generation: 3,
                target_turn: turn("turn-1"),
                status: InterruptStatus::Accepted,
            }),
        };

        let error = submit_registered(&adapter, &binding, &envelope).unwrap_err();
        assert_eq!(
            error,
            AdapterError::WrongTurn {
                expected: Some("turn-1".into()),
                observed: Some("turn-2".into())
            }
        );
        std::fs::remove_dir_all(root).ok();
    }

    #[test]
    fn registered_submit_rejects_stale_receipt_generation() {
        let root = std::env::temp_dir().join(format!(
            "collab-adapter-receipt-stale-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(&root).unwrap();
        let identity = registered_identity("appserver-desktop", 7, "binding-7");
        let binding = EndpointBinding::new(EndpointKind::Desktop, &identity);
        let envelope = request(&binding, Some(&turn("turn-1")), 7, &root);
        let mut stale_receipt = receipt(EndpointKind::Desktop, &binding, Some(turn("turn-1")));
        stale_receipt.generation = 6;
        let adapter = FakeAdapter {
            endpoint: EndpointKind::Desktop,
            submit: Ok(stale_receipt),
            interrupt: Err(AdapterError::EndpointUnavailable {
                endpoint: EndpointKind::Desktop,
            }),
        };

        let error = submit_registered(&adapter, &binding, &envelope).unwrap_err();
        assert_eq!(
            error,
            AdapterError::StaleBinding {
                expected: 7,
                observed: 6
            }
        );
        std::fs::remove_dir_all(root).ok();
    }

    #[test]
    fn unavailable_adapter_never_falls_back_to_another_endpoint() {
        let root = std::env::temp_dir().join(format!(
            "collab-adapter-unavailable-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(&root).unwrap();
        let identity = registered_identity("appserver-tui", 3, "binding-3");
        let binding = EndpointBinding::new(EndpointKind::Tui, &identity);
        let envelope = request(&binding, Some(&turn("turn-1")), 3, &root);
        let adapter = AdapterRegistry::new().adapter(EndpointKind::Tui);

        let submit_error = submit_registered(&adapter, &binding, &envelope).unwrap_err();
        assert_eq!(
            submit_error,
            AdapterError::EndpointUnavailable {
                endpoint: EndpointKind::Tui
            }
        );
        assert!(!submit_error.to_string().contains("tmux"));

        let interrupt_error =
            interrupt_registered(&adapter, &binding, &turn("turn-1")).unwrap_err();
        assert_eq!(
            interrupt_error,
            AdapterError::EndpointUnavailable {
                endpoint: EndpointKind::Tui
            }
        );
        std::fs::remove_dir_all(root).ok();
    }

    #[test]
    fn unsupported_capabilities_fail_before_adapter_execution() {
        let root = temp_root("capability");
        let identity = registered_identity("appserver-desktop", 2, "binding-2");
        let binding = EndpointBinding::new(EndpointKind::Desktop, &identity);
        let envelope = request(&binding, Some(&turn("turn-1")), 2, &root);
        let adapter = UnsupportedAdapter;

        let submit_error = submit_registered(&adapter, &binding, &envelope).unwrap_err();
        assert_eq!(
            submit_error,
            AdapterError::CapabilityUnavailable {
                endpoint: EndpointKind::Desktop,
                operation: "submit",
            }
        );

        let interrupt_error =
            interrupt_registered(&adapter, &binding, &turn("turn-1")).unwrap_err();
        assert_eq!(
            interrupt_error,
            AdapterError::CapabilityUnavailable {
                endpoint: EndpointKind::Desktop,
                operation: "interrupt",
            }
        );
        std::fs::remove_dir_all(root).ok();
    }

    #[test]
    fn endpoint_mismatch_fails_before_adapter_execution() {
        let root = temp_root("endpoint-mismatch");
        let identity = registered_identity("appserver-desktop", 2, "binding-2");
        let binding = EndpointBinding::new(EndpointKind::Desktop, &identity);
        let envelope = request(&binding, Some(&turn("turn-1")), 2, &root);
        let adapter = EndpointMismatchAdapter::new();

        let submit_error = submit_registered(&adapter, &binding, &envelope).unwrap_err();
        assert!(matches!(
            submit_error,
            AdapterError::InvalidBinding { ref detail }
                if detail == "adapter endpoint mismatch: expected desktop, observed tui"
        ));
        assert_eq!(adapter.submit_calls.get(), 0);

        let interrupt_error =
            interrupt_registered(&adapter, &binding, &turn("turn-1")).unwrap_err();
        assert!(matches!(
            interrupt_error,
            AdapterError::InvalidBinding { ref detail }
                if detail == "adapter endpoint mismatch: expected desktop, observed tui"
        ));
        assert_eq!(adapter.interrupt_calls.get(), 0);

        std::fs::remove_dir_all(root).ok();
    }

    #[test]
    fn selected_endpoint_failure_does_not_retry_daemon_or_tmux() {
        let root = temp_root("no-fallback");
        let socket = PathBuf::from(format!(
            "/tmp/collab-adapter-no-fallback-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        let listener = UnixListener::bind(&socket).unwrap();
        listener.set_nonblocking(true).unwrap();
        let identity = registered_identity("appserver-desktop", 6, "binding-6");

        let error = crate::client::call_with_runtime_identity_at_root_for_endpoint::<
            serde_json::Value,
        >(&socket, &Req::Ping, &root, &identity, EndpointKind::Desktop)
        .unwrap_err();
        assert!(error.to_string().contains("ADAPTER_ENDPOINT_UNAVAILABLE"));
        assert!(matches!(
            listener.accept(),
            Err(error) if error.kind() == std::io::ErrorKind::WouldBlock
        ));
        drop(listener);
        std::fs::remove_file(&socket).ok();
        std::fs::remove_dir_all(root).ok();
    }

    #[test]
    fn registered_interrupt_accepts_matching_turn() {
        let root = std::env::temp_dir().join(format!(
            "collab-adapter-interrupt-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(&root).unwrap();
        let identity = registered_identity("appserver-desktop", 5, "binding-5");
        let binding = EndpointBinding::new(EndpointKind::Desktop, &identity);
        let expected = InterruptReceipt {
            endpoint: EndpointKind::Desktop,
            generation: 5,
            target_turn: turn("turn-1"),
            status: InterruptStatus::Accepted,
        };
        let adapter = FakeAdapter {
            endpoint: EndpointKind::Desktop,
            submit: Err(AdapterError::EndpointUnavailable {
                endpoint: EndpointKind::Desktop,
            }),
            interrupt: Ok(expected.clone()),
        };

        assert_eq!(
            interrupt_registered(&adapter, &binding, &turn("turn-1")).unwrap(),
            expected
        );
        std::fs::remove_dir_all(root).ok();
    }

    #[test]
    fn registered_interrupt_rejects_wrong_native_turn() {
        let identity = registered_identity("appserver-desktop", 5, "binding-5");
        let binding = EndpointBinding::new(EndpointKind::Desktop, &identity);
        let adapter = FakeAdapter {
            endpoint: EndpointKind::Desktop,
            submit: Err(AdapterError::EndpointUnavailable {
                endpoint: EndpointKind::Desktop,
            }),
            interrupt: Ok(InterruptReceipt {
                endpoint: EndpointKind::Desktop,
                generation: 5,
                target_turn: turn("turn-2"),
                status: InterruptStatus::Accepted,
            }),
        };

        let error = interrupt_registered(&adapter, &binding, &turn("turn-1")).unwrap_err();
        assert_eq!(
            error,
            AdapterError::WrongTurn {
                expected: Some("turn-1".into()),
                observed: Some("turn-2".into())
            }
        );
    }

    #[test]
    fn timeout_and_unknown_are_explicit_typed_adapter_errors() {
        let timeout_error = timeout("submit");
        assert_eq!(
            timeout_error,
            AdapterError::Timeout {
                operation: "submit"
            }
        );
        assert!(timeout_error.to_string().starts_with("ADAPTER_TIMEOUT:"));

        let unknown_error = unknown("interrupt", "cursor lost");
        assert_eq!(
            unknown_error,
            AdapterError::Unknown {
                operation: "interrupt",
                detail: "cursor lost".into()
            }
        );
        assert!(unknown_error.to_string().starts_with("ADAPTER_UNKNOWN:"));
        assert_eq!(
            InterruptStatus::Failed("native stop failed".into()),
            InterruptStatus::Failed("native stop failed".into())
        );
        assert_eq!(
            InterruptStatus::Unknown("native stop not observed".into()),
            InterruptStatus::Unknown("native stop not observed".into())
        );
    }
}
