#!/usr/bin/env bash
set -euo pipefail

cd "$(dirname "$0")/.."

echo "building release binary..."
cargo build --release

TARGET_DIR="${WHISKER_INSTALL_DIR:-$HOME/.local/bin}"
mkdir -p "$TARGET_DIR"
# Replace by rename so macOS sees a new inode; overwriting an existing binary
# in place can leave a stale code-signature cache and the kernel kills it with
# SIGKILL on launch.
tmp="$TARGET_DIR/.whisker.$$"
install -m 0755 target/release/whisker "$tmp"
mv -f "$tmp" "$TARGET_DIR/whisker"

echo "installed: $TARGET_DIR/whisker"

case ":$PATH:" in
  *":$TARGET_DIR:"*) ;;
  *) echo "note: add $TARGET_DIR to PATH, e.g. export PATH=\"$TARGET_DIR:\$PATH\"" ;;
esac
