#!/bin/sh
set -eu

repo_root=$(CDPATH= cd -- "$(dirname -- "$0")/.." && pwd)
cargo_home=${CARGO_HOME:-"$HOME/.cargo"}
bin_dir="$cargo_home/bin"

cd "$repo_root"
"$repo_root/scripts/build-collab.sh"

candidate_collab="$repo_root/target/release/collab"
candidate_mcp="$repo_root/target/release/collab-mcp"
test -x "$candidate_collab"
test -x "$candidate_mcp"

mkdir -p "$bin_dir"
collab_tmp="$bin_dir/.collab.install.$$"
mcp_tmp="$bin_dir/.collab-mcp.install.$$"
trap 'rm -f "$collab_tmp" "$mcp_tmp"' EXIT HUP INT TERM

cp "$candidate_collab" "$collab_tmp"
cp "$candidate_mcp" "$mcp_tmp"
chmod 755 "$collab_tmp" "$mcp_tmp"
mv "$collab_tmp" "$bin_dir/collab"
mv "$mcp_tmp" "$bin_dir/collab-mcp"
trap - EXIT HUP INT TERM

"$bin_dir/collab" install-skills \
  --target "$HOME/.agents/skills/collab" \
  --force

for relative in \
  SKILL.md \
  references/migration-daemon.md \
  references/notifications.md \
  references/resource-waits.md \
  references/task-worktree-lifecycle.md \
  references/verification.md \
  references/state-paths.md
do
  cmp "$repo_root/skills/collab/$relative" "$HOME/.agents/skills/collab/$relative"
done

candidate_collab_hash=$(shasum -a 256 "$candidate_collab" | cut -d ' ' -f 1)
installed_collab_hash=$(shasum -a 256 "$bin_dir/collab" | cut -d ' ' -f 1)
candidate_mcp_hash=$(shasum -a 256 "$candidate_mcp" | cut -d ' ' -f 1)
installed_mcp_hash=$(shasum -a 256 "$bin_dir/collab-mcp" | cut -d ' ' -f 1)

test "$candidate_collab_hash" = "$installed_collab_hash"
test "$candidate_mcp_hash" = "$installed_mcp_hash"

printf 'version=%s\n' "$("$bin_dir/collab" --version)"
printf 'collab_sha256=%s\n' "$installed_collab_hash"
printf 'collab_mcp_sha256=%s\n' "$installed_mcp_hash"
