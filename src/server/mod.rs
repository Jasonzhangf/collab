pub mod knock;
pub mod state;
pub mod timers;

use state::{now_ms, ClaimRec, Event, Message, State, WorkerRec};
use crate::proto::{Req, Resp, MSG_TYPES};
use crate::scope::Scope;
use crate::server::knock::{append_log, knock_or_log};
use serde_json::json;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};
use tokio::net::UnixListener;

const DEFAULT_LEASE_MS: i64 = 30 * 60 * 1000;
const MAX_POLL_MS: u64 = 3_600_000;
const MAX_WAIT_MS: u64 = 7_200_000;
const POLL_TICK_MS: u64 = 250;

pub struct Server {
    pub root: PathBuf,
    pub state: Mutex<State>,
    pub journal: Mutex<std::fs::File>,
}

impl Server {
    pub fn log_path(&self) -> PathBuf {
        self.root.join(".agent-collab").join("server").join("log.txt")
    }

    /// Apply events to memory and persist them atomically-ordered in the journal.
    fn commit(&self, evs: &[Event]) {
        let mut st = self.state.lock().unwrap();
        let mut j = self.journal.lock().unwrap();
        use std::io::Write;
        for ev in evs {
            st.apply(ev);
            let line = serde_json::to_string(ev).expect("serialize event");
            let _ = writeln!(j, "{}", line);
        }
        let _ = j.flush();
    }
}

pub fn gen_msg_id() -> String {
    use std::sync::atomic::{AtomicU64, Ordering};
    static SEQ: AtomicU64 = AtomicU64::new(1);
    let n = SEQ.fetch_add(1, Ordering::Relaxed);
    format!("m{}-{}", now_ms(), n)
}

fn knock_text(from: &str, mtype: &str, msg_id: &str) -> String {
    format!("[MAIL] from={} type={} id={}", from, mtype, msg_id)
}

fn iso(ms: i64) -> String {
    chrono::DateTime::from_timestamp_millis(ms)
        .map(|d| d.to_rfc3339())
        .unwrap_or_default()
}

// ---------- handlers ----------

fn verify(state: &State, worker_id: &str, token: &str) -> Result<WorkerRec, Resp> {
    match state.workers.get(worker_id) {
        Some(w) if w.token == token => Ok(w.clone()),
        Some(_) => Err(Resp::err("token mismatch: identity does not own this worker_id")),
        None => Err(Resp::err(format!("worker {} not registered", worker_id))),
    }
}

fn handle_register(server: &Server, worker_id: String, token: String, pane: Option<String>, cwd: String) -> Resp {
    {
        let st = server.state.lock().unwrap();
        if let Some(existing) = st.workers.get(&worker_id) {
            if existing.token != token {
                return Resp::err(format!("worker_id {} already registered by another token", worker_id));
            }
            let refreshed = WorkerRec {
                id: worker_id.clone(),
                token: existing.token.clone(),
                pane: pane.or_else(|| existing.pane.clone()),
                cwd,
                registered_ms: existing.registered_ms,
            };
            drop(st);
            server.commit(&[Event::Registered { worker: refreshed }]);
            return Resp::data(json!({"worker_id": worker_id, "reused": true}));
        }
    }
    let rec = WorkerRec { id: worker_id.clone(), token, pane, cwd, registered_ms: now_ms() };
    server.commit(&[Event::Registered { worker: rec.clone() }]);
    Resp::data(json!({"worker_id": worker_id, "registered_at": iso(rec.registered_ms)}))
}

fn handle_send(
    server: &Server,
    from: String,
    to: String,
    mtype: String,
    body: String,
    in_reply_to: Option<String>,
) -> Resp {
    if !MSG_TYPES.contains(&mtype.as_str()) {
        return Resp::err(format!("invalid type {}; must be one of {:?}", mtype, MSG_TYPES));
    }
    let st = server.state.lock().unwrap();
    if !st.workers.contains_key(&to) {
        return Resp::err(format!("recipient {} not registered", to));
    }
    if let Some(ref rid) = in_reply_to {
        if !st.msgs.contains_key(rid) {
            return Resp::err(format!("in_reply_to message {} not found", rid));
        }
    }
    let pane = st.worker_pane(&to);
    let msg = Message {
        id: gen_msg_id(),
        from: from.clone(),
        to,
        mtype: mtype.clone(),
        body,
        in_reply_to,
        created_ms: now_ms(),
        state: "pending".into(),
        nudge_count: 0,
        last_nudge_ms: 0,
    };
    let mid = msg.id.clone();
    drop(st);
    server.commit(&[Event::Sent { msg }]);
    if let Some(p) = pane {
        knock_or_log(&server.log_path(), &p, &knock_text(&from, &mtype, &mid));
    }
    Resp::data(json!({"msg_id": mid}))
}

fn handle_poll(server: &Server, worker_id: String, timeout_ms: u64) -> Resp {
    let timeout_ms = timeout_ms.min(MAX_POLL_MS);
    let deadline = Instant::now() + Duration::from_millis(timeout_ms);
    loop {
        let ids: Vec<String>;
        let msgs: Vec<Message>;
        {
            let st = server.state.lock().unwrap();
            let unread = st.inbox_of(&worker_id);
            if unread.is_empty() {
                ids = Vec::new();
                msgs = Vec::new();
            } else {
                ids = unread.iter().map(|m| m.id.clone()).collect();
                msgs = unread.into_iter().cloned().collect();
            }
        }
        if !ids.is_empty() {
            server.commit(&[Event::Delivered { ids }]);
            return Resp::data(json!({
                "messages": msgs,
                "count": msgs.len(),
                "fetched_at": iso(now_ms()),
            }));
        }
        if Instant::now() >= deadline {
            return Resp::data(json!({"messages": [], "count": 0, "timeout": true}));
        }
        std::thread::sleep(Duration::from_millis(POLL_TICK_MS));
    }
}

fn handle_claim_acquire(
    server: &Server,
    worker_id: String,
    claim_id: String,
    intent: Option<String>,
    lease_ms: Option<u64>,
    force: bool,
) -> Resp {
    const LEASE: i64 = DEFAULT_LEASE_MS;
    let now = now_ms();
    let lease_until = now + lease_ms.map(|m| m as i64).unwrap_or(LEASE);

    let snap: Option<ClaimRec> = server.state.lock().unwrap().claims.get(&claim_id).cloned();

    let mut evs: Vec<Event> = Vec::new();
    let mut knocks: Vec<(String, String)> = Vec::new();

    let result = match snap {
        None => {
            evs.push(Event::ClaimAcquired { id: claim_id.clone(), owner: worker_id.clone(), intent, lease_until_ms: lease_until, at_ms: now });
            Ok(json!({"claim": claim_id, "status": "acquired"}))
        }
        Some(c) => {
            if c.owner.as_deref() == Some(worker_id.as_str()) {
                Ok(json!({"claim": claim_id, "status": "already-owner", "lease_until": iso(c.lease_until_ms)}))
            } else if c.owner.is_none() {
                match c.reserved_for.as_deref() {
                    Some(r) if r != worker_id => Ok(json!({
                        "claim": claim_id, "status": "reserved",
                        "reserved_for": r,
                        "hint": "FIFO reservation held by another worker"
                    })),
                    _ => {
                        evs.push(Event::ClaimAcquired { id: claim_id.clone(), owner: worker_id.clone(), intent, lease_until_ms: lease_until, at_ms: now });
                        Ok(json!({"claim": claim_id, "status": "acquired"}))
                    }
                }
            } else if c.lease_expired(now) {
                if force {
                    let old = c.owner.clone().unwrap();
                    evs.push(Event::ClaimAcquired { id: claim_id.clone(), owner: worker_id.clone(), intent: intent.clone(), lease_until_ms: lease_until, at_ms: now });
                    let body = format!(
                        "TAKEOVER: your claim '{}' was taken over by {} (lease expired since {})",
                        claim_id, worker_id, iso(c.lease_until_ms)
                    );
                    let pane = server.state.lock().unwrap().worker_pane(&old);
                    let sid = gen_msg_id();
                    evs.push(Event::Sent { msg: Message {
                        id: sid.clone(), from: "collab-server".into(), to: old.clone(),
                        mtype: "system".into(), body, in_reply_to: None,
                        created_ms: now, state: "pending".into(), nudge_count: 0, last_nudge_ms: 0,
                    }});
                    if let Some(p) = pane {
                        knocks.push((p, knock_text("collab-server", "system", &sid)));
                    }
                    Ok(json!({"claim": claim_id, "status": "takeover", "previous_owner": old}))
                } else {
                    Ok(json!({
                        "claim": claim_id, "status": "expired",
                        "owner": c.owner, "lease_until": iso(c.lease_until_ms),
                        "hint": "lease expired; re-run with --force to take over"
                    }))
                }
            } else {
                // healthy contention: FIFO enqueue + report position
                let already = c.queue.iter().any(|q| q.worker_id == worker_id);
                if !already {
                    evs.push(Event::ClaimQueued { id: claim_id.clone(), worker_id: worker_id.clone() });
                }
                let pos = c.queue.iter().position(|q| q.worker_id == worker_id).map(|i| i + 1)
                    .unwrap_or(c.queue.len() + 1);
                Ok(json!({
                    "claim": claim_id, "status": "queued",
                    "owner": c.owner, "lease_until": iso(c.lease_until_ms),
                    "queue_position": pos,
                    "hint": "use `collab claim wait` to block until release"
                }))
            }
        }
    };

    if !evs.is_empty() {
        server.commit(&evs);
        for (pane, text) in knocks {
            knock_or_log(&server.log_path(), &pane, &text);
        }
    }
    match result {
        Ok(v) => Resp::data(v),
        Err(r) => r,
    }
}

fn handle_claim_release(server: &Server, worker_id: String, claim_id: String) -> Resp {
    let snap: Option<ClaimRec> = server.state.lock().unwrap().claims.get(&claim_id).cloned();
    let Some(c) = snap else {
        return Resp::err(format!("claim {} not found", claim_id));
    };
    if c.owner.as_deref() != Some(worker_id.as_str()) {
        return Resp::err(format!("claim {} is not owned by {}", claim_id, worker_id));
    }
    let reserved_for = c.queue.first().map(|q| q.worker_id.clone());
    server.commit(&[Event::ClaimReleased { id: claim_id.clone(), by: worker_id, reserved_for: reserved_for.clone() }]);

    // wake the next waiter with a direct notice
    if let Some(head) = reserved_for {
        let mut evs = Vec::new();
        let mut knocks = Vec::new();
        let pane = server.state.lock().unwrap().worker_pane(&head);
        let sid = gen_msg_id();
        evs.push(Event::Sent { msg: Message {
            id: sid.clone(), from: "collab-server".into(), to: head.clone(),
            mtype: "system".into(),
            body: format!("RELEASED: claim '{}' is free; you are first in queue — acquire it now", claim_id),
            in_reply_to: None, created_ms: now_ms(), state: "pending".into(), nudge_count: 0, last_nudge_ms: 0,
        }});
        if let Some(p) = pane {
            knocks.push((p, knock_text("collab-server", "system", &sid)));
        }
        server.commit(&evs);
        for (pane, text) in knocks {
            knock_or_log(&server.log_path(), &pane, &text);
        }
    }
    Resp::data(json!({"claim": claim_id, "status": "released"}))
}

fn handle_claim_renew(server: &Server, worker_id: String, claim_id: String, lease_ms: Option<u64>) -> Resp {
    let snap: Option<ClaimRec> = server.state.lock().unwrap().claims.get(&claim_id).cloned();
    let Some(c) = snap else {
        return Resp::err(format!("claim {} not found", claim_id));
    };
    if c.owner.as_deref() != Some(worker_id.as_str()) {
        return Resp::err(format!("claim {} is not owned by {}", claim_id, worker_id));
    }
    let until = now_ms() + lease_ms.map(|m| m as i64).unwrap_or(DEFAULT_LEASE_MS);
    server.commit(&[Event::ClaimRenewed { id: claim_id.clone(), lease_until_ms: until }]);
    Resp::data(json!({"claim": claim_id, "status": "renewed", "lease_until": iso(until)}))
}

fn handle_claim_wait(server: &Arc<Server>, worker_id: String, claim_id: String, timeout_ms: u64) -> Resp {
    let timeout_ms = timeout_ms.min(MAX_WAIT_MS);

    // register interest in the FIFO queue while waiting on a live claim
    {
        let st = server.state.lock().unwrap();
        if let Some(c) = st.claims.get(&claim_id) {
            if c.owner.is_some()
                && c.owner.as_deref() != Some(worker_id.as_str())
                && !c.queue.iter().any(|q| q.worker_id == worker_id)
            {
                drop(st);
                server.commit(&[Event::ClaimQueued { id: claim_id.clone(), worker_id: worker_id.clone() }]);
            }
        }
    }

    let deadline = Instant::now() + Duration::from_millis(timeout_ms);
    loop {
        {
            let st = server.state.lock().unwrap();
            match st.claims.get(&claim_id) {
                None => return Resp::data(json!({"claim": claim_id, "wait_status": "free"})),
                Some(c) => {
                    if c.owner.as_deref() == Some(worker_id.as_str()) {
                        return Resp::data(json!({"claim": claim_id, "wait_status": "owner"}));
                    }
                    if c.owner.is_none() && c.reserved_for.as_deref() == Some(worker_id.as_str()) {
                        return Resp::data(json!({"claim": claim_id, "wait_status": "yours-to-take"}));
                    }
                    if c.owner.is_none() {
                        return Resp::data(json!({"claim": claim_id, "wait_status": "free-unreserved"}));
                    }
                    if c.expired_notified {
                        return Resp::data(json!({
                            "claim": claim_id, "wait_status": "expired",
                            "owner": c.owner, "hint": "holder lease expired; --force takeover possible"
                        }));
                    }
                }
            }
        }
        if Instant::now() >= deadline {
            return Resp::data(json!({"claim": claim_id, "wait_status": "timeout"}));
        }
        std::thread::sleep(Duration::from_millis(POLL_TICK_MS));
    }
}

// ---------- dispatch ----------

fn dispatch(server: &Arc<Server>, req: Req) -> Resp {
    match req {
        Req::Register { worker_id, token, pane, cwd } => handle_register(server, worker_id, token, pane, cwd),
        Req::Send { from, to, mtype, body, in_reply_to } => handle_send(server, from, to, mtype, body, in_reply_to),
        Req::Poll { worker_id, token, timeout_ms } => {
            let check = server.state.lock().unwrap();
            if let Err(e) = verify(&check, &worker_id, &token) {
                return e;
            }
            drop(check);
            handle_poll(server, worker_id, timeout_ms)
        }
        Req::Ack { worker_id, token, ids } => {
            let st = server.state.lock().unwrap();
            if let Err(e) = verify(&st, &worker_id, &token) {
                return e;
            }
            let owned: Vec<String> = ids.into_iter()
                .filter(|id| st.msgs.get(id).map(|m| m.to == worker_id).unwrap_or(false))
                .collect();
            drop(st);
            if owned.is_empty() {
                return Resp::err("no ackable messages (must address your own inbox)");
            }
            server.commit(&[Event::Acked { ids: owned.clone() }]);
            Resp::data(json!({"acked": owned}))
        }
        Req::Inbox { worker_id, token } => {
            let st = server.state.lock().unwrap();
            if let Err(e) = verify(&st, &worker_id, &token) {
                return e;
            }
            let inbox: Vec<&Message> = st.inbox_of(&worker_id);
            let items: Vec<serde_json::Value> = inbox.iter().map(|m| json!({
                "id": m.id, "from": m.from, "type": m.mtype,
                "state": m.state, "created_at": iso(m.created_ms),
                "body": m.body,
            })).collect();
            Resp::data(json!({"unread": items.len(), "messages": items}))
        }
        Req::MsgStatus { msg_id } => {
            let st = server.state.lock().unwrap();
            match st.msgs.get(&msg_id) {
                Some(m) => Resp::data(json!({
                    "id": m.id, "from": m.from, "to": m.to, "type": m.mtype,
                    "state": m.state, "nudges": m.nudge_count,
                    "created_at": iso(m.created_ms), "answered": st.answered(&msg_id),
                })),
                None => Resp::err(format!("message {} not found", msg_id)),
            }
        }
        Req::ClaimAcquire { worker_id, claim_id, intent, lease_ms, force } =>
            handle_claim_acquire(server, worker_id, claim_id, intent, lease_ms, force),
        Req::ClaimRelease { worker_id, claim_id } => handle_claim_release(server, worker_id, claim_id),
        Req::ClaimRenew { worker_id, claim_id, lease_ms } => handle_claim_renew(server, worker_id, claim_id, lease_ms),
        Req::ClaimStatus { claim_id } => {
            let st = server.state.lock().unwrap();
            let view = |c: &ClaimRec| json!({
                "id": c.id, "owner": c.owner, "intent": c.intent,
                "lease_until": iso(c.lease_until_ms),
                "expired": c.lease_expired(now_ms()),
                "reserved_for": c.reserved_for,
                "queue": c.queue.iter().map(|q| q.worker_id.clone()).collect::<Vec<_>>(),
            });
            match claim_id {
                Some(id) => match st.claims.get(&id) {
                    Some(c) => Resp::data(view(c)),
                    None => Resp::err(format!("claim {} not found", id)),
                },
                None => {
                    let all: Vec<serde_json::Value> = st.claims.values().map(|c| view(c)).collect();
                    Resp::data(json!({"claims": all}))
                }
            }
        }
        Req::ClaimWait { worker_id, claim_id, timeout_ms } => handle_claim_wait(server, worker_id, claim_id, timeout_ms),
        Req::Ping => {
            let st = server.state.lock().unwrap();
            Resp::data(json!({
                "workers": st.workers.len(),
                "messages": st.msgs.len(),
                "claims": st.claims.len(),
                "now": iso(now_ms()),
            }))
        }
    }
}

async fn conn_task(server: Arc<Server>, stream: tokio::net::UnixStream) {
    use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
    let (reader, mut writer) = stream.into_split();
    let mut lines = BufReader::new(reader).lines();
    while let Ok(Some(line)) = lines.next_line().await {
        if line.trim().is_empty() {
            continue;
        }
        let resp = match serde_json::from_str::<Req>(&line) {
            Ok(req) => {
                // blocking handlers (poll/wait) run off the async reactor thread pool
                let srv = server.clone();
                tokio::task::spawn_blocking(move || dispatch(&srv, req))
                    .await
                    .unwrap_or_else(|e| Resp::err(format!("handler join error: {}", e)))
            }
            Err(e) => Resp::err(format!("bad request: {}", e)),
        };
        let mut out = serde_json::to_string(&resp).expect("serialize resp");
        out.push('\n');
        if writer.write_all(out.as_bytes()).await.is_err() {
            break;
        }
    }
}

fn replay(root: &Path) -> anyhow::Result<State> {
    let journal = root.join(".agent-collab/server/journal.jsonl");
    let mut st = State::default();
    if !journal.exists() {
        return Ok(st);
    }
    let content = std::fs::read_to_string(journal)?;
    for line in content.lines() {
        if line.trim().is_empty() {
            continue;
        }
        match serde_json::from_str::<Event>(line) {
            Ok(ev) => st.apply(&ev),
            Err(e) => append_log(&root.join(".agent-collab/server/log.txt"), &format!("journal replay skip: {}", e)),
        }
    }
    Ok(st)
}

pub async fn run(scope: Scope) -> anyhow::Result<()> {
    let sock_path = scope.sock_path();
    let server_dir = scope.server_dir();
    std::fs::create_dir_all(&server_dir)?;

    if sock_path.exists() {
        if crate::client::alive(&sock_path) {
            anyhow::bail!("server already running at {}", sock_path.display());
        }
        std::fs::remove_file(&sock_path)?;
    }

    let journal_file = std::fs::OpenOptions::new()
        .create(true).append(true)
        .open(server_dir.join("journal.jsonl"))?;

    let state = replay(&scope.root)?;
    append_log(&server_dir.join("log.txt"), "server starting");

    let server = Arc::new(Server {
        root: scope.root.clone(),
        state: Mutex::new(state),
        journal: Mutex::new(journal_file),
    });

    let listener = UnixListener::bind(&sock_path)?;

    // background scheduler: leases + nudges
    let sched = server.clone();
    tokio::spawn(async move {
        let mut interval = tokio::time::interval(Duration::from_secs(5));
        loop {
            interval.tick().await;
            crate::server::timers::tick(&sched);
        }
    });

    loop {
        match listener.accept().await {
            Ok((stream, _)) => {
                let srv = server.clone();
                tokio::spawn(conn_task(srv, stream));
            }
            Err(e) => append_log(&server_dir.join("log.txt"), &format!("accept error: {}", e)),
        }
    }
}
