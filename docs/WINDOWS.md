# ultrafast on Windows / NVIDIA (RTX and friends)

On Windows ultrafast renders through the same Vulkan backend as on Linux
(`crates/ultrafast-gpu/src/vulkan.rs`), on NVIDIA's Windows Vulkan driver: the
same kernels, the same SPIR-V. It is built from WSL2, where the rest of the
toolchain lives, and runs as a Windows program. WSL2 itself has no Vulkan
driver for NVIDIA GPUs (only CUDA and the video libraries), so a Linux build
there would only find a software rasterizer.

## Build and install (in WSL2)

Needs rustup, mingw-w64 (`sudo apt install mingw-w64`), curl and unzip.

```bash
tools/windows/build.sh          # build ultrafast.exe and install it
ultrafast-win info                  # backend: vulkan, device: NVIDIA GeForce RTX 5090
tools/windows/build.sh --test   # also run every test on Windows
```

The script installs into `%USERPROFILE%\ultrafast` (override with
`ULTRAFAST_WIN_DIR`): `ultrafast.exe`, `ffmpeg.exe` and `ffprobe.exe` beside it
(Windows finds programs next to the one running them) and
`slang-2026.19\` (ultrafast also looks for a `slang*` folder beside the
executable). `ultrafast-win` in `~/.local/bin` runs it from WSL.

## Running

- `ultrafast-win <command> ...` from a directory inside WSL: relative paths and
  absolute Linux paths both work (the program's working directory is the
  `\\wsl.localhost\...` share, so `/home/...` resolves there). For heavy
  media, run from a directory on `C:` (`/mnt/c/...`) with relative paths;
  reading through the share is slower.
- `ultrafast-win live doc.json` opens the preview window on the Windows desktop:
  the timeline, keys and sound are the same as on macOS, and so is the
  control port (`ultrafast-win send '{"cmd":"pause"}'`, 127.0.0.1:7878).
- Caches (kernels, pipelines, fonts) live in `%LOCALAPPDATA%\ultrafast`.

Exports skip the CPU as on Linux: the last kernel writes NV12 into Vulkan
memory exported as a Windows handle, CUDA imports it and NVENC encodes it,
and ffmpeg only muxes. Measured on an RTX 5090 with the example films at
1080p: `glass` 228 fps (M3 Max: 91), `flow` 292 (157), `type` 265 (271), with
no readbacks; the preview window draws `glass` in about 0.3 ms of GPU a
frame.

## Current limits on this platform

- Without NVENC (no NVIDIA driver, or `ULTRAFAST_NVENC=0`) export reads frames
  back and pipes them to ffmpeg.
- Color emoji come from Noto Color Emoji, fetched once (10.7 MB, checked
  against its hash) into the font cache, because Windows' Segoe UI Emoji
  draws color with COLR layers that ultrafast does not read; the render service
  draws the same font, so local and cloud renders match.
- Video and sound decode through `ffmpeg` / `ffprobe` processes, as on
  Linux.
- Text set in macOS system fonts (Helvetica Neue, SF) falls back to Windows
  fonts; bundled fonts and Google Fonts render the same everywhere.
- `ultrafast voiceover` speaks with eSpeak NG (`espeak-ng` on the PATH) off
  macOS; `ocr` and `reconstruct` read text with Tesseract (`tesseract`).
- Symlinks need Developer Mode; where ultrafast would link an input it copies
  it instead.
