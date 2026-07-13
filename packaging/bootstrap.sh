#!/bin/sh
#
# One-line installer for kino-agent (Linux / systemd).
#
#   curl -fsSL https://raw.githubusercontent.com/Samarthegde/kino-agent/main/packaging/bootstrap.sh \
#     | sudo sh -s -- --relay-url wss://kino-relay.example.com
#
# Detects the CPU architecture, downloads the matching prebuilt binary from the
# latest GitHub release (no Rust toolchain needed), then registers the systemd
# service via install.sh. Override the source with KINO_REPO / KINO_VERSION.
set -eu

REPO="${KINO_REPO:-Samarthegde/kino-agent}"
VERSION="${KINO_VERSION:-latest}"
RELAY_URL=""
AGENT_ID=""

# ── args ─────────────────────────────────────────────────────────────────────
while [ $# -gt 0 ]; do
  case "$1" in
    --relay-url) RELAY_URL="${2:-}"; shift 2 ;;
    --agent-id)  AGENT_ID="${2:-}";  shift 2 ;;
    --version)   VERSION="${2:-}";   shift 2 ;;
    --repo)      REPO="${2:-}";      shift 2 ;;
    -h|--help)
      echo "Usage: bootstrap.sh --relay-url wss://host [--agent-id ID] [--version vX.Y.Z]"
      exit 0 ;;
    *) echo "error: unknown argument: $1" >&2; exit 1 ;;
  esac
done

die() { echo "error: $*" >&2; exit 1; }

[ -n "$RELAY_URL" ] || die "--relay-url is required (e.g. wss://kino-relay.example.com)"
[ "$(id -u)" = "0" ] || die "must run as root - pipe through sudo:
  curl -fsSL <url> | sudo sh -s -- --relay-url $RELAY_URL"

# curl or wget, whichever exists.
if command -v curl >/dev/null 2>&1; then
  dl() { curl -fsSL "$1" -o "$2"; }
  fetch() { curl -fsSL "$1"; }
elif command -v wget >/dev/null 2>&1; then
  dl() { wget -qO "$2" "$1"; }
  fetch() { wget -qO- "$1"; }
else
  die "need curl or wget installed"
fi

# ── platform detection ───────────────────────────────────────────────────────
[ "$(uname -s)" = "Linux" ] || die "this installer is for Linux; on other systems run kino-agent directly"

case "$(uname -m)" in
  x86_64|amd64)  ARCH="x86_64" ;;
  aarch64|arm64) ARCH="aarch64" ;;
  *) die "unsupported architecture: $(uname -m)" ;;
esac
TARGET="${ARCH}-unknown-linux-gnu"
ASSET="kino-agent-${TARGET}"

# ── resolve the release tag ──────────────────────────────────────────────────
if [ "$VERSION" = "latest" ]; then
  TAG=$(fetch "https://api.github.com/repos/${REPO}/releases/latest" \
        | grep '"tag_name":' | head -1 \
        | sed -E 's/.*"tag_name": *"([^"]+)".*/\1/')
  [ -n "$TAG" ] || die "could not determine the latest release of ${REPO} (no releases yet?)"
else
  TAG="$VERSION"
fi
echo "==> Installing kino-agent ${TAG} (${TARGET})"

# ── download binary + installer into a temp dir, then delegate ───────────────
TMP=$(mktemp -d)
trap 'rm -rf "$TMP"' EXIT

BASE="https://github.com/${REPO}/releases/download/${TAG}"
RAW="https://raw.githubusercontent.com/${REPO}/${TAG}/packaging"

echo "==> Downloading binary"
dl "${BASE}/${ASSET}" "$TMP/kino-agent" || die "failed to download ${BASE}/${ASSET}"
chmod +x "$TMP/kino-agent"

echo "==> Downloading installer"
dl "${RAW}/install.sh" "$TMP/install.sh"           || die "failed to download install.sh"
dl "${RAW}/kino-agent.service" "$TMP/kino-agent.service" || die "failed to download unit file"

# install.sh picks up the binary sitting next to it.
set -- --relay-url "$RELAY_URL"
[ -n "$AGENT_ID" ] && set -- "$@" --agent-id "$AGENT_ID"
sh "$TMP/install.sh" "$@"
