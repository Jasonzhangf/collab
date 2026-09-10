//! Host-wide typed state for the v1 global daemon.
//!
//! This module owns the host-wide typed identity, scope and command projection
//! used by the resident daemon.  It stays independent of the legacy
//! project-local [`super::state::State`] data model; the daemon reducer imports
//! these types without creating a second journal or notification store.

use crate::identity::{
    AgentId, AppServerId, BindingId, CommandId, NativeThreadId, OperationId, RuntimeId,
};
use crate::scope::{ProjectScopeId, RouteScope};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::{BTreeMap, BTreeSet};
use std::fmt;
use std::path::Path;

/// A project scope is the canonical, registered project root.  The alias
/// keeps the storage key and the route identity visibly distinct from an
/// execution worktree path.
pub type CanonicalProjectScope = ProjectScopeId;
pub type AppScopeId = AppServerId;

pub const INITIAL_EPOCH: u64 = 1;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct StateVersion {
    pub epoch: u64,
    pub sequence: u64,
    pub revision: u64,
}

impl StateVersion {
    fn from_state(state: &GlobalState) -> Self {
        Self {
            epoch: state.epoch,
            sequence: state.sequence,
            revision: state.revision,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum StateError {
    Invalid {
        field: &'static str,
        reason: String,
    },
    ProjectNotRegistered(String),
    RegistrationConflict {
        project_scope: String,
        app_scope_id: String,
    },
    BindingNotFound(String),
    BindingConflict(String),
    StaleBinding {
        binding_id: String,
        expected_generation: u64,
        observed_generation: u64,
    },
    MasterGrantRequiresApproval,
    MasterGrantConflict(String),
    MasterGrantBindingMismatch(String),
    CommandIdReuse {
        command_id: String,
        existing_operation: String,
        observed_operation: String,
    },
    OperationIdReuse {
        operation_id: String,
        existing_command: String,
    },
    ReceiptConflict(String),
    CompareAndSwapMismatch {
        expected: u64,
        observed: u64,
    },
    CounterOverflow(&'static str),
    Invariant(String),
}

impl StateError {
    fn invalid(field: &'static str, reason: impl Into<String>) -> Self {
        Self::Invalid {
            field,
            reason: reason.into(),
        }
    }
}

impl fmt::Display for StateError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Invalid { field, reason } => write!(f, "invalid {field}: {reason}"),
            Self::ProjectNotRegistered(scope) => {
                write!(f, "project scope is not registered: {scope}")
            }
            Self::RegistrationConflict {
                project_scope,
                app_scope_id,
            } => write!(
                f,
                "project registration already exists for ({project_scope}, {app_scope_id})"
            ),
            Self::BindingNotFound(binding_id) => write!(f, "runtime binding not found: {binding_id}"),
            Self::BindingConflict(reason) => write!(f, "runtime binding conflict: {reason}"),
            Self::StaleBinding {
                binding_id,
                expected_generation,
                observed_generation,
            } => write!(
                f,
                "stale runtime binding {binding_id}: expected generation {expected_generation}, observed {observed_generation}"
            ),
            Self::MasterGrantRequiresApproval => {
                f.write_str("master grant requires explicit user approval")
            }
            Self::MasterGrantConflict(reason) => write!(f, "master grant conflict: {reason}"),
            Self::MasterGrantBindingMismatch(reason) => {
                write!(f, "master grant binding mismatch: {reason}")
            }
            Self::CommandIdReuse {
                command_id,
                existing_operation,
                observed_operation,
            } => write!(
                f,
                "command {command_id} belongs to operation {existing_operation}, not {observed_operation}"
            ),
            Self::OperationIdReuse {
                operation_id,
                existing_command,
            } => write!(
                f,
                "operation {operation_id} already belongs to command {existing_command}"
            ),
            Self::ReceiptConflict(reason) => write!(f, "command receipt conflict: {reason}"),
            Self::CompareAndSwapMismatch { expected, observed } => write!(
                f,
                "compare-and-swap revision mismatch: expected {expected}, observed {observed}"
            ),
            Self::CounterOverflow(counter) => write!(f, "{counter} counter overflow"),
            Self::Invariant(reason) => write!(f, "global state invariant failed: {reason}"),
        }
    }
}

impl std::error::Error for StateError {}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum PeerRole {
    Peer,
    Master,
}

impl Default for PeerRole {
    fn default() -> Self {
        Self::Peer
    }
}

/// A project registration is keyed by its canonical project scope and then
/// by AppServer scope.  A second AppServer for the same project therefore
/// gets a separate registration without replacing the first one.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ProjectRegistration {
    pub project_scope: ProjectScopeId,
    pub app_scope_id: AppServerId,
    #[serde(default)]
    pub registered_at_ms: i64,
}

impl ProjectRegistration {
    pub fn new(
        project_scope: ProjectScopeId,
        app_scope_id: AppServerId,
    ) -> Result<Self, StateError> {
        Self::with_registered_at(project_scope, app_scope_id, 0)
    }

    pub fn with_registered_at(
        project_scope: ProjectScopeId,
        app_scope_id: AppServerId,
        registered_at_ms: i64,
    ) -> Result<Self, StateError> {
        let registration = Self {
            project_scope,
            app_scope_id,
            registered_at_ms,
        };
        registration.validate()?;
        Ok(registration)
    }

    pub fn route_scope(&self) -> RouteScope {
        RouteScope {
            app_scope_id: self.app_scope_id.clone(),
            project_scope_id: self.project_scope.clone(),
        }
    }

    pub fn validate(&self) -> Result<(), StateError> {
        validate_project_scope(&self.project_scope)?;
        validate_app_scope(&self.app_scope_id)
    }
}

/// One current runtime endpoint for one registered project/AppServer scope.
/// `endpoint_generation` is the reconnect fence: commands must use the
/// current generation exactly.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct RuntimeBinding {
    pub project_scope: ProjectScopeId,
    pub app_scope_id: AppServerId,
    pub agent_id: AgentId,
    pub runtime_id: RuntimeId,
    pub binding_id: BindingId,
    pub endpoint_generation: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub native_thread_id: Option<NativeThreadId>,
}

impl RuntimeBinding {
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        project_scope: ProjectScopeId,
        app_scope_id: AppServerId,
        agent_id: AgentId,
        runtime_id: RuntimeId,
        binding_id: BindingId,
        endpoint_generation: u64,
        native_thread_id: Option<NativeThreadId>,
    ) -> Result<Self, StateError> {
        let binding = Self {
            project_scope,
            app_scope_id,
            agent_id,
            runtime_id,
            binding_id,
            endpoint_generation,
            native_thread_id,
        };
        binding.validate()?;
        Ok(binding)
    }

    pub fn route_scope(&self) -> RouteScope {
        RouteScope {
            app_scope_id: self.app_scope_id.clone(),
            project_scope_id: self.project_scope.clone(),
        }
    }

    pub fn validate(&self) -> Result<(), StateError> {
        validate_project_scope(&self.project_scope)?;
        validate_app_scope(&self.app_scope_id)?;
        validate_agent_id(&self.agent_id)?;
        validate_runtime_id(&self.runtime_id)?;
        validate_binding_id(&self.binding_id)?;
        if let Some(thread_id) = &self.native_thread_id {
            validate_native_thread_id(thread_id)?;
        }
        Ok(())
    }

    fn same_principal(&self, other: &Self) -> bool {
        self.project_scope == other.project_scope
            && self.app_scope_id == other.app_scope_id
            && self.agent_id == other.agent_id
    }
}

/// A master capability is an explicit grant bound to one live runtime
/// generation.  A registration has no role field: absence of this record is
/// the durable default `Peer` role.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct MasterGrant {
    pub project_scope: ProjectScopeId,
    pub app_scope_id: AppServerId,
    pub agent_id: AgentId,
    pub boundary: String,
    pub granted_by: String,
    pub approval: String,
    pub binding_id: BindingId,
    pub endpoint_generation: u64,
    pub granted_at_ms: i64,
}

impl MasterGrant {
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        project_scope: ProjectScopeId,
        app_scope_id: AppServerId,
        agent_id: AgentId,
        boundary: impl Into<String>,
        granted_by: impl Into<String>,
        approval: impl Into<String>,
        binding_id: BindingId,
        endpoint_generation: u64,
        granted_at_ms: i64,
    ) -> Result<Self, StateError> {
        let grant = Self {
            project_scope,
            app_scope_id,
            agent_id,
            boundary: boundary.into(),
            granted_by: granted_by.into(),
            approval: approval.into(),
            binding_id,
            endpoint_generation,
            granted_at_ms,
        };
        grant.validate()?;
        Ok(grant)
    }

    pub fn validate(&self) -> Result<(), StateError> {
        validate_project_scope(&self.project_scope)?;
        validate_app_scope(&self.app_scope_id)?;
        validate_agent_id(&self.agent_id)?;
        validate_binding_id(&self.binding_id)?;
        validate_non_empty_text("master grant boundary", &self.boundary)?;
        validate_non_empty_text("master grant actor", &self.granted_by)?;
        if self.approval.trim().is_empty() {
            return Err(StateError::MasterGrantRequiresApproval);
        }
        if self.approval.chars().any(char::is_control) {
            return Err(StateError::invalid(
                "master grant approval",
                "must not contain control characters",
            ));
        }
        Ok(())
    }
}

/// Project-local state owned by this model.  It contains only registrations,
/// runtime bindings and capability grants.  Tasks, messages and legacy
/// notification records are intentionally absent so this module cannot become
/// a second copy of `server::state::State`.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ProjectState {
    pub project_scope: ProjectScopeId,
    #[serde(default)]
    pub registrations: BTreeMap<String, ProjectRegistration>,
    #[serde(default)]
    pub runtime_bindings: BTreeMap<String, RuntimeBinding>,
    #[serde(default)]
    pub master_grants: BTreeMap<String, MasterGrant>,
}

impl ProjectState {
    pub fn new(project_scope: ProjectScopeId) -> Result<Self, StateError> {
        validate_project_scope(&project_scope)?;
        Ok(Self {
            project_scope,
            registrations: BTreeMap::new(),
            runtime_bindings: BTreeMap::new(),
            master_grants: BTreeMap::new(),
        })
    }

    pub fn validate(&self) -> Result<(), StateError> {
        validate_project_scope(&self.project_scope)?;

        for (app_scope_key, registration) in &self.registrations {
            registration.validate()?;
            if registration.project_scope != self.project_scope {
                return Err(StateError::Invariant(format!(
                    "registration {} belongs to {}, expected {}",
                    app_scope_key,
                    registration.project_scope.as_str(),
                    self.project_scope.as_str()
                )));
            }
            if app_scope_key != registration.app_scope_id.as_str() {
                return Err(StateError::Invariant(format!(
                    "registration key {app_scope_key} does not match app scope {}",
                    registration.app_scope_id
                )));
            }
        }

        let mut runtime_ids = BTreeSet::new();
        for (binding_key, binding) in &self.runtime_bindings {
            binding.validate()?;
            if binding.project_scope != self.project_scope {
                return Err(StateError::Invariant(format!(
                    "binding {binding_key} belongs to another project scope"
                )));
            }
            if binding_key != binding.binding_id.as_str() {
                return Err(StateError::Invariant(format!(
                    "binding key {binding_key} does not match binding {}",
                    binding.binding_id
                )));
            }
            if !self
                .registrations
                .contains_key(binding.app_scope_id.as_str())
            {
                return Err(StateError::Invariant(format!(
                    "binding {binding_key} has no app scope registration"
                )));
            }
            if !runtime_ids.insert(binding.runtime_id.as_str().to_owned()) {
                return Err(StateError::Invariant(format!(
                    "runtime {} is bound more than once",
                    binding.runtime_id
                )));
            }
        }

        for (binding_key, grant) in &self.master_grants {
            grant.validate()?;
            if grant.project_scope != self.project_scope {
                return Err(StateError::Invariant(format!(
                    "master grant {binding_key} belongs to another project scope"
                )));
            }
            if binding_key != grant.binding_id.as_str() {
                return Err(StateError::Invariant(format!(
                    "master grant key {binding_key} does not match binding {}",
                    grant.binding_id
                )));
            }
            let Some(binding) = self.runtime_bindings.get(binding_key) else {
                return Err(StateError::Invariant(format!(
                    "master grant {binding_key} has no runtime binding"
                )));
            };
            if grant.app_scope_id != binding.app_scope_id
                || grant.agent_id != binding.agent_id
                || grant.endpoint_generation != binding.endpoint_generation
            {
                return Err(StateError::Invariant(format!(
                    "master grant {binding_key} is not bound to the current runtime generation"
                )));
            }
        }
        Ok(())
    }

    pub fn lookup_registration(&self, app_scope_id: &AppServerId) -> Option<&ProjectRegistration> {
        self.registrations.get(app_scope_id.as_str())
    }

    pub fn lookup_binding(&self, binding_id: &BindingId) -> Option<&RuntimeBinding> {
        self.runtime_bindings.get(binding_id.as_str())
    }

    pub fn lookup_master_grant(&self, binding_id: &BindingId) -> Option<&MasterGrant> {
        self.master_grants.get(binding_id.as_str())
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct CommandReceipt {
    pub command_id: CommandId,
    pub operation_id: OperationId,
    pub epoch: u64,
    pub sequence: u64,
    pub revision: u64,
    #[serde(default)]
    pub outcome: Value,
}

impl CommandReceipt {
    pub fn validate(&self) -> Result<(), StateError> {
        validate_command_id(&self.command_id)?;
        validate_operation_id(&self.operation_id)?;
        if self.epoch == 0 {
            return Err(StateError::invalid(
                "command receipt epoch",
                "must be non-zero",
            ));
        }
        if self.sequence == 0 {
            return Err(StateError::invalid(
                "command receipt sequence",
                "must be non-zero",
            ));
        }
        if self.revision == 0 {
            return Err(StateError::invalid(
                "command receipt revision",
                "must be non-zero",
            ));
        }
        Ok(())
    }
}

/// One host-wide reducer state.  Project state is nested under a canonical
/// project-scope key; AppServer registrations are nested under each project.
/// Command IDs are host-wide so retries cannot be rebound across projects.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct GlobalState {
    pub epoch: u64,
    pub sequence: u64,
    pub revision: u64,
    #[serde(default)]
    pub projects: BTreeMap<String, ProjectState>,
    #[serde(default)]
    pub command_receipts: BTreeMap<String, CommandReceipt>,
}

impl Default for GlobalState {
    fn default() -> Self {
        Self::new(INITIAL_EPOCH).expect("the fixed initial epoch is valid")
    }
}

impl GlobalState {
    pub fn new(epoch: u64) -> Result<Self, StateError> {
        if epoch == 0 {
            return Err(StateError::invalid("epoch", "must be non-zero"));
        }
        Ok(Self {
            epoch,
            sequence: 0,
            revision: 0,
            projects: BTreeMap::new(),
            command_receipts: BTreeMap::new(),
        })
    }

    pub fn version(&self) -> StateVersion {
        StateVersion::from_state(self)
    }

    /// The daemon journal owns the host version when this projection is
    /// nested in `server::state::State`.  Keep these counters synchronized
    /// after the resident reducer commits an event; standalone callers still
    /// advance them through the typed mutation methods below.
    pub(crate) fn set_counters(&mut self, sequence: u64, revision: u64) {
        self.sequence = sequence;
        self.revision = revision;
    }

    pub fn validate(&self) -> Result<(), StateError> {
        if self.epoch == 0 {
            return Err(StateError::invalid("epoch", "must be non-zero"));
        }

        for (scope_key, project) in &self.projects {
            project.validate()?;
            if scope_key != project.project_scope.as_str() {
                return Err(StateError::Invariant(format!(
                    "project key {scope_key} does not match scope {}",
                    project.project_scope.as_str()
                )));
            }
        }

        let mut operations = BTreeSet::new();
        for (command_key, receipt) in &self.command_receipts {
            receipt.validate()?;
            if command_key != receipt.command_id.as_str() {
                return Err(StateError::Invariant(format!(
                    "command receipt key {command_key} does not match command {}",
                    receipt.command_id
                )));
            }
            if receipt.epoch != self.epoch {
                return Err(StateError::Invariant(format!(
                    "command {command_key} belongs to epoch {}, expected {}",
                    receipt.epoch, self.epoch
                )));
            }
            if !operations.insert(receipt.operation_id.as_str().to_owned()) {
                return Err(StateError::Invariant(format!(
                    "operation {} is recorded more than once",
                    receipt.operation_id
                )));
            }
        }
        Ok(())
    }

    /// Build the only canonical project key accepted by this model.  The
    /// caller supplies the registered project root; execution worktrees are
    /// intentionally not accepted as a substitute registration root.
    pub fn canonical_project_scope(path: &Path) -> Result<ProjectScopeId, StateError> {
        let canonical = std::fs::canonicalize(path).map_err(|error| {
            StateError::invalid(
                "project scope",
                format!("cannot canonicalize {}: {error}", path.display()),
            )
        })?;
        let text = canonical.to_str().ok_or_else(|| {
            StateError::invalid("project scope", "canonical path must be valid UTF-8")
        })?;
        ProjectScopeId::new(text.to_owned())
            .map_err(|error| StateError::invalid("project scope", error.to_string()))
    }

    pub fn lookup_project(&self, project_scope: &ProjectScopeId) -> Option<&ProjectState> {
        self.projects.get(project_scope.as_str())
    }

    /// Register one `(app_scope_id, project_scope_id)` route.  The route is
    /// the typed key used by future server routing; registration creation
    /// remains the only operation that creates a project entry.
    pub fn register_project_for_route(
        &mut self,
        route_scope: &RouteScope,
        registered_at_ms: i64,
    ) -> Result<StateVersion, StateError> {
        validate_route_scope(route_scope)?;
        let registration = ProjectRegistration::with_registered_at(
            route_scope.project_scope_id.clone(),
            route_scope.app_scope_id.clone(),
            registered_at_ms,
        )?;
        self.register_project(registration)
    }

    /// Look up a project only when the requested AppServer is registered for
    /// that project.  This prevents a known project from being treated as a
    /// valid route for an unknown AppServer scope.
    pub fn lookup_project_for_route(&self, route_scope: &RouteScope) -> Option<&ProjectState> {
        validate_route_scope(route_scope).ok()?;
        self.lookup_registration(&route_scope.project_scope_id, &route_scope.app_scope_id)
            .and_then(|_| self.lookup_project(&route_scope.project_scope_id))
    }

    pub fn lookup_registration(
        &self,
        project_scope: &ProjectScopeId,
        app_scope_id: &AppServerId,
    ) -> Option<&ProjectRegistration> {
        self.lookup_project(project_scope)
            .and_then(|project| project.lookup_registration(app_scope_id))
    }

    /// Look up an unscoped binding only when its ID is host-wide unique.
    /// Route-aware callers must use [`Self::lookup_binding_for`], because the
    /// same binding ID is allowed in two independent project scopes.
    pub fn lookup_binding(&self, binding_id: &BindingId) -> Option<&RuntimeBinding> {
        let mut found = None;
        for project in self.projects.values() {
            if let Some(binding) = project.lookup_binding(binding_id) {
                if found.is_some() {
                    // An ambiguous unscoped lookup must fail closed rather
                    // than selecting whichever project sorts first.
                    return None;
                }
                found = Some(binding);
            }
        }
        found
    }

    pub fn lookup_binding_for(
        &self,
        route_scope: &RouteScope,
        binding_id: &BindingId,
    ) -> Option<&RuntimeBinding> {
        self.lookup_project_for_route(route_scope)
            .and_then(|project| project.lookup_binding(binding_id))
            .filter(|binding| binding.app_scope_id == route_scope.app_scope_id)
    }

    pub fn lookup_master_grant_for(
        &self,
        route_scope: &RouteScope,
        binding_id: &BindingId,
    ) -> Option<&MasterGrant> {
        self.lookup_project_for_route(route_scope)
            .and_then(|project| project.lookup_master_grant(binding_id))
            .filter(|grant| {
                grant.project_scope == route_scope.project_scope_id
                    && grant.app_scope_id == route_scope.app_scope_id
            })
    }

    pub fn lookup_master_grant(
        &self,
        project_scope: &ProjectScopeId,
        binding_id: &BindingId,
    ) -> Option<&MasterGrant> {
        self.lookup_project(project_scope)
            .and_then(|project| project.lookup_master_grant(binding_id))
    }

    pub fn lookup_command_receipt(&self, command_id: &CommandId) -> Option<&CommandReceipt> {
        self.command_receipts.get(command_id.as_str())
    }

    /// Install a receipt that was already committed by the host journal.
    ///
    /// The legacy reducer owns the journal sequence/revision while this
    /// host-wide map owns command idempotency.  Consequently the receipt's
    /// coordinates are validated as durable values but do not have to be
    /// bounded by this reducer's independent version counters.
    pub fn record_command_projection(&mut self, receipt: CommandReceipt) -> Result<(), StateError> {
        receipt.validate()?;
        if receipt.epoch != self.epoch {
            return Err(StateError::Invariant(format!(
                "command {} belongs to epoch {}, expected {}",
                receipt.command_id, receipt.epoch, self.epoch
            )));
        }
        if let Some(existing) = self.lookup_command_receipt(&receipt.command_id) {
            if existing == &receipt {
                return Ok(());
            }
            if existing.operation_id != receipt.operation_id {
                return Err(StateError::CommandIdReuse {
                    command_id: receipt.command_id.as_str().to_owned(),
                    existing_operation: existing.operation_id.as_str().to_owned(),
                    observed_operation: receipt.operation_id.as_str().to_owned(),
                });
            }
            return Err(StateError::ReceiptConflict(format!(
                "command {} was already projected with a different receipt",
                receipt.command_id
            )));
        }
        if let Some(existing) = self
            .command_receipts
            .values()
            .find(|current| current.operation_id == receipt.operation_id)
        {
            return Err(StateError::OperationIdReuse {
                operation_id: receipt.operation_id.as_str().to_owned(),
                existing_command: existing.command_id.as_str().to_owned(),
            });
        }
        self.command_receipts
            .insert(receipt.command_id.as_str().to_owned(), receipt);
        Ok(())
    }

    /// A new registration advances the host version exactly once.  Repeating
    /// the same registration is idempotent; a conflicting registration is
    /// rejected so one AppServer cannot silently replace another identity.
    pub fn register_project(
        &mut self,
        registration: ProjectRegistration,
    ) -> Result<StateVersion, StateError> {
        registration.validate()?;
        let scope_key = registration.project_scope.as_str().to_owned();
        let app_key = registration.app_scope_id.as_str().to_owned();
        if self
            .lookup_registration(&registration.project_scope, &registration.app_scope_id)
            .is_some_and(|current| current == &registration)
        {
            return Ok(self.version());
        }
        if self
            .lookup_registration(&registration.project_scope, &registration.app_scope_id)
            .is_some()
        {
            return Err(StateError::RegistrationConflict {
                project_scope: scope_key,
                app_scope_id: app_key,
            });
        }

        self.mutate(|next| {
            let project = next
                .projects
                .entry(scope_key.clone())
                .or_insert_with(|| ProjectState {
                    project_scope: registration.project_scope.clone(),
                    registrations: BTreeMap::new(),
                    runtime_bindings: BTreeMap::new(),
                    master_grants: BTreeMap::new(),
                });
            project.registrations.insert(app_key.clone(), registration);
            Ok(())
        })
    }

    /// Register or reconnect a runtime binding.  An older generation is
    /// rejected before state mutation.  A newer generation may replace a
    /// binding with the same principal; reconnecting revokes any grant tied
    /// to the old generation and requires an explicit new grant.
    pub fn bind_runtime(&mut self, binding: RuntimeBinding) -> Result<StateVersion, StateError> {
        binding.validate()?;
        let scope_key = binding.project_scope.as_str().to_owned();
        let binding_key = binding.binding_id.as_str().to_owned();
        let app_key = binding.app_scope_id.as_str().to_owned();

        let project = self
            .lookup_project(&binding.project_scope)
            .ok_or_else(|| StateError::ProjectNotRegistered(scope_key.clone()))?;
        if project.lookup_registration(&binding.app_scope_id).is_none() {
            return Err(StateError::ProjectNotRegistered(format!(
                "{} (app scope {})",
                binding.project_scope.as_str(),
                binding.app_scope_id
            )));
        }

        if let Some(current) = project.lookup_binding(&binding.binding_id) {
            if current == &binding {
                return Ok(self.version());
            }
            if !current.same_principal(&binding) {
                return Err(StateError::BindingConflict(format!(
                    "binding {} changes project, app scope, or agent",
                    binding.binding_id
                )));
            }
            if binding.endpoint_generation < current.endpoint_generation {
                return Err(StateError::StaleBinding {
                    binding_id: binding_key,
                    expected_generation: current.endpoint_generation,
                    observed_generation: binding.endpoint_generation,
                });
            }
            if binding.endpoint_generation == current.endpoint_generation {
                return Err(StateError::BindingConflict(format!(
                    "binding {} has a different runtime at generation {}",
                    binding.binding_id, binding.endpoint_generation
                )));
            }
        }

        let duplicate_runtime = project.runtime_bindings.values().find(|current| {
            current.runtime_id == binding.runtime_id && current.binding_id != binding.binding_id
        });
        if let Some(current) = duplicate_runtime {
            return Err(StateError::BindingConflict(format!(
                "runtime {} is already bound as {}",
                current.runtime_id, current.binding_id
            )));
        }

        self.mutate(|next| {
            let project = next
                .projects
                .get_mut(&scope_key)
                .ok_or_else(|| StateError::ProjectNotRegistered(scope_key.clone()))?;
            // The registration check above is repeated on the candidate so
            // this method remains safe if its mutation body is reused.
            if !project.registrations.contains_key(&app_key) {
                return Err(StateError::ProjectNotRegistered(format!(
                    "{} (app scope {})",
                    binding.project_scope.as_str(),
                    binding.app_scope_id
                )));
            }
            project
                .runtime_bindings
                .insert(binding_key.clone(), binding);
            // A capability is fenced to the endpoint generation.  Rebinding
            // the same binding ID therefore revokes the old capability in
            // the same candidate transaction.
            project.master_grants.remove(&binding_key);
            Ok(())
        })
    }

    /// Validate an actor binding against the current host state.  This is a
    /// pure lookup and never repairs or advances a stale binding.
    pub fn validate_binding(&self, incoming: &RuntimeBinding) -> Result<(), StateError> {
        incoming.validate()?;
        let route_scope = incoming.route_scope();
        let Some(current) = self.lookup_binding_for(&route_scope, &incoming.binding_id) else {
            return Err(StateError::BindingNotFound(
                incoming.binding_id.as_str().to_owned(),
            ));
        };
        if !current.same_principal(incoming) {
            return Err(StateError::BindingConflict(format!(
                "binding {} does not belong to the registered principal",
                incoming.binding_id
            )));
        }
        if incoming.endpoint_generation < current.endpoint_generation {
            return Err(StateError::StaleBinding {
                binding_id: incoming.binding_id.as_str().to_owned(),
                expected_generation: current.endpoint_generation,
                observed_generation: incoming.endpoint_generation,
            });
        }
        if incoming.endpoint_generation != current.endpoint_generation {
            return Err(StateError::BindingConflict(format!(
                "binding {} generation {} is not current generation {}",
                incoming.binding_id, incoming.endpoint_generation, current.endpoint_generation
            )));
        }
        if incoming != current {
            return Err(StateError::BindingConflict(format!(
                "binding {} identity differs from the registered endpoint",
                incoming.binding_id
            )));
        }
        Ok(())
    }

    pub fn grant_master(&mut self, grant: MasterGrant) -> Result<StateVersion, StateError> {
        grant.validate()?;
        let scope_key = grant.project_scope.as_str().to_owned();
        let binding_key = grant.binding_id.as_str().to_owned();
        let project = self
            .lookup_project(&grant.project_scope)
            .ok_or_else(|| StateError::ProjectNotRegistered(scope_key.clone()))?;
        let Some(binding) = project.lookup_binding(&grant.binding_id) else {
            return Err(StateError::BindingNotFound(binding_key));
        };
        if binding.app_scope_id != grant.app_scope_id || binding.agent_id != grant.agent_id {
            return Err(StateError::MasterGrantBindingMismatch(format!(
                "grant {} targets a different app scope or agent",
                grant.binding_id
            )));
        }
        if grant.endpoint_generation != binding.endpoint_generation {
            return Err(StateError::StaleBinding {
                binding_id: grant.binding_id.as_str().to_owned(),
                expected_generation: binding.endpoint_generation,
                observed_generation: grant.endpoint_generation,
            });
        }
        if let Some(current) = project.lookup_master_grant(&grant.binding_id) {
            if current == &grant {
                return Ok(self.version());
            }
            return Err(StateError::MasterGrantConflict(format!(
                "binding {} already has a different grant",
                grant.binding_id
            )));
        }

        self.mutate(|next| {
            let project = next
                .projects
                .get_mut(&scope_key)
                .ok_or_else(|| StateError::ProjectNotRegistered(scope_key.clone()))?;
            project.master_grants.insert(binding_key, grant);
            Ok(())
        })
    }

    /// Revoke the current master capability for one scoped binding.  The
    /// operation is idempotent, while a later reconnect also removes the
    /// grant automatically through `bind_runtime`.
    pub fn revoke_master(
        &mut self,
        project_scope: &ProjectScopeId,
        binding_id: &BindingId,
    ) -> Result<StateVersion, StateError> {
        validate_project_scope(project_scope)?;
        validate_binding_id(binding_id)?;
        let scope_key = project_scope.as_str().to_owned();
        let project = self
            .lookup_project(project_scope)
            .ok_or_else(|| StateError::ProjectNotRegistered(scope_key.clone()))?;
        if project.lookup_master_grant(binding_id).is_none() {
            return Ok(self.version());
        }

        self.mutate(|next| {
            let project = next
                .projects
                .get_mut(&scope_key)
                .ok_or_else(|| StateError::ProjectNotRegistered(scope_key.clone()))?;
            project.master_grants.remove(binding_id.as_str());
            Ok(())
        })
    }

    pub fn role_for_binding(
        &self,
        project_scope: &ProjectScopeId,
        binding_id: &BindingId,
    ) -> PeerRole {
        let Some(project) = self.lookup_project(project_scope) else {
            return PeerRole::Peer;
        };
        let Some(binding) = project.lookup_binding(binding_id) else {
            return PeerRole::Peer;
        };
        project
            .lookup_master_grant(binding_id)
            .filter(|grant| grant.endpoint_generation == binding.endpoint_generation)
            .map_or(PeerRole::Peer, |_| PeerRole::Master)
    }

    pub fn role_for_route(&self, route_scope: &RouteScope, binding_id: &BindingId) -> PeerRole {
        let Some(binding) = self.lookup_binding_for(route_scope, binding_id) else {
            return PeerRole::Peer;
        };
        self.lookup_master_grant_for(route_scope, binding_id)
            .filter(|grant| grant.endpoint_generation == binding.endpoint_generation)
            .map_or(PeerRole::Peer, |_| PeerRole::Master)
    }

    pub fn record_command(
        &mut self,
        command_id: CommandId,
        operation_id: OperationId,
        outcome: Value,
    ) -> Result<CommandReceipt, StateError> {
        validate_command_id(&command_id)?;
        validate_operation_id(&operation_id)?;
        if let Some(existing) = self.lookup_command_receipt(&command_id) {
            if existing.operation_id != operation_id {
                return Err(StateError::CommandIdReuse {
                    command_id: command_id.as_str().to_owned(),
                    existing_operation: existing.operation_id.as_str().to_owned(),
                    observed_operation: operation_id.as_str().to_owned(),
                });
            }
            if existing.outcome == outcome {
                return Ok(existing.clone());
            }
            return Err(StateError::ReceiptConflict(format!(
                "command {} was already recorded with a different outcome",
                command_id
            )));
        }
        if let Some(existing) = self
            .command_receipts
            .values()
            .find(|receipt| receipt.operation_id == operation_id)
        {
            return Err(StateError::OperationIdReuse {
                operation_id: operation_id.as_str().to_owned(),
                existing_command: existing.command_id.as_str().to_owned(),
            });
        }

        let sequence = self
            .sequence
            .checked_add(1)
            .ok_or(StateError::CounterOverflow("sequence"))?;
        let revision = self
            .revision
            .checked_add(1)
            .ok_or(StateError::CounterOverflow("revision"))?;
        let receipt = CommandReceipt {
            command_id: command_id.clone(),
            operation_id,
            epoch: self.epoch,
            sequence,
            revision,
            outcome,
        };
        receipt.validate()?;
        let committed = receipt.clone();
        self.mutate_with(|next| {
            next.command_receipts
                .insert(command_id.as_str().to_owned(), receipt);
            Ok(committed)
        })
        .map(|(receipt, _)| receipt)
    }

    /// Compare-and-swap is the caller-visible transaction fence.  The
    /// mutation runs on a clone and is published only after invariants pass;
    /// a failed closure, validation error or stale expected revision leaves
    /// this value untouched.  The closure edits project data, while the
    /// helper owns the one host-wide sequence/revision increment.
    pub fn compare_and_swap<F>(
        &mut self,
        expected_revision: u64,
        mutate: F,
    ) -> Result<StateVersion, StateError>
    where
        F: FnOnce(&mut GlobalState) -> Result<(), StateError>,
    {
        if self.revision != expected_revision {
            return Err(StateError::CompareAndSwapMismatch {
                expected: expected_revision,
                observed: self.revision,
            });
        }
        let mut next = self.clone();
        mutate(&mut next)?;
        // Counter and epoch ownership stays with this helper.  Direct writes
        // to those fields inside the closure are ignored rather than allowed
        // to forge a host ordering value.
        next.epoch = self.epoch;
        next.sequence = self.sequence;
        next.revision = self.revision;
        let version = next.bump_counters()?;
        next.validate()?;
        *self = next;
        Ok(version)
    }

    fn mutate<F>(&mut self, mutate: F) -> Result<StateVersion, StateError>
    where
        F: FnOnce(&mut GlobalState) -> Result<(), StateError>,
    {
        self.mutate_with(|next| {
            mutate(next)?;
            Ok(())
        })
        .map(|(_, version)| version)
    }

    fn mutate_with<R, F>(&mut self, mutate: F) -> Result<(R, StateVersion), StateError>
    where
        F: FnOnce(&mut GlobalState) -> Result<R, StateError>,
    {
        let mut next = self.clone();
        let result = mutate(&mut next)?;
        let version = next.bump_counters()?;
        next.validate()?;
        *self = next;
        Ok((result, version))
    }

    fn bump_counters(&mut self) -> Result<StateVersion, StateError> {
        let sequence = self
            .sequence
            .checked_add(1)
            .ok_or(StateError::CounterOverflow("sequence"))?;
        let revision = self
            .revision
            .checked_add(1)
            .ok_or(StateError::CounterOverflow("revision"))?;
        self.sequence = sequence;
        self.revision = revision;
        Ok(self.version())
    }
}

fn validate_project_scope(scope: &ProjectScopeId) -> Result<(), StateError> {
    ProjectScopeId::new(scope.as_str().to_owned())
        .map_err(|error| StateError::invalid("project scope", error.to_string()))
        .map(|_| ())
}

fn validate_route_scope(scope: &RouteScope) -> Result<(), StateError> {
    validate_app_scope(&scope.app_scope_id)?;
    validate_project_scope(&scope.project_scope_id)
}

fn validate_app_scope(scope: &AppServerId) -> Result<(), StateError> {
    AppServerId::new(scope.as_str().to_owned())
        .map_err(|error| StateError::invalid("app scope", error.to_string()))
        .map(|_| ())
}

fn validate_agent_id(id: &AgentId) -> Result<(), StateError> {
    AgentId::new(id.as_str().to_owned())
        .map_err(|error| StateError::invalid("agent id", error.to_string()))
        .map(|_| ())
}

fn validate_runtime_id(id: &RuntimeId) -> Result<(), StateError> {
    RuntimeId::new(id.as_str().to_owned())
        .map_err(|error| StateError::invalid("runtime id", error.to_string()))
        .map(|_| ())
}

fn validate_binding_id(id: &BindingId) -> Result<(), StateError> {
    BindingId::new(id.as_str().to_owned())
        .map_err(|error| StateError::invalid("binding id", error.to_string()))
        .map(|_| ())
}

fn validate_native_thread_id(id: &NativeThreadId) -> Result<(), StateError> {
    NativeThreadId::new(id.as_str().to_owned())
        .map_err(|error| StateError::invalid("native thread id", error.to_string()))
        .map(|_| ())
}

fn validate_command_id(id: &CommandId) -> Result<(), StateError> {
    CommandId::new(id.as_str().to_owned())
        .map_err(|error| StateError::invalid("command id", error.to_string()))
        .map(|_| ())
}

fn validate_operation_id(id: &OperationId) -> Result<(), StateError> {
    OperationId::new(id.as_str().to_owned())
        .map_err(|error| StateError::invalid("operation id", error.to_string()))
        .map(|_| ())
}

fn validate_non_empty_text(field: &'static str, value: &str) -> Result<(), StateError> {
    if value.trim().is_empty() {
        return Err(StateError::invalid(field, "must not be empty"));
    }
    if value.chars().any(char::is_control) {
        return Err(StateError::invalid(
            field,
            "must not contain control characters",
        ));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn project_scope() -> ProjectScopeId {
        GlobalState::canonical_project_scope(Path::new(env!("CARGO_MANIFEST_DIR")))
            .expect("repository root is canonical")
    }

    fn app_scope(id: &str) -> AppServerId {
        AppServerId::new(id).expect("app scope id")
    }

    fn registration(scope: &ProjectScopeId, app: &str) -> ProjectRegistration {
        ProjectRegistration::new(scope.clone(), app_scope(app)).expect("registration")
    }

    fn binding(
        scope: &ProjectScopeId,
        app: &str,
        agent: &str,
        runtime: &str,
        binding_id: &str,
        generation: u64,
    ) -> RuntimeBinding {
        RuntimeBinding::new(
            scope.clone(),
            app_scope(app),
            AgentId::new(agent).unwrap(),
            RuntimeId::new(runtime).unwrap(),
            BindingId::new(binding_id).unwrap(),
            generation,
            None,
        )
        .expect("binding")
    }

    fn grant(
        scope: &ProjectScopeId,
        app: &str,
        agent: &str,
        binding_id: &str,
        generation: u64,
    ) -> MasterGrant {
        MasterGrant::new(
            scope.clone(),
            app_scope(app),
            AgentId::new(agent).unwrap(),
            "task-scoped",
            "operator",
            "user approved",
            BindingId::new(binding_id).unwrap(),
            generation,
            1,
        )
        .expect("master grant")
    }

    #[test]
    fn different_projects_are_stored_without_overwriting_each_other() {
        let root = project_scope();
        let second = ProjectScopeId::new(format!("{}/second", root.as_str())).unwrap();
        let mut state = GlobalState::default();
        state
            .register_project(registration(&root, "app-one"))
            .unwrap();
        state
            .register_project(registration(&second, "app-two"))
            .unwrap();

        assert_eq!(state.projects.len(), 2);
        assert!(state.lookup_project(&root).is_some());
        assert!(state.lookup_project(&second).is_some());
        state.validate().unwrap();
    }

    #[test]
    fn route_registration_is_idempotent_and_conflicts_keep_the_original() {
        let scope = project_scope();
        let route = RouteScope {
            app_scope_id: app_scope("app-one"),
            project_scope_id: scope.clone(),
        };
        let mut state = GlobalState::default();
        let first = state
            .register_project_for_route(&route, 11)
            .expect("first route registration");
        let replay = state
            .register_project_for_route(&route, 11)
            .expect("identical route registration is idempotent");
        assert_eq!(replay, first);
        assert_eq!(state.projects.len(), 1);

        let conflict = state.register_project_for_route(&route, 12);
        assert!(matches!(
            conflict,
            Err(StateError::RegistrationConflict {
                project_scope,
                app_scope_id
            }) if project_scope == scope.as_str() && app_scope_id == "app-one"
        ));
        assert_eq!(
            state
                .lookup_registration(&scope, &route.app_scope_id)
                .unwrap()
                .registered_at_ms,
            11
        );
        state.validate().unwrap();
    }

    #[test]
    fn route_lookup_rejects_unknown_project_or_app_scope() {
        let scope = project_scope();
        let unknown_project =
            ProjectScopeId::new(format!("{}/unknown", scope.as_str())).expect("scope");
        let known_route = RouteScope {
            app_scope_id: app_scope("app-one"),
            project_scope_id: scope.clone(),
        };
        let unknown_app_route = RouteScope {
            app_scope_id: app_scope("app-unknown"),
            project_scope_id: scope.clone(),
        };
        let unknown_project_route = RouteScope {
            app_scope_id: app_scope("app-one"),
            project_scope_id: unknown_project.clone(),
        };
        let mut state = GlobalState::default();
        state
            .register_project_for_route(&known_route, 1)
            .expect("known route registration");

        assert!(state.lookup_project_for_route(&unknown_app_route).is_none());
        assert!(state
            .lookup_project_for_route(&unknown_project_route)
            .is_none());
        let binding = binding(
            &unknown_project,
            "app-one",
            "agent-one",
            "runtime-one",
            "binding-one",
            1,
        );
        let before = state.version();
        assert!(matches!(
            state.bind_runtime(binding),
            Err(StateError::ProjectNotRegistered(_))
        ));
        assert_eq!(state.version(), before);
        state.validate().unwrap();
    }

    #[test]
    fn same_project_different_app_scopes_keep_separate_registrations_and_bindings() {
        let scope = project_scope();
        let mut state = GlobalState::default();
        state
            .register_project(registration(&scope, "app-one"))
            .unwrap();
        state
            .register_project(registration(&scope, "app-two"))
            .unwrap();
        state
            .bind_runtime(binding(
                &scope,
                "app-one",
                "agent-one",
                "runtime-one",
                "binding-one",
                1,
            ))
            .unwrap();
        state
            .bind_runtime(binding(
                &scope,
                "app-two",
                "agent-two",
                "runtime-two",
                "binding-two",
                1,
            ))
            .unwrap();

        let project = state.lookup_project(&scope).unwrap();
        assert_eq!(project.registrations.len(), 2);
        assert_eq!(project.runtime_bindings.len(), 2);
        assert_eq!(
            project.registrations["app-one"].app_scope_id.as_str(),
            "app-one"
        );
        assert_eq!(
            project.registrations["app-two"].app_scope_id.as_str(),
            "app-two"
        );
        assert_eq!(
            state
                .lookup_binding_for(
                    &binding(
                        &scope,
                        "app-two",
                        "agent-two",
                        "runtime-two",
                        "binding-two",
                        1
                    )
                    .route_scope(),
                    &BindingId::new("binding-two").unwrap()
                )
                .unwrap()
                .app_scope_id
                .as_str(),
            "app-two"
        );
        state.validate().unwrap();
    }

    #[test]
    fn same_binding_id_in_different_projects_is_route_scoped() {
        let first_scope = project_scope();
        let second_scope = ProjectScopeId::new(format!("{}/second", first_scope.as_str()))
            .expect("second project scope");
        let mut state = GlobalState::default();
        state
            .register_project(registration(&first_scope, "app-one"))
            .unwrap();
        state
            .register_project(registration(&second_scope, "app-one"))
            .unwrap();

        let first = binding(
            &first_scope,
            "app-one",
            "agent-one",
            "runtime-one",
            "shared-binding",
            1,
        );
        let second = binding(
            &second_scope,
            "app-one",
            "agent-two",
            "runtime-two",
            "shared-binding",
            1,
        );
        state.bind_runtime(first.clone()).unwrap();
        state.bind_runtime(second.clone()).unwrap();

        let shared_id = BindingId::new("shared-binding").unwrap();
        assert!(state.lookup_binding(&shared_id).is_none());
        assert_eq!(
            state.lookup_binding_for(&first.route_scope(), &shared_id),
            Some(&first)
        );
        assert_eq!(
            state.lookup_binding_for(&second.route_scope(), &shared_id),
            Some(&second)
        );
        state.validate_binding(&first).unwrap();
        state.validate_binding(&second).unwrap();
        state.validate().unwrap();
    }

    #[test]
    fn master_grants_are_isolated_by_project_route_and_generation() {
        let first_scope = project_scope();
        let second_scope = ProjectScopeId::new(format!("{}/second", first_scope.as_str()))
            .expect("second project scope");
        let first_route = RouteScope {
            app_scope_id: app_scope("app-one"),
            project_scope_id: first_scope.clone(),
        };
        let second_route = RouteScope {
            app_scope_id: app_scope("app-one"),
            project_scope_id: second_scope.clone(),
        };
        let mut state = GlobalState::default();
        state.register_project_for_route(&first_route, 1).unwrap();
        state.register_project_for_route(&second_route, 2).unwrap();
        let first_binding = binding(
            &first_scope,
            "app-one",
            "agent-one",
            "runtime-one",
            "shared-binding",
            3,
        );
        let second_binding = binding(
            &second_scope,
            "app-one",
            "agent-two",
            "runtime-two",
            "shared-binding",
            4,
        );
        state.bind_runtime(first_binding.clone()).unwrap();
        state.bind_runtime(second_binding.clone()).unwrap();
        state
            .grant_master(grant(
                &first_scope,
                "app-one",
                "agent-one",
                "shared-binding",
                3,
            ))
            .unwrap();
        state
            .grant_master(grant(
                &second_scope,
                "app-one",
                "agent-two",
                "shared-binding",
                4,
            ))
            .unwrap();

        let shared_id = BindingId::new("shared-binding").unwrap();
        assert_eq!(
            state
                .lookup_master_grant_for(&first_route, &shared_id)
                .unwrap()
                .project_scope,
            first_scope
        );
        assert_eq!(
            state
                .lookup_master_grant_for(&second_route, &shared_id)
                .unwrap()
                .project_scope,
            second_scope
        );
        assert_eq!(
            state.role_for_route(&first_route, &shared_id),
            PeerRole::Master
        );
        assert_eq!(
            state.role_for_route(&second_route, &shared_id),
            PeerRole::Master
        );
        let wrong_route = RouteScope {
            app_scope_id: app_scope("app-unknown"),
            project_scope_id: first_scope,
        };
        assert_eq!(
            state.role_for_route(&wrong_route, &shared_id),
            PeerRole::Peer
        );
        state.validate().unwrap();
    }

    #[test]
    fn sequence_and_revision_advance_together_and_cas_is_fenced() {
        let scope = project_scope();
        let mut state = GlobalState::default();
        assert_eq!(
            state.version(),
            StateVersion {
                epoch: 1,
                sequence: 0,
                revision: 0
            }
        );
        let first = state
            .register_project(registration(&scope, "app-one"))
            .unwrap();
        let second = state
            .bind_runtime(binding(
                &scope,
                "app-one",
                "agent-one",
                "runtime-one",
                "binding-one",
                1,
            ))
            .unwrap();
        assert_eq!((first.sequence, first.revision), (1, 1));
        assert_eq!((second.sequence, second.revision), (2, 2));

        let third = state
            .compare_and_swap(second.revision, |next| {
                next.projects
                    .get_mut(scope.as_str())
                    .unwrap()
                    .registrations
                    .get_mut("app-one")
                    .unwrap()
                    .registered_at_ms = 7;
                Ok(())
            })
            .unwrap();
        assert_eq!((third.sequence, third.revision), (3, 3));
        assert!(matches!(
            state.compare_and_swap(2, |_| Ok(())),
            Err(StateError::CompareAndSwapMismatch {
                expected: 2,
                observed: 3
            })
        ));
        state.validate().unwrap();
    }

    #[test]
    fn old_generation_is_rejected_without_mutating_state() {
        let scope = project_scope();
        let mut state = GlobalState::default();
        state
            .register_project(registration(&scope, "app-one"))
            .unwrap();
        let current = binding(
            &scope,
            "app-one",
            "agent-one",
            "runtime-one",
            "binding-one",
            2,
        );
        state.bind_runtime(current.clone()).unwrap();
        let before = state.version();
        let stale = binding(
            &scope,
            "app-one",
            "agent-one",
            "runtime-one",
            "binding-one",
            1,
        );
        assert!(matches!(
            state.validate_binding(&stale),
            Err(StateError::StaleBinding {
                expected_generation: 2,
                observed_generation: 1,
                ..
            })
        ));
        assert!(matches!(
            state.bind_runtime(stale),
            Err(StateError::StaleBinding {
                expected_generation: 2,
                observed_generation: 1,
                ..
            })
        ));
        assert_eq!(state.version(), before);
        assert_eq!(
            state
                .lookup_binding(&current.binding_id)
                .unwrap()
                .endpoint_generation,
            2
        );
    }

    #[test]
    fn registration_defaults_to_peer_until_an_explicit_current_grant() {
        let scope = project_scope();
        let mut state = GlobalState::default();
        state
            .register_project(registration(&scope, "app-one"))
            .unwrap();
        let runtime = binding(
            &scope,
            "app-one",
            "agent-one",
            "runtime-one",
            "binding-one",
            4,
        );
        state.bind_runtime(runtime.clone()).unwrap();
        assert_eq!(
            state.role_for_binding(&scope, &runtime.binding_id),
            PeerRole::Peer
        );

        state
            .grant_master(grant(&scope, "app-one", "agent-one", "binding-one", 4))
            .unwrap();
        assert_eq!(
            state.role_for_binding(&scope, &runtime.binding_id),
            PeerRole::Master
        );
        state.validate().unwrap();
    }

    #[test]
    fn reconnect_revokes_old_master_grant_and_explicit_revoke_is_idempotent() {
        let scope = project_scope();
        let mut state = GlobalState::default();
        state
            .register_project(registration(&scope, "app-one"))
            .unwrap();
        let current = binding(
            &scope,
            "app-one",
            "agent-one",
            "runtime-one",
            "binding-one",
            4,
        );
        state.bind_runtime(current.clone()).unwrap();
        state
            .grant_master(grant(&scope, "app-one", "agent-one", "binding-one", 4))
            .unwrap();
        assert_eq!(
            state.role_for_binding(&scope, &current.binding_id),
            PeerRole::Master
        );

        let reconnected = binding(
            &scope,
            "app-one",
            "agent-one",
            "runtime-one",
            "binding-one",
            5,
        );
        state.bind_runtime(reconnected.clone()).unwrap();
        assert!(state
            .lookup_master_grant(&scope, &reconnected.binding_id)
            .is_none());
        assert_eq!(
            state.role_for_binding(&scope, &reconnected.binding_id),
            PeerRole::Peer
        );
        assert!(matches!(
            state.grant_master(grant(&scope, "app-one", "agent-one", "binding-one", 4)),
            Err(StateError::StaleBinding {
                expected_generation: 5,
                observed_generation: 4,
                ..
            })
        ));
        state
            .grant_master(grant(&scope, "app-one", "agent-one", "binding-one", 5))
            .unwrap();
        assert_eq!(
            state.role_for_binding(&scope, &reconnected.binding_id),
            PeerRole::Master
        );

        let before_revoke = state.version();
        state
            .revoke_master(&scope, &reconnected.binding_id)
            .unwrap();
        assert_eq!(
            state.role_for_binding(&scope, &reconnected.binding_id),
            PeerRole::Peer
        );
        assert!(state
            .lookup_master_grant(&scope, &reconnected.binding_id)
            .is_none());
        let after_revoke = state.version();
        assert!(after_revoke.revision > before_revoke.revision);
        state
            .revoke_master(&scope, &reconnected.binding_id)
            .unwrap();
        assert_eq!(state.version(), after_revoke);
        state.validate_binding(&reconnected).unwrap();
        state.validate().unwrap();
    }

    #[test]
    fn command_receipts_are_host_wide_and_idempotent() {
        let mut state = GlobalState::default();
        let first = state
            .record_command(
                CommandId::new("command-one").unwrap(),
                OperationId::new("operation-one").unwrap(),
                serde_json::json!({"ok": true}),
            )
            .unwrap();
        let before = state.version();
        let replay = state
            .record_command(
                CommandId::new("command-one").unwrap(),
                OperationId::new("operation-one").unwrap(),
                serde_json::json!({"ok": true}),
            )
            .unwrap();
        assert_eq!(first, replay);
        assert_eq!(state.version(), before);
        assert!(matches!(
            state.record_command(
                CommandId::new("command-one").unwrap(),
                OperationId::new("operation-two").unwrap(),
                Value::Null,
            ),
            Err(StateError::CommandIdReuse { .. })
        ));
        state.validate().unwrap();
    }
}
