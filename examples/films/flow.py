"""FLOW — simulation is just another node. Type bleeds into ink, rises as
smoke, grows as living coral, and echoes into a tunnel; every effect is a
fluid or feedback node fed by ordinary type. 1080x1920, 16 s, with sound."""

from ultrafastfilm import *

f = Film(1080, 1920, fps=30, duration=16.0, motion_blur=(4, 0.5))
W, H = f.W, f.H
CX = W / 2
SCORE = "../../out/films/hero_score.wav"
SERIF, ITALIC = "InstrumentSerif-Regular.ttf", "InstrumentSerif-Italic.ttf"
MONO, SANS, FR, UNB = "GeistMono-VF.ttf", "Geist-VF.ttf", "Fraunces-VF.ttf", "Unbounded-VF.ttf"
CUTS = [0.0, 4.2, 8.4, 12.4, 16.0]

f.add("audio", {"src": SCORE, "start": 4.0, "gain": keys((0, 0.0), (0.5, 0.9), (15.0, 0.9), (16.0, 0.0))},
      name="score")


def label(n, title, dark):
    col = "#1d1f2a" if dark else "#f1eee8"
    a = f.text(f"{n:02d}", MONO, 26, [72, 132], color=col + "e6", tracking=2, weight=500, name="label")
    b = f.text(title, MONO, 26, [W - 72, 132], color=col + "e6", tracking=4, weight=500, align="right",
               name="label")
    return f.over(a, b, name="labels")


def window(i):
    return {"in": CUTS[i], "out": CUTS[i + 1]}


# ---------------------------------------------------------------- 01 ink
t0 = CUTS[0]
paper = f.add("solid", {"color": "#efe8dc"}, name="paper")
fibre = f.add("noise", {"a": "#efe8dc00", "b": "#5a463018", "scale": [3, 90], "octaves": 3}, name="fibre")
paper = f.over(fibre, paper, name="paper_fibre")
ink_col = "#121626"
words = []
for i, (txt, fnt, y) in enumerate([("Every", ITALIC, 860), ("frame", SERIF, 1150)]):
    w = f.text(txt, fnt, 330, [CX, y], color=ink_col, align="center", tracking=-6, name="ink_word")
    words.append(w)
type_ink = f.over(words[0], words[1], name="ink_type")
# The type pours into the water for a moment, pushing it aside as it does.
pour = f.add("opacity", {"opacity": keys((0.0, 0.0), (0.9, 0.0), (1.05, 1.0), (1.5, 1.0), (1.9, 0.0))},
             {"source": type_ink}, name="pour")
ink = f.add("fluid", {"start": 0.8, "emit": 10, "dissipation": 0.0, "velocity_dissipation": 0.35,
                      "vorticity": 6, "turbulence": keys((0.8, 120.0), (2.2, 420.0)), "turbulence_scale": 340,
                      "buoyancy": -70, "stir": 800, "swirl": 450, "resolution": 0.25, "dye_resolution": 0.75, "seed": 3},
            {"dye": pour, "stir": pour}, name="ink")
crisp = f.add("opacity", {"opacity": keys((0.2, 0.0), (0.6, 1.0, SOFT), (1.1, 1.0), (1.7, 0.0))},
              {"source": type_ink}, name="crisp")
s1 = f.over(ink, paper, name="ink_on_paper")
s1 = f.over(crisp, s1, name="crisp_ink")
cap1 = f.text("type, poured into water", ITALIC, 64, [CX, 1690], color="#1d1f2acc", align="center",
              name="cap")
cap1 = f.add("opacity", {"opacity": keys((1.6, 0.0), (2.3, 1.0, SOFT))}, {"source": cap1}, name="cap_in")
s1 = f.over(cap1, s1, name="s1_cap")
s1 = f.over(label(1, "INK", True), s1, name="s1")

# ---------------------------------------------------------------- 02 smoke
t1 = CUTS[1]
night = f.add("gradient", {"kind": "radial", "center": [CX, 1500], "radius": 1500,
                           "stops": "0:#1a0d0a, 0.6:#070506, 1:#020203"}, name="night")
line = f.text("is a simulation", SANS, 118, [CX, 1520], color="#ffffff", align="center", weight=600,
              tracking=-3, name="smoke_line")
heat = f.add("gradient", {"kind": "linear", "start": keys((t1, [-300, 0]), (CUTS[2], [500, 0])),
                          "end": keys((t1, [700, 0]), (CUTS[2], [1500, 0])),
                          "stops": "0:#ff2e63, 0.5:#ffb347, 1:#ff5a1f"}, name="heat")
hot_line = f.add("mask", {}, {"source": heat, "matte": line}, name="hot_line")
smoke = f.add("fluid", {"start": t1, "emit": 3.5, "dissipation": 0.32, "velocity_dissipation": 0.15,
                        "vorticity": 9, "turbulence": 650, "turbulence_scale": 200, "buoyancy": 1500,
                        "wind": [30, 0], "stir": 0, "resolution": 0.25, "dye_resolution": 0.5, "seed": 7},
              {"dye": hot_line}, name="smoke")
s2 = f.blend(smoke, night, "add", name="smoke_add")
s2 = f.add("bloom", {"threshold": 0.35, "radius": 60, "intensity": 0.9, "wide": 0.5}, {"source": s2},
           name="smoke_bloom")
core = f.text("is a simulation", SANS, 118, [CX, 1520], color="#fff4e6", align="center", weight=600,
              tracking=-3, name="smoke_core")
s2 = f.over(core, s2, name="line_over")
s2 = f.over(label(2, "SMOKE", False), s2, name="s2")

# ---------------------------------------------------------------- 03 coral
t2 = CUTS[2]
RD = r'''
float hash21(float2 p) { return frac(sin(dot(p, float2(127.1, 311.7))) * 43758.5453); }
float4 update(float2 pos, float px) {
    float mask = input(0, pos).a;
    if (step_index() == 0) {
        float spot = step(0.982, hash21(floor(pos / (px * 3.0))));
        return float4(1.0, spot * mask, 0.0, 1.0);
    }
    float2 c = prev(pos).xy;
    float2 lap = -c
        + 0.2 * (prev(pos + float2(px, 0)).xy + prev(pos - float2(px, 0)).xy
               + prev(pos + float2(0, px)).xy + prev(pos - float2(0, px)).xy)
        + 0.05 * (prev(pos + float2(px, px)).xy + prev(pos - float2(px, px)).xy
                + prev(pos + float2(px, -px)).xy + prev(pos + float2(-px, px)).xy);
    float a = c.x, b = c.y;
    float kill = lerp(param(2), param(1), mask);
    float r = a * b * b;
    a += lap.x - r + param(0) * (1.0 - a);
    b += 0.5 * lap.y + r - (kill + param(0)) * b;
    return float4(saturate(a), saturate(b), 0.0, 1.0);
}
'''
CORAL = r'''
float4 shade(float2 pos, float px) {
    float e = 2.5;
    float b = input(0, pos).y;
    float bx = input(0, pos + float2(e, 0)).y - input(0, pos - float2(e, 0)).y;
    float by = input(0, pos + float2(0, e)).y - input(0, pos - float2(0, e)).y;
    float3 n = normalize(float3(-bx * 5.0, -by * 5.0, 1.0));
    float3 l = normalize(float3(-0.45, -0.75, 0.55));
    float diff = saturate(dot(n, l));
    float spec = pow(saturate(dot(reflect(-l, n), float3(0, 0, 1))), 30.0);
    float m = smoothstep(0.1, 0.3, b);
    float2 uv = pos / size();
    float3 sea = lerp(srgb_to_linear(float3(0.02, 0.09, 0.13)), srgb_to_linear(float3(0.01, 0.03, 0.06)), uv.y);
    float3 coral = srgb_to_linear(float3(1.0, 0.42, 0.33));
    float3 tip = srgb_to_linear(float3(1.0, 0.86, 0.72));
    float3 col = lerp(coral, tip, smoothstep(0.28, 0.48, b)) * (0.35 + 0.85 * diff) + spec * 0.8;
    // A soft shadow under the growth.
    float sh = input(0, pos - float2(-10.0, -16.0)).y;
    sea *= 1.0 - 0.6 * smoothstep(0.1, 0.35, sh);
    return float4(lerp(sea, col, m), 1.0);
}
'''
grow = f.text("alive", FR, 470, [CX, 1120], color="#ffffff", align="center", weight=900, axis_SOFT=100,
              axis_opsz=144, tracking=-10, name="grow_word")
rd = f.add("feedback", {"code": RD, "start": t2, "resolution": 0.6, "substeps": 26,
                        "u": [0.0545, 0.062, 0.09],
                        "u2": keys((t2, 0.09), (t2 + 2.2, 0.09), (t2 + 3.6, 0.0635))},
           {"in0": grow}, name="coral")
s3 = f.add("shader", {"code": CORAL}, {"in0": rd}, name="coral_lit")
cap3 = f.text("grown, not drawn", ITALIC, 64, [CX, 1690], color="#ffd2b5d0", align="center", name="cap")
cap3 = f.add("opacity", {"opacity": keys((t2 + 1.2, 0.0), (t2 + 1.9, 1.0, SOFT))}, {"source": cap3},
             name="cap_in")
s3 = f.over(cap3, s3, name="s3_cap")
s3 = f.over(label(3, "CORAL", False), s3, name="s3")

# ---------------------------------------------------------------- 04 echo
t3 = CUTS[3]
ECHO = r'''
float3 hue_rotate(float3 c, float a) {
    float3 k = float3(0.57735, 0.57735, 0.57735);
    float ca = cos(a);
    return c * ca + cross(k, c) * sin(a) + k * dot(k, c) * (1.0 - ca);
}
float4 update(float2 pos, float px) {
    float2 c = float2(param(4), param(5));
    float2 q = pos - c;
    float a = param(0) * dt();
    float z = 1.0 - param(1) * dt();
    float2 r = float2(cos(a) * q.x - sin(a) * q.y, sin(a) * q.x + cos(a) * q.y) * z;
    float4 p = max(prev_sharp(c + r), 0.0);
    p.rgb = max(hue_rotate(p.rgb, param(2) * dt()), 0.0);
    p *= exp(-param(3) * dt());
    float4 i = input(0, pos);
    return i + p * (1.0 - i.a);
}
'''
void = f.add("solid", {"color": "#050407"}, name="void")
mark = f.text("ultrafast", UNB, 210, [CX, 1000], color="#ff6a3d", align="center", valign="middle", weight=800,
              name="mark")
mark_spin = f.add("transform", {"anchor": [CX, 1000], "rotation": keys((t3, -14.0, INOUT), (t3 + 2.4, 0.0)),
                                "scale": keys((t3, 0.82, OUT), (t3 + 2.4, 1.0))},
                  {"source": mark}, name="mark_spin")
hue = f.add("adjust", {"hue": keys((t3, 0.0), (CUTS[4], -170.0))}, {"source": mark_spin}, name="mark_hue")
# The echoes are of the outline: a tunnel of drawn lines, not a smear.
ring = f.text("ultrafast", UNB, 210, [CX, 1000], color="#ff6a3d", align="center", valign="middle", weight=800,
              stroke=3, name="ring")
ring = f.add("transform", {"anchor": [CX, 1000], "rotation": keys((t3, -14.0, INOUT), (t3 + 2.4, 0.0)),
                           "scale": keys((t3, 0.82, OUT), (t3 + 2.4, 1.0))}, {"source": ring}, name="ring_spin")
ring = f.add("adjust", {"hue": keys((t3, 0.0), (CUTS[4], -170.0))}, {"source": ring}, name="ring_hue")
echo = f.add("feedback", {"code": ECHO, "start": t3, "resolution": 1.0,
                          "u": [0.35, 0.9, 1.6, 1.4, CX, 1500],
                          "u3": keys((t3, 1.0), (t3 + 2.2, 1.0), (t3 + 3.0, 6.0))},
             {"in0": ring}, name="echo")
s4 = f.blend(echo, void, "add", name="echo_add")
s4 = f.add("bloom", {"threshold": 0.3, "radius": 48, "intensity": 0.7, "wide": 0.4}, {"source": s4},
           name="echo_bloom")
s4 = f.over(hue, s4, name="mark_over")
tag = f.text("simulation is just another node.", ITALIC, 66, [CX, 1690], color="#f1eee8", align="center",
             name="tag")
tag = f.add("opacity", {"opacity": keys((t3 + 1.3, 0.0), (t3 + 1.9, 1.0, SOFT))}, {"source": tag}, name="tag_in")
s4 = f.over(tag, s4, name="s4_tag")
s4 = f.over(label(4, "ECHO", False), s4, name="s4")

# ---------------------------------------------------------------- cut
shots = [s1, s2, s3, s4]
comp = f.add("solid", {"color": "#000000"}, name="black")
for i, s in enumerate(shots):
    comp = f.over(s, comp, timing=window(i), name="at")
flash = f.add("solid", {"color": "#fff6ea"}, name="flash")
for c in CUTS[1:4]:
    fl = f.add("opacity", {"opacity": keys((c, 0.3), (c + 0.07, 0.0))}, {"source": flash}, name="flash_o")
    comp = f.over(fl, comp, timing={"in": c, "out": c + 0.07}, name="flash_at")
f.output = finish(f, comp, grain=0.06)
f.save(os.path.join(FILMS_DIR, "flow.json"))
