#!/bin/sh

set -eu

REPOSITORY="${BCODEX_REPOSITORY:-ummay0432/bettercodex}"
RELEASE="${BCODEX_RELEASE:-latest}"
BIN_DIR="${BCODEX_INSTALL_DIR:-$HOME/.local/bin}"
BUILD_DIR="${BCODEX_BUILD_DIR:-${XDG_CACHE_HOME:-$HOME/.cache}/bettercodex/build}"
BIN_PATH="$BIN_DIR/bcodex"

existing_install=0
if [ -f "$BIN_PATH" ]; then
  existing_install=1
fi
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

Downloads tagged bettercodex source from the private repository and compiles it
for this machine.

Environment:
  BCODEX_RELEASE      Version to install, such as v1.2.3 (default: latest).
  BCODEX_INSTALL_DIR  Binary directory (default: ~/.local/bin).
  BCODEX_BUILD_DIR    Persistent Cargo build cache (default: ~/.cache/bettercodex/build).
  BCODEX_REPOSITORY   GitHub repository (default: $REPOSITORY).

Requires an authenticated GitHub CLI, Python 3, Rust through rustup, and a
native C toolchain. The checked-in Rust toolchain and Cargo.lock select exact
versions.
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

validate_absolute_path "$BIN_DIR" BCODEX_INSTALL_DIR
validate_absolute_path "$BUILD_DIR" BCODEX_BUILD_DIR

require_command() {
  command -v "$1" >/dev/null 2>&1 || fail "$1 is required"
}

require_command awk
require_command cargo
require_command cc
require_command gh
require_command grep
require_command mktemp
require_command python3
require_command rustc
require_command rustup
require_command sed
require_command tar

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

export GH_PROMPT_DISABLED=1

if ! gh auth status --active --hostname github.com >/dev/null 2>&1; then
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

latest_stable_tag() {
  awk '
    /^v[0-9]+\.[0-9]+\.[0-9]+$/ {
      tag = $0
      split(substr(tag, 2), component, ".")
      major = component[1] + 0
      minor = component[2] + 0
      patch = component[3] + 0
      if (!found || major > best_major ||
          (major == best_major && minor > best_minor) ||
          (major == best_major && minor == best_minor && patch > best_patch)) {
        found = 1
        best_tag = tag
        best_major = major
        best_minor = minor
        best_patch = patch
      }
    }
    END {
      if (!found) exit 1
      print best_tag
    }
  '
}

if [ "$RELEASE" = "latest" ]; then
  if ! tag_names="$(
    gh api "repos/$REPOSITORY/tags?per_page=100" --paginate --jq '.[].name' 2>/dev/null
  )"; then
    fail "could not list bettercodex source tags"
  fi
  resolved_tag="$(printf '%s\n' "$tag_names" | latest_stable_tag)" ||
    fail "no stable bettercodex source tag is available"
else
  resolved_tag="$RELEASE"
fi

if ! printf '%s\n' "$resolved_tag" | grep -Eq '^v[0-9]+\.[0-9]+\.[0-9]+$'; then
  fail "source tag $resolved_tag is not a supported stable version"
fi

if ! resolved_commit="$(
  gh api "repos/$REPOSITORY/commits/$resolved_tag" --jq .sha 2>/dev/null
)"; then
  fail "source tag $resolved_tag does not exist or is not accessible"
fi
if ! printf '%s\n' "$resolved_commit" | grep -Eq '^[0-9a-fA-F]{40}$'; then
  fail "source tag $resolved_tag did not resolve to a valid commit"
fi

tmp_dir="$(mktemp -d "${TMPDIR:-/tmp}/bettercodex-install.XXXXXX")"
archive_path="$tmp_dir/source.tar.gz"
source_dir="$tmp_dir/source"
mkdir -p "$source_dir" "$BUILD_DIR"

platform_label="$os $arch"
short_commit="$(printf '%.12s' "$resolved_commit")"
step "Installing bettercodex ${resolved_tag#v} for $platform_label"
step "Downloading tagged source at $short_commit"
if ! gh api "repos/$REPOSITORY/tarball/$resolved_commit" >"$archive_path"; then
  fail "could not download source commit $resolved_commit"
fi
[ -s "$archive_path" ] || fail "downloaded source archive is empty"
if ! tar -xzf "$archive_path" -C "$source_dir" --strip-components=1; then
  fail "downloaded source archive could not be extracted"
fi

manifest_version="$(
  sed -n 's/^version = "\([^"]*\)"/\1/p' "$source_dir/Cargo.toml" 2>/dev/null | head -n 1
)"
expected_version="${resolved_tag#v}"
[ "$manifest_version" = "$expected_version" ] ||
  fail "source tag $resolved_tag contains package version ${manifest_version:-unknown}"

target_dir="$BUILD_DIR/target"
step "Compiling bettercodex $expected_version locally (the cache is reused next time)"
if ! python3 "$source_dir/scripts/dev.py" package-build --target-dir "$target_dir"; then
  fail "local bettercodex compilation failed"
fi

built_binary="$target_dir/release/bcodex"
[ -f "$built_binary" ] || fail "local build did not produce bcodex"
version_output="$("$built_binary" --version 2>/dev/null || true)"
[ "$version_output" = "bcodex $expected_version" ] ||
  fail "built binary did not report bcodex $expected_version"

step "Testing the locally compiled runtime and embedded resources"
if ! runtime_smoke="$("$built_binary" --internal-package-smoke)"; then
  fail "built binary failed its V8 and ICU runtime smoke test"
fi
[ "$runtime_smoke" = "bcodex $expected_version package smoke passed" ] ||
  fail "built binary returned an unexpected runtime smoke result"

smoke_root="$tmp_dir/smoke"
mkdir -p \
  "$smoke_root/home" \
  "$smoke_root/codex-home" \
  "$smoke_root/bcodex-home" \
  "$smoke_root/workspace"
if ! (
  cd "$smoke_root/workspace"
  HOME="$smoke_root/home" \
    CODEX_HOME="$smoke_root/codex-home" \
    BCODEX_HOME="$smoke_root/bcodex-home" \
    BCODEX_SKIP_UPDATE_CHECK=1 \
    "$built_binary" --tool-context-json >"$smoke_root/context.json"
); then
  fail "built binary failed its embedded-resource smoke test"
fi
[ -s "$smoke_root/context.json" ] || fail "built binary produced no tool context"
[ -s "$smoke_root/bcodex-home/skills/.system/loop/references/evals-manifest.md" ] ||
  fail "built binary is missing the embedded evaluator manifest"
[ -s "$smoke_root/bcodex-home/skills/.system/openai-docs/SKILL.md" ] ||
  fail "built binary is missing the embedded OpenAI documentation skill"
[ -s "$smoke_root/bcodex-home/skills/.system/openai-docs/scripts/resolve-latest-model-info.cjs" ] ||
  fail "built binary is missing an embedded OpenAI documentation resource"
grep -Fq 'openaiDeveloperDocs__search_openai_docs' "$smoke_root/context.json" ||
  fail "built binary tool context is incomplete"

mkdir -p "$BIN_DIR"
tmp_binary="$BIN_DIR/.bcodex.$$"
cp "$built_binary" "$tmp_binary"
chmod 0755 "$tmp_binary"
mv -f "$tmp_binary" "$BIN_PATH"
tmp_binary=""

installed_version="$("$BIN_PATH" --version 2>/dev/null || true)"
[ "$installed_version" = "bcodex $expected_version" ] ||
  fail "installed binary could not be verified"

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

if [ "$existing_install" -eq 1 ]; then
  step "Updated $installed_version at $BIN_PATH"
  case "$path_action" in
    added | updated) step "PATH was configured in $path_profile" ;;
    configured) step "PATH is configured in $path_profile" ;;
  esac
  step "Restart bettercodex to use the updated version"
else
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
fi
