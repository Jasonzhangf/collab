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

/// Load or create one tmux-session identity. Commands outside tmux fail before
/// writing a run identity, so diagnostics cannot accidentally declare a peer.
pub fn load_or_create(
    scope: &Scope,
    worker_id: Option<String>,
    pane_override: Option<String>,
) -> anyhow::Result<Identity> {
    let pane = pane_override
        .or_else(|| std::env::var("TMUX_PANE").ok())
        .filter(|pane| pane.starts_with('%'))
        .ok_or_else(|| anyhow::anyhow!("collab identity requires a live tmux pane"))?;
    let session = tmux_session(&pane)
        .ok_or_else(|| anyhow::anyhow!("cannot resolve tmux session for pane {}", pane))?;
    let (id, path) = if let Some(worker_id) = worker_id {
        let path = identity_path(scope, &worker_id);
        (worker_id, path)
    } else {
        let dir = scope
            .root
            .join(".agent-collab")
            .join("runs")
            .join("by-pane");
        std::fs::create_dir_all(&dir)?;
        let path = dir.join(format!("tmux-{}.json", pane_file_name(&session)));
        (session.clone(), path)
    };

    if path.exists() {
        let ident: Identity = serde_json::from_str(&std::fs::read_to_string(&path)?)?;
        return Ok(Identity {
            pane: Some(pane),
            session: Some(session),
            ..ident
        });
    }

    let ident = Identity {
        worker_id: id,
        token: hex(16),
        pane: Some(pane),
        session: Some(session),
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
    use super::*;

    #[test]
    fn non_tmux_diagnostic_cannot_declare_identity() {
        let root = std::env::temp_dir().join(format!(
            "collab-identity-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(root.join(".agent-collab/runs")).unwrap();
        let scope = Scope { root: root.clone() };
        let result = load_or_create(&scope, None, Some("not-tmux".into()));
        assert!(result.is_err());
        assert_eq!(
            std::fs::read_dir(root.join(".agent-collab/runs"))
                .unwrap()
                .count(),
            0
        );
        std::fs::remove_dir_all(root).ok();
    }
}
