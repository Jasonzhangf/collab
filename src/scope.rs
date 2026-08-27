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
        "handoff",
        "merge-queue",
        "panes",
        "mailbox",
        "messages",
        "mailboxes",
        "server",
    ] {
        std::fs::create_dir_all(base.join(sub))?;
    }
    let docs = root.join("docs");
    std::fs::create_dir_all(&docs)?;
    let collab_doc = docs.join("collab.md");
    if !collab_doc.exists() {
        std::fs::write(&collab_doc, COLLAB_DOC)?;
    }
    Ok(base)
}

pub const COLLAB_DOC: &str = r#"# collab workflow

This project uses the local `collab` daemon for multi-agent coordination.
The binary lives in `~/code/collab`; the installed command is
`~/.cargo/bin/collab`.

## Runtime boundary

- Every registration must come from a live tmux pane or a live Herdr pane.
- The first registered pane fixes the project runtime (`tmux` or `herdr`).
- Later workers must use the same runtime; mixed tmux/Herdr projects are
  rejected, as are messages across runtimes.
- The runtime is part of the worker identity boundary, not a task preference.

The current master may migrate ownership before a restart:
`collab transfer-master <worker-id>`. To remove an old registration, use
`collab remove-worker <worker-id>`; active task owners must deliver or release
their task first. The master cannot remove itself.

## Roles

- First registered pane becomes `master`; every later pane becomes `worker`.
- Master creates tasks, reviews deliveries, merges, closes tasks, and cleans
  declared worktrees after merge.
- Workers claim `available` tasks and work independently. They do not request
  claim approval and do not register tasks.
- Check identity with `collab role`, `collab who`, or `collab master`.
- If a message names a different role or owner, confirm identity first and
  return the owner contact; do not act outside your role.

## Task lifecycle

```
available -> working -> verifying -> reviewed -> delivered
          -> master merge -> close/cleanup -> closed
          -> rework -> working
```

Task records use a fixed shape:
`id / owner / feature_id / worktree_path / branch / base_commit / priority /
status`. Valid statuses are `available`, `working`, `verifying`, `reviewed`,
`delivered`, `rework`, `merged`, `closed`, and `cancelled`.

## Common commands

```sh
collab config                     # show .agent-collab/collab.json
collab config --heartbeat-minutes 45
collab who                        # workers + active task status
collab task status [task-id]      # board or one task
collab task register <id> --feature <feature-id> --worktree <path> \
  --branch <branch> --base-commit <sha> --priority p2
collab task claim <id>            # worker self-service
collab task deliver <id> --evidence "commit=<sha>; gates=pass"
collab task update <id> --status merged
collab task close <id>            # master; verifies merged/clean then cleans
```

Master owns the fixed task contract and the project board. After merging a
delivered worker branch, run `collab task close <id>` to verify the merge,
remove the clean declared `./playground/` worktree, remove the merged branch,
and dispatch registered available tasks to idle workers. Then register the
next decomposed tasks from the new main commit and run `collab task dispatch`.
Workers never share worktrees: claim one task, work in its declared clean
worktree, test, commit, and deliver; after delivery they inspect the returned
available board and claim the next independent task without approval. A
worker remains registered after task closure.

## Message handling

On `[MAIL]`, read the body or `body-ref` first, confirm identity and task
ownership, decide collaborate/defer/reject/continue, reply only when there is
meaningful evidence or a required ownership redirect, then acknowledge and
resume the current task. A notify does not require a reply. A request requires
one substantive reply. A reply from a peer is work input, not a stop signal.

`collab inbox` and `collab msg <id>` query the durable local mailbox after a
tmux pane disappears; mailbox state remains authoritative.

## Heartbeat and dispatch

Only workers with an active claim receive heartbeats. `collab who` exposes
`active_task` and `active_status` for every worker, so master can dispatch to
idle workers without messaging busy ones.

`.agent-collab/collab.json` configures the heartbeat interval. The daemon
reloads it without a restart; invalid values fail closed to the default. Only
workers with an active claim receive heartbeat prompts. When working, ignore a
heartbeat and continue. When intentionally waiting at a safe breakpoint, use
`collab recv --timeout 300`; on timeout inspect the task next step and continue
without waiting. The tmux heartbeat uses literal text, waits two seconds, then
sends an explicit Enter.
"#;

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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn init_releases_collab_doc_only_once() {
        let root = std::env::temp_dir().join(format!(
            "collab-scope-init-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        init(&root).unwrap();
        let path = root.join("docs/collab.md");
        assert!(path.exists());
        let first = std::fs::read_to_string(&path).unwrap();
        assert!(first.contains("# collab workflow"));

        init(&root).unwrap();
        let second = std::fs::read_to_string(&path).unwrap();
        assert_eq!(first, second);
        std::fs::remove_dir_all(root).ok();
    }
}
