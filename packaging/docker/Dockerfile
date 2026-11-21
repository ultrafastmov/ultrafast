# ghcr.io/ultrafastmov/ultrafast: the ultrafast CLI and its self-hosted
# render service, with Slang (bundled in the release archive), ffmpeg, the
# Vulkan loader, Mesa's drivers for AMD and Intel GPUs (and lavapipe, a CPU
# fallback), eSpeak NG (voiceover) and Tesseract (ocr, reconstruct) for Linux,
# and fonts for every writing system.
#
#   docker run --gpus all ghcr.io/ultrafastmov/ultrafast:latest info
#   docker run --gpus all -p 8080:8080 -v ultrafast:/data ghcr.io/ultrafastmov/ultrafast:latest serve
#
# Renders, uploads and job records live in /data. Compiled kernels, pipelines
# and fetched fonts go to /cache/ultrafast; mount a volume there to keep them
# between containers.
#
# The build context is made by packaging/docker/build.sh: the unpacked Linux
# release archive as ultrafast/ and the NVIDIA Vulkan manifest.
FROM ubuntu:24.04
RUN apt-get update \
 && DEBIAN_FRONTEND=noninteractive apt-get install -y --no-install-recommends \
      ca-certificates curl ffmpeg libvulkan1 mesa-vulkan-drivers tini \
      espeak-ng tesseract-ocr \
      fonts-liberation fonts-noto-core fonts-noto-cjk fonts-noto-color-emoji \
 && rm -rf /var/lib/apt/lists/*
# The NVIDIA container toolkit mounts the driver; some hosts (Cloud Run)
# mount it without a Vulkan manifest, so ship one.
COPY nvidia_icd.json /etc/vulkan/icd.d/nvidia_icd.json
COPY ultrafast/ /opt/ultrafast/
RUN ln -s /opt/ultrafast/ultrafast /usr/local/bin/ultrafast \
 && ln -s /opt/ultrafast/uf /usr/local/bin/uf \
 && ultrafast --version \
 && mkdir -p /data /cache/ultrafast /work
ENV NVIDIA_VISIBLE_DEVICES=all \
    NVIDIA_DRIVER_CAPABILITIES=all \
    ULTRAFAST_SLANG_FETCH=0 \
    ULTRAFAST_CACHE_DIR=/cache/ultrafast \
    XDG_CACHE_HOME=/cache \
    ULTRAFAST_DATA=/data \
    PORT=8080
VOLUME ["/data"]
WORKDIR /work
EXPOSE 8080
# For `serve`: healthy once the GPU is ready (503 before).
HEALTHCHECK --interval=30s --timeout=5s --start-period=60s \
  CMD curl -fsS "http://127.0.0.1:${PORT}/v1/health" >/dev/null || exit 1
LABEL org.opencontainers.image.source="https://github.com/ultrafastmov/ultrafast" \
      org.opencontainers.image.url="https://ultrafast.mov" \
      org.opencontainers.image.description="The GPU video engine for agents: the ultrafast CLI and render service" \
      org.opencontainers.image.licenses="Apache-2.0 OR MIT" \
      io.modelcontextprotocol.server.name="io.github.ultrafastmov/ultrafast"
# tini as PID 1: forwards docker stop's SIGTERM and reaps ffmpeg children.
ENTRYPOINT ["/usr/bin/tini", "--", "ultrafast"]
CMD ["serve"]
