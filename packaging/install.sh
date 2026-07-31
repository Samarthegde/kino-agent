#!/usr/bin/env bash
#
# Install kino-agent as a systemd service on this host.
#
#   sudo ./install.sh --relay-url wss://relay.example.com [--agent-id <id>]
#
# Re-running is safe: it updates the binary and config in place and restarts
# the service. Omitting --agent-id keeps the existing one, or mints a new one
# on a first install.
set -euo pipefail

BIN_DEST=/usr/local/bin/kino-agent
CONF_DIR=/etc/kino-agent
ENV_FILE="$CONF_DIR/kino-agent.env"
UNIT_DEST=/etc/systemd/system/kino-agent.service
SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"

RELAY_URL=""
CONTROL_URL=""
AGENT_ID=""
TOKEN=""
AGENT_KEY=""

die() { echo "error: $*" >&2; exit 1; }

usage() {
  cat <<'EOF'
Install kino-agent as a systemd service on this host.

  # Fixed relay:
  sudo ./install.sh --relay-url wss://relay.example.com [--agent-id <id>] [--token <token>]
  # Kino Cloud (relay discovery + automatic token refresh):
  sudo ./install.sh --control-url https://control.example.com --agent-id <id> --agent-key <kca_...>

Re-running is safe: it updates the binary and config in place and restarts the
service, keeping any existing values you don't override.
EOF
}

while [[ $# -gt 0 ]]; do
  case "$1" in
    --relay-url)   RELAY_URL="${2:-}";   shift 2 ;;
    --control-url) CONTROL_URL="${2:-}"; shift 2 ;;
    --agent-id)    AGENT_ID="${2:-}";    shift 2 ;;
    --token)       TOKEN="${2:-}";       shift 2 ;;
    --agent-key)   AGENT_KEY="${2:-}";   shift 2 ;;
    -h|--help)     usage; exit 0 ;;
    *)             die "unknown argument: $1" ;;
  esac
done

[[ $EUID -eq 0 ]] || die "must run as root (try: sudo $0 ...)"
command -v systemctl >/dev/null || die "systemd not found; this installer targets systemd hosts"

# ── Reuse existing config when re-running ────────────────────────────────────
if [[ -f "$ENV_FILE" ]]; then
  # shellcheck disable=SC1090
  source "$ENV_FILE"
  RELAY_URL="${RELAY_URL:-${KINO_RELAY_URL:-}}"
  CONTROL_URL="${CONTROL_URL:-${KINO_CONTROL_URL:-}}"
  AGENT_ID="${AGENT_ID:-${KINO_AGENT_ID:-}}"
  TOKEN="${TOKEN:-${KINO_TOKEN:-}}"
  AGENT_KEY="${AGENT_KEY:-${KINO_AGENT_KEY:-}}"
fi

[[ -n "$RELAY_URL" || -n "$CONTROL_URL" ]] || die "--relay-url or --control-url is required"
[[ -z "$AGENT_KEY" || -n "$CONTROL_URL" ]] || die "--agent-key requires --control-url"

if [[ -z "$AGENT_ID" ]]; then
  if [[ -r /proc/sys/kernel/random/uuid ]]; then
    AGENT_ID="$(cat /proc/sys/kernel/random/uuid)"
  elif command -v uuidgen >/dev/null; then
    AGENT_ID="$(uuidgen)"
  else
    die "cannot generate an agent id; pass --agent-id explicitly"
  fi
  echo "==> Generated agent id: $AGENT_ID"
fi

# ── Locate or build the binary ───────────────────────────────────────────────
BIN_SRC=""
for candidate in \
  "$SCRIPT_DIR/kino-agent" \
  "$SCRIPT_DIR/../target/release/kino-agent"
do
  [[ -x "$candidate" ]] && { BIN_SRC="$candidate"; break; }
done

if [[ -z "$BIN_SRC" ]]; then
  command -v cargo >/dev/null || die "no prebuilt binary found and cargo is not installed"
  echo "==> No prebuilt binary found; building from source (this takes a minute)…"
  ( cd "$SCRIPT_DIR/.." && cargo build --release )
  BIN_SRC="$SCRIPT_DIR/../target/release/kino-agent"
  [[ -x "$BIN_SRC" ]] || die "build succeeded but $BIN_SRC is missing"
fi

# ── Install ──────────────────────────────────────────────────────────────────
echo "==> Installing binary to $BIN_DEST"
install -m 0755 "$BIN_SRC" "$BIN_DEST"

echo "==> Writing config to $ENV_FILE"
install -d -m 0755 "$CONF_DIR"
umask 077
cat > "$ENV_FILE" <<EOF
# Managed by kino-agent install.sh - edit and 'systemctl restart kino-agent'.
KINO_AGENT_ID=$AGENT_ID
# The agent logs at INFO by default; raise to debug/trace when diagnosing.
# On Linux it logs to stdout, which systemd captures into the journal.
RUST_LOG=info
EOF
# Secrets stay out of the unit's command line on purpose - this file is
# root-only (0600).
[[ -n "$RELAY_URL" ]] && echo "KINO_RELAY_URL=$RELAY_URL" >> "$ENV_FILE"
[[ -n "$CONTROL_URL" ]] && echo "KINO_CONTROL_URL=$CONTROL_URL" >> "$ENV_FILE"
[[ -n "$TOKEN" ]] && echo "KINO_TOKEN=$TOKEN" >> "$ENV_FILE"
[[ -n "$AGENT_KEY" ]] && echo "KINO_AGENT_KEY=$AGENT_KEY" >> "$ENV_FILE"
chmod 0600 "$ENV_FILE"

echo "==> Installing unit to $UNIT_DEST"
install -m 0644 "$SCRIPT_DIR/kino-agent.service" "$UNIT_DEST"

systemctl daemon-reload
systemctl enable --now kino-agent
sleep 1

echo
if systemctl is-active --quiet kino-agent; then
  echo "✅ kino-agent is running."
else
  echo "⚠️  kino-agent is not active. Recent logs:"
  journalctl -u kino-agent -n 20 --no-pager || true
fi

cat <<EOF

  Relay:    ${RELAY_URL:-"(discovered via kino-control)"}
  Control:  ${CONTROL_URL:-"none"}
  Agent ID: $AGENT_ID
  Token:    $([[ -n "$AGENT_KEY" ]] && echo "agent key set (auto-refreshing)" || { [[ -n "$TOKEN" ]] && echo "set (stored in $ENV_FILE)" || echo "none"; })

Add this host in Kino SSH Manager with connection mode "Kino Agent",
using the relay URL and agent ID above.

  Logs:     journalctl -u kino-agent -f
  Restart:  systemctl restart kino-agent
  Remove:   systemctl disable --now kino-agent && rm -rf $UNIT_DEST $CONF_DIR $BIN_DEST
EOF
