use crate::scope::Scope;
use rand::Rng;
use serde::{Deserialize, Serialize};
use std::path::PathBuf;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Identity {
    pub worker_id: String,
    pub token: String,
    pub pane: Option<String>,
    #[serde(default)]
    pub session: Option<String>,
}

fn now_compact() -> String {
    chrono::Utc::now().format("%Y%m%dT%H%M%S").to_string()
}

fn host() -> String {
    std::process::Command::new("hostname")
        .output()
        .ok()
        .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_string())
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| "localhost".to_string())
}

pub fn gen_worker_id() -> String {
    let rnd: String = rand::thread_rng()
        .sample_iter(&rand::distributions::Alphanumeric)
        .take(6)
        .map(|c| c as char)
        .collect();
    format!(
        "{}-{}-{}-{}",
        now_compact(),
        host(),
        std::process::id(),
        rnd.to_lowercase()
    )
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

fn herdr_identity(socket: &str, pane: &str) -> String {
    format!(
        "pane-herdr-{}-{}",
        pane_file_name(socket),
        pane_file_name(pane)
    )
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

/// Resolve the current Herdr pane through Herdr's own socket API. Some Herdr
/// launch paths do not export HERDR_* variables into the child agent process.
pub fn current_herdr_pane() -> Option<String> {
    let output = std::process::Command::new("herdr")
        .args(["pane", "current"])
        .output()
        .ok()?;
    if !output.status.success() {
        return None;
    }
    let value: serde_json::Value = serde_json::from_slice(&output.stdout).ok()?;
    let pane = value.get("result")?.get("pane")?;
    let socket = pane
        .get("session_socket")
        .and_then(serde_json::Value::as_str)
        .map(str::to_owned)
        .or_else(|| {
            let status = std::process::Command::new("herdr")
                .args(["status", "server", "--json"])
                .output()
                .ok()?;
            let value: serde_json::Value = serde_json::from_slice(&status.stdout).ok()?;
            Some(value.get("socket")?.as_str()?.to_owned())
        })?;
    let pane_id = pane.get("pane_id")?.as_str()?;
    Some(format!("herdr:{}|{}", socket, pane_id))
}

/// Load existing identity or create one. A tmux session is the stable
/// deployment identity; the pane remains an endpoint only. Without tmux,
/// Herdr keeps its session/pane identity and a supplied worker_id remains
/// authoritative.
pub fn load_or_create(
    scope: &Scope,
    worker_id: Option<String>,
    pane_override: Option<String>,
) -> anyhow::Result<Identity> {
    let pane = pane_override.clone().or_else(|| {
        if std::env::var("HERDR_ENV").ok().as_deref() == Some("1") {
            let pane = std::env::var("HERDR_PANE_ID").ok()?;
            let socket = std::env::var("HERDR_SOCKET_PATH").ok()?;
            Some(format!("herdr:{}|{}", socket, pane))
        } else {
            std::env::var("TMUX_PANE").ok().or_else(current_herdr_pane)
        }
    });
    let herdr_session = pane
        .as_deref()
        .and_then(|p| p.strip_prefix("herdr:").and_then(|v| v.rsplit_once('|')))
        .map(|(socket, pane_id)| (socket.to_owned(), pane_id.to_owned()));
    let tmux_session = pane.as_deref().and_then(|p| {
        if p.starts_with('%') {
            tmux_session(p)
        } else {
            None
        }
    });

    let (id, path) = match worker_id {
        Some(w) => {
            let path = identity_path(scope, &w);
            (w, path)
        }
        None => match &pane {
            Some(p) => {
                let dir = scope
                    .root
                    .join(".agent-collab")
                    .join("runs")
                    .join("by-pane");
                std::fs::create_dir_all(&dir)?;
                let key = if let Some(session) = &tmux_session {
                    format!("tmux-{}", pane_file_name(session))
                } else if let Some((socket, pane_id)) = &herdr_session {
                    format!("{}-{}", pane_file_name(socket), pane_file_name(pane_id))
                } else {
                    pane_file_name(p)
                };
                let file = dir.join(format!("{}.json", key));
                (String::new(), file)
            }
            None => {
                let w = gen_worker_id();
                let path = identity_path(scope, &w);
                (w, path)
            }
        },
    };

    if path.exists() {
        let ident: Identity = serde_json::from_str(&std::fs::read_to_string(&path)?)?;
        if pane.is_some() {
            return Ok(Identity {
                pane,
                session: tmux_session.or(ident.session),
                ..ident
            });
        }
        return Ok(ident);
    }

    let id = if id.is_empty() {
        // tmux session is the deployment identity; panes are wake endpoints.
        if let Some(session) = &tmux_session {
            session.clone()
        } else if let Some((socket, pane_id)) = &herdr_session {
            herdr_identity(socket, pane_id)
        } else {
            let p = pane.as_deref().unwrap_or("unknown");
            format!("pane-{}", p.trim_start_matches('%'))
        }
    } else {
        id
    };
    let ident = Identity {
        worker_id: id,
        token: hex(16),
        pane,
        session: tmux_session,
    };
    let dir = path.parent().unwrap();
    std::fs::create_dir_all(dir)?;
    // atomic-ish write: temp + rename
    let tmp = dir.join("identity.json.tmp");
    std::fs::write(&tmp, serde_json::to_string_pretty(&ident)?)?;
    std::fs::rename(&tmp, &path)?;
    Ok(ident)
}

#[cfg(test)]
mod tests {
    use super::herdr_identity;

    #[test]
    fn same_herdr_session_and_pane_reuse_identity() {
        assert_eq!(
            herdr_identity("/tmp/one.sock", "w1:p1"),
            herdr_identity("/tmp/one.sock", "w1:p1")
        );
    }

    #[test]
    fn different_herdr_sessions_do_not_collide_on_reused_pane() {
        assert_ne!(
            herdr_identity("/tmp/one.sock", "w1:p1"),
            herdr_identity("/tmp/two.sock", "w1:p1")
        );
    }
}
