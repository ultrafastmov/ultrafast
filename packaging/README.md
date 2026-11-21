# Releasing ultrafast

Every install path the docs name, and what builds it:

| Install | Built by | Published by `release.yml` to |
|---|---|---|
| `cargo install ultrafast` | the workspace crates (`ultrafast` is `crates/ultrafast-cli`) | crates.io |
| prebuilt archives (macOS arm64, Linux x86_64 glibc 2.28+) | `dist.sh` | GitHub release assets |
| `brew install ultrafastmov/tap/ultrafast` | `homebrew/formula.sh` (from the archives) | `ultrafastmov/homebrew-tap`, `Formula/ultrafast.rb` |
| `docker run --gpus all ghcr.io/ultrafastmov/ultrafast:latest info` / `serve` | `docker/Dockerfile` (from the Linux archive) | ghcr.io, tags `X.Y.Z`, `X.Y`, `latest` |

Slang: the archives carry the compiler libraries ultrafast loads (libslang,
libslang-compiler, slang-glslang for spirv-opt, the GLSL module; not LLVM or
the tools), from the official release pinned in
`crates/ultrafast-shaderc/src/lib.rs` (`SLANG_VERSION` and the sha256 per
platform). Builds without one (`cargo install`) fetch the same release once
into the cache. The image adds ffmpeg, the Vulkan loader, Mesa (AMD, Intel,
and lavapipe without a GPU), tini and fonts for every script.

## Locally

```bash
# a release archive (Linux: cargo-zigbuild pins glibc 2.28)
cargo zigbuild --release --target x86_64-unknown-linux-gnu.2.28 -p ultrafast
packaging/dist.sh x86_64-unknown-linux-gnu target/x86_64-unknown-linux-gnu/release dist
# on a Mac: cargo build --release -p ultrafast && packaging/dist.sh aarch64-apple-darwin target/release dist

packaging/smoke.sh path/to/uf                      # the quickstart: info, new, check, sheet, render, export
packaging/docker/build.sh dist/ultrafast-*-x86_64-unknown-linux-gnu.tar.gz
packaging/homebrew/formula.sh dist file://$PWD/dist > ultrafast.rb   # needs both platforms' .sha256
cargo package --workspace                           # what crates.io gets, built from the packed sources
```

## A release

1. Set `version` in the root `Cargo.toml` and commit.
2. Run **Release** by hand with publish off: it builds and tests every path
   (archives on Linux, a clean Ubuntu 22.04 and Rocky 8; macOS; brew install
   and audit of the real formula on both; the image with `info`, the
   quickstart and `serve`) and publishes nothing.
3. Push the tag `vX.Y.Z`. The same run then publishes crates, the GitHub
   release, the image and the formula.

Once, before the first release:

- The repository must be public: brew and anonymous downloads fetch the
  release assets from it.
- `ultrafastmov/homebrew-tap` must exist (an empty public repository), with a
  fine-grained token that can write to it saved as `HOMEBREW_TAP_TOKEN`.
- A crates.io token that can publish new crates saved as
  `CARGO_REGISTRY_TOKEN` (later releases can switch to trusted publishing for
  each crate and drop it).
- After the first image push, make the `ultrafast` package on ghcr.io public
  (organization packages start private) and link it to the repository.
