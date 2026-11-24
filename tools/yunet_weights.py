#!/usr/bin/env python3
"""Writes crates/ultrafast-framing/models/yunet_2023mar.bin from OpenCV Zoo's
face_detection_yunet_2023mar.onnx (MIT, Shiqi Yu; see models/LICENSE-YuNet).

    pip install onnx numpy
    curl -LO https://github.com/opencv/opencv_zoo/raw/main/models/face_detection_yunet/face_detection_yunet_2023mar.onnx
    python3 tools/yunet_weights.py face_detection_yunet_2023mar.onnx crates/ultrafast-framing/models/yunet_2023mar.bin

The file is the convolutions' weights and biases (batch norm already folded
in by the export), in graph order, skipping the landmark heads, which face
tracking doesn't use: b"YUNET\\0\\0\\x01", a u32 tensor count, then per tensor
a u32 rank, u32 dims and little-endian f32 values. faces.rs builds the
network in the same order and checks every shape. The script checks the
graph is the one it knows, so a different export fails here, not there.
"""

import struct
import sys

import numpy as np
import onnx
from onnx import numpy_helper

src, dst = sys.argv[1], sys.argv[2]
graph = onnx.load(src).graph
init = {t.name: numpy_helper.to_array(t) for t in graph.initializer}

# The graph's operators in order (output layout ops left out): a 3x3
# stride-2 convolution, units of a pointwise and a depthwise convolution
# with ReLU after, max pools, two upsample-and-add steps, then per head a
# pointwise and a 3x3 convolution.
ops = [n.op_type for n in graph.node if n.op_type not in ("Transpose", "Reshape", "Sigmoid")]
unit = ["Conv", "Conv", "Relu"]
want = (
    ["Conv", "Relu"] + unit + ["MaxPool"] + unit * 4 + ["MaxPool"]
    + unit * 2 + ["MaxPool"] + unit * 2 + ["MaxPool"] + unit * 2
    + unit + ["Resize", "Add"] + unit + ["Resize", "Add"] + unit
    + ["Conv"] * 24
)
assert ops == want, f"not the YuNet 2023mar graph: {ops}"

convs = [n for n in graph.node if n.op_type == "Conv"]
assert len(convs) == 53
tensors = []
for n in convs[:47]:  # the last six: the landmark heads
    w, b = init[n.input[1]].astype("<f4"), init[n.input[2]].astype("<f4")
    tensors += [w, b]

with open(dst, "wb") as f:
    f.write(b"YUNET\0\0\x01")
    f.write(struct.pack("<I", len(tensors)))
    for t in tensors:
        f.write(struct.pack("<I", t.ndim))
        f.write(struct.pack(f"<{t.ndim}I", *t.shape))
        f.write(np.ascontiguousarray(t).tobytes())
print(f"{dst}: {len(tensors)} tensors, {sum(t.size for t in tensors)} values")
