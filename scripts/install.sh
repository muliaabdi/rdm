#!/usr/bin/env bash
# RDM Standalone Web Installer
# Usage:
#   bash <(curl -fsSL https://raw.githubusercontent.com/muliaabdi/rdm/main/scripts/install.sh)
#
# Environment variables:
#   RDM_VERSION      Tag/version to install (default: latest release)
#   RDM_BIN_DIR      Directory for executables (default: ~/.local/bin)
#   RDM_EXT_ID       Chrome extension ID (default: enhihiehgiamccjmplnbpgjfbcglkaci)

set -euo pipefail

REPO="muliaabdi/rdm"
BIN_DIR="${RDM_BIN_DIR:-$HOME/.local/bin}"
DATA_DIR="$HOME/.local/share/rdm"
EXT_ID="${RDM_EXT_ID:-enhihiehgiamccjmplnbpgjfbcglkaci}"

log() { printf "\033[1;34m==>\033[0m %s\n" "$*"; }
err() { printf "\033[1;31merror:\033[0m %s\n" "$*" >&2; exit 1; }

# Pre-checks
[ "$(uname -s)" = "Linux" ] || err "This installer currently supports Linux only."
command -v curl >/dev/null 2>&1 || err "'curl' is required to run this installer."
command -v tar >/dev/null 2>&1 || err "'tar' is required to run this installer."

ARCH_RAW="$(uname -m)"
case "$ARCH_RAW" in
  x86_64|amd64) ARCH="x86_64" ;;
  aarch64|arm64) ARCH="aarch64" ;;
  *) err "Unsupported architecture: $ARCH_RAW. Supported architectures: x86_64, aarch64." ;;
esac

log "Determining release version for $REPO..."
if [ -n "${RDM_VERSION:-}" ]; then
  TAG="$RDM_VERSION"
else
  # Resolve latest release from GitHub API
  TAG="$(curl -fsSL "https://api.github.com/repos/$REPO/releases/latest" 2>/dev/null | grep '"tag_name":' | head -1 | sed -E 's/.*"([^"]+)".*/\1/' || true)"
  if [ -z "$TAG" ]; then
    # Fallback to default tag if no GitHub release yet
    TAG="v0.1.0"
  fi
fi

ASSET_NAME="rdm-linux-${ARCH}.tar.gz"
DOWNLOAD_URL="https://github.com/$REPO/releases/download/$TAG/$ASSET_NAME"

TMP_DIR="$(mktemp -d -t rdm-install-XXXXXX)"
cleanup() { rm -rf "$TMP_DIR"; }
trap cleanup EXIT INT TERM

log "Downloading RDM $TAG ($ASSET_NAME)..."
if ! curl -fL --progress-bar "$DOWNLOAD_URL" -o "$TMP_DIR/$ASSET_NAME"; then
  err "Failed to download $DOWNLOAD_URL. Make sure release $TAG exists and contains $ASSET_NAME."
fi

log "Stopping any running daemon instances..."
systemctl --user stop rdm-daemon 2>/dev/null || true
pkill -x rdm-daemon 2>/dev/null || true
sleep 0.3

log "Installing binaries into $BIN_DIR..."
mkdir -p "$BIN_DIR"
tar -xzf "$TMP_DIR/$ASSET_NAME" -C "$TMP_DIR"

# Install binaries
for b in rdm rdm-daemon rdm-ui rdm-host; do
  if [ -f "$TMP_DIR/$b" ]; then
    install -m 0755 "$TMP_DIR/$b" "$BIN_DIR/$b"
  fi
done

log "Configuring systemd user service..."
mkdir -p "$DATA_DIR" "$HOME/.config/systemd/user"
cat > "$HOME/.config/systemd/user/rdm-daemon.service" <<EOF
[Unit]
Description=RDM download daemon

[Service]
ExecStart=$BIN_DIR/rdm-daemon
Restart=on-failure
RestartSec=1

[Install]
WantedBy=default.target
EOF

if command -v systemctl >/dev/null 2>&1; then
  systemctl --user daemon-reload
  systemctl --user reset-failed rdm-daemon 2>/dev/null || true
  systemctl --user enable --now rdm-daemon || true
fi

log "Registering browser native-messaging hosts..."
HOST_NAME="com.rdm.host"
HOST_PATH="$BIN_DIR/rdm-host"
MANIFEST=$(printf '{"name":"%s","description":"RDM native messaging host","path":"%s","type":"stdio","allowed_origins":["chrome-extension://%s/"]}' \
  "$HOST_NAME" "$HOST_PATH" "$EXT_ID")

for d in \
  "$HOME/.config/google-chrome/NativeMessagingHosts" \
  "$HOME/.config/chromium/NativeMessagingHosts" \
  "$HOME/.config/vivaldi/NativeMessagingHosts" \
  "$HOME/.config/microsoft-edge/NativeMessagingHosts" \
  "$HOME/.config/BraveSoftware/Brave-Browser/NativeMessagingHosts" \
  "$HOME/.config/BraveSoftware/Brave-Origin/NativeMessagingHosts" \
  "$HOME/.mozilla/native-messaging-hosts"; do
  mkdir -p "$d"
  printf '%s\n' "$MANIFEST" > "$d/$HOST_NAME.json"
done

log "Installing desktop application shortcut..."
ICON_DIR_128="$HOME/.local/share/icons/hicolor/128x128/apps"
ICON_DIR_256="$HOME/.local/share/icons/hicolor/256x256/apps"
mkdir -p "$HOME/.local/share/applications" "$ICON_DIR_128" "$ICON_DIR_256"

if [ -f "$TMP_DIR/icons/128x128.png" ]; then
  install -m 0644 "$TMP_DIR/icons/128x128.png" "$ICON_DIR_128/rdm.png"
fi
if [ -f "$TMP_DIR/icons/128x128@2x.png" ]; then
  install -m 0644 "$TMP_DIR/icons/128x128@2x.png" "$ICON_DIR_256/rdm.png"
fi

cat > "$HOME/.local/share/applications/rdm.desktop" <<EOF
[Desktop Entry]
Type=Application
Name=RDM
Comment=Download manager
Exec=$BIN_DIR/rdm-ui
Icon=rdm
Terminal=false
StartupWMClass=com.rdm.ui
Categories=Network;FileTransfer;
EOF

update-desktop-database "$HOME/.local/share/applications" 2>/dev/null || true
gtk-update-icon-cache -f -t "$HOME/.local/share/icons/hicolor" 2>/dev/null || true
command -v kbuildsycoca6 >/dev/null 2>&1 && kbuildsycoca6 --noincremental 2>/dev/null || true

printf "\n\033[1;32m✓ RDM successfully installed!\033[0m\n\n"
echo "  CLI      : rdm --help"
echo "  UI       : Launch 'RDM' from app launcher, or run '$BIN_DIR/rdm-ui &'"
echo "  Daemon   : systemctl --user status rdm-daemon"
echo "  Extension: Install or reload extension ID: $EXT_ID"
echo
