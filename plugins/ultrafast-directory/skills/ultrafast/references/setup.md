# Setting up ultrafast

This plugin works through the `uf` command line, run in the shell Claude
Code already has. Once `uf` is installed there is nothing else to set up.
The commands here install software: show them to the person, and run one
only when they ask you to.

## Install the engine

One binary, `ultrafast`, with `uf` as its short name:

```bash
brew install ultrafastmov/tap/ultrafast            # macOS, prebuilt
cargo install ultrafast --version 0.2.2 --locked   # from crates.io
```

The cargo line pins the release this guide is for; Homebrew installs the
tap's newest.

Prebuilt Linux archives and the other install options are at
https://ultrafast.mov/docs/#install.

It renders with Metal on macOS (Apple silicon) and Vulkan on Linux and
Windows (WSL2). H.264 and HEVC exports encode on the GPU (Metal, or NVENC
on NVIDIA); everything else (ProRes, AV1, VP9, GIF, PNG and EXR sequences,
sound alone) and exports on other GPUs go through ffmpeg, so install it
(`brew install ffmpeg`, `apt install ffmpeg`).

## Check it works

```bash
uf --version
uf info          # names the GPU it found
```

If the shell can't find `uf` after `cargo install`, add `~/.cargo/bin` to
`PATH`. Then ask for a video, for example: "a 10-second vertical teaser for
my pottery class, Saturdays, first class free".
