#!/bin/sh
set -eu

REPO="lsk567/omar"
INSTALL_DIR="${OMAR_INSTALL_DIR:-/usr/local/bin}"

# Detect OS
OS="$(uname -s)"
case "$OS" in
  Linux*)  OS=linux ;;
  Darwin*) OS=darwin ;;
  *)       echo "Error: unsupported OS: $OS"; exit 1 ;;
esac

# Detect architecture
ARCH="$(uname -m)"
case "$ARCH" in
  x86_64|amd64)   ARCH=amd64 ;;
  arm64|aarch64)   ARCH=arm64 ;;
  *)               echo "Error: unsupported architecture: $ARCH"; exit 1 ;;
esac

# Get latest version or use OMAR_VERSION
if [ -z "${OMAR_VERSION:-}" ]; then
  OMAR_VERSION=$(curl -fsSL "https://api.github.com/repos/$REPO/releases/latest" \
    | grep '"tag_name"' \
    | sed 's/.*"v\(.*\)".*/\1/')
  if [ -z "$OMAR_VERSION" ]; then
    echo "Error: could not determine latest version. Set OMAR_VERSION manually."
    exit 1
  fi
fi

TARBALL="omar-${OS}-${ARCH}.tar.gz"
URL="https://github.com/$REPO/releases/download/v${OMAR_VERSION}/${TARBALL}"

echo "Installing omar v${OMAR_VERSION} (${OS}/${ARCH})"
echo "  -> ${INSTALL_DIR}"

# Download and extract
TMP="$(mktemp -d)"
trap 'rm -rf "$TMP"' EXIT

curl -fsSL "$URL" -o "$TMP/$TARBALL"
tar xzf "$TMP/$TARBALL" -C "$TMP" --strip-components=1

# Install (use sudo if needed)
SUDO=""
if [ ! -w "$INSTALL_DIR" ]; then
  SUDO="sudo"
  echo "Need sudo to install to $INSTALL_DIR"
fi

$SUDO install -d "$INSTALL_DIR"
$SUDO install "$TMP/omar" "$INSTALL_DIR/"
$SUDO install "$TMP/omar-slack" "$INSTALL_DIR/"
$SUDO install "$TMP/omar-computer" "$INSTALL_DIR/"
# The compiler every `.omar` program goes through. Tarballs from v0.4.0 on
# carry it; an older one pinned through OMAR_VERSION does not, and installing
# the rest is still better than failing.
if [ -f "$TMP/omarc" ]; then
  $SUDO install "$TMP/omarc" "$INSTALL_DIR/"
fi

echo ""
echo "Done! Installed:"
echo "  - omar"
echo "  - omar-slack"
echo "  - omar-computer"
if [ -f "$TMP/omarc" ]; then
  echo "  - omarc"
fi
echo ""
echo "Make sure tmux is installed:"
echo "  brew install tmux    # macOS"
echo "  apt install tmux     # Debian/Ubuntu"
echo ""
echo "Run:  omar"
