use crate::scope::Scope;
use rand::Rng;
use serde::{Deserialize, Serialize};
use std::path::PathBuf;
use std::process::Command;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Identity {
    pub worker_id: String,
    pub token: String,
    pub pane: Option<String>,
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

fn herdr_terminal_id(socket: &str, pane: &str) -> anyhow::Result<String> {
    let output = Command::new("herdr")
        .env("HERDR_SOCKET_PATH", socket)
        .args(["pane", "get", pane])
        .output()?;
    if !output.status.success() {
        anyhow::bail!(
            "cannot query Herdr pane {} for a unique terminal identity",
            pane
        );
    }
    let value: serde_json::Value = serde_json::from_slice(&output.stdout)?;
    parse_terminal_id(&value)
}

fn parse_terminal_id(value: &serde_json::Value) -> anyhow::Result<String> {
    value
        .pointer("/result/pane/terminal_id")
        .and_then(|v| v.as_str())
        .filter(|v| !v.is_empty())
        .map(str::to_owned)
        .ok_or_else(|| anyhow::anyhow!("Herdr pane response has no terminal_id"))
}

/// Load existing identity or create one. Identity is keyed to the tmux pane
/// when available (same pane = same worker across invocations), otherwise to
/// the given worker_id; without either, a fresh identity is created each time.
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
            std::env::var("TMUX_PANE").ok()
        }
    });
    let herdr_terminal = pane
        .as_deref()
        .and_then(|p| p.strip_prefix("herdr:").and_then(|v| v.rsplit_once('|')))
        .map(|(socket, pane_id)| herdr_terminal_id(socket, pane_id))
        .transpose()?;

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
                let key = if let Some(terminal_id) = &herdr_terminal {
                    format!("{}-{}", pane_file_name(p), pane_file_name(terminal_id))
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
        if let Some(p) = pane_override.as_ref() {
            return Ok(Identity {
                pane: Some(p.clone()),
                ..ident
            });
        }
        return Ok(ident);
    }

    let id = if id.is_empty() {
        // pane-keyed identity: derive worker_id from pane for traceability
        let p = pane.as_deref().unwrap_or("unknown");
        if let Some(terminal_id) = &herdr_terminal {
            format!("pane-herdr-{}", pane_file_name(terminal_id))
        } else {
            format!("pane-{}", p.trim_start_matches('%'))
        }
    } else {
        id
    };
    let ident = Identity {
        worker_id: id,
        token: hex(16),
        pane,
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
    use super::parse_terminal_id;

    #[test]
    fn parses_herdr_terminal_identity() {
        let value = serde_json::json!({"result":{"pane":{"terminal_id":"term_123"}}});
        assert_eq!(parse_terminal_id(&value).unwrap(), "term_123");
    }

    #[test]
    fn rejects_missing_herdr_terminal_identity() {
        let value = serde_json::json!({"result":{"pane":{}}});
        assert!(parse_terminal_id(&value).is_err());
    }
}
