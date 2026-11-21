#!/usr/bin/env bash
# Builds ultrafast for Windows from WSL2 and installs it where Windows runs it.
#
#   tools/windows/build.sh           release build, install, `ultrafast-win info`
#   tools/windows/build.sh --test    the same, then every test on Windows
#
# WSL2 has no Vulkan driver for NVIDIA GPUs (only CUDA), so ultrafast runs as a
# Windows program on NVIDIA's Vulkan driver, cross-built here with mingw-w64.
# Installs ultrafast.exe with ffmpeg.exe and ffprobe.exe beside it (Windows finds
# programs next to the one running them) and Slang in slang-<version>/, all
# in $ULTRAFAST_WIN_DIR (default %USERPROFILE%\ultrafast), plus `ultrafast-win` in
# ~/.local/bin to run it from WSL. Needs rustup, mingw-w64
# (`sudo apt install mingw-w64`), curl and unzip. See docs/WINDOWS.md.
set -euo pipefail

SLANG_VERSION=2026.19
FFMPEG_VERSION=8.1
TARGET=x86_64-pc-windows-gnu
root="$(cd "$(dirname "$0")/../.." && pwd)"
# rustup and cargo even from a non-interactive shell (ssh, nohup).
[ -d "$HOME/.cargo/bin" ] && PATH="$HOME/.cargo/bin:$PATH"
powershell=/mnt/c/Windows/System32/WindowsPowerShell/v1.0/powershell.exe
log() { echo "[$(date -u +%H:%M:%SZ)] $*"; }

command -v x86_64-w64-mingw32-gcc >/dev/null || {
  echo "mingw-w64 is missing: sudo apt install mingw-w64" >&2
  exit 1
}
profile="$("$powershell" -NoProfile -Command '$env:USERPROFILE' | tr -d '\r')"
dest="${ULTRAFAST_WIN_DIR:-$(wslpath -u "$profile")/ultrafast}"
slang="$dest/slang-$SLANG_VERSION"
mkdir -p "$dest"
export CARGO_TARGET_X86_64_PC_WINDOWS_GNU_LINKER=x86_64-w64-mingw32-gcc
# The Linux Slang is only baked in as a default; Windows gets ULTRAFAST_SLANG_DIR.
export SLANG_DIR="${SLANG_DIR:-$root/../_data/toolchains/slang-$SLANG_VERSION}"
rustup target add "$TARGET" >/dev/null

log "building ultrafast.exe"
(cd "$root" && cargo build --release --target "$TARGET" -p ultrafast)
install -m 755 "$root/target/$TARGET/release/ultrafast.exe" "$dest/ultrafast.exe"

if [ ! -f "$slang/bin/slang.dll" ]; then
  log "fetching Slang $SLANG_VERSION for Windows"
  tmp="$(mktemp -d)"
  curl -fsSLo "$tmp/slang.zip" \
    "https://github.com/shader-slang/slang/releases/download/v$SLANG_VERSION/slang-$SLANG_VERSION-windows-x86_64.zip"
  mkdir -p "$slang"
  unzip -q -o "$tmp/slang.zip" -d "$slang"
  rm -rf "$tmp"
fi
if [ ! -f "$dest/ffmpeg.exe" ]; then
  log "fetching ffmpeg $FFMPEG_VERSION for Windows (with NVENC)"
  tmp="$(mktemp -d)"
  name="ffmpeg-n$FFMPEG_VERSION-latest-win64-gpl-$FFMPEG_VERSION"
  curl -fsSLo "$tmp/ffmpeg.zip" "https://github.com/BtbN/FFmpeg-Builds/releases/download/latest/$name.zip"
  unzip -q -j -o "$tmp/ffmpeg.zip" "$name/bin/ffmpeg.exe" "$name/bin/ffprobe.exe" -d "$dest"
  rm -rf "$tmp"
fi

mkdir -p "$HOME/.local/bin"
cat >"$HOME/.local/bin/ultrafast-win" <<EOF
#!/usr/bin/env bash
# ultrafast.exe (Windows, NVIDIA Vulkan) from WSL; written by tools/windows/build.sh.
export ULTRAFAST_SLANG_DIR="$slang"
export WSLENV="ULTRAFAST_SLANG_DIR/p:ULTRAFAST_CACHE_DIR/p:\${WSLENV:-}"
exec "$dest/ultrafast.exe" "\$@"
EOF
chmod +x "$HOME/.local/bin/ultrafast-win"
log "installed in $dest"
"$HOME/.local/bin/ultrafast-win" info

if [ "${1:-}" = "--test" ]; then
  cd "$root"
  log "building tests"
  cargo test --release --target "$TARGET" --workspace --no-run
  cp "$dest/ffmpeg.exe" "$dest/ffprobe.exe" "target/$TARGET/release/deps/"
  log "running tests on Windows"
  # Test binaries run from this checkout, so the absolute Linux paths baked
  # into them resolve through \\wsl.localhost.
  ULTRAFAST_SLANG_DIR="$slang" WSLENV="ULTRAFAST_SLANG_DIR/p:${WSLENV:-}" \
    cargo test --release --target "$TARGET" --workspace
fi
