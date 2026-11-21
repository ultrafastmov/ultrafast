#!/usr/bin/env bash
# Builds an MCP Bundle (.mcpb) from a release archive: `ultrafast mcp` as a
# binary server, with the Slang libraries, licenses and README beside it.
# Claude Desktop installs it with one click; the MCP Registry lists it.
#
#   packaging/mcpb/build.sh <ultrafast-<version>-<target>.tar.gz> [out-dir] [tools-binary]
#
# Writes <out-dir>/ultrafast-<version>-<os>-<arch>.mcpb and a .sha256 beside
# it (out-dir defaults to dist/), and prints the bundle's path. The manifest
# lists the tools the server reports; when the archive's binary can't run
# here (a macOS archive on Linux), pass an ultrafast binary of the same
# version that can, or set ULTRAFAST_MCPB_TOOLS_BIN. Needs python3 and npx.
set -euo pipefail
ARCHIVE=${1:?usage: build.sh <release archive> [out-dir] [tools-binary]}
OUT=${2:-dist}
TOOLS_BIN=${3:-${ULTRAFAST_MCPB_TOOLS_BIN:-}}
HERE=$(cd "$(dirname "$0")" && pwd)
MCPB_CLI="@anthropic-ai/mcpb@2.1.2"

base=$(basename "$ARCHIVE" .tar.gz)            # ultrafast-0.1.1-aarch64-apple-darwin
rest=${base#ultrafast-}
VERSION=${rest%%-*}
TARGET=${rest#*-}
case "$TARGET" in
  aarch64-apple-darwin)       PLATFORM=darwin; SLUG=macos-arm64 ;;
  x86_64-apple-darwin)        PLATFORM=darwin; SLUG=macos-x86_64 ;;
  x86_64-unknown-linux-gnu)   PLATFORM=linux;  SLUG=linux-x86_64 ;;
  aarch64-unknown-linux-gnu)  PLATFORM=linux;  SLUG=linux-arm64 ;;
  *) echo "build.sh: no bundle for $TARGET" >&2; exit 1 ;;
esac

WORK=$(mktemp -d)
trap 'rm -rf "$WORK"' EXIT
tar xzf "$ARCHIVE" -C "$WORK"
SRC="$WORK/$base"
STAGE="$WORK/bundle"
mkdir -p "$STAGE/server"
cp "$SRC/ultrafast" "$STAGE/server/"
cp -R "$SRC"/slang-* "$STAGE/server/"
cp -R "$SRC/README.md" "$SRC/THIRD-PARTY.md" "$SRC"/LICENSE-* "$SRC/licenses" "$STAGE/"
cp "$HERE/icon.png" "$STAGE/"
chmod 755 "$STAGE/server/ultrafast"

# The tools, as the server lists them.
[ -n "$TOOLS_BIN" ] || TOOLS_BIN="$STAGE/server/ultrafast"
printf '%s\n' \
  '{"jsonrpc":"2.0","id":1,"method":"initialize","params":{"protocolVersion":"2025-06-18","capabilities":{},"clientInfo":{"name":"mcpb-build","version":"0"}}}' \
  '{"jsonrpc":"2.0","method":"notifications/initialized"}' \
  '{"jsonrpc":"2.0","id":2,"method":"tools/list"}' |
  (cd "$WORK" && "$TOOLS_BIN" mcp) 2>/dev/null |
  python3 -I -c '
import json, sys
version = sys.argv[1]
for line in sys.stdin:
    m = json.loads(line)
    if m.get("id") == 1 and m["result"]["serverInfo"]["version"] != version:
        sys.exit("build.sh: the tools binary is %s, not %s" % (m["result"]["serverInfo"]["version"], version))
    if m.get("id") == 2:
        json.dump(m["result"], sys.stdout); sys.exit(0)
sys.exit("build.sh: the server listed no tools")' "$VERSION" > "$WORK/tools.json"

python3 -I "$HERE/manifest.py" "$VERSION" "$PLATFORM" "$WORK/tools.json" > "$STAGE/manifest.json"

mkdir -p "$OUT"
BUNDLE="$(cd "$OUT" && pwd)/ultrafast-$VERSION-$SLUG.mcpb"
npx --yes "$MCPB_CLI" validate "$STAGE/manifest.json" >&2
npx --yes "$MCPB_CLI" pack "$STAGE" "$BUNDLE" >&2
if command -v sha256sum >/dev/null; then sum=$(sha256sum "$BUNDLE"); else sum=$(shasum -a 256 "$BUNDLE"); fi
echo "${sum%% *}  $(basename "$BUNDLE")" > "$BUNDLE.sha256"
echo "$BUNDLE"
