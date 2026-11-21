# Contributing to ultrafast

Thanks for helping. Issues and pull requests are welcome.

## Building

You need Rust 1.91+, [Slang](https://github.com/shader-slang/slang/releases)
2026.19 or newer (fetched on first use if you have none) and ffmpeg (see the
README). Then:

```bash
export SLANG_DIR=/path/to/slang      # optional: the folder with lib/libslang.*
cargo build --release
cargo test --release --workspace
```

GPU tests run on Metal (macOS) or Vulkan (Linux, Windows). CI runs the CPU
tests on every push; please run the whole suite on a GPU before sending a
change to the renderer.

## Changes

- Keep a pull request to one change, with a test when it fixes a bug or
  adds an op.
- `cargo fmt` (the repository's `rustfmt.toml`) and `cargo clippy` should be
  clean.
- A new or changed op updates its schema in `crates/ultrafast-ops` and
  `docs/OPS.md` (`ultrafast ops` prints the same vocabulary as JSON).
- Changes to the HTTP contract in `docs/service/API.md` are additive, with
  fixtures in `docs/service/fixtures` that the service's tests parse.

## License

By contributing you agree that your contribution is licensed under
Apache-2.0 OR MIT, as the rest of the project is.
