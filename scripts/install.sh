#!/bin/sh

# Install a verified native BetterCodex release, falling back to an immutable
# source build only when no compatible release asset exists. Release-aware
# binaries update themselves directly; this script remains the bootstrap and
# migration path for older installations.

set -eu

DEFAULT_REPOSITORY="ummay0432/bettercodex"
GITHUB_API_ROOT="https://api.github.com"
GITHUB_ARCHIVE_ROOT="https://codeload.github.com"
GITHUB_RELEASE_ROOT="https://github.com"
MAX_GITHUB_ATTEMPTS=3
MAX_RELEASE_ATTEMPTS=3
MAX_SOURCE_ATTEMPTS=3
MAX_ASSET_BYTES=134217728
MAX_BINARY_BLOCKS=262144
RELEASE_TAG_PREFIX="bcodex-v"

tmp_dir=""
staged_binary=""
lock_dir=""
lock_acquired=0
build_cache_used=0
target_dir=""
install_mode=""

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

usage() {
  cat <<EOF
Usage: install.sh

Resolves the latest published BetterCodex release, downloads and verifies this
computer's native executable, and atomically installs it. Rust and a C compiler
are needed only when a compatible release asset is unavailable and the installer
must fall back to a local source build.

Environment:
  BCODEX_INSTALL_DIR  Binary directory (default: \$HOME/.local/bin).
  BCODEX_REPOSITORY   GitHub repository (default: $DEFAULT_REPOSITORY).
EOF
}

cleanup_recorded_temp() {
  stale_lock="$1"
  recorded_temp="$(sed -n '1p' "$stale_lock/tmp" 2>/dev/null || true)"
  recorded_parent="$(sed -n '1p' "$stale_lock/tmp-parent" 2>/dev/null || true)"
  if [ -z "$recorded_parent" ]; then
    recorded_parent="${TMPDIR:-/tmp}"
  fi
  case "$recorded_parent" in
    /*) ;;
    *) return ;;
  esac
  case "$recorded_parent" in
    *'
'*) return ;;
  esac
  recorded_parent="${recorded_parent%/}"
  [ -n "$recorded_parent" ] || recorded_parent="/"
  if [ "$recorded_parent" = "/" ]; then
    expected_prefix="/bettercodex-install."
  else
    expected_prefix="$recorded_parent/bettercodex-install."
  fi
  case "$recorded_temp" in
    "$expected_prefix"*)
      recorded_suffix="${recorded_temp#"$expected_prefix"}"
      case "$recorded_suffix" in
        "" | */*) ;;
        *) rm -rf "$recorded_temp" ;;
      esac
      ;;
  esac
}

cleanup() {
  set +e
  cleanup_incomplete=0
  if [ "$build_cache_used" -eq 1 ] && [ -n "$target_dir" ]; then
    if ! remove_bettercodex_outputs; then
      cleanup_incomplete=1
      warn "could not remove BetterCodex-owned compilation output from $target_dir"
    fi
  fi
  if [ -n "$staged_binary" ]; then
    if ! rm -f "$staged_binary"; then
      cleanup_incomplete=1
      warn "could not remove staged binary $staged_binary"
    fi
  fi
  if [ -n "$tmp_dir" ]; then
    cd /
    if ! rm -rf "$tmp_dir"; then
      cleanup_incomplete=1
      warn "could not remove temporary install tree $tmp_dir"
    fi
  fi
  if [ "$lock_acquired" -eq 1 ] && [ -n "$lock_dir" ]; then
    if [ "$cleanup_incomplete" -eq 0 ]; then
      rm -rf "$lock_dir" || warn "could not remove installer lock $lock_dir"
    else
      warn "retaining installer lock so the next install can retry cleanup"
    fi
  fi
  return 0
}

trap cleanup 0
trap 'exit 1' 1 2 15

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

repository="${BCODEX_REPOSITORY:-$DEFAULT_REPOSITORY}"
if [ -n "${BCODEX_INSTALL_DIR:-}" ]; then
  bin_dir="$BCODEX_INSTALL_DIR"
else
  [ -n "${HOME:-}" ] || fail "HOME or BCODEX_INSTALL_DIR must be set"
  bin_dir="$HOME/.local/bin"
fi
bin_path="$bin_dir/bcodex"

case "$repository" in
  */*) ;;
  *) fail "BCODEX_REPOSITORY must be an owner/repository name" ;;
esac
if ! printf '%s\n' "$repository" | grep -Eq '^[A-Za-z0-9_.-]+/[A-Za-z0-9_.-]+$'; then
  fail "BCODEX_REPOSITORY must be an owner/repository name"
fi

validate_absolute_path() {
  value="$1"
  label="$2"
  case "$value" in
    /*) ;;
    *) fail "$label must be an absolute path" ;;
  esac
  case "$value" in
    *'
'*) fail "$label must not contain a newline" ;;
  esac
}

if [ -n "${HOME:-}" ]; then
  validate_absolute_path "$HOME" HOME
fi
validate_absolute_path "$bin_dir" BCODEX_INSTALL_DIR
if [ -n "${TMPDIR:-}" ]; then
  validate_absolute_path "$TMPDIR" TMPDIR
fi
if [ -n "${XDG_CACHE_HOME:-}" ]; then
  validate_absolute_path "$XDG_CACHE_HOME" XDG_CACHE_HOME
fi

require_command() {
  command -v "$1" >/dev/null 2>&1 || fail "$1 is required"
}

require_command uname

case "$(uname -s)" in
  Darwin) os="macOS" ;;
  Linux) os="Linux" ;;
  *) fail "only macOS and Linux are supported" ;;
esac

case "$(uname -m)" in
  x86_64 | amd64) arch="x86-64" ;;
  arm64 | aarch64) arch="ARM64" ;;
  *) fail "unsupported architecture: $(uname -m)" ;;
esac

case "$os:$arch" in
  macOS:x86-64) host_target="x86_64-apple-darwin" ;;
  macOS:ARM64) host_target="aarch64-apple-darwin" ;;
  Linux:x86-64) host_target="x86_64-unknown-linux-gnu" ;;
  Linux:ARM64) host_target="aarch64-unknown-linux-gnu" ;;
  *) fail "could not determine the native Rust target" ;;
esac

require_command awk
require_command cmp
require_command curl
require_command dirname
require_command grep
require_command gzip
require_command mktemp
require_command sed
require_command wc

mkdir -p "$bin_dir"
if [ -L "$bin_path" ]; then
  fail "refusing to replace symlinked BetterCodex executable $bin_path"
fi
if [ -e "$bin_path" ] && [ ! -f "$bin_path" ]; then
  fail "$bin_path exists but is not a regular file"
fi

acquire_install_lock() {
  lock_dir="$bin_dir/.bcodex-install.lock"
  lock_waits=0
  while ! mkdir "$lock_dir" 2>/dev/null; do
    lock_pid="$(sed -n '1p' "$lock_dir/pid" 2>/dev/null || true)"
    case "$lock_pid" in
      "" | *[!0-9]*) ;;
      *)
        if kill -0 "$lock_pid" 2>/dev/null; then
          fail "another BetterCodex install is already running (process $lock_pid)"
        fi
        ;;
    esac

    if [ "$lock_waits" -lt 2 ]; then
      lock_waits=$((lock_waits + 1))
      sleep 1
      continue
    fi

    stale_lock="$bin_dir/.bcodex-stale-lock.$$"
    if mv "$lock_dir" "$stale_lock" 2>/dev/null; then
      if ! cleanup_recorded_temp "$stale_lock"; then
        if ! mv "$stale_lock" "$lock_dir" 2>/dev/null; then
          warn "orphan cleanup record remains at $stale_lock"
        fi
        fail "could not remove an orphaned BetterCodex install tree"
      fi
      rm -rf "$stale_lock"
      lock_waits=0
    fi
  done
  lock_acquired=1
  printf '%s\n' "$$" >"$lock_dir/pid"

  for stale_binary in "$bin_dir"/.bcodex-stage.*; do
    if [ -e "$stale_binary" ] || [ -L "$stale_binary" ]; then
      rm -f "$stale_binary"
    fi
  done
}

acquire_install_lock

cache_base=""
if [ -n "${XDG_CACHE_HOME:-}" ]; then
  cache_base="$XDG_CACHE_HOME"
elif [ -n "${HOME:-}" ]; then
  cache_base="$HOME/.cache"
fi
cache_root=""
if [ -n "$cache_base" ]; then
  cache_root="$cache_base/bettercodex"
fi

cleanup_retired_updater_cache() {
  [ -n "$cache_root" ] || return 0

  # The retired cache used build/target directly. The current cache stores one
  # compatible generation below build/<host>/target.
  for legacy_path in "$cache_root/build/target" "$cache_root/tmp"; do
    if [ -L "$legacy_path" ]; then
      warn "not removing retired cache symlink $legacy_path"
    elif [ -d "$legacy_path" ]; then
      step "Removing retired updater cache at $legacy_path"
      rm -rf "$legacy_path" || fail "could not remove retired updater cache $legacy_path"
    fi
  done
}

# Remove only layouts that cannot be reused by the current updater.
cleanup_retired_updater_cache

github_get() {
  curl \
    --proto '=https' \
    --tlsv1.2 \
    --fail \
    --silent \
    --show-error \
    --location \
    --connect-timeout 10 \
    --max-time 120 \
    --user-agent bettercodex \
    --header 'Accept: application/vnd.github+json' \
    "$1"
}

# Download one public GitHub response while preserving the distinction between
# a genuine 404 (no release asset) and a transient/network failure. Returns 44
# only for 404 and 75 for a retryable or unexpected response.
github_download_once() {
  download_url="$1"
  download_destination="$2"
  download_partial="$download_destination.partial"
  rm -f "$download_partial"
  if ! download_status="$(
    curl \
      --proto '=https' \
      --tlsv1.2 \
      --silent \
      --show-error \
      --location \
      --connect-timeout 10 \
      --max-time 120 \
      --max-filesize "$MAX_ASSET_BYTES" \
      --user-agent bettercodex \
      --header 'Accept: application/vnd.github+json' \
      --output "$download_partial" \
      --write-out '%{http_code}' \
      "$download_url"
  )"; then
    rm -f "$download_partial"
    return 75
  fi
  case "$download_status" in
    200)
      if [ ! -s "$download_partial" ]; then
        rm -f "$download_partial"
        return 75
      fi
      mv "$download_partial" "$download_destination"
      return 0
      ;;
    404)
      rm -f "$download_partial"
      return 44
      ;;
    *)
      rm -f "$download_partial"
      return 75
      ;;
  esac
}

download_release_file() {
  release_url="$1"
  release_destination="$2"
  release_label="$3"
  github_attempt=1
  while [ "$github_attempt" -le "$MAX_GITHUB_ATTEMPTS" ]; do
    if github_download_once "$release_url" "$release_destination"; then
      return 0
    else
      download_result=$?
    fi
    if [ "$download_result" -eq 44 ]; then
      return 44
    fi
    if [ "$github_attempt" -lt "$MAX_GITHUB_ATTEMPTS" ]; then
      warn "$release_label download failed; retrying ($((github_attempt + 1))/$MAX_GITHUB_ATTEMPTS)"
      sleep "$github_attempt"
    fi
    github_attempt=$((github_attempt + 1))
  done
  return 75
}

resolve_main_commit() {
  github_attempt=1
  while [ "$github_attempt" -le "$MAX_GITHUB_ATTEMPTS" ]; do
    if github_response="$(
      github_get "$GITHUB_API_ROOT/repos/$repository/commits/main" 2>/dev/null
    )"; then
      github_commit="$(
        printf '%s\n' "$github_response" |
          sed -n 's/^[[:space:]]*"sha"[[:space:]]*:[[:space:]]*"\([0-9a-fA-F]\{40\}\)".*/\1/p' |
          sed -n '1p'
      )"
      if is_source_revision "$github_commit"; then
        printf '%s\n' "$github_commit"
        return 0
      fi
    fi
    if [ "$github_attempt" -lt "$MAX_GITHUB_ATTEMPTS" ]; then
      warn "GitHub request failed; retrying ($((github_attempt + 1))/$MAX_GITHUB_ATTEMPTS)"
      sleep "$github_attempt"
    fi
    github_attempt=$((github_attempt + 1))
  done
  return 1
}

download_source_archive() {
  archive_destination="$1"
  archive_revision="$2"
  archive_partial="$archive_destination.partial"
  github_attempt=1
  while [ "$github_attempt" -le "$MAX_GITHUB_ATTEMPTS" ]; do
    rm -f "$archive_partial"
    if github_get \
      "$GITHUB_ARCHIVE_ROOT/$repository/tar.gz/$archive_revision" >"$archive_partial" &&
      [ -s "$archive_partial" ]; then
      mv "$archive_partial" "$archive_destination"
      return 0
    fi
    if [ "$github_attempt" -lt "$MAX_GITHUB_ATTEMPTS" ]; then
      warn "GitHub source download failed; retrying ($((github_attempt + 1))/$MAX_GITHUB_ATTEMPTS)"
      sleep "$github_attempt"
    fi
    github_attempt=$((github_attempt + 1))
  done
  rm -f "$archive_partial"
  return 1
}

is_source_revision() {
  printf '%s\n' "$1" | grep -Eq '^[0-9a-fA-F]{40}$'
}

is_canonical_source_revision() {
  printf '%s\n' "$1" | grep -Eq '^[0-9a-f]{40}$'
}

is_package_version() {
  printf '%s\n' "$1" | grep -Eq '^[0-9]+\.[0-9]+\.[0-9]+$'
}

parse_release_tag() {
  parsed_tag="$1"
  case "$parsed_tag" in
    "$RELEASE_TAG_PREFIX"*) ;;
    *) return 1 ;;
  esac
  parsed_release="${parsed_tag#"$RELEASE_TAG_PREFIX"}"
  parsed_revision="${parsed_release##*-}"
  parsed_version="${parsed_release%-*}"
  [ "$parsed_version" != "$parsed_release" ] || return 1
  is_package_version "$parsed_version" || return 1
  is_canonical_source_revision "$parsed_revision" || return 1
  printf '%s|%s|%s\n' "$parsed_tag" "$parsed_version" "$parsed_revision"
}

resolve_latest_release() {
  release_metadata="$tmp_dir/latest-release.json"
  if download_release_file \
    "$GITHUB_API_ROOT/repos/$repository/releases/latest" \
    "$release_metadata" \
    "GitHub release metadata"; then
    :
  else
    return $?
  fi
  release_tag="$({
    sed -n 's/^[[:space:]]*"tag_name"[[:space:]]*:[[:space:]]*"\([^"]*\)".*/\1/p' \
      "$release_metadata"
  } | sed -n '1p')"
  release_draft="$({
    sed -n 's/^[[:space:]]*"draft"[[:space:]]*:[[:space:]]*\(true\|false\).*/\1/p' \
      "$release_metadata"
  } | sed -n '1p')"
  release_prerelease="$({
    sed -n 's/^[[:space:]]*"prerelease"[[:space:]]*:[[:space:]]*\(true\|false\).*/\1/p' \
      "$release_metadata"
  } | sed -n '1p')"
  [ "$release_draft" = false ] && [ "$release_prerelease" = false ] || return 1
  parse_release_tag "$release_tag"
}

requested_revision="${BCODEX_INSTALL_REVISION:-}"
if [ -n "$requested_revision" ] && ! is_source_revision "$requested_revision"; then
  fail "BCODEX_INSTALL_REVISION must be a full 40-character commit ID"
fi
requested_release_tag="${BCODEX_INSTALL_RELEASE_TAG:-}"
requested_version="${BCODEX_INSTALL_VERSION:-}"
if [ -n "$requested_release_tag" ]; then
  if ! requested_release="$(parse_release_tag "$requested_release_tag")"; then
    fail "BCODEX_INSTALL_RELEASE_TAG is not a canonical BetterCodex release tag"
  fi
  requested_tag_version="$(printf '%s\n' "$requested_release" | awk -F '|' '{ print $2 }')"
  requested_tag_revision="$(printf '%s\n' "$requested_release" | awk -F '|' '{ print $3 }')"
  if [ -n "$requested_revision" ] && [ "$requested_revision" != "$requested_tag_revision" ]; then
    fail "BCODEX_INSTALL_REVISION does not match BCODEX_INSTALL_RELEASE_TAG"
  fi
  if [ -n "$requested_version" ] && [ "$requested_version" != "$requested_tag_version" ]; then
    fail "BCODEX_INSTALL_VERSION does not match BCODEX_INSTALL_RELEASE_TAG"
  fi
  requested_revision="$requested_tag_revision"
  requested_version="$requested_tag_version"
elif [ -n "$requested_version" ]; then
  fail "BCODEX_INSTALL_VERSION requires BCODEX_INSTALL_RELEASE_TAG"
fi

file_sha256() {
  if command -v sha256sum >/dev/null 2>&1; then
    sha256sum "$1" | awk '{ print $1 }'
  elif command -v shasum >/dev/null 2>&1; then
    shasum -a 256 "$1" | awk '{ print $1 }'
  elif command -v openssl >/dev/null 2>&1; then
    openssl dgst -sha256 "$1" | sed 's/^.*= //'
  else
    return 1
  fi
}

require_source_build_tools() {
  require_command tar
  if ! command -v rustup >/dev/null 2>&1; then
    fail "no compatible prebuilt release was available; rustup is required for the source fallback (install Rust from https://rustup.rs/)"
  fi
  if ! command -v cc >/dev/null 2>&1 || ! cc --version >/dev/null 2>&1; then
    if [ "$os" = "macOS" ]; then
      fail "no compatible prebuilt release was available; run 'xcode-select --install' to enable the source fallback"
    fi
    fail "no compatible prebuilt release was available; a native C compiler is required for the source fallback"
  fi
}

prepare_compilation_target() {
  build_cache_used=0
  target_dir="$attempt_root/target"
  [ -n "$cache_root" ] || return 0

  build_root="$cache_root/build"
  target_cache="$build_root/$host_target"
  if [ -L "$build_root" ] || { [ -e "$build_root" ] && [ ! -d "$build_root" ]; }; then
    warn "compiled-dependency cache $build_root is not a regular directory; using disposable output"
    return 0
  fi
  mkdir -p "$build_root"
  if [ -L "$target_cache" ] || { [ -e "$target_cache" ] && [ ! -d "$target_cache" ]; }; then
    warn "compiled-dependency cache $target_cache is not a regular directory; using disposable output"
    return 0
  fi
  mkdir -p "$target_cache"

  desired_identity="$attempt_root/build-cache-identity"
  if ! toolchain_hash="$(file_sha256 "$source_dir/rust-toolchain.toml")" ||
    ! manifest_hash="$(file_sha256 "$source_dir/Cargo.toml")" ||
    ! lockfile_hash="$(file_sha256 "$source_dir/Cargo.lock")" ||
    ! v8_wrapper_hash="$(file_sha256 "$source_dir/scripts/cargo-with-v8.sh")"; then
    warn "sha256sum or shasum is unavailable; using disposable compilation output"
    return 0
  fi
  printf '%s\n' \
    'bettercodex-build-cache-v1' \
    "host=$host_target" \
    "toolchain=$toolchain_hash" \
    "manifest=$manifest_hash" \
    "lockfile=$lockfile_hash" \
    "v8-wrapper=$v8_wrapper_hash" >"$desired_identity"

  identity_path="$target_cache/identity"
  if ! cmp -s "$desired_identity" "$identity_path"; then
    step "Resetting incompatible compiled-dependency cache for $host_target"
    if [ -L "$target_cache/target" ]; then
      rm -f "$target_cache/target" || fail "could not replace the compiled-dependency cache"
    else
      rm -rf "$target_cache/target" || fail "could not reset the compiled-dependency cache"
    fi
    if [ -L "$identity_path" ]; then
      rm -f "$identity_path" || fail "could not replace the compiled-dependency cache identity"
    elif [ -d "$identity_path" ]; then
      rm -rf "$identity_path" || fail "could not replace the compiled-dependency cache identity"
    fi
    mv -f "$desired_identity" "$identity_path" ||
      fail "could not record the compiled-dependency cache identity"
  fi

  target_dir="$target_cache/target"
  if [ -L "$target_dir" ] || { [ -e "$target_dir" ] && [ ! -d "$target_dir" ]; }; then
    warn "compiled-dependency cache $target_dir is not a regular directory; using disposable output"
    target_dir="$attempt_root/target"
    return 0
  fi
  mkdir -p "$target_dir"
  build_cache_used=1
  step "Reusing compiled dependencies at $target_dir"
}

remove_bettercodex_outputs() {
  # The source archive has a new workspace path on every update. Remove only
  # this package's prior outputs so those path-specific files cannot accumulate;
  # registry and Git dependency artifacts remain available to Cargo.
  for build_output in \
    "$target_dir/release/bcodex" \
    "$target_dir/release/bcodex.d" \
    "$target_dir/release/deps"/bcodex-* \
    "$target_dir/release/deps"/bettercodex-* \
    "$target_dir/release/.fingerprint"/bettercodex-* \
    "$target_dir/release/incremental"/bcodex-* \
    "$target_dir/release/incremental"/bettercodex-*; do
    if [ -e "$build_output" ] || [ -L "$build_output" ]; then
      rm -rf "$build_output" || return 1
    fi
  done
  return 0
}

install_prebuilt_release() {
  release_descriptor="$1"
  prebuilt_tag="$(printf '%s\n' "$release_descriptor" | awk -F '|' '{ print $1 }')"
  prebuilt_version="$(printf '%s\n' "$release_descriptor" | awk -F '|' '{ print $2 }')"
  prebuilt_revision="$(printf '%s\n' "$release_descriptor" | awk -F '|' '{ print $3 }')"
  short_prebuilt="$(printf '%.12s' "$prebuilt_revision")"
  asset_name="bcodex-$host_target.gz"
  checksum_name="$asset_name.sha256"
  release_base="$GITHUB_RELEASE_ROOT/$repository/releases/download/$prebuilt_tag"
  attempt_root="$tmp_dir/prebuilt-$short_prebuilt"
  checksum_path="$attempt_root/$checksum_name"
  asset_path="$attempt_root/$asset_name"
  smoke_root="$attempt_root/smoke"
  mkdir -p "$attempt_root"

  step "Installing published BetterCodex $prebuilt_version ($short_prebuilt) for $os $arch"
  if download_release_file \
    "$release_base/$checksum_name" \
    "$checksum_path" \
    "BetterCodex checksum"; then
    :
  else
    prebuilt_result=$?
    if [ "$prebuilt_result" -eq 44 ]; then
      return 44
    fi
    fail "could not download the BetterCodex checksum for $host_target"
  fi

  expected_sha256="$(
    awk -v expected="$asset_name" '
      NR == 1 && NF == 2 && $2 == expected &&
        length($1) == 64 && $1 !~ /[^0-9a-f]/ {
        digest = $1
        next
      }
      { invalid = 1 }
      END {
        if (NR != 1 || invalid || digest == "") exit 1
        print digest
      }
    ' "$checksum_path" 2>/dev/null || true
  )"
  [ -n "$expected_sha256" ] || fail "GitHub returned an invalid checksum for $asset_name"

  step "Downloading the native BetterCodex executable"
  if download_release_file \
    "$release_base/$asset_name" \
    "$asset_path" \
    "BetterCodex release asset"; then
    :
  else
    prebuilt_result=$?
    if [ "$prebuilt_result" -eq 44 ]; then
      return 44
    fi
    fail "could not download the BetterCodex release asset for $host_target"
  fi

  asset_size="$(wc -c <"$asset_path" | awk '{ print $1 }')"
  case "$asset_size" in
    "" | *[!0-9]*) fail "could not determine the BetterCodex release asset size" ;;
  esac
  if [ "$asset_size" -le 0 ] || [ "$asset_size" -gt "$MAX_ASSET_BYTES" ]; then
    fail "BetterCodex release asset is empty or exceeds $MAX_ASSET_BYTES bytes"
  fi
  if ! actual_sha256="$(file_sha256 "$asset_path")"; then
    fail "sha256sum, shasum, or openssl is required to verify BetterCodex"
  fi
  [ "$actual_sha256" = "$expected_sha256" ] ||
    fail "$asset_name has SHA-256 $actual_sha256, expected $expected_sha256"

  staged_binary="$bin_dir/.bcodex-stage.$$"
  rm -f "$staged_binary"
  if ! (
    ulimit -f "$MAX_BINARY_BLOCKS"
    gzip -dc "$asset_path" >"$staged_binary"
  ); then
    fail "BetterCodex release asset could not be decompressed"
  fi
  staged_size="$(wc -c <"$staged_binary" | awk '{ print $1 }')"
  case "$staged_size" in
    "" | *[!0-9]*) fail "could not determine the staged BetterCodex size" ;;
  esac
  if [ "$staged_size" -le 0 ] || [ "$staged_size" -gt "$MAX_ASSET_BYTES" ]; then
    fail "staged BetterCodex executable is empty or exceeds $MAX_ASSET_BYTES bytes"
  fi
  chmod 0755 "$staged_binary"
  if ! version_output="$("$staged_binary" --version 2>/dev/null)"; then
    warn "the published $host_target executable cannot run on this system; using the source fallback"
    rm -f "$staged_binary"
    staged_binary=""
    return 45
  fi
  [ "$version_output" = "bcodex $prebuilt_version" ] ||
    fail "published binary did not report bcodex $prebuilt_version"
  revision_output="$("$staged_binary" --internal-source-revision 2>/dev/null || true)"
  [ "$revision_output" = "$prebuilt_revision" ] ||
    fail "published binary did not embed source revision $prebuilt_revision"

  step "Smoke-testing V8 and every embedded system resource"
  mkdir -p "$smoke_root/home" "$smoke_root/codex-home" "$smoke_root/bcodex-home" "$smoke_root/workspace"
  if ! smoke_output="$(
    cd "$smoke_root/workspace"
    HOME="$smoke_root/home" \
      CODEX_HOME="$smoke_root/codex-home" \
      BCODEX_HOME="$smoke_root/bcodex-home" \
      BCODEX_SKIP_UPDATE_CHECK=1 \
      "$staged_binary" --internal-install-smoke
  )"; then
    warn "the published $host_target executable failed its runtime smoke test; using the source fallback"
    rm -f "$staged_binary"
    staged_binary=""
    return 45
  fi
  [ "$smoke_output" = "bcodex $prebuilt_version install smoke passed" ] ||
    fail "published binary returned an unexpected install smoke result"

  installed_revision="$prebuilt_revision"
  installed_version="$prebuilt_version"
  return 0
}

remove_source_updater_caches() {
  [ -n "$cache_root" ] || return 0
  if [ -L "$cache_base" ]; then
    warn "not removing source-updater caches through symlink $cache_base"
    return 0
  fi
  if [ -L "$cache_root" ]; then
    warn "not removing symlinked source-updater cache root $cache_root"
    return 0
  fi
  for source_cache in "$cache_root/build" "$cache_root/cargo" "$cache_root/tmp"; do
    if [ -L "$source_cache" ]; then
      warn "not removing source-updater cache symlink $source_cache"
    elif [ -d "$source_cache" ]; then
      rm -rf "$source_cache" || return 1
    fi
  done
  return 0
}

install_temp_parent="${TMPDIR:-/tmp}"
install_temp_parent="${install_temp_parent%/}"
[ -n "$install_temp_parent" ] || install_temp_parent="/"
tmp_dir="$(mktemp -d "$install_temp_parent/bettercodex-install.XXXXXX")"
printf '%s\n' "$tmp_dir" >"$lock_dir/tmp"
printf '%s\n' "$install_temp_parent" >"$lock_dir/tmp-parent"

existing_install=0
if [ -f "$bin_path" ] || [ -L "$bin_path" ]; then
  existing_install=1
fi

release_descriptor=""
fallback_release_tag=""
fallback_release_version=""
if [ -n "$requested_release_tag" ]; then
  release_descriptor="$requested_release"
elif [ -z "$requested_revision" ]; then
  if release_descriptor="$(resolve_latest_release)"; then
    :
  else
    release_result=$?
    if [ "$release_result" -eq 44 ]; then
      warn "no published BetterCodex release exists yet; using the source fallback"
      release_descriptor=""
    else
      fail "could not resolve the latest published BetterCodex release"
    fi
  fi
fi

installed_revision=""
installed_version=""
if [ -n "$release_descriptor" ]; then
  release_attempt=1
  while [ "$release_attempt" -le "$MAX_RELEASE_ATTEMPTS" ]; do
    if install_prebuilt_release "$release_descriptor"; then
      prebuilt_result=0
    else
      prebuilt_result=$?
    fi
    if [ "$prebuilt_result" -eq 44 ] || [ "$prebuilt_result" -eq 45 ]; then
      requested_revision="$(printf '%s\n' "$release_descriptor" | awk -F '|' '{ print $3 }')"
      fallback_release_tag="$(printf '%s\n' "$release_descriptor" | awk -F '|' '{ print $1 }')"
      fallback_release_version="$(printf '%s\n' "$release_descriptor" | awk -F '|' '{ print $2 }')"
      warn "no compatible prebuilt asset is available for $host_target"
      warn "falling back to a local source build; this requires Rust, a C compiler, several gigabytes of disk, and substantially more time"
      installed_revision=""
      installed_version=""
      break
    fi

    if ! latest_release_descriptor="$(resolve_latest_release)"; then
      fail "could not verify the latest BetterCodex release after downloading"
    fi
    current_release_tag="$(printf '%s\n' "$release_descriptor" | awk -F '|' '{ print $1 }')"
    latest_release_tag="$(printf '%s\n' "$latest_release_descriptor" | awk -F '|' '{ print $1 }')"
    if [ "$latest_release_tag" != "$current_release_tag" ]; then
      rm -f "$staged_binary"
      staged_binary=""
      rm -rf "$attempt_root"
      if [ "$release_attempt" -ge "$MAX_RELEASE_ATTEMPTS" ]; then
        fail "BetterCodex releases kept advancing during all $MAX_RELEASE_ATTEMPTS install attempts"
      fi
      step "A newer BetterCodex release appeared while downloading; retrying"
      release_descriptor="$latest_release_descriptor"
      release_attempt=$((release_attempt + 1))
      continue
    fi

    mv -f "$staged_binary" "$bin_path"
    staged_binary=""
    [ "$("$bin_path" --version 2>/dev/null || true)" = "bcodex $installed_version" ] ||
      fail "installed prebuilt binary could not be verified"
    [ "$("$bin_path" --internal-source-revision 2>/dev/null || true)" = "$installed_revision" ] ||
      fail "installed prebuilt binary lost its source revision"
    install_mode="prebuilt"
    if ! remove_source_updater_caches; then
      warn "could not remove every retired source-updater cache"
    fi
    break
  done
fi

if [ -z "$installed_revision" ]; then
  require_source_build_tools
  install_mode="source"
  cargo_home="$tmp_dir/cargo-home"
  v8_cache_home="$tmp_dir/cache"
  if [ -n "$cache_root" ]; then
    cargo_home="$cache_root/cargo"
    v8_cache_home="$cache_base"
    step "Using dependency download cache at $cache_root"
  else
    warn "HOME and XDG_CACHE_HOME are unset; dependency downloads cannot be reused"
  fi
  mkdir -p "$cargo_home" "$v8_cache_home"

source_attempt=1
while [ "$source_attempt" -le "$MAX_SOURCE_ATTEMPTS" ]; do
  if [ -n "$requested_revision" ]; then
    resolved_commit="$requested_revision"
  else
    if ! resolved_commit="$(resolve_main_commit)"; then
      fail "could not resolve the current BetterCodex main commit"
    fi
  fi
  is_source_revision "$resolved_commit" ||
    fail "BetterCodex main did not resolve to a valid commit"

  short_commit="$(printf '%.12s' "$resolved_commit")"
  attempt_root="$tmp_dir/attempt-$source_attempt-$short_commit"
  archive_path="$attempt_root/source.tar.gz"
  source_dir="$attempt_root/source"
  compiler_tmp="$attempt_root/compiler-tmp"
  smoke_root="$attempt_root/smoke"
  mkdir -p "$source_dir" "$compiler_tmp"

  step "Installing BetterCodex main $short_commit for $os $arch"
  step "Downloading the immutable source snapshot"
  if ! download_source_archive "$archive_path" "$resolved_commit"; then
    fail "could not download public BetterCodex source commit $resolved_commit from $repository"
  fi
  [ -s "$archive_path" ] || fail "downloaded source archive is empty"
  if ! tar -xzf "$archive_path" -C "$source_dir" --strip-components=1; then
    fail "downloaded source archive could not be extracted"
  fi
  [ -f "$source_dir/Cargo.lock" ] || fail "source commit has no Cargo.lock"
  [ -f "$source_dir/rust-toolchain.toml" ] ||
    fail "source commit has no pinned Rust toolchain"
  [ -x "$source_dir/scripts/cargo-with-v8.sh" ] ||
    fail "source commit has no executable Cargo/V8 wrapper"

  expected_version="$(
    sed -n 's/^version = "\([^"]*\)"/\1/p' "$source_dir/Cargo.toml" 2>/dev/null |
      sed -n '1p'
  )"
  [ -n "$expected_version" ] || fail "source commit has no BetterCodex package version"
  if [ -n "$fallback_release_version" ] && [ "$expected_version" != "$fallback_release_version" ]; then
    fail "released source reports version $expected_version, expected $fallback_release_version"
  fi

  rust_toolchain="$(
    sed -n 's/^channel = "\([A-Za-z0-9._-]*\)"/\1/p' "$source_dir/rust-toolchain.toml" |
      sed -n '1p'
  )"
  [ -n "$rust_toolchain" ] || fail "source commit has no valid pinned Rust toolchain"
  step "Using the pinned Rust $rust_toolchain toolchain"
  if cargo_program="$(rustup which --toolchain "$rust_toolchain" cargo 2>/dev/null)" &&
    rustc_program="$(rustup which --toolchain "$rust_toolchain" rustc 2>/dev/null)" &&
    [ -x "$cargo_program" ] && [ -x "$rustc_program" ]; then
    :
  else
    temporary_rustup_home="$tmp_dir/rustup-home"
    mkdir -p "$temporary_rustup_home"
    step "Downloading missing Rust $rust_toolchain into the disposable install tree"
    RUSTUP_HOME="$temporary_rustup_home" \
      rustup toolchain install "$rust_toolchain" --profile minimal ||
      fail "could not download the pinned Rust $rust_toolchain toolchain"
    cargo_program="$(
      RUSTUP_HOME="$temporary_rustup_home" \
        rustup which --toolchain "$rust_toolchain" cargo
    )" ||
      fail "could not locate Cargo for Rust $rust_toolchain"
    rustc_program="$(
      RUSTUP_HOME="$temporary_rustup_home" \
        rustup which --toolchain "$rust_toolchain" rustc
    )" ||
      fail "could not locate rustc for Rust $rust_toolchain"
  fi
  [ -x "$cargo_program" ] || fail "pinned Cargo executable is unavailable"
  [ -x "$rustc_program" ] || fail "pinned rustc executable is unavailable"
  rust_toolchain_bin="$(dirname "$cargo_program")"

  prepare_compilation_target
  remove_bettercodex_outputs || fail "could not remove obsolete BetterCodex output"
  if [ "$build_cache_used" -eq 1 ]; then
    step "Compiling BetterCodex $expected_version with cached dependencies"
  else
    step "Compiling BetterCodex $expected_version in a disposable build directory"
  fi
  if ! (
    unset \
      BCODEX_SOURCE_REVISION \
      CARGO \
      CARGO_BUILD_JOBS \
      CARGO_BUILD_BUILD_DIR \
      CARGO_BUILD_TARGET \
      CARGO_BUILD_TARGET_DIR \
      CARGO_ENCODED_RUSTFLAGS \
      CARGO_HOME \
      CARGO_INCREMENTAL \
      CARGO_INSTALL_ROOT \
      CARGO_TARGET_DIR \
      RUSTC \
      RUSTC_WORKSPACE_WRAPPER \
      RUSTC_WRAPPER \
      RUSTFLAGS \
      RUSTUP_TOOLCHAIN \
      RUSTY_V8_ARCHIVE \
      RUSTY_V8_SRC_BINDING_PATH \
      V8_FROM_SOURCE \
      XDG_CACHE_HOME
    cd "$source_dir"
    BCODEX_SOURCE_REVISION="$resolved_commit" \
      CARGO="$cargo_program" \
      CARGO_HOME="$cargo_home" \
      CARGO_INCREMENTAL=0 \
      CARGO_TARGET_DIR="$target_dir" \
      PATH="$rust_toolchain_bin:$PATH" \
      RUSTC="$rustc_program" \
      TEMP="$compiler_tmp" \
      TMP="$compiler_tmp" \
      TMPDIR="$compiler_tmp" \
      XDG_CACHE_HOME="$v8_cache_home" \
      ./scripts/cargo-with-v8.sh build --release --locked --bin bcodex
  ); then
    fail "local BetterCodex compilation failed"
  fi

  built_binary="$target_dir/release/bcodex"
  [ -f "$built_binary" ] || fail "local build did not produce bcodex"
  version_output="$("$built_binary" --version 2>/dev/null || true)"
  [ "$version_output" = "bcodex $expected_version" ] ||
    fail "built binary did not report bcodex $expected_version"
  revision_output="$("$built_binary" --internal-source-revision 2>/dev/null || true)"
  [ "$revision_output" = "$resolved_commit" ] ||
    fail "built binary did not embed source revision $resolved_commit"

  step "Smoke-testing V8 and every embedded system resource"
  mkdir -p "$smoke_root/home" "$smoke_root/codex-home" "$smoke_root/bcodex-home" "$smoke_root/workspace"
  if ! smoke_output="$(
    cd "$smoke_root/workspace"
    HOME="$smoke_root/home" \
      CODEX_HOME="$smoke_root/codex-home" \
      BCODEX_HOME="$smoke_root/bcodex-home" \
      BCODEX_SKIP_UPDATE_CHECK=1 \
      "$built_binary" --internal-install-smoke
  )"; then
    fail "built binary failed its runtime and embedded-resource smoke test"
  fi
  [ "$smoke_output" = "bcodex $expected_version install smoke passed" ] ||
    fail "built binary returned an unexpected install smoke result"

  staged_binary="$bin_dir/.bcodex-stage.$$"
  rm -f "$staged_binary"
  cp "$built_binary" "$staged_binary"
  chmod 0755 "$staged_binary"
  cmp -s "$built_binary" "$staged_binary" ||
    fail "staged binary does not exactly match the verified build"
  [ "$("$staged_binary" --version 2>/dev/null || true)" = "bcodex $expected_version" ] ||
    fail "staged binary could not be verified"
  [ "$("$staged_binary" --internal-source-revision 2>/dev/null || true)" = "$resolved_commit" ] ||
    fail "staged binary lost its embedded source revision"

  latest_distribution_tag=""
  if [ -n "$fallback_release_tag" ]; then
    if ! latest_distribution="$(resolve_latest_release)"; then
      fail "could not verify the latest BetterCodex release after building"
    fi
    latest_distribution_tag="$(printf '%s\n' "$latest_distribution" | awk -F '|' '{ print $1 }')"
    latest_version="$(printf '%s\n' "$latest_distribution" | awk -F '|' '{ print $2 }')"
    latest_commit="$(printf '%s\n' "$latest_distribution" | awk -F '|' '{ print $3 }')"
  elif ! latest_commit="$(resolve_main_commit)"; then
    fail "could not verify BetterCodex main after building"
  fi
  is_source_revision "$latest_commit" ||
    fail "BetterCodex main did not resolve to a valid commit after building"
  if [ "$latest_commit" != "$resolved_commit" ] ||
    { [ -n "$fallback_release_tag" ] && [ "$latest_distribution_tag" != "$fallback_release_tag" ]; }; then
    rm -f "$staged_binary"
    staged_binary=""
    rm -rf "$attempt_root"
    if [ "$source_attempt" -ge "$MAX_SOURCE_ATTEMPTS" ]; then
      if [ -n "$fallback_release_tag" ]; then
        fail "BetterCodex releases kept advancing during all $MAX_SOURCE_ATTEMPTS build attempts"
      fi
      fail "BetterCodex main kept advancing during all $MAX_SOURCE_ATTEMPTS build attempts"
    fi
    latest_short="$(printf '%.12s' "$latest_commit")"
    if [ -n "$fallback_release_tag" ]; then
      step "A newer release replaced $short_commit with $latest_short while building; retrying with cached dependencies"
      fallback_release_tag="$latest_distribution_tag"
      fallback_release_version="$latest_version"
      requested_revision="$latest_commit"
    else
      step "Main advanced from $short_commit to $latest_short while building; retrying with cached dependencies"
      requested_revision=""
    fi
    source_attempt=$((source_attempt + 1))
    continue
  fi

  mv -f "$staged_binary" "$bin_path"
  staged_binary=""
  installed_revision="$resolved_commit"
  installed_version="$expected_version"
  break
done
fi

[ -n "$installed_revision" ] || fail "no BetterCodex source revision was installed"
[ "$("$bin_path" --version 2>/dev/null || true)" = "bcodex $installed_version" ] ||
  fail "installed binary did not retain version $installed_version"
[ "$("$bin_path" --internal-source-revision 2>/dev/null || true)" = "$installed_revision" ] ||
  fail "installed binary could not be verified"
if [ "$install_mode" = source ]; then
  cmp -s "$built_binary" "$bin_path" ||
    fail "installed binary does not exactly match the verified build"
  remove_bettercodex_outputs ||
    fail "could not remove BetterCodex-owned compilation output after installation"
fi

pick_profile() {
  case "$os:${SHELL:-}" in
    macOS:*/zsh) printf '%s\n' "$HOME/.zprofile" ;;
    macOS:*/bash) printf '%s\n' "$HOME/.bash_profile" ;;
    Linux:*/zsh) printf '%s\n' "$HOME/.zshrc" ;;
    Linux:*/bash) printf '%s\n' "$HOME/.bashrc" ;;
    *) printf '%s\n' "$HOME/.profile" ;;
  esac
}

path_action="already"
path_profile=""
path_needs_refresh=0
configure_path() {
  visible_bcodex="$(command -v bcodex 2>/dev/null || true)"
  if [ "$visible_bcodex" = "$bin_path" ]; then
    return
  fi
  path_needs_refresh=1
  if [ -n "$visible_bcodex" ]; then
    warn "$visible_bcodex currently shadows the installed command at $bin_path"
  fi

  if [ -z "${HOME:-}" ]; then
    path_action="unavailable"
    return
  fi

  path_profile="$(pick_profile)"
  begin_marker="# >>> bettercodex installer >>>"
  end_marker="# <<< bettercodex installer <<<"
  escaped_bin_dir="$(printf '%s' "$bin_dir" | sed "s/'/'\\\\''/g")"
  path_line="export PATH='$escaped_bin_dir':\"\$PATH\""

  if [ -f "$path_profile" ] && grep -F "$begin_marker" "$path_profile" >/dev/null 2>&1; then
    if grep -F "$path_line" "$path_profile" >/dev/null 2>&1; then
      path_action="configured"
      return
    fi
    if grep -F "$end_marker" "$path_profile" >/dev/null 2>&1; then
      if [ -L "$path_profile" ]; then
        warn "not replacing symlinked shell profile $path_profile; update its BetterCodex PATH block manually"
        path_action="unavailable"
        return
      fi
      rewritten_profile="$tmp_dir/profile"
      if ! cp -p "$path_profile" "$rewritten_profile"; then
        warn "could not stage an update for $path_profile"
        path_action="unavailable"
        return
      fi
      if ! awk -v begin="$begin_marker" -v end="$end_marker" -v line="$path_line" '
        BEGIN { in_block = 0; replaced = 0 }
        $0 == begin {
          if (!replaced) {
            print begin
            print line
            print end
            replaced = 1
          }
          in_block = 1
          next
        }
        in_block {
          if ($0 == end) in_block = 0
          next
        }
        { print }
        END { if (in_block) exit 1 }
      ' "$path_profile" >"$rewritten_profile"; then
        warn "could not rewrite the BetterCodex PATH block in $path_profile"
        path_action="unavailable"
        return
      fi
      if ! mv "$rewritten_profile" "$path_profile"; then
        warn "could not replace $path_profile with its updated PATH block"
        path_action="unavailable"
        return
      fi
      path_action="updated"
      return
    fi
    warn "the BetterCodex PATH block in $path_profile is incomplete; update it manually"
    path_action="unavailable"
    return
  fi

  if ! {
    printf '\n%s\n' "$begin_marker"
    printf '%s\n' "$path_line"
    printf '%s\n' "$end_marker"
  } >>"$path_profile"; then
    warn "could not add $bin_dir to PATH in $path_profile"
    path_action="unavailable"
    return
  fi
  path_action="added"
}

configure_path

cd /
if ! rm -rf "$tmp_dir"; then
  fail "BetterCodex was installed, but temporary build cleanup failed at $tmp_dir"
fi
tmp_dir=""
rm -f "$lock_dir/tmp"

short_installed="$(printf '%.12s' "$installed_revision")"
if [ "$existing_install" -eq 1 ]; then
  step "Updated bcodex $installed_version ($short_installed) at $bin_path"
  step "Restart BetterCodex to use the updated binary"
else
  step "Installed bcodex $installed_version ($short_installed) at $bin_path"
fi
if [ "$install_mode" = prebuilt ]; then
  step "Installed the verified native release without Rust, Cargo, or local compilation"
  if [ -n "$cache_root" ]; then
    step "Removed retired source-build caches; retained shared V8 downloads at $cache_root"
  fi
elif [ -n "$cache_root" ]; then
  if [ "$build_cache_used" -eq 1 ]; then
    step "Removed disposable source and BetterCodex scratch output; retained compiled dependencies at $cache_root/build/$host_target"
  else
    step "Removed disposable source and build output; retained dependency downloads at $cache_root"
  fi
else
  step "Removed the disposable install tree: source, dependency downloads, optional Rust toolchain, and build output"
fi

case "$path_action" in
  added | updated)
    step "PATH was configured in $path_profile; open a new terminal"
    step "For this terminal: export PATH=\"$bin_dir:\$PATH\""
    ;;
  configured)
    step "PATH is configured in $path_profile"
    if [ "$path_needs_refresh" -eq 1 ]; then
      step "For this terminal: export PATH=\"$bin_dir:\$PATH\""
    fi
    ;;
  unavailable) step "Add $bin_dir to PATH in your shell profile" ;;
esac

if [ "$existing_install" -eq 0 ]; then
  step "Run: bcodex login"
fi
