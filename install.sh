#!/bin/sh
# playdown-remote installer — detects OS/arch, grabs the latest release
# binary, and puts it on your PATH.
#   curl -fsSL https://raw.githubusercontent.com/z-alamsyah/playdown-remote/main/install.sh | sh
set -e

REPO="z-alamsyah/playdown-remote"

case "$(uname -s)" in
  Darwin) os=darwin ;;
  Linux)  os=linux ;;
  *) echo "Unsupported OS: $(uname -s) (macOS/Linux only)"; exit 1 ;;
esac
case "$(uname -m)" in
  arm64|aarch64) arch=arm64 ;;
  x86_64|amd64)  arch=x64 ;;
  *) echo "Unsupported architecture: $(uname -m)"; exit 1 ;;
esac

TAG=$(curl -fsSL "https://api.github.com/repos/$REPO/releases/latest" \
  | grep -m1 '"tag_name"' | cut -d'"' -f4)
[ -n "$TAG" ] || { echo "Could not resolve the latest release."; exit 1; }

ASSET="playdown-remote-$TAG-$os-$arch.tar.gz"
URL="https://github.com/$REPO/releases/download/$TAG/$ASSET"

TMP=$(mktemp -d)
trap 'rm -rf "$TMP"' EXIT
echo "Downloading $ASSET…"
curl -fsSL "$URL" -o "$TMP/$ASSET"
tar -C "$TMP" -xzf "$TMP/$ASSET"

DEST="/usr/local/bin"
if [ ! -w "$DEST" ]; then
  DEST="$HOME/.local/bin"
  mkdir -p "$DEST"
fi
install -m 755 "$TMP/playdown-remote" "$DEST/playdown-remote"

echo "Installed playdown-remote $TAG → $DEST/playdown-remote"
case ":$PATH:" in
  *":$DEST:"*) echo "Run: playdown-remote" ;;
  *) echo "NOTE: $DEST is not on your PATH — add this to your shell profile:"
     echo "  export PATH=\"$DEST:\$PATH\"" ;;
esac
