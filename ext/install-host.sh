#!/bin/sh
# Install native messaging manifest for rdm-host.
# Usage: ./install-host.sh <chrome-extension-id>
set -e

EXT_ID="${1:?usage: install-host.sh <chrome-extension-id>}"
ROOT="$(cd "$(dirname "$0")/.." && pwd)"
HOST_NAME="com.rdm.host"

BIN="$ROOT/target/release/rdm-host"
[ -x "$BIN" ] || BIN="$ROOT/target/debug/rdm-host"
[ -x "$BIN" ] || { echo "error: build rdm-host first (cargo build)" >&2; exit 1; }

MANIFEST=$(printf '{"name":"%s","description":"RDM native messaging host","path":"%s","type":"stdio","allowed_origins":["chrome-extension://%s/"]}' \
  "$HOST_NAME" "$BIN" "$EXT_ID")

for d in \
  "$HOME/.config/google-chrome/NativeMessagingHosts" \
  "$HOME/.config/chromium/NativeMessagingHosts" \
  "$HOME/.config/vivaldi/NativeMessagingHosts" \
  "$HOME/.config/BraveSoftware/Brave-Browser/NativeMessagingHosts" \
  "$HOME/.config/Microsoft Edge/NativeMessagingHosts" \
  "$HOME/.mozilla/native-messaging-hosts"; do
  mkdir -p "$d"
  printf '%s\n' "$MANIFEST" > "$d/$HOST_NAME.json"
done

echo "installed: host=$BIN origin=chrome-extension://$EXT_ID/"
echo "start daemon with: $ROOT/target/debug/rdm-daemon"
