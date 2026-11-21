//! Built-in components: common effects expressed as ultrafast graphs, available
//! to every document by op name (a document's own component of the same
//! name wins). They expand like any component, so they fuse with the rest
//! of the graph and stay fully editable when copied into a document.

use std::sync::OnceLock;
use ultrafast_graph::{Component, Document};

const SOURCES: &[(&str, &str)] = &[
    (
        "drop_shadow",
        r##"{
        "params": { "color": "#000000", "opacity": 0.6, "offset": [8, 8], "radius": 12 },
        "inputs": ["source"],
        "output": "out",
        "nodes": {
            "tint": { "op": "solid", "params": { "color": "$color" } },
            "silhouette": { "op": "mask", "inputs": { "source": "tint", "matte": "$source" } },
            "soft": { "op": "blur", "inputs": { "source": "silhouette" }, "params": { "radius": "$radius" } },
            "moved": { "op": "transform", "inputs": { "source": "soft" }, "params": { "anchor": [0, 0], "position": "$offset" } },
            "faded": { "op": "opacity", "inputs": { "source": "moved" }, "params": { "opacity": "$opacity" } },
            "out": { "op": "over", "inputs": { "fg": "$source", "bg": "faded" } }
        }}"##,
    ),
    (
        "glow",
        r##"{
        "params": { "radius": 16, "intensity": 1.0 },
        "inputs": ["source"],
        "output": "out",
        "nodes": {
            "soft": { "op": "blur", "inputs": { "source": "$source" }, "params": { "radius": "$radius" } },
            "strength": { "op": "opacity", "inputs": { "source": "soft" }, "params": { "opacity": "$intensity" } },
            "out": { "op": "blend", "inputs": { "fg": "strength", "bg": "$source" }, "params": { "mode": "add" } }
        }}"##,
    ),
    (
        "vignette",
        r##"{
        "params": { "inner": "#00000000", "outer": "#000000b0" },
        "inputs": ["source"],
        "output": "out",
        "nodes": {
            "shade": { "op": "gradient", "params": { "kind": "radial", "a": "$inner", "b": "$outer" } },
            "out": { "op": "over", "inputs": { "fg": "shade", "bg": "$source" } }
        }}"##,
    ),
    (
        "stroke_outline",
        r##"{
        "params": { "color": "#ffffff", "width": 4 },
        "inputs": ["source"],
        "output": "out",
        "nodes": {
            "tint": { "op": "solid", "params": { "color": "$color" } },
            "grown": { "op": "blur", "inputs": { "source": "$source" }, "params": { "radius": "$width" } },
            "edge": { "op": "shader", "inputs": { "in0": "grown" }, "params": { "code": "float4 shade(float2 pos, float px) {\n  // The blurred coverage, cut where it is about `width` px out.\n  float k = smoothstep(0.08, 0.2, input(0, pos).a);\n  return float4(k, k, k, k);\n}" } },
            "silhouette": { "op": "mask", "inputs": { "source": "tint", "matte": "edge" }, "params": { "mode": "alpha" } },
            "out": { "op": "over", "inputs": { "fg": "$source", "bg": "silhouette" } }
        }}"##,
    ),
    // Looks: the styles of examples/films, as reusable effects.
    (
        "bloom",
        r##"{
 "params": {
  "threshold": 0.75,
  "radius": 32,
  "intensity": 0.6,
  "wide": 0.3
 },
 "inputs": [
  "source"
 ],
 "output": "out",
 "nodes": {
  "hot": {
   "op": "levels",
   "inputs": {
    "source": "$source"
   },
   "params": {
    "in_black": "$threshold",
    "in_white": 2.0
   }
  },
  "near": {
   "op": "blur",
   "inputs": {
    "source": "hot"
   },
   "params": {
    "radius": "$radius"
   }
  },
  "far": {
   "op": "blur",
   "inputs": {
    "source": "hot"
   },
   "params": {
    "radius": 140
   }
  },
  "lit": {
   "op": "blend",
   "inputs": {
    "fg": "near",
    "bg": "$source"
   },
   "params": {
    "mode": "add",
    "opacity": "$intensity"
   }
  },
  "out": {
   "op": "blend",
   "inputs": {
    "fg": "far",
    "bg": "lit"
   },
   "params": {
    "mode": "add",
    "opacity": "$wide"
   }
  }
 }
}"##,
    ),
    (
        "grain",
        r##"{
 "params": {
  "amount": 0.07
 },
 "inputs": [
  "source"
 ],
 "output": "out",
 "nodes": {
  "noise": {
   "op": "shader",
   "params": {
    "code": "float4 shade(float2 pos, float px) {\n  float2 p = floor(pos) + floor(time() * 24.0) * float2(37.0, 17.0);\n  float h = frac(sin(dot(p, float2(12.9898, 78.233))) * 43758.5453);\n  float g = frac(sin(dot(p + 3.1, float2(39.346, 11.135))) * 24634.6345);\n  float n = (h + g) * 0.5;\n  return float4(float3(n), 1.0);\n}"
   }
  },
  "out": {
   "op": "blend",
   "inputs": {
    "fg": "noise",
    "bg": "$source"
   },
   "params": {
    "mode": "overlay",
    "opacity": "$amount"
   }
  }
 }
}"##,
    ),
    (
        "aberration",
        r##"{
 "params": {
  "amount": 8
 },
 "inputs": [
  "source"
 ],
 "output": "out",
 "nodes": {
  "out": {
   "op": "shader",
   "inputs": {
    "in0": "$source"
   },
   "params": {
    "code": "float4 shade(float2 pos, float px) {\n  float2 c = size() * 0.5;\n  float2 dir = (pos - c) / size().y;\n  float k = param(0);\n  float4 g = input(0, pos);\n  float2 lo = float2(0.5), hi = size() - 0.5;\n  float r = input(0, clamp(pos + dir * k, lo, hi)).r;\n  float b = input(0, clamp(pos - dir * k, lo, hi)).b;\n  return float4(r, g.g, b, g.a);\n}",
    "u0": "$amount"
   }
  }
 }
}"##,
    ),
    (
        "repeater",
        r##"{
 "params": { "count": 6, "dx": 0, "dy": 0, "rotate": 0, "scale": 1, "fade": 1, "cx": 0, "cy": 0 },
 "inputs": ["source"],
 "output": "out",
 "nodes": {
  "out": {
   "op": "shader",
   "inputs": { "in0": "$source" },
   "params": {
    "code": "float4 shade(float2 pos, float px) {\n  int n = clamp(int(param(0) + 0.5), 1, 128);\n  float2 step = float2(param(1), param(2));\n  float rot = param(3) * 0.01745329;\n  float2 c = float2(param(6), param(7));\n  float4 acc = float4(0.0);\n  // Back to front, so copy 0 (the original) ends on top.\n  for (int i = n - 1; i >= 0; i--) {\n    float k = float(i);\n    float a = -rot * k;\n    float s = pow(max(param(4), 1e-3), k);\n    float2 q = pos - step * k - c;\n    q = float2(cos(a) * q.x - sin(a) * q.y, sin(a) * q.x + cos(a) * q.y) / s + c;\n    float4 v = input(0, q) * pow(saturate(param(5)), k);\n    acc = v + acc * (1.0 - v.a);\n  }\n  return acc;\n}",
    "u0": "$count", "u1": "$dx", "u2": "$dy", "u3": "$rotate", "u4": "$scale", "u5": "$fade", "u6": "$cx", "u7": "$cy"
   }
  }
 }
}"##,
    ),
    (
        "chroma_key",
        r##"{
 "params": { "r": 0.0, "g": 0.69, "b": 0.25, "tolerance": 0.12, "softness": 0.08, "spill": 0.8 },
 "inputs": ["source"],
 "output": "out",
 "nodes": {
  "out": {
   "op": "shader",
   "inputs": { "in0": "$source" },
   "params": {
    "code": "float4 shade(float2 pos, float px) {\n  float4 c = input(0, pos);\n  if (c.a <= 1e-5) return c;\n  float3 rgb = linear_to_srgb(saturate(c.rgb / c.a));\n  float3 key = float3(param(0), param(1), param(2));\n  // Distance in chroma (Cb, Cr), so shadows on the screen key too.\n  float2 ch = float2(dot(rgb, float3(-0.169, -0.331, 0.5)), dot(rgb, float3(0.5, -0.419, -0.081)));\n  float2 kc = float2(dot(key, float3(-0.169, -0.331, 0.5)), dot(key, float3(0.5, -0.419, -0.081)));\n  float d = length(ch - kc);\n  float a = smoothstep(param(3), param(3) + max(param(4), 1e-4), d);\n  // Spill: the key's channel may not exceed the other two.\n  float s = param(5);\n  if (key.g >= key.r && key.g >= key.b) rgb.g = lerp(rgb.g, min(rgb.g, max(rgb.r, rgb.b)), s);\n  else if (key.b >= key.r) rgb.b = lerp(rgb.b, min(rgb.b, max(rgb.r, rgb.g)), s);\n  else rgb.r = lerp(rgb.r, min(rgb.r, max(rgb.g, rgb.b)), s);\n  float alpha = a * c.a;\n  return float4(srgb_to_linear(rgb) * alpha, alpha);\n}",
    "u0": "$r", "u1": "$g", "u2": "$b", "u3": "$tolerance", "u4": "$softness", "u5": "$spill"
   }
  }
 }
}"##,
    ),
    (
        "halftone",
        r##"{
 "params": {
  "cell": 9,
  "angle": 15,
  "r": 1.0,
  "g": 0.28,
  "b": 0.69,
  "seed": 1,
  "tone": 0
 },
 "inputs": [
  "source"
 ],
 "output": "out",
 "nodes": {
  "out": {
   "op": "shader",
   "inputs": {
    "in0": "$source"
   },
   "params": {
    "code": "float4 shade(float2 pos, float px) {\n  float ang = param(0) * 0.01745329;\n  float cell = param(1);\n  float2x2 R = float2x2(cos(ang), -sin(ang), sin(ang), cos(ang));\n  float2 q = mul(R, pos) / cell;\n  float2 c = floor(q) + 0.5;\n  float2 cpos = mul(transpose(R), c * cell);\n  float4 s = input(0, cpos);\n  // Dots from coverage (type, shapes, mattes) or, with tone, from how dark the source is (footage).\n  float dark = s.a > 0.0 ? 1.0 - dot(s.rgb / s.a, float3(0.2126, 0.7152, 0.0722)) : 0.0;\n  float d = saturate(param(6) > 0.5 ? dark * s.a : s.a);\n  // Dot radius from coverage (area-true up to touching dots, then filling in).\n  float r = d < 0.785 ? sqrt(d / 3.14159) : 0.5 + (d - 0.785) * 1.1;\n  float dist = length(q - c);\n  float cov = saturate((r - dist) * cell / max(px, 1e-3) + 0.5);\n  // Riso ink is uneven: mottle it.\n  float m = 0.86 + 0.14 * snoise(float3(pos * 0.035, param(5)));\n  cov *= m;\n  float3 ink = srgb_to_linear(float3(param(2), param(3), param(4)));\n  return float4(ink * cov, cov);\n}",
    "u0": "$angle",
    "u1": "$cell",
    "u2": "$r",
    "u3": "$g",
    "u4": "$b",
    "u5": "$seed",
    "u6": "$tone"
   }
  }
 }
}"##,
    ),
    (
        "glitch",
        r##"{
 "params": {
  "amount": 0.5,
  "roll": 0.35,
  "tracking": 0.3
 },
 "inputs": [
  "source"
 ],
 "output": "out",
 "nodes": {
  "out": {
   "op": "shader",
   "inputs": {
    "in0": "$source"
   },
   "params": {
    "code": "float h2(float2 p) { return frac(sin(dot(p, float2(127.1, 311.7))) * 43758.5453); }\nfloat4 shade(float2 pos, float px) {\n  float t = time();\n  float amt = param(0);\n  float frame = floor(t * 30.0);\n  // Tear the frame into bands of random height and slide some sideways.\n  float coarse = floor(pos.y / 64.0);\n  float bandH = 6.0 + 90.0 * h2(float2(coarse, frame));\n  float band = floor(pos.y / bandH);\n  float pick = h2(float2(band, frame + 1.0));\n  float shift = pick > 1.0 - 0.4 * amt ? (h2(float2(band, frame + 7.0)) - 0.5) * 260.0 * amt : 0.0;\n  float2 p = pos + float2(shift, 0.0);\n  // Split the channels.\n  float split = (2.0 + 22.0 * amt) * (0.6 + 0.8 * h2(float2(frame, 3.0)));\n  float4 g = input(0, p);\n  float r = input(0, p + float2(split, 0.0)).r;\n  float b = input(0, p - float2(split, 0.0)).b;\n  float4 c = float4(r, g.g, b, g.a);\n  // Blocks of the picture freeze to a flat color now and then.\n  float2 blk = floor(pos / 24.0);\n  if (h2(blk + frame * 0.37) > 1.0 - 0.05 * amt * amt) c.rgb = input(0, (blk + 0.5) * 24.0).rgb * 1.3;\n  // Scanlines, a rolling tracking band, and snow.\n  c.rgb *= 0.86 + 0.14 * step(1.5, fmod(pos.y, 3.0));\n  float roll = frac(t * param(1)) * (size().y + 200.0) - 100.0;\n  float trk = smoothstep(46.0, 0.0, abs(pos.y - roll)) * param(2);\n  c.rgb = lerp(c.rgb, float3(h2(float2(pos.x * 0.05, frame + pos.y))) * 0.6, trk * 0.7);\n  c.rgb += (h2(pos + frame * 13.0) - 0.5) * (0.04 + 0.12 * amt);\n  return float4(max(c.rgb, 0.0), c.a);\n}",
    "u0": "$amount",
    "u1": "$roll",
    "u2": "$tracking"
   }
  }
 }
}"##,
    ),
    (
        "crt",
        r##"{
 "params": {
  "phosphor": 0.9
 },
 "inputs": [
  "source"
 ],
 "output": "out",
 "nodes": {
  "glow": {
   "op": "blur",
   "inputs": {
    "source": "$source"
   },
   "params": {
    "radius": 14
   }
  },
  "lit": {
   "op": "blend",
   "inputs": {
    "fg": "glow",
    "bg": "$source"
   },
   "params": {
    "mode": "add",
    "opacity": "$phosphor"
   }
  },
  "out": {
   "op": "shader",
   "inputs": {
    "in0": "lit"
   },
   "params": {
    "code": "float4 shade(float2 pos, float px) {\n  float2 sz = size();\n  float2 uv = pos / sz * 2.0 - 1.0;\n  // Curved glass.\n  uv *= 1.0 + float2(0.045, 0.07) * float2(uv.y * uv.y, uv.x * uv.x);\n  float2 p = (uv * 0.5 + 0.5) * sz;\n  float2 e = abs(uv);\n  float inside = smoothstep(1.0, 0.985, max(e.x, e.y));\n  float4 c = input(0, p);\n  // Beam rows and an aperture grille.\n  float row = 0.62 + 0.38 * pow(abs(sin(pos.y * 3.14159 / 3.0)), 0.8);\n  float m = fmod(floor(pos.x), 3.0);\n  float3 grille = m < 1.0 ? float3(1.0, 0.72, 0.72) : (m < 2.0 ? float3(0.72, 1.0, 0.72) : float3(0.72, 0.72, 1.0));\n  float flick = 0.97 + 0.03 * sin(time() * 115.0) + 0.02 * sin(time() * 7.0);\n  float vig = 1.0 - 0.35 * dot(uv, uv) * 0.5;\n  c.rgb *= grille * row * flick * vig;\n  // The glass itself: a faint sheen and a dark bezel.\n  c.rgb += 0.012 * float3(0.7, 1.0, 0.8) * (1.0 - smoothstep(0.0, 1.2, length(uv - float2(-0.4, -0.5))));\n  return float4(c.rgb * inside, 1.0) + float4(float3(0.004), 1.0) * (1.0 - inside);\n}"
   }
  }
 }
}"##,
    ),
    (
        "dot_matrix",
        r##"{
 "params": {
  "cell": 11,
  "r": 0.49,
  "g": 1.0,
  "b": 0.61
 },
 "inputs": [
  "source"
 ],
 "output": "out",
 "nodes": {
  "out": {
   "op": "shader",
   "inputs": {
    "in0": "$source"
   },
   "params": {
    "code": "float4 shade(float2 pos, float px) {\n  // Re-sample the wordmark on an LED dot grid.\n  float cell = param(0);\n  float2 c = (floor(pos / cell) + 0.5) * cell;\n  float a = input(0, c).a;\n  float d = length(pos - c) / cell;\n  float dot_ = smoothstep(0.42, 0.30, d) * step(0.35, a);\n  float3 col = srgb_to_linear(float3(param(1), param(2), param(3)));\n  return float4(col * dot_, dot_);\n}",
    "u0": "$cell",
    "u1": "$r",
    "u2": "$g",
    "u3": "$b"
   }
  }
 }
}"##,
    ),
    (
        "glass_pane",
        r##"{
 "params": {
  "x": 540,
  "y": 540,
  "width": 600,
  "height": 400,
  "radius": 48,
  "frost": 26,
  "bend": 36,
  "bevel": 34,
  "gain": 0.92,
  "lift": 0.06,
  "light": 1.0
 },
 "inputs": [
  "source"
 ],
 "output": "out",
 "nodes": {
  "pane": {
   "op": "shader",
   "inputs": {
    "in0": "$source"
   },
   "params": {
    "code": "float4 shade(float2 pos, float px) {\n  float2 c = float2(param(0), param(1));\n  float2 hs = float2(param(2), param(3)) * 0.5;\n  float r = param(4);\n  float2 q = pos - c;\n  float d = sd_round_box(q, hs, r);\n  if (d > 2.0) return float4(0.0);\n  float aa = saturate(0.5 - d / max(px, 1e-3));\n  float e = 1.0;\n  float2 g = float2(sd_round_box(q + float2(e, 0.0), hs, r) - sd_round_box(q - float2(e, 0.0), hs, r),\n                    sd_round_box(q + float2(0.0, e), hs, r) - sd_round_box(q - float2(0.0, e), hs, r));\n  float2 n = g / max(length(g), 1e-5);\n  // The rim is a lens: bend light inwards near the edge.\n  float h = saturate(1.0 + d / param(6));\n  float2 sp = pos - n * h * h * param(5);\n  // Frost: a golden-angle disk of taps.\n  float rad = param(7);\n  float4 acc = float4(0.0);\n  for (int k = 0; k < 16; k++) {\n    float rr = rad * sqrt((float(k) + 0.5) / 16.0);\n    float a = float(k) * 2.39996;\n    acc += input(0, sp + rr * float2(cos(a), sin(a)));\n  }\n  float3 col = acc.rgb / 16.0;\n  col = col * param(8) + param(9);\n  // Light catches the upper-left rim; a softer return on the lower right.\n  float rim = pow(h, 5.0);\n  float lit = saturate(dot(n, float2(-0.55, -0.83)));\n  col += rim * (0.55 * lit + 0.12) * param(10);\n  // A hairline at the very edge.\n  col += smoothstep(1.8, 0.0, abs(d + 1.0)) * 0.22 * param(10);\n  return float4(col * aa, aa);\n}",
    "u0": "$x",
    "u1": "$y",
    "u2": "$width",
    "u3": "$height",
    "u4": "$radius",
    "u5": "$bend",
    "u6": "$bevel",
    "u7": "$frost",
    "u8": "$gain",
    "u9": "$lift",
    "u10": "$light"
   }
  },
  "out": {
   "op": "over",
   "inputs": {
    "fg": "pane",
    "bg": "$source"
   }
  }
 }
}"##,
    ),
    (
        "marble",
        r##"{
 "params": {
  "filaments": 0.8,
  "resolution": 0.5,
  "veins": 1.0
 },
 "inputs": [],
 "output": "out",
 "nodes": {
  "out": {
   "op": "shader",
   "params": {
    "code": "float fbm(float3 p) {\n  float s = 0.0, a = 0.5;\n  for (int i = 0; i < 4; i++) { s += a * snoise(p); p = p * 2.07 + float3(17.1, 3.7, 9.2); a *= 0.5; }\n  return s;\n}\nfloat4 shade(float2 pos, float px) {\n  float2 uv = (pos - size() * 0.5) / size().y;\n  float t = time() * 0.07;\n  float3 p = float3(uv * 0.6 + float2(0.0, t * 0.5), t);\n  float2 q = float2(fbm(p), fbm(p + float3(5.2, 1.3, 2.0)));\n  float2 r = float2(fbm(p + float3(2.6 * q, 0.0) + float3(1.7, 9.2, t)),\n                    fbm(p + float3(2.6 * q, 0.0) + float3(8.3, 2.8, -t)));\n  float v = fbm(p + float3(2.4 * r, 0.0));\n  // Marbled ink: dark body, luminous filaments where the field crosses zero.\n  float3 deep = float3(0.003, 0.004, 0.012);\n  float3 blue = float3(0.015, 0.04, 0.16);\n  float body = smoothstep(-0.25, 0.55, v);\n  float fil = pow(saturate(1.0 - abs(v + 0.08) * 4.0), 7.0);\n  float fil2 = pow(saturate(1.0 - abs(r.y - 0.1) * 5.0), 9.0) * 0.6;\n  float3 hue = lerp(float3(0.95, 0.20, 0.32), float3(1.0, 0.66, 0.32), saturate(q.x * 1.4 + 0.5));\n  float3 c = lerp(deep, blue, body * 0.8);\n  c += hue * fil * param(1) * 1.4 + float3(0.55, 0.65, 1.0) * fil2 * param(2);\n  return float4(c, 1.0);\n}",
    "u1": "$filaments",
    "u2": "$veins",
     "resolution": "$resolution"
   }
  }
 }
}"##,
    ),
    (
        "starfield",
        r##"{
 "params": {
  "travel": 0.0,
  "speed": 0.3
 },
 "inputs": [],
 "output": "out",
 "nodes": {
  "out": {
   "op": "shader",
   "params": {
    "code": "float h1(float n) { return frac(sin(n * 12.9898) * 43758.5453); }\nfloat4 shade(float2 pos, float px) {\n  float2 c = size() * 0.5;\n  float2 d = pos - c;\n  float r = length(d) / size().y;\n  float a = atan2(d.y, d.x) / 6.2831853 + 0.5;\n  float travel = param(0), speed = param(1);\n  float3 col = float3(0.0);\n  for (int L = 0; L < 4; L++) {\n    float sectors = 180.0 + 150.0 * float(L);\n    float fs = a * sectors;\n    float s = floor(fs);\n    float h = h1(s * 3.7 + float(L) * 19.1);\n    float z = frac(h * 9.1 + travel * (0.8 + 0.25 * float(L)));\n    float rr = 0.015 + pow(z, 2.2) * 1.15;\n    float len = 0.002 + speed * 0.09 * pow(z, 1.6);\n    float across = abs(frac(fs) - 0.5) / sectors * 6.2831853 * r * size().y;\n    float w = 0.5 + 1.6 * z;\n    float lat = smoothstep(w + px, 0.0, across);\n    float lon = smoothstep(0.0, 0.004, r - (rr - len)) * smoothstep(0.004, 0.0, r - rr);\n    float3 tint = lerp(float3(0.55, 0.7, 1.0), float3(1.0, 0.85, 0.7), h1(s + 7.0));\n    col += tint * lat * lon * (0.35 + 1.8 * z * z) * step(0.25, h1(s * 1.3 + float(L)));\n  }\n  // The tunnel's heart.\n  col += float3(0.25, 0.4, 1.0) * 0.6 * exp(-r * 9.0) * (0.3 + 0.3 * speed);\n  return float4(col, 1.0);\n}",
    "u0": "$travel",
    "u1": "$speed"
   }
  }
 }
}"##,
    ),
    (
        "grade",
        r##"{
 "params": {
  "look": 0,
  "amount": 1.0
 },
 "inputs": [
  "source"
 ],
 "output": "out",
 "nodes": {
  "out": {
   "op": "shader",
   "inputs": {
    "in0": "$source"
   },
   "params": {
    "code": "\nfloat3 lift_gamma_gain(float3 c, float3 lift, float3 gamma, float3 gain) {\n  c = c * gain + lift * (1.0 - c);\n  return pow(max(c, 0.0), 1.0 / gamma);\n}\nfloat4 shade(float2 pos, float px) {\n  float4 src = input(0, pos);\n  if (src.a <= 1e-5) return src;\n  float3 lin = src.rgb / src.a;\n  float3 c = linear_to_srgb(saturate(lin));\n  float y = dot(c, float3(0.2126, 0.7152, 0.0722));\n  int look = int(param(0) + 0.5);\n  float3 g = c;\n  if (look == 0) {\n    // Teal and orange: cool shadows, warm skin and highlights.\n    float3 shadow = float3(0.0, 0.52, 0.58), high = float3(1.0, 0.62, 0.32);\n    g = lerp(c, lerp(c * shadow * 1.6, c * high * 1.5, smoothstep(0.15, 0.75, y)), 0.45);\n    g = lerp(float3(dot(g, float3(0.333))), g, 1.15);\n  } else if (look == 1) {\n    // Bleach bypass: silver, contrasty, desaturated.\n    float3 mono = float3(y);\n    float3 over = y < 0.5 ? 2.0 * c * mono : 1.0 - 2.0 * (1.0 - c) * (1.0 - mono);\n    g = lerp(c, over, 0.85);\n    g = lerp(float3(dot(g, float3(0.2126, 0.7152, 0.0722))), g, 0.45);\n  } else if (look == 2) {\n    // Warm film: lifted blacks, creamy highlights, gentle rolloff.\n    g = lift_gamma_gain(c, float3(0.05, 0.035, 0.02), float3(1.0, 0.97, 0.9), float3(1.0, 0.96, 0.86));\n    g = lerp(float3(dot(g, float3(0.2126, 0.7152, 0.0722))), g, 0.85);\n  } else if (look == 3) {\n    // Night: cool, crushed, a little green in the mids.\n    g = lift_gamma_gain(c, float3(0.0, 0.01, 0.03), float3(0.95, 1.02, 1.05), float3(0.85, 0.95, 1.08));\n    g = pow(g, float3(1.15));\n  } else {\n    // Mono with a print's contrast.\n    float m = smoothstep(0.03, 0.97, y);\n    g = float3(m);\n  }\n  float3 out_ = lerp(c, saturate(g), param(1));\n  return float4(srgb_to_linear(out_) * src.a, src.a);\n}",
    "u0": "$look",
    "u1": "$amount"
   }
  }
 }
}"##,
    ),
    (
        "lens_flare",
        r##"{
 "params": {
  "x": 1400,
  "y": 300,
  "intensity": 1.0,
  "r": 1.0,
  "g": 0.82,
  "b": 0.6
 },
 "inputs": [
  "source"
 ],
 "output": "out",
 "nodes": {
  "flare": {
   "op": "shader",
   "params": {
    "code": "\nfloat4 shade(float2 pos, float px) {\n  float2 sz = size();\n  float2 l = float2(param(0), param(1));\n  float2 c = sz * 0.5;\n  float s = sz.y;\n  float3 tint = srgb_to_linear(float3(param(3), param(4), param(5)));\n  float d = length(pos - l) / s;\n  float3 col = tint * (exp(-d * 18.0) * 2.5 + exp(-d * 4.0) * 0.25);\n  // Anamorphic streak.\n  float2 q = (pos - l) / s;\n  col += tint * exp(-abs(q.y) * 220.0) * exp(-abs(q.x) * 1.6) * 0.9;\n  // Ghosts along the line through the center.\n  for (int k = 1; k <= 5; k++) {\n    float f = -0.35 * float(k) + 0.2;\n    float2 gp = c + (l - c) * f;\n    float r = 0.012 + 0.018 * float(k % 3);\n    float gd = length(pos - gp) / s;\n    float ring = smoothstep(r, r * 0.8, gd) * 0.12 + smoothstep(r * 1.6, r * 1.2, gd) * smoothstep(r * 1.0, r * 1.2, gd) * 0.15;\n    float3 hue = 0.5 + 0.5 * cos(float3(0.0, 2.1, 4.2) + float(k) * 1.3);\n    col += hue * tint * ring;\n  }\n  // Halo ring around the source.\n  col += tint * smoothstep(0.012, 0.0, abs(d - 0.22)) * 0.08;\n  col *= param(2);\n  return float4(col, 0.0);\n}",
    "u0": "$x",
    "u1": "$y",
    "u2": "$intensity",
    "u3": "$r",
    "u4": "$g",
    "u5": "$b"
   }
  },
  "out": {
   "op": "composite",
   "inputs": {
    "fg": "flare",
    "bg": "$source"
   },
   "params": {
    "operator": "plus"
   }
  }
 }
}"##,
    ),
    (
        "light_leak",
        r##"{
 "params": {
  "speed": 1.0,
  "intensity": 0.6,
  "r": 1.0,
  "g": 0.45,
  "b": 0.18
 },
 "inputs": [
  "source"
 ],
 "output": "out",
 "nodes": {
  "leak": {
   "op": "shader",
   "params": {
    "code": "\nfloat4 shade(float2 pos, float px) {\n  float2 uv = pos / size();\n  float t = time() * param(0);\n  float3 warm = srgb_to_linear(float3(param(2), param(3), param(4)));\n  float a = 0.0;\n  for (int k = 0; k < 3; k++) {\n    float fk = float(k);\n    float2 cpos = float2(-0.15 + 0.5 * sin(t * (0.3 + 0.1 * fk) + fk * 2.0), 0.2 + 0.6 * cos(t * (0.23 + 0.07 * fk) + fk));\n    a += exp(-pow(length((uv - cpos) * float2(1.0, 1.6)) / (0.35 + 0.1 * fk), 2.0));\n  }\n  a += 0.35 * snoise(float3(uv * 1.3, t * 0.4));\n  a = saturate(a) * param(1);\n  return float4(warm * a, 0.0);\n}",
    "u0": "$speed",
    "u1": "$intensity",
    "u2": "$r",
    "u3": "$g",
    "u4": "$b",
    "resolution": 0.25
   }
  },
  "out": {
   "op": "composite",
   "inputs": {
    "fg": "leak",
    "bg": "$source"
   },
   "params": {
    "operator": "plus"
   }
  }
 }
}"##,
    ),
    (
        "film_look",
        r##"{
 "params": {
  "halation": 0.9,
  "softness": 0.55,
  "grit": 0.35,
  "lift": 0.03,
  "saturation": 0.82,
  "grain": 0.09,
  "vignette": "#000000b0",
  "aberration": 0.8
 },
 "inputs": [
  "source"
 ],
 "output": "out",
 "nodes": {
  "soft": {
   "op": "blur",
   "inputs": {
    "source": "$source"
   },
   "params": {
    "radius": 0.8
   }
  },
  "lens": {
   "op": "mix",
   "inputs": {
    "a": "$source",
    "b": "soft"
   },
   "params": {
    "t": "$softness"
   }
  },
  "hi": {
   "op": "levels",
   "inputs": {
    "source": "lens"
   },
   "params": {
    "in_black": 0.42,
    "in_white": 1.0
   }
  },
  "halo": {
   "op": "blur",
   "inputs": {
    "source": "hi"
   },
   "params": {
    "radius": 22
   }
  },
  "red": {
   "op": "color_matrix",
   "inputs": {
    "source": "halo"
   },
   "params": {
    "matrix": [
     1.0,
     0,
     0,
     0,
     0,
     0.25,
     0,
     0,
     0,
     0,
     0.05,
     0,
     0,
     0,
     0,
     0,
     0,
     0,
     1,
     0
    ]
   }
  },
  "halated": {
   "op": "blend",
   "inputs": {
    "fg": "red",
    "bg": "lens"
   },
   "params": {
    "mode": "add",
    "opacity": "$halation"
   }
  },
  "bloomed": {
   "op": "bloom",
   "inputs": {
    "source": "halated"
   },
   "params": {
    "threshold": 0.7,
    "radius": 40,
    "intensity": 0.4,
    "wide": 0.5
   }
  },
  "grit_noise": {
   "op": "noise",
   "params": {
    "a": "#5a5a5a",
    "b": "#a6a6a6",
    "scale": [
     260,
     260
    ],
    "octaves": 5
   }
  },
  "gritted": {
   "op": "blend",
   "inputs": {
    "fg": "grit_noise",
    "bg": "bloomed"
   },
   "params": {
    "mode": "soft_light",
    "opacity": "$grit"
   }
  },
  "lifted": {
   "op": "levels",
   "inputs": {
    "source": "gritted"
   },
   "params": {
    "out_black": "$lift",
    "gamma": 0.92
   }
  },
  "graded": {
   "op": "adjust",
   "inputs": {
    "source": "lifted"
   },
   "params": {
    "saturation": "$saturation",
    "contrast": 0.95
   }
  },
  "fringed": {
   "op": "aberration",
   "inputs": {
    "source": "graded"
   },
   "params": {
    "amount": "$aberration"
   }
  },
  "vignetted": {
   "op": "vignette",
   "inputs": {
    "source": "fringed"
   },
   "params": {
    "inner": "#00000000",
    "outer": "$vignette"
   }
  },
  "out": {
   "op": "grain",
   "inputs": {
    "source": "vignetted"
   },
   "params": {
    "amount": "$grain"
   }
  }
 }
}"##,
    ),
    (
        "engrave",
        r##"{
 "params": {
  "spacing": 6,
  "backdrop": 0.3,
  "ink_r": 0.12,
  "ink_g": 0.07,
  "ink_b": 0.04,
  "paper_r": 0.93,
  "paper_g": 0.88,
  "paper_b": 0.76
 },
 "inputs": [
  "source"
 ],
 "output": "out",
 "nodes": {
  "out": {
   "op": "shader",
   "inputs": {
    "in0": "$source"
   },
   "params": {
    "code": "float fbm2(float2 p, int oct) {\n  float s = 0.0, a = 0.5;\n  for (int i = 0; i < oct; i++) { s += a * snoise(float3(p, 1.7 * float(i))); p = p * 2.03 + float2(5.1, 1.7); a *= 0.5; }\n  return s;\n}\n// Copperplate engraving: tone becomes three layers of hatching that switch\n// on as the image darkens, each line thickening with its tone; contours are\n// inked; what is darker than `backdrop` is left as bare paper.\nfloat lum(float2 p) { return pow(saturate(dot(input(0, p).rgb, float3(0.3, 0.59, 0.11))), 0.45); }\nfloat hatch(float2 pos, float px, float ang, float spacing, float tone, float thr) {\n  float2 d = float2(cos(ang), sin(ang));\n  float u = dot(pos, float2(-d.y, d.x)) / spacing;\n  float w = saturate((thr - tone) / thr) * 0.5;\n  float dist = 0.5 - abs(frac(u) - 0.5);\n  float aa = 0.75 * px / spacing;\n  return smoothstep(w + aa, w - aa, dist) * step(0.02, w);\n}\nfloat4 shade(float2 pos, float px) {\n  float sp = max(param(0), 2.0);\n  float raw = lum(pos);\n  float back = param(1);\n  float bg = back > 0.0 ? smoothstep(back, back + 0.12, raw) : 1.0;\n  float L = lerp(1.0, saturate((raw - back) / max(0.92 - back, 0.05)), bg);\n  float ink = 0.0;\n  ink = max(ink, hatch(pos, px, radians(35.0), sp, L, 0.72));\n  ink = max(ink, hatch(pos, px, radians(-35.0), sp, L, 0.42));\n  ink = max(ink, hatch(pos, px, radians(90.0), sp * 0.85, L, 0.2));\n  float gx = lum(pos + float2(1, -1)) + 2.0 * lum(pos + float2(1, 0)) + lum(pos + float2(1, 1))\n           - lum(pos + float2(-1, -1)) - 2.0 * lum(pos + float2(-1, 0)) - lum(pos + float2(-1, 1));\n  float gy = lum(pos + float2(-1, 1)) + 2.0 * lum(pos + float2(0, 1)) + lum(pos + float2(1, 1))\n           - lum(pos + float2(-1, -1)) - 2.0 * lum(pos + float2(0, -1)) - lum(pos + float2(1, -1));\n  ink = max(ink, smoothstep(0.25, 0.6, length(float2(gx, gy))));\n  float3 paper = float3(param(5), param(6), param(7)) * (0.95 + 0.05 * fbm2(pos / 2.5, 2)) * (0.94 + 0.06 * fbm2(pos / 220.0, 3));\n  float3 c = lerp(paper, float3(param(2), param(3), param(4)), ink * 0.92);\n  return float4(srgb_to_linear(c), 1.0);\n}",
    "u0": "$spacing",
    "u1": "$backdrop",
    "u2": "$ink_r",
    "u3": "$ink_g",
    "u4": "$ink_b",
    "u5": "$paper_r",
    "u6": "$paper_g",
    "u7": "$paper_b"
   }
  }
 }
}"##,
    ),
];

/// One line on what each built-in does, for the op vocabulary.
pub fn summary(name: &str) -> &'static str {
    match name {
        "drop_shadow" => "soft shadow under the source (color, opacity, offset, radius)",
        "glow" => "soft colored glow around the source",
        "vignette" => "darkened edges (inner, outer colors)",
        "stroke_outline" => "solid outline around the source's shape (color, width px)",
        "bloom" => "light bleeding from bright areas (threshold, radius, intensity, wide)",
        "grain" => "animated film grain (amount)",
        "aberration" => "chromatic fringes toward the edges",
        "repeater" => {
            "count copies, each moved by (dx, dy), turned by rotate degrees and scaled by scale about (cx, cy), fading by fade: trails, radial arrays, tunnels"
        }
        "halftone" => {
            "riso halftone dots of one ink (cell, angle, r, g, b): sized by the source's coverage, or with tone 1 by how dark it is (to screen footage)"
        }
        "chroma_key" => "keys out a green (or any) screen: key color r, g, b (sRGB 0..1), tolerance, softness, spill",
        "glitch" => "digital glitch: slices, offsets, color splits",
        "crt" => "CRT screen: curvature, scanlines, aperture grille",
        "dot_matrix" => "LED dot-matrix resampling",
        "glass_pane" => "frosted, refracting glass pane over what is beneath",
        "marble" => "flowing marble/ink generator (half resolution by default)",
        "starfield" => "drifting starfield generator",
        "grade" => "color grade looks 0..4 (look, amount)",
        "lens_flare" => "lens flare from a light at (x, y)",
        "light_leak" => "warm film light leaks drifting over the frame",
        "film_look" => {
            "lens and film over the source: red halation, a hair of softness, grit, lifted blacks, grade, fringes, vignette, grain (hides the tells of plain 3D)"
        }
        "engrave" => {
            "copperplate engraving: tone to hatching, contours inked, dark backdrop left as paper (spacing, backdrop, ink_*, paper_*)"
        }
        _ => "built-in component",
    }
}

pub fn components() -> &'static [(String, Component)] {
    static C: OnceLock<Vec<(String, Component)>> = OnceLock::new();
    C.get_or_init(|| {
        SOURCES
            .iter()
            .map(|(name, src)| {
                (
                    name.to_string(),
                    serde_json::from_str(src).expect("built-in component parses"),
                )
            })
            .collect()
    })
}

/// Whether `doc` uses a built-in it does not define itself.
pub fn used_by(doc: &Document) -> bool {
    let uses = |nodes: &std::collections::BTreeMap<String, ultrafast_graph::Node>| {
        nodes
            .values()
            .any(|n| components().iter().any(|(name, _)| *name == n.op) && !doc.components.contains_key(&n.op))
    };
    uses(&doc.nodes) || doc.components.values().any(|c| uses(&c.nodes))
}

/// `doc` with the built-ins it uses added to its components.
pub fn with_builtins(doc: &Document) -> Document {
    let mut d = doc.clone();
    for (name, c) in components() {
        d.components.entry(name.clone()).or_insert_with(|| c.clone());
    }
    d
}
