#!/bin/sh
# Generates the test clip used by examples/video.json (needs ffmpeg).
# Tagged BT.709 so every decoder agrees on its colors.
cd "$(dirname "$0")" && ffmpeg -loglevel error -y -f lavfi -i "testsrc2=size=1280x720:rate=30:duration=6" \
  -vf "scale=out_color_matrix=bt709:out_range=tv" -c:v libx264 -pix_fmt yuv420p -g 30 -crf 28 \
  -colorspace bt709 -color_primaries bt709 -color_trc bt709 -color_range tv testsrc.mp4
