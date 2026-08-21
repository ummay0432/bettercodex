#!/bin/sh

# Lint, build, and atomically install the current bettercodex worktree for
# local development. Cargo's target directory is deliberately retained so
# repeated installs reuse compiled artifacts.

set -eu

candidate=""

step() {
  printf '==> %s\n' "$1"
}

warn() {
  printf 'bettercodex local installer: warning: %s\n' "$1" >&2
}

fail() {
  printf 'bettercodex local installer: %s\n' "$1" >&2
  exit 1
}

cleanup() {
  set +e
  if [ -n "$candidate" ] && { [ -e "$candidate" ] || [ -L "$candidate" ]; }; then
    rm -f "$candidate" || warn "could not remove temporary install file $candidate"
  fi
  return 0
}

trap cleanup 0
trap 'exit 1' 1 2 15

for command_name in cargo chmod cmp cp dirname mkdir mktemp mv rm; do
  command -v "$command_name" >/dev/null 2>&1 || fail "required command not found: $command_name"
done

script_dir=$(CDPATH= cd "$(dirname "$0")" && pwd -P) ||
  fail "could not locate the script directory"
repo_root=$(CDPATH= cd "$script_dir/.." && pwd -P) ||
  fail "could not locate the repository root"
cd "$repo_root" || fail "could not enter the repository root"

if [ -n "${CARGO_TARGET_DIR:-}" ]; then
  case "$CARGO_TARGET_DIR" in
    /*) target_dir=$CARGO_TARGET_DIR ;;
    *) target_dir=$repo_root/$CARGO_TARGET_DIR ;;
  esac
else
  target_dir=$repo_root/target
fi
artifact=$target_dir/release/bcodex

[ -n "${HOME:-}" ] || fail "HOME must be set"
case "$HOME" in
  /*) ;;
  *) fail "HOME must be an absolute path" ;;
esac
bin_dir=$HOME/.local/bin
bin_path=$bin_dir/bcodex

case ":${PATH:-}:" in
  *":$bin_dir:"*) ;;
  *) fail "$bin_dir is not on PATH" ;;
esac

step "Linting the current worktree"
cargo lint --locked --target-dir "$target_dir"

step "Building the current worktree"
cargo build --release --locked --bin bcodex --target-dir "$target_dir"

[ -f "$artifact" ] || fail "Cargo did not produce $artifact"
[ ! -L "$artifact" ] || fail "refusing to install a symlinked Cargo artifact"
[ -x "$artifact" ] || fail "Cargo artifact is not executable: $artifact"

mkdir -p "$bin_dir"
if [ -L "$bin_path" ]; then
  fail "refusing to replace symlinked executable $bin_path"
fi
if [ -e "$bin_path" ] && [ ! -f "$bin_path" ]; then
  fail "$bin_path exists but is not a regular file"
fi

if [ -f "$bin_path" ] && cmp -s "$artifact" "$bin_path"; then
  resolved_path=$(command -v bcodex 2>/dev/null || true)
  [ "$resolved_path" = "$bin_path" ] ||
    fail "bcodex resolves to $resolved_path instead of $bin_path"
  step "Installed bcodex already exactly matches the release build"
  exit 0
fi

candidate=$(mktemp "$bin_dir/.bcodex-local.XXXXXXXX") ||
  fail "could not create a temporary install file in $bin_dir"
cp "$artifact" "$candidate" || fail "could not stage the release build"
chmod 755 "$candidate" || fail "could not make the staged executable runnable"
cmp -s "$artifact" "$candidate" || fail "staged executable does not match the release build"

mv -f "$candidate" "$bin_path" || fail "could not atomically install $bin_path"
candidate=""

cmp -s "$artifact" "$bin_path" || fail "installed executable does not match the release build"
resolved_path=$(command -v bcodex 2>/dev/null || true)
[ "$resolved_path" = "$bin_path" ] ||
  fail "bcodex resolves to $resolved_path instead of $bin_path"
"$bin_path" --version >/dev/null || fail "installed executable did not run successfully"

step "Installed the current release build at $bin_path"
