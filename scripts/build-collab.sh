#!/bin/sh
set -eu

repo_root=$(CDPATH= cd -- "$(dirname -- "$0")/.." && pwd)
state_dir=${COLLAB_BUILD_STATE_DIR:-${COLLAB_STATE_DIR:-"$HOME/.collab"}}
version_path="$state_dir/build-version"
lock_path="$state_dir/build-version.lock"

mkdir -p "$state_dir"

exec 9>"$lock_path"
if command -v flock >/dev/null 2>&1; then
  flock -x 9
elif command -v lockf >/dev/null 2>&1; then
  lockf 9
else
  printf 'Collab build requires flock or lockf for %s\n' "$lock_path" >&2
  exit 1
fi

current=$(cat "$version_path" 2>/dev/null || printf '0')
case "$current" in
  ''|*[!0-9]*) printf 'invalid Collab build counter: %s\n' "$current" >&2; exit 1 ;;
esac
next=$((current + 1))

temporary_path="$state_dir/build-version.tmp.$$"
trap 'rm -f "$temporary_path"' EXIT HUP INT TERM
printf '%s\n' "$next" >"$temporary_path"
mv "$temporary_path" "$version_path"
trap - EXIT HUP INT TERM

cd "$repo_root"
COLLAB_BUILD_VERSION="$next" cargo build --release --locked "$@"
printf 'collab_build_version=0.2.%04d\n' "$next"
