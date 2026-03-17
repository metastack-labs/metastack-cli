#!/bin/sh
set -eu

die() {
  printf '%s\n' "error: $*" >&2
  exit 1
}

usage() {
  cat <<'EOF'
Build versioned GitHub release archives for the supported CLI targets.

Usage:
  ./scripts/release-artifacts.sh [--output-dir DIR] [--expect-version VERSION] [--target TARGET...]

Options:
  --output-dir DIR        Write artifacts under DIR/<version> instead of target/release-artifacts/<version>
  --expect-version VALUE  Fail unless Cargo.toml reports VALUE (accepts vX.Y.Z or X.Y.Z)
  --target TARGET         Limit packaging to the named Rust target triple (repeatable)
  -h, --help              Show this help output

Environment:
  META_RELEASE_BINARY_NAME         Override the packaged binary name (default: meta)
  META_RELEASE_CARGO               Override the cargo executable used for native builds (default: cargo)
  META_RELEASE_CROSS               Override the cross executable used for Linux ARM builds (default: cross)
  META_RELEASE_EXPECT_VERSION      Fail unless Cargo.toml reports this version
  META_RELEASE_TARGET_DIR          Override the Cargo target directory root (default: <repo>/target)
  META_RELEASE_VERIFY_ALL_TARGETS  Run version checks for every built binary when set to 1
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

package_field() {
  key=$1
  awk -F ' = ' -v key="$key" '
    $0 == "[package]" { in_package = 1; next }
    in_package && /^\[/ { exit }
    in_package && $1 == key {
      gsub(/"/, "", $2)
      print $2
      exit
    }
  ' "$CARGO_TOML"
}

checksum_line() {
  asset_path=$1
  asset_name=$(basename "$asset_path")

  if command -v shasum >/dev/null 2>&1; then
    checksum=$(shasum -a 256 "$asset_path" | awk '{ print $1 }')
  elif command -v sha256sum >/dev/null 2>&1; then
    checksum=$(sha256sum "$asset_path" | awk '{ print $1 }')
  else
    die "missing required checksum tool (need 'shasum' or 'sha256sum')"
  fi

  printf '%s  %s\n' "$checksum" "$asset_name" >> "$CHECKSUM_MANIFEST"
}

verify_binary_version() {
  target=$1
  binary_path=$2
  expected="$BINARY_NAME $PACKAGE_VERSION"

  if [ "${META_RELEASE_VERIFY_ALL_TARGETS:-0}" != "1" ] && [ -n "$HOST_TARGET" ] && [ "$target" != "$HOST_TARGET" ]; then
    return
  fi

  actual=$("$binary_path" --version | tr -d '\r')
  if [ "$actual" != "$expected" ]; then
    die "expected '$binary_path --version' to print '$expected', got '$actual'"
  fi
}

target_metadata() {
  case "$1" in
    aarch64-apple-darwin)
      TARGET_OS=darwin
      TARGET_ARCH=arm64
      TARGET_BUILDER=cargo
      ;;
    x86_64-apple-darwin)
      TARGET_OS=darwin
      TARGET_ARCH=x64
      TARGET_BUILDER=cargo
      ;;
    x86_64-unknown-linux-musl)
      TARGET_OS=linux
      TARGET_ARCH=x64
      TARGET_BUILDER=cross
      ;;
    aarch64-unknown-linux-musl)
      TARGET_OS=linux
      TARGET_ARCH=arm64
      TARGET_BUILDER=cross
      ;;
    *)
      return 1
      ;;
  esac
}

append_target() {
  target=$1
  target_metadata "$target" || die "unsupported release target '$target'"

  case " $TARGETS " in
    *" $target "*) return ;;
  esac

  TARGETS="${TARGETS:+$TARGETS }$target"
}

build_target() {
  target=$1
  builder=$2

  if [ "$builder" = "cargo" ]; then
    require_command "$CARGO_BIN"
    "$CARGO_BIN" build --locked --release --target "$target" --target-dir "$TARGET_DIR_ROOT"
  else
    require_command "$CROSS_BIN"
    "$CROSS_BIN" build --locked --release --target "$target" --target-dir "$TARGET_DIR_ROOT"
  fi
}

package_target() {
  target=$1
  os=$2
  arch=$3
  builder=$4

  build_target "$target" "$builder"

  binary_path=$TARGET_DIR_ROOT/$target/release/$BINARY_NAME
  if [ ! -x "$binary_path" ]; then
    die "expected built binary at '$binary_path'"
  fi

  verify_binary_version "$target" "$binary_path"

  asset_name=$PACKAGE_NAME-$PACKAGE_VERSION-$os-$arch.tar.gz
  stage_dir=$(mktemp -d "$OUTPUT_DIR/.stage.XXXXXX")
  cp "$binary_path" "$stage_dir/$BINARY_NAME"
  tar -C "$stage_dir" -czf "$OUTPUT_DIR/$asset_name" "$BINARY_NAME"
  rm -rf "$stage_dir"

  checksum_line "$OUTPUT_DIR/$asset_name"
  printf '%s\n' "$OUTPUT_DIR/$asset_name"
}

ROOT_DIR=$(CDPATH= cd -- "$(dirname "$0")/.." && pwd)
CARGO_TOML=$ROOT_DIR/Cargo.toml
CARGO_BIN=${META_RELEASE_CARGO:-cargo}
CROSS_BIN=${META_RELEASE_CROSS:-cross}
BINARY_NAME=${META_RELEASE_BINARY_NAME:-meta}
TARGET_DIR_ROOT=${META_RELEASE_TARGET_DIR:-$ROOT_DIR/target}
OUTPUT_ROOT=
EXPECTED_VERSION=${META_RELEASE_EXPECT_VERSION:-}
TARGETS=

while [ "$#" -gt 0 ]; do
  case "$1" in
    --output-dir)
      shift
      [ "$#" -gt 0 ] || die "--output-dir requires a value"
      OUTPUT_ROOT=$1
      ;;
    --expect-version)
      shift
      [ "$#" -gt 0 ] || die "--expect-version requires a value"
      EXPECTED_VERSION=$1
      ;;
    --target)
      shift
      [ "$#" -gt 0 ] || die "--target requires a value"
      append_target "$1"
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

PACKAGE_NAME=$(package_field name)
PACKAGE_VERSION=$(package_field version)
[ -n "$PACKAGE_NAME" ] || die "failed to parse package name from Cargo.toml"
[ -n "$PACKAGE_VERSION" ] || die "failed to parse package version from Cargo.toml"

if [ -n "$EXPECTED_VERSION" ]; then
  EXPECTED_VERSION=$(normalize_version "$EXPECTED_VERSION")
  if [ "$EXPECTED_VERSION" != "$PACKAGE_VERSION" ]; then
    die "expected Cargo.toml version '$EXPECTED_VERSION', found '$PACKAGE_VERSION'"
  fi
fi

OUTPUT_ROOT=${OUTPUT_ROOT:-$TARGET_DIR_ROOT/release-artifacts}
OUTPUT_DIR=$OUTPUT_ROOT/$PACKAGE_VERSION
CHECKSUM_MANIFEST=$OUTPUT_DIR/SHA256SUMS
HOST_TARGET=$(rustc -vV 2>/dev/null | awk '/^host: / { print $2 }')

require_command tar
rm -rf "$OUTPUT_DIR"
mkdir -p "$OUTPUT_DIR"
: > "$CHECKSUM_MANIFEST"

if [ -z "$TARGETS" ]; then
  TARGETS="aarch64-apple-darwin x86_64-apple-darwin x86_64-unknown-linux-musl aarch64-unknown-linux-musl"
fi

for target in $TARGETS; do
  target_metadata "$target" || die "unsupported release target '$target'"
  package_target "$target" "$TARGET_OS" "$TARGET_ARCH" "$TARGET_BUILDER"
done

printf '%s\n' "$CHECKSUM_MANIFEST"
