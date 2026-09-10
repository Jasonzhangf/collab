use serde::{Deserialize, Serialize};

use crate::identity::{
    validate_binding, BindingId, CommandId, DispatchId, MessageId, OperationId, RuntimeIdentity,
    TurnId,
};
use crate::scope::{ProjectScopeId, RouteScope};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CommandEnvelope {
    pub command_id: CommandId,
    pub operation_id: OperationId,
    pub actor_binding_id: BindingId,
    pub endpoint_generation: u64,
    pub scope: RouteScope,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub expected_revision: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub turn_id: Option<TurnId>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub message_id: Option<MessageId>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub dispatch_id: Option<DispatchId>,
}

impl CommandEnvelope {
    pub fn new(
        command_id: CommandId,
        operation_id: OperationId,
        actor_binding_id: BindingId,
        endpoint_generation: u64,
        scope: RouteScope,
        expected_revision: Option<u64>,
        turn_id: Option<TurnId>,
        message_id: Option<MessageId>,
        dispatch_id: Option<DispatchId>,
    ) -> Self {
        Self {
            command_id,
            operation_id,
            actor_binding_id,
            endpoint_generation,
            scope,
            expected_revision,
            turn_id,
            message_id,
            dispatch_id,
        }
    }

    pub fn validate(&self) -> anyhow::Result<()> {
        crate::identity::validate_id_for_protocol(self.command_id.as_str())?;
        crate::identity::validate_id_for_protocol(self.operation_id.as_str())?;
        crate::identity::validate_id_for_protocol(self.actor_binding_id.as_str())?;
        self.scope.validate()?;
        for id in [
            self.turn_id.as_ref().map(TurnId::as_str),
            self.message_id.as_ref().map(MessageId::as_str),
            self.dispatch_id.as_ref().map(DispatchId::as_str),
        ]
        .into_iter()
        .flatten()
        {
            crate::identity::validate_id_for_protocol(id)?;
        }
        Ok(())
    }

    pub fn validate_for(
        &self,
        registered: &RuntimeIdentity,
        registered_scope: &RouteScope,
    ) -> anyhow::Result<()> {
        registered.validate()?;
        self.validate()?;
        registered_scope.validate()?;
        let incoming = RuntimeIdentity {
            agent_id: registered.agent_id.clone(),
            runtime_id: registered.runtime_id.clone(),
            appserver_id: registered.appserver_id.clone(),
            endpoint_generation: self.endpoint_generation,
            binding_id: self.actor_binding_id.clone(),
            native_thread_id: registered.native_thread_id.clone(),
        };
        validate_binding(registered, &incoming)?;
        self.scope.validate_same_route(registered_scope)?;
        if self.scope.app_scope_id != registered.appserver_id {
            anyhow::bail!("command app scope does not match actor AppServer identity");
        }
        Ok(())
    }
}

/// The project identity carried by every project-scoped wire request.  The
/// root and scope are deliberately both present: the root is the registered
/// filesystem context, while the scope is the value used by route checks.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ProjectContext {
    pub canonical_root: String,
    pub project_scope: ProjectScopeId,
}

impl ProjectContext {
    pub fn for_registered_root(root: &std::path::Path) -> anyhow::Result<Self> {
        let canonical = std::fs::canonicalize(root)?;
        let canonical_root = canonical.to_str().ok_or_else(|| {
            anyhow::anyhow!("registered project root must be valid UTF-8 for the wire context")
        })?;
        let project_scope = ProjectScopeId::new(canonical_root.to_owned())?;
        Ok(Self {
            canonical_root: canonical_root.to_owned(),
            project_scope,
        })
    }

    pub fn validate(&self) -> anyhow::Result<()> {
        if self.canonical_root.is_empty() {
            anyhow::bail!("project context canonical root must not be empty");
        }
        if self.canonical_root.chars().any(char::is_control) {
            anyhow::bail!("project context canonical root must not contain control characters");
        }
        if !std::path::Path::new(&self.canonical_root).is_absolute() {
            anyhow::bail!("project context canonical root must be an absolute path");
        }
        let expected_scope = ProjectScopeId::new(self.canonical_root.clone())?;
        if self.project_scope != expected_scope {
            anyhow::bail!("project context scope does not match its canonical root");
        }
        Ok(())
    }

    pub fn validate_registered_root(&self, root: &std::path::Path) -> anyhow::Result<()> {
        self.validate()?;
        let expected = Self::for_registered_root(root)?;
        if self != &expected {
            anyhow::bail!(
                "project context does not match registered project root {}",
                root.display()
            );
        }
        Ok(())
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "op")]
pub enum Req {
    SubagentObserve {
        id: Option<String>,
        snapshot_lines: Option<usize>,
    },
    Subagent {
        worker_id: String,
        token: String,
        command: crate::subagent::Action,
        #[serde(default)]
        launch_env: std::collections::BTreeMap<String, String>,
    },
    Register {
        worker_id: String,
        token: String,
        pane: Option<String>,
        cwd: String,
    },
    Send {
        from: String,
        #[serde(default)]
        worker_id: Option<String>,
        #[serde(default)]
        token: Option<String>,
        #[serde(default)]
        command: Option<CommandEnvelope>,
        to: String,
        #[serde(rename = "type")]
        mtype: String,
        #[serde(default)]
        subject: Option<String>,
        body: String,
        in_reply_to: Option<String>,
        #[serde(default = "default_delivery_mode")]
        delivery: String,
    },
    CrossProjectSend {
        from: String,
        from_project: String,
        source_master_assigned_by: String,
        source_master_approval: Option<String>,
        source_master_assigned_ms: i64,
        to: String,
        subject: String,
        body: String,
        in_reply_to: Option<String>,
    },
    NotificationMethods,
    NotificationSubscribe {
        worker_id: String,
        token: String,
        event: String,
        subject: Option<String>,
        trigger_ms: Option<i64>,
        #[serde(default)]
        trigger_times_ms: Vec<i64>,
        #[serde(default)]
        interval_ms: Option<i64>,
        #[serde(default = "crate::server::state::default_repeat_count")]
        repeat_count: u32,
        ttl_seconds: u64,
    },
    NotificationStatus {
        worker_id: String,
        token: String,
    },
    NotificationUnsubscribe {
        worker_id: String,
        token: String,
        subscription_id: String,
    },
    Poll {
        worker_id: String,
        token: String,
        #[serde(default = "default_poll_timeout")]
        timeout_ms: u64,
    },
    Ack {
        worker_id: String,
        token: String,
        ids: Vec<String>,
    },
    Inbox {
        worker_id: String,
        token: String,
    },
    Context {
        worker_id: String,
        token: String,
    },
    MsgStatus {
        msg_id: String,
    },
    TaskRegister {
        worker_id: String,
        token: String,
        task_id: String,
        owner: Option<String>,
        feature_id: Option<String>,
        worktree_path: Option<String>,
        branch: Option<String>,
        base_commit: Option<String>,
        #[serde(default = "crate::server::state::default_priority")]
        priority: String,
        next_step: Option<String>,
        #[serde(default)]
        goal_prompt: Option<String>,
    },
    TaskRelocate {
        worker_id: String,
        token: String,
        task_id: String,
        worktree_path: String,
        branch: Option<String>,
        base_commit: Option<String>,
    },
    TaskUpdate {
        worker_id: String,
        token: String,
        task_id: String,
        status: Option<String>,
        next_step: Option<String>,
    },
    TaskAccept {
        worker_id: String,
        token: String,
        task_id: String,
    },
    TaskClaim {
        worker_id: String,
        token: String,
        task_id: String,
    },
    TaskWait {
        worker_id: String,
        token: String,
        task_id: String,
        blocking_task_id: String,
    },
    TaskDeliver {
        worker_id: String,
        token: String,
        task_id: String,
        evidence: Option<String>,
        worktree: Option<String>,
    },
    TaskReview {
        worker_id: String,
        token: String,
        task_id: String,
        accept: bool,
        rework: bool,
        evidence: String,
    },
    TaskIntegrated {
        worker_id: String,
        token: String,
        task_id: String,
        commit: String,
        evidence: String,
    },
    TaskClose {
        worker_id: String,
        token: String,
        task_id: String,
        force: bool,
        reason: Option<String>,
    },
    TaskDispatch {
        worker_id: String,
        token: String,
    },
    TaskStatus {
        task_id: Option<String>,
    },
    TaskConflicts {
        feature_id: Option<String>,
        worktree_path: Option<String>,
    },
    MigrationInspect {
        worker_id: String,
        token: String,
    },
    MigrationPlan {
        worker_id: String,
        token: String,
    },
    MigrationApply {
        worker_id: String,
        token: String,
    },
    MigrationVerify {
        worker_id: String,
        token: String,
    },
    #[serde(alias = "RootPromote")]
    MasterPromote {
        worker_id: String,
        token: String,
        approval: String,
    },
    #[serde(alias = "RootDelegate")]
    MasterDelegate {
        worker_id: String,
        token: String,
        target_id: String,
    },
    #[serde(alias = "RootStatus")]
    MasterStatus,
    Role {
        worker_id: String,
    },
    Workers,
    WorkerStatus {
        worker_id: Option<String>,
    },
    /// Live master retires a worker registration, optionally killing its tmux session.
    WorkerClose {
        worker_id: String,
        token: String,
        target_id: String,
        reason: String,
        kill_session: bool,
    },
    MasterId,
    MasterRecover {
        worker_id: String,
        token: String,
        session: String,
    },
    TransferMaster {
        worker_id: String,
        token: String,
        target_id: String,
    },
    RemoveWorker {
        worker_id: String,
        token: String,
        target_id: String,
        #[serde(default)]
        force: bool,
    },
    ResetBindings {
        confirm: bool,
    },
    Shutdown {
        operator: bool,
    },
    Ping,
    StatusAll,
    MailboxRead {
        #[serde(default)]
        all: bool,
        #[serde(default)]
        sort: Option<String>,
        #[serde(default)]
        worker_id: Option<String>,
    },
}

/// Wire envelope for the resident host daemon.  The request body keeps the
/// v1 tagged operation shape so existing command names remain compatible;
/// project context is an additive top-level field.  A missing context is
/// accepted only for the context-free Ping readiness probe and is rejected by
/// the server for every project-scoped operation.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RequestEnvelope {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub project_context: Option<ProjectContext>,
    #[serde(flatten)]
    pub request: Req,
}

impl RequestEnvelope {
    pub fn new(request: Req, project_context: Option<ProjectContext>) -> Self {
        Self {
            project_context,
            request,
        }
    }

    pub fn with_context(request: Req, project_context: ProjectContext) -> Self {
        Self::new(request, Some(project_context))
    }

    pub fn into_parts(self) -> (Option<ProjectContext>, Req) {
        (self.project_context, self.request)
    }
}

fn default_delivery_mode() -> String {
    "immediate".into()
}

fn default_poll_timeout() -> u64 {
    600_000
}

#[derive(Debug, Serialize, Deserialize)]
pub struct Resp {
    pub ok: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
    #[serde(flatten)]
    pub data: serde_json::Value,
}

impl Resp {
    pub fn data(v: serde_json::Value) -> Self {
        Resp {
            ok: true,
            error: None,
            data: v,
        }
    }
    pub fn err(msg: impl Into<String>) -> Self {
        let m = msg.into();
        eprintln!("collab: error: {}", m);
        Resp {
            ok: false,
            error: Some(m),
            data: serde_json::Value::Null,
        }
    }

    pub fn err_data(msg: impl Into<String>, data: serde_json::Value) -> Self {
        let m = msg.into();
        eprintln!("collab: error: {}", m);
        Resp {
            ok: false,
            error: Some(m),
            data,
        }
    }
}

pub const MSG_TYPES: [&str; 3] = ["notify", "request", "reply"];

#[cfg(test)]
mod tests {
    use super::*;
    use crate::identity::{
        AgentId, AppServerId, BindingId, BindingValidationError, CommandId, NativeThreadId,
        OperationId, RuntimeId,
    };
    use crate::scope::RouteScope;
    use std::path::Path;

    fn registered_identity() -> RuntimeIdentity {
        RuntimeIdentity {
            agent_id: AgentId::new("agent-1").unwrap(),
            runtime_id: RuntimeId::new("runtime-1").unwrap(),
            appserver_id: AppServerId::new("appserver-1").unwrap(),
            endpoint_generation: 7,
            binding_id: BindingId::new("binding-1").unwrap(),
            native_thread_id: Some(NativeThreadId::new("thread-1").unwrap()),
        }
    }

    fn registered_scope() -> RouteScope {
        let root = Path::new(env!("CARGO_MANIFEST_DIR"));
        RouteScope::for_registered_project(AppServerId::new("appserver-1").unwrap(), root).unwrap()
    }

    fn envelope(scope: RouteScope) -> CommandEnvelope {
        CommandEnvelope::new(
            CommandId::new("command-1").unwrap(),
            OperationId::new("operation-1").unwrap(),
            BindingId::new("binding-1").unwrap(),
            7,
            scope,
            Some(12),
            Some(TurnId::new("turn-1").unwrap()),
            Some(MessageId::new("message-1").unwrap()),
            Some(DispatchId::new("dispatch-1").unwrap()),
        )
    }

    #[test]
    fn command_envelope_round_trips_all_wire_fields() {
        let command = envelope(registered_scope());
        let encoded = serde_json::to_value(&command).unwrap();
        assert_eq!(encoded["command_id"], "command-1");
        assert_eq!(encoded["operation_id"], "operation-1");
        assert_eq!(encoded["actor_binding_id"], "binding-1");
        assert_eq!(encoded["endpoint_generation"], 7);
        assert_eq!(encoded["scope"]["app_scope_id"], "appserver-1");
        assert_eq!(
            encoded["scope"]["project_scope_id"],
            env!("CARGO_MANIFEST_DIR")
        );
        assert_eq!(encoded["expected_revision"], 12);
        assert_eq!(encoded["turn_id"], "turn-1");
        assert_eq!(encoded["message_id"], "message-1");
        assert_eq!(encoded["dispatch_id"], "dispatch-1");
        assert_eq!(
            serde_json::from_value::<CommandEnvelope>(encoded).unwrap(),
            command
        );
    }

    #[test]
    fn command_envelope_accepts_matching_binding_and_scope() {
        let identity = registered_identity();
        let scope = registered_scope();
        let command = envelope(scope.clone());
        let before_command = command.clone();
        let before_identity = identity.clone();
        let before_scope = scope.clone();

        command.validate_for(&identity, &scope).unwrap();
        assert_eq!(command, before_command);
        assert_eq!(identity, before_identity);
        assert_eq!(scope, before_scope);
    }

    #[test]
    fn command_envelope_rejects_stale_generation_without_mutation() {
        let identity = registered_identity();
        let scope = registered_scope();
        let mut command = envelope(scope.clone());
        command.endpoint_generation = 6;
        let before_command = command.clone();
        let before_identity = identity.clone();
        let before_scope = scope.clone();

        assert!(command.validate_for(&identity, &scope).is_err());
        assert_eq!(command, before_command);
        assert_eq!(identity, before_identity);
        assert_eq!(scope, before_scope);
    }

    #[test]
    fn command_envelope_rejects_wrong_binding_without_mutation() {
        let identity = registered_identity();
        let scope = registered_scope();
        let mut command = envelope(scope.clone());
        command.actor_binding_id = BindingId::new("binding-other").unwrap();
        let before_command = command.clone();
        let before_identity = identity.clone();
        let before_scope = scope.clone();

        assert!(command.validate_for(&identity, &scope).is_err());
        assert_eq!(command, before_command);
        assert_eq!(identity, before_identity);
        assert_eq!(scope, before_scope);
    }

    #[test]
    fn command_envelope_rejects_wrong_app_scope_without_mutation() {
        let identity = registered_identity();
        let registered_scope = registered_scope();
        let wrong_scope = RouteScope::for_registered_project(
            AppServerId::new("appserver-other").unwrap(),
            Path::new(env!("CARGO_MANIFEST_DIR")),
        )
        .unwrap();
        let command = envelope(wrong_scope);
        let before_command = command.clone();
        let before_identity = identity.clone();
        let before_scope = registered_scope.clone();

        assert!(command.validate_for(&identity, &registered_scope).is_err());
        assert_eq!(command, before_command);
        assert_eq!(identity, before_identity);
        assert_eq!(registered_scope, before_scope);
    }

    #[test]
    fn command_envelope_rejects_scope_identity_mismatch_without_mutation() {
        let identity = registered_identity();
        let registered_scope = registered_scope();
        let scope = RouteScope::for_registered_project(
            AppServerId::new("appserver-other").unwrap(),
            Path::new(env!("CARGO_MANIFEST_DIR")),
        )
        .unwrap();
        let command = envelope(scope);
        let before_command = command.clone();
        let before_identity = identity.clone();
        let before_scope = registered_scope.clone();

        assert!(command.validate_for(&identity, &registered_scope).is_err());
        assert_eq!(command, before_command);
        assert_eq!(identity, before_identity);
        assert_eq!(registered_scope, before_scope);
    }

    #[test]
    fn command_envelope_rejects_invalid_wire_identifier_without_mutation() {
        let identity = registered_identity();
        let scope = registered_scope();
        let mut command = envelope(scope.clone());
        command.command_id = serde_json::from_value(serde_json::json!("")).unwrap();
        let before_command = command.clone();
        let before_identity = identity.clone();
        let before_scope = scope.clone();

        assert!(command.validate_for(&identity, &scope).is_err());
        assert_eq!(command, before_command);
        assert_eq!(identity, before_identity);
        assert_eq!(scope, before_scope);
    }

    #[test]
    fn command_envelope_preserves_typed_binding_error_chain() {
        let identity = registered_identity();
        let scope = registered_scope();
        let mut command = envelope(scope.clone());
        command.endpoint_generation = 6;

        let error = command.validate_for(&identity, &scope).unwrap_err();
        assert!(matches!(
            error.downcast_ref::<BindingValidationError>(),
            Some(BindingValidationError::StaleGeneration {
                expected: 7,
                observed: 6
            })
        ));
    }

    #[test]
    fn command_envelope_accepts_long_registered_cwd() {
        let mut root = std::env::temp_dir().join(format!(
            "collab-long-envelope-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        for index in 0..24 {
            root = root.join(format!("segment-{index:02}-abcdef"));
        }
        std::fs::create_dir_all(&root).unwrap();
        let scope =
            RouteScope::for_registered_project(AppServerId::new("appserver-1").unwrap(), &root)
                .unwrap();
        assert!(scope.project_scope_id.as_str().len() > 256);
        let command = envelope(scope.clone());
        command
            .validate_for(&registered_identity(), &scope)
            .unwrap();
        std::fs::remove_dir_all(root).ok();
    }

    #[test]
    fn wire_request_carries_canonical_project_context() {
        let root = std::env::temp_dir().join(format!(
            "collab-proto-context-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(&root).unwrap();
        let context = ProjectContext::for_registered_root(&root).unwrap();
        let request = RequestEnvelope::new(Req::Ping, Some(context.clone()));
        let encoded = serde_json::to_value(&request).unwrap();
        assert_eq!(
            encoded["project_context"]["canonical_root"],
            context.canonical_root
        );
        assert_eq!(
            encoded["project_context"]["project_scope"],
            context.project_scope.as_str()
        );
        assert_eq!(encoded["op"], "Ping");
        let decoded = serde_json::from_value::<RequestEnvelope>(encoded).unwrap();
        assert_eq!(decoded.project_context, request.project_context);
        assert_eq!(
            serde_json::to_value(decoded.request).unwrap(),
            serde_json::json!({"op": "Ping"})
        );
        std::fs::remove_dir_all(root).ok();
    }
}
