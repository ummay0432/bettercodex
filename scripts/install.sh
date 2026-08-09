#!/bin/sh

# Install one exact public bettercodex `main` revision from source. Source and
# compiler scratch space are disposable; verified downloads and Cargo's native
# compilation cache are reused across revisions.

set -eu

DEFAULT_REPOSITORY="ummay0432/bettercodex"
GITHUB_API_ROOT="https://api.github.com"
GITHUB_ARCHIVE_ROOT="https://codeload.github.com"
RUSTUP_INIT_URL="https://sh.rustup.rs"
MAX_GITHUB_ATTEMPTS=3
MAX_RUSTUP_ATTEMPTS=3
MAX_METADATA_BYTES=1048576
MAX_SOURCE_ARCHIVE_BYTES=134217728

tmp_dir=""
staged_binary=""
lock_dir=""
lock_acquired=0
build_cache_used=0
target_dir=""
rustup_program=""
rustup_home_override=""
managed_rustup_home=""

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

Resolves the exact source revision at public main, builds and verifies it, and
atomically installs it. Missing Rust and native build tools are bootstrapped
automatically when the host provides a supported installation path.

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
require_command curl
require_command dirname
require_command grep
require_command mktemp
require_command sed
require_command tr

mkdir -p "$bin_dir"
if [ -L "$bin_path" ]; then
  fail "refusing to replace symlinked bettercodex executable $bin_path"
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
          fail "another bettercodex install is already running (process $lock_pid)"
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
        fail "could not remove an orphaned bettercodex install tree"
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

if [ -n "$cache_root" ]; then
  if [ -L "$cache_base" ] || { [ -e "$cache_base" ] && [ ! -d "$cache_base" ]; }; then
    warn "cache home $cache_base is not a regular directory; using disposable caches"
    cache_base=""
    cache_root=""
  elif [ -L "$cache_root" ] || { [ -e "$cache_root" ] && [ ! -d "$cache_root" ]; }; then
    warn "bettercodex cache root $cache_root is not a regular directory; using disposable caches"
    cache_base=""
    cache_root=""
  fi
fi

cleanup_retired_updater_cache() {
  [ -n "$cache_root" ] || return 0

  # The retired cache used build/target directly. The current cache stores one
  # native Cargo target below build/<host>/target.
  legacy_build_root="$cache_root/build"
  if [ -L "$legacy_build_root" ]; then
    warn "not removing retired cache through symlink $legacy_build_root"
  elif [ -d "$legacy_build_root/target" ]; then
    step "Removing retired updater cache at $legacy_build_root/target"
    rm -rf "$legacy_build_root/target" ||
      fail "could not remove retired updater cache $legacy_build_root/target"
  fi

  for legacy_path in "$cache_root/tmp"; do
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
    --compressed \
    --connect-timeout 10 \
    --max-time 120 \
    --max-filesize "$1" \
    --user-agent bettercodex \
    --header 'Accept: application/vnd.github+json' \
    "$2"
}

resolve_main_commit() {
  github_attempt=1
  while [ "$github_attempt" -le "$MAX_GITHUB_ATTEMPTS" ]; do
    if github_response="$(
      github_get \
        "$MAX_METADATA_BYTES" \
        "$GITHUB_API_ROOT/repos/$repository/git/ref/heads/main" 2>/dev/null
    )"; then
      github_compact="$(printf '%s\n' "$github_response" | tr -d '[:space:]')"
      github_object="$(
        printf '%s\n' "$github_compact" |
          sed -n 's/^.*"object":{\([^}]*\)}.*$/\1/p'
      )"
      github_commit=""
      case "$github_compact" in
        *'"ref":"refs/heads/main"'*)
          case "$github_object" in
            *'"type":"commit"'*)
              github_commit="$(
                printf '%s\n' "$github_object" |
                  sed -n 's/^.*"sha":"\([0-9a-fA-F]\{40\}\)".*$/\1/p'
              )"
              ;;
          esac
          ;;
      esac
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
      "$MAX_SOURCE_ARCHIVE_BYTES" \
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
  elif command -v openssl >/dev/null 2>&1; then
    openssl dgst -sha256 "$1" | sed 's/^.*= //'
  else
    return 1
  fi
}

source_build_input_hash() {
  raw_hashes="$attempt_root/source-input-hashes"
  sorted_hashes="$attempt_root/source-input-hashes.sorted"

  set -- Cargo.toml Cargo.lock rust-toolchain.toml scripts/cargo-with-v8.sh src
  for optional_input in .cargo build.rs bundled-skills docs/evals prompts; do
    if [ -e "$source_dir/$optional_input" ] || [ -L "$source_dir/$optional_input" ]; then
      set -- "$@" "$optional_input"
    fi
  done

  if command -v sha256sum >/dev/null 2>&1; then
    (
      cd "$source_dir"
      find "$@" \( -type f -o -type l \) -exec sha256sum {} +
    ) >"$raw_hashes" || return 1
  elif command -v shasum >/dev/null 2>&1; then
    (
      cd "$source_dir"
      find "$@" \( -type f -o -type l \) -exec shasum -a 256 {} +
    ) >"$raw_hashes" || return 1
  elif command -v openssl >/dev/null 2>&1; then
    (
      cd "$source_dir"
      find "$@" \( -type f -o -type l \) -exec openssl dgst -sha256 {} +
    ) >"$raw_hashes" || return 1
  else
    return 1
  fi
  LC_ALL=C sort "$raw_hashes" >"$sorted_hashes" || return 1
  file_sha256 "$sorted_hashes"
}

require_source_utilities() {
  require_command find
  require_command sort
  require_command tar
}

native_build_tools_work() {
  command -v cc >/dev/null 2>&1 &&
    command -v c++ >/dev/null 2>&1 &&
    cc --version >/dev/null 2>&1 &&
    c++ --version >/dev/null 2>&1
}

run_with_system_privileges() {
  if [ "$(id -u 2>/dev/null || true)" = "0" ]; then
    "$@"
  elif command -v sudo >/dev/null 2>&1; then
    sudo "$@"
  else
    return 126
  fi
}

install_linux_build_tools() {
  if command -v apt-get >/dev/null 2>&1; then
    native_build_remedy="sudo apt-get update && sudo apt-get install -y build-essential"
    run_with_system_privileges apt-get update &&
      run_with_system_privileges apt-get install -y build-essential
  elif command -v dnf >/dev/null 2>&1; then
    native_build_remedy="sudo dnf -y group install 'Development Tools'"
    run_with_system_privileges dnf -y group install "Development Tools"
  elif command -v yum >/dev/null 2>&1; then
    native_build_remedy="sudo yum -y groupinstall 'Development Tools'"
    run_with_system_privileges yum -y groupinstall "Development Tools"
  elif command -v zypper >/dev/null 2>&1; then
    native_build_remedy="sudo zypper --non-interactive install --type pattern devel_basis"
    run_with_system_privileges \
      zypper --non-interactive install --type pattern devel_basis
  elif command -v pacman >/dev/null 2>&1; then
    native_build_remedy="sudo pacman -S --needed base-devel"
    run_with_system_privileges pacman -S --needed --noconfirm base-devel
  elif command -v xbps-install >/dev/null 2>&1; then
    native_build_remedy="sudo xbps-install -Sy base-devel"
    run_with_system_privileges xbps-install -Sy base-devel
  else
    native_build_remedy="install your system's C/C++ compiler, linker, and libc development headers"
    return 127
  fi
}

ensure_native_build_tools() {
  native_build_tools_work && return 0

  if [ "$os" = "macOS" ]; then
    require_command xcode-select
    step "Requesting Xcode Command Line Tools from macOS"
    if ! xcode-select --install >/dev/null 2>&1; then
      fail "macOS could not start the Xcode Command Line Tools installer; \
run 'xcode-select --install' or install them from Software Update, then retry"
    fi
    native_build_tools_work && return 0
    fail "Xcode Command Line Tools installation was requested; \
finish the macOS installation dialog, then rerun this command"
  fi

  step "Installing the native build tools required by bettercodex"
  native_build_remedy=""
  if ! install_linux_build_tools; then
    fail "automatic native build-tool installation failed; run: $native_build_remedy"
  fi
  native_build_tools_work ||
    fail "native build tools were installed but no working C/C++ compiler is available; run: $native_build_remedy"
}

download_rustup_init() {
  rustup_installer="$tmp_dir/rustup-init.sh"
  rustup_attempt=1
  while [ "$rustup_attempt" -le "$MAX_RUSTUP_ATTEMPTS" ]; do
    rm -f "$rustup_installer"
    if curl \
      --proto '=https' \
      --tlsv1.2 \
      --fail \
      --silent \
      --show-error \
      --location \
      --connect-timeout 10 \
      --max-time 120 \
      --max-filesize "$MAX_METADATA_BYTES" \
      --output "$rustup_installer" \
      "$RUSTUP_INIT_URL" &&
      [ -s "$rustup_installer" ]; then
      return 0
    fi
    if [ "$rustup_attempt" -lt "$MAX_RUSTUP_ATTEMPTS" ]; then
      warn "rustup bootstrap download failed; retrying ($((rustup_attempt + 1))/$MAX_RUSTUP_ATTEMPTS)"
      sleep "$rustup_attempt"
    fi
    rustup_attempt=$((rustup_attempt + 1))
  done
  rm -f "$rustup_installer"
  return 1
}

discover_rustup() {
  rustup_program="$(command -v rustup 2>/dev/null || true)"
  [ -z "$rustup_program" ] || return 0

  if [ -n "${CARGO_HOME:-}" ]; then
    case "$CARGO_HOME" in
      /*)
        if [ -x "$CARGO_HOME/bin/rustup" ]; then
          rustup_program="$CARGO_HOME/bin/rustup"
          step "Using rustup at $rustup_program even though it is not on PATH"
          return 0
        fi
        ;;
    esac
  fi
  if [ -n "${HOME:-}" ] && [ -x "$HOME/.cargo/bin/rustup" ]; then
    rustup_program="$HOME/.cargo/bin/rustup"
    step "Using rustup at $rustup_program even though it is not on PATH"
    return 0
  fi
  if [ -x "$cargo_home/bin/rustup" ]; then
    rustup_program="$cargo_home/bin/rustup"
    rustup_home_override="$managed_rustup_home"
    step "Reusing the installer-managed rustup at $rustup_program"
  fi
}

bootstrap_rustup() {
  step "Installing rustup for the pinned bettercodex toolchain"
  download_rustup_init || fail "could not download the official rustup installer from $RUSTUP_INIT_URL"
  mkdir -p "$cargo_home" "$managed_rustup_home"
  if ! CARGO_HOME="$cargo_home" \
    RUSTUP_HOME="$managed_rustup_home" \
    /bin/sh "$rustup_installer" \
      -y --no-modify-path --profile minimal --default-toolchain none; then
    fail "the official rustup installer failed"
  fi
  rustup_program="$cargo_home/bin/rustup"
  rustup_home_override="$managed_rustup_home"
  [ -x "$rustup_program" ] || fail "rustup installation did not produce $rustup_program"
}

run_rustup() {
  if [ -n "$rustup_home_override" ]; then
    RUSTUP_HOME="$rustup_home_override" "$rustup_program" "$@"
  else
    "$rustup_program" "$@"
  fi
}

locate_pinned_rust_tools() {
  cargo_program="$(run_rustup which --toolchain "$rust_toolchain" cargo 2>/dev/null)" &&
    rustc_program="$(run_rustup which --toolchain "$rust_toolchain" rustc 2>/dev/null)" &&
    [ -x "$cargo_program" ] &&
    [ -x "$rustc_program" ]
}

prepare_compilation_target() {
  build_cache_used=0
  target_dir="$attempt_root/target"
  [ -n "$cache_root" ] || return 0

  build_root="$cache_root/build"
  target_cache="$build_root/$host_target"
  if [ -L "$build_root" ] || { [ -e "$build_root" ] && [ ! -d "$build_root" ]; }; then
    warn "Cargo compilation cache $build_root is not a regular directory; using disposable output"
    return 0
  fi
  mkdir -p "$build_root"
  if [ -L "$target_cache" ] || { [ -e "$target_cache" ] && [ ! -d "$target_cache" ]; }; then
    warn "Cargo compilation cache $target_cache is not a regular directory; using disposable output"
    return 0
  fi
  mkdir -p "$target_cache"

  # Older installers discarded this entire target whenever Cargo.toml or
  # Cargo.lock changed. Cargo already fingerprints the compiler, profile,
  # manifest, lockfile, features, build scripts, and source inputs at artifact
  # granularity. Retire the coarse identity while preserving every artifact
  # Cargo can still use; the content hash below closes its source-mtime gap.
  identity_path="$target_cache/identity"
  if [ -L "$identity_path" ] || [ -f "$identity_path" ]; then
    rm -f "$identity_path" || fail "could not retire the obsolete compilation-cache identity"
  elif [ -e "$identity_path" ]; then
    warn "obsolete compilation-cache identity $identity_path is not a regular file; leaving it untouched"
  fi

  target_dir="$target_cache/target"
  if [ -L "$target_dir" ] || { [ -e "$target_dir" ] && [ ! -d "$target_dir" ]; }; then
    warn "Cargo compilation cache $target_dir is not a regular directory; using disposable output"
    target_dir="$attempt_root/target"
    return 0
  fi
  mkdir -p "$target_dir"
  build_cache_used=1
  step "Reusing Cargo compilation cache at $target_dir"
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

installed_revision=""
installed_version=""
require_source_utilities
cargo_home="$tmp_dir/cargo-home"
v8_cache_home="$tmp_dir/cache"
managed_rustup_home="$tmp_dir/rustup-home"
if [ -n "$cache_root" ]; then
  cargo_home="$cache_root/cargo"
  v8_cache_home="$cache_base"
  managed_rustup_home="$cache_root/rustup"
  step "Using dependency download cache at $cache_root"
else
  warn "HOME and XDG_CACHE_HOME are unset; dependency downloads cannot be reused"
fi
mkdir -p "$cargo_home" "$v8_cache_home"

if [ -n "$requested_revision" ]; then
  resolved_commit="$requested_revision"
else
  if ! resolved_commit="$(resolve_main_commit)"; then
    fail "could not resolve the current bettercodex main commit"
  fi
fi
is_source_revision "$resolved_commit" ||
  fail "bettercodex main did not resolve to a valid commit"

short_commit="$(printf '%.12s' "$resolved_commit")"
attempt_root="$tmp_dir/source-$short_commit"
archive_path="$attempt_root/source.tar.gz"
source_dir="$attempt_root/source"
compiler_tmp="$attempt_root/compiler-tmp"
smoke_root="$attempt_root/smoke"
mkdir -p "$source_dir" "$compiler_tmp"

step "Installing bettercodex main $short_commit for $os $arch"
step "Downloading the immutable source snapshot"
if ! download_source_archive "$archive_path" "$resolved_commit"; then
  fail "could not download public bettercodex source commit $resolved_commit from $repository"
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
[ -n "$expected_version" ] || fail "source commit has no bettercodex package version"
rust_toolchain="$(
  sed -n 's/^channel = "\([A-Za-z0-9._-]*\)"/\1/p' "$source_dir/rust-toolchain.toml" |
    sed -n '1p'
)"
[ -n "$rust_toolchain" ] || fail "source commit has no valid pinned Rust toolchain"

ensure_native_build_tools
if [ "$os" = "macOS" ]; then
  require_command codesign
fi

discover_rustup
if [ -z "$rustup_program" ]; then
  bootstrap_rustup
fi

step "Using the pinned Rust $rust_toolchain toolchain"
if ! locate_pinned_rust_tools; then
  rustup_home_override="$managed_rustup_home"
  mkdir -p "$managed_rustup_home"
  if ! locate_pinned_rust_tools; then
    if [ -n "$cache_root" ]; then
      step "Caching missing Rust $rust_toolchain at $managed_rustup_home"
    else
      step "Downloading missing Rust $rust_toolchain into the disposable install tree"
    fi
    run_rustup toolchain install "$rust_toolchain" --profile minimal ||
      fail "could not download the pinned Rust $rust_toolchain toolchain"
    locate_pinned_rust_tools ||
      fail "could not locate Cargo and rustc for Rust $rust_toolchain"
  fi
fi
[ -x "$cargo_program" ] || fail "pinned Cargo executable is unavailable"
[ -x "$rustc_program" ] || fail "pinned rustc executable is unavailable"
rust_toolchain_bin="$(dirname "$cargo_program")"

prepare_compilation_target
if ! build_input_hash="$(source_build_input_hash)" ||
  ! printf '%s\n' "$build_input_hash" | grep -Eq '^[0-9a-fA-F]{64}$'; then
  # A revision-derived key still guarantees a rebuild on every update when the
  # host has no supported SHA-256 utility. Normal hosts take the byte-accurate
  # path above and keep non-build revisions fresh.
  build_input_hash="$resolved_commit$(printf '%.24s' "$resolved_commit")"
  warn "could not calculate a SHA-256 build-input hash; using conservative per-revision freshness"
fi
if [ "$build_cache_used" -eq 1 ]; then
  step "Compiling bettercodex $expected_version with the warm Cargo cache"
else
  step "Compiling bettercodex $expected_version in a disposable build directory"
fi
if ! (
  unset \
    BCODEX_BUILD_INPUT_HASH \
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
  BCODEX_BUILD_INPUT_HASH="$build_input_hash" \
    CARGO="$cargo_program" \
    CARGO_HOME="$cargo_home" \
    CARGO_INCREMENTAL=1 \
    CARGO_TARGET_DIR="$target_dir" \
    PATH="$rust_toolchain_bin:$PATH" \
    RUSTC="$rustc_program" \
    TEMP="$compiler_tmp" \
    TMP="$compiler_tmp" \
    TMPDIR="$compiler_tmp" \
    XDG_CACHE_HOME="$v8_cache_home" \
    ./scripts/cargo-with-v8.sh build --release --locked --bin bcodex
); then
  fail "local bettercodex compilation failed"
fi

built_binary="$target_dir/release/bcodex"
[ -f "$built_binary" ] || fail "local build did not produce bcodex"
version_output="$("$built_binary" --version 2>/dev/null || true)"
[ "$version_output" = "bcodex $expected_version" ] ||
  fail "built binary did not report bcodex $expected_version"

staged_binary="$bin_dir/.bcodex-stage.$$"
rm -f "$staged_binary"
if ! "$built_binary" --internal-install-stage \
  "$staged_binary" "$resolved_commit" "$build_input_hash"; then
  fail "built binary could not stage source revision $resolved_commit"
fi
chmod 0755 "$staged_binary"
if [ "$os" = "macOS" ]; then
  codesign --force --sign - "$staged_binary" >/dev/null 2>&1 ||
    fail "could not apply the required macOS ad-hoc signature to the staged binary"
  codesign --verify --strict "$staged_binary" >/dev/null 2>&1 ||
    fail "staged binary has an invalid macOS code signature"
fi
[ "$("$staged_binary" --version 2>/dev/null || true)" = "bcodex $expected_version" ] ||
  fail "staged binary could not be verified"
[ "$("$staged_binary" --internal-source-revision 2>/dev/null || true)" = "$resolved_commit" ] ||
  fail "staged binary lost its embedded source revision"

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
  fail "staged binary failed its runtime and embedded-resource smoke test"
fi
[ "$smoke_output" = "bcodex $expected_version install smoke passed" ] ||
  fail "staged binary returned an unexpected install smoke result"

mv -f "$staged_binary" "$bin_path"
staged_binary=""
installed_revision="$resolved_commit"
installed_version="$expected_version"

[ -n "$installed_revision" ] || fail "no bettercodex source revision was installed"
[ "$("$bin_path" --version 2>/dev/null || true)" = "bcodex $installed_version" ] ||
  fail "installed binary did not retain version $installed_version"
[ "$("$bin_path" --internal-source-revision 2>/dev/null || true)" = "$installed_revision" ] ||
  fail "installed binary could not be verified"

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
        warn "not replacing symlinked shell profile $path_profile; update its bettercodex PATH block manually"
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
        warn "could not rewrite the bettercodex PATH block in $path_profile"
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
    warn "the bettercodex PATH block in $path_profile is incomplete; update it manually"
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
  fail "bettercodex was installed, but temporary build cleanup failed at $tmp_dir"
fi
tmp_dir=""
rm -f "$lock_dir/tmp"

short_installed="$(printf '%.12s' "$installed_revision")"
if [ "$existing_install" -eq 1 ]; then
  step "Updated bcodex $installed_version ($short_installed) at $bin_path"
  step "Restart bettercodex to use the updated binary"
else
  step "Installed bcodex $installed_version ($short_installed) at $bin_path"
fi
if [ -n "$cache_root" ]; then
  if [ "$build_cache_used" -eq 1 ]; then
    if [ -n "$rustup_home_override" ]; then
      step "Removed disposable source and compiler scratch; retained warm Rust and Cargo caches under $cache_root"
    else
      step "Removed disposable source and compiler scratch; \
retained the warm Cargo cache at $cache_root/build/$host_target"
    fi
  else
    if [ -n "$rustup_home_override" ]; then
      step "Removed disposable source and build output; retained Rust and dependency downloads at $cache_root"
    else
      step "Removed disposable source and build output; retained dependency downloads at $cache_root"
    fi
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
