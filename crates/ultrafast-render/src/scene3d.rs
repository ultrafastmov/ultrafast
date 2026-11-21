//! 3D scenes: meshes (primitives, glTF), lights and cameras as graph
//! nodes, rendered by a `render3d` node into an image like any other.
//!
//! Coordinates are right-handed with +y up; the default camera sits at
//! (0, 0, 5) looking at the origin. Units are arbitrary.

use crate::RenderError;
use crate::ops::text;
use std::path::Path;
use ultrafast_graph::{Node, Param};

/// Row-major 4x4 matrix; vectors are columns (`m * v`).
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Mat4(pub [f32; 16]);

impl Mat4 {
    pub const IDENTITY: Mat4 = Mat4([
        1.0, 0.0, 0.0, 0.0, 0.0, 1.0, 0.0, 0.0, 0.0, 0.0, 1.0, 0.0, 0.0, 0.0, 0.0, 1.0,
    ]);

    pub fn mul(&self, o: &Mat4) -> Mat4 {
        let mut r = [0.0f32; 16];
        for i in 0..4 {
            for j in 0..4 {
                r[i * 4 + j] = (0..4).map(|k| self.0[i * 4 + k] * o.0[k * 4 + j]).sum();
            }
        }
        Mat4(r)
    }

    pub fn translate(x: f32, y: f32, z: f32) -> Mat4 {
        let mut m = Self::IDENTITY;
        m.0[3] = x;
        m.0[7] = y;
        m.0[11] = z;
        m
    }

    pub fn scale(x: f32, y: f32, z: f32) -> Mat4 {
        let mut m = Self::IDENTITY;
        m.0[0] = x;
        m.0[5] = y;
        m.0[10] = z;
        m
    }

    /// Rotation from Euler angles in degrees, applied X, then Y, then Z.
    pub fn rotate_xyz(deg: [f32; 3]) -> Mat4 {
        let [x, y, z] = deg.map(f32::to_radians);
        let (sx, cx) = x.sin_cos();
        let (sy, cy) = y.sin_cos();
        let (sz, cz) = z.sin_cos();
        let rx = Mat4([
            1.0, 0.0, 0.0, 0.0, 0.0, cx, -sx, 0.0, 0.0, sx, cx, 0.0, 0.0, 0.0, 0.0, 1.0,
        ]);
        let ry = Mat4([
            cy, 0.0, sy, 0.0, 0.0, 1.0, 0.0, 0.0, -sy, 0.0, cy, 0.0, 0.0, 0.0, 0.0, 1.0,
        ]);
        let rz = Mat4([
            cz, -sz, 0.0, 0.0, sz, cz, 0.0, 0.0, 0.0, 0.0, 1.0, 0.0, 0.0, 0.0, 0.0, 1.0,
        ]);
        rz.mul(&ry).mul(&rx)
    }

    /// Right-handed perspective projection with depth in [0, 1].
    pub fn perspective(fov_y_deg: f32, aspect: f32, near: f32, far: f32) -> Mat4 {
        let f = 1.0 / (fov_y_deg.to_radians() / 2.0).tan();
        let a = far / (near - far);
        Mat4([
            f / aspect,
            0.0,
            0.0,
            0.0,
            0.0,
            f,
            0.0,
            0.0,
            0.0,
            0.0,
            a,
            a * near,
            0.0,
            0.0,
            -1.0,
            0.0,
        ])
    }

    pub fn look_at(eye: [f32; 3], target: [f32; 3], up: [f32; 3]) -> Mat4 {
        let f = normalize(sub(target, eye));
        let s = normalize(cross(f, up));
        let u = cross(s, f);
        Mat4([
            s[0],
            s[1],
            s[2],
            -dot(s, eye),
            u[0],
            u[1],
            u[2],
            -dot(u, eye),
            -f[0],
            -f[1],
            -f[2],
            dot(f, eye),
            0.0,
            0.0,
            0.0,
            1.0,
        ])
    }

    pub fn transpose(&self) -> Mat4 {
        let mut r = [0.0; 16];
        for i in 0..4 {
            for j in 0..4 {
                r[j * 4 + i] = self.0[i * 4 + j];
            }
        }
        Mat4(r)
    }

    /// Inverse of an affine matrix (rotation/scale/translation).
    pub fn inverse_affine(&self) -> Mat4 {
        let m = &self.0;
        let (a, b, c, d, e, f, g, h, i) = (m[0], m[1], m[2], m[4], m[5], m[6], m[8], m[9], m[10]);
        let det = a * (e * i - f * h) - b * (d * i - f * g) + c * (d * h - e * g);
        if det.abs() < 1e-12 {
            return Mat4::IDENTITY;
        }
        let inv = 1.0 / det;
        let r = [
            (e * i - f * h) * inv,
            (c * h - b * i) * inv,
            (b * f - c * e) * inv,
            (f * g - d * i) * inv,
            (a * i - c * g) * inv,
            (c * d - a * f) * inv,
            (d * h - e * g) * inv,
            (b * g - a * h) * inv,
            (a * e - b * d) * inv,
        ];
        let t = [m[3], m[7], m[11]];
        let tt = [
            -(r[0] * t[0] + r[1] * t[1] + r[2] * t[2]),
            -(r[3] * t[0] + r[4] * t[1] + r[5] * t[2]),
            -(r[6] * t[0] + r[7] * t[1] + r[8] * t[2]),
        ];
        Mat4([
            r[0], r[1], r[2], tt[0], r[3], r[4], r[5], tt[1], r[6], r[7], r[8], tt[2], 0.0, 0.0, 0.0, 1.0,
        ])
    }
}

fn sub(a: [f32; 3], b: [f32; 3]) -> [f32; 3] {
    [a[0] - b[0], a[1] - b[1], a[2] - b[2]]
}
fn dot(a: [f32; 3], b: [f32; 3]) -> f32 {
    a[0] * b[0] + a[1] * b[1] + a[2] * b[2]
}
fn cross(a: [f32; 3], b: [f32; 3]) -> [f32; 3] {
    [
        a[1] * b[2] - a[2] * b[1],
        a[2] * b[0] - a[0] * b[2],
        a[0] * b[1] - a[1] * b[0],
    ]
}
fn normalize(a: [f32; 3]) -> [f32; 3] {
    let l = dot(a, a).sqrt().max(1e-12);
    [a[0] / l, a[1] / l, a[2] / l]
}

/// Interleaved vertices (position, normal, uv: 8 floats) and indices.
#[derive(Debug, Clone, Default)]
pub struct MeshData {
    pub vertices: Vec<f32>,
    pub indices: Vec<u32>,
}

impl MeshData {
    fn vertex(&mut self, p: [f32; 3], n: [f32; 3], uv: [f32; 2]) -> u32 {
        self.vertices.extend([p[0], p[1], p[2], n[0], n[1], n[2], uv[0], uv[1]]);
        (self.vertices.len() / 8 - 1) as u32
    }

    /// A grid of vertices from `f(u, v) -> (position, normal)` over
    /// [0,1]^2 with `nu` x `nv` cells.
    fn grid(&mut self, nu: usize, nv: usize, f: impl Fn(f32, f32) -> ([f32; 3], [f32; 3])) {
        let base = (self.vertices.len() / 8) as u32;
        for j in 0..=nv {
            for i in 0..=nu {
                let (u, v) = (i as f32 / nu as f32, j as f32 / nv as f32);
                let (p, n) = f(u, v);
                self.vertex(p, n, [u, v]);
            }
        }
        let row = nu as u32 + 1;
        for j in 0..nv as u32 {
            for i in 0..nu as u32 {
                let a = base + j * row + i;
                self.indices.extend([a, a + 1, a + row, a + 1, a + row + 1, a + row]);
            }
        }
    }
}

/// Builds a primitive. All fit in a unit box centered at the origin.
pub fn primitive(kind: &str, segments: usize) -> Option<MeshData> {
    let mut m = MeshData::default();
    let seg = segments.max(3);
    let tau = std::f32::consts::TAU;
    match kind {
        "cube" => {
            // (normal, u axis, v axis) per face.
            let faces: [([f32; 3], [f32; 3], [f32; 3]); 6] = [
                ([0.0, 0.0, 1.0], [1.0, 0.0, 0.0], [0.0, -1.0, 0.0]),
                ([0.0, 0.0, -1.0], [-1.0, 0.0, 0.0], [0.0, -1.0, 0.0]),
                ([1.0, 0.0, 0.0], [0.0, 0.0, -1.0], [0.0, -1.0, 0.0]),
                ([-1.0, 0.0, 0.0], [0.0, 0.0, 1.0], [0.0, -1.0, 0.0]),
                ([0.0, 1.0, 0.0], [1.0, 0.0, 0.0], [0.0, 0.0, 1.0]),
                ([0.0, -1.0, 0.0], [1.0, 0.0, 0.0], [0.0, 0.0, -1.0]),
            ];
            for (n, u, v) in faces {
                m.grid(1, 1, |a, b| {
                    let (x, y) = (a - 0.5, b - 0.5);
                    let p = [
                        n[0] * 0.5 + u[0] * x + v[0] * y,
                        n[1] * 0.5 + u[1] * x + v[1] * y,
                        n[2] * 0.5 + u[2] * x + v[2] * y,
                    ];
                    (p, n)
                });
            }
        }
        "plane" => m.grid(1, 1, |u, v| ([u - 0.5, 0.5 - v, 0.0], [0.0, 0.0, 1.0])),
        "sphere" => m.grid(seg * 2, seg, |u, v| {
            let (theta, phi) = (u * tau, v * std::f32::consts::PI);
            let n = [-phi.sin() * theta.sin(), phi.cos(), -phi.sin() * theta.cos()];
            ([n[0] * 0.5, n[1] * 0.5, n[2] * 0.5], n)
        }),
        "cylinder" | "cone" => {
            let top = if kind == "cone" { 0.0 } else { 0.5 };
            let slope = (0.5 - top) / 1.0;
            m.grid(seg * 2, 1, |u, v| {
                let a = u * tau;
                let r = top + (0.5 - top) * v;
                let n = normalize([a.sin(), slope, a.cos()]);
                ([r * a.sin(), 0.5 - v, r * a.cos()], n)
            });
            // Caps as fans.
            for (y, r, ny) in [(0.5f32, top, 1.0f32), (-0.5, 0.5, -1.0)] {
                if r <= 0.0 {
                    continue;
                }
                let c = m.vertex([0.0, y, 0.0], [0.0, ny, 0.0], [0.5, 0.5]);
                let first = (m.vertices.len() / 8) as u32;
                for i in 0..=seg * 2 {
                    let a = i as f32 / (seg * 2) as f32 * tau;
                    m.vertex(
                        [r * a.sin(), y, r * a.cos()],
                        [0.0, ny, 0.0],
                        [0.5 + 0.5 * a.sin(), 0.5 + 0.5 * a.cos()],
                    );
                }
                for i in 0..(seg * 2) as u32 {
                    m.indices.extend([c, first + i, first + i + 1]);
                }
            }
        }
        "torus" => m.grid(seg * 2, seg, |u, v| {
            let (a, b) = (u * tau, v * tau);
            let (big, small) = (0.35, 0.15);
            let n = [b.cos() * a.sin(), b.sin(), b.cos() * a.cos()];
            (
                [
                    (big + small * b.cos()) * a.sin(),
                    small * b.sin(),
                    (big + small * b.cos()) * a.cos(),
                ],
                n,
            )
        }),
        _ => return None,
    }
    Some(m)
}

/// Loads every mesh of a glTF/GLB file, with node transforms applied.
pub fn load_gltf(path: &Path) -> Result<MeshData, String> {
    let (doc, buffers) = if crate::svg::external_files_allowed() {
        let (doc, buffers, _) = gltf::import(path).map_err(|e| format!("{}: {e}", path.display()))?;
        (doc, buffers)
    } else {
        // Embedded buffers only (a GLB's binary chunk, data URIs).
        let bytes = std::fs::read(path).map_err(|e| format!("{}: {e}", path.display()))?;
        let g = gltf::Gltf::from_slice(&bytes).map_err(|e| format!("{}: {e}", path.display()))?;
        let buffers = gltf::import_buffers(&g.document, None, g.blob)
            .map_err(|e| format!("{}: {e} (external files are off)", path.display()))?;
        (g.document, buffers)
    };
    let mut m = MeshData::default();
    fn visit(node: gltf::Node, parent: Mat4, buffers: &[gltf::buffer::Data], m: &mut MeshData) {
        let local = Mat4(node.transform().matrix().map(|c| c).concat().try_into().unwrap()).transpose();
        let world = parent.mul(&local);
        if let Some(mesh) = node.mesh() {
            let normal_m = world.inverse_affine().transpose();
            for prim in mesh.primitives() {
                let r = prim.reader(|b| Some(&buffers[b.index()]));
                let Some(pos) = r.read_positions() else { continue };
                let pos: Vec<[f32; 3]> = pos.collect();
                let normals: Vec<[f32; 3]> = r
                    .read_normals()
                    .map(|n| n.collect())
                    .unwrap_or_else(|| vec![[0.0, 1.0, 0.0]; pos.len()]);
                let uvs: Vec<[f32; 2]> = r
                    .read_tex_coords(0)
                    .map(|t| t.into_f32().collect())
                    .unwrap_or_else(|| vec![[0.0, 0.0]; pos.len()]);
                let base = (m.vertices.len() / 8) as u32;
                for i in 0..pos.len() {
                    let p = apply(&world, pos[i], 1.0);
                    let n = normalize(apply(&normal_m, normals[i], 0.0));
                    m.vertex(p, n, uvs[i]);
                }
                match r.read_indices() {
                    Some(ix) => m.indices.extend(ix.into_u32().map(|i| base + i)),
                    None => m.indices.extend((0..pos.len() as u32).map(|i| base + i)),
                }
            }
        }
        for child in node.children() {
            visit(child, world, buffers, m);
        }
    }
    let scene = doc
        .default_scene()
        .or_else(|| doc.scenes().next())
        .ok_or("no scene in file")?;
    for node in scene.nodes() {
        visit(node, Mat4::IDENTITY, &buffers, &mut m);
    }
    if m.indices.is_empty() {
        return Err(format!("{}: no triangles", path.display()));
    }
    Ok(m)
}

fn apply(m: &Mat4, v: [f32; 3], w: f32) -> [f32; 3] {
    let a = &m.0;
    [
        a[0] * v[0] + a[1] * v[1] + a[2] * v[2] + a[3] * w,
        a[4] * v[0] + a[5] * v[1] + a[6] * v[2] + a[7] * w,
        a[8] * v[0] + a[9] * v[1] + a[10] * v[2] + a[11] * w,
    ]
}

/// Numeric param reader for 3D nodes.
pub struct Params<'a> {
    pub node: &'a Node,
    pub t: f64,
}

impl Params<'_> {
    pub fn vec<const N: usize>(&self, name: &str, default: [f32; N]) -> [f32; N] {
        let mut out = default;
        if let Some(v) = self.node.params.get(name).and_then(|p| p.eval(self.t)) {
            if v.len() == 1 {
                out = [v[0] as f32; N];
            } else {
                for (o, x) in out.iter_mut().zip(v) {
                    *o = x as f32;
                }
            }
        }
        out
    }

    pub fn num(&self, name: &str, default: f32) -> f32 {
        self.vec(name, [default])[0]
    }

    pub fn text(&self, name: &str) -> Option<&str> {
        text(self.node, name)
    }

    /// Linear straight-alpha color from a document color param.
    pub fn color(&self, name: &str, default: [f32; 4]) -> [f32; 4] {
        let c = match self.node.params.get(name) {
            None => return default,
            Some(Param::Text(s)) => match crate::ops::parse_hex(s) {
                Some(c) => c,
                None => return default,
            },
            Some(p) => {
                let v = p.eval(self.t).unwrap_or_default();
                match v.len() {
                    3 => [v[0], v[1], v[2], 1.0],
                    4 => [v[0], v[1], v[2], v[3]],
                    _ => return default,
                }
            }
        };
        let l = |x: f64| crate::ops::srgb_to_linear(x) as f32;
        [l(c[0]), l(c[1]), l(c[2]), c[3] as f32]
    }

    /// Object transform: T(position) * R(rotation) * S(scale).
    pub fn model(&self) -> Mat4 {
        let p = self.vec("position", [0.0, 0.0, 0.0]);
        let r = self.vec("rotation", [0.0, 0.0, 0.0]);
        let s = self.vec("scale", [1.0, 1.0, 1.0]);
        Mat4::translate(p[0], p[1], p[2])
            .mul(&Mat4::rotate_xyz(r))
            .mul(&Mat4::scale(s[0], s[1], s[2]))
    }
}

/// Mesh cache key and loader for a `mesh` node.
/// Params that only place or shade a mesh; everything else shapes it.
const NON_GEOMETRY: &[&str] = &[
    "position",
    "rotation",
    "scale",
    "color",
    "metallic",
    "roughness",
    "emissive",
    "opacity",
];

pub fn mesh_key(node: &Node) -> String {
    use std::hash::{Hash, Hasher};
    match (text(node, "src"), text(node, "primitive")) {
        (Some(src), _) => format!("file:{src}"),
        (None, Some("text" | "path")) => {
            let mut h = std::collections::hash_map::DefaultHasher::new();
            for (k, v) in &node.params {
                if !NON_GEOMETRY.contains(&k.as_str()) {
                    k.hash(&mut h);
                    serde_json::to_string(v).unwrap_or_default().hash(&mut h);
                }
            }
            format!("extruded:{:x}", h.finish())
        }
        (None, kind) => {
            format!(
                "prim:{}:{}",
                kind.unwrap_or("cube"),
                Params { node, t: 0.0 }.num("segments", 32.0) as usize
            )
        }
    }
}

pub fn build_mesh(id: &str, node: &Node, base: &Path, fonts: &mut crate::geom::Fonts) -> Result<MeshData, RenderError> {
    let bad = |param: &str, reason: String| RenderError::BadParam {
        node: id.to_string(),
        param: param.to_string(),
        reason,
    };
    let p = Params { node, t: 0.0 };
    match (text(node, "src"), text(node, "primitive").unwrap_or("cube")) {
        (Some(src), _) => {
            let path = if Path::new(src).is_absolute() {
                Path::new(src).to_path_buf()
            } else {
                base.join(src)
            };
            load_gltf(&path).map_err(|e| bad("src", e))
        }
        (None, "text") => {
            // Lay the text out at 100 px per world unit, centered, then extrude.
            let size = p.num("size", 1.0).max(1e-3) as f64;
            let mut layout = ultrafast_graph::Node {
                op: "text".into(),
                ..Default::default()
            };
            for k in [
                "text",
                "font",
                "weight",
                "style",
                "tracking",
                "line_height",
                "features",
                "width",
            ] {
                if let Some(v) = node.params.get(k) {
                    layout.params.insert(k.into(), v.clone());
                }
            }
            layout.params.insert("size".into(), Param::Number(100.0));
            layout.params.insert(
                "align".into(),
                node.params
                    .get("align")
                    .cloned()
                    .unwrap_or(Param::Text("center".into())),
            );
            layout.params.insert("valign".into(), Param::Text("middle".into()));
            let fmt = ultrafast_graph::Format {
                width: 1,
                height: 1,
                fps: 1.0,
                duration: 1.0,
                motion_blur: None,
            };
            let paths: Vec<kurbo::BezPath> =
                crate::geom::text_paths(id, &layout, &fmt, 0.0, crate::ops::Affine::IDENTITY, fonts, base)?
                    .0
                    .into_iter()
                    .map(|(p, _, _)| p)
                    .collect();
            extrude(
                &paths,
                100.0 / size,
                p.num("depth", (0.25 * size) as f32) as f64,
                p.num("bevel", 0.0) as f64,
            )
            .map_err(|e| bad("text", e))
        }
        (None, "path") => {
            let d = text(node, "d").ok_or_else(|| bad("d", "path mesh needs `d` (SVG path data)".into()))?;
            let path = kurbo::BezPath::from_svg(d).map_err(|e| bad("d", format!("invalid SVG path data: {e}")))?;
            extrude(
                &[path],
                p.num("units", 100.0) as f64,
                p.num("depth", 0.25) as f64,
                p.num("bevel", 0.0) as f64,
            )
            .map_err(|e| bad("d", e))
        }
        (None, kind) => {
            let seg = p.num("segments", 32.0) as usize;
            primitive(kind, seg).ok_or_else(|| {
                bad(
                    "primitive",
                    format!("unknown primitive `{kind}` (cube, plane, sphere, cylinder, cone, torus, text, path)"),
                )
            })
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn primitives_are_well_formed() {
        for k in ["cube", "plane", "sphere", "cylinder", "cone", "torus"] {
            let m = primitive(k, 16).unwrap();
            let n = (m.vertices.len() / 8) as u32;
            assert!(m.indices.len().is_multiple_of(3) && !m.indices.is_empty(), "{k}");
            assert!(m.indices.iter().all(|&i| i < n), "{k}");
        }
    }

    #[test]
    fn affine_inverse_roundtrips() {
        let m = Mat4::translate(1.0, 2.0, 3.0)
            .mul(&Mat4::rotate_xyz([10.0, 20.0, 30.0]))
            .mul(&Mat4::scale(2.0, 3.0, 4.0));
        let i = m.mul(&m.inverse_affine());
        for (a, b) in i.0.iter().zip(Mat4::IDENTITY.0.iter()) {
            assert!((a - b).abs() < 1e-5);
        }
    }

    #[test]
    fn look_at_puts_target_in_front() {
        let v = Mat4::look_at([0.0, 0.0, 5.0], [0.0, 0.0, 0.0], [0.0, 1.0, 0.0]);
        let p = apply(&v, [0.0, 0.0, 0.0], 1.0);
        assert!((p[2] + 5.0).abs() < 1e-5, "{p:?}");
    }
}

/// Extrudes 2D outlines (output-space px, y down) into a mesh: front and
/// back caps tessellated with the nonzero rule, side walls from the
/// flattened contours. `units` px make one world unit; the result is
/// centered on the origin, +y up, extruded `depth` along z.
/// Extrudes closed 2D paths (pixel units, y down) into a mesh centered on
/// the origin: `units` pixels per world unit, `depth` world units deep.
/// `bevel` (world units) chamfers both rims at 45 degrees. Sides are smooth
/// along curves and sharp at corners.
pub fn extrude(paths: &[kurbo::BezPath], units: f64, depth: f64, bevel: f64) -> Result<MeshData, String> {
    use lyon_tessellation::geometry_builder::{BuffersBuilder, VertexBuffers};
    use lyon_tessellation::path::Path as LPath;
    use lyon_tessellation::{FillOptions, FillRule, FillTessellator, FillVertex};

    // Bounds for centering.
    let mut b = [f64::MAX, f64::MAX, f64::MIN, f64::MIN];
    for p in paths {
        let r = kurbo::Shape::bounding_box(p);
        b = [b[0].min(r.x0), b[1].min(r.y0), b[2].max(r.x1), b[3].max(r.y1)];
    }
    if b[0] > b[2] {
        return Err("nothing to extrude".into());
    }
    let (cx, cy) = ((b[0] + b[2]) / 2.0, (b[1] + b[3]) / 2.0);
    let to_world = |x: f64, y: f64| -> [f32; 2] { [((x - cx) / units) as f32, (-(y - cy) / units) as f32] };

    // Contours, flattened by kurbo (exactly as the 2D renderer does).
    let mut contours: Vec<Vec<[f32; 2]>> = Vec::new();
    for p in paths {
        let mut cur: Vec<[f32; 2]> = Vec::new();
        kurbo::flatten(p.iter(), 0.25, |el| match el {
            kurbo::PathEl::MoveTo(q) => {
                if cur.len() > 2 {
                    contours.push(std::mem::take(&mut cur));
                }
                cur.clear();
                cur.push(to_world(q.x, q.y));
            }
            kurbo::PathEl::LineTo(q) => {
                let w = to_world(q.x, q.y);
                // Drop near-zero segments: their normals are noise, and a
                // noisy normal breaks the smooth shading of a curve.
                if cur
                    .last()
                    .is_none_or(|l| (l[0] - w[0]).abs() + (l[1] - w[1]).abs() > 2e-3)
                {
                    cur.push(w);
                }
            }
            kurbo::PathEl::ClosePath => {
                if cur.len() > 2 {
                    contours.push(std::mem::take(&mut cur));
                }
            }
            _ => {}
        });
        if cur.len() > 2 {
            contours.push(cur);
        }
    }
    for c in &mut contours {
        if c.len() > 1 && (c[0][0] - c[c.len() - 1][0]).abs() + (c[0][1] - c[c.len() - 1][1]).abs() < 2e-3 {
            c.pop();
        }
    }
    contours.retain(|c| c.len() > 2);

    // Which way is out of the solid, per contour: contour direction depends
    // on the font format and on holes, so probe the filled region (nonzero
    // winding over every contour) just off the longest segment.
    let winding = |p: [f32; 2]| -> i32 {
        let mut w = 0;
        for c in &contours {
            for i in 0..c.len() {
                let (a, q) = (c[i], c[(i + 1) % c.len()]);
                let side = (q[0] - a[0]) * (p[1] - a[1]) - (p[0] - a[0]) * (q[1] - a[1]);
                if a[1] <= p[1] {
                    if q[1] > p[1] && side > 0.0 {
                        w += 1;
                    }
                } else if q[1] <= p[1] && side < 0.0 {
                    w -= 1;
                }
            }
        }
        w
    };
    let raw_normal = |c: &[[f32; 2]], i: usize| -> [f32; 2] {
        let (a, q) = (c[i], c[(i + 1) % c.len()]);
        let (dx, dy) = (q[0] - a[0], q[1] - a[1]);
        let l = (dx * dx + dy * dy).sqrt().max(1e-9);
        [dy / l, -dx / l]
    };
    let flips: Vec<f32> = contours
        .iter()
        .map(|c| {
            let i = (0..c.len())
                .max_by(|&x, &y| {
                    let len = |i: usize| {
                        let (a, q) = (c[i], c[(i + 1) % c.len()]);
                        (q[0] - a[0]).hypot(q[1] - a[1])
                    };
                    len(x).partial_cmp(&len(y)).unwrap()
                })
                .unwrap();
            let (a, q) = (c[i], c[(i + 1) % c.len()]);
            let n = raw_normal(c, i);
            let probe = [(a[0] + q[0]) / 2.0 + n[0] * 1e-3, (a[1] + q[1]) / 2.0 + n[1] * 1e-3];
            if winding(probe) != 0 { -1.0 } else { 1.0 }
        })
        .collect();
    // Outward normal of segment i (vertex i to i+1) of contour ci.
    let seg_normal_of = |ci: usize, c: &[[f32; 2]], i: usize| -> [f32; 2] {
        let n = raw_normal(c, i);
        [n[0] * flips[ci], n[1] * flips[ci]]
    };
    const SMOOTH: f32 = 0.82; // cos 35 degrees
    let bevel = bevel.clamp(0.0, depth * 0.49) as f32;
    // The inset outline the caps sit on (miter offset, so it stays parallel).
    let inset: Vec<Vec<[f32; 2]>> = contours
        .iter()
        .enumerate()
        .map(|(ci, c)| {
            (0..c.len())
                .map(|i| {
                    if bevel <= 0.0 {
                        return c[i];
                    }
                    let (np, nn) = (
                        seg_normal_of(ci, c, (i + c.len() - 1) % c.len()),
                        seg_normal_of(ci, c, i),
                    );
                    let m = [np[0] + nn[0], np[1] + nn[1]];
                    let ml = (m[0] * m[0] + m[1] * m[1]).sqrt().max(1e-6);
                    let m = [m[0] / ml, m[1] / ml];
                    let len = bevel / (m[0] * nn[0] + m[1] * nn[1]).max(0.5);
                    [c[i][0] - m[0] * len, c[i][1] - m[1] * len]
                })
                .collect()
        })
        .collect();

    let mut lp = LPath::builder();
    for c in &inset {
        lp.begin(lyon_tessellation::math::point(c[0][0], c[0][1]));
        for v in &c[1..] {
            lp.line_to(lyon_tessellation::math::point(v[0], v[1]));
        }
        lp.end(true);
    }
    let path = lp.build();
    let mut buffers: VertexBuffers<[f32; 2], u32> = VertexBuffers::new();
    FillTessellator::new()
        .tessellate_path(
            &path,
            &FillOptions::tolerance(0.001).with_fill_rule(FillRule::NonZero),
            &mut BuffersBuilder::new(&mut buffers, |v: FillVertex| v.position().to_array()),
        )
        .map_err(|e| format!("tessellation failed: {e:?}"))?;

    let mut m = MeshData::default();
    let hz = (depth / 2.0) as f32;
    let uv = |p: [f32; 2]| {
        [
            ((p[0] as f64 * units + cx - b[0]) / (b[2] - b[0]).max(1e-9)) as f32,
            ((cy - p[1] as f64 * units - b[1]) / (b[3] - b[1]).max(1e-9)) as f32,
        ]
    };
    // Front (+z) and back (-z) caps.
    for (z, nz, flip) in [(hz, 1.0f32, false), (-hz, -1.0f32, true)] {
        let base = (m.vertices.len() / 8) as u32;
        for p in &buffers.vertices {
            m.vertex([p[0], p[1], z], [0.0, 0.0, nz], uv(*p));
        }
        for tri in buffers.indices.chunks_exact(3) {
            if flip {
                m.indices.extend([base + tri[0], base + tri[2], base + tri[1]]);
            } else {
                m.indices.extend([base + tri[0], base + tri[1], base + tri[2]]);
            }
        }
    }
    if depth <= 0.0 {
        return Ok(m);
    }
    // Walls: for each segment, the bevel ring at the front, the side, and
    // the bevel ring at the back. Vertex normals are averaged across a
    // vertex where the outline is smooth, so curves shade round.
    let side_z = hz - bevel;
    for (ci, c) in contours.iter().enumerate() {
        let n = c.len();
        let normals: Vec<[f32; 2]> = (0..n).map(|i| seg_normal_of(ci, c, i)).collect();
        let vertex_normal = |i: usize, seg: usize| -> [f32; 2] {
            let (np, nn) = (normals[(i + n - 1) % n], normals[i % n]);
            if np[0] * nn[0] + np[1] * nn[1] > SMOOTH {
                let s = [np[0] + nn[0], np[1] + nn[1]];
                let l = (s[0] * s[0] + s[1] * s[1]).sqrt().max(1e-9);
                [s[0] / l, s[1] / l]
            } else {
                normals[seg]
            }
        };
        for i in 0..n {
            let j = (i + 1) % n;
            let (na, nb) = (vertex_normal(i, i), vertex_normal(j, i));
            // Side wall.
            let v0 = m.vertex([c[i][0], c[i][1], side_z], [na[0], na[1], 0.0], [0.0, 0.0]);
            let v1 = m.vertex([c[j][0], c[j][1], side_z], [nb[0], nb[1], 0.0], [1.0, 0.0]);
            let v2 = m.vertex([c[j][0], c[j][1], -side_z], [nb[0], nb[1], 0.0], [1.0, 1.0]);
            let v3 = m.vertex([c[i][0], c[i][1], -side_z], [na[0], na[1], 0.0], [0.0, 1.0]);
            m.indices.extend([v0, v1, v2, v0, v2, v3]);
            if bevel > 0.0 {
                let q = &inset[ci];
                let k = std::f32::consts::FRAC_1_SQRT_2;
                for (zs, zo, sign) in [(side_z, hz, 1.0f32), (-side_z, -hz, -1.0f32)] {
                    let (ba, bb) = ([na[0] * k, na[1] * k, sign * k], [nb[0] * k, nb[1] * k, sign * k]);
                    let w0 = m.vertex([c[i][0], c[i][1], zs], ba, [0.0, 0.0]);
                    let w1 = m.vertex([c[j][0], c[j][1], zs], bb, [1.0, 0.0]);
                    let w2 = m.vertex([q[j][0], q[j][1], zo], bb, [1.0, 1.0]);
                    let w3 = m.vertex([q[i][0], q[i][1], zo], ba, [0.0, 1.0]);
                    if sign > 0.0 {
                        m.indices.extend([w0, w2, w1, w0, w3, w2]);
                    } else {
                        m.indices.extend([w0, w1, w2, w0, w2, w3]);
                    }
                }
            }
        }
    }
    Ok(m)
}

#[cfg(test)]
mod bevel_tests {
    use super::*;

    #[test]
    fn bevel_adds_rings_and_insets_caps() {
        let sq = kurbo::BezPath::from_svg("M0 0 H 100 V 100 H 0 Z").unwrap();
        let flat = extrude(std::slice::from_ref(&sq), 100.0, 0.5, 0.0).unwrap();
        let bev = extrude(&[sq], 100.0, 0.5, 0.1).unwrap();
        assert!(
            bev.vertices.len() > flat.vertices.len(),
            "{} vs {}",
            bev.vertices.len(),
            flat.vertices.len()
        );
        // The front cap is inset by the bevel: its x extent shrinks by 0.1 each side.
        let front: Vec<f32> = bev
            .vertices
            .chunks_exact(8)
            .filter(|v| v[5] > 0.99)
            .map(|v| v[0])
            .collect();
        let max = front.iter().cloned().fold(f32::MIN, f32::max);
        assert!((max - 0.4).abs() < 1e-3, "front cap reaches {max}");
    }
}

#[cfg(test)]
mod smooth_tests {
    use super::*;

    #[test]
    fn curved_sides_shade_smoothly() {
        use kurbo::Shape;
        let circle = kurbo::Circle::new((50.0, 50.0), 40.0).to_path(0.1);
        let m = extrude(&[circle], 100.0, 0.5, 0.0).unwrap();
        // Side vertices: normal z == 0. Group by position; normals at the same
        // position must agree.
        let mut by_pos: std::collections::HashMap<(i64, i64, i64), Vec<[f32; 3]>> = Default::default();
        for v in m.vertices.chunks_exact(8) {
            if v[5].abs() < 1e-3 {
                let k = ((v[0] * 1e4) as i64, (v[1] * 1e4) as i64, (v[2] * 1e4) as i64);
                by_pos.entry(k).or_default().push([v[3], v[4], v[5]]);
            }
        }
        let mut worst = 1.0f32;
        for ns in by_pos.values() {
            for a in ns {
                for b in ns {
                    worst = worst.min(a[0] * b[0] + a[1] * b[1] + a[2] * b[2]);
                }
            }
        }
        assert!(worst > 0.999, "normals at a shared vertex disagree: cos {worst}");
    }
}
