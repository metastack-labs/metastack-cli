#!/bin/sh
set -eu

die() {
  printf '%s\n' "error: $*" >&2
  exit 1
}

usage() {
  cat <<'EOF'
Install the `meta` CLI from GitHub Releases.

Usage:
  ./scripts/install-meta.sh [--version VERSION] [--bin-dir DIR] [--repo OWNER/REPO]

Options:
  --version VERSION  Install a specific version (accepts vX.Y.Z or X.Y.Z)
  --bin-dir DIR      Install into DIR instead of ~/.local/bin
  --repo OWNER/REPO  Override the GitHub repository (default: metastack-systems/metastack-cli)
  -h, --help         Show this help output

Environment:
  META_INSTALL_BASE_URL        Override the GitHub base URL (default: https://github.com)
  META_INSTALL_DIR             Default install directory when --bin-dir is omitted
  META_INSTALL_REPO            Default repository when --repo is omitted
  META_INSTALL_VERSION         Default version when --version is omitted
  META_INSTALL_LATEST_VERSION  Testing override for latest-version resolution
  META_INSTALL_OS             Override detected OS slug (default: uname -s)
  META_INSTALL_ARCH           Override detected architecture slug (default: uname -m)
EOF
}

require_command() {
  if ! command -v "$1" >/dev/null 2>&1; then
    die "missing required command '$1'"
  fi
}

normalize_version() {
  version=$1
  case "$version" in
    v*) printf '%s\n' "${version#v}" ;;
    *) printf '%s\n' "$version" ;;
  esac
}

resolve_os() {
  raw_os=${META_INSTALL_OS:-$(uname -s)}
  case "$raw_os" in
    Darwin|darwin) printf '%s\n' "darwin" ;;
    Linux|linux) printf '%s\n' "linux" ;;
    *) die "unsupported operating system '$raw_os' (supported: macOS, Linux)" ;;
  esac
}

resolve_arch() {
  raw_arch=${META_INSTALL_ARCH:-$(uname -m)}
  case "$raw_arch" in
    arm64|aarch64) printf '%s\n' "arm64" ;;
    x86_64|amd64|x64) printf '%s\n' "x64" ;;
    *) die "unsupported architecture '$raw_arch' (supported: x86_64, arm64)" ;;
  esac
}

resolve_latest_tag() {
  if [ -n "${META_INSTALL_LATEST_VERSION:-}" ]; then
    printf 'v%s\n' "$(normalize_version "$META_INSTALL_LATEST_VERSION")"
    return
  fi

  latest_url=$BASE_URL/$REPO/releases/latest
  effective_url=$(curl -fsSL -o /dev/null -w '%{url_effective}' "$latest_url")

  case "$effective_url" in
    */releases/tag/*)
      printf '%s\n' "${effective_url##*/}"
      ;;
    *)
      die "failed to resolve the latest release tag from '$latest_url'"
      ;;
  esac
}

sha256_file() {
  file_path=$1

  if command -v shasum >/dev/null 2>&1; then
    shasum -a 256 "$file_path" | awk '{ print $1 }'
  elif command -v sha256sum >/dev/null 2>&1; then
    sha256sum "$file_path" | awk '{ print $1 }'
  else
    die "missing required checksum tool (need 'shasum' or 'sha256sum')"
  fi
}

VERSION_INPUT=${META_INSTALL_VERSION:-}
BIN_DIR=${META_INSTALL_DIR:-$HOME/.local/bin}
REPO=${META_INSTALL_REPO:-metastack-systems/metastack-cli}
BASE_URL=${META_INSTALL_BASE_URL:-https://github.com}
BASE_URL=${BASE_URL%/}

while [ "$#" -gt 0 ]; do
  case "$1" in
    --version)
      shift
      [ "$#" -gt 0 ] || die "--version requires a value"
      VERSION_INPUT=$1
      ;;
    --bin-dir)
      shift
      [ "$#" -gt 0 ] || die "--bin-dir requires a value"
      BIN_DIR=$1
      ;;
    --repo)
      shift
      [ "$#" -gt 0 ] || die "--repo requires a value"
      REPO=$1
      ;;
    -h|--help)
      usage
      exit 0
      ;;
    *)
      die "unknown argument '$1'"
      ;;
  esac
  shift
done

require_command curl
require_command tar
require_command mktemp
require_command uname

OS=$(resolve_os)
ARCH=$(resolve_arch)

if [ -n "$VERSION_INPUT" ]; then
  VERSION=$(normalize_version "$VERSION_INPUT")
  TAG=v$VERSION
else
  TAG=$(resolve_latest_tag)
  VERSION=$(normalize_version "$TAG")
fi

ASSET_NAME=metastack-cli-$VERSION-$OS-$ARCH.tar.gz
DOWNLOAD_ROOT=$BASE_URL/$REPO/releases/download/$TAG

tmp_dir=$(mktemp -d)
cleanup() {
  rm -rf "$tmp_dir"
}
trap cleanup EXIT INT TERM

archive_path=$tmp_dir/$ASSET_NAME
checksum_path=$tmp_dir/SHA256SUMS
extract_dir=$tmp_dir/extract
install_path=$BIN_DIR/meta

curl -fsSL "$DOWNLOAD_ROOT/$ASSET_NAME" -o "$archive_path" || {
  die "failed to download '$ASSET_NAME' from release '$TAG'"
}
curl -fsSL "$DOWNLOAD_ROOT/SHA256SUMS" -o "$checksum_path" || {
  die "failed to download SHA256SUMS from release '$TAG'"
}

expected_checksum=$(awk -v name="$ASSET_NAME" '$2 == name { print $1 }' "$checksum_path")
[ -n "$expected_checksum" ] || die "release '$TAG' is missing a checksum entry for '$ASSET_NAME'"

actual_checksum=$(sha256_file "$archive_path")
[ "$actual_checksum" = "$expected_checksum" ] || {
  die "checksum mismatch for '$ASSET_NAME'"
}

mkdir -p "$extract_dir"
tar -xzf "$archive_path" -C "$extract_dir"
[ -f "$extract_dir/meta" ] || die "release archive '$ASSET_NAME' did not contain a 'meta' binary"

mkdir -p "$BIN_DIR"
cp "$extract_dir/meta" "$install_path"
chmod 755 "$install_path"

printf '%s\n' "installed meta $VERSION to $install_path"
case ":$PATH:" in
  *":$BIN_DIR:"*) ;;
  *)
    printf '%s\n' "note: add $BIN_DIR to your PATH to run 'meta' globally" >&2
    ;;
esac
