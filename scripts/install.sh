#!/bin/sh

set -eu

REPOSITORY="${BCODEX_REPOSITORY:-ummay0432/bettercodex}"
RELEASE="${BCODEX_RELEASE:-latest}"
BIN_DIR="${BCODEX_INSTALL_DIR:-$HOME/.local/bin}"
BIN_PATH="$BIN_DIR/bcodex"

path_action="already"
path_profile=""
tmp_dir=""
tmp_binary=""

step() {
  printf '==> %s\n' "$1"
}

fail() {
  printf 'bettercodex installer: %s\n' "$1" >&2
  exit 1
}

usage() {
  cat <<EOF
Usage: install.sh [--release VERSION]

Downloads a private bettercodex release with the authenticated GitHub CLI.

Environment:
  BCODEX_RELEASE      Version to install, such as v0.1.0 (default: latest).
  BCODEX_INSTALL_DIR  Binary directory (default: ~/.local/bin).
  BCODEX_REPOSITORY   GitHub repository (default: $REPOSITORY).
EOF
}

cleanup() {
  if [ -n "$tmp_binary" ]; then
    rm -f "$tmp_binary"
  fi
  if [ -n "$tmp_dir" ]; then
    rm -rf "$tmp_dir"
  fi
}

trap cleanup EXIT HUP INT TERM

while [ "$#" -gt 0 ]; do
  case "$1" in
    --release)
      [ "$#" -ge 2 ] || fail "--release requires a version"
      RELEASE="$2"
      shift
      ;;
    -h | --help)
      usage
      exit 0
      ;;
    *)
      fail "unknown argument: $1"
      ;;
  esac
  shift
done

case "$BIN_DIR" in
  /*) ;;
  *) fail "BCODEX_INSTALL_DIR must be an absolute path" ;;
esac

case "$BIN_DIR" in
  *'
'*) fail "BCODEX_INSTALL_DIR must not contain a newline" ;;
esac

require_command() {
  command -v "$1" >/dev/null 2>&1 || fail "$1 is required"
}

require_command awk
require_command gh
require_command grep
require_command mktemp
require_command sed
require_command tar

if ! gh api user >/dev/null 2>&1; then
  fail "GitHub CLI is not signed in; run 'gh auth login' and retry"
fi

if ! gh api "repos/$REPOSITORY" >/dev/null 2>&1; then
  fail "your GitHub account cannot access $REPOSITORY; accept the repository invitation and retry"
fi

case "$RELEASE" in
  latest) ;;
  v*) ;;
  *) RELEASE="v$RELEASE" ;;
esac

if [ "$RELEASE" = "latest" ]; then
  if ! resolved_tag="$(gh release view --repo "$REPOSITORY" --json tagName --jq .tagName 2>/dev/null)"; then
    fail "no published bettercodex release is available"
  fi
else
  if ! resolved_tag="$(gh release view "$RELEASE" --repo "$REPOSITORY" --json tagName --jq .tagName 2>/dev/null)"; then
    fail "release $RELEASE does not exist or is not accessible"
  fi
fi

if ! printf '%s\n' "$resolved_tag" | grep -Eq '^v[0-9]+\.[0-9]+\.[0-9]+$'; then
  fail "release tag $resolved_tag is not a supported stable version"
fi

case "$(uname -s)" in
  Darwin) os="darwin" ;;
  Linux) os="linux" ;;
  *) fail "only macOS and Linux are supported" ;;
esac

case "$(uname -m)" in
  x86_64 | amd64) arch="x86_64" ;;
  arm64 | aarch64) arch="aarch64" ;;
  *) fail "unsupported architecture: $(uname -m)" ;;
esac

if [ "$os" = "darwin" ] && [ "$arch" = "x86_64" ]; then
  if [ "$(sysctl -n sysctl.proc_translated 2>/dev/null || true)" = "1" ]; then
    arch="aarch64"
  fi
fi

if [ "$os" = "darwin" ]; then
  target="$arch-apple-darwin"
  platform_label="macOS $arch"
else
  target="$arch-unknown-linux-gnu"
  platform_label="Linux $arch"
fi

asset="bcodex-$target.tar.gz"
tmp_dir="$(mktemp -d "${TMPDIR:-/tmp}/bettercodex-install.XXXXXX")"
archive_path="$tmp_dir/$asset"
manifest_path="$tmp_dir/SHA256SUMS"

step "Installing bettercodex ${resolved_tag#v} for $platform_label"
step "Downloading private release from $REPOSITORY"
if ! gh release download "$resolved_tag" \
  --repo "$REPOSITORY" \
  --pattern "$asset" \
  --pattern SHA256SUMS \
  --dir "$tmp_dir"; then
  fail "release $resolved_tag does not contain $asset"
fi

[ -f "$archive_path" ] || fail "downloaded release is missing $asset"
[ -f "$manifest_path" ] || fail "downloaded release is missing SHA256SUMS"

expected_digest="$(awk -v asset="$asset" '
  $2 == asset && length($1) == 64 && $1 !~ /[^0-9a-fA-F]/ {
    print tolower($1)
    found = 1
    exit
  }
  END { if (!found) exit 1 }
' "$manifest_path" 2>/dev/null || true)"
[ -n "$expected_digest" ] || fail "SHA256SUMS has no valid digest for $asset"

file_sha256() {
  if command -v sha256sum >/dev/null 2>&1; then
    sha256sum "$1" | awk '{print $1}'
  elif command -v shasum >/dev/null 2>&1; then
    shasum -a 256 "$1" | awk '{print $1}'
  elif command -v openssl >/dev/null 2>&1; then
    openssl dgst -sha256 "$1" | sed 's/^.*= //'
  else
    fail "sha256sum, shasum, or openssl is required"
  fi
}

actual_digest="$(file_sha256 "$archive_path")"
[ "$actual_digest" = "$expected_digest" ] || fail "downloaded archive failed SHA-256 verification"

archive_entries="$(tar -tzf "$archive_path" | sed 's#^\./##')"
[ "$archive_entries" = "bcodex" ] || fail "release archive has an unexpected layout"

extract_dir="$tmp_dir/extract"
mkdir -p "$extract_dir"
tar -xzf "$archive_path" -C "$extract_dir"
extracted_binary="$extract_dir/bcodex"
[ -f "$extracted_binary" ] || fail "release archive does not contain bcodex"
chmod 0755 "$extracted_binary"

expected_version="${resolved_tag#v}"
version_output="$("$extracted_binary" --version 2>/dev/null || true)"
[ "$version_output" = "bcodex $expected_version" ] || fail "downloaded binary did not report bcodex $expected_version"

mkdir -p "$BIN_DIR"
tmp_binary="$BIN_DIR/.bcodex.$$"
cp "$extracted_binary" "$tmp_binary"
chmod 0755 "$tmp_binary"
mv -f "$tmp_binary" "$BIN_PATH"
tmp_binary=""

installed_version="$("$BIN_PATH" --version 2>/dev/null || true)"
[ "$installed_version" = "bcodex $expected_version" ] || fail "installed binary could not be verified"

pick_profile() {
  case "$os:${SHELL:-}" in
    darwin:*/zsh) printf '%s\n' "$HOME/.zprofile" ;;
    darwin:*/bash) printf '%s\n' "$HOME/.bash_profile" ;;
    linux:*/zsh) printf '%s\n' "$HOME/.zshrc" ;;
    linux:*/bash) printf '%s\n' "$HOME/.bashrc" ;;
    *) printf '%s\n' "$HOME/.profile" ;;
  esac
}

configure_path() {
  case ":$PATH:" in
    *":$BIN_DIR:"*) return ;;
  esac

  path_profile="$(pick_profile)"
  begin_marker="# >>> bettercodex installer >>>"
  end_marker="# <<< bettercodex installer <<<"
  escaped_bin_dir="$(printf '%s' "$BIN_DIR" | sed "s/'/'\\\\''/g")"
  path_line="export PATH='$escaped_bin_dir':\"\$PATH\""

  if [ -f "$path_profile" ] && grep -F "$begin_marker" "$path_profile" >/dev/null 2>&1; then
    if grep -F "$path_line" "$path_profile" >/dev/null 2>&1; then
      path_action="configured"
      return
    fi

    if grep -F "$end_marker" "$path_profile" >/dev/null 2>&1; then
      rewritten_profile="$tmp_dir/profile"
      awk -v begin="$begin_marker" -v end="$end_marker" -v line="$path_line" '
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
      ' "$path_profile" >"$rewritten_profile"
      mv "$rewritten_profile" "$path_profile"
      path_action="updated"
      return
    fi
  fi

  {
    printf '\n%s\n' "$begin_marker"
    printf '%s\n' "$path_line"
    printf '%s\n' "$end_marker"
  } >>"$path_profile"
  path_action="added"
}

configure_path

step "Installed $installed_version at $BIN_PATH"
case "$path_action" in
  added | updated)
    step "Open a new terminal and run: bcodex login"
    step "For this terminal: export PATH=\"$BIN_DIR:\$PATH\" && bcodex login"
    step "PATH was configured in $path_profile"
    ;;
  configured)
    step "Open a new terminal and run: bcodex login"
    step "PATH is configured in $path_profile"
    ;;
  *)
    step "Run: bcodex login"
    ;;
esac
step "After signing in, run bcodex from a project directory"
