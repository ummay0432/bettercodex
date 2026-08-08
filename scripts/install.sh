#!/bin/sh

# Install the current integrated BetterCodex source with reusable dependency
# downloads and compiled dependencies. Source and compiler scratch space remain
# disposable. This script is also fetched and run by `bcodex update`, following
# upstream Codex's standalone-updater pattern.

set -eu

DEFAULT_REPOSITORY="ummay0432/bettercodex"
GITHUB_API_ROOT="https://api.github.com"
GITHUB_ARCHIVE_ROOT="https://codeload.github.com"
MAX_GITHUB_ATTEMPTS=3
MAX_SOURCE_ATTEMPTS=3

tmp_dir=""
staged_binary=""
lock_dir=""
lock_acquired=0
build_cache_used=0
target_dir=""

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

Resolves public BetterCodex main to an immutable commit, compiles that source
for this Mac or Linux computer, verifies the result, and atomically installs it.
Source, compiler scratch space, BetterCodex-owned outputs, and any installer-only
Rust toolchain created by the install are removed. Cargo dependency downloads,
verified V8 artifacts, and compatible compiled dependencies are reused.

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
require_command mktemp
require_command sed
require_command tar

if ! command -v rustup >/dev/null 2>&1; then
  fail "rustup is required; install Rust from https://rustup.rs/ and retry"
fi
if ! command -v cc >/dev/null 2>&1 || ! cc --version >/dev/null 2>&1; then
  if [ "$os" = "macOS" ]; then
    fail "Xcode Command Line Tools are required; run 'xcode-select --install', finish the installation, and retry"
  fi
  fail "a working native C compiler is required; install your system's C build tools and retry"
fi

mkdir -p "$bin_dir"
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

requested_revision="${BCODEX_INSTALL_REVISION:-}"
if [ -n "$requested_revision" ] && ! is_source_revision "$requested_revision"; then
  fail "BCODEX_INSTALL_REVISION must be a full 40-character commit ID"
fi

file_sha256() {
  if command -v sha256sum >/dev/null 2>&1; then
    sha256sum "$1" | awk '{ print $1 }'
  elif command -v shasum >/dev/null 2>&1; then
    shasum -a 256 "$1" | awk '{ print $1 }'
  else
    return 1
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

install_temp_parent="${TMPDIR:-/tmp}"
install_temp_parent="${install_temp_parent%/}"
[ -n "$install_temp_parent" ] || install_temp_parent="/"
tmp_dir="$(mktemp -d "$install_temp_parent/bettercodex-install.XXXXXX")"
printf '%s\n' "$tmp_dir" >"$lock_dir/tmp"
printf '%s\n' "$install_temp_parent" >"$lock_dir/tmp-parent"
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

existing_install=0
if [ -f "$bin_path" ] || [ -L "$bin_path" ]; then
  existing_install=1
fi

source_attempt=1
installed_revision=""
installed_version=""
while [ "$source_attempt" -le "$MAX_SOURCE_ATTEMPTS" ]; do
  if [ "$source_attempt" -eq 1 ] && [ -n "$requested_revision" ]; then
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

  if ! latest_commit="$(resolve_main_commit)"; then
    fail "could not verify BetterCodex main after building"
  fi
  is_source_revision "$latest_commit" ||
    fail "BetterCodex main did not resolve to a valid commit after building"
  if [ "$latest_commit" != "$resolved_commit" ]; then
    rm -f "$staged_binary"
    staged_binary=""
    rm -rf "$attempt_root"
    if [ "$source_attempt" -ge "$MAX_SOURCE_ATTEMPTS" ]; then
      fail "BetterCodex main kept advancing during all $MAX_SOURCE_ATTEMPTS build attempts"
    fi
    latest_short="$(printf '%.12s' "$latest_commit")"
    step "Main advanced from $short_commit to $latest_short while building; retrying with cached dependencies"
    requested_revision=""
    source_attempt=$((source_attempt + 1))
    continue
  fi

  mv -f "$staged_binary" "$bin_path"
  staged_binary=""
  installed_revision="$resolved_commit"
  installed_version="$expected_version"
  break
done

[ -n "$installed_revision" ] || fail "no BetterCodex source revision was installed"
cmp -s "$built_binary" "$bin_path" ||
  fail "installed binary does not exactly match the verified build"
[ "$("$bin_path" --internal-source-revision 2>/dev/null || true)" = "$installed_revision" ] ||
  fail "installed binary could not be verified"
remove_bettercodex_outputs ||
  fail "could not remove BetterCodex-owned compilation output after installation"

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
if [ -n "$cache_root" ]; then
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
