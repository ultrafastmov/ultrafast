# Releasing ultrafast

Every install path the docs name, and what builds it:

| Install | Built by | Published by `release.yml` to |
|---|---|---|
| `cargo install ultrafast` | the workspace crates (`ultrafast` is `crates/ultrafast-cli`) | crates.io |
| prebuilt archives (macOS arm64, Linux x86_64 glibc 2.28+) | `dist.sh` | GitHub release assets |
| `brew install ultrafastmov/tap/ultrafast` | `homebrew/formula.sh` (from the archives) | `ultrafastmov/homebrew-tap`, `Formula/ultrafast.rb` |
| `docker run --gpus all ghcr.io/ultrafastmov/ultrafast:latest info` / `serve` | `docker/Dockerfile` (from the Linux archive) | ghcr.io, tags `X.Y.Z`, `X.Y`, `latest` |
| `docker build .` (MCP directories such as Glama build a server from its repository) | the root `Dockerfile`: the released image run as `ultrafast mcp` | nothing (built by whoever builds it) |
| `pip install ultrafastmov` | `sdk/python` | PyPI, at the version in `pyproject.toml` (skipped if published) |
| `npm install ultrafastmov` | `sdk/typescript` | npm, at the version in `package.json` (skipped if published) |
| Claude Desktop: `ultrafast-X.Y.Z-macos-arm64.mcpb`, `-linux-x86_64.mcpb` | `mcpb/build.sh` (from the archives) | GitHub release assets |
| the MCP Registry, `io.github.ultrafastmov/ultrafast` | `mcpb/server-json.py` (from the root `server.json`) | registry.modelcontextprotocol.io |

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

MCP Bundles: `packaging/mcpb/build.sh dist/ultrafast-*-x86_64-unknown-linux-gnu.tar.gz`
(for the macOS archive, pass a runnable `ultrafast` of the same version as
the third argument: the manifest lists the tools it serves), then
`packaging/mcpb/smoke.sh <bundle> [--render]` installs it the way Claude
Desktop does and drives it. The server starts through `/bin/sh` in a
working folder (a setting, default `~/ultrafast`) with Homebrew and
`/usr/local` on PATH: hosts start local servers in `/` without the shell's
PATH, and the tools resolve relative paths against the working directory.

## A release

1. Set `version` in the root `Cargo.toml` (and, for a new SDK release, the
   SDK's own version) and commit.
2. Optionally run **Release** by hand with publish off: it builds and tests
   every path (archives on Linux, a clean Ubuntu 22.04 and Rocky 8; macOS;
   brew install and audit of the real formula on both; the image with
   `info`, the quickstart and `serve`; both SDKs' tests and packages) and
   publishes nothing.
3. Push the tag `vX.Y.Z`. The run publishes crates, the GitHub release, the
   image, the formula, any SDK version not yet published, the MCP bundles
   and the MCP Registry entry. Nobody needs to sign in or approve anything.

How it signs in (set up once; nothing expires):

- crates.io, PyPI and npm: trusted publishing. Each registry trusts
  `ultrafastmov/ultrafast`'s `release.yml` in the GitHub environment
  `release`, so no registry tokens are stored. crates.io's tokens last 30
  minutes, so `publish-crates.sh` gets a new one for each crate. (crates.io
  only trusts crates that exist, so a brand-new crate needs a one-off
  `CARGO_REGISTRY_TOKEN` secret for its first release; the job uses that
  secret whenever it is set.)
- The Homebrew tap: the `ultrafastmov-release` GitHub App, installed on
  `ultrafastmov/homebrew-tap` only, with contents: write
  (`vars.RELEASE_APP_ID`, `secrets.RELEASE_APP_PRIVATE_KEY`). Its commits
  are signed as `ultrafastmov-release[bot]`.
- ghcr.io: the workflow's own `GITHUB_TOKEN`. The `ultrafast` package is
  public.
- The MCP Registry: the job's GitHub OIDC token (`mcp-publisher login
  github-oidc`), which proves the `io.github.ultrafastmov/` namespace. Each
  package proves itself: the bundles by their release URL and SHA-256, the
  crate by the line `mcp-name: io.github.ultrafastmov/ultrafast` in the
  README crates.io shows (visible text: crates.io strips HTML comments), the
  image by its `io.modelcontextprotocol.server.name` label. `server-json.py`
  leaves out the crate or the image if its marker is missing.
  `mcp-registry.yml` does the publishing: Release calls it on a tag, and
  running it by hand publishes a release that is already out.
