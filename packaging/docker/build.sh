#!/usr/bin/env bash
# Builds the public image from a Linux release archive (packaging/dist.sh).
#
#   packaging/docker/build.sh <ultrafast-VERSION-x86_64-unknown-linux-gnu.tar.gz> [tag ...]
#
# Tags default to ghcr.io/ultrafastmov/ultrafast:<version>. Uses docker, or
# podman when there is no docker ($DOCKER picks one). Release CI builds the
# same context with buildx and pushes it.
set -euo pipefail
ARCHIVE=${1:?usage: build.sh <linux release archive> [tag ...]}
shift
HERE=$(cd "$(dirname "$0")" && pwd)
ROOT=$(cd "$HERE/../.." && pwd)
NAME=$(basename "$ARCHIVE" .tar.gz)
VERSION=$(echo "$NAME" | sed -n 's/^ultrafast-\([^-]*\)-.*/\1/p')
[ $# -gt 0 ] || set -- "ghcr.io/ultrafastmov/ultrafast:$VERSION"
DOCKER=${DOCKER:-$(command -v docker || command -v podman)}
CTX=$(mktemp -d)
trap 'rm -rf "$CTX"' EXIT
tar -xzf "$ARCHIVE" -C "$CTX"
mv "$CTX/$NAME" "$CTX/ultrafast"
cp "$HERE/Dockerfile" "$HERE/nvidia_icd.json" "$CTX/"
TAGS=()
for t in "$@"; do TAGS+=(-t "$t"); done
# podman writes OCI images unless told otherwise, which drop HEALTHCHECK.
case "$(basename "$DOCKER")" in podman*) TAGS+=(--format docker) ;; esac
"$DOCKER" build "${TAGS[@]}" "$CTX" >&2
echo "$1"
