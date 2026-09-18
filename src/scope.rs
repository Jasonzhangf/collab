use std::path::{Path, PathBuf};

use crate::identity::{validate_id_for_protocol, AppServerId};
use serde::{Deserialize, Serialize};

pub const COLLAB_STATE_DIR_ENV: &str = "COLLAB_STATE_DIR";
pub const HOME_ENV: &str = "HOME";
pub const COLLAB_SOCKET_PATH_ENV: &str = "COLLAB_SOCKET_PATH";
pub const COLLAB_HOST_SOCKET_ENV: &str = "COLLAB_HOST_SOCKET";
pub const COLLAB_LOCK_PATH_ENV: &str = "COLLAB_LOCK_PATH";
pub const COLLAB_HOST_LOCK_ENV: &str = "COLLAB_HOST_LOCK";

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HostPaths {
    state_root: PathBuf,
    socket_path: PathBuf,
    lock_path: PathBuf,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CanonicalProjectRoute {
    pub root: PathBuf,
    pub app_scope_id: AppServerId,
}

impl HostPaths {
    pub fn from_state_root(root: impl AsRef<Path>) -> anyhow::Result<Self> {
        let state_root = validate_host_path(root.as_ref().to_path_buf(), "host state root")?;
        Ok(Self {
            socket_path: state_root.join("server.sock"),
            lock_path: state_root.join("daemon.lock"),
            state_root,
        })
    }

    pub fn for_state_root(root: impl AsRef<Path>) -> anyhow::Result<Self> {
        Self::from_state_root(root)
    }

    pub fn resolve_from_env() -> anyhow::Result<Self> {
        let state_root = resolve_state_root(
            std::env::var_os(COLLAB_STATE_DIR_ENV),
            std::env::var_os(HOME_ENV),
        )?;
        let mut paths = Self::from_state_root(state_root)?;
        apply_endpoint_overrides(
            &mut paths,
            first_env_path([COLLAB_SOCKET_PATH_ENV, COLLAB_HOST_SOCKET_ENV])?,
            first_env_path([COLLAB_LOCK_PATH_ENV, COLLAB_HOST_LOCK_ENV])?,
        )?;
        Ok(paths)
    }

    pub fn from_env() -> anyhow::Result<Self> {
        Self::resolve_from_env()
    }

    pub fn resolve() -> anyhow::Result<Self> {
        Self::resolve_from_env()
    }

    pub fn for_project(_project_root: &Path) -> anyhow::Result<Self> {
        Self::resolve_from_env()
    }

    pub fn state_root(&self) -> &Path {
        &self.state_root
    }

    pub fn server_dir(&self) -> PathBuf {
        self.state_root.clone()
    }

    pub fn socket_path(&self) -> PathBuf {
        self.socket_path.clone()
    }

    pub fn sock_path(&self) -> PathBuf {
        self.socket_path()
    }

    pub fn lock_path(&self) -> PathBuf {
        self.lock_path.clone()
    }

    pub fn down_path(&self) -> PathBuf {
        self.state_root.join("DOWN")
    }

    pub fn events_path(&self) -> PathBuf {
        self.state_root.join("events.jsonl")
    }

    pub fn journal_path(&self) -> PathBuf {
        self.state_root.join("journal.jsonl")
    }

    pub fn pid_path(&self) -> PathBuf {
        self.state_root.join("server.pid")
    }

    pub fn log_path(&self) -> PathBuf {
        self.state_root.join("log.txt")
    }

    pub fn ensure_root(&self) -> std::io::Result<()> {
        std::fs::create_dir_all(&self.state_root)
    }
}

/// Resolve the canonical registered project route for an execution worktree.
///
/// A Git worktree has no project-local `.agent-collab/` and therefore cannot
/// own a peer identity. The host route journal is the only durable index that
/// can map an execution cwd back to the registered canonical project root.
/// This lookup is read-only and requires an exact filesystem ancestor; it
/// never searches arbitrary parents or chooses a route by name.
fn load_route_records(host_paths: &HostPaths) -> anyhow::Result<Vec<CanonicalProjectRoute>> {
    let route_journal = host_paths.state_root().join("routes.jsonl");
    let content = match std::fs::read_to_string(&route_journal) {
        Ok(content) => content,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(error) => {
            anyhow::bail!(
                "cannot read host route journal {}: {error}",
                route_journal.display()
            )
        }
    };
    if content.is_empty() {
        return Ok(Vec::new());
    }
    if !content.ends_with('\n') {
        anyhow::bail!("host route journal must end with a newline");
    }

    let mut records = Vec::new();
    for line in content.lines() {
        if line.trim().is_empty() {
            anyhow::bail!("host route journal contains an empty line");
        }
        let value: serde_json::Value = serde_json::from_str(line)?;
        let Some(canonical_root) = value
            .get("canonical_root")
            .and_then(serde_json::Value::as_str)
        else {
            continue;
        };
        let root = match std::fs::canonicalize(canonical_root) {
            Ok(root) => root,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => continue,
            Err(error) => {
                anyhow::bail!("cannot canonicalize route root {}: {error}", canonical_root)
            }
        };
        let app_scope_id = value
            .get("app_scope_id")
            .and_then(serde_json::Value::as_str)
            .ok_or_else(|| anyhow::anyhow!("host route record is missing app_scope_id"))?;
        records.push(CanonicalProjectRoute {
            root,
            app_scope_id: AppServerId::new(app_scope_id.to_owned())?,
        });
    }
    records.sort_by(|left, right| {
        right
            .root
            .components()
            .count()
            .cmp(&left.root.components().count())
            .then_with(|| left.root.cmp(&right.root))
    });
    Ok(records)
}

/// Resolve the canonical route bound to one registered peer identity.
///
/// The identity's App Server scope is authoritative. The caller's cwd may be
/// a Git worktree, so it must be inside the route's canonical root but is not
/// allowed to select a different project by itself.
pub fn canonical_route_for_identity(
    host_paths: &HostPaths,
    cwd: &Path,
    app_scope_id: &AppServerId,
) -> anyhow::Result<CanonicalProjectRoute> {
    let cwd = std::fs::canonicalize(cwd)?;
    let mut matches = load_route_records(host_paths)?
        .into_iter()
        .filter(|route| {
            &route.app_scope_id == app_scope_id && cwd.strip_prefix(&route.root).is_ok()
        })
        .collect::<Vec<_>>();
    match matches.len() {
        1 => Ok(matches.pop().unwrap()),
        0 => anyhow::bail!(
            "no registered Collab route matches app scope {} and contains cwd {}",
            app_scope_id,
            cwd.display()
        ),
        _ => {
            let roots = matches
                .iter()
                .map(|route| route.root.display().to_string())
                .collect::<Vec<_>>();
            anyhow::bail!(
                "multiple canonical Collab routes match app scope {} and contain cwd {}: {}",
                app_scope_id,
                cwd.display(),
                roots.join(", ")
            )
        }
    }
}

fn apply_endpoint_overrides(
    paths: &mut HostPaths,
    socket_override: Option<PathBuf>,
    lock_override: Option<PathBuf>,
) -> anyhow::Result<()> {
    if let Some(value) = socket_override {
        let parent = value.parent().ok_or_else(|| {
            anyhow::anyhow!("host socket path has no parent: {}", value.display())
        })?;
        if parent != paths.state_root() {
            anyhow::bail!(
                "host socket path must be inside host state root {}: {}",
                paths.state_root().display(),
                value.display()
            );
        }
        paths.socket_path = value;
    }
    if let Some(value) = lock_override {
        let expected = paths.state_root().join("daemon.lock");
        if value != expected {
            anyhow::bail!(
                "host lock path must be {} so client and daemon share one lock owner: {}",
                expected.display(),
                value.display()
            );
        }
        paths.lock_path = value;
    }
    Ok(())
}

fn first_env_path<const N: usize>(names: [&str; N]) -> anyhow::Result<Option<PathBuf>> {
    for name in names {
        if let Some(value) = nonempty_env(name) {
            return Ok(Some(validate_host_path(
                PathBuf::from(value),
                "host endpoint",
            )?));
        }
    }
    Ok(None)
}

fn nonempty_env(name: &str) -> Option<std::ffi::OsString> {
    std::env::var_os(name).filter(|value| !value.is_empty())
}

fn resolve_state_root(
    state_dir: Option<std::ffi::OsString>,
    home: Option<std::ffi::OsString>,
) -> anyhow::Result<PathBuf> {
    if let Some(value) = state_dir.filter(|value| !value.is_empty()) {
        return Ok(PathBuf::from(value));
    }
    if let Some(value) = home.filter(|value| !value.is_empty()) {
        return Ok(PathBuf::from(value).join(".collab"));
    }
    anyhow::bail!(
        "collab host state root is unavailable; set ${COLLAB_STATE_DIR_ENV} or ${HOME_ENV}"
    )
}

fn validate_host_path(path: PathBuf, label: &str) -> anyhow::Result<PathBuf> {
    if !path.is_absolute() {
        anyhow::bail!("{label} must be an absolute path: {}", path.display());
    }
    if path.components().any(|component| {
        matches!(
            component,
            std::path::Component::ParentDir | std::path::Component::CurDir
        )
    }) {
        anyhow::bail!(
            "{label} must not contain '.' or '..' path components: {}",
            path.display()
        );
    }
    Ok(path)
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(transparent)]
pub struct ProjectScopeId(String);

impl ProjectScopeId {
    pub fn new(value: impl Into<String>) -> anyhow::Result<Self> {
        let value = value.into();
        if value.is_empty() {
            anyhow::bail!("project scope id must not be empty");
        }
        if value.chars().any(char::is_control) {
            anyhow::bail!("project scope id must not contain control characters");
        }
        if !Path::new(&value).is_absolute() {
            anyhow::bail!("project scope id must be an absolute path");
        }
        Ok(Self(value))
    }

    fn from_registered_cwd(cwd: &Path) -> anyhow::Result<Self> {
        let root = normalize_registered_cwd(cwd)?;
        let value = root.to_str().ok_or_else(|| {
            anyhow::anyhow!("registered project cwd must be valid UTF-8 for the wire scope")
        })?;
        Self::new(value.to_owned())
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }

    pub fn validate(&self) -> anyhow::Result<()> {
        Self::new(self.0.clone()).map(|_| ())
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RouteScope {
    pub app_scope_id: AppServerId,
    pub project_scope_id: ProjectScopeId,
}

impl RouteScope {
    pub fn for_registered_project(
        app_scope_id: AppServerId,
        registered_cwd: &Path,
    ) -> anyhow::Result<Self> {
        Ok(Self {
            app_scope_id,
            project_scope_id: ProjectScopeId::from_registered_cwd(registered_cwd)?,
        })
    }

    pub fn validate_registered_cwd(&self, registered_cwd: &Path) -> anyhow::Result<()> {
        let normalized = ProjectScopeId::from_registered_cwd(registered_cwd)?;
        if self.project_scope_id != normalized {
            anyhow::bail!(
                "project cwd is outside the registered project scope: {}",
                registered_cwd.display()
            );
        }
        Ok(())
    }

    pub fn validate(&self) -> anyhow::Result<()> {
        validate_id_for_protocol(self.app_scope_id.as_str())?;
        self.project_scope_id.validate()
    }

    pub fn validate_same_route(&self, other: &Self) -> anyhow::Result<()> {
        if self != other {
            anyhow::bail!("route scope mismatch");
        }
        Ok(())
    }
}

fn normalize_registered_cwd(cwd: &Path) -> anyhow::Result<PathBuf> {
    if !cwd.is_absolute() || !cwd.is_dir() {
        anyhow::bail!(
            "registered project cwd must be an existing absolute directory: {}",
            cwd.display()
        );
    }
    Ok(std::fs::canonicalize(cwd)?)
}

fn validate_project_root(root: PathBuf) -> anyhow::Result<PathBuf> {
    if !root.is_absolute() || !root.is_dir() {
        anyhow::bail!(
            "project root must be an existing absolute directory: {}",
            root.display()
        );
    }
    Ok(root)
}

/// The launching environment owns project scope. Every peer, including a
/// Codex App Server thread, is bound to the exact process cwd. No caller may
/// select a path and no ancestor is searched.
fn inherited_cwd_if_initialized(cwd: PathBuf) -> anyhow::Result<PathBuf> {
    if cwd.join(".agent-collab").is_dir() {
        validate_project_root(cwd)
    } else {
        anyhow::bail!("no .agent-collab found in inherited cwd {}", cwd.display())
    }
}

pub fn project_root() -> anyhow::Result<PathBuf> {
    Ok(Scope::resolve()?.root)
}

/// Resolve the exact destination for `collab init`. Initialization binds to
/// the process cwd.
pub fn project_root_for_init() -> anyhow::Result<PathBuf> {
    init_project_root(std::env::current_dir()?)
}

fn init_project_root(cwd: PathBuf) -> anyhow::Result<PathBuf> {
    validate_project_root(cwd)
}

/// Create only the current Collab-owned empty project baseline.
///
/// Reset uses this instead of [`init`] so retiring a legacy control plane
/// cannot mutate project MCP settings, editor permissions, or global AppSDK
/// configuration as an unrecorded side effect.
pub fn init_collab_baseline(root: &Path) -> std::io::Result<PathBuf> {
    let base = root.join(".agent-collab");
    for sub in [
        "runs",
        "handoff",
        "merge-queue",
        "mailbox",
        "messages",
        "mailboxes",
        "server",
    ] {
        std::fs::create_dir_all(base.join(sub))?;
    }
    Ok(base)
}

pub fn init(root: &Path) -> std::io::Result<PathBuf> {
    let base = init_collab_baseline(root)?;
    let docs = root.join("docs");
    std::fs::create_dir_all(&docs)?;
    let collab_doc = docs.join("collab.md");
    if !collab_doc.exists() {
        std::fs::write(&collab_doc, COLLAB_DOC)?;
    }
    ensure_project_collab_mcp(root)?;
    ensure_codex_collab_permissions(root)?;
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
tokens, mixed runtime writes, and guessing thread identity are deprecated.

## Runtime boundary

- Every peer registration must include a server-verified App Server candidate.
- Registration owns one deterministic seven-day default direct-message lease;
  daemon restart restores it only while the registered App Server thread still
  matches the peer identity. A shorter explicit lease cannot suppress it.
- App Server is the only registered notification transport.
- Server state, journal, and mailbox are durable truth; a failed wake cannot
  roll back state or fabricate success.
- The runtime is part of the worker identity boundary, not a task preference.

## Roles

- Every registered identity is an equal `peer`; there is no inferred master
  from first registration. Codex root is not Collab master.
- `collab init` and peer registration never create a master. A master exists
  only when a registered peer has a live server-verified transport and was assigned by
  user-approved self-promotion or live-master delegation. A recorded identity
  with a dead App Server thread is not a live master.
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
or asynchronous-result notices. Never type peer messages into a terminal. After the
receiving Agent registers a finite subscription, the daemon may send one id,
abbreviated subject, safe one-line original body preview, and final submit as
one App Server queue operation. The direct-message lease is reusable until expiry;
resource, deadline, and async-result subscriptions remain one-shot.

`collab inbox` and `collab msg <id>` query the durable local mailbox after a
registered App Server thread becomes unavailable; mailbox state remains
authoritative.

## Notifications and waits

There is no periodic continuation. Agent-owned subscriptions are exact-event,
exact-subject, and finite. Direct-message delivery is serialized and reusable
until expiry; other subscriptions are one-shot. No registration, absent,
unknown, working, expired, cancelled, consumed, or exhausted message produces
App Server input. Every wait stores waiter, blocking task owner, reason, deadline,
resume events, and P2P escalation. Timeout changes state without unsolicited
messages; resource release notifies only an exact active subscriber.
"#;

/// Scope guard used by every command except init.
#[derive(Clone)]
pub struct Scope {
    pub root: PathBuf,
}

impl Scope {
    pub fn resolve() -> anyhow::Result<Self> {
        let cwd = std::env::current_dir()?;
        // Validate the host endpoint while resolution is still fallible.
        // The infallible compatibility accessors below are only used after
        // this check (or by isolated unit fixtures).
        HostPaths::resolve()?;
        if cwd.join(".agent-collab").is_dir() {
            return Self::from_project_root(cwd);
        }
        let identity_scope = Scope { root: cwd.clone() };
        let Some(identity) = crate::identity::load_existing(&identity_scope, None)? else {
            anyhow::bail!("no .agent-collab found in inherited cwd {}", cwd.display());
        };
        let Some(runtime) = identity.runtime.as_ref() else {
            anyhow::bail!(
                "persisted Collab identity {} has no registered runtime",
                identity.worker_id
            );
        };
        let host_paths = HostPaths::resolve()?;
        let route = canonical_route_for_identity(&host_paths, &cwd, &runtime.appserver_id)?;
        Ok(Scope { root: route.root })
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
        // This remains the project-local reducer/journal directory.  The
        // daemon socket and lease are exposed separately through host_paths.
        self.root.join(".agent-collab").join("server")
    }

    pub fn host_paths(&self) -> anyhow::Result<HostPaths> {
        // A few in-process startup fixtures construct a bare `Scope` without
        // running `collab init`. Keep those fixtures isolated from the real
        // host endpoint; production scopes always have the project marker and
        // therefore use the host-wide state root below.
        #[cfg(test)]
        if !self.root.join(".agent-collab").is_dir()
            || self.server_dir().join("daemon.lock").exists()
        {
            return HostPaths::from_state_root(self.server_dir());
        }
        HostPaths::for_project(&self.root)
    }

    pub fn host_server_dir(&self) -> PathBuf {
        self.host_paths()
            .expect("Scope::resolve validates the host endpoint")
            .server_dir()
    }

    pub fn sock_path(&self) -> PathBuf {
        self.host_paths()
            .expect("Scope::resolve validates the host endpoint")
            .socket_path()
    }

    pub fn route_scope(&self, app_scope_id: AppServerId) -> anyhow::Result<RouteScope> {
        RouteScope::for_registered_project(app_scope_id, &self.root)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

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
    fn init_scope_uses_unmarked_process_cwd() {
        let cwd = test_root("init-unmarked-cwd");
        std::fs::create_dir_all(&cwd).unwrap();
        let resolved = init_project_root(cwd.clone()).unwrap();
        assert_eq!(resolved, cwd);
        assert!(!resolved.join(".agent-collab").exists());
        std::fs::remove_dir_all(resolved).ok();
    }

    #[test]
    fn init_scope_rejects_a_missing_process_cwd() {
        let missing = test_root("init-missing-cwd");
        assert!(init_project_root(missing).is_err());
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
    fn route_scope_uses_exact_registered_project_cwd() {
        let parent = test_root("route-scope");
        let registered = parent.join("registered");
        let sibling = parent.join("sibling");
        std::fs::create_dir_all(&registered).unwrap();
        std::fs::create_dir_all(&sibling).unwrap();
        let route = RouteScope::for_registered_project(
            AppServerId::new("appserver-1").unwrap(),
            &registered,
        )
        .unwrap();

        route.validate_registered_cwd(&registered).unwrap();
        assert!(route.validate_registered_cwd(&sibling).is_err());
        assert!(route.validate_registered_cwd(&parent).is_err());
        assert_eq!(
            route.project_scope_id.as_str(),
            registered.canonicalize().unwrap().to_string_lossy()
        );
        std::fs::remove_dir_all(parent).ok();
    }

    #[test]
    fn identity_route_resolves_only_for_its_app_scope_and_contains_cwd() {
        let root = test_root("worktree-route");
        let canonical = root.join("project");
        let worktree = canonical.join("playground/task-a");
        let sibling = root.join("project-other");
        let unrelated = root.join("unrelated");
        std::fs::create_dir_all(&worktree).unwrap();
        std::fs::create_dir_all(&sibling).unwrap();
        std::fs::create_dir_all(&unrelated).unwrap();

        let state_root = root.join("host-state");
        std::fs::create_dir_all(&state_root).unwrap();
        let route = json!({
            "version": 1,
            "op": "register",
            "app_scope_id": "appserver-cli",
            "project_scope": canonical.canonicalize().unwrap(),
            "canonical_root": canonical.canonicalize().unwrap(),
            "storage_root": canonical.canonicalize().unwrap(),
            "registered_ms": 1
        });
        std::fs::write(state_root.join("routes.jsonl"), format!("{route}\n")).unwrap();

        let host_paths = HostPaths::for_state_root(&state_root).unwrap();
        let app_scope = AppServerId::new("appserver-cli").unwrap();
        let resolved = canonical_route_for_identity(&host_paths, &worktree, &app_scope).unwrap();
        assert_eq!(resolved.root, canonical.canonicalize().unwrap());
        assert_eq!(resolved.app_scope_id.as_str(), "appserver-cli");
        assert!(canonical_route_for_identity(
            &host_paths,
            &unrelated,
            &AppServerId::new("appserver-cli").unwrap()
        )
        .is_err());
        assert!(canonical_route_for_identity(
            &host_paths,
            &sibling,
            &AppServerId::new("appserver-cli").unwrap()
        )
        .is_err());
        assert!(canonical_route_for_identity(
            &host_paths,
            &worktree,
            &AppServerId::new("appserver-other").unwrap()
        )
        .is_err());

        std::fs::remove_dir_all(root).ok();
    }

    #[test]
    fn scope_resolve_reuses_identity_route_from_a_worktree() {
        let root = test_root("scope-worktree-identity");
        let canonical = root.join("project");
        let worktree = canonical.join("playground/task-a");
        let state_root = root.join("host-state");
        std::fs::create_dir_all(canonical.join(".agent-collab")).unwrap();
        std::fs::create_dir_all(&worktree).unwrap();
        std::fs::create_dir_all(&state_root).unwrap();
        let canonical = canonical.canonicalize().unwrap();
        let route = json!({
            "version": 1,
            "op": "register",
            "app_scope_id": "appserver-cli",
            "project_scope": canonical,
            "canonical_root": canonical,
            "storage_root": canonical,
            "registered_ms": 1
        });
        std::fs::write(state_root.join("routes.jsonl"), format!("{route}\n")).unwrap();
        let identity_dir = state_root.join("identities/worker-a");
        std::fs::create_dir_all(&identity_dir).unwrap();
        std::fs::write(
            identity_dir.join("identity.json"),
            json!({
                "worker_id": "worker-a",
                "token": "token-a",
                "runtime": {
                    "agent_id": "worker-a",
                    "runtime_id": "runtime-a",
                    "appserver_id": "appserver-cli",
                    "endpoint_generation": 1,
                    "binding_id": "binding-a",
                    "native_thread_id": "thread-a"
                },
                "transport": {
                    "kind": "appserver",
                    "endpoint": "unix:///tmp/test.sock",
                    "namespace": "codex_tui",
                    "thread_id": "thread-a",
                    "capabilities": [],
                    "self_check": "test"
                }
            })
            .to_string(),
        )
        .unwrap();

        let previous_state = std::env::var_os(COLLAB_STATE_DIR_ENV);
        let previous_thread = std::env::var_os("CODEX_THREAD_ID");
        let previous_cwd = std::env::current_dir().unwrap();
        std::env::set_var(COLLAB_STATE_DIR_ENV, &state_root);
        std::env::set_var("CODEX_THREAD_ID", "thread-a");
        std::env::set_current_dir(&worktree).unwrap();
        let resolved = Scope::resolve().unwrap();
        std::env::set_current_dir(previous_cwd).unwrap();
        match previous_state {
            Some(value) => std::env::set_var(COLLAB_STATE_DIR_ENV, value),
            None => std::env::remove_var(COLLAB_STATE_DIR_ENV),
        }
        match previous_thread {
            Some(value) => std::env::set_var("CODEX_THREAD_ID", value),
            None => std::env::remove_var("CODEX_THREAD_ID"),
        }

        assert_eq!(resolved.root, canonical);
        std::fs::remove_dir_all(root).ok();
    }

    #[test]
    fn route_scope_serializes_two_levels_and_does_not_mutate_paths() {
        let root = test_root("route-serialization");
        std::fs::create_dir_all(&root).unwrap();
        let route =
            RouteScope::for_registered_project(AppServerId::new("appserver-1").unwrap(), &root)
                .unwrap();
        let before = route.clone();
        let encoded = serde_json::to_value(&route).unwrap();
        assert_eq!(encoded["app_scope_id"], "appserver-1");
        assert_eq!(
            encoded["project_scope_id"],
            root.canonicalize().unwrap().to_string_lossy().as_ref()
        );
        assert_eq!(
            serde_json::from_value::<RouteScope>(encoded).unwrap(),
            route
        );
        assert_eq!(route, before);
        std::fs::remove_dir_all(root).ok();
    }

    #[cfg(unix)]
    #[test]
    fn non_utf8_registered_cwd_fails_closed_without_scope_collision() {
        use std::ffi::OsString;
        use std::os::unix::ffi::OsStringExt;

        let parent = test_root("non-utf8-route-scope");
        std::fs::create_dir_all(&parent).unwrap();
        let first = parent.join(OsString::from_vec(b"project-\xff".to_vec()));
        let second = parent.join(OsString::from_vec(b"project-\xfe".to_vec()));
        if std::fs::create_dir(&first).is_err() || std::fs::create_dir(&second).is_err() {
            std::fs::remove_dir_all(parent).ok();
            return;
        }

        assert!(RouteScope::for_registered_project(
            AppServerId::new("appserver-1").unwrap(),
            &first
        )
        .is_err());
        assert!(RouteScope::for_registered_project(
            AppServerId::new("appserver-1").unwrap(),
            &second
        )
        .is_err());
        std::fs::remove_dir_all(parent).ok();
    }

    #[test]
    fn long_registered_cwd_has_a_valid_unbounded_project_scope() {
        let base = test_root("long-route-scope");
        let mut root = base.clone();
        for index in 0..24 {
            root = root.join(format!("segment-{index:02}-abcdef"));
        }
        std::fs::create_dir_all(&root).unwrap();
        let route =
            RouteScope::for_registered_project(AppServerId::new("appserver-1").unwrap(), &root)
                .unwrap();
        assert!(route.project_scope_id.as_str().len() > 256);
        route.validate_registered_cwd(&root).unwrap();
        std::fs::remove_dir_all(base).ok();
    }

    #[test]
    fn host_endpoint_is_stable_across_project_roots() {
        let host_root = test_root("host-endpoint").join("state");
        let first_project = test_root("host-project-one");
        let second_project = test_root("host-project-two");
        std::fs::create_dir_all(&first_project).unwrap();
        std::fs::create_dir_all(&second_project).unwrap();

        let first = HostPaths::for_state_root(&host_root).unwrap();
        let second = HostPaths::for_state_root(&host_root).unwrap();
        assert_eq!(first.socket_path(), second.socket_path());
        assert_eq!(first.lock_path(), second.lock_path());
        assert_ne!(
            first.socket_path(),
            first_project.join(".agent-collab/server/server.sock")
        );
        assert_ne!(
            second.socket_path(),
            second_project.join(".agent-collab/server/server.sock")
        );

        std::fs::remove_dir_all(first_project).ok();
        std::fs::remove_dir_all(second_project).ok();
        std::fs::remove_dir_all(host_root.parent().unwrap()).ok();
    }

    #[test]
    fn host_endpoint_rejects_relative_state_roots() {
        let error = HostPaths::for_state_root("collab-state").unwrap_err();
        assert!(error.to_string().contains("absolute"));
    }

    #[test]
    fn default_host_endpoint_uses_dot_collab_in_home() {
        let home = test_root("host-home-default");
        std::fs::create_dir_all(&home).unwrap();
        let state_root =
            resolve_state_root(Some("".into()), Some(home.clone().into_os_string())).unwrap();
        assert_eq!(state_root, home.join(".collab"));
        std::fs::remove_dir_all(home).ok();
    }

    #[test]
    fn host_endpoint_rejects_split_socket_root() {
        let root = test_root("host-socket-split");
        let mut paths = HostPaths::for_state_root(&root).unwrap();
        let error = apply_endpoint_overrides(
            &mut paths,
            Some(root.join("nested").join("server.sock")),
            None,
        )
        .unwrap_err();
        assert!(error.to_string().contains("inside host state root"));
        std::fs::remove_dir_all(root).ok();
    }

    #[test]
    fn host_endpoint_rejects_split_lock_owner() {
        let root = test_root("host-lock-split");
        let mut paths = HostPaths::for_state_root(&root).unwrap();
        let error =
            apply_endpoint_overrides(&mut paths, None, Some(root.join("other.lock"))).unwrap_err();
        assert!(error.to_string().contains("one lock owner"));
        std::fs::remove_dir_all(root).ok();
    }

    #[test]
    fn init_releases_collab_doc_only_once() {
        let root = test_root("init");
        init(&root).unwrap();
        let path = root.join("docs/collab.md");
        assert!(path.exists());
        let first = std::fs::read_to_string(&path).unwrap();
        assert!(first.contains("# collab workflow"));
        for mcp in [root.join(".mcp.json")] {
            assert!(std::fs::read_to_string(&mcp)
                .unwrap()
                .contains("collab-mcp"));
        }

        init(&root).unwrap();
        let second = std::fs::read_to_string(&path).unwrap();
        assert_eq!(first, second);
        std::fs::write(
            root.join(".mcp.json"),
            r#"{"mcpServers":{"other":{"command":"keep-me"}}}"#,
        )
        .unwrap();
        init(&root).unwrap();
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
        std::fs::write(
            root.join(".codex/config.toml"),
            "model = \"keep-me\"\nsandbox_mode = \"workspace-write\"\n",
        )
        .unwrap();
        init(&root).unwrap();
        let upgraded: toml::Value =
            toml::from_str(&std::fs::read_to_string(root.join(".codex/config.toml")).unwrap())
                .unwrap();
        assert_eq!(upgraded["model"].as_str(), Some("keep-me"));
        assert!(upgraded.get("sandbox_mode").is_none());
        std::fs::remove_dir_all(root).ok();
    }
}
