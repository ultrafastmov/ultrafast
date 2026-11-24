//! Machine-readable op vocabulary: every op's ports and params with types,
//! defaults and meaning. Agents read it (`ultrafast ops`, the live `ops` request)
//! and `ultrafast check` uses it to flag misspelled or mistyped params.

use serde_json::{Value, json};
use ultrafast_graph::{Document, Param};

/// (name, type, default, description)
type P = (&'static str, &'static str, &'static str, &'static str);

const PLACEMENT: &[P] = &[
    ("position", "vec2", "[0,0]", "where the anchor lands, px"),
    ("anchor", "vec2", "[0,0]", "pivot in the node's own space, px"),
    ("scale", "vec2|number", "[1,1]", "scale around the anchor"),
    ("rotation", "number", "0", "degrees, clockwise"),
];

const STROKE: &[P] = &[
    ("stroke", "number", "0", "stroke width px; 0 fills"),
    ("join", "enum(miter|round|bevel)", "miter", "stroke joins"),
    ("cap", "enum(butt|round|square)", "butt", "stroke caps"),
    ("dash", "number[]", "", "dash pattern [on, off, ...] px"),
    ("dash_offset", "number", "0", "dash phase px"),
];

struct OpSchema {
    name: &'static str,
    summary: &'static str,
    ports: &'static [&'static str],
    params: &'static [P],
    /// Shared param groups appended to `params`.
    extra: &'static [&'static [P]],
}

const GRADIENT: &[P] = &[
    ("kind", "enum(linear|radial|conic)", "linear", "gradient shape"),
    ("a", "color", "", "start color (with b: a two-stop gradient)"),
    ("b", "color", "", "end color"),
    (
        "stops",
        "text|number[]",
        "",
        "\"0:#ff0000, 1:#0000ff\" or [pos,r,g,b,a,...], up to 8",
    ),
    (
        "space",
        "enum(linear|srgb)",
        "linear",
        "interpolation space; srgb matches CSS/Figma",
    ),
    ("start", "vec2", "[0,0]", "linear: start point px"),
    ("end", "vec2", "[0,h]", "linear: end point px"),
    ("center", "vec2", "frame center", "radial/conic center px"),
    ("radius", "number", "max(w,h)/2", "radial radius px"),
    ("angle", "number", "0", "conic start angle, degrees"),
];

const SCHEMAS: &[OpSchema] = &[
    OpSchema {
        name: "solid",
        summary: "flat color",
        ports: &[],
        params: &[("color", "color", "#000000", "fill color")],
        extra: &[],
    },
    OpSchema {
        name: "gradient",
        summary: "linear, radial or conic gradient",
        ports: &[],
        params: GRADIENT,
        extra: &[],
    },
    OpSchema {
        name: "shape",
        summary: "analytic rectangle / rounded rect / ellipse (SDF, antialiased)",
        ports: &[],
        params: &[
            ("kind", "enum(rect|ellipse|circle)", "rect", "shape"),
            ("color", "color", "#ffffff", "fill (or stroke) color"),
            ("center", "vec2", "frame center", "center px"),
            ("size", "vec2", "[w/4,w/4]", "width and height px"),
            ("radius", "number", "0", "corner radius px"),
            ("rotation", "number", "0", "degrees"),
            (
                "scale",
                "number|vec2",
                "1",
                "scale about the center (size, corners and stroke)",
            ),
            ("opacity", "number", "1", "0..1, fades the shape"),
            ("stroke", "number", "0", "outline width px; 0 fills"),
            ("softness", "number", "1", "edge softness in output pixels"),
        ],
        extra: &[],
    },
    OpSchema {
        name: "checker",
        summary: "checkerboard",
        ports: &[],
        params: &[
            ("a", "color", "#333333", "first cell"),
            ("b", "color", "#666666", "second cell"),
            ("size", "number", "32", "cell px"),
        ],
        extra: &[],
    },
    OpSchema {
        name: "noise",
        summary: "fractal simplex noise between two colors",
        ports: &[],
        params: &[
            ("a", "color", "#000000", "low color"),
            ("b", "color", "#ffffff", "high color"),
            ("scale", "vec2|number", "[200,200]", "px per noise cell"),
            ("offset", "vec2", "[0,0]", "pan px"),
            ("evolution", "number", "0", "third noise dimension; animate it"),
            ("octaves", "number", "4", "1..8"),
        ],
        extra: &[],
    },
    OpSchema {
        name: "image",
        summary: "still image or image sequence",
        ports: &[],
        params: &[
            (
                "src",
                "text",
                "",
                "file path relative to the document; {frame} / {frame:05} for sequences",
            ),
            (
                "fit",
                "enum(contain|cover|fill|none)",
                "contain",
                "placement in the frame, or in `rect` (cover there by default); only fill stretches",
            ),
            (
                "rect",
                "vec4",
                "",
                "[x,y,w,h] px box the picture goes in, by `fit`; it keeps its aspect ratio",
            ),
            ("opacity", "number", "1", "0..1"),
            (
                "space",
                "enum(linear|srgb)",
                "linear",
                "interpolation space when scaled; srgb filters the encoded values as browsers do",
            ),
            ("fps", "number", "document fps", "sequence frame rate"),
            ("first_frame", "number", "0", "sequence frame number at local time 0"),
        ],
        extra: &[],
    },
    OpSchema {
        name: "video",
        summary: "video clip, hardware decoded",
        ports: &[],
        params: &[
            ("src", "text", "", "file path"),
            (
                "fit",
                "enum(contain|cover|fill|none)",
                "contain",
                "placement in the frame, or in `rect` (cover there by default); only fill stretches",
            ),
            (
                "rect",
                "vec4",
                "",
                "[x,y,w,h] px box the footage goes in, by `fit`; it keeps its aspect ratio",
            ),
            ("opacity", "number", "1", "0..1"),
            (
                "space",
                "enum(linear|srgb)",
                "linear",
                "interpolation space when scaled; srgb filters the encoded values as browsers do",
            ),
            ("start", "number", "0", "source seconds at local time 0"),
            ("speed", "number", "1", "playback rate"),
            (
                "loop",
                "bool",
                "false",
                "wrap at the clip's end (else hold the last frame)",
            ),
            (
                "time",
                "number",
                "",
                "explicit source time (animate for time remapping)",
            ),
        ],
        extra: &[],
    },
    OpSchema {
        name: "path",
        summary: "vector path (SVG data or preset), GPU exact-coverage raster",
        ports: &[],
        params: &[
            (
                "to",
                "text",
                "",
                "SVG path data to morph toward; several separated by | to morph through each in turn",
            ),
            (
                "morph",
                "number",
                "0",
                "0 = this path, 1 = `to` (with several targets, 2 = the second, ...); animate it",
            ),
            ("d", "text", "", "SVG path data"),
            (
                "kind",
                "enum(star|polygon|circle|line|polyline|polygon_points)",
                "",
                "preset instead of d",
            ),
            ("center", "vec2", "frame center", "preset center"),
            ("radius", "number", "h/4", "preset radius"),
            ("sides", "number", "5 / 6", "star points / polygon sides"),
            ("inner_radius", "number", "0.45 r", "star inner radius"),
            (
                "points",
                "number[]",
                "",
                "flat [x0,y0,x1,y1,...] for line/polyline/polygon_points",
            ),
            ("color", "color", "#ffffff", "paint"),
            (
                "opacity",
                "number",
                "1",
                "0..1, fades the whole node (animate for fades)",
            ),
            ("fill_rule", "enum(nonzero|evenodd)", "nonzero", "fill rule"),
            ("trim_start", "number", "0", "0..1 of length"),
            ("trim_end", "number", "1", "0..1 of length (animate for draw-on)"),
        ],
        extra: &[STROKE, PLACEMENT],
    },
    OpSchema {
        name: "text",
        summary: "shaped text, GPU exact-coverage raster",
        ports: &[],
        params: &[
            (
                "text",
                "text",
                "",
                "content; \\n breaks lines; *word* is emphasis (em_ params), [words]{color=#ffd23f highlight=#ff4d6d} styles a span",
            ),
            ("font", "text", "Helvetica Neue", "family name or font file path"),
            ("weight", "number", "400", "100..900"),
            ("style", "enum(normal|italic)", "normal", "style"),
            ("size", "number", "64", "px"),
            ("color", "color", "#ffffff", "paint"),
            (
                "opacity",
                "number",
                "1",
                "0..1, fades the whole node (animate for fades)",
            ),
            (
                "align",
                "enum(left|center|right)",
                "left",
                "horizontal anchor of each line",
            ),
            (
                "valign",
                "enum(top|middle|baseline|bottom)",
                "top",
                "vertical anchor of the block",
            ),
            ("width", "number", "", "wrap width px"),
            ("line_height", "number", "1.2", "multiple of size"),
            ("tracking", "number", "0", "extra px between glyphs"),
            ("features", "text", "", "OpenType features, e.g. \"liga=0,ss01\""),
            (
                "variations",
                "text",
                "",
                "variable-font axes, e.g. \"wdth=80, opsz=144\" (weight drives wght; opsz follows size unless set)",
            ),
            (
                "axis_<tag>",
                "number",
                "axis default",
                "one variable-font axis, animatable: axis_wdth, axis_SOFT",
            ),
            (
                "stagger",
                "number",
                "0",
                "seconds between glyphs for the glyph_* animators",
            ),
            (
                "stagger_order",
                "enum(forward|reverse|center|random)",
                "forward",
                "which glyphs go first",
            ),
            (
                "glyph_offset",
                "vec2",
                "[0,0]",
                "per-glyph offset px, keyed in glyph time (t - stagger * order)",
            ),
            (
                "glyph_scale",
                "vec2",
                "[1,1]",
                "per-glyph scale about the glyph's center, glyph time",
            ),
            (
                "glyph_rotation",
                "number",
                "0",
                "per-glyph rotation degrees, glyph time",
            ),
            ("glyph_opacity", "number", "1", "per-glyph opacity, glyph time"),
            ("reveal", "number", "all", "glyphs visible (fractional fades)"),
            ("reveal_softness", "number", "1", "glyphs the reveal fade spans"),
            ("reveal_offset", "vec2", "[0,0]", "offset of not-yet-revealed glyphs px"),
            ("fill_rule", "enum(nonzero|evenodd)", "nonzero", "fill rule"),
            (
                "skim",
                "bool",
                "false",
                "glanced at, not read (code, data labels): ultrafast review skips its reading-time check",
            ),
            (
                "markup",
                "bool",
                "true",
                "parse inline styling in text: *emphasis* and [words]{color=#ffd23f weight=800 font=\"ultrafast:serif\" style=italic size=80 highlight=#ff4d6d underline=4 underline_color=#fff rise=10 draw=1}",
            ),
            ("em_color", "color", "text color", "color of *emphasis*"),
            (
                "em_font",
                "text",
                "font",
                "face of *emphasis* (e.g. ultrafast:serif for a serif accent)",
            ),
            ("em_weight", "number", "weight", "weight of *emphasis*"),
            (
                "em_style",
                "enum(normal|italic)",
                "italic when no em_ param is set",
                "style of *emphasis* (italic is synthesized for faces without one)",
            ),
            ("em_size", "number", "size", "size px of *emphasis*"),
            ("em_highlight", "color", "none", "marker box behind *emphasis*"),
            ("em_underline", "number", "0", "underline thickness px under *emphasis*"),
            ("em_underline_color", "color", "em_color", "underline color"),
            ("em_rise", "number", "0", "baseline shift px of *emphasis* (up)"),
            (
                "em_draw",
                "number",
                "1",
                "share of the emphasis highlights and underlines drawn, 0..1, one stroke across lines in reading order (animate it)",
            ),
            (
                "on_path",
                "text",
                "",
                "SVG path data the text follows (closed paths wrap: circular badges); align picks start, middle or end",
            ),
            (
                "path_offset",
                "number",
                "0",
                "px along the path; animate it to run text along",
            ),
            (
                "number",
                "number",
                "",
                "show this value instead of text (animate it: counters)",
            ),
            ("decimals", "number", "0", "digits after the point for number"),
            ("thousands", "text", ",", "group separator for number (\"\" for none)"),
            ("prefix", "text", "", "before number, e.g. $"),
            ("suffix", "text", "", "after number, e.g. % or k"),
            (
                "scramble",
                "number",
                "",
                "decoding text: animate 0 (all random) to 1 (the text)",
            ),
            (
                "scramble_chars",
                "text",
                "A-Z, 0-9, symbols",
                "characters the undecoded ones cycle through",
            ),
        ],
        extra: &[STROKE, PLACEMENT],
    },
    OpSchema {
        name: "shader",
        summary: "agent-written Slang: float4 shade(float2 pos, float px)",
        ports: &["in0", "in1", "in2", "in3"],
        params: &[
            (
                "resolution",
                "number",
                "1",
                "render at this fraction of the frame size and upscale (soft, costly looks; write shaders with pos/size())",
            ),
            (
                "code",
                "text",
                "",
                "Slang source defining shade(); `static const` tables of 64+ numbers are read from the params (editing their values compiles nothing)",
            ),
            ("u", "number[]", "", "up to 16 values, param(i)"),
            ("u0", "number", "", "u0..u15 individually (animatable)"),
            (
                "motion_blur",
                "bool",
                "false",
                "true: the format's motion blur samples its clock and animated u values (a shader that moves things); otherwise it holds at the frame's time",
            ),
        ],
        extra: &[],
    },
    OpSchema {
        name: "opacity",
        summary: "multiply alpha",
        ports: &["source"],
        params: &[("opacity", "number", "1", "0..1")],
        extra: &[],
    },
    OpSchema {
        name: "over",
        summary: "fg over bg (Porter-Duff)",
        ports: &["fg", "bg"],
        params: &[
            ("opacity", "number", "1", "fg opacity"),
            ("space", "enum(linear|srgb)", "linear", "blend space"),
        ],
        extra: &[],
    },
    OpSchema {
        name: "blend",
        summary: "blend modes (W3C)",
        ports: &["fg", "bg"],
        params: &[
            (
                "mode",
                "enum(normal|multiply|screen|overlay|darken|lighten|color_dodge|color_burn|hard_light|soft_light|difference|exclusion|hue|saturation|color|luminosity|add|subtract|divide)",
                "normal",
                "mode",
            ),
            ("opacity", "number", "1", "fg opacity"),
            ("space", "enum(linear|srgb)", "linear", "blend space"),
        ],
        extra: &[],
    },
    OpSchema {
        name: "composite",
        summary: "Porter-Duff operators",
        ports: &["fg", "bg"],
        params: &[("operator", "enum(in|out|atop|xor|plus|over)", "over", "operator")],
        extra: &[],
    },
    OpSchema {
        name: "mask",
        summary: "track matte",
        ports: &["source", "matte"],
        params: &[(
            "mode",
            "enum(alpha|alpha_inverted|luma|luma_inverted)",
            "alpha",
            "matte channel",
        )],
        extra: &[],
    },
    OpSchema {
        name: "mix",
        summary: "crossfade",
        ports: &["a", "b"],
        params: &[("t", "number", "0.5", "0 = a, 1 = b")],
        extra: &[],
    },
    OpSchema {
        name: "adjust",
        summary: "exposure, contrast, saturation, hue, brightness, gamma",
        ports: &["source"],
        params: &[
            ("exposure", "number", "0", "stops (linear light)"),
            ("contrast", "number", "1", "around mid grey"),
            ("saturation", "number", "1", "0 = grey"),
            ("hue", "number", "0", "degrees"),
            ("brightness", "number", "0", "offset"),
            ("gamma", "number", "1", "gamma"),
        ],
        extra: &[],
    },
    OpSchema {
        name: "levels",
        summary: "input/output levels with gamma",
        ports: &["source"],
        params: &[
            ("in_black", "number", "0", ""),
            ("in_white", "number", "1", ""),
            ("gamma", "number", "1", ""),
            ("out_black", "number", "0", ""),
            ("out_white", "number", "1", ""),
        ],
        extra: &[],
    },
    OpSchema {
        name: "color_matrix",
        summary: "4x5 matrix on straight linear RGBA",
        ports: &["source"],
        params: &[("matrix", "number[]", "identity", "20 numbers, row-major")],
        extra: &[],
    },
    OpSchema {
        name: "lut",
        summary: "3D LUT color grade (.cube)",
        ports: &["source"],
        params: &[
            ("src", "text", "", ".cube file (3D LUT)"),
            ("amount", "number", "1", "0..1 mix with the ungraded image"),
            (
                "space",
                "enum(srgb|linear)",
                "srgb",
                "what the LUT expects as input (most grading LUTs: srgb)",
            ),
        ],
        extra: &[],
    },
    OpSchema {
        name: "invert",
        summary: "invert colors",
        ports: &["source"],
        params: &[("amount", "number", "1", "0..1")],
        extra: &[],
    },
    OpSchema {
        name: "tint",
        summary: "map luminance between two colors",
        ports: &["source"],
        params: &[
            ("dark", "color", "#000000", "shadows"),
            ("light", "color", "#ffffff", "highlights"),
        ],
        extra: &[],
    },
    OpSchema {
        name: "threshold",
        summary: "luminance threshold",
        ports: &["source"],
        params: &[("level", "number", "0.5", ""), ("softness", "number", "0.01", "")],
        extra: &[],
    },
    OpSchema {
        name: "crop",
        summary: "keep a rectangle",
        ports: &["source"],
        params: &[
            ("rect", "vec4", "frame", "[x,y,w,h] px"),
            ("softness", "number", "1", "edge px"),
        ],
        extra: &[],
    },
    OpSchema {
        name: "transform",
        summary: "2D transform (absorbed into paths/text for sharpness)",
        ports: &["source"],
        params: &[
            ("position", "vec2", "frame center", "where the anchor lands"),
            ("anchor", "vec2", "frame center", "pivot"),
            ("scale", "vec2|number", "[1,1]", "scale"),
            ("rotation", "number", "0", "degrees"),
            ("skew", "number", "0", "degrees"),
        ],
        extra: &[],
    },
    OpSchema {
        name: "corner_pin",
        summary: "perspective: map the frame (or source_rect) to four corners",
        ports: &["source"],
        params: &[
            ("corners", "number[]", "frame corners", "[tl, tr, br, bl] as 8 numbers"),
            ("source_rect", "vec4", "frame", "[x,y,w,h] of the source mapped"),
        ],
        extra: &[],
    },
    OpSchema {
        name: "wave",
        summary: "sine displacement",
        ports: &["source"],
        params: &[
            ("amplitude", "number", "10", "px"),
            ("wavelength", "number", "100", "px"),
            ("phase", "number", "0", "degrees"),
            ("direction", "number", "0", "degrees"),
        ],
        extra: &[],
    },
    OpSchema {
        name: "twirl",
        summary: "swirl around a center",
        ports: &["source"],
        params: &[
            ("center", "vec2", "frame center", ""),
            ("radius", "number", "min(w,h)/2", ""),
            ("angle", "number", "90", "degrees"),
        ],
        extra: &[],
    },
    OpSchema {
        name: "tile",
        summary: "repeat a region",
        ports: &["source"],
        params: &[("origin", "vec2", "[0,0]", ""), ("size", "vec2", "frame", "")],
        extra: &[],
    },
    OpSchema {
        name: "displace",
        summary: "displace source by a map's red/green",
        ports: &["source", "map"],
        params: &[("amount", "vec2", "[20,20]", "px")],
        extra: &[],
    },
    OpSchema {
        name: "blur",
        summary: "gaussian blur",
        ports: &["source"],
        params: &[("radius", "number", "8", "sigma px")],
        extra: &[],
    },
    OpSchema {
        name: "mesh",
        summary: "3D mesh (scene object for render3d)",
        ports: &["texture"],
        params: &[
            (
                "primitive",
                "enum(cube|plane|sphere|cylinder|cone|torus|text|path)",
                "cube",
                "unit-sized primitive, or extruded text / SVG path",
            ),
            ("text", "text", "", "primitive text: content"),
            ("font", "text", "Helvetica Neue", "primitive text: family or file"),
            ("weight", "number", "400", "primitive text: weight"),
            ("style", "enum(normal|italic)", "normal", "primitive text: style"),
            ("size", "number", "1", "primitive text: size in world units"),
            (
                "align",
                "enum(left|center|right)",
                "center",
                "primitive text: alignment",
            ),
            (
                "tracking",
                "number",
                "0",
                "primitive text: extra spacing (px at 100 px/unit)",
            ),
            ("line_height", "number", "1.2", "primitive text: multiple of size"),
            ("d", "text", "", "primitive path: SVG path data"),
            ("units", "number", "100", "primitive path: path px per world unit"),
            ("depth", "number", "0.25", "extrusion depth, world units"),
            (
                "bevel",
                "number",
                "0",
                "text/path: 45-degree chamfer on both rims, world units",
            ),
            ("src", "text", "", ".gltf/.glb file instead of a primitive"),
            ("segments", "number", "32", "tessellation"),
            ("position", "vec3", "[0,0,0]", ""),
            ("rotation", "vec3", "[0,0,0]", "degrees, X then Y then Z"),
            ("scale", "vec3|number", "[1,1,1]", ""),
            ("color", "color", "#cccccc", "base color"),
            ("metallic", "number", "0", "0..1"),
            ("roughness", "number", "0.5", "0..1"),
            ("emissive", "color", "#000000", "emitted light"),
            (
                "transmission",
                "number",
                "0",
                "glass: 0..1 of the scene behind seen through it (refracted)",
            ),
            (
                "ior",
                "number",
                "1.45",
                "glass: index of refraction (water 1.33, glass 1.5, diamond 2.4)",
            ),
            (
                "thickness",
                "number",
                "0.5",
                "glass: how far refracted rays travel inside, world units (more bends more)",
            ),
            ("dispersion", "number", "0", "glass: color splitting (try 1..2)"),
            ("opacity", "number", "1", "0..1"),
        ],
        extra: &[],
    },
    OpSchema {
        name: "light",
        summary: "3D light (scene object)",
        ports: &[],
        params: &[
            ("kind", "enum(directional|point)", "directional", ""),
            ("direction", "vec3", "[0.4,0.8,0.6]", "toward the light (directional)"),
            ("position", "vec3", "[0,2,2]", "point light position"),
            ("color", "color", "#ffffff", ""),
            ("intensity", "number", "3", ""),
        ],
        extra: &[],
    },
    OpSchema {
        name: "camera",
        summary: "3D camera (scene object)",
        ports: &[],
        params: &[
            ("position", "vec3", "[0,0,5]", ""),
            (
                "orbit",
                "vec3",
                "",
                "[azimuth, elevation, distance] around target (overrides position)",
            ),
            ("target", "vec3", "[0,0,0]", ""),
            ("up", "vec3", "[0,1,0]", ""),
            ("fov", "number", "45", "vertical, degrees"),
            ("near", "number", "0.1", ""),
            ("far", "number", "1000", ""),
        ],
        extra: &[],
    },
    OpSchema {
        name: "render3d",
        summary: "renders connected meshes/lights/camera (any port names; camera on `camera`)",
        ports: &["*"],
        params: &[
            ("ambient", "number", "0.35", "hemisphere light intensity"),
            ("sky", "color", "#e6f2ff", "ambient from above"),
            ("ground", "color", "#5a4d47", "ambient from below"),
            ("background", "color", "transparent", "clear color"),
            (
                "shadows",
                "bool",
                "true",
                "the first directional light casts a shadow map",
            ),
            ("shadow_strength", "number", "0.85", "0 = no darkening, 1 = full"),
            (
                "shadow_bias",
                "number",
                "1",
                "depth bias in shadow-map texels (raise if surfaces self-shadow)",
            ),
            (
                "environment_intensity",
                "number",
                "1",
                "scale of the image on the `environment` port (equirectangular; lights and reflects)",
            ),
            (
                "environment_rotation",
                "number",
                "0",
                "turns the environment around +y, degrees",
            ),
            (
                "fog",
                "vec2",
                "",
                "[start, end] distance from the camera where surfaces fade into fog_color",
            ),
            ("fog_color", "color", "background", "fog color"),
            (
                "aperture",
                "number",
                "0",
                "depth of field: blur radius px far from focus (0 = off)",
            ),
            (
                "focus",
                "number",
                "camera to target",
                "depth of field: in-focus distance, world units",
            ),
        ],
        extra: &[],
    },
    OpSchema {
        name: "swarm",
        summary: "particles that assemble into text (or a path) and scatter: progress 0..1",
        ports: &[],
        params: &[
            ("text", "text", "", "what the particles form (or `d`)"),
            (
                "d",
                "text",
                "",
                "SVG path data to form instead of text (offset by position)",
            ),
            ("font", "text", "Helvetica Neue", "font family or file"),
            ("weight", "number", "400", "font weight"),
            ("style", "enum(normal|italic)", "normal", "font style"),
            ("tracking", "number", "0", "extra px between glyphs"),
            ("variations", "text", "", "variable-font axes, as on text"),
            (
                "axis_<tag>",
                "number",
                "axis default",
                "one variable-font axis, as on text",
            ),
            ("size", "number", "64", "text size px"),
            ("position", "vec2", "[0,0]", "text position (as `text`)"),
            ("align", "enum(left|center|right)", "left", "text alignment"),
            (
                "valign",
                "enum(top|middle|baseline|bottom)",
                "top",
                "vertical alignment",
            ),
            ("count", "number", "4000", "particles"),
            ("progress", "number", "1", "0 scattered, 1 formed; animate it"),
            (
                "stagger",
                "number",
                "0.5",
                "spread of arrival times (fraction of progress)",
            ),
            ("scatter", "number", "700", "how far particles start from home, px"),
            ("origin", "vec2", "", "start every particle from this point instead"),
            ("turbulence", "number", "40", "drift amplitude while away from home, px"),
            ("particle_size", "vec2", "[2,3]", "size scattered and formed, px"),
            ("color", "color", "#ffffff", "color when formed"),
            ("color_scatter", "color", "color", "color when scattered"),
            ("shimmer", "number", "0", "0..1 twinkle"),
            ("stretch", "number", "0", "streak along travel while moving"),
            ("seed", "number", "1", "random seed"),
            ("blend", "enum(over|add)", "over", "how particles combine"),
        ],
        extra: &[],
    },
    OpSchema {
        name: "particles",
        summary: "deterministic particle system (closed form: scrub anywhere), drawn as sprites",
        ports: &["sprite"],
        params: &[
            ("seed", "number", "1", "random seed"),
            ("rate", "number", "60", "particles emitted per second"),
            ("max", "number", "5000", "cap on particles alive"),
            ("life", "vec2", "[1.5,2.5]", "lifetime range, s"),
            ("position", "vec2", "[960,540]", "emitter center px"),
            ("emitter_size", "vec2", "[0,0]", "emitter rectangle px"),
            ("direction", "number", "-90", "degrees (-90 = up)"),
            ("spread", "number", "30", "cone angle, degrees"),
            ("speed", "vec2", "[200,400]", "initial speed range, px/s"),
            ("gravity", "vec2", "[0,300]", "px/s^2"),
            ("drag", "number", "0", "linear drag, 1/s"),
            ("size", "vec2", "[8,2]", "size at birth and death, px"),
            ("size_jitter", "number", "0.3", "0..1 random size reduction"),
            ("spin", "vec2", "[0,0]", "rotation speed range, deg/s"),
            ("color_start", "color", "#ffffff", "color at birth"),
            ("color_end", "color", "color_start", "color at death"),
            ("fade_in", "number", "0.1", "fraction of life fading in"),
            ("fade_out", "number", "0.3", "fraction of life fading out"),
            (
                "shape",
                "enum(circle|square|spark|sprite)",
                "circle",
                "sprite uses the `sprite` input",
            ),
            ("stretch", "number", "0", "spark stretch along velocity"),
            ("blend", "enum(over|add)", "over", "how particles combine"),
            ("emit_start", "number", "0", "emission start, s (negative pre-warms)"),
            ("emit_end", "number", "forever", "emission end, s"),
        ],
        extra: &[],
    },
    OpSchema {
        name: "fluid",
        summary: "ink/smoke simulation: `dye` pours color in, `stir` pushes it around; stepped once per frame (exact on export, seekable)",
        ports: &["dye", "stir"],
        params: &[
            (
                "start",
                "number",
                "0",
                "local time the simulation starts (blank before)",
            ),
            (
                "emit",
                "number",
                "6",
                "how fast `dye` replaces the fluid where it is, per s",
            ),
            ("dissipation", "number", "0.15", "dye fade per s"),
            ("velocity_dissipation", "number", "0.4", "motion fade per s"),
            (
                "vorticity",
                "number",
                "4",
                "keeps eddies curling (0 = silky, 10+ = churning)",
            ),
            (
                "turbulence",
                "number",
                "0",
                "curl-noise stirring, px/s^2 (try 400..3000)",
            ),
            ("turbulence_scale", "number", "260", "size of the noise eddies, px"),
            ("wind", "vec2", "[0,0]", "drift of everything, px/s"),
            ("buoyancy", "number", "0", "dye rises (smoke), px/s^2 per unit alpha"),
            ("stir", "number", "2500", "push away from the edges of the `stir` image"),
            ("swirl", "number", "0", "spin around the edges of the `stir` image"),
            ("resolution", "number", "0.25", "velocity grid, fraction of the frame"),
            ("dye_resolution", "number", "0.5", "dye grid, fraction of the frame"),
            (
                "pressure_iterations",
                "number",
                "24",
                "incompressibility solver iterations",
            ),
            ("substeps", "number", "1", "steps per frame (1..8)"),
            ("seed", "number", "1", "turbulence pattern"),
        ],
        extra: &[],
    },
    OpSchema {
        name: "feedback",
        summary: "agent Slang stepped once per frame on its own previous result: float4 update(float2 pos, float px) with prev(pos), input(i,pos), dt(), step_index()",
        ports: &["in0", "in1", "in2", "in3"],
        params: &[
            ("code", "text", "", "Slang source defining update()"),
            (
                "start",
                "number",
                "0",
                "local time the simulation starts (blank before)",
            ),
            ("resolution", "number", "1", "state grid, fraction of the frame"),
            ("substeps", "number", "1", "updates per frame (1..64)"),
            ("u", "number[]", "", "up to 16 values, param(i)"),
            ("u0", "number", "", "u0..u15 individually (animatable)"),
        ],
        extra: &[],
    },
    OpSchema {
        name: "raymarch",
        summary: "3D as a signed-distance scene marched per pixel: write float2 map(float3 p) -> (distance, material id); soft shadows, occlusion, a reflection bounce, environment and antialiasing come with it",
        ports: &["in0", "in1", "in2", "in3"],
        params: &[
            (
                "scene",
                "text",
                "",
                "Slang: float2 map(float3 p) (+ optional void material(float id, float3 p, float3 n, inout float3 albedo, inout float metallic, inout float roughness)); helpers sdSphere, sdBox, sdCylinder, sdTorus, sdCapsule, sdBand, sdPlane, smin, opU, rotX/Y/Z, fbm3, knob(i), layerDistance + extrude (any ultrafast layer as a 3D solid), presetMaterial (ids 0 matte, 1 brass, 2 polished brass, 3 steel, 4 gold, 5 copper, 6 black lacquer, 7 ceramic, 8 rubber, 9 clear glass, 10 chrome; metallic < 0 makes glass); input(i, px) reads in0..in3",
            ),
            (
                "orbit",
                "vec3",
                "[30,20,8]",
                "camera [azimuth, elevation, distance] around target, degrees and world units",
            ),
            ("target", "vec3", "[0,0,0]", "what the camera looks at"),
            ("fov", "number", "35", "vertical, degrees"),
            ("light", "vec3", "[-4,5,4]", "point light position (casts soft shadows)"),
            ("light_color", "color", "#fff2e0", "light color"),
            ("light_intensity", "number", "40", "inverse-square intensity"),
            (
                "environment",
                "enum(studio|room|night|sky|lightbox|horizon)",
                "studio",
                "what reflections and ambient light see (lightbox: bars of light, made for glass and chrome)",
            ),
            (
                "background",
                "text",
                "environment",
                "environment, transparent (to composite over 2D), or a color",
            ),
            (
                "background_level",
                "number",
                "0.35",
                "brightness of an environment background",
            ),
            ("exposure", "number", "1", "before tone mapping"),
            ("samples", "number", "2", "rays per pixel (1..8), jittered"),
            (
                "bounce",
                "bool",
                "true",
                "trace one reflection bounce for glossy surfaces",
            ),
            (
                "ior",
                "number",
                "1.5",
                "glass index of refraction (materials with metallic < 0, preset 9)",
            ),
            ("dispersion", "number", "1", "glass color splitting (0 none, 2 strong)"),
            ("max_distance", "number", "80", "rays stop here"),
            (
                "k0",
                "number",
                "",
                "k0..k4: free knobs read as knob(0..4) in the scene (animatable)",
            ),
            ("resolution", "number", "1", "render scale"),
        ],
        extra: &[],
    },
    OpSchema {
        name: "transition",
        summary: "from shot `a` to shot `b` over `duration` s starting `at`: only a renders before, only b after",
        ports: &["a", "b"],
        params: &[
            (
                "kind",
                "enum(crossfade|dip|wipe|push|slide|iris|zoom|dissolve|glitch|blur)",
                "crossfade",
                "how: dip goes through `color`; wipe/push/slide move along `direction`; iris opens from `center`; dissolve burns through noise",
            ),
            ("at", "number", "0", "start, seconds"),
            ("duration", "number", "0.8", "seconds"),
            ("direction", "number", "0", "degrees: 0 moves rightward, 90 downward"),
            ("softness", "number", "per kind", "edge softness (blur: strength)"),
            (
                "color",
                "color",
                "#000000",
                "dip color; wipe and dissolve edge glow color (dissolve: ember #ff9a4a)",
            ),
            ("glow", "number", "0 (dissolve 2)", "edge glow strength"),
            ("center", "vec2", "frame center", "iris and zoom center px"),
            ("grain", "number", "160", "dissolve noise size px"),
            (
                "ease",
                "text|number[]",
                "per kind",
                "progress easing: a name (in_out_cubic) or [x1, y1, x2, y2]",
            ),
        ],
        extra: &[],
    },
    OpSchema {
        name: "block",
        summary: "a ready-made animated block in one house style: title, lower_third, quote, stat, chapter, end_card, and charts: bar_chart, line_chart, donut (place it in time with a timing offset)",
        ports: &[],
        params: &[
            (
                "kind",
                "enum(title|lower_third|quote|stat|chapter|end_card|bar_chart|line_chart|donut)",
                "title",
                "which block",
            ),
            (
                "title",
                "text",
                "",
                "title / chapter / end_card / charts (above the chart)",
            ),
            ("kicker", "text", "", "title, charts: small line above"),
            ("subtitle", "text", "", "title: line below"),
            ("number", "text", "", "chapter: its number, e.g. 01"),
            ("name", "text", "", "lower_third"),
            ("role", "text", "", "lower_third"),
            ("quote", "text", "", "quote"),
            ("author", "text", "", "quote"),
            ("value", "number", "0", "stat: counts up to it"),
            ("prefix", "text", "", "stat, bar and line values"),
            ("suffix", "text", "", "stat, bar and line values"),
            ("decimals", "number", "0", "stat, bar and line values"),
            ("label", "text", "", "stat"),
            ("tagline", "text", "", "end_card"),
            ("cta", "text", "", "end_card: a pill button"),
            ("logo", "text", "", "end_card: SVG file"),
            ("values", "number[]", "", "charts: the data, e.g. [24, 31, 58, 265]"),
            (
                "labels",
                "text",
                "",
                "charts: one per value, comma separated (or | when labels hold commas)",
            ),
            (
                "highlight",
                "number",
                "bar, donut: the largest; line: the last",
                "charts: index of the value set in the accent and called out (-1: none)",
            ),
            ("height", "number", "34% of the height", "charts: plot height px"),
            (
                "max",
                "number",
                "largest value",
                "bar, line: the value at the top of the plot",
            ),
            ("min", "number", "0 or the smallest", "line: the value at the baseline"),
            (
                "color",
                "color",
                "muted (line: accent)",
                "bar: other bars; line: the line",
            ),
            ("area", "bool", "true", "line: fill under the line"),
            ("draw_time", "number", "1.6", "line: seconds the line takes to draw"),
            (
                "colors",
                "text",
                "accent, then a palette",
                "donut: segment colors, comma separated",
            ),
            ("radius", "number", "fits the plot", "donut: outer radius px"),
            ("thickness", "number", "30% of radius", "donut: ring thickness px"),
            (
                "number_font",
                "text",
                "ultrafast:wide",
                "stat, donut: the big number's face",
            ),
            ("title_font", "text", "ultrafast:wide", "end_card: the title's face"),
            (
                "duration",
                "number",
                "3.5 (lower_third 4, quote 5, chapter 2.5, charts 5)",
                "seconds before it exits (end_card stays)",
            ),
            ("ink", "color", "#f6f3ee", "type color"),
            ("muted", "color", "ink at 72%", "secondary type"),
            ("accent", "color", "#ff5c7a", "rules, bars, quote mark, pill"),
            ("display_font", "text", "ultrafast:display", "headline face"),
            ("text_font", "text", "ultrafast:sans", "body face"),
            ("mono_font", "text", "ultrafast:mono", "labels"),
            ("align", "enum(left|center)", "left", "alignment (end_card is centered)"),
            (
                "position",
                "vec2",
                "per kind",
                "title/quote/stat: top of the block; lower_third: bottom-left; end_card: center",
            ),
            ("width", "number", "80% of the width", "wrap width px"),
            ("scale", "number", "1", "size multiplier (sizes follow the frame)"),
            ("opacity", "number", "1", "0..1"),
        ],
        extra: &[],
    },
    OpSchema {
        name: "lottie",
        summary: "a Lottie (Bodymovin) animation as editable ultrafast nodes: shape layers become paths with Lottie's keyframes, transforms stay vector; loops by default",
        ports: &[],
        params: &[
            ("src", "text", "", "Lottie .json file"),
            ("size", "vec2|number", "the animation's size", "fit it into this box px"),
            ("loop", "bool", "true", "repeat over its duration"),
            ("position", "vec2", "[0,0]", "where its top-left (or anchor) lands px"),
            ("anchor", "vec2", "[0,0]", "pivot px in its fitted space"),
            ("scale", "vec2|number", "[1,1]", "scale"),
            ("rotation", "number", "0", "degrees"),
            ("opacity", "number", "1", "0..1"),
        ],
        extra: &[],
    },
    OpSchema {
        name: "code",
        summary: "source code in a panel: syntax-highlighted, optionally typed on with a caret, line numbers, highlighted lines, window chrome",
        ports: &[],
        params: &[
            (
                "code",
                "text",
                "",
                "the source (\\n between lines; tabs become 4 spaces)",
            ),
            (
                "language",
                "enum(rust|python|js|ts|json|bash|go|swift|c|cpp|slang|sql|css|html|text)",
                "text",
                "how to color it",
            ),
            (
                "theme",
                "enum(ultrafast|paper|mono)",
                "ultrafast",
                "colors (paper is light)",
            ),
            ("title", "text", "", "file name in the title bar"),
            ("window", "bool", "true", "title bar with three dots"),
            ("background", "bool", "true", "the panel and its shadow"),
            ("line_numbers", "bool", "true", "gutter numbers"),
            ("highlight", "text", "", "lines to mark, e.g. \"3\" or \"2,5-7\""),
            ("highlight_at", "number", "0", "when the marked lines light up"),
            (
                "type_speed",
                "number",
                "0",
                "characters per second typed on (0: all shown)",
            ),
            ("at", "number", "0", "when typing starts"),
            ("caret", "bool", "true", "a blinking caret while and after typing"),
            ("font", "text", "ultrafast:mono", "a monospaced face"),
            ("size", "number", "30 at 1080p", "px"),
            ("line_height", "number", "1.55", "multiple of size"),
            ("padding", "number", "1.4 x size", "px inside the panel"),
            ("radius", "number", "0.55 x size", "panel corner radius px"),
            ("width", "number", "fits the longest line", "panel width px"),
            ("position", "vec2", "centered", "top-left of the panel"),
            ("opacity", "number", "1", "0..1"),
        ],
        extra: &[],
    },
    OpSchema {
        name: "stack",
        summary: "lays its inputs (in0, in1, ...: text, path, shape, svg or stack) out in a column or row by their measured bounds",
        ports: &[
            "in0", "in1", "in2", "in3", "in4", "in5", "in6", "in7", "in8", "in9", "in10", "in11", "in12", "in13",
            "in14", "in15",
        ],
        params: &[
            ("direction", "enum(column|row)", "column", "stack downward or rightward"),
            (
                "at",
                "number",
                "0",
                "seconds at which children are measured (when their entrances have landed)",
            ),
            ("gap", "number", "24", "px between children"),
            ("align", "enum(start|center|end)", "start", "children across the stack"),
            ("position", "vec2", "[0,0]", "where the block's anchor goes"),
            (
                "anchor",
                "enum(top_left|top|top_right|left|center|right|bottom_left|bottom|bottom_right)",
                "top_left",
                "which point of the block sits at position",
            ),
        ],
        extra: &[],
    },
    OpSchema {
        name: "clip_camera",
        summary: "video framed on its speaker: faces are tracked from the footage (or given), then a follow camera with dead zone and eased pans, cuts on shots and speaker changes, scope zoom, protect mode (blurred cover behind the contained frame)",
        ports: &[],
        params: &[
            ("src", "text", "", "source video"),
            (
                "faces",
                "text",
                "tracked from src",
                "face tracks, inline as JSON text or a file: {sourceWidth, sourceHeight, fps, frames: [{frame, faces: [{id, x, y, w, h, talk, focus}]}], shots: [{startFrame, endFrame, protect, cinematicScope}]}; omit it and the clip's faces, who is talking and its cuts are tracked from the footage (`ultrafast faces` writes the same)",
            ),
            ("start", "number", "0", "source seconds where the clip begins"),
            ("duration", "number", "document duration", "clip seconds"),
            ("tuning", "text", "", "camera tuning overrides as JSON"),
            ("opacity", "number", "1", "0..1"),
        ],
        extra: &[],
    },
    OpSchema {
        name: "captions",
        summary: "animated captions from word timings (karaoke, pop, reveal): pages laid out to fit, every word its own text node",
        ports: &[],
        params: &[
            ("src", "text", "", "transcript: Whisper/word JSON, .srt or .vtt"),
            (
                "words",
                "text",
                "",
                "inline instead: lines of `start end word`, SRT text or JSON",
            ),
            (
                "style",
                "enum(karaoke|pop|reveal|plain)",
                "karaoke",
                "karaoke lights the spoken word; pop pops each word on; reveal fades them on",
            ),
            ("font", "text", "Helvetica Neue", "family or file"),
            ("size", "number", "6.5% of the short side", "px"),
            ("weight", "number", "800", "100..900"),
            ("color", "color", "#ffffff", "words not yet spoken"),
            ("highlight", "color", "#ffd23f", "the word being spoken"),
            ("spoken", "color", "color", "words already spoken"),
            (
                "highlight_background",
                "color",
                "",
                "a rounded pill behind the spoken word",
            ),
            ("background", "color", "", "a rounded box behind each page"),
            ("uppercase", "bool", "false", "set in capitals"),
            ("tracking", "number", "0", "extra px between glyphs"),
            ("width", "number", "82% of the width", "line width px"),
            ("max_lines", "number", "2", "lines per page"),
            ("line_height", "number", "1.18", "multiple of size"),
            (
                "position",
                "vec2",
                "[w/2, 0.72 h]",
                "center of the caption block (moved into `safe` if it would cross it)",
            ),
            ("align", "enum(left|center|right)", "center", "line alignment"),
            (
                "safe",
                "enum(auto|social|margin|none)",
                "auto",
                "area pages stay inside: auto is clear of the social apps' buttons and captions on a vertical frame (what `review --safe social` checks) and a 4% margin otherwise; none places them exactly at position",
            ),
            ("linger", "number", "0.6", "seconds a page stays after its last word"),
            ("opacity", "number", "1", "0..1"),
        ],
        extra: &[],
    },
    OpSchema {
        name: "svg",
        summary: "an SVG file (logo, icon, illustration) as vector paths: fills, strokes, gradients, blend modes, and text as outlines in the fonts the text op finds; `ultrafast import-svg` gives the same as editable nodes. It composites in sRGB, like a browser: put it on the film with an `over` in `srgb` space to match one there too",
        ports: &[],
        params: &[
            ("src", "text", "", ".svg file (relative to the document)"),
            (
                "size",
                "vec2|number",
                "the SVG's own size",
                "fit inside [w, h] px, keeping its aspect",
            ),
            ("position", "vec2", "[0,0]", "where the anchor lands, px"),
            ("anchor", "vec2", "[0,0]", "pivot in the artwork's px (after size)"),
            (
                "scale",
                "vec2|number",
                "[1,1]",
                "scale around the anchor (stays vector-sharp)",
            ),
            ("rotation", "number", "0", "degrees, clockwise"),
            ("opacity", "number", "1", "0..1"),
        ],
        extra: &[],
    },
    OpSchema {
        name: "audio",
        summary: "sound mixed into exports",
        ports: &[],
        params: &[
            ("src", "text", "", "any media file with audio"),
            ("start", "number", "0", "source seconds playing at local time `from`"),
            ("gain", "number", "1", "linear, animatable"),
            (
                "from",
                "number",
                "0",
                "local seconds the sound starts at (silent before)",
            ),
            (
                "duration",
                "number",
                "0",
                "seconds it plays from `from` (0: until the source ends)",
            ),
            (
                "rate",
                "number",
                "1",
                "playback speed with the pitch kept (time stretched)",
            ),
        ],
        extra: &[],
    },
];

fn find(op: &str) -> Option<&'static OpSchema> {
    SCHEMAS.iter().find(|s| s.name == op)
}

/// Params whose animation changes how a node looks without moving anything
/// on screen (opacities, colors, grades, light levels): motion blur does
/// not sample them.
pub fn moves_nothing(op: &str, param: &str) -> bool {
    match (op, param) {
        (_, "opacity" | "glyph_opacity") | ("adjust" | "levels", _) | ("lut" | "invert", "amount") => true,
        ("light", "intensity") | ("render3d", "ambient") | ("raymarch", "light_intensity" | "exposure") => true,
        _ => find(op).is_some_and(|s| {
            s.params
                .iter()
                .chain(s.extra.iter().flat_map(|e| e.iter()))
                .any(|p| p.0 == param && p.1 == "color")
        }),
    }
}

fn all_params(s: &OpSchema) -> impl Iterator<Item = &'static P> {
    s.params.iter().chain(s.extra.iter().flat_map(|g| g.iter()))
}

/// The op's entry for param `k`, counting the numbered families (`u0` to
/// `u15`, `k0` to `k4`) and `axis_<tag>`.
fn op_param(s: &OpSchema, k: &str) -> Option<&'static P> {
    all_params(s).find(|p| {
        p.0 == k
            || (k.starts_with('u') && p.0 == "u0" && k[1..].parse::<u8>().is_ok_and(|i| i < 16))
            || (k.starts_with('k') && p.0 == "k0" && k[1..].parse::<u8>().is_ok_and(|i| i < 5))
            || (k.starts_with("axis_") && p.0 == "axis_<tag>" && (6..=9).contains(&k.len()))
    })
}

/// The component `op` names: the document's own, else a builtin.
fn component<'a>(doc: &'a Document, op: &str) -> Option<&'a ultrafast_graph::Component> {
    doc.components.get(op).or_else(|| {
        crate::builtins::components()
            .iter()
            .find(|(n, _)| n == op)
            .map(|(_, c)| c)
    })
}

/// Why a node whose op is `op` takes no param `k`, with the nearest it
/// does take (what [`check`] warns), or `None` when it takes `k` or `op`
/// is unknown.
pub fn unknown_param(doc: &Document, op: &str, k: &str) -> Option<String> {
    let hint = |names: &mut dyn Iterator<Item = &str>| {
        names
            .min_by_key(|n| edit_distance(n, k))
            .filter(|n| edit_distance(n, k) <= 3)
            .map(|n| format!(" (did you mean `{n}`?)"))
            .unwrap_or_default()
    };
    if let Some(c) = component(doc, op) {
        // Declared, or used as `$k` inside: every param an instance sets
        // reaches its nodes.
        let used = c.nodes.values().any(|n| {
            n.params
                .values()
                .any(|v| matches!(v, Param::Text(s) if s.strip_prefix('$') == Some(k)))
        });
        return (!c.params.contains_key(k) && !used).then(|| {
            format!(
                "component `{op}` has no param `{k}`{}",
                hint(&mut c.params.keys().map(String::as_str))
            )
        });
    }
    let s = find(op)?;
    op_param(s, k)
        .is_none()
        .then(|| format!("`{op}` has no param `{k}`{}", hint(&mut all_params(s).map(|p| p.0))))
}

/// The full vocabulary as JSON.
pub fn vocabulary() -> Value {
    let mut ops: Vec<Value> = SCHEMAS
        .iter()
        .map(|s| {
            json!({
                "op": s.name,
                "summary": s.summary,
                "ports": s.ports,
                "params": all_params(s).map(|(n, ty, d, desc)| json!({ "name": n, "type": ty, "default": d, "doc": desc })).collect::<Vec<_>>(),
            })
        })
        .collect();
    for (name, c) in crate::builtins::components() {
        ops.push(json!({
            "op": name,
            "summary": crate::builtins::summary(name),
            "ports": c.inputs,
            "params": c
                .params
                .iter()
                .map(|(k, v)| {
                    let v = serde_json::to_value(v).unwrap();
                    // A builtin's params are its defaults; their form says the type.
                    let ty = match &v {
                        Value::Number(_) => "number",
                        Value::Bool(_) => "bool",
                        Value::String(s) if s.starts_with('#') => "color",
                        Value::String(_) => "text",
                        Value::Array(a) if a.len() == 2 => "vec2",
                        Value::Array(_) => "vector",
                        _ => "animated",
                    };
                    let default = match &v {
                        Value::String(s) => s.clone(),
                        other => other.to_string(),
                    };
                    json!({ "name": k, "type": ty, "default": default })
                })
                .collect::<Vec<_>>(),
        }));
    }
    json!(ops)
}

/// A problem found by [`check`].
#[derive(Debug, Clone)]
pub struct Issue {
    pub node: String,
    pub severity: &'static str,
    pub message: String,
}

/// Static checks agents can run before rendering: unknown ops, misspelled
/// params (with suggestions), wrong value shapes, missing inputs, unused
/// nodes.
pub fn check(doc: &Document) -> Vec<Issue> {
    let mut out = Vec::new();
    let mut issue = |node: &str, severity: &'static str, message: String| {
        out.push(Issue {
            node: node.to_string(),
            severity,
            message,
        });
    };
    let mut graphs: Vec<(String, &std::collections::BTreeMap<String, ultrafast_graph::Node>)> =
        vec![(String::new(), &doc.nodes)];
    for (name, c) in &doc.components {
        graphs.push((format!("{name}/"), &c.nodes));
    }
    for (prefix, nodes) in graphs {
        for (id, node) in nodes {
            let path = format!("{prefix}{id}");
            if let Some(c) = component(doc, &node.op) {
                for k in node.params.keys() {
                    if let Some(why) = unknown_param(doc, &node.op, k) {
                        issue(&path, "warning", why);
                    }
                }
                for p in node.inputs.keys() {
                    if !c.inputs.contains(p) {
                        issue(&path, "warning", format!("component `{}` has no input `{p}`", node.op));
                    }
                }
                continue;
            }
            let Some(schema) = find(&node.op) else {
                issue(&path, "error", format!("unknown op `{}`", node.op));
                continue;
            };
            for (k, v) in &node.params {
                if prefix.is_empty() || !matches!(v, Param::Text(s) if s.starts_with('$')) {
                    match op_param(schema, k) {
                        None => {
                            let why = unknown_param(doc, &node.op, k).unwrap_or_default();
                            issue(&path, "warning", format!("{why}; it is ignored"));
                        }
                        Some(p) => {
                            if let Param::Expr { expr } = v {
                                for e in expr.errors() {
                                    issue(&path, "error", format!("param `{k}`: {e}"));
                                }
                            }
                            if let Some(msg) = type_mismatch(p.1, v) {
                                issue(&path, "warning", format!("param `{k}`: {msg}"));
                            }
                        }
                    }
                }
            }
            if schema.ports != ["*"] {
                for p in node.inputs.keys() {
                    if !schema.ports.contains(&p.as_str()) {
                        issue(
                            &path,
                            "warning",
                            format!("`{}` has no input `{p}` (ports: {:?})", node.op, schema.ports),
                        );
                    }
                }
                let optional = matches!(
                    node.op.as_str(),
                    "shader" | "raymarch" | "mesh" | "particles" | "fluid" | "feedback" | "stack"
                );
                for p in schema.ports {
                    if !optional && !node.inputs.contains_key(*p) {
                        issue(&path, "error", format!("missing input `{p}`"));
                    }
                }
            }
        }
    }
    if let Ok(order) = doc.reachable_topo_order() {
        for id in doc.nodes.keys() {
            let used_elsewhere = doc.nodes.get(id).is_some_and(|n| n.op == "audio");
            if !order.contains(id) && !used_elsewhere {
                issue(id, "info", "not reachable from the output (not rendered)".into());
            }
        }
        // A timed effect (a crop, a mask, a transform...) passes its source
        // through untouched outside its window: if the source shows longer
        // than the effect, that is the uncropped, unmasked source on screen.
        // When each node can reach the screen (a hull): everything at the
        // output; a transition shows `a` only until it ends and `b` only
        // from when it starts.
        let mut reach: std::collections::HashMap<&str, (f64, f64)> = std::collections::HashMap::new();
        reach.insert(doc.output.as_str(), (0.0, doc.format.duration));
        let num = |n: &ultrafast_graph::Node, k: &str, d: f64| match n.params.get(k) {
            Some(Param::Number(v)) => *v,
            _ => d,
        };
        for id in order.iter().rev() {
            let Some(&(ra, rb)) = reach.get(id.as_str()) else {
                continue;
            };
            let node = &doc.nodes[id];
            // A timed node shows its inputs only inside its window, except the
            // one it passes through outside it.
            let window = node.timing.filter(|t| t.has_window()).map(|t| {
                (
                    t.in_point.unwrap_or(f64::NEG_INFINITY),
                    t.out_point.unwrap_or(f64::INFINITY),
                )
            });
            let through = through_port(doc, id, node);
            for (port, src) in &node.inputs {
                let (mut a, mut b) = (ra, rb);
                if let Some((wa, wb)) = window
                    && through.as_deref() != Some(port.as_str())
                {
                    a = a.max(wa);
                    b = b.min(wb);
                }
                if node.op == "transition" {
                    let at = num(node, "at", 0.0);
                    let end = at + num(node, "duration", 0.8);
                    if port == "a" {
                        b = b.min(end);
                    } else if port == "b" {
                        a = a.max(at);
                    }
                }
                if a < b {
                    let e = reach.entry(src.as_str()).or_insert((a, b));
                    *e = (e.0.min(a), e.1.max(b));
                }
            }
        }
        for id in &order {
            let node = &doc.nodes[id];
            let Some(w) = node.timing.filter(|t| t.has_window()) else {
                continue;
            };
            // Only effects that bound, place or hide their source leak when
            // they pass it through: a timed blur or grade is meant to apply
            // for a moment and leave the picture as it was.
            if !matches!(
                node.op.as_str(),
                "crop" | "mask" | "transform" | "corner_pin" | "opacity" | "tile" | "wave" | "twirl" | "displace"
            ) {
                continue;
            }
            let Some(port) = through_port(doc, id, node) else {
                continue;
            };
            let Some(src) = node.inputs.get(&port) else { continue };
            let (a, b) = visible(doc, src, 0);
            let (wa, wb) = (
                w.in_point.unwrap_or(f64::NEG_INFINITY),
                w.out_point.unwrap_or(f64::INFINITY),
            );
            // Leaks only where the effect reaches the screen.
            let (ra, rb) = reach
                .get(id.as_str())
                .copied()
                .unwrap_or((f64::NEG_INFINITY, f64::INFINITY));
            let (a, b) = (a.max(ra), b.min(rb));
            let before = a < wa - 1e-6 && a < b;
            let after = b > wb + 1e-6 && a < b;
            if before || after {
                issue(
                    id,
                    "warning",
                    format!(
                        "outside its window ({}) `{id}` passes `{src}` through as it is, and `{src}` shows {}: give `{src}` the same window, or time a composite above both",
                        span(wa, wb),
                        span(a, b)
                    ),
                );
            }
        }
    }
    out
}

/// The input a node shows unchanged outside its timing window: an op's pass
/// through port, or `source` for a component instance that takes one (an
/// effect: its inner nodes go inactive and pass their sources on).
fn through_port(doc: &Document, id: &str, n: &ultrafast_graph::Node) -> Option<String> {
    let component =
        doc.components.contains_key(&n.op) || crate::builtins::components().iter().any(|(name, _)| *name == n.op);
    if component {
        return n.inputs.contains_key("source").then(|| "source".to_string());
    }
    let def = crate::ops::resolve(id, n).ok()?;
    let ports = crate::ops::connected_ports(def, id, n).ok()?;
    let k = crate::ops::passthrough(def, ports.len())?;
    ports.get(k).cloned()
}

/// When a node can show anything: its own window for a generator, its
/// inputs' for effects and composites (a window on those narrows nothing:
/// outside it they pass an input through). Hulls, not exact sets.
fn visible(doc: &Document, id: &str, depth: usize) -> (f64, f64) {
    let all = (f64::NEG_INFINITY, f64::INFINITY);
    let Some(n) = doc.nodes.get(id) else { return all };
    // Generators that read images (a particle sprite, a raymarch texture,
    // 3D) show only in their own window, whatever their inputs do.
    let generator = matches!(
        n.op.as_str(),
        "particles" | "raymarch" | "render3d" | "swarm" | "fluid" | "feedback"
    );
    let own = n
        .timing
        .filter(|t| t.has_window())
        .map(|t| {
            (
                t.in_point.unwrap_or(f64::NEG_INFINITY),
                t.out_point.unwrap_or(f64::INFINITY),
            )
        })
        .unwrap_or(all);
    if n.inputs.is_empty() || generator || depth > 64 {
        return own;
    }
    // A mask (not inverted) shows only where and when its matte does.
    let inverted = matches!(n.params.get("mode"), Some(Param::Text(m)) if m.ends_with("inverted"));
    if n.op == "mask" && !inverted {
        let empty = (f64::INFINITY, f64::NEG_INFINITY);
        let src = n
            .inputs
            .get("source")
            .map(|s| visible(doc, s, depth + 1))
            .unwrap_or(empty);
        let matte = n
            .inputs
            .get("matte")
            .map(|s| visible(doc, s, depth + 1))
            .unwrap_or(empty);
        let inside = (src.0.max(matte.0).max(own.0), src.1.min(matte.1).min(own.1));
        let mut hull = if inside.0 < inside.1 { inside } else { empty };
        // Outside its own window it passes the source through.
        let film = (0.0, doc.format.duration);
        for (a, b) in [(src.0, src.1.min(own.0)), (src.0.max(own.1), src.1)] {
            let (a, b) = (a.max(film.0), b.min(film.1));
            if a < b {
                hull = (hull.0.min(a), hull.1.max(b));
            }
        }
        return hull;
    }
    if n.op == "transition" {
        let num = |k: &str, d: f64| match n.params.get(k) {
            Some(Param::Number(v)) => *v,
            _ => d,
        };
        let at = num("at", 0.0);
        let end = at + num("duration", 0.8);
        let a = n
            .inputs
            .get("a")
            .map(|s| visible(doc, s, depth + 1))
            .unwrap_or((f64::INFINITY, f64::NEG_INFINITY));
        let b = n
            .inputs
            .get("b")
            .map(|s| visible(doc, s, depth + 1))
            .unwrap_or((f64::INFINITY, f64::NEG_INFINITY));
        let a = (a.0, a.1.min(end));
        let b = (b.0.max(at), b.1);
        return (a.0.min(b.0), a.1.max(b.1));
    }
    // With a window, inputs show only inside it, except the one the node
    // passes through outside it.
    let through = n
        .timing
        .filter(|t| t.has_window())
        .and_then(|_| through_port(doc, id, n));
    let windowed = n.timing.is_some_and(|t| t.has_window());
    let mut hull = (f64::INFINITY, f64::NEG_INFINITY);
    for (port, src) in &n.inputs {
        let (mut a, mut b) = visible(doc, src, depth + 1);
        if windowed && through.as_deref() != Some(port.as_str()) {
            a = a.max(own.0);
            b = b.min(own.1);
        }
        if a < b {
            hull = (hull.0.min(a), hull.1.max(b));
        }
    }
    hull
}

fn span(a: f64, b: f64) -> String {
    let t = |v: f64, open: &str| {
        if v.is_finite() {
            format!("{v:.2} s")
        } else {
            open.to_string()
        }
    };
    format!("{} to {}", t(a, "the start"), t(b, "the end"))
}

fn type_mismatch(ty: &str, v: &Param) -> Option<String> {
    let numeric = !matches!(v, Param::Text(_) | Param::Bool(_));
    let len = match v {
        Param::Number(_) => Some(1),
        Param::Vector(x) => Some(x.len()),
        Param::Animated { keys, .. } => keys.first().map(|k| k.v.len()),
        // Drivers: their value's shape at t = 0.
        p => p.eval(0.0).map(|x| x.len()),
    };
    let ok = match ty {
        "color" => matches!(v, Param::Text(s) if s.starts_with('#')) || matches!(len, Some(3 | 4)),
        "number" => len == Some(1),
        // Scalars broadcast to vectors.
        "vec2" => matches!(len, Some(1 | 2)),
        "vec3" => matches!(len, Some(1 | 3)),
        "vec4" => matches!(len, Some(1 | 4)),
        "vec2|number" => matches!(len, Some(1 | 2)),
        "vec3|number" => matches!(len, Some(1 | 3)),
        "number[]" => numeric,
        "text" => matches!(v, Param::Text(_)),
        "text|number[]" => matches!(v, Param::Text(_)) || numeric,
        "bool" => matches!(v, Param::Bool(_)),
        t if t.starts_with("enum(") => match v {
            Param::Text(s) => t[5..t.len() - 1].split('|').any(|o| o == s),
            _ => false,
        },
        _ => true,
    };
    (!ok).then(|| format!("expected {ty}, got {}", serde_json::to_string(v).unwrap_or_default()))
}

/// Edits from `a` to `b`, two letters swapped counting as one: `valeus`
/// is one from `values` and two from `value`.
pub(crate) fn edit_distance(a: &str, b: &str) -> usize {
    let (a, b): (Vec<char>, Vec<char>) = (a.chars().collect(), b.chars().collect());
    let mut d = vec![vec![0; b.len() + 1]; a.len() + 1];
    for (i, row) in d.iter_mut().enumerate() {
        row[0] = i;
    }
    for j in 0..=b.len() {
        d[0][j] = j;
    }
    for i in 1..=a.len() {
        for j in 1..=b.len() {
            let mut v = (d[i - 1][j] + 1)
                .min(d[i][j - 1] + 1)
                .min(d[i - 1][j - 1] + (a[i - 1] != b[j - 1]) as usize);
            if i > 1 && j > 1 && a[i - 1] == b[j - 2] && a[i - 2] == b[j - 1] {
                v = v.min(d[i - 2][j - 2] + 1);
            }
            d[i][j] = v;
        }
    }
    d[a.len()][b.len()]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_op_has_a_schema() {
        for d in crate::ops::OPS {
            assert!(find(d.name).is_some(), "no schema for {}", d.name);
        }
    }

    #[test]
    fn a_swapped_pair_is_one_edit() {
        assert_eq!(edit_distance("valeus", "values"), 1);
        assert_eq!(edit_distance("valeus", "value"), 2);
        assert_eq!(edit_distance("kitten", "sitting"), 3);
        assert_eq!(edit_distance("", "ab"), 2);
        let doc = Document::from_json(
            r#"{"version":1,"format":{"width":100,"height":100,"fps":30,"duration":1},"output":"c",
            "nodes":{"c":{"op":"block","params":{"kind":"bar_chart","values":[1],"valeus":[2]}}}}"#,
        )
        .unwrap();
        let msgs: Vec<String> = check(&doc).into_iter().map(|i| i.message).collect();
        assert!(
            msgs.contains(&"`block` has no param `valeus` (did you mean `values`?); it is ignored".to_string()),
            "{msgs:?}"
        );
    }

    #[test]
    fn params_a_node_does_take_are_known() {
        let doc = Document::from_json(
            r#"{"version":1,"format":{"width":100,"height":100,"fps":30,"duration":1},"output":"o",
            "components":{"card":{"output":"t","params":{"size":40},
                "nodes":{"t":{"op":"text","params":{"text":"$title","size":"$size"}}}}},
            "nodes":{"c":{"op":"card","params":{"title":"Hi","sise":30}},
                     "e":{"op":"block","params":{"kind":"end_card","title_font":"ultrafast:sans"}},
                     "o":{"op":"over","inputs":{"fg":"c","bg":"e"}}}}"#,
        )
        .unwrap();
        // The end card reads `title_font`; a component's nodes read every
        // param its instance sets, declared or not (`title` here).
        assert_eq!(unknown_param(&doc, "block", "title_font"), None);
        assert_eq!(unknown_param(&doc, "card", "title"), None);
        assert_eq!(
            unknown_param(&doc, "card", "sise").as_deref(),
            Some("component `card` has no param `sise` (did you mean `size`?)")
        );
        let msgs: Vec<String> = check(&doc).into_iter().map(|i| i.message).collect();
        assert_eq!(msgs, ["component `card` has no param `sise` (did you mean `size`?)"]);
    }

    #[test]
    fn check_flags_an_effect_that_would_pass_its_source_through() {
        // The crop is on for 1..2 s but the video under it shows the whole
        // film: before and after, the uncropped picture would be on screen.
        let doc = Document::from_json(
            r##"{"version":1,"format":{"width":64,"height":64,"fps":30,"duration":3},"output":"o",
            "nodes":{"bg":{"op":"solid"},"pic":{"op":"checker"},
                     "cut":{"op":"crop","params":{"rect":[0,0,32,32]},"inputs":{"source":"pic"},"timing":{"in":1,"out":2}},
                     "o":{"op":"over","inputs":{"fg":"cut","bg":"bg"}}}}"##,
        )
        .unwrap();
        let found = check(&doc);
        assert!(
            found
                .iter()
                .any(|i| i.node == "cut" && i.severity == "warning" && i.message.contains("passes `pic` through")),
            "{found:?}"
        );
        // Given the same window, the picture cannot leak.
        let mut doc = doc;
        doc.nodes.get_mut("pic").unwrap().timing = doc.nodes["cut"].timing;
        assert!(!check(&doc).iter().any(|i| i.severity == "warning"));
    }

    #[test]
    fn check_flags_typos_and_shapes() {
        let doc = Document::from_json(
            r##"{"version":1,"format":{"width":8,"height":8,"fps":30,"duration":1},"output":"s",
            "nodes":{"s":{"op":"shape","params":{"colour":"#ff0000","radius":[1,2],"kind":"triangle"}},
                     "x":{"op":"sparkle"}}}"##,
        )
        .unwrap();
        let issues = check(&doc);
        let text: Vec<String> = issues.iter().map(|i| format!("{}: {}", i.node, i.message)).collect();
        assert!(
            text.iter()
                .any(|t| t.contains("colour") && t.contains("did you mean `color`")),
            "{text:?}"
        );
        assert!(text.iter().any(|t| t.contains("param `radius`")), "{text:?}");
        assert!(text.iter().any(|t| t.contains("param `kind`")), "{text:?}");
        assert!(text.iter().any(|t| t.contains("unknown op `sparkle`")), "{text:?}");
    }
}
