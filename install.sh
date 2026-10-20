#!/bin/sh
# RDM installer (user-local, no root):
#   - cargo build --release (all binaries)
#   - binaries   -> ~/.local/bin/{rdm,rdm-daemon,rdm-ui,rdm-host}
#   - daemon     -> systemd user service (auto-start at login, auto-restart)
#   - host       -> browser native-messaging manifests (ext/install-host.sh)
#   - UI         -> ~/.local/share/applications/rdm.desktop + icon
# Usage: ./install.sh [extension-id]
set -e

ROOT="$(cd "$(dirname "$0")" && pwd)"
BIN_DIR="$HOME/.local/bin"
DATA_DIR="$HOME/.local/share/rdm"

command -v cargo >/dev/null || { echo "error: cargo not found" >&2; exit 1; }

echo "==> build (release)"
cargo build --release --manifest-path "$ROOT/Cargo.toml"

echo "==> install binaries -> $BIN_DIR"
mkdir -p "$BIN_DIR"
for b in rdm rdm-daemon rdm-ui rdm-host; do
  install -m 0755 "$ROOT/target/release/$b" "$BIN_DIR/$b"
done

echo "==> systemd user service"
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
# a manually started daemon still owns the socket: stop it so systemd takes over
pkill -x rdm-daemon 2>/dev/null || true
sleep 0.3
systemctl --user daemon-reload
systemctl --user enable --now rdm-daemon

echo "==> browser native-messaging host"
RDM_HOST_BIN="$BIN_DIR/rdm-host" "$ROOT/ext/install-host.sh" "${1:-}"

echo "==> desktop entry"
mkdir -p "$HOME/.local/share/applications" "$HOME/.local/share/icons/hicolor/128x128/apps" "$HOME/.local/share/icons/hicolor/256x256/apps"
install -m 0644 "$ROOT/crates/rdm-ui/icons/128x128.png" \
  "$HOME/.local/share/icons/hicolor/128x128/apps/rdm.png"
install -m 0644 "$ROOT/crates/rdm-ui/icons/128x128@2x.png" \
  "$HOME/.local/share/icons/hicolor/256x256/apps/rdm.png"
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
command -v kbuildsycoca6 >/dev/null && kbuildsycoca6 --noincremental 2>/dev/null || true

echo
echo "RDM installed:"
echo "  daemon : systemctl --user status rdm-daemon  (socket \$XDG_RUNTIME_DIR/rdm.sock)"
echo "  data   : $DATA_DIR (rdm.db + settings.json)"
echo "  ui     : $BIN_DIR/rdm-ui  (menu: RDM)"
echo "  reload the browser extension if it was updated: brave://extensions"
