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
fn inherited_cwd_if_initialized(cwd: PathBuf) -> anyhow::Result<PathBuf> {
    if cwd.join(".agent-collab").is_dir() {
        validate_project_root(cwd)
    } else {
        anyhow::bail!("no .agent-collab found in inherited cwd {}", cwd.display())
    }
}

pub fn project_root() -> anyhow::Result<PathBuf> {
    let pane = std::env::var("TMUX_PANE").ok();
    let cwd = std::env::current_dir()?;
    match project_root_from(pane.as_deref(), cwd.clone(), |pane| {
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
    }) {
        Ok(root) => Ok(root),
        Err(_) if pane.is_some() => inherited_cwd_if_initialized(cwd),
        Err(error) => Err(error),
    }
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
    ensure_project_collab_mcp(root)?;
    ensure_codex_collab_permissions(root)?;
    ensure_cursor_cli_permissions(root)?;
    ensure_claude_collab_permissions(root)?;
    crate::config::ensure_written()
        .map_err(|error| std::io::Error::new(std::io::ErrorKind::Other, error))?;
    Ok(base)
}

fn collab_mcp_command() -> String {
    std::env::current_exe()
        .ok()
        .and_then(|p| p.parent().map(|d| d.join("collab-mcp")))
        .filter(|p| p.is_file())
        .map(|p| p.to_string_lossy().into_owned())
        .or_else(|| {
            std::env::var_os("PATH").and_then(|path| {
                std::env::split_paths(&path).find_map(|dir| {
                    let candidate = dir.join("collab-mcp");
                    candidate
                        .is_file()
                        .then(|| candidate.to_string_lossy().into_owned())
                })
            })
        })
        .unwrap_or_else(|| "collab-mcp".into())
}

fn ensure_project_collab_mcp(root: &Path) -> std::io::Result<()> {
    merge_collab_mcp(root.join(".cursor").join("mcp.json"))?;
    merge_collab_mcp(root.join(".mcp.json"))
}

fn merge_collab_mcp(path: PathBuf) -> std::io::Result<()> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let mut root_value = if path.exists() {
        serde_json::from_str(&std::fs::read_to_string(&path)?)
            .unwrap_or_else(|_| serde_json::json!({}))
    } else {
        serde_json::json!({})
    };
    let servers = root_value
        .as_object_mut()
        .map(|table| {
            table
                .entry("mcpServers")
                .or_insert_with(|| serde_json::json!({}))
        })
        .and_then(|value| value.as_object_mut());
    let Some(servers) = servers else {
        return Ok(());
    };
    if servers.contains_key("collab") {
        return Ok(());
    }
    servers.insert(
        "collab".into(),
        serde_json::json!({"command": collab_mcp_command()}),
    );
    std::fs::write(
        path,
        format!("{}\n", serde_json::to_string_pretty(&root_value).unwrap()),
    )
}

fn ensure_codex_collab_permissions(root: &Path) -> std::io::Result<()> {
    let path = root.join(".codex").join("config.toml");
    std::fs::create_dir_all(path.parent().unwrap())?;
    let mut table = if path.exists() {
        toml::from_str::<toml::Value>(&std::fs::read_to_string(&path)?)
            .ok()
            .and_then(|value| value.as_table().cloned())
            .unwrap_or_default()
    } else {
        toml::Table::new()
    };
    table.remove("sandbox_mode");
    table.remove("approval_policy");
    let servers = table
        .entry("mcp_servers")
        .or_insert_with(|| toml::Value::Table(toml::Table::new()));
    if let Some(servers) = servers.as_table_mut() {
        if !servers.contains_key("collab") {
            let mut collab = toml::Table::new();
            collab.insert("command".into(), toml::Value::String(collab_mcp_command()));
            servers.insert("collab".into(), toml::Value::Table(collab));
        }
    }
    std::fs::write(
        path,
        format!("{}\n", toml::to_string_pretty(&table).unwrap()),
    )
}

fn merge_allow_patterns(value: &mut serde_json::Value, key: &str, patterns: &[&str]) {
    let list = value
        .as_object_mut()
        .map(|table| table.entry(key).or_insert_with(|| serde_json::json!([])))
        .and_then(|item| item.as_array_mut());
    let Some(list) = list else {
        return;
    };
    for pattern in patterns {
        if !list.iter().any(|item| item.as_str() == Some(*pattern)) {
            list.push(serde_json::json!(pattern));
        }
    }
}

fn ensure_cursor_cli_permissions(root: &Path) -> std::io::Result<()> {
    let path = root.join(".cursor").join("cli.json");
    std::fs::create_dir_all(path.parent().unwrap())?;
    let mut root_value = if path.exists() {
        serde_json::from_str(&std::fs::read_to_string(&path)?)
            .unwrap_or_else(|_| serde_json::json!({}))
    } else {
        serde_json::json!({})
    };
    if let Some(table) = root_value.as_object_mut() {
        table.remove("sandbox");
        table.remove("approvalMode");
        let permissions = table
            .entry("permissions")
            .or_insert_with(|| serde_json::json!({}));
        merge_allow_patterns(
            permissions,
            "allow",
            &[
                "Shell(collab)",
                "Shell(collab *)",
                "Shell(collab-mcp)",
                "Mcp(collab,*)",
            ],
        );
        merge_allow_patterns(permissions, "deny", &[]);
    }
    std::fs::write(
        path,
        format!("{}\n", serde_json::to_string_pretty(&root_value).unwrap()),
    )
}

fn ensure_claude_collab_permissions(root: &Path) -> std::io::Result<()> {
    let path = root.join(".claude").join("settings.json");
    std::fs::create_dir_all(path.parent().unwrap())?;
    let mut root_value = if path.exists() {
        serde_json::from_str(&std::fs::read_to_string(&path)?)
            .unwrap_or_else(|_| serde_json::json!({}))
    } else {
        serde_json::json!({})
    };
    if let Some(table) = root_value.as_object_mut() {
        let permissions = table
            .entry("permissions")
            .or_insert_with(|| serde_json::json!({}));
        merge_allow_patterns(
            permissions,
            "allow",
            &["Bash(collab)", "Bash(collab *)", "Bash(collab-mcp)"],
        );
    }
    std::fs::write(
        path,
        format!("{}\n", serde_json::to_string_pretty(&root_value).unwrap()),
    )
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

- Every registered identity is an equal `peer`; there is no inferred master
  from first registration. Codex/Cursor root is not Collab master.
- `collab init` and peer registration never create a master. A master exists
  only when a registered peer has a live tmux pane and was assigned by
  user-approved self-promotion or live-master delegation. A recorded identity
  with a dead pane is not a live master.
- If a live master exists, other peers cannot promote; only that master may
  `collab master delegate <peer>`. If no live master exists, a peer may
  `collab master promote --approval "<user text>"` itself after explicit user
  approval. Master authority is arbitration only; it does not take another
  peer's task. Independent peers may temporarily decline a master
  collaboration invite to protect their own task; managed subagents must obey
  the master.
- Each peer self-registers one task and owns its full worktree, test,
  integration, main verification, push, cleanup, and resource lifecycle.
- Task owner, resource holder, integration lease, and daemon operator are
  scoped capabilities, never durable identity roles.
- Peers send no normal progress reports. P2P communication is limited to
  durable resource occupancy and release coordination.

## Task lifecycle

```
working -> verifying -> reviewed -> delivered
        -> accepted -> integrated/merged -> cleanup_pending
        -> cleanup_verified -> closed
        -> rework -> working
blocked -> bounded waiting -> resource release/timeout -> owner recheck
```

Task records use a fixed shape:
`id / owner / feature_id / worktree_path / branch / base_commit / priority /
 status`. Normal statuses are `working`, `blocked`, `waiting`, `verifying`,
`reviewed`, `delivered`, `accepted`, `rework`, `merged`, `closed`, and
`cancelled`.

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
collab master status              # live master, or recorded-but-dead identity
collab master promote --approval "<user text>"
collab master delegate <peer>     # live master only
collab task register <id> --feature <feature-id> --worktree <path> \
  --branch <branch> --base-commit <sha> --priority p2
collab task wait <id> --for <blocking-task>
collab task deliver <id> --evidence "commit=<sha>; gates=pass" --worktree <path>
collab task block <id> --next "blocked: <evidence and next condition>"
collab task review <id> --accept --evidence "review gates=pass"
collab task integrated <id> --commit <main-sha> --evidence "main gates=pass"
collab task close <id>            # owner; verifies merged/clean, releases claim
collab task close <id> --force --reason "..."  # master/approved fallback close
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
as one submit. Cursor uses literal keys, then `C-m` after 250ms in a second
tmux process; Codex uses `paste-buffer -p` plus `C-m` in one tmux queue. The direct-message lease is reusable until expiry;
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
    fn sandboxed_tmux_lookup_falls_back_to_initialized_cwd() {
        let cwd = test_root("sandbox-cwd");
        init(&cwd).unwrap();
        let resolved = match project_root_from(Some("%743"), cwd.clone(), |_| {
            anyhow::bail!("cannot resolve project root for tmux pane %743")
        }) {
            Ok(root) => root,
            Err(_) => inherited_cwd_if_initialized(cwd.clone()).unwrap(),
        };
        assert_eq!(resolved, cwd);
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
        for mcp in [root.join(".cursor/mcp.json"), root.join(".mcp.json")] {
            assert!(std::fs::read_to_string(&mcp)
                .unwrap()
                .contains("collab-mcp"));
        }

        init(&root).unwrap();
        let second = std::fs::read_to_string(&path).unwrap();
        assert_eq!(first, second);
        let cursor_mcp = root.join(".cursor/mcp.json");
        std::fs::write(&cursor_mcp, "{\"keep\":true}").unwrap();
        std::fs::write(
            root.join(".mcp.json"),
            r#"{"mcpServers":{"other":{"command":"keep-me"}}}"#,
        )
        .unwrap();
        init(&root).unwrap();
        let merged: serde_json::Value =
            serde_json::from_str(&std::fs::read_to_string(&cursor_mcp).unwrap()).unwrap();
        assert_eq!(merged["keep"], true);
        assert!(merged["mcpServers"]["collab"]["command"]
            .as_str()
            .unwrap()
            .contains("collab-mcp"));
        let generic: serde_json::Value =
            serde_json::from_str(&std::fs::read_to_string(root.join(".mcp.json")).unwrap())
                .unwrap();
        assert_eq!(generic["mcpServers"]["other"]["command"], "keep-me");
        assert!(generic["mcpServers"]["collab"]["command"]
            .as_str()
            .unwrap()
            .contains("collab-mcp"));
        let codex: toml::Value =
            toml::from_str(&std::fs::read_to_string(root.join(".codex/config.toml")).unwrap())
                .unwrap();
        assert!(codex.get("sandbox_mode").is_none());
        assert!(codex.get("approval_policy").is_none());
        assert!(codex["mcp_servers"]["collab"]["command"]
            .as_str()
            .unwrap()
            .contains("collab-mcp"));
        let cursor_cli: serde_json::Value =
            serde_json::from_str(&std::fs::read_to_string(root.join(".cursor/cli.json")).unwrap())
                .unwrap();
        assert!(cursor_cli.get("sandbox").is_none());
        assert_eq!(cursor_cli["permissions"]["deny"], serde_json::json!([]));
        assert!(cursor_cli["permissions"]["allow"]
            .as_array()
            .unwrap()
            .iter()
            .any(|item| item.as_str() == Some("Shell(collab *)")));
        std::fs::write(
            root.join(".codex/config.toml"),
            "model = \"keep-me\"\nsandbox_mode = \"workspace-write\"\n",
        )
        .unwrap();
        std::fs::write(
            root.join(".cursor/cli.json"),
            r#"{"sandbox":{"mode":"disabled"},"permissions":{"allow":["Shell(other)"]}}"#,
        )
        .unwrap();
        init(&root).unwrap();
        let upgraded: toml::Value =
            toml::from_str(&std::fs::read_to_string(root.join(".codex/config.toml")).unwrap())
                .unwrap();
        assert_eq!(upgraded["model"].as_str(), Some("keep-me"));
        assert!(upgraded.get("sandbox_mode").is_none());
        let repaired: serde_json::Value =
            serde_json::from_str(&std::fs::read_to_string(root.join(".cursor/cli.json")).unwrap())
                .unwrap();
        assert!(repaired.get("sandbox").is_none());
        assert!(repaired["permissions"]["allow"]
            .as_array()
            .unwrap()
            .iter()
            .any(|item| item.as_str() == Some("Shell(other)")));
        std::fs::remove_dir_all(root).ok();
    }
}
