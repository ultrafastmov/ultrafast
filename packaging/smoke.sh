#!/bin/sh
# The docs' quickstart against an installed ultrafast, in a scratch folder:
# version, info, new, check, sheet, render, export. Release CI runs it on
# every archive and on the image; run it by hand on a clean machine.
#
#   packaging/smoke.sh [path/to/uf]     (default: uf on PATH)
#
# Exports need ffmpeg on Linux; without it the export step is skipped and
# said so. SMOKE_EXPORT=0 skips it anyway (a slow software GPU).
set -eu
UF=${1:-uf}
case "$UF" in */*) UF=$(cd "$(dirname "$UF")" && pwd)/$(basename "$UF") ;; esac
DIR=$(mktemp -d)
trap 'rm -rf "$DIR"' EXIT
cd "$DIR"
step() { printf '\n== %s\n' "$*"; }
step "$UF --version";                 "$UF" --version
step "$UF info";                      "$UF" info
step "$UF new vertical film.json";    "$UF" new vertical film.json
step "$UF check film.json";           "$UF" check film.json
step "$UF sheet film.json --count 4"; "$UF" sheet film.json --count 4 --out sheet.png
step "$UF render film.json --at 2.5"; "$UF" render film.json --at 2.5 --out frame.png
test -s sheet.png && test -s frame.png
if [ "${SMOKE_EXPORT:-1}" = 0 ]; then
  echo "(export skipped: SMOKE_EXPORT=0)"
elif [ "$(uname -s)" = Darwin ] || command -v ffmpeg >/dev/null 2>&1; then
  step "$UF export film.json film.mp4 --to 1"; "$UF" export film.json film.mp4 --to 1
  test -s film.mp4
else
  echo "(export skipped: no ffmpeg on PATH, which exports on Vulkan use)"
fi
printf '\nsmoke: ok\n'
