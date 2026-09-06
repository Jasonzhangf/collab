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

fn pane_identity_path(scope: &Scope, pane: &str) -> PathBuf {
    scope
        .root
        .join(".agent-collab")
        .join("panes")
        .join(format!("{}.json", pane_file_name(pane)))
}

fn session_identity_path(scope: &Scope, session: &str) -> PathBuf {
    scope
        .root
        .join(".agent-collab")
        .join("runs")
        .join("by-pane")
        .join(format!("tmux-{}.json", pane_file_name(session)))
}

fn write_identity(path: &std::path::Path, ident: &Identity) -> anyhow::Result<()> {
    let dir = path.parent().unwrap();
    std::fs::create_dir_all(dir)?;
    let tmp = dir.join("identity.json.tmp");
    std::fs::write(&tmp, serde_json::to_string_pretty(ident)?)?;
    std::fs::rename(&tmp, path)?;
    Ok(())
}

fn persist_identity(scope: &Scope, ident: &Identity) -> anyhow::Result<()> {
    write_identity(&identity_path(scope, &ident.worker_id), ident)?;
    if let Some(session) = ident.session.as_deref() {
        write_identity(&session_identity_path(scope, session), ident)?;
    }
    if let Some(pane) = ident.pane.as_deref() {
        write_identity(&pane_identity_path(scope, pane), ident)?;
    }
    Ok(())
}

fn read_identity(path: &std::path::Path) -> anyhow::Result<Option<Identity>> {
    if !path.exists() {
        return Ok(None);
    }
    Ok(Some(serde_json::from_str(&std::fs::read_to_string(path)?)?))
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

/// Parent/daemon binds a live child pane so the agent does not self-register.
pub fn provision(
    scope: &Scope,
    worker_id: &str,
    pane: &str,
    session: &str,
) -> anyhow::Result<Identity> {
    if !pane.starts_with('%') {
        anyhow::bail!("collab identity requires a live tmux pane");
    }
    if worker_id.is_empty() || session.is_empty() {
        anyhow::bail!("collab identity requires a session name");
    }
    let ident = read_identity(&identity_path(scope, worker_id))?.unwrap_or_else(|| Identity {
        worker_id: worker_id.into(),
        token: hex(16),
        pane: Some(pane.into()),
        session: Some(session.into()),
    });
    let ident = Identity {
        pane: Some(pane.into()),
        session: Some(session.into()),
        ..ident
    };
    persist_identity(scope, &ident)?;
    Ok(ident)
}

/// Load or create one tmux-session identity. Commands outside tmux fail before
/// writing a run identity, so diagnostics cannot accidentally declare a peer.
/// A parent-provisioned pane binding is enough; tmux lookup is not required.
pub fn load_or_create(
    scope: &Scope,
    worker_id: Option<String>,
    pane_override: Option<String>,
) -> anyhow::Result<Identity> {
    let pane = pane_override
        .or_else(|| std::env::var("TMUX_PANE").ok())
        .filter(|pane| pane.starts_with('%'))
        .ok_or_else(|| anyhow::anyhow!("collab identity requires a live tmux pane"))?;
    let requested = worker_id.or_else(|| std::env::var("COLLAB_WORKER").ok());
    if let Some(ident) = read_identity(&pane_identity_path(scope, &pane))? {
        return Ok(Identity {
            pane: Some(pane),
            ..ident
        });
    }
    if let Some(worker_id) = requested.clone() {
        if let Some(ident) = read_identity(&identity_path(scope, &worker_id))? {
            return Ok(Identity {
                pane: Some(pane),
                ..ident
            });
        }
    }
    let session = tmux_session(&pane)
        .ok_or_else(|| anyhow::anyhow!("cannot resolve tmux session for pane {}", pane))?;
    if let Some(ident) = read_identity(&session_identity_path(scope, &session))? {
        let ident = Identity {
            pane: Some(pane),
            session: Some(session),
            ..ident
        };
        persist_identity(scope, &ident)?;
        return Ok(ident);
    }
    let ident = Identity {
        worker_id: requested.unwrap_or_else(|| session.clone()),
        token: hex(16),
        pane: Some(pane),
        session: Some(session),
    };
    persist_identity(scope, &ident)?;
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

    #[test]
    fn provisioned_pane_identity_does_not_need_tmux() {
        let root = std::env::temp_dir().join(format!(
            "collab-identity-provision-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(root.join(".agent-collab")).unwrap();
        let scope = Scope { root: root.clone() };
        let provisioned = provision(&scope, "child-peer", "%743", "child-peer").unwrap();
        let loaded = load_or_create(&scope, None, Some("%743".into())).unwrap();
        assert_eq!(loaded.worker_id, "child-peer");
        assert_eq!(loaded.token, provisioned.token);
        assert_eq!(loaded.pane.as_deref(), Some("%743"));
        std::fs::remove_dir_all(root).ok();
    }
}
