#!/bin/sh

# Adapted from OpenAI Codex's setup-rusty-v8 action and package V8 resolver.
# Bettercodex runs code mode in-process, so every Cargo build needs the
# sandbox-enabled archive and generated binding published by OpenAI Codex.

set -eu

V8_VERSION="150.4.0"
V8_PROFILE="ptrcomp_sandbox_release"
V8_RELEASE_URL="https://github.com/openai/codex/releases/download/rusty-v8-v${V8_VERSION}"

fail() {
  printf 'bettercodex Cargo setup: %s\n' "$1" >&2
  exit 1
}

script_dir="$(CDPATH= cd -P "$(dirname "$0")" && pwd)"
repository_root="$(dirname "$script_dir")"
cargo_command="${CARGO:-cargo}"
locked_v8_version="$(
  awk '
    $0 == "name = \"v8\"" {
      if ((getline version_line) <= 0 || version_line !~ /^version = "/) exit 2
      sub(/^version = "/, "", version_line)
      sub(/"$/, "", version_line)
      print version_line
    }
  ' "$repository_root/Cargo.lock"
)"
[ "$locked_v8_version" = "$V8_VERSION" ] ||
  fail "Cargo.lock resolves V8 ${locked_v8_version:-unknown}, but the verified artifact pair is $V8_VERSION"

case "${V8_FROM_SOURCE:-}" in
  1 | true | yes) exec "$cargo_command" "$@" ;;
esac

archive_override="${RUSTY_V8_ARCHIVE:-}"
binding_override="${RUSTY_V8_SRC_BINDING_PATH:-}"
if [ -n "$archive_override" ] || [ -n "$binding_override" ]; then
  [ -n "$archive_override" ] && [ -n "$binding_override" ] ||
    fail "RUSTY_V8_ARCHIVE and RUSTY_V8_SRC_BINDING_PATH must be set together"
  exec "$cargo_command" "$@"
fi

host_target="$(rustc -vV | sed -n 's/^host: //p')"
case "$host_target" in
  x86_64-unknown-linux-gnu)
    archive_sha256="a35c75d1f26e6a983885a45b33490a4ebe54f05050568b32b89cfb421b30b583"
    binding_sha256="7727826ae479bdb645e807239fb12d1f8e2e23de7a6cf16f5ee592690d1d8506"
    ;;
  aarch64-unknown-linux-gnu)
    archive_sha256="d1517eed405468537029b005d5fe997ec74d5c8d351f916b3a6df20b7d2811ba"
    binding_sha256="7727826ae479bdb645e807239fb12d1f8e2e23de7a6cf16f5ee592690d1d8506"
    ;;
  x86_64-apple-darwin)
    archive_sha256="e0d9bb64e8b3a034c2930c83972f3f35760211148342fa0407b38250ef330856"
    binding_sha256="ca5adf0cf89c9a70ad460ae73648b2fe89b74aa113b3cb7f757b6a02b758394f"
    ;;
  aarch64-apple-darwin)
    archive_sha256="00adbb48798848c77550441c68673a5e8529b8e1b73eabcdee232cb39b40f4a1"
    binding_sha256="ca5adf0cf89c9a70ad460ae73648b2fe89b74aa113b3cb7f757b6a02b758394f"
    ;;
  *) fail "no verified sandboxed V8 artifact is available for Rust host $host_target" ;;
esac

sha256_file() {
  if command -v sha256sum >/dev/null 2>&1; then
    sha256sum "$1" | awk '{ print $1 }'
  elif command -v shasum >/dev/null 2>&1; then
    shasum -a 256 "$1" | awk '{ print $1 }'
  else
    fail "sha256sum or shasum is required"
  fi
}

temporary_file=""
cleanup() {
  if [ -n "$temporary_file" ]; then
    rm -f "$temporary_file"
  fi
}
trap cleanup EXIT
trap 'exit 1' HUP INT TERM

download_verified() {
  artifact_url="$1"
  artifact_path="$2"
  expected_sha256="$3"
  if [ -f "$artifact_path" ] && [ "$(sha256_file "$artifact_path")" = "$expected_sha256" ]; then
    return
  fi

  command -v curl >/dev/null 2>&1 || fail "curl is required to download sandboxed V8"
  mkdir -p "$(dirname "$artifact_path")"
  temporary_file="${artifact_path}.tmp.$$"
  rm -f "$temporary_file"
  printf 'Downloading %s\n' "$(basename "$artifact_path")" >&2
  curl -fsSL --retry 3 "$artifact_url" -o "$temporary_file"
  actual_sha256="$(sha256_file "$temporary_file")"
  if [ "$actual_sha256" != "$expected_sha256" ]; then
    fail "$(basename "$artifact_path") has SHA-256 $actual_sha256, expected $expected_sha256"
  fi
  mv -f "$temporary_file" "$artifact_path"
  temporary_file=""
}

cache_root="${XDG_CACHE_HOME:-$HOME/.cache}/bettercodex"
artifact_dir="$cache_root/rusty-v8-${V8_VERSION}-${host_target}"
archive_name="librusty_v8_${V8_PROFILE}_${host_target}.a.gz"
binding_name="src_binding_${V8_PROFILE}_${host_target}.rs"
archive_path="$artifact_dir/$archive_name"
binding_path="$artifact_dir/$binding_name"

download_verified "$V8_RELEASE_URL/$archive_name" "$archive_path" "$archive_sha256"
download_verified "$V8_RELEASE_URL/$binding_name" "$binding_path" "$binding_sha256"

RUSTY_V8_ARCHIVE="$archive_path"
RUSTY_V8_SRC_BINDING_PATH="$binding_path"
export RUSTY_V8_ARCHIVE RUSTY_V8_SRC_BINDING_PATH
exec "$cargo_command" "$@"
