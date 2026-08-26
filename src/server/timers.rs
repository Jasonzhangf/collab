use crate::server::state::{now_ms, Event, Message};
use crate::server::Server;
use std::sync::Arc;

const NUDGE_INTERVAL_MS: i64 = 5 * 60 * 1000;
const MAX_NUDGES: u32 = 3;

/// Server-side watchdog. Runs every 5s; emits journal events only, all
/// mutations go through the same commit path as client ops.
pub fn tick(server: &Arc<Server>) {
    let now = now_ms();
    let mut evs: Vec<Event> = Vec::new();
    let mut knocks: Vec<(String, String)> = Vec::new();

    {
        let st = server.state.lock().unwrap();

        // 1) lease expiry watch
        for c in st.claims.values() {
            if c.owner.is_some() && c.lease_until_ms < now && !c.expired_notified {
                evs.push(Event::ClaimExpiredNotified { id: c.id.clone() });
                let body = format!(
                    "LEASE EXPIRED: claim '{}' held by {} was not renewed (lease_until {})",
                    c.id,
                    c.owner.clone().unwrap_or_default(),
                    chrono::DateTime::from_timestamp_millis(c.lease_until_ms)
                        .map(|d| d.to_rfc3339()).unwrap_or_default()
                );
                let mut targets: Vec<String> = vec![c.owner.clone().unwrap()];
                targets.extend(c.queue.iter().map(|q| q.worker_id.clone()));
                for t in targets {
                    if let Some(pane) = st.worker_pane(&t) {
                        knocks.push((pane, "[MAIL] lease-expired".into()));
                    }
                    evs.push(sent_system(&t, body.clone()));
                }
            }
        }

        // 2) unanswered request nudges: 5min each, escalate after MAX_NUDGES
        for m in st.msgs.values() {
            if m.mtype != "request" || m.state == "read" || st.answered(&m.id) {
                continue;
            }
            let due = m.nudge_count < MAX_NUDGES
                && now - m.created_ms >= NUDGE_INTERVAL_MS * (m.nudge_count as i64 + 1);
            if !due {
                continue;
            }
            let k = m.nudge_count + 1;
            // The recipient gets exactly one knock per request; later ticks do
            // not repeat the same wake-up while the request remains pending.
            if k == 1 {
                if let Some(pane) = st.worker_pane(&m.to) {
                    knocks.push((pane, knock_line(&m.id, &m.from)));
                }
                evs.push(sent_system(&m.to, nudge_body(&m.id, &m.from)));
            }
            if k >= MAX_NUDGES {
                let sender_body = format!(
                    "ESCALATE: request '{}' to {} has no reply after {} nudges; stop waiting and escalate per protocol",
                    m.id, m.to, MAX_NUDGES
                );
                evs.push(sent_system(&m.from, sender_body));
            }
            evs.push(Event::Nudged { msg_id: m.id.clone() });
        }
    }

    if evs.is_empty() {
        return;
    }
    server.commit(&evs);
    let log = server.log_path();
    for (pane, text) in knocks {
        crate::server::knock::knock_or_log(&log, &pane, &text);
    }
}

fn sent_system(to: &str, body: String) -> Event {
    Event::Sent {
        msg: Message {
            id: super::gen_msg_id(),
            from: "collab-server".into(),
            to: to.into(),
            mtype: "system".into(),
            body,
            in_reply_to: None,
            created_ms: now_ms(),
            state: "pending".into(),
            nudge_count: 0,
            last_nudge_ms: 0,
        },
    }
}

fn knock_line(msg_id: &str, from: &str) -> String {
    format!("[MAIL] NUDGE pending-request id={} from={}", msg_id, from)
}

fn nudge_body(msg_id: &str, from: &str) -> String {
    format!(
        "NUDGE: you have an unanswered request '{}' from {}; respond per protocol (HOLD/YIELD/SPLIT or REPLY)",
        msg_id, from
    )
}
