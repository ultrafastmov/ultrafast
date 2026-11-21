#!/usr/bin/env bash
# Packs a release archive: the ultrafast and uf binaries, the Slang compiler
# libraries they load (from the official Slang release ultrafast-shaderc
# pins, checked against its sha256), the licenses and the README.
#
#   packaging/dist.sh <target-triple> <dir holding ultrafast and uf> [out-dir]
#
# Writes <out-dir>/ultrafast-<version>-<target>.tar.gz and a .sha256 beside
# it (out-dir defaults to dist/), and prints the archive's path. Slang
# archives are kept in $ULTRAFAST_DIST_CACHE (default ~/.cache/ultrafast-dist).
# Runs on Linux and macOS (bash 3.2).
set -euo pipefail
TARGET=${1:?usage: dist.sh <target-triple> <bin-dir> [out-dir]}
BIN=${2:?usage: dist.sh <target-triple> <bin-dir> [out-dir]}
OUT=${3:-dist}
ROOT=$(cd "$(dirname "$0")/.." && pwd)
VERSION=$(sed -n 's/^version = "\(.*\)"$/\1/p' "$ROOT/Cargo.toml" | head -1)
SHADERC="$ROOT/crates/ultrafast-shaderc/src/lib.rs"
SLANG=$(sed -n 's/^pub const SLANG_VERSION: &str = "\(.*\)";$/\1/p' "$SHADERC")
[ -n "$VERSION" ] && [ -n "$SLANG" ] || { echo "dist.sh: cannot read the version or SLANG_VERSION" >&2; exit 1; }

case "$TARGET" in
  x86_64-unknown-linux-gnu*)  ASSET=linux-x86_64-glibc-2.28 ;;
  aarch64-unknown-linux-gnu*) ASSET=linux-aarch64-glibc-2.28 ;;
  aarch64-apple-darwin)       ASSET=macos-aarch64 ;;
  x86_64-apple-darwin)        ASSET=macos-x86_64 ;;
  *) echo "dist.sh: no Slang release for $TARGET" >&2; exit 1 ;;
esac
TARGET=${TARGET%%.2.*}   # x86_64-unknown-linux-gnu.2.28 (zigbuild's glibc pin) -> the plain triple
case "$TARGET" in *apple*) EXT=dylib ;; *) EXT=so ;; esac
# The sha256 is the line after the asset name in ultrafast-shaderc's table.
SHA=$(awk -v a="\"$ASSET\"," '$1 == a { getline; gsub(/[",[:space:]]/, ""); print; exit }' "$SHADERC")
[ ${#SHA} = 64 ] || { echo "dist.sh: no sha256 for $ASSET in $SHADERC" >&2; exit 1; }

sha256() { if command -v sha256sum >/dev/null; then sha256sum "$1" | cut -d' ' -f1; else shasum -a 256 "$1" | cut -d' ' -f1; fi; }

CACHE=${ULTRAFAST_DIST_CACHE:-${XDG_CACHE_HOME:-$HOME/.cache}/ultrafast-dist}
mkdir -p "$CACHE"
ARCHIVE="$CACHE/slang-$SLANG-$ASSET.tar.gz"
if [ ! -f "$ARCHIVE" ] || [ "$(sha256 "$ARCHIVE")" != "$SHA" ]; then
  echo "dist.sh: fetching Slang $SLANG ($ASSET)" >&2
  curl -fsSL --retry 3 -o "$ARCHIVE.part" \
    "https://github.com/shader-slang/slang/releases/download/v$SLANG/slang-$SLANG-$ASSET.tar.gz"
  mv "$ARCHIVE.part" "$ARCHIVE"
fi
GOT=$(sha256 "$ARCHIVE")
[ "$GOT" = "$SHA" ] || { echo "dist.sh: $ARCHIVE has sha256 $GOT, expected $SHA" >&2; exit 1; }

NAME="ultrafast-$VERSION-$TARGET"
WORK=$(mktemp -d)
trap 'rm -rf "$WORK"' EXIT
mkdir -p "$WORK/slang" "$WORK/$NAME/slang-$SLANG/lib"
tar -xzf "$ARCHIVE" -C "$WORK/slang"
S="$WORK/slang"
D="$WORK/$NAME/slang-$SLANG"
# What the engine loads: libslang (a link to libslang-compiler), spirv-opt
# inside slang-glslang, and the GLSL module agent shaders may import. Not
# LLVM (CPU targets), gfx, the tools or the headers.
if [ "$EXT" = so ]; then
  LIBS="libslang.so libslang-compiler.so libslang-compiler.so.0.$SLANG libslang-glslang-$SLANG.so libslang-glsl-module-$SLANG.so"
else
  LIBS="libslang.dylib libslang-compiler.dylib libslang-compiler.0.$SLANG.dylib libslang-glslang-$SLANG.dylib libslang-glsl-module-$SLANG.dylib"
fi
for f in $LIBS; do
  [ -e "$S/lib/$f" ] || [ -L "$S/lib/$f" ] || { echo "dist.sh: Slang $ASSET lacks lib/$f" >&2; exit 1; }
  cp -RP "$S/lib/$f" "$D/lib/"
done
cp "$S/LICENSE" "$D/LICENSE"
for d in LICENSES third-party-notices; do [ -d "$S/$d" ] && cp -R "$S/$d" "$D/$d"; done
find "$D" -name '._*' -delete

for b in ultrafast uf; do
  [ -x "$BIN/$b" ] || { echo "dist.sh: $BIN/$b missing" >&2; exit 1; }
  cp "$BIN/$b" "$WORK/$NAME/$b"
done
cp "$ROOT/LICENSE-APACHE" "$ROOT/LICENSE-MIT" "$ROOT/README.md" "$WORK/$NAME/"
mkdir -p "$WORK/$NAME/licenses"
cp "$ROOT"/examples/assets/fonts/licenses/{geist,geistmono,instrumentserif,fraunces,unbounded}-OFL.txt "$WORK/$NAME/licenses/"
cat > "$WORK/$NAME/THIRD-PARTY.md" <<EOF
# Bundled third-party software

- \`slang-$SLANG/\`: the Slang shader compiler $SLANG
  (https://github.com/shader-slang/slang), Apache-2.0 WITH LLVM-exception;
  its notices, and those of glslang and SPIRV-Tools inside it, are in
  \`slang-$SLANG/LICENSE\`, \`LICENSES/\` and \`third-party-notices/\`.
- Fonts built into the binaries: Geist, Geist Mono, Instrument Serif,
  Fraunces and Unbounded, under the SIL Open Font License 1.1 (\`licenses/\`).
- Rust crates compiled into the binaries: see \`Cargo.lock\` in the source
  (https://github.com/ultrafastmov/ultrafast), all under permissive licenses
  checked by cargo-deny.
EOF

mkdir -p "$OUT"
OUT=$(cd "$OUT" && pwd)
( cd "$WORK" && COPYFILE_DISABLE=1 tar -czf "$OUT/$NAME.tar.gz" "$NAME" )
( cd "$OUT" && echo "$(sha256 "$NAME.tar.gz")  $NAME.tar.gz" > "$NAME.tar.gz.sha256" )
echo "$OUT/$NAME.tar.gz"
