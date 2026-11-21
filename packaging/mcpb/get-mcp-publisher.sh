#!/usr/bin/env bash
# Downloads the MCP Registry's mcp-publisher at a pinned release into <dir>,
# checking its Sigstore bundle with cosign (as the registry's own guide does).
#
#   packaging/mcpb/get-mcp-publisher.sh <dir>
set -euo pipefail
DIR=${1:?usage: get-mcp-publisher.sh <dir>}
VERSION=v1.8.11
os=$(uname -s | tr '[:upper:]' '[:lower:]')
arch=$(uname -m | sed 's/x86_64/amd64/;s/aarch64/arm64/')
archive="mcp-publisher_${os}_${arch}.tar.gz"
base="https://github.com/modelcontextprotocol/registry/releases/download/$VERSION"
WORK=$(mktemp -d)
trap 'rm -rf "$WORK"' EXIT
curl -fsSL -o "$WORK/$archive" "$base/$archive"
curl -fsSL -o "$WORK/$archive.sigstore.json" "$base/$archive.sigstore.json"
cosign verify-blob \
  --bundle "$WORK/$archive.sigstore.json" \
  --certificate-identity "https://github.com/modelcontextprotocol/registry/.github/workflows/release.yml@refs/tags/$VERSION" \
  --certificate-oidc-issuer "https://token.actions.githubusercontent.com" \
  "$WORK/$archive" >&2
mkdir -p "$DIR"
tar -xzf "$WORK/$archive" -C "$DIR" mcp-publisher
"$DIR/mcp-publisher" --version >&2
