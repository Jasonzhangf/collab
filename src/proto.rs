use serde::{Deserialize, Serialize};

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
