# ultrafast's MCP server over stdio, for MCP directories that build a server
# from its repository (Glama). It is the release image,
# ghcr.io/ultrafastmov/ultrafast (packaging/docker/Dockerfile, built and
# tested by Release), started as `ultrafast mcp`.
#
#   docker build -t ultrafast-mcp .
#   docker run -i --rm -v "$PWD:/work" ultrafast-mcp
#
# Documents and videos given as relative paths go in /work. Add --gpus all
# (NVIDIA Container Toolkit) to render on the GPU; without it the image
# renders on the CPU. --build-arg VERSION=X.Y.Z pins a release.
ARG VERSION=latest
FROM ghcr.io/ultrafastmov/ultrafast:${VERSION}
# The release image's health check is for `serve`.
HEALTHCHECK NONE
CMD ["mcp"]
