#!/usr/bin/env bash
set -euo pipefail

REPO="${WHISKER_REPO:-bugeng777/whisker}"
VERSION="${WHISKER_VERSION:-latest}"
INSTALL_DIR="${WHISKER_INSTALL_DIR:-$HOME/.local/bin}"

usage() {
  cat <<'EOF'
Install Whisker from a GitHub release.

Usage: install.sh [--help]

Environment:
  WHISKER_VERSION       Release tag to install (default: latest)
  WHISKER_INSTALL_DIR   Install directory (default: ~/.local/bin)
  WHISKER_REPO          GitHub repo (default: bugeng777/whisker)
  WHISKER_BASE_URL      Override the download base URL (for testing)
EOF
}

case "${1:-}" in
  -h|--help) usage; exit 0 ;;
  "") ;;
  *) echo "unknown argument: $1" >&2; usage >&2; exit 2 ;;
esac

os="$(uname -s)"
arch="$(uname -m)"
case "$os:$arch" in
  Darwin:arm64) target="aarch64-apple-darwin" ;;
  Darwin:x86_64) target="x86_64-apple-darwin" ;;
  Linux:x86_64|Linux:amd64) target="x86_64-unknown-linux-gnu" ;;
  Linux:aarch64|Linux:arm64) target="aarch64-unknown-linux-gnu" ;;
  *)
    echo "unsupported platform: $os $arch" >&2
    exit 1
    ;;
esac

if [ -n "${WHISKER_BASE_URL:-}" ]; then
  base="${WHISKER_BASE_URL%/}"
elif [ "$VERSION" = "latest" ]; then
  base="https://github.com/$REPO/releases/latest/download"
else
  case "$VERSION" in
    v*) ;;
    *) VERSION="v$VERSION" ;;
  esac
  base="https://github.com/$REPO/releases/download/$VERSION"
fi

asset="whisker-$target.tar.gz"
tmp="$(mktemp -d)"
trap 'rm -rf "$tmp"' EXIT

download() {
  if [ -n "${WHISKER_BASE_URL:-}" ]; then
    curl -fL --retry 3 --connect-timeout 10 -o "$2" "$1"
  else
    curl -fL --retry 3 --connect-timeout 10 --proto '=https' --tlsv1.2 -o "$2" "$1"
  fi
}

download "$base/$asset" "$tmp/$asset"
download "$base/SHA256SUMS" "$tmp/SHA256SUMS"

line="$(awk -v asset="$asset" '$2 == asset { print; exit }' "$tmp/SHA256SUMS")"
if [ -z "$line" ]; then
  echo "checksum missing for $asset" >&2
  exit 1
fi
if command -v sha256sum >/dev/null 2>&1; then
  (cd "$tmp" && printf '%s\n' "$line" | sha256sum -c -)
elif command -v shasum >/dev/null 2>&1; then
  (cd "$tmp" && printf '%s\n' "$line" | shasum -a 256 -c -)
else
  echo "sha256sum or shasum is required to verify the download" >&2
  exit 1
fi

tar -xzf "$tmp/$asset" -C "$tmp"
if [ ! -x "$tmp/whisker" ]; then
  echo "release archive does not contain a whisker binary" >&2
  exit 1
fi

mkdir -p "$INSTALL_DIR"
tmpbin="$INSTALL_DIR/.whisker.$$"
install -m 0755 "$tmp/whisker" "$tmpbin"
mv -f "$tmpbin" "$INSTALL_DIR/whisker"

echo "installed: $INSTALL_DIR/whisker"
"$INSTALL_DIR/whisker" --version || true
case ":$PATH:" in
  *":$INSTALL_DIR:"*) ;;
  *) echo "note: add $INSTALL_DIR to PATH, e.g. export PATH=\"$INSTALL_DIR:\$PATH\"" ;;
esac
