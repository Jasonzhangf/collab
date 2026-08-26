use serde::{Deserialize, Serialize};

#[derive(Debug, Serialize, Deserialize)]
#[serde(tag = "op")]
pub enum Req {
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
        body: String,
        in_reply_to: Option<String>,
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
    TaskDeliver {
        worker_id: String,
        token: String,
        task_id: String,
        evidence: Option<String>,
    },
    TaskClose {
        worker_id: String,
        token: String,
        task_id: String,
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
    Role {
        worker_id: String,
    },
    Workers,
    MasterId,
    Ping,
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
}

pub const MSG_TYPES: [&str; 3] = ["notify", "request", "reply"];
