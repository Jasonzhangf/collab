use std::path::{Path, PathBuf};

/// Resolve project root by walking up from `start` to find `.agent-collab`.
/// Returns None when absent — commands other than `init` refuse to guess.
pub fn find_root(start: &Path) -> Option<PathBuf> {
    let mut cur = Some(start.to_path_buf());
    while let Some(dir) = cur {
        if dir.join(".agent-collab").is_dir() {
            return Some(dir);
        }
        cur = dir.parent().map(|p| p.to_path_buf());
    }
    None
}

pub fn init(root: &Path) -> std::io::Result<PathBuf> {
    let base = root.join(".agent-collab");
    for sub in [
        "runs",
        "claims",
        "handoff",
        "merge-queue",
        "panes",
        "mailboxes",
        "server",
    ] {
        std::fs::create_dir_all(base.join(sub))?;
    }
    Ok(base)
}

/// Scope guard used by every command except init.
pub struct Scope {
    pub root: PathBuf,
}

impl Scope {
    pub fn resolve() -> anyhow::Result<Self> {
        let cwd = std::env::current_dir()?;
        match find_root(&cwd) {
            Some(root) => Ok(Scope { root }),
            None => Err(anyhow::anyhow!(
                "no .agent-collab found between {} and filesystem root; run `collab init` in the project root first",
                cwd.display()
            )),
        }
    }
    pub fn server_dir(&self) -> PathBuf {
        self.root.join(".agent-collab").join("server")
    }
    pub fn sock_path(&self) -> PathBuf {
        self.server_dir().join("server.sock")
    }
}
