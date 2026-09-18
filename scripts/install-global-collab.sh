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

Builds the current Collab release, stages collab and collab-mcp in one
versioned directory, atomically switches one current symlink, installs the
embedded collab Skill, and removes only verified legacy local binary copies.
Unverified path collisions fail explicitly. The global daemon is not stopped
or restarted.
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
managed_root="$cargo_bin_dir/.collab"
versions_dir="$managed_root/versions"
current_link="$managed_root/current"
release_dir="$repo_root/target/release"
release_bin="$release_dir/collab"
release_mcp="$release_dir/collab-mcp"
skill_target="$user_home/.agents/skills/collab"

stage_dir=''
stage_link=''
skill_stage=''
skill_backup=''
cleanup() {
  [[ -z "$stage_dir" || ! -d "$stage_dir" ]] || rm -rf -- "$stage_dir"
  [[ -z "$stage_link" || ( ! -e "$stage_link" && ! -L "$stage_link" ) ]] || rm -f -- "$stage_link"
  [[ -z "$skill_stage" || ! -d "$skill_stage" ]] || rm -rf -- "$skill_stage"
  if [[ -n "$skill_backup" && ( -e "$skill_backup" || -L "$skill_backup" ) ]]; then
    if [[ ! -e "$skill_target" && ! -L "$skill_target" ]]; then
      mv -- "$skill_backup" "$skill_target"
    fi
  fi
}
trap cleanup EXIT

probe_collab_version() {
  local candidate="$1"
  local output
  [[ -x "$candidate" ]] || return 1
  output="$("$candidate" --version 2>/dev/null)" || return 1
  if [[ "$output" =~ ^collab[[:space:]]([0-9]+\.[0-9]+\.[0-9]+)$ ]]; then
    printf '%s\n' "${BASH_REMATCH[1]}"
  else
    return 1
  fi
}

probe_mcp_version() {
  local candidate="$1"
  local output
  [[ -x "$candidate" ]] || return 1
  output="$(
    printf '%s\n' \
      '{"jsonrpc":"2.0","id":1,"method":"initialize","params":{"protocolVersion":"2024-11-05","capabilities":{},"clientInfo":{"name":"installer","version":"1"}}}' \
      | "$candidate" 2>/dev/null
  )" || return 1
  [[ "$output" == *'"name":"collab"'* ]] || return 1
  [[ "$output" =~ \"version\":\"([0-9]+\.[0-9]+\.[0-9]+)\" ]] || return 1
  printf '%s\n' "${BASH_REMATCH[1]}"
}

atomic_link() {
  local target="$1"
  local path="$2"
  local parent
  local temporary="${path}.install.$$"
  parent="$(dirname "$path")"
  mkdir -p "$parent"
  if [[ -d "$path" && ! -L "$path" ]]; then
    echo "error: refusing to replace a directory with a managed symlink: $path" >&2
    return 1
  fi
  [[ ! -e "$temporary" && ! -L "$temporary" ]] || {
    echo "error: temporary install path already exists: $temporary" >&2
    return 1
  }
  ln -s "$target" "$temporary"
  mv -f -- "$temporary" "$path"
}

resolve_current_target() {
  local target
  target="$(readlink "$current_link")" || return 1
  if [[ "$target" != /* ]]; then
    target="$cargo_bin_dir/$target"
  fi
  printf '%s\n' "$target"
}

verify_pair() {
  local directory="$1"
  local expected_version="$2"
  local collab_version
  local mcp_version
  collab_version="$(probe_collab_version "$directory/collab")" || {
    echo "error: collab identity check failed: $directory/collab" >&2
    return 1
  }
  mcp_version="$(probe_mcp_version "$directory/collab-mcp")" || {
    echo "error: collab-mcp identity check failed: $directory/collab-mcp" >&2
    return 1
  }
  [[ "$collab_version" == "$mcp_version" ]] || {
    echo "error: mixed Collab baseline: collab=$collab_version collab-mcp=$mcp_version" >&2
    return 1
  }
  [[ "$collab_version" == "$expected_version" ]] || {
    echo "error: installed pair version mismatch: expected=$expected_version observed=$collab_version" >&2
    return 1
  }
}

verify_skill_bundle() {
  local directory="$1"
  local relative
  for relative in \
    SKILL.md \
    references/migration-daemon.md \
    references/notifications.md \
    references/resource-waits.md \
    references/task-worktree-lifecycle.md \
    references/verification.md \
    references/state-paths.md; do
    [[ -s "$directory/$relative" ]] || {
      echo "error: installed Collab Skill file is missing or empty: $directory/$relative" >&2
      return 1
    }
    cmp -s "$repo_root/skills/collab/$relative" "$directory/$relative" || {
      echo "error: installed Collab Skill differs from source: $relative" >&2
      return 1
    }
  done
}

echo "Building Collab release from $repo_root"
cargo build --release --manifest-path "$repo_root/Cargo.toml" --bins

if [[ ! -x "$release_bin" || ! -x "$release_mcp" ]]; then
  echo "error: release binaries were not produced" >&2
  exit 1
fi

release_version="$(probe_collab_version "$release_bin")" || {
  echo "error: release collab binary returned an invalid version" >&2
  exit 1
}
release_mcp_version="$(probe_mcp_version "$release_mcp")" || {
  echo "error: release collab-mcp binary failed its identity check" >&2
  exit 1
}
[[ "$release_version" == "$release_mcp_version" ]] || {
  echo "error: release binaries are not one version: collab=$release_version collab-mcp=$release_mcp_version" >&2
  exit 1
}

release_digest="$(shasum -a 256 "$release_bin" | cut -d ' ' -f 1)"
release_mcp_digest="$(shasum -a 256 "$release_mcp" | cut -d ' ' -f 1)"
release_id="$release_version-${release_digest:0:12}-${release_mcp_digest:0:12}"
version_dir="$versions_dir/$release_id"
mkdir -p "$versions_dir"
stage_dir="$(mktemp -d "$versions_dir/.stage.XXXXXX")"
cp "$release_bin" "$stage_dir/collab"
cp "$release_mcp" "$stage_dir/collab-mcp"
chmod 0755 "$stage_dir/collab" "$stage_dir/collab-mcp"
verify_pair "$stage_dir" "$release_version"
printf '%s\n' "managed by scripts/install-global-collab.sh" > "$stage_dir/.managed"

if [[ -e "$version_dir" ]]; then
  [[ -f "$version_dir/.managed" ]] || {
    echo "error: version directory exists without a managed marker: $version_dir" >&2
    exit 1
  }
  cmp -s "$stage_dir/collab" "$version_dir/collab" || {
    echo "error: managed version directory has different collab bytes: $version_dir" >&2
    exit 1
  }
  cmp -s "$stage_dir/collab-mcp" "$version_dir/collab-mcp" || {
    echo "error: managed version directory has different collab-mcp bytes: $version_dir" >&2
    exit 1
  }
  rm -rf -- "$stage_dir"
  stage_dir=''
else
  mv -- "$stage_dir" "$version_dir"
  stage_dir=''
fi

legacy_paths=()
legacy_dirs=()
verified_legacy_bins=()

verify_legacy_pair_dir() {
  local directory="$1"
  local collab_version
  local mcp_version
  local directory_name
  directory_name="$(basename "$directory")"
  [[ "$directory_name" =~ ^[0-9]+\.[0-9]+\.[0-9]+$ ]] || {
    echo "error: legacy Collab directory has an invalid version name: $directory" >&2
    return 1
  }
  [[ -f "$directory/collab" && ! -L "$directory/collab" ]] || {
    echo "error: legacy Collab directory is missing a regular collab binary: $directory" >&2
    return 1
  }
  [[ -f "$directory/collab-mcp" && ! -L "$directory/collab-mcp" ]] || {
    echo "error: legacy Collab directory is missing a regular collab-mcp binary: $directory" >&2
    return 1
  }
  collab_version="$(probe_collab_version "$directory/collab")" || {
    echo "error: refusing to remove an unverified legacy collab binary: $directory/collab" >&2
    return 1
  }
  mcp_version="$(probe_mcp_version "$directory/collab-mcp")" || {
    echo "error: refusing to remove an unverified legacy collab-mcp binary: $directory/collab-mcp" >&2
    return 1
  }
  [[ "$collab_version" == "$mcp_version" ]] || {
    echo "error: legacy Collab pair is mixed: $directory" >&2
    return 1
  }
  echo "Verified legacy Collab pair: $directory ($collab_version)"
  legacy_dirs+=("$directory")
  legacy_paths+=("$directory/collab" "$directory/collab-mcp")
  verified_legacy_bins+=("$directory/collab" "$directory/collab-mcp")
}

resolve_symlink_target() {
  local link="$1"
  local target
  target="$(readlink "$link")" || return 1
  if [[ "$target" != /* ]]; then
    target="$(dirname "$link")/$target"
  fi
  printf '%s\n' "$target"
}

is_verified_legacy_bin() {
  local candidate="$1"
  local verified
  for verified in "${verified_legacy_bins[@]}"; do
    [[ "$candidate" == "$verified" ]] && return 0
  done
  return 1
}

verify_legacy_bin_pair() {
  local collab_path="$1"
  local mcp_path="$2"
  local collab_version
  local mcp_version
  local collab_target
  local mcp_target
  [[ -e "$collab_path" || -L "$collab_path" ]] || {
    echo "error: legacy Collab binary pair is incomplete: $collab_path" >&2
    return 1
  }
  [[ -e "$mcp_path" || -L "$mcp_path" ]] || {
    echo "error: legacy Collab binary pair is incomplete: $mcp_path" >&2
    return 1
  }
  if [[ -L "$collab_path" || -L "$mcp_path" ]]; then
    [[ -L "$collab_path" && -L "$mcp_path" ]] || {
      echo "error: legacy Collab binary pair mixes a symlink and a regular file" >&2
      return 1
    }
    collab_target="$(resolve_symlink_target "$collab_path")" || return 1
    mcp_target="$(resolve_symlink_target "$mcp_path")" || return 1
    is_verified_legacy_bin "$collab_target" || {
      echo "error: legacy symlink does not target a verified Collab pair: $collab_path" >&2
      return 1
    }
    is_verified_legacy_bin "$mcp_target" || {
      echo "error: legacy symlink does not target a verified Collab pair: $mcp_path" >&2
      return 1
    }
  else
    collab_version="$(probe_collab_version "$collab_path")" || {
      echo "error: refusing to remove an unverified legacy collab binary: $collab_path" >&2
      return 1
    }
    mcp_version="$(probe_mcp_version "$mcp_path")" || {
      echo "error: refusing to remove an unverified legacy collab-mcp binary: $mcp_path" >&2
      return 1
    }
    [[ "$collab_version" == "$mcp_version" ]] || {
      echo "error: legacy Collab binary pair is mixed: $collab_path" >&2
      return 1
    }
  fi
  echo "Verified legacy Collab copy pair: $collab_path"
  legacy_paths+=("$collab_path" "$mcp_path")
}

# Verify every candidate before changing the active pair. Pathname alone is not
# ownership evidence: a regular pair must identify itself as one Collab version,
# and a symlink pair must target a verified pair under the legacy version root.
for legacy_dir in "$user_home"/.local/lib/collab/*; do
  [[ -d "$legacy_dir" && ! -L "$legacy_dir" ]] || continue
  [[ "$legacy_dir" != "$versions_dir/"* ]] || continue
  verify_legacy_pair_dir "$legacy_dir"
done
if [[ "$user_home/.local/bin/collab" != "$canonical_bin" || "$user_home/.local/bin/collab-mcp" != "$canonical_mcp" ]]; then
  if [[ -e "$user_home/.local/bin/collab" || -L "$user_home/.local/bin/collab" || -e "$user_home/.local/bin/collab-mcp" || -L "$user_home/.local/bin/collab-mcp" ]]; then
    verify_legacy_bin_pair "$user_home/.local/bin/collab" "$user_home/.local/bin/collab-mcp"
  fi
fi

previous_dir=''
if [[ -L "$current_link" ]]; then
  previous_dir="$(resolve_current_target)" || {
    echo "error: cannot resolve current Collab link: $current_link" >&2
    exit 1
  }
  [[ "$previous_dir" == "$versions_dir/"* ]] || {
    echo "error: current Collab link points outside the managed version root: $previous_dir" >&2
    exit 1
  }
  [[ -f "$previous_dir/.managed" ]] || {
    echo "error: current Collab link target has no managed marker: $previous_dir" >&2
    exit 1
  }
  previous_version="$(probe_collab_version "$previous_dir/collab")" || exit 1
  verify_pair "$previous_dir" "$previous_version" || exit 1
  canonical_version="$(probe_collab_version "$canonical_bin")" || {
    echo "error: canonical collab binary is not a verified Collab binary" >&2
    exit 1
  }
  canonical_mcp_version="$(probe_mcp_version "$canonical_mcp")" || {
    echo "error: canonical collab-mcp binary is not a verified Collab binary" >&2
    exit 1
  }
  [[ "$canonical_version" == "$previous_version" && "$canonical_mcp_version" == "$previous_version" ]] || {
    echo "error: canonical binaries do not match the current managed baseline; refusing to overwrite" >&2
    exit 1
  }
elif [[ -e "$current_link" ]]; then
  echo "error: current Collab path is not a symlink: $current_link" >&2
  exit 1
else
  if [[ -e "$canonical_bin" || -L "$canonical_bin" || -e "$canonical_mcp" || -L "$canonical_mcp" ]]; then
    if [[ ! -x "$canonical_bin" || ! -x "$canonical_mcp" ]]; then
      echo "error: existing canonical Collab pair is incomplete; refusing to mix versions" >&2
      exit 1
    fi
    previous_dir="$(mktemp -d "$versions_dir/.previous.XXXXXX")"
    cp -pL "$canonical_bin" "$previous_dir/collab"
    cp -pL "$canonical_mcp" "$previous_dir/collab-mcp"
    chmod 0755 "$previous_dir/collab" "$previous_dir/collab-mcp"
    previous_version="$(probe_collab_version "$previous_dir/collab")" || {
      echo "error: existing canonical collab binary failed its identity check" >&2
      exit 1
    }
    verify_pair "$previous_dir" "$previous_version" || exit 1
    printf '%s\n' "managed by scripts/install-global-collab.sh" > "$previous_dir/.managed"
  fi
fi

if [[ -z "$previous_dir" ]]; then
  atomic_link "$version_dir" "$current_link"
else
  if [[ ! -L "$current_link" ]]; then
    atomic_link "$previous_dir" "$current_link"
  fi
fi
atomic_link "$current_link/collab" "$canonical_bin"
atomic_link "$current_link/collab-mcp" "$canonical_mcp"
atomic_link "$version_dir" "$current_link"

if ! verify_pair "$current_link" "$release_version"; then
  if [[ -n "$previous_dir" ]]; then
    atomic_link "$previous_dir" "$current_link"
    echo "error: installed pair verification failed; restored the previous baseline" >&2
  else
    echo "error: installed pair verification failed after a fresh install" >&2
  fi
  exit 1
fi

skill_parent="$(dirname "$skill_target")"
mkdir -p "$skill_parent"
skill_stage="$(mktemp -d "$skill_parent/.collab-skill.XXXXXX")"
"$canonical_bin" install-skills --target "$skill_stage" --force >/dev/null
verify_skill_bundle "$skill_stage" || {
  echo "error: staged Collab Skill verification failed" >&2
  exit 1
}
if [[ -e "$skill_target" || -L "$skill_target" ]]; then
  skill_backup="$skill_target.previous.$$"
  [[ ! -e "$skill_backup" && ! -L "$skill_backup" ]] || {
    echo "error: Skill backup path already exists: $skill_backup" >&2
    exit 1
  }
  mv -- "$skill_target" "$skill_backup"
fi
mv -- "$skill_stage" "$skill_target"
skill_stage=''
if ! verify_skill_bundle "$skill_target"; then
  if [[ -n "$skill_backup" ]]; then
    rm -rf -- "$skill_target"
    mv -- "$skill_backup" "$skill_target"
    skill_backup=''
  fi
  if [[ -n "$previous_dir" ]]; then
    atomic_link "$previous_dir" "$current_link"
    echo "error: Skill install failed; restored the previous binary baseline" >&2
  else
    echo "error: Skill install failed after a fresh install" >&2
  fi
  exit 1
fi
if [[ -n "$skill_backup" ]]; then
  rm -rf -- "$skill_backup"
  skill_backup=''
fi

for legacy_path in ${legacy_paths[@]+"${legacy_paths[@]}"}; do
  echo "Removing verified legacy Collab copy: $legacy_path"
  rm -f -- "$legacy_path"
done
for legacy_dir in ${legacy_dirs[@]+"${legacy_dirs[@]}"}; do
  rmdir "$legacy_dir" 2>/dev/null || true
done

installed_digest="$(shasum -a 256 "$canonical_bin" | cut -d ' ' -f 1)"
printf 'Installed: %s\nVersion: %s\nSHA-256 (diagnostic): %s\n' \
  "$canonical_bin" "$release_version" "$installed_digest"
printf 'Skill installed: %s\n' "$skill_target"
printf '%s\n' 'The running daemon was not restarted; use an explicit maintenance window when it must load the new binary.'
printf '%s\n' 'Refresh the current shell command cache with: rehash (zsh) or hash -r (bash)'
