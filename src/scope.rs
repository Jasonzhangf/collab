use std::path::{Path, PathBuf};
use std::process::Command;

fn validate_project_root(root: PathBuf) -> anyhow::Result<PathBuf> {
    if !root.is_absolute() || !root.is_dir() {
        anyhow::bail!(
            "project root must be an existing absolute directory: {}",
            root.display()
        );
    }
    Ok(root)
}

fn project_root_from<F>(pane: Option<&str>, cwd: PathBuf, pane_cwd: F) -> anyhow::Result<PathBuf>
where
    F: FnOnce(&str) -> anyhow::Result<PathBuf>,
{
    match pane {
        Some(pane) => {
            if !pane.starts_with('%') {
                anyhow::bail!("invalid TMUX_PANE value: {pane}");
            }
            validate_project_root(pane_cwd(pane)?)
        }
        None => validate_project_root(cwd),
    }
}

/// The launching environment owns project scope. A tmux Agent is bound to the
/// exact current directory of its pane; a non-tmux operator is bound to the
/// exact process cwd. No caller may select a path and no ancestor is searched.
pub fn project_root() -> anyhow::Result<PathBuf> {
    let pane = std::env::var("TMUX_PANE").ok();
    let cwd = std::env::current_dir()?;
    project_root_from(pane.as_deref(), cwd, |pane| {
        let output = Command::new("tmux")
            .args(["display-message", "-p", "-t", pane, "#{pane_current_path}"])
            .output()?;
        if !output.status.success() {
            anyhow::bail!("cannot resolve project root for tmux pane {pane}");
        }
        let path = String::from_utf8(output.stdout)?;
        let path = path.trim();
        if path.is_empty() {
            anyhow::bail!("tmux pane {pane} returned an empty project root");
        }
        Ok(PathBuf::from(path))
    })
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

The daemon is detached. Normal commands may start it when no explicit `DOWN`
marker exists. `collab init` creates the local
`.agent-collab/server` skeleton, so old projects need no manual repair. Use
`collab down` only for an explicit stop; use `collab up` to clear that stop and
start it again. Never start a second daemon.
Existing projects migrate through `collab migrate inspect`, `plan`, `apply`,
controlled daemon upgrade/restart, identity rebind, and `verify`;
deleting `.agent-collab`, editing JSON state, clearing mailboxes, copying
tokens, mixed runtime writes, and guessing pane identity are deprecated.

## Runtime boundary

- Every peer registration must come from a live tmux pane.
- Registration owns one deterministic seven-day default direct-message lease;
  daemon restart restores it only while the registered tmux session still
  matches the peer identity. A shorter explicit lease cannot suppress it.
- tmux is the only live notification channel and carries one bounded preview.
- Server state, journal, and mailbox are durable truth; a failed wake cannot
  roll back state or fabricate success.
- The runtime is part of the worker identity boundary, not a task preference.

## Roles

- Every registered identity is an equal `peer`; there is no permanent master.
- Each peer self-registers one task and owns its full worktree, test,
  integration, main verification, push, cleanup, and resource lifecycle.
- Task owner, resource holder, integration lease, and daemon operator are
  scoped capabilities, never durable identity roles.
- Peers send no normal progress reports. P2P communication is limited to
  durable resource occupancy and release coordination.

## Task lifecycle

```
working -> verifying -> reviewed -> delivered
        -> owner sync/verify/integrate -> merged -> cleanup_pending
        -> cleanup_verified -> closed
        -> rework -> working
blocked -> bounded waiting -> resource release/timeout -> owner recheck
```

Task records use a fixed shape:
`id / owner / feature_id / worktree_path / branch / base_commit / priority /
 status`. Normal statuses are `working`, `blocked`, `waiting`, `verifying`,
 `reviewed`, `delivered`, `rework`, `merged`, `closed`, and `cancelled`.

## Common commands

```sh
collab up                         # clear explicit down and start daemon
collab down                       # explicit stop; disables auto-restart
collab who                        # registered peers + local state projection
collab task status [task-id]      # durable task registry
collab notify methods             # discover opt-in notification methods
collab notify subscribe --event direct-message --ttl-seconds 600
collab notify status
collab context                    # read-only authoritative state snapshot
collab task register <id> --feature <feature-id> --worktree <path> \
  --branch <branch> --base-commit <sha> --priority p2
collab task wait <id> --for <blocking-task>
collab task deliver <id> --evidence "commit=<sha>; gates=pass" --worktree <path>
collab task block <id> --next "blocked: <evidence and next condition>"
collab task update <id> --status merged
collab task close <id>            # owner; verifies merged/clean, releases claim
```

Peers never share worktrees. Each task owner starts from latest main in one
declared clean `./playground/` worktree, implements and tests, commits the exact
change set, syncs latest main again, verifies the candidate, acquires a short
integration lease, merges the exact commit to main, verifies and pushes main,
then closes the task to remove only its clean merged worktree/branch and persist
a cleanup receipt. A bound worktree is a mandatory cleanup obligation;
`delivered`/`merged` are not cleanup completion, and a task with a pending or
unproven cleanup cannot become closed or pass audit. Delivery is an owner-local
durable milestone and sends no peer notification. `/goal`
delegation and interactive task recognition are intentionally deferred.

## Message handling

On a notification, use its id and abbreviated subject to weigh urgency against
the current task. Query durable state before acting when the notice is relevant.
`collab sendmessage` requires `--subject` and accepts only explicit coordination
or asynchronous-result notices. Never type peer messages with tmux. After the
receiving Agent registers a finite subscription, the daemon may send one id,
abbreviated subject, safe one-line original body preview, and final submit key
as one tmux command queue. The direct-message lease is reusable until expiry;
resource, deadline, and async-result subscriptions remain one-shot.

`collab inbox` and `collab msg <id>` query the durable local mailbox after a
tmux pane disappears; mailbox state remains authoritative.

## Notifications and waits

There is no periodic continuation. Agent-owned subscriptions are exact-event,
exact-subject, and finite. Direct-message delivery is serialized and reusable
until expiry; other subscriptions are one-shot. No registration, absent,
unknown, working, expired, cancelled, consumed, or exhausted message produces
tmux input. Every wait stores waiter, blocking task owner, reason, deadline,
resume events, and P2P escalation. Timeout changes state without unsolicited
messages; resource release notifies only an exact active subscriber.
"#;

/// Scope guard used by every command except init.
pub struct Scope {
    pub root: PathBuf,
}

impl Scope {
    pub fn resolve() -> anyhow::Result<Self> {
        Self::from_project_root(project_root()?)
    }

    fn from_project_root(root: PathBuf) -> anyhow::Result<Self> {
        if root.join(".agent-collab").is_dir() {
            Ok(Scope { root })
        } else {
            Err(anyhow::anyhow!(
                "no .agent-collab found in exact project root {}; run `collab init` there first",
                root.display()
            ))
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

    fn test_root(name: &str) -> PathBuf {
        std::env::temp_dir().join(format!(
            "collab-scope-{name}-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ))
    }

    #[test]
    fn tmux_pane_cwd_is_the_exact_project_root() {
        let process_cwd = test_root("process-cwd");
        let pane_cwd = test_root("pane-cwd");
        std::fs::create_dir_all(&process_cwd).unwrap();
        std::fs::create_dir_all(&pane_cwd).unwrap();

        let resolved = project_root_from(Some("%7"), process_cwd.clone(), |pane| {
            assert_eq!(pane, "%7");
            Ok(pane_cwd.clone())
        })
        .unwrap();
        assert_eq!(resolved, pane_cwd);

        std::fs::remove_dir_all(process_cwd).ok();
        std::fs::remove_dir_all(resolved).ok();
    }

    #[test]
    fn non_tmux_operator_uses_exact_process_cwd() {
        let cwd = test_root("operator-cwd");
        std::fs::create_dir_all(&cwd).unwrap();
        let resolved = project_root_from(None, cwd.clone(), |_| unreachable!()).unwrap();
        assert_eq!(resolved, cwd);
        std::fs::remove_dir_all(resolved).ok();
    }

    #[test]
    fn exact_root_never_captures_ancestor_or_sibling_state() {
        let parent = test_root("exact-scope");
        let first = parent.join("first");
        let second = parent.join("second");
        init(&parent).unwrap();
        std::fs::create_dir_all(&first).unwrap();
        init(&second).unwrap();

        assert!(Scope::from_project_root(first).is_err());
        assert_eq!(
            Scope::from_project_root(second.clone()).unwrap().root,
            second
        );

        std::fs::remove_dir_all(parent).ok();
    }

    #[test]
    fn invalid_tmux_pane_or_path_fails_closed() {
        let cwd = test_root("invalid-pane");
        std::fs::create_dir_all(&cwd).unwrap();
        assert!(project_root_from(Some("pane-7"), cwd.clone(), |_| Ok(cwd.clone())).is_err());
        assert!(
            project_root_from(Some("%7"), cwd.clone(), |_| { Ok(cwd.join("missing")) }).is_err()
        );
        assert!(project_root_from(Some("%7"), cwd.clone(), |_| {
            anyhow::bail!("tmux lookup failed")
        })
        .is_err());
        std::fs::remove_dir_all(cwd).ok();
    }

    #[test]
    fn init_releases_collab_doc_only_once() {
        let root = test_root("init");
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
