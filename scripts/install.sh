#!/bin/sh

# Install the latest published bettercodex binary. The only persistent payload
# is the executable itself; source, Rust, Cargo, and native build tools are not
# downloaded or required.

set -eu

DEFAULT_REPOSITORY="ummay0432/bettercodex"
MAX_ARCHIVE_BYTES=134217728
MAX_BINARY_BYTES=268435456

candidate=""
smoke_root=""

step() {
  printf '==> %s\n' "$1"
}

warn() {
  printf 'bettercodex installer: warning: %s\n' "$1" >&2
}

fail() {
  printf 'bettercodex installer: %s\n' "$1" >&2
  exit 1
}

cleanup() {
  set +e
  if [ -n "$candidate" ]; then
    rm -f "$candidate"
  fi
  if [ -n "$smoke_root" ]; then
    rm -rf "$smoke_root"
  fi
}

trap cleanup 0
trap 'exit 1' 1 2 15

usage() {
  cat <<EOF
Usage: install.sh

Downloads, verifies, and atomically installs the matching binary from the
latest published bettercodex GitHub release. No compilation is performed.

Environment:
  BCODEX_INSTALL_DIR          Binary directory (default: \$HOME/.local/bin).
  BCODEX_REPOSITORY           GitHub repository (default: $DEFAULT_REPOSITORY).
  BCODEX_INSTALL_RELEASE_TAG  Exact release tag (used internally by updates).
EOF
}

if [ "$#" -gt 0 ]; then
  case "$1" in
    -h | --help)
      [ "$#" -eq 1 ] || fail "--help does not accept arguments"
      usage
      exit 0
      ;;
    *) fail "unknown argument: $1" ;;
  esac
fi

require_command() {
  command -v "$1" >/dev/null 2>&1 || fail "$1 is required"
}

validate_absolute_path() {
  case "$1" in
    /*) ;;
    *) fail "$2 must be an absolute path" ;;
  esac
  case "$1" in
    *'
'*) fail "$2 must not contain a newline" ;;
  esac
}

valid_release_tag() {
  printf '%s\n' "$1" |
    grep -Eq '^bcodex-v[0-9]+\.[0-9]+\.[0-9]+-[0-9a-fA-F]{40}$'
}

require_command chmod
require_command curl
require_command grep
require_command gzip
require_command mkdir
require_command mktemp
require_command mv
require_command rm
require_command rmdir
require_command tr
require_command uname
require_command wc

repository="${BCODEX_REPOSITORY:-$DEFAULT_REPOSITORY}"
if ! printf '%s\n' "$repository" | grep -Eq '^[A-Za-z0-9_.-]+/[A-Za-z0-9_.-]+$'; then
  fail "BCODEX_REPOSITORY must be an owner/repository name"
fi

if [ -n "${BCODEX_INSTALL_DIR:-}" ]; then
  bin_dir="$BCODEX_INSTALL_DIR"
else
  [ -n "${HOME:-}" ] || fail "HOME or BCODEX_INSTALL_DIR must be set"
  bin_dir="$HOME/.local/bin"
fi
validate_absolute_path "$bin_dir" BCODEX_INSTALL_DIR

case "$(uname -s):$(uname -m)" in
  Darwin:arm64 | Darwin:aarch64)
    platform="macOS Apple silicon"
    asset="bcodex-aarch64-apple-darwin.gz"
    ;;
  Linux:x86_64 | Linux:amd64)
    platform="Linux x86-64"
    asset="bcodex-x86_64-unknown-linux-gnu.gz"
    ;;
  Darwin:*) fail "bettercodex supports Apple silicon macOS only" ;;
  Linux:*) fail "bettercodex supports x86-64 Linux only" ;;
  *) fail "only macOS and Linux are supported by install.sh" ;;
esac

expected_tag="${BCODEX_INSTALL_RELEASE_TAG:-}"
if [ -n "$expected_tag" ] && ! valid_release_tag "$expected_tag"; then
  fail "BCODEX_INSTALL_RELEASE_TAG is invalid"
fi

if [ -n "$expected_tag" ]; then
  download_url="https://github.com/$repository/releases/download/$expected_tag/$asset"
else
  download_url="https://github.com/$repository/releases/latest/download/$asset"
fi

mkdir -p "$bin_dir"
bin_path="$bin_dir/bcodex"
if [ -L "$bin_path" ]; then
  fail "refusing to replace symlinked bettercodex executable $bin_path"
fi
if [ -e "$bin_path" ] && [ ! -f "$bin_path" ]; then
  fail "$bin_path exists but is not a regular file"
fi

candidate="$(mktemp "$bin_dir/.bcodex-stage.XXXXXXXX")" ||
  fail "could not create a staged executable in $bin_dir"

step "Downloading bettercodex for $platform"
if ! curl \
  --proto '=https' \
  --tlsv1.2 \
  --fail \
  --silent \
  --show-error \
  --location \
  --retry 3 \
  --connect-timeout 10 \
  --max-time 300 \
  --max-filesize "$MAX_ARCHIVE_BYTES" \
  --user-agent bettercodex-installer \
  "$download_url" | gzip -dc >"$candidate"; then
  fail "could not download and decompress $asset"
fi

binary_size="$(wc -c <"$candidate" | tr -d '[:space:]')"
case "$binary_size" in
  '' | *[!0-9]*) fail "downloaded binary has an invalid size" ;;
esac
if [ "$binary_size" -eq 0 ] || [ "$binary_size" -gt "$MAX_BINARY_BYTES" ]; then
  fail "downloaded binary size is outside the allowed range"
fi
chmod 755 "$candidate"

candidate_tag="$("$candidate" --internal-release-tag 2>/dev/null)" ||
  fail "downloaded binary has no valid bettercodex release tag"
valid_release_tag "$candidate_tag" ||
  fail "downloaded binary reported an invalid bettercodex release tag"
if [ -n "$expected_tag" ] && [ "$candidate_tag" != "$expected_tag" ]; then
  fail "downloaded binary is $candidate_tag, expected $expected_tag"
fi

tag_suffix="${candidate_tag#bcodex-v}"
candidate_version="${tag_suffix%-*}"
version_output="$("$candidate" --version 2>/dev/null)" ||
  fail "downloaded binary did not report its version"
if [ "$version_output" != "bcodex $candidate_version" ]; then
  fail "downloaded binary version does not match its release tag"
fi

if [ "$(uname -s)" = "Darwin" ]; then
  require_command codesign
  codesign --verify --strict "$candidate" >/dev/null 2>&1 ||
    fail "downloaded macOS binary has an invalid code signature"
fi

smoke_parent="${TMPDIR:-/tmp}"
validate_absolute_path "$smoke_parent" TMPDIR
smoke_root="$(mktemp -d "$smoke_parent/bettercodex-smoke.XXXXXXXX")" ||
  fail "could not create a temporary smoke-test directory"
mkdir -p "$smoke_root/codex" "$smoke_root/bcodex"
step "Verifying bettercodex $candidate_version"
smoke_output="$(
  CODEX_HOME="$smoke_root/codex" \
    BCODEX_HOME="$smoke_root/bcodex" \
    BCODEX_SKIP_UPDATE_CHECK=1 \
    "$candidate" --internal-install-smoke 2>/dev/null
)" || fail "downloaded binary failed its runtime smoke test"
if [ "$smoke_output" != "bcodex $candidate_version install smoke passed" ]; then
  fail "downloaded binary returned an invalid smoke-test result"
fi
rm -rf "$smoke_root"
smoke_root=""

mv -f "$candidate" "$bin_path"
candidate=""

cleanup_legacy_install_state() {
  [ -n "${HOME:-}" ] || return 0
  cache_base="${XDG_CACHE_HOME:-$HOME/.cache}"
  case "$cache_base" in
    /*) ;;
    *) return 0 ;;
  esac
  cache_root="$cache_base/bettercodex"
  [ ! -L "$cache_root" ] || return 0
  legacy_source_install=0
  for legacy_name in build cargo rustup tmp downloads; do
    legacy_path="$cache_root/$legacy_name"
    if [ -d "$legacy_path" ] && [ ! -L "$legacy_path" ]; then
      legacy_source_install=1
      rm -rf "$legacy_path" || warn "could not remove obsolete installer cache $legacy_path"
    fi
  done
  if [ "$legacy_source_install" -eq 1 ]; then
    for legacy_path in "$cache_root"/rusty-v8-*; do
      if [ -d "$legacy_path" ] && [ ! -L "$legacy_path" ]; then
        rm -rf "$legacy_path" || warn "could not remove obsolete installer cache $legacy_path"
      fi
    done
  fi
  rmdir "$cache_root" 2>/dev/null || true
  private_path="$bin_dir/bcodex-path"
  if [ -d "$private_path" ] && [ ! -L "$private_path" ]; then
    rm -rf "$private_path" || warn "could not remove obsolete private helper directory $private_path"
  fi
}

cleanup_legacy_install_state

path_ready=0
case ":${PATH:-}:" in
  *":$bin_dir:"*) path_ready=1 ;;
esac
if [ "$path_ready" -eq 0 ]; then
  default_bin="${HOME:-}/.local/bin"
  if [ -n "${HOME:-}" ] && [ "$bin_dir" = "$default_bin" ]; then
    case "${SHELL:-}" in
      */zsh) profile="$HOME/.zprofile" ;;
      */bash)
        if [ "$(uname -s)" = "Darwin" ]; then
          profile="$HOME/.bash_profile"
        else
          profile="$HOME/.profile"
        fi
        ;;
      *) profile="$HOME/.profile" ;;
    esac
    managed_line='export PATH="$HOME/.local/bin:$PATH"'
    if ! grep -Fqx "$managed_line" "$profile" 2>/dev/null; then
      printf '\n%s\n' "$managed_line" >>"$profile" ||
        warn "could not add $bin_dir to PATH in $profile"
    fi
    step "PATH configured in $profile; open a new terminal"
  else
    warn "$bin_dir is not on PATH; add it before running bcodex"
  fi
fi

step "Installed bcodex $candidate_version at $bin_path"
step "Run: bcodex login"
