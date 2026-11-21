#!/usr/bin/env bash
# Prints the Homebrew formula for a release, from the .sha256 files that
# packaging/dist.sh wrote beside its archives.
#
#   packaging/homebrew/formula.sh <dist-dir> [url-base] > Formula/ultrafast.rb
#
# url-base defaults to the GitHub release of the version in Cargo.toml
# (https://github.com/ultrafastmov/ultrafast/releases/download/v<version>);
# a file:// folder tests the formula before anything is published.
set -euo pipefail
DIST=${1:?usage: formula.sh <dist-dir> [url-base]}
HERE=$(cd "$(dirname "$0")" && pwd)
ROOT=$(cd "$HERE/../.." && pwd)
VERSION=$(sed -n 's/^version = "\(.*\)"$/\1/p' "$ROOT/Cargo.toml" | head -1)
URL=${2:-https://github.com/ultrafastmov/ultrafast/releases/download/v$VERSION}
sha() {
  local f="$DIST/ultrafast-$VERSION-$1.tar.gz.sha256"
  [ -f "$f" ] || { echo "formula.sh: $f missing" >&2; exit 1; }
  cut -d' ' -f1 "$f"
}
sed -e "s|@VERSION@|$VERSION|g" -e "s|@URL@|$URL|g" \
    -e "s|@SHA_AARCH64_APPLE_DARWIN@|$(sha aarch64-apple-darwin)|" \
    -e "s|@SHA_X86_64_UNKNOWN_LINUX_GNU@|$(sha x86_64-unknown-linux-gnu)|" \
    -e '1,3d' "$HERE/ultrafast.rb.in"
