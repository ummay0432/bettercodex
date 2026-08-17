#!/bin/sh

# Install the latest published bettercodex binary. The only persistent payload
# is the executable itself; source, Rust, Cargo, and native build tools are not
# downloaded or required.

set -eu

DEFAULT_REPOSITORY="ummay0432/bettercodex"
GITHUB_API_ROOT="https://api.github.com"
GITHUB_API_VERSION="2026-03-10"
MAX_METADATA_BYTES=1048576
MAX_ARCHIVE_BYTES=134217728
MAX_BINARY_BYTES=134217728
# POSIX shells express `ulimit -f` in 512-byte blocks, while Bash and zsh use
# 1024-byte units for this builtin.
if [ -n "${BASH_VERSION:-}" ] || [ -n "${ZSH_VERSION:-}" ]; then
  FILE_SIZE_LIMIT_BLOCK_BYTES=1024
else
  FILE_SIZE_LIMIT_BLOCK_BYTES=512
fi
MAX_METADATA_BLOCKS=$((MAX_METADATA_BYTES / FILE_SIZE_LIMIT_BLOCK_BYTES))
MAX_ARCHIVE_BLOCKS=$((MAX_ARCHIVE_BYTES / FILE_SIZE_LIMIT_BLOCK_BYTES))
MAX_BINARY_BLOCKS=$((MAX_BINARY_BYTES / FILE_SIZE_LIMIT_BLOCK_BYTES))
METADATA_TIMEOUT_SECONDS=30
ASSET_TIMEOUT_SECONDS=300

lock_acquired=0
lock_dir=""
transaction_dir=""

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
  cleanup_incomplete=0
  if [ -n "$transaction_dir" ]; then
    if ! rm -rf "$transaction_dir"; then
      cleanup_incomplete=1
      warn "could not remove installer transaction $(display_path "$transaction_dir")"
    fi
  fi
  if [ "$lock_acquired" -eq 1 ] && [ -n "$lock_dir" ]; then
    if [ "$cleanup_incomplete" -eq 0 ]; then
      rm -rf "$lock_dir" ||
        warn "could not remove installer lock $(display_path "$lock_dir")"
    else
      warn "retaining installer lock so the next install can retry cleanup"
    fi
  fi
  return 0
}

trap cleanup 0
trap 'exit 1' 1 2 15

usage() {
  cat <<EOF
Usage: install.sh

Downloads, verifies, and atomically installs the matching binary from the
selected published bettercodex GitHub release. No compilation is performed.

Environment:
  BCODEX_INSTALL_DIR          Binary directory (default: \$HOME/.local/bin).
  BCODEX_REPOSITORY           GitHub repository (default: $DEFAULT_REPOSITORY).
  BCODEX_INSTALL_RELEASE_TAG  Exact immutable release tag.
  BCODEX_INSTALL_ASSET_SHA256 Expected archive SHA-256 (internal updater input).
  BCODEX_INSTALL_ASSET_SIZE   Expected archive byte size (internal updater input).
EOF
}

if [ "$#" -gt 0 ]; then
  case "$1" in
    -h | --help)
      [ "$#" -eq 1 ] || fail "--help does not accept arguments"
      usage
      exit 0
      ;;
    *) fail "unknown argument; install.sh accepts only --help" ;;
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
  case "$1/" in
    */../*) fail "$2 must not contain parent-directory components" ;;
  esac
}

display_path() {
  display_unsafe=0
  for display_byte in $(printf '%s' "$1" | LC_ALL=C od -An -v -tu1); do
    if [ "$display_byte" -lt 32 ] || [ "$display_byte" -gt 126 ]; then
      display_unsafe=1
      break
    fi
  done
  if [ "$display_unsafe" -eq 1 ]; then
    display_hex="$(printf '%s' "$1" | LC_ALL=C od -An -v -tx1 | tr -d '[:space:]')"
    printf 'bytes:0x%s' "$display_hex"
  else
    printf "'"
    printf '%s' "$1" | sed "s/'/'\\\\''/g"
    printf "'"
  fi
}

is_cargo_artifact_path() {
  case "$1/" in
    */target/ | */target/debug/* | */target/release/* | */target/*/debug/* | */target/*/release/*)
      return 0
      ;;
  esac
  cargo_cursor="$1"
  while :; do
    if [ -f "$cargo_cursor/.rustc_info.json" ] ||
      { [ -d "$cargo_cursor/.fingerprint" ] && [ -d "$cargo_cursor/deps" ]; }; then
      return 0
    fi
    [ "$cargo_cursor" != "/" ] || break
    cargo_parent="${cargo_cursor%/*}"
    [ -n "$cargo_parent" ] || cargo_parent="/"
    [ "$cargo_parent" != "$cargo_cursor" ] || break
    cargo_cursor="$cargo_parent"
  done
  return 1
}

reject_cargo_artifact_path() {
  if is_cargo_artifact_path "$1"; then
    fail "refusing to install into Cargo artifact path $(display_path "$1")"
  fi

  cargo_existing="$1"
  while [ ! -d "$cargo_existing" ]; do
    cargo_parent="${cargo_existing%/*}"
    [ -n "$cargo_parent" ] || cargo_parent="/"
    [ "$cargo_parent" != "$cargo_existing" ] ||
      fail "could not resolve install directory $(display_path "$1")"
    cargo_existing="$cargo_parent"
  done

  cargo_physical_status=0
  (
    CDPATH= cd -P "$cargo_existing" 2>/dev/null || exit 2
    is_cargo_artifact_path "$PWD"
  ) || cargo_physical_status=$?
  case "$cargo_physical_status" in
    0) fail "refusing to install into Cargo artifact path $(display_path "$1")" ;;
    1) ;;
    *) fail "could not resolve install directory $(display_path "$1")" ;;
  esac
}

valid_version() {
  printf '%s\n' "$1" | LC_ALL=C awk -F . '
    function component(value) {
      return value ~ /^(0|[1-9][0-9]*)$/ &&
        (length(value) < 20 ||
          (length(value) == 20 && ("x" value) <= "x18446744073709551615"))
    }
    NF == 3 && component($1) && component($2) && component($3) { valid = 1 }
    END { exit !valid }
  '
}

valid_release_tag() {
  case "$1" in
    bcodex-v*-*) ;;
    *) return 1 ;;
  esac
  tag_remainder="${1#bcodex-v}"
  tag_revision="${tag_remainder##*-}"
  tag_version="${tag_remainder%-*}"
  [ "$tag_version" != "$tag_remainder" ] || return 1
  valid_version "$tag_version" || return 1
  printf '%s\n' "$tag_revision" | LC_ALL=C grep -Eq '^[0-9a-f]{40}$'
}

release_version() {
  tag_remainder="${1#bcodex-v}"
  printf '%s\n' "${tag_remainder%-*}"
}

release_revision() {
  tag_remainder="${1#bcodex-v}"
  printf '%s\n' "${tag_remainder##*-}"
}

valid_sha256() {
  printf '%s\n' "$1" | LC_ALL=C grep -Eq '^[0-9a-f]{64}$'
}

valid_asset_size() {
  case "$1" in
    '' | *[!0-9]*) return 1 ;;
  esac
  [ "${#1}" -le 9 ] || return 1
  [ "$1" -gt 0 ] && [ "$1" -le "$MAX_ARCHIVE_BYTES" ]
}

file_size() {
  wc -c <"$1" | tr -d '[:space:]'
}

file_sha256() {
  if command -v sha256sum >/dev/null 2>&1; then
    sha256sum <"$1" | awk '{ print $1 }' | tr 'A-F' 'a-f'
  elif command -v shasum >/dev/null 2>&1; then
    shasum -a 256 <"$1" | awk '{ print $1 }' | tr 'A-F' 'a-f'
  elif command -v openssl >/dev/null 2>&1; then
    openssl dgst -sha256 <"$1" | sed 's/^.*= //' | tr 'A-F' 'a-f'
  else
    return 1
  fi
}

require_sha256_tool() {
  command -v sha256sum >/dev/null 2>&1 ||
    command -v shasum >/dev/null 2>&1 ||
    command -v openssl >/dev/null 2>&1 ||
    fail "sha256sum, shasum, or openssl is required"
}

# Older curl versions do not enforce --max-filesize while streaming a response
# with no declared length. POSIX sh expresses this process limit in 512-byte
# blocks. Reset the inherited EXIT trap so a killed child stays a failure.
run_with_file_size_limit() {
  maximum_blocks="$1"
  shift
  (
    trap - 0
    ulimit -c 0 2>/dev/null || true
    current_file_limit="$(ulimit -f 2>/dev/null)" || exit 1
    case "$current_file_limit" in
      unlimited) ulimit -f "$maximum_blocks" || exit 1 ;;
      '' | *[!0-9]*) exit 1 ;;
      *)
        if [ "$current_file_limit" -gt "$maximum_blocks" ]; then
          ulimit -f "$maximum_blocks" || exit 1
        fi
        ;;
    esac
    "$@"
  )
}

# Adapted from the current upstream Codex standalone installer. It parses only
# the strictly validated top-level release fields and immediate asset objects;
# nested lookalikes and JSON field order cannot influence selection.
parse_release_metadata() {
  LC_ALL=C fold -b -w 4096 | LC_ALL=C awk '
    function top_value(value_kind, value) {
      if (object_depth != 1) {
        return
      }
      if ((key == "tag_name" || key == "target_commitish") && value_kind == "string") {
        print key "\t" value
      } else if ((key == "draft" || key == "prerelease" || key == "immutable") &&
          value_kind == "primitive") {
        print key "\t" value
      }
    }

    function asset_value(value_kind, value) {
      if (object_depth != asset_object_depth) {
        return
      }
      if (key == "name" && value_kind == "string") {
        if (++asset_name_count == 1) asset_name = value; else asset_invalid = 1
      } else if (key == "state" && value_kind == "string") {
        if (++asset_state_count == 1) asset_state = value; else asset_invalid = 1
      } else if (key == "size" && value_kind == "primitive") {
        if (++asset_size_count == 1) asset_size = value; else asset_invalid = 1
      } else if (key == "digest" && value_kind == "string") {
        if (++asset_digest_count == 1) asset_digest = value; else asset_invalid = 1
      }
    }

    function finish_value(value_kind, value) {
      top_value(value_kind, value)
      asset_value(value_kind, value)
      expecting_value = 0
      key = ""
    }

    function begin_asset() {
      asset_object_depth = object_depth
      asset_name = ""
      asset_state = ""
      asset_size = ""
      asset_digest = ""
      asset_name_count = 0
      asset_state_count = 0
      asset_size_count = 0
      asset_digest_count = 0
      asset_invalid = 0
    }

    function finish_asset() {
      if (!asset_invalid && asset_name_count == 1 && asset_state_count == 1 &&
          asset_size_count == 1 && asset_digest_count == 1) {
        print "asset\t" asset_name "\t" asset_state "\t" asset_size "\t" asset_digest
      }
      asset_object_depth = 0
    }

    {
      for (i = 1; i <= length($0); i++) {
        char = substr($0, i, 1)

        if (in_string) {
          if (escaped) {
            token = token "\\" char
            escaped = 0
          } else if (char == "\\") {
            escaped = 1
          } else if (char == "\"") {
            in_string = 0
            if (string_is_value) {
              finish_value("string", token)
            } else {
              pending_key = token
            }
          } else {
            token = token char
          }
          continue
        }

        if (in_primitive) {
          if (char ~ /[ \t\r\n]/ || char == "," || char == "}" || char == "]") {
            in_primitive = 0
            finish_value("primitive", token)
          } else {
            token = token char
            continue
          }
        }

        if (!document_started) {
          if (char ~ /[ \t\r\n]/) {
            continue
          }
          if (char != "{") {
            exit 1
          }
          document_started = 1
        } else if (document_finished) {
          if (char ~ /[ \t\r\n]/) {
            continue
          }
          exit 1
        }

        if (char == "\"") {
          in_string = 1
          token = ""
          escaped = 0
          string_is_value = expecting_value
        } else if (char == ":" && pending_key != "") {
          key = pending_key
          pending_key = ""
          expecting_value = 1
          if (object_depth == 1 && key == "assets" && ++assets_key_count > 1) {
            exit 1
          }
        } else if (expecting_value && char !~ /[ \t\r\n]/ && char != "{" && char != "[") {
          in_primitive = 1
          token = char
        } else if (char == "{") {
          object_depth++
          if (assets_array_depth != 0 && array_depth == assets_array_depth &&
              asset_object_depth == 0) {
            begin_asset()
          }
          expecting_value = 0
          key = ""
        } else if (char == "}") {
          if (object_depth == asset_object_depth) {
            finish_asset()
          }
          object_depth--
          if (object_depth < 0) exit 1
          if (object_depth == 0) document_finished = 1
          expecting_value = 0
          key = ""
          pending_key = ""
        } else if (char == "[") {
          array_depth++
          if (expecting_value && key == "assets" && object_depth == 1) {
            assets_array_depth = array_depth
          }
          expecting_value = 0
          key = ""
        } else if (char == "]") {
          if (array_depth == assets_array_depth) {
            assets_array_depth = 0
          }
          array_depth--
          if (array_depth < 0) exit 1
          expecting_value = 0
          key = ""
          pending_key = ""
        } else if (char == ",") {
          expecting_value = 0
          key = ""
          pending_key = ""
        }
      }
    }

    END {
      if (!document_started || !document_finished || in_string || in_primitive || escaped ||
          object_depth != 0 || array_depth != 0) {
        exit 1
      }
    }
  '
}

metadata_scalar() {
  metadata_key="$1"
  printf '%s\n' "$release_metadata" | awk -F '\t' -v key="$metadata_key" '
    $1 == key { count++; value = $2 }
    END {
      if (count != 1) exit 1
      print value
    }
  '
}

metadata_asset_field() {
  metadata_asset="$1"
  metadata_field="$2"
  printf '%s\n' "$release_metadata" | awk -F '\t' -v asset="$metadata_asset" -v field="$metadata_field" '
    $1 == "asset" && $2 == asset { count++; value = $field }
    END {
      if (count != 1) exit 1
      print value
    }
  '
}

download_metadata() {
  metadata_url="$1"
  metadata_path="$2"
  rm -f "$metadata_path"
  run_with_file_size_limit "$MAX_METADATA_BLOCKS" curl \
    --disable \
    --proto '=https' \
    --proto-redir '=https' \
    --tlsv1.2 \
    --fail \
    --silent \
    --show-error \
    --location \
    --max-redirs 5 \
    --retry 3 \
    --retry-max-time "$METADATA_TIMEOUT_SECONDS" \
    --connect-timeout 10 \
    --max-time "$METADATA_TIMEOUT_SECONDS" \
    --max-filesize "$MAX_METADATA_BYTES" \
    --user-agent bettercodex-installer \
    --header 'Accept: application/vnd.github+json' \
    --header "X-GitHub-Api-Version: $GITHUB_API_VERSION" \
    --output "$metadata_path" \
    "$metadata_url" || return 1
  metadata_bytes="$(file_size "$metadata_path")"
  case "$metadata_bytes" in
    '' | *[!0-9]*) return 1 ;;
  esac
  [ "$metadata_bytes" -gt 0 ] && [ "$metadata_bytes" -le "$MAX_METADATA_BYTES" ]
}

download_asset() {
  asset_url="$1"
  archive_path="$2"
  rm -f "$archive_path"
  run_with_file_size_limit "$MAX_ARCHIVE_BLOCKS" curl \
    --disable \
    --proto '=https' \
    --proto-redir '=https' \
    --tlsv1.2 \
    --fail \
    --silent \
    --show-error \
    --location \
    --max-redirs 5 \
    --retry 3 \
    --retry-max-time "$ASSET_TIMEOUT_SECONDS" \
    --connect-timeout 10 \
    --max-time "$ASSET_TIMEOUT_SECONDS" \
    --max-filesize "$MAX_ARCHIVE_BYTES" \
    --user-agent bettercodex-installer \
    --output "$archive_path" \
    "$asset_url" || return 1
}

acquire_install_lock() {
  lock_dir="$bin_dir/.bcodex-install.lock"
  lock_attempts=0
  while ! mkdir "$lock_dir" 2>/dev/null; do
    lock_pid="$(sed -n '1p' "$lock_dir/pid" 2>/dev/null || true)"
    case "$lock_pid" in
      '' | *[!0-9]*) ;;
      *)
        if kill -0 "$lock_pid" 2>/dev/null; then
          fail "another bettercodex install is already running (process $lock_pid)"
        fi
        ;;
    esac
    if [ "$lock_attempts" -lt 2 ]; then
      lock_attempts=$((lock_attempts + 1))
      sleep 1
      continue
    fi
    stale_lock="$bin_dir/.bcodex-stale-lock.$$"
    if mv "$lock_dir" "$stale_lock" 2>/dev/null; then
      rm -rf "$stale_lock" ||
        fail "could not remove stale installer lock $(display_path "$stale_lock")"
      lock_attempts=0
    elif [ -e "$lock_dir" ] || [ -L "$lock_dir" ]; then
      fail "could not reclaim stale installer lock $(display_path "$lock_dir")"
    fi
  done
  lock_acquired=1
  printf '%s\n' "$$" >"$lock_dir/pid"

  for stale_transaction in "$bin_dir"/.bcodex-transaction.* "$bin_dir"/.bcodex-stage.*; do
    if [ -e "$stale_transaction" ] || [ -L "$stale_transaction" ]; then
      rm -rf "$stale_transaction" ||
        fail "could not remove stale installer transaction $(display_path "$stale_transaction")"
    fi
  done
}

require_command awk
require_command chmod
require_command curl
require_command fold
require_command grep
require_command gzip
require_command mkdir
require_command mktemp
require_command mv
require_command od
require_command rm
require_command rmdir
require_command sed
require_command sleep
require_command tr
require_command uname
require_command wc
require_sha256_tool

repository="${BCODEX_REPOSITORY:-$DEFAULT_REPOSITORY}"
if ! printf '%s\n' "$repository" | LC_ALL=C grep -Eq '^[A-Za-z0-9_.-]+/[A-Za-z0-9_.-]+$'; then
  fail "BCODEX_REPOSITORY must be an owner/repository name"
fi
repository_owner="${repository%%/*}"
repository_name="${repository#*/}"
if [ "$repository_owner" = "." ] || [ "$repository_owner" = ".." ] ||
  [ "$repository_name" = "." ] || [ "$repository_name" = ".." ]; then
  fail "BCODEX_REPOSITORY must be an owner/repository name"
fi

if [ -n "${BCODEX_INSTALL_DIR:-}" ]; then
  bin_dir="$BCODEX_INSTALL_DIR"
  validate_absolute_path "$bin_dir" BCODEX_INSTALL_DIR
else
  [ -n "${HOME:-}" ] || fail "HOME or BCODEX_INSTALL_DIR must be set"
  validate_absolute_path "$HOME" HOME
  bin_dir="$HOME/.local/bin"
fi

case "$(uname -s):$(uname -m)" in
  Darwin:arm64 | Darwin:aarch64)
    system="Darwin"
    platform="macOS Apple silicon"
    asset="bcodex-aarch64-apple-darwin.gz"
    ;;
  Linux:x86_64 | Linux:amd64)
    system="Linux"
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
attested_sha256="${BCODEX_INSTALL_ASSET_SHA256:-}"
attested_size="${BCODEX_INSTALL_ASSET_SIZE:-}"
if [ -n "$attested_sha256" ] || [ -n "$attested_size" ]; then
  [ -n "$expected_tag" ] ||
    fail "BCODEX_INSTALL_ASSET_SHA256 and BCODEX_INSTALL_ASSET_SIZE require BCODEX_INSTALL_RELEASE_TAG"
  [ -n "$attested_sha256" ] && [ -n "$attested_size" ] ||
    fail "BCODEX_INSTALL_ASSET_SHA256 and BCODEX_INSTALL_ASSET_SIZE must be set together"
  valid_sha256 "$attested_sha256" || fail "BCODEX_INSTALL_ASSET_SHA256 is invalid"
  valid_asset_size "$attested_size" || fail "BCODEX_INSTALL_ASSET_SIZE is invalid"
fi

reject_cargo_artifact_path "$bin_dir"
mkdir -p "$bin_dir"
bin_path="$bin_dir/bcodex"
acquire_install_lock
if [ -L "$bin_path" ]; then
  fail "refusing to replace symlinked bettercodex executable $(display_path "$bin_path")"
fi
if [ -e "$bin_path" ] && [ ! -f "$bin_path" ]; then
  fail "$(display_path "$bin_path") exists but is not a regular file"
fi
existing_install=0
if [ -f "$bin_path" ]; then
  existing_install=1
fi

transaction_dir="$(mktemp -d "$bin_dir/.bcodex-transaction.XXXXXXXX")" ||
  fail "could not create an installer transaction in $(display_path "$bin_dir")"
metadata_path="$transaction_dir/release.json"
archive_path="$transaction_dir/$asset"
candidate_name=".bcodex-candidate.${transaction_dir##*.}"
candidate="$transaction_dir/$candidate_name"

if [ -n "$attested_sha256" ]; then
  resolved_tag="$expected_tag"
  expected_sha256="$attested_sha256"
  expected_size="$attested_size"
else
  if [ -n "$expected_tag" ]; then
    metadata_url="$GITHUB_API_ROOT/repos/$repository/releases/tags/$expected_tag"
    requested_release="$expected_tag"
  else
    metadata_url="$GITHUB_API_ROOT/repos/$repository/releases/latest"
    requested_release="latest"
  fi
  step "Resolving bettercodex release metadata"
  download_metadata "$metadata_url" "$metadata_path" ||
    fail "could not fetch bounded GitHub release metadata for $requested_release"
  release_metadata="$(parse_release_metadata <"$metadata_path")" ||
    fail "GitHub returned malformed bettercodex release metadata"
  resolved_tag="$(metadata_scalar tag_name)" ||
    fail "GitHub release metadata has no unique tag"
  target_commitish="$(metadata_scalar target_commitish)" ||
    fail "GitHub release metadata has no unique target revision"
  release_draft="$(metadata_scalar draft)" ||
    fail "GitHub release metadata has no unique draft state"
  release_prerelease="$(metadata_scalar prerelease)" ||
    fail "GitHub release metadata has no unique prerelease state"
  release_immutable="$(metadata_scalar immutable)" ||
    fail "GitHub release metadata has no unique immutable state"
  valid_release_tag "$resolved_tag" || fail "GitHub release tag is invalid"
  if [ -n "$expected_tag" ] && [ "$resolved_tag" != "$expected_tag" ]; then
    fail "GitHub returned release $resolved_tag, expected $expected_tag"
  fi
  resolved_revision="$(release_revision "$resolved_tag")"
  [ "$target_commitish" = "$resolved_revision" ] ||
    fail "GitHub release target does not match its encoded source revision"
  [ "$release_draft" = "false" ] && [ "$release_prerelease" = "false" ] ||
    fail "GitHub returned a draft or prerelease"
  [ "$release_immutable" = "true" ] || fail "GitHub release is not immutable"

  asset_state="$(metadata_asset_field "$asset" 3)" ||
    fail "GitHub release has no unique $asset asset"
  expected_size="$(metadata_asset_field "$asset" 4)" ||
    fail "GitHub release has no unique $asset size"
  asset_digest="$(metadata_asset_field "$asset" 5)" ||
    fail "GitHub release has no unique $asset digest"
  [ "$asset_state" = "uploaded" ] || fail "GitHub release asset is not uploaded"
  valid_asset_size "$expected_size" || fail "GitHub release asset size is invalid"
  case "$asset_digest" in
    sha256:*) expected_sha256="${asset_digest#sha256:}" ;;
    *) fail "GitHub release asset digest is invalid" ;;
  esac
  valid_sha256 "$expected_sha256" || fail "GitHub release asset digest is invalid"
fi

resolved_revision="$(release_revision "$resolved_tag")"
resolved_version="$(release_version "$resolved_tag")"
download_url="https://github.com/$repository/releases/download/$resolved_tag/$asset"

step "Downloading bettercodex for $platform"
download_asset "$download_url" "$archive_path" || fail "could not download $asset"
archive_size="$(file_size "$archive_path")"
valid_asset_size "$archive_size" || fail "downloaded archive size is outside the allowed range"
if [ "$archive_size" != "$expected_size" ]; then
  fail "downloaded archive size does not match GitHub release metadata"
fi
actual_sha256="$(file_sha256 "$archive_path")" || fail "could not calculate the archive SHA-256"
valid_sha256 "$actual_sha256" || fail "archive SHA-256 tool returned an invalid digest"
if [ "$actual_sha256" != "$expected_sha256" ]; then
  fail "downloaded archive SHA-256 does not match GitHub release metadata"
fi

run_with_file_size_limit "$MAX_BINARY_BLOCKS" gzip -dc "$archive_path" >"$candidate" ||
  fail "downloaded asset is not a valid gzip archive or exceeds the binary size limit"
binary_size="$(file_size "$candidate")"
case "$binary_size" in
  '' | *[!0-9]*) fail "downloaded binary has an invalid size" ;;
esac
if [ "$binary_size" -eq 0 ] || [ "$binary_size" -gt "$MAX_BINARY_BYTES" ]; then
  fail "downloaded binary size is outside the allowed range"
fi
chmod 755 "$candidate"

if [ "$system" = "Darwin" ]; then
  require_command codesign
  codesign --verify --strict "$candidate" >/dev/null 2>&1 ||
    fail "downloaded macOS binary has an invalid code signature"
fi

candidate_tag="$("$candidate" --internal-release-tag 2>/dev/null)" ||
  fail "downloaded binary has no valid bettercodex release tag"
valid_release_tag "$candidate_tag" ||
  fail "downloaded binary reported an invalid bettercodex release tag"
if [ "$candidate_tag" != "$resolved_tag" ]; then
  fail "downloaded binary is $candidate_tag, expected $resolved_tag"
fi
candidate_revision="$("$candidate" --internal-source-revision 2>/dev/null)" ||
  fail "downloaded binary has no valid bettercodex source revision"
if [ "$candidate_revision" != "$resolved_revision" ]; then
  fail "downloaded binary source revision does not match its release tag"
fi
version_output="$("$candidate" --version 2>/dev/null)" ||
  fail "downloaded binary did not report its version"
if [ "$version_output" != "bcodex $resolved_version" ]; then
  fail "downloaded binary version does not match its release tag"
fi

mv -f "$candidate" "$bin_path" ||
  fail "could not atomically replace $(display_path "$bin_path")"
if [ -e "$candidate" ] || [ -L "$candidate" ] || [ ! -f "$bin_path" ] || [ -L "$bin_path" ]; then
  misplaced_candidate="$bin_path/$candidate_name"
  if [ -f "$misplaced_candidate" ] && [ ! -L "$misplaced_candidate" ]; then
    rm -f "$misplaced_candidate" 2>/dev/null ||
      warn "could not remove misplaced installer candidate $(display_path "$misplaced_candidate")"
  fi
  fail "install destination changed during atomic replacement of $(display_path "$bin_path")"
fi

cleanup_legacy_install_state() {
  [ -n "${HOME:-}" ] || return 0
  cache_base="${XDG_CACHE_HOME:-$HOME/.cache}"
  case "$cache_base" in
    /*) ;;
    *) return 0 ;;
  esac
  if [ -L "$cache_base" ] || { [ -e "$cache_base" ] && [ ! -d "$cache_base" ]; }; then
    warn "not removing obsolete installer state through cache path $(display_path "$cache_base")"
    return 0
  fi
  cache_root="$cache_base/bettercodex"
  if [ -L "$cache_root" ] || { [ -e "$cache_root" ] && [ ! -d "$cache_root" ]; }; then
    warn "not removing obsolete installer state through cache path $(display_path "$cache_root")"
    return 0
  fi
  legacy_source_install=0
  for legacy_name in build cargo rustup tmp downloads; do
    legacy_path="$cache_root/$legacy_name"
    if [ -d "$legacy_path" ] && [ ! -L "$legacy_path" ]; then
      legacy_source_install=1
      rm -rf "$legacy_path" ||
        warn "could not remove obsolete installer cache $(display_path "$legacy_path")"
    fi
  done
  if [ "$legacy_source_install" -eq 1 ]; then
    for legacy_path in "$cache_root"/rusty-v8-*; do
      if [ -d "$legacy_path" ] && [ ! -L "$legacy_path" ]; then
        rm -rf "$legacy_path" ||
          warn "could not remove obsolete installer cache $(display_path "$legacy_path")"
      fi
    done
  fi
  rmdir "$cache_root" 2>/dev/null || true
  private_path="$bin_dir/bcodex-path"
  if [ -d "$private_path" ] && [ ! -L "$private_path" ]; then
    rm -rf "$private_path" ||
      warn "could not remove obsolete private helper directory $(display_path "$private_path")"
  fi
}

cleanup_legacy_install_state

pick_profile() {
  case "$system:${SHELL:-}" in
    Darwin:*/zsh) printf '%s\n' "$HOME/.zprofile" ;;
    Darwin:*/bash) printf '%s\n' "$HOME/.bash_profile" ;;
    Linux:*/zsh) printf '%s\n' "$HOME/.zshrc" ;;
    Linux:*/bash) printf '%s\n' "$HOME/.bashrc" ;;
    *) printf '%s\n' "$HOME/.profile" ;;
  esac
}

path_ready=0
remaining_path="${PATH:-}:"
while [ -n "$remaining_path" ]; do
  path_entry="${remaining_path%%:*}"
  remaining_path="${remaining_path#*:}"
  if [ "$path_entry" = "$bin_dir" ]; then
    path_ready=1
    break
  fi
done
if [ "$path_ready" -eq 0 ]; then
  default_bin="${HOME:-}/.local/bin"
  if [ -n "${HOME:-}" ] && [ "$bin_dir" = "$default_bin" ]; then
    profile="$(pick_profile)"
    managed_line='export PATH="$HOME/.local/bin:$PATH"'
    if ! grep -Fqx "$managed_line" "$profile" 2>/dev/null; then
      printf '\n%s\n' "$managed_line" >>"$profile" ||
        warn "could not add $(display_path "$bin_dir") to PATH in $(display_path "$profile")"
    fi
    step "PATH configured in $(display_path "$profile"); open a new terminal"
  else
    warn "install directory $(display_path "$bin_dir") is not on PATH; add it before running bcodex"
  fi
fi

rm -rf "$transaction_dir" ||
  warn "could not remove installer transaction $(display_path "$transaction_dir")"
transaction_dir=""
rm -rf "$lock_dir" || warn "could not remove installer lock $(display_path "$lock_dir")"
lock_acquired=0
lock_dir=""

if [ "$existing_install" -eq 1 ]; then
  step "Updated bcodex $resolved_version at $(display_path "$bin_path")"
else
  step "Installed bcodex $resolved_version at $(display_path "$bin_path")"
  step "Run: bcodex"
fi
