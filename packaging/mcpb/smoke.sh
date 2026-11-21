#!/usr/bin/env bash
# Installs an .mcpb the way Claude Desktop does and drives its server (see
# smoke.mjs): tools/list against the manifest, a relative path landing in
# ~/ultrafast, and with --render a frame on the GPU.
#
#   packaging/mcpb/smoke.sh <bundle.mcpb> [--render]
set -euo pipefail
BUNDLE=$(cd "$(dirname "${1:?usage: smoke.sh <bundle.mcpb> [--render]}")" && pwd)/$(basename "$1")
shift
HERE=$(cd "$(dirname "$0")" && pwd)
WORK=$(mktemp -d)
trap 'rm -rf "$WORK"' EXIT
cp "$HERE/smoke.mjs" "$WORK/"
(cd "$WORK" && npm init -y >/dev/null && npm install --silent --no-audit --no-fund @anthropic-ai/mcpb@2.1.2 >/dev/null)
node "$WORK/smoke.mjs" "$BUNDLE" "$@"
