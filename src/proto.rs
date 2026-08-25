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
    ClaimAcquire {
        worker_id: String,
        claim_id: String,
        intent: Option<String>,
        lease_ms: Option<u64>,
        #[serde(default)]
        force: bool,
    },
    ClaimRelease {
        worker_id: String,
        claim_id: String,
    },
    ClaimRenew {
        worker_id: String,
        claim_id: String,
        lease_ms: Option<u64>,
    },
    ClaimStatus {
        claim_id: Option<String>,
    },
    ClaimWait {
        worker_id: String,
        claim_id: String,
        #[serde(default = "default_wait_timeout")]
        timeout_ms: u64,
    },
    Ping,
}

fn default_poll_timeout() -> u64 {
    600_000
}
fn default_wait_timeout() -> u64 {
    1_800_000
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
        Resp { ok: true, error: None, data: v }
    }
    pub fn err(msg: impl Into<String>) -> Self {
        let m = msg.into();
        eprintln!("collab: error: {}", m);
        Resp { ok: false, error: Some(m), data: serde_json::Value::Null }
    }
}

pub const MSG_TYPES: [&str; 3] = ["notify", "request", "reply"];
