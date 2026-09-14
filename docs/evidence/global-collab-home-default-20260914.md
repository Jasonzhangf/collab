# Global Collab home default: `~/.collab` verification (2026-09-14)

Scope: candidate `codex/global-collab-home-20260914`, which changes the
default host state root in `src/scope.rs` `HostPaths::resolve_from_env()` from
`$HOME/.local/state/collab` to `$HOME/.collab`. `COLLAB_STATE_DIR` and
`XDG_STATE_HOME` precedence is unchanged.

This document records the verification required before accepting that change.
All commands ran against the candidate `target/release/collab` built from this
worktree. No project journal, mailbox, identity token or binding was edited,
deleted or reset.

## Source and unit evidence

- `cargo fmt -- --check`: pass.
- `cargo build --release`: pass.
- `scope::tests`: pass, including the new
  `default_host_endpoint_uses_dot_collab_in_home`, which asserts that
  `resolve_state_root(Some(""), Some(""), Some(HOME))` falls through to
  `HOME/.collab`.
- `server::startup_tests`: 6 passed / 0 failed.

## Default-path resolution

With `COLLAB_STATE_DIR` and `XDG_STATE_HOME` unset, the host state root and
endpoint paths resolve under `HOME/.collab`:

```text
state_root   = $HOME/.collab
socket_path  = $HOME/.collab/server.sock
lock_path    = $HOME/.collab/daemon.lock
```

This is asserted by `default_host_endpoint_uses_dot_collab_in_home` and
confirmed by the temporary-`HOME` black box below, which leaves
`COLLAB_STATE_DIR` and `XDG_STATE_HOME` unset.

```sh
HOME_TMP=$(mktemp -d /tmp/collab-home-default-XXXXXX)
A=$(mktemp -d /tmp/collab-home-a-XXXXXX)
COLLAB_BIN="$PWD/target/release/collab"
(cd "$A" && env -u TMUX -u TMUX_PANE -u COLLAB_STATE_DIR -u XDG_STATE_HOME \
    HOME="$HOME_TMP" "$COLLAB_BIN" up)
```

`$COLLAB_BIN` is an absolute path captured before the subshell changes
directory, so the command runs from the isolated project `$A`.

Observed output:

```json
{"ok":true,"server":"/tmp/collab-home-default-<id>/.collab/server.sock","started":true}
```

The daemon published `$HOME_TMP/.collab/server.sock` and created
`daemon.lock`, `server.pid`, `events.jsonl` and `log.txt` under
`$HOME_TMP/.collab`, with no state under `$HOME_TMP/.local/state/collab`. The
only project-side effects were `scope::init`'s own outputs in `$A`
(`.agent-collab/{runs,handoff,merge-queue,panes,mailbox,messages,mailboxes,server}`,
`docs/collab.md`, and the agent MCP/permission files it merges); `$HOME_TMP`
contained no project journal. `$A/.agent-collab/server/journal.jsonl` is the
project reducer journal created by the daemon, not host endpoint state.

## Isolated host-daemon black box

A temporary isolated state root was used so no production daemon or journal
was touched. `TMUX`/`TMUX_PANE` were unset so the project scope came from the
process cwd rather than an inherited pane:

```sh
STATE=$(mktemp -d /tmp/collab-clean-proof-XXXXXX)
A=$(mktemp -d /tmp/collab-clean-a-XXXXXX)
B=$(mktemp -d /tmp/collab-clean-b-XXXXXX)
COLLAB_BIN="$PWD/target/release/collab"
run() { env -u TMUX -u TMUX_PANE COLLAB_STATE_DIR="$STATE" \
        "$COLLAB_BIN" "$@"; }

cd "$A"; run up        # {"ok":true,"server":"$STATE/server.sock","started":true}
cd "$A"; run status    # {"messages":0,"tasks":0,"workers":0}
cd "$B"; run up        # {"ok":true,"server":"$STATE/server.sock","started":false}
cd "$B"; run status    # {"messages":0,"tasks":0,"workers":0}
cd "$A"; run down      # {"down":true,"ok":true,"server":"$STATE/server.sock"}
cd "$A"; run up        # {"ok":true,"server":"$STATE/server.sock","started":true}
cd "$A"; run status    # {"messages":0,"tasks":0,"workers":0}
cd "$A"; run down
```

Observed results:

- Host endpoint files are owned entirely by the host state root:
  `$STATE/server.sock`, `$STATE/daemon.lock`, `$STATE/server.pid`,
  `$STATE/events.jsonl`, `$STATE/log.txt`, and `$STATE/DOWN` after `down`.
- One host daemon serves two independent project scopes. Project A and
  project B both report the same `server` path from `up`; the second `up`
  returns `started:false` because the daemon is already running.
- Each project's reducer journal stays project-local. The daemon holds only
  `$A/.agent-collab/server/journal.jsonl` (0 lines) and does not create a
  `journal.jsonl` in `$B` until B first writes; neither scope reads the other
  project's journal or the legacy host daemon's state.
- `up` creates the project `.agent-collab` skeleton when absent
  (`handoff/`, `mailbox/`, `mailboxes/`, `merge-queue/`, `messages/`,
  `panes/`, `runs/`, `server/`).
- Controlled restart: `down` returns `{"down":true,"ok":true}`, removes the
  daemon process (PID no longer alive) and writes `$STATE/DOWN`; a following
  `up` starts a fresh daemon (`started:true`) on the same socket and `status`
  succeeds again.

## Old versus new host endpoint coordination

The candidate fences the legacy single-host writer instead of racing it. With
a legacy host daemon holding `/tmp/collab-host.lock`, starting the new host
daemon in an isolated state root fails closed:

```text
DAEMON_MIGRATION_REQUIRED: legacy host daemon lock is held at
/tmp/collab-host.lock; stop or migrate the legacy writer before starting the
host daemon
```

After the operator stopped the legacy writer through its own service command
(`collab down`), `/tmp/collab-host.lock` had no holder and the new host daemon
started normally in the isolated root, as recorded above. `startup_tests`
`startup_rejects_a_held_legacy_project_lock`,
`startup_rejects_a_reachable_legacy_project_daemon` and
`running_host_daemon_holds_legacy_writer_fence` cover the same fence at the
unit level.

## Boundary

This verifies that the default host state root is `$HOME/.collab`, that one
host daemon owns the endpoint files under that root and serves multiple
project scopes, that restart via `down`/`up` works, and that the new host
daemon refuses to start while a legacy host writer holds the legacy fence.
It does not claim a production migration, an installed-binary restart of the
live host daemon, or a completed governance-history reset.
