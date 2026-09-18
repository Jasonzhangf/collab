#!/usr/bin/env bash

set -euo pipefail

umask 077

script_path="${BASH_SOURCE[0]}"
script_dir="$(cd "$(dirname "$script_path")" && pwd -P)"
repo_root="$(cd "$script_dir/.." && pwd -P)"
user_home="${HOME:?HOME is required to resolve the user-local install roots}"

if [[ $# -eq 1 && "${1:-}" == "--help" ]]; then
  cat <<'USAGE'
Usage: scripts/install-global-collab.sh

Builds the current Collab release, atomically installs collab and collab-mcp
beside the active cargo executable, installs the embedded collab Skill, and
removes exact Collab-managed legacy local binary copies. The global daemon is
not stopped or restarted.
USAGE
  exit 0
fi

if [[ $# -ne 0 ]]; then
  echo "error: no arguments are supported; run this script from any directory" >&2
  exit 2
fi

cargo_path="$(command -v cargo || true)"
if [[ -z "$cargo_path" || ! -x "$cargo_path" ]]; then
  echo "error: cargo is not available on PATH" >&2
  exit 1
fi

cargo_bin_dir="$(dirname "$cargo_path")"
canonical_bin="$cargo_bin_dir/collab"
canonical_mcp="$cargo_bin_dir/collab-mcp"
release_dir="$repo_root/target/release"
release_bin="$release_dir/collab"
release_mcp="$release_dir/collab-mcp"
skill_target="$user_home/.agents/skills/collab"

stage_bin=''
stage_mcp=''
cleanup() {
  [[ -z "$stage_bin" || ! -e "$stage_bin" ]] || rm -f -- "$stage_bin"
  [[ -z "$stage_mcp" || ! -e "$stage_mcp" ]] || rm -f -- "$stage_mcp"
}
trap cleanup EXIT

echo "Building Collab release from $repo_root"
cargo build --release --manifest-path "$repo_root/Cargo.toml" --bins

if [[ ! -x "$release_bin" || ! -x "$release_mcp" ]]; then
  echo "error: release binaries were not produced" >&2
  exit 1
fi

release_version="$("$release_bin" --version)"
if [[ ! "$release_version" =~ ^collab[[:space:]][0-9]+\.[0-9]+\.[0-9]+$ ]]; then
  echo "error: release binary returned an invalid version: $release_version" >&2
  exit 1
fi

mkdir -p "$cargo_bin_dir"
stage_bin="$(mktemp "$cargo_bin_dir/.collab-install.XXXXXX")"
cp "$release_bin" "$stage_bin"
chmod 0755 "$stage_bin"
if [[ "$("$stage_bin" --version)" != "$release_version" ]]; then
  echo "error: staged collab binary changed during install" >&2
  exit 1
fi

stage_mcp="$(mktemp "$cargo_bin_dir/.collab-mcp-install.XXXXXX")"
cp "$release_mcp" "$stage_mcp"
chmod 0755 "$stage_mcp"

mv -f -- "$stage_bin" "$canonical_bin"
stage_bin=''
mv -f -- "$stage_mcp" "$canonical_mcp"
stage_mcp=''

remove_exact_copy() {
  local candidate="$1"
  if [[ "$candidate" == "$canonical_bin" || "$candidate" == "$canonical_mcp" ]]; then
    return 0
  fi
  if [[ -e "$candidate" || -L "$candidate" ]]; then
    echo "Removing legacy Collab copy: $candidate"
    rm -f -- "$candidate"
  fi
}

# These are exact Collab-managed user-local locations only. Do not scan or
# delete arbitrary files under the home directory.
remove_exact_copy "$user_home/.local/bin/collab"
remove_exact_copy "$user_home/.local/bin/collab-mcp"
for legacy_copy in "$user_home"/.local/lib/collab/*/collab "$user_home"/.local/lib/collab/*/collab-mcp; do
  [[ -e "$legacy_copy" || -L "$legacy_copy" ]] || continue
  remove_exact_copy "$legacy_copy"
done

if [[ ! -x "$canonical_bin" || "$("$canonical_bin" --version)" != "$release_version" ]]; then
  echo "error: canonical install verification failed: $canonical_bin" >&2
  exit 1
fi

"$canonical_bin" install-skills --target "$skill_target" --force >/dev/null
if [[ ! -s "$skill_target/SKILL.md" ]]; then
  echo "error: collab Skill install verification failed: $skill_target" >&2
  exit 1
fi

digest_line="$(shasum -a 256 "$canonical_bin")"
digest="${digest_line%% *}"
printf 'Installed: %s\nVersion: %s\nSHA-256 (diagnostic): %s\n' \
  "$canonical_bin" "$release_version" "$digest"
printf 'Skill installed: %s\n' "$skill_target"
printf '%s\n' 'The running daemon was not restarted; use an explicit maintenance window when it must load the new binary.'
printf '%s\n' 'Refresh the current shell command cache with: rehash (zsh) or hash -r (bash)'
