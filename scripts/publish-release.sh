#!/bin/sh

# Build and verify one native BetterCodex release asset set. By default the
# assets are copied to an explicit output directory. --upload sends them to an
# already-created draft GitHub Release; this script never creates or publishes
# a release and never invokes GitHub Actions.

set -eu

DEFAULT_REPOSITORY="ummay0432/bettercodex"
GLIBC_MAX_MAJOR=2
GLIBC_MAX_MINOR=31

fail() {
  printf 'bettercodex release: %s\n' "$1" >&2
  exit 1
}

warn() {
  printf 'bettercodex release: warning: %s\n' "$1" >&2
}

usage() {
  cat <<'EOF'
Usage:
  scripts/publish-release.sh OUTPUT_DIRECTORY
  scripts/publish-release.sh --upload

Builds the current tagged revision for this native host, verifies it, and
creates portable gzip, fast zstd, and (when a previous release exists) compact
previous-binary update assets plus their .sha256 files. --upload requires an
existing draft release and an authenticated GitHub CLI; publishing the draft
remains a separate explicit maintainer action.
EOF
}

upload=0
output_dir=""
if [ "$#" -ne 1 ]; then
  usage >&2
  exit 2
fi
case "$1" in
  --help | -h) usage; exit 0 ;;
  --upload) upload=1 ;;
  *) output_dir="$1" ;;
esac

case "$output_dir" in
  "" | /*) ;;
  *) fail "OUTPUT_DIRECTORY must be an absolute path" ;;
esac

for required_command in awk basename cmp cp curl dirname git grep gzip mktemp rustup sed wc zstd; do
  command -v "$required_command" >/dev/null 2>&1 ||
    fail "$required_command is required"
done
zstd_version="$(zstd --version | sed -n 's/.* v\([0-9][0-9]*\.[0-9][0-9]*\.[0-9][0-9]*\).*/\1/p')"
if ! printf '%s\n' "$zstd_version" | awk -F. '
  NF == 3 && ($1 > 1 || ($1 == 1 && ($2 > 5 || ($2 == 5 && $3 >= 7)))) {
    supported = 1
  }
  END { exit !supported }
'; then
  fail "zstd 1.5.7 or newer is required for compact release assets"
fi

script_dir="$(CDPATH= cd -P "$(dirname "$0")" && pwd)"
repository_root="$(dirname "$script_dir")"
cd "$repository_root"

[ -z "$(git status --porcelain --untracked-files=all)" ] ||
  fail "release packaging requires a clean checkout"
revision="$(git rev-parse --verify HEAD)"
printf '%s\n' "$revision" | grep -Eq '^[0-9a-f]{40}$' ||
  fail "HEAD did not resolve to a canonical full revision"
version="$(
  sed -n 's/^version = "\([0-9][0-9]*\.[0-9][0-9]*\.[0-9][0-9]*\)"/\1/p' Cargo.toml |
    sed -n '1p'
)"
[ -n "$version" ] || fail "Cargo.toml has no supported package version"
release_tag="bcodex-v$version-$revision"
tag_revision="$(git rev-parse --verify "refs/tags/$release_tag^{commit}" 2>/dev/null || true)"
[ "$tag_revision" = "$revision" ] ||
  fail "annotated tag $release_tag must exist at HEAD before packaging"
[ "$(git cat-file -t "refs/tags/$release_tag" 2>/dev/null || true)" = tag ] ||
  fail "$release_tag must be an annotated tag"

repository="${BCODEX_REPOSITORY:-$DEFAULT_REPOSITORY}"
case "$repository" in
  */*) ;;
  *) fail "BCODEX_REPOSITORY must be an owner/repository name" ;;
esac
printf '%s\n' "$repository" | grep -Eq '^[A-Za-z0-9_.-]+/[A-Za-z0-9_.-]+$' ||
  fail "BCODEX_REPOSITORY must be an owner/repository name"

toolchain="$(
  sed -n 's/^channel = "\([A-Za-z0-9._-]*\)"/\1/p' rust-toolchain.toml |
    sed -n '1p'
)"
[ -n "$toolchain" ] || fail "rust-toolchain.toml has no pinned toolchain"
cargo_program="$(rustup which --toolchain "$toolchain" cargo)" ||
  fail "the pinned Cargo toolchain is unavailable"
rustc_program="$(rustup which --toolchain "$toolchain" rustc)" ||
  fail "the pinned rustc toolchain is unavailable"
toolchain_bin="$(dirname "$cargo_program")"
host_target="$("$rustc_program" -vV | sed -n 's/^host: //p')"
case "$host_target" in
  x86_64-unknown-linux-gnu | aarch64-unknown-linux-gnu)
    host_os="linux"
    ;;
  x86_64-apple-darwin | aarch64-apple-darwin)
    host_os="macos"
    ;;
  *) fail "unsupported native release target $host_target" ;;
esac

if command -v sha256sum >/dev/null 2>&1; then
  sha256_file() { sha256sum "$1" | awk '{ print $1 }'; }
elif command -v shasum >/dev/null 2>&1; then
  sha256_file() { shasum -a 256 "$1" | awk '{ print $1 }'; }
else
  fail "sha256sum or shasum is required"
fi

write_checksum() {
  checksum_asset="$1"
  checksum_path="$2"
  checksum_name="$(basename "$checksum_asset")"
  checksum_digest="$(sha256_file "$checksum_asset")"
  printf '%s  %s\n' "$checksum_digest" "$checksum_name" >"$checksum_path"
}

verify_checksum() {
  verified_asset="$1"
  verified_checksum="$2"
  verified_name="$(basename "$verified_asset")"
  expected_digest="$(
    awk -v expected="$verified_name" '
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
    ' "$verified_checksum" 2>/dev/null || true
  )"
  [ -n "$expected_digest" ] ||
    fail "invalid checksum for release asset $verified_name"
  [ "$(sha256_file "$verified_asset")" = "$expected_digest" ] ||
    fail "release asset $verified_name failed checksum verification"
}

verify_uploaded_digest() {
  uploaded_asset="$1"
  uploaded_name="$(basename "$uploaded_asset")"
  uploaded_expected="sha256:$(sha256_file "$uploaded_asset")"
  uploaded_actual="$(
    gh api "repos/$repository/releases/tags/$release_tag" \
      --jq ".assets[] | select(.name == \"$uploaded_name\") | .digest"
  )" || fail "could not inspect uploaded release asset $uploaded_name"
  [ "$uploaded_actual" = "$uploaded_expected" ] ||
    fail "GitHub reported digest $uploaded_actual for $uploaded_name, expected $uploaded_expected"
}

# Return 44 only for a confirmed 404 and 75 for every transient or unexpected
# response. Partial responses are never retained.
download_optional() {
  optional_url="$1"
  optional_destination="$2"
  optional_partial="$optional_destination.partial"
  rm -f "$optional_partial"
  if ! optional_status="$(
    curl \
      --proto '=https' \
      --tlsv1.2 \
      --silent \
      --show-error \
      --location \
      --connect-timeout 10 \
      --max-time 300 \
      --max-filesize 134217728 \
      --retry 2 \
      --retry-delay 1 \
      --retry-connrefused \
      --user-agent bettercodex-release \
      --header 'Accept: application/vnd.github+json' \
      --header 'X-GitHub-Api-Version: 2022-11-28' \
      --output "$optional_partial" \
      --write-out '%{http_code}' \
      "$optional_url"
  )"; then
    rm -f "$optional_partial"
    return 75
  fi
  case "$optional_status" in
    200)
      [ -s "$optional_partial" ] || {
        rm -f "$optional_partial"
        return 75
      }
      mv "$optional_partial" "$optional_destination"
      return 0
      ;;
    404)
      rm -f "$optional_partial"
      return 44
      ;;
    *)
      rm -f "$optional_partial"
      return 75
      ;;
  esac
}

temporary_parent="${TMPDIR:-/tmp}"
case "$temporary_parent" in
  /*) ;;
  *) fail "TMPDIR must be an absolute path" ;;
esac
temporary_root="$(mktemp -d "$temporary_parent/bettercodex-release.XXXXXX")"
cleanup() {
  set +e
  cd /
  rm -rf "$temporary_root"
}
trap cleanup 0
trap 'exit 1' 1 2 15

target_dir="$temporary_root/target"
compiler_tmp="$temporary_root/compiler-tmp"
smoke_root="$temporary_root/smoke"
artifact_dir="$temporary_root/artifacts"
mkdir -p "$compiler_tmp" "$smoke_root/home" "$smoke_root/codex-home" \
  "$smoke_root/bcodex-home" "$smoke_root/workspace" "$artifact_dir"

previous_revision=""
previous_version=""
previous_binary="$temporary_root/previous-bcodex"
previous_metadata="$temporary_root/previous-release.json"
if download_optional \
  "https://api.github.com/repos/$repository/releases/latest" \
  "$previous_metadata"; then
  previous_tag="$(
    sed -n 's/^[[:space:]]*"tag_name"[[:space:]]*:[[:space:]]*"\([^"]*\)".*/\1/p' \
      "$previous_metadata" |
      sed -n '1p'
  )"
  case "$previous_tag" in
    bcodex-v*) ;;
    *) fail "latest published release has an invalid BetterCodex tag" ;;
  esac
  previous_release="${previous_tag#bcodex-v}"
  previous_revision="${previous_release##*-}"
  previous_version="${previous_release%-*}"
  printf '%s\n' "$previous_revision" | grep -Eq '^[0-9a-f]{40}$' ||
    fail "latest published release tag has an invalid source revision"
  printf '%s\n' "$previous_version" | grep -Eq '^[0-9]+\.[0-9]+\.[0-9]+$' ||
    fail "latest published release tag has an invalid package version"
  [ "$previous_tag" != "$release_tag" ] ||
    fail "$release_tag is already the latest published release"

  previous_base="https://github.com/$repository/releases/download/$previous_tag"
  previous_found=0
  for previous_format in zst gz; do
    previous_asset_name="bcodex-$host_target.$previous_format"
    previous_asset="$temporary_root/$previous_asset_name"
    previous_checksum="$previous_asset.sha256"
    if download_optional \
      "$previous_base/$previous_asset_name.sha256" \
      "$previous_checksum"; then
      if download_optional \
        "$previous_base/$previous_asset_name" \
        "$previous_asset"; then
        :
      else
        previous_result=$?
        [ "$previous_result" -ne 44 ] ||
          fail "latest release checksum exists without $previous_asset_name"
        fail "could not download previous release asset $previous_asset_name"
      fi
      verify_checksum "$previous_asset" "$previous_checksum"
      case "$previous_format" in
        zst) zstd -q -d -c "$previous_asset" >"$previous_binary" ;;
        gz) gzip -dc "$previous_asset" >"$previous_binary" ;;
      esac
      previous_found=1
      break
    else
      previous_result=$?
      [ "$previous_result" -eq 44 ] ||
        fail "could not download the previous release checksum"
    fi
  done
  [ "$previous_found" -eq 1 ] ||
    fail "latest release has no verified native $host_target executable"
  chmod 0755 "$previous_binary"
  [ "$("$previous_binary" --version 2>/dev/null || true)" = "bcodex $previous_version" ] ||
    fail "previous release binary reported the wrong version"
  [ "$("$previous_binary" --internal-source-revision 2>/dev/null || true)" = "$previous_revision" ] ||
    fail "previous release binary reported the wrong source revision"
else
  previous_result=$?
  [ "$previous_result" -eq 44 ] ||
    fail "could not resolve the latest published release for compact update generation"
fi

printf '==> Building BetterCodex %s (%s) for %s\n' "$version" "$(printf '%.12s' "$revision")" "$host_target"
if [ "$host_os" = macos ]; then
  MACOSX_DEPLOYMENT_TARGET=12.0
  export MACOSX_DEPLOYMENT_TARGET
fi
if ! (
  unset CARGO_BUILD_BUILD_DIR CARGO_BUILD_TARGET CARGO_BUILD_TARGET_DIR \
    CARGO_ENCODED_RUSTFLAGS CARGO_INCREMENTAL CARGO_INSTALL_ROOT RUSTC_WRAPPER \
    RUSTC_WORKSPACE_WRAPPER RUSTFLAGS RUSTUP_TOOLCHAIN
  BCODEX_SOURCE_REVISION="$revision" \
    CARGO="$cargo_program" \
    CARGO_INCREMENTAL=0 \
    CARGO_TARGET_DIR="$target_dir" \
    PATH="$toolchain_bin:$PATH" \
    RUSTC="$rustc_program" \
    TEMP="$compiler_tmp" \
    TMP="$compiler_tmp" \
    TMPDIR="$compiler_tmp" \
    ./scripts/cargo-with-v8.sh build --profile distribution --locked --bin bcodex
); then
  fail "native release build failed"
fi

binary="$target_dir/distribution/bcodex"
[ -f "$binary" ] && [ -x "$binary" ] || fail "release build produced no bcodex executable"
[ "$("$binary" --version 2>/dev/null || true)" = "bcodex $version" ] ||
  fail "release binary reported the wrong version"
[ "$("$binary" --internal-source-revision 2>/dev/null || true)" = "$revision" ] ||
  fail "release binary reported the wrong source revision"
smoke_output="$(
  cd "$smoke_root/workspace"
  HOME="$smoke_root/home" \
    CODEX_HOME="$smoke_root/codex-home" \
    BCODEX_HOME="$smoke_root/bcodex-home" \
    BCODEX_SKIP_UPDATE_CHECK=1 \
    "$binary" --internal-install-smoke
)" || fail "release binary failed its install smoke test"
[ "$smoke_output" = "bcodex $version install smoke passed" ] ||
  fail "release binary returned an unexpected smoke result"

if [ "$host_os" = linux ]; then
  command -v readelf >/dev/null 2>&1 || fail "readelf is required for Linux compatibility verification"
  version_information="$(readelf --version-info "$binary" 2>/dev/null)" ||
    fail "could not inspect the Linux binary's glibc requirements"
  if ! printf '%s\n' "$version_information" |
    awk -v max_major="$GLIBC_MAX_MAJOR" -v max_minor="$GLIBC_MAX_MINOR" '
      {
        line = $0
        while (match(line, /GLIBC_[0-9]+\.[0-9]+/)) {
          version = substr(line, RSTART + 6, RLENGTH - 6)
          split(version, parts, ".")
          if (parts[1] > max_major ||
              (parts[1] == max_major && parts[2] > max_minor)) exit 1
          line = substr(line, RSTART + RLENGTH)
        }
      }
    '; then
    fail "Linux binary imports symbols newer than the glibc 2.31 compatibility floor"
  fi
elif command -v codesign >/dev/null 2>&1; then
  if codesign -dv "$binary" >/dev/null 2>&1; then
    codesign --verify --strict "$binary" || fail "macOS code-signature verification failed"
  else
    warn "macOS release binary is unsigned; sign and notarize production assets before publication"
  fi
fi

gzip_name="bcodex-$host_target.gz"
gzip_path="$artifact_dir/$gzip_name"
gzip_checksum="$gzip_path.sha256"
zstd_name="bcodex-$host_target.zst"
zstd_path="$artifact_dir/$zstd_name"
zstd_checksum="$zstd_path.sha256"

gzip -n -9 -c "$binary" >"$gzip_path"
zstd -q --ultra -T1 -22 --check -c "$binary" >"$zstd_path"
write_checksum "$gzip_path" "$gzip_checksum"
write_checksum "$zstd_path" "$zstd_checksum"
verify_checksum "$gzip_path" "$gzip_checksum"
verify_checksum "$zstd_path" "$zstd_checksum"
gzip -dc "$gzip_path" | cmp -s - "$binary" ||
  fail "gzip release asset did not reproduce the release binary"
zstd -q -d -c "$zstd_path" | cmp -s - "$binary" ||
  fail "zstd release asset did not reproduce the release binary"

patch_name=""
patch_path=""
patch_checksum=""
if [ -n "$previous_revision" ]; then
  patch_name="bcodex-$host_target-from-$previous_revision.patch.zst"
  patch_path="$artifact_dir/$patch_name"
  patch_checksum="$patch_path.sha256"
  printf '==> Creating compact update from %s\n' "$(printf '%.12s' "$previous_revision")"
  zstd -q --ultra -T1 -22 --check --patch-from="$previous_binary" -c "$binary" >"$patch_path"
  write_checksum "$patch_path" "$patch_checksum"
  verify_checksum "$patch_path" "$patch_checksum"
  zstd -q -d --patch-from="$previous_binary" -c "$patch_path" |
    cmp -s - "$binary" ||
    fail "compact update did not reproduce the release binary"
fi

if [ "$upload" -eq 1 ]; then
  command -v gh >/dev/null 2>&1 || fail "GitHub CLI is required for --upload"
  is_draft="$(gh release view "$release_tag" --repo "$repository" --json isDraft --jq .isDraft)" ||
    fail "could not inspect draft release $release_tag"
  [ "$is_draft" = true ] || fail "$release_tag must remain a draft while assets are uploaded"
  gh release upload \
    "$release_tag" \
    "$gzip_path" \
    "$gzip_checksum" \
    "$zstd_path" \
    "$zstd_checksum" \
    --repo "$repository" ||
    fail "could not upload $host_target release assets"
  if [ -n "$patch_path" ]; then
    gh release upload \
      "$release_tag" \
      "$patch_path" \
      "$patch_checksum" \
      --repo "$repository" ||
      fail "could not upload $host_target compact update asset"
  fi
  for uploaded_asset in "$gzip_path" "$gzip_checksum" "$zstd_path" "$zstd_checksum"; do
    verify_uploaded_digest "$uploaded_asset"
  done
  if [ -n "$patch_path" ]; then
    verify_uploaded_digest "$patch_path"
    verify_uploaded_digest "$patch_checksum"
  fi
  printf '==> Uploaded %s release assets to draft %s\n' "$host_target" "$release_tag"
else
  mkdir -p "$output_dir"
  cp "$gzip_path" "$gzip_checksum" "$zstd_path" "$zstd_checksum" "$output_dir/"
  if [ -n "$patch_path" ]; then
    cp "$patch_path" "$patch_checksum" "$output_dir/"
  fi
  printf '==> Wrote %s release assets to %s\n' "$host_target" "$output_dir"
fi

printf '==> Raw executable bytes %s\n' "$(wc -c <"$binary" | awk '{ print $1 }')"
printf '==> Gzip bytes %s\n' "$(wc -c <"$gzip_path" | awk '{ print $1 }')"
printf '==> Zstd bytes %s\n' "$(wc -c <"$zstd_path" | awk '{ print $1 }')"
if [ -n "$patch_path" ]; then
  printf '==> Compact update bytes %s\n' "$(wc -c <"$patch_path" | awk '{ print $1 }')"
fi
