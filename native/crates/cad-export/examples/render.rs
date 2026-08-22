//! Rasterise a mesh to a PNG, so the result can be looked at.
//!
//! `cargo run --release -p cad-export --example render -- file.glb|file.obj out.png \
//!      [--size WxH] [--yaw DEG] [--pitch DEG] [--zoom F] [--at X,Y,Z] [--part NAME] \
//!      [--no-textures]`
//!
//! Every measurement in this project answers a question about the mesh; none of
//! them answers "does it look right". This does, offline and with no viewer in
//! the loop, so a render can be put beside a reference mesher's the same way the
//! numbers are.
//!
//! Flat shading on purpose: interpolated normals hide exactly the faceting the
//! renders are meant to show.

// Shared with the other tools here; each uses the part of it it needs.
#[allow(dead_code)]
#[path = "common/glb_read.rs"]
mod glb_read;

use std::io::Write;

type V3 = [f64; 3];

/// One triangle with the appearance its file gave it.
#[derive(Clone)]
struct Tri {
    at: [V3; 3],
    base: [f64; 3],
    metallic: f64,
    roughness: f64,
    /// Texture coordinates, already multiplied by the material's tile scale,
    /// so one unit is one repeat.
    uv: Option<[[f64; 2]; 3]>,
    /// The material's images, decoded once and shared.
    maps: Option<std::rc::Rc<Maps>>,
}

/// A material's decoded images. One per material, not per triangle.
struct Maps {
    colour: Option<Bitmap>,
    normal: Option<Bitmap>,
    normal_scale: f64,
}

struct Bitmap {
    width: usize,
    height: usize,
    /// RGB, 0..1, linear for a colour map and raw for a normal map.
    texels: Vec<[f64; 3]>,
}

impl Bitmap {
    /// Nearest-neighbour, wrapping. Nearest rather than bilinear on purpose:
    /// this is here to show whether the grain is present and the right size,
    /// and smoothing it would hide exactly that.
    fn at(&self, u: f64, v: f64) -> [f64; 3] {
        let wrap = |x: f64, n: usize| -> usize {
            let n = n.max(1);
            let i = (x * n as f64).floor() as i64 % n as i64;
            (if i < 0 { i + n as i64 } else { i }) as usize
        };
        self.texels[wrap(v, self.height) * self.width + wrap(u, self.width)]
    }
}

/// The default look for a file that carries no materials, and for `--grey`.
const PLAIN: ([f64; 3], f64, f64) = ([0.82, 0.84, 0.88], 0.0, 0.55);

/// A glTF image as a bitmap. `srgb` for a colour map, which glTF stores
/// gamma-encoded; a normal map is raw and must not be linearised.
fn bitmap(data: &gltf::image::Data, srgb: bool) -> Option<Bitmap> {
    use gltf::image::Format;
    let channels = match data.format {
        Format::R8G8B8 => 3,
        Format::R8G8B8A8 => 4,
        Format::R8 => 1,
        Format::R8G8 => 2,
        _ => return None,
    };
    let to_linear = |c: f64| {
        if !srgb {
            c
        } else if c <= 0.04045 {
            c / 12.92
        } else {
            ((c + 0.055) / 1.055).powf(2.4)
        }
    };
    let texels = data
        .pixels
        .chunks_exact(channels)
        .map(|p| {
            let g = |i: usize| to_linear(p[i.min(channels - 1)] as f64 / 255.0);
            if channels >= 3 { [g(0), g(1), g(2)] } else { [g(0); 3] }
        })
        .collect();
    Some(Bitmap { width: data.width as usize, height: data.height as usize, texels })
}

fn sub(a: V3, b: V3) -> V3 { [a[0] - b[0], a[1] - b[1], a[2] - b[2]] }
fn cross(a: V3, b: V3) -> V3 {
    [a[1] * b[2] - a[2] * b[1], a[2] * b[0] - a[0] * b[2], a[0] * b[1] - a[1] * b[0]]
}
fn dot(a: V3, b: V3) -> f64 { a[0] * b[0] + a[1] * b[1] + a[2] * b[2] }
fn norm(a: V3) -> V3 {
    let l = dot(a, a).sqrt();
    if l > 0.0 { [a[0] / l, a[1] / l, a[2] / l] } else { [0.0, 0.0, 1.0] }
}

struct Args {
    input: String,
    output: String,
    width: usize,
    height: usize,
    yaw: f64,
    pitch: f64,
    zoom: f64,
    at: Option<V3>,
    part: Option<String>,
    /// Ignore the file's materials and draw everything in one neutral grey.
    grey: bool,
    /// Keep the materials but drop their images, so the same mesh can be seen
    /// with and without its grain and relief and nothing else differs.
    no_textures: bool,
    /// True when the file calls Z up, as an OBJ from a CAD kernel usually
    /// does, so two writers can be framed the same way.
    z_up: bool,
}

fn parse_args() -> Option<Args> {
    let mut it = std::env::args().skip(1);
    let input = it.next()?;
    let output = it.next()?;
    let mut a = Args {
        input,
        output,
        width: 1400,
        height: 1000,
        yaw: 35.0,
        pitch: 22.0,
        zoom: 1.0,
        at: None,
        part: None,
        z_up: false,
        grey: false,
        no_textures: false,
    };
    while let Some(flag) = it.next() {
        if flag == "--no-textures" {
            a.no_textures = true;
            continue;
        }
        if flag == "--grey" || flag == "--gray" {
            a.grey = true;
            continue;
        }
        let value = it.next().unwrap_or_default();
        match flag.as_str() {
            "--size" => {
                let mut p = value.split(['x', 'X']).filter_map(|v| v.parse().ok());
                if let (Some(w), Some(h)) = (p.next(), p.next()) {
                    a.width = w;
                    a.height = h;
                }
            }
            "--yaw" => a.yaw = value.parse().unwrap_or(a.yaw),
            "--pitch" => a.pitch = value.parse().unwrap_or(a.pitch),
            "--zoom" => a.zoom = value.parse().unwrap_or(a.zoom),
            "--at" => {
                let c: Vec<f64> = value.split(',').filter_map(|v| v.trim().parse().ok()).collect();
                if c.len() == 3 {
                    a.at = Some([c[0], c[1], c[2]]);
                }
            }
            "--part" => a.part = Some(value),
            "--up" => a.z_up = value.eq_ignore_ascii_case("z"),
            _ => {}
        }
    }
    Some(a)
}

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let Some(args) = parse_args() else {
        eprintln!("usage: render <file.glb|file.obj> <out.png> [--size WxH] [--yaw D] \
                   [--pitch D] [--zoom F] [--at X,Y,Z] [--part NAME]");
        std::process::exit(2);
    };

    let mut tris = if args.input.to_lowercase().ends_with(".obj") {
        load_obj(&args.input, args.part.as_deref())?
    } else {
        load_glb(&args.input, args.part.as_deref())?
    };
    if tris.is_empty() {
        return Err("no triangles to draw".into());
    }
    if args.z_up {
        for t in &mut tris {
            for v in &mut t.at {
                *v = [v[0], v[2], -v[1]];
            }
        }
    }
    if args.no_textures {
        for t in &mut tris {
            t.uv = None;
            t.maps = None;
        }
    }
    if args.grey {
        for t in &mut tris {
            t.base = PLAIN.0;
            t.metallic = PLAIN.1;
            t.roughness = PLAIN.2;
        }
    }

    // Both writers put the model where they please and in whatever unit; the
    // camera is framed on what is actually there.
    let (mut lo, mut hi) = ([f64::MAX; 3], [f64::MIN; 3]);
    for t in &tris {
        for v in &t.at {
            for k in 0..3 {
                lo[k] = lo[k].min(v[k]);
                hi[k] = hi[k].max(v[k]);
            }
        }
    }
    eprintln!(
        "  what is drawn spans [{:.4}, {:.4}, {:.4}] .. [{:.4}, {:.4}, {:.4}]",
        lo[0], lo[1], lo[2], hi[0], hi[1], hi[2]
    );
    let centre = args
        .at
        .unwrap_or([(lo[0] + hi[0]) / 2.0, (lo[1] + hi[1]) / 2.0, (lo[2] + hi[2]) / 2.0]);
    let span = (0..3).map(|k| hi[k] - lo[k]).fold(0.0f64, f64::max).max(1e-9);

    // Recentre and scale into a unit-ish box so one camera works for any file.
    for t in &mut tris {
        for v in &mut t.at {
            for k in 0..3 {
                v[k] = (v[k] - centre[k]) / span;
            }
        }
    }

    let (sy, cy) = args.yaw.to_radians().sin_cos();
    let (sp, cp) = args.pitch.to_radians().sin_cos();
    let view = |p: V3| -> V3 {
        let x = p[0] * cy + p[2] * sy;
        let z = -p[0] * sy + p[2] * cy;
        let y = p[1] * cp - z * sp;
        let zz = p[1] * sp + z * cp;
        [x, y, zz]
    };
    // A light over the viewer's shoulder, and a dim fill from below so a face
    // turned away is shaded rather than black.
    let key = norm([0.4, 0.7, 0.6]);
    let fill = norm([-0.3, -0.5, 0.2]);

    let scale = args.zoom * 0.9 * args.width.min(args.height) as f64;
    let mut colour = vec![[24u8, 26u8, 30u8]; args.width * args.height];
    let mut depth = vec![f64::MAX; args.width * args.height];

    // The eye looks down −z in view space, so this is the direction to it.
    let eye = [0.0, 0.0, -1.0];
    for t in &tris {
        let p: Vec<V3> = t.at.iter().map(|v| view(*v)).collect();
        let mut n = norm(cross(sub(p[1], p[0]), sub(p[2], p[0])));
        // Flat shading, two-sided: a mesh is judged on its shape here, not on
        // whether this renderer agrees with it about which side is out.
        if dot(n, eye) < 0.0 {
            n = [-n[0], -n[1], -n[2]];
        }

        // A metal has no diffuse term and tints its highlight with its own
        // colour; a dielectric keeps a white highlight over a coloured body.
        // That distinction is the whole point of recovering metal-or-not from
        // the file, so it is drawn rather than averaged away.
        let spec_power = 2.0 / (t.roughness.clamp(0.03, 1.0).powi(4)) - 2.0;

        // Two punctual lights leave a metal black everywhere but its
        // highlight, because a metal has no diffuse term and there is nothing
        // for it to reflect. A real viewer supplies an environment; this one
        // supplies the cheapest honest stand-in — a sky that is bright above,
        // dim at the horizon and darker below — sampled along the reflected
        // direction, tinted by the metal's own colour. Without it the
        // difference between the steel and the paint, which is most of what
        // the material work recovered, does not appear at all.
        let sky = |d: V3| -> f64 {
            let up = d[1].clamp(-1.0, 1.0);
            if up >= 0.0 {
                0.30 + 0.70 * up
            } else {
                0.30 * (1.0 + up * 0.7)
            }
        };
        let encode = |v: f64| {
            // Reinhard, then sRGB: the highlights on a polished metal are far
            // brighter than white and clip to a flat disc without it.
            let m = (v / (1.0 + v)).clamp(0.0, 1.0);
            let s = if m <= 0.0031308 { m * 12.92 } else { 1.055 * m.powf(1.0 / 2.4) - 0.055 };
            (s * 255.0).round() as u8
        };

        // The whole shading, as a function of a normal and a base colour, so
        // that a textured triangle can run it per pixel and an untextured one
        // can run it once.
        let shade = |n: V3, base: [f64; 3]| -> [u8; 3] {
            let mut rgb = [0.0f64; 3];
            for (light, strength) in [(key, 1.0), (fill, 0.28)] {
                let lambert = dot(n, light).max(0.0) * strength;
                let half = norm([
                    light[0] + eye[0],
                    light[1] + eye[1],
                    light[2] + eye[2],
                ]);
                let spec = if lambert > 0.0 {
                    dot(n, half).max(0.0).powf(spec_power.clamp(1.0, 4096.0)) * strength
                } else {
                    0.0
                };
                for k in 0..3 {
                    let diffuse = base[k] * lambert * (1.0 - t.metallic);
                    let tint = 0.04 + (base[k] - 0.04) * t.metallic;
                    rgb[k] += diffuse + tint * spec * 1.6;
                }
            }
            let reflected = {
                let c = 2.0 * dot(n, eye);
                norm([n[0] * c - eye[0], n[1] * c - eye[1], n[2] * c - eye[2]])
            };
            // A rough metal gathers the sky from all around rather than
            // mirroring it, so its reflection is pulled toward the average.
            let gathered = sky(reflected) * (1.0 - t.roughness) + sky(n) * t.roughness;
            for k in 0..3 {
                // Metals reflect their own colour; a dielectric reflects white,
                // faintly, and lights its body from the sky instead.
                rgb[k] += t.metallic * base[k] * gathered * 1.25
                    + (1.0 - t.metallic) * (base[k] * sky(n) * 0.45 + gathered * 0.03);
            }
            [encode(rgb[0]), encode(rgb[1]), encode(rgb[2])]
        };

        // Sampled per pixel where there is anything to sample. It has to be:
        // the tessellation is adaptive, so a flat cast face is a handful of
        // very large triangles, and one sample each turns a 6.35 mm grain into
        // blotches the size of the triangles. The first version of this did
        // exactly that and the render looked like a fault in the mesh.
        let textured = t.uv.is_some() && t.maps.is_some();
        let tint = if textured { [0u8; 3] } else { shade(n, t.base) };



        let to_px = |q: V3| -> (f64, f64) {
            (
                args.width as f64 * 0.5 + q[0] * scale,
                args.height as f64 * 0.5 - q[1] * scale,
            )
        };
        let a = to_px(p[0]);
        let b = to_px(p[1]);
        let c = to_px(p[2]);

        let min_x = a.0.min(b.0).min(c.0).floor().max(0.0) as usize;
        let max_x = (a.0.max(b.0).max(c.0).ceil() as isize).clamp(0, args.width as isize) as usize;
        let min_y = a.1.min(b.1).min(c.1).floor().max(0.0) as usize;
        let max_y = (a.1.max(b.1).max(c.1).ceil() as isize).clamp(0, args.height as isize) as usize;

        let area = (b.0 - a.0) * (c.1 - a.1) - (b.1 - a.1) * (c.0 - a.0);
        if area.abs() < 1e-12 {
            continue;
        }
        for y in min_y..max_y {
            for x in min_x..max_x {
                let px = x as f64 + 0.5;
                let py = y as f64 + 0.5;
                let w0 = ((b.0 - a.0) * (py - a.1) - (b.1 - a.1) * (px - a.0)) / area;
                let w1 = ((c.0 - b.0) * (py - b.1) - (c.1 - b.1) * (px - b.0)) / area;
                let w2 = ((a.0 - c.0) * (py - c.1) - (a.1 - c.1) * (px - c.0)) / area;
                if w0 < 0.0 || w1 < 0.0 || w2 < 0.0 {
                    continue;
                }
                // Barycentric depth: w1 belongs to vertex a, w2 to b, w0 to c.
                let z = w1 * p[0][2] + w2 * p[1][2] + w0 * p[2][2];
                let i = y * args.width + x;
                if z < depth[i] {
                    depth[i] = z;
                    colour[i] = if textured {
                        let uv = t.uv.expect("checked above");
                        let maps = t.maps.as_ref().expect("checked above");
                        // w1 belongs to vertex a, w2 to b, w0 to c, the same
                        // way the depth above is interpolated.
                        let u = w1 * uv[0][0] + w2 * uv[1][0] + w0 * uv[2][0];
                        let v = w1 * uv[0][1] + w2 * uv[1][1] + w0 * uv[2][1];

                        let mut base = t.base;
                        if let Some(map) = &maps.colour {
                            let texel = map.at(u, v);
                            for k in 0..3 {
                                base[k] *= texel[k];
                            }
                        }
                        let mut shaded = n;
                        if let Some(map) = &maps.normal {
                            // Tangent space without tangents: a basis built
                            // from the face normal. The exporter's coordinates
                            // come from an axis-aligned projection, so this
                            // agrees with them up to a rotation within the
                            // plane — enough to show the relief, not enough to
                            // stand in for a viewer.
                            let texel = map.at(u, v);
                            let (tx, ty) = (
                                (texel[0] * 2.0 - 1.0) * maps.normal_scale,
                                (texel[1] * 2.0 - 1.0) * maps.normal_scale,
                            );
                            let up = if n[1].abs() < 0.9 {
                                [0.0, 1.0, 0.0]
                            } else {
                                [1.0, 0.0, 0.0]
                            };
                            let tangent = norm(cross(up, n));
                            let bitangent = cross(n, tangent);
                            shaded = norm([
                                n[0] + tangent[0] * tx + bitangent[0] * ty,
                                n[1] + tangent[1] * tx + bitangent[1] * ty,
                                n[2] + tangent[2] * tx + bitangent[2] * ty,
                            ]);
                        }
                        shade(shaded, base)
                    } else {
                        tint
                    };
                }
            }
        }
    }

    write_png(&args.output, args.width, args.height, &colour)?;
    println!(
        "{} -> {}  {} triangles  {}x{}",
        args.input,
        args.output,
        tris.len(),
        args.width,
        args.height
    );
    Ok(())
}

fn load_glb(path: &str, part: Option<&str>) -> Result<Vec<Tri>, Box<dyn std::error::Error>> {
    let bytes = std::fs::read(path)?;
    let (doc, buffers, images) = glb_read::open_with_images(&bytes)?;
    // One entry per material, built lazily below.
    let mut material_maps: Vec<Option<std::rc::Rc<Maps>>> = vec![None; doc.materials().len()];
    for (index, material) in doc.materials().enumerate() {
        let pbr = material.pbr_metallic_roughness();
        let colour = pbr
            .base_color_texture()
            .and_then(|i| images.get(i.texture().source().index()))
            .and_then(|d| bitmap(d, true));
        let normal = material
            .normal_texture()
            .and_then(|i| images.get(i.texture().source().index()))
            .and_then(|d| bitmap(d, false));
        if colour.is_some() || normal.is_some() {
            let scale = material.normal_texture().map_or(1.0, |i| i.scale() as f64);
            material_maps[index] = Some(std::rc::Rc::new(Maps {
                colour,
                normal,
                normal_scale: scale,
            }));
        }
    }
    let scales = texture_scales(&bytes);
    let mut out = Vec::new();
    let digits = |s: &str| s.chars().filter(|c| c.is_ascii_digit()).collect::<String>();
    let want = part.map(digits);

    #[allow(clippy::too_many_arguments)]
    fn walk(
        node: &gltf::Node,
        parent: [[f32; 4]; 4],
        buffers: &[gltf::buffer::Data],
        want: &Option<String>,
        maps: &[Option<std::rc::Rc<Maps>>],
        scales: &[[f64; 2]],
        out: &mut Vec<Tri>,
    ) {
        let m = mat_mul(parent, node.transform().matrix());
        if let Some(mesh) = node.mesh() {
            let keep = match want {
                None => true,
                Some(w) => mesh
                    .name()
                    .map(|n| n.chars().filter(|c| c.is_ascii_digit()).collect::<String>())
                    .is_some_and(|d| d.contains(w.as_str())),
            };
            if keep {
                for p in mesh.primitives() {
                    // Whatever the file says this primitive looks like. A glTF
                    // base colour is already linear, which is what the shading
                    // below wants.
                    let pbr = p.material().pbr_metallic_roughness();
                    let c = pbr.base_color_factor();
                    let base = [c[0] as f64, c[1] as f64, c[2] as f64];
                    let metallic = pbr.metallic_factor() as f64;
                    let roughness = pbr.roughness_factor() as f64;

                    // The tile scale lives in KHR_texture_transform, which the
                    // reader does not expand, so it is taken from the JSON
                    // that the material carries. Without it the grain is drawn
                    // once across the whole part and the render says nothing.
                    let tile = p
                        .material()
                        .index()
                        .and_then(|i| scales.get(i).copied())
                        .unwrap_or([1.0, 1.0]);
                    let material_maps = p.material().index().and_then(|i| maps[i].clone());

                    let r = p.reader(|b| Some(&buffers[b.index()]));
                    let pos: Vec<[f32; 3]> =
                        glb_read::positions(&p, &buffers);
                    let world: Vec<V3> = pos.iter().map(|v| apply(m, *v)).collect();
                    let uvs: Option<Vec<[f32; 2]>> = r
                        .read_tex_coords(0)
                        .map(|c| c.into_f32().collect());
                    if let Some(ix) = r.read_indices() {
                        for t in ix.into_u32().collect::<Vec<u32>>().chunks_exact(3) {
                            let uv = uvs.as_ref().map(|u| {
                                let g = |k: usize| {
                                    let c = u[t[k] as usize];
                                    [c[0] as f64 * tile[0], c[1] as f64 * tile[1]]
                                };
                                [g(0), g(1), g(2)]
                            });
                            out.push(Tri {
                                at: [
                                    world[t[0] as usize],
                                    world[t[1] as usize],
                                    world[t[2] as usize],
                                ],
                                base,
                                metallic,
                                roughness,
                                uv,
                                maps: material_maps.clone(),
                            });
                        }
                    }
                }
            }
        }
        for child in node.children() {
            walk(&child, m, buffers, want, maps, scales, out);
        }
    }
    for scene in doc.scenes() {
        for node in scene.nodes() {
            walk(&node, IDENTITY, &buffers, &want, &material_maps, &scales, &mut out);
        }
    }
    Ok(out)
}

/// The `KHR_texture_transform` scale of every material, by index.
///
/// Read from the GLB's own JSON chunk: the `gltf` crate gives no typed view of
/// the extension, and the scale is the whole point — the coordinates in the
/// file are millimetres of surface, and this is what turns them into repeats.
/// A material without one gets 1, which draws a single stretched repeat and
/// makes it obvious that something is missing.
fn texture_scales(bytes: &[u8]) -> Vec<[f64; 2]> {
    let mut out = Vec::new();
    let json = (|| -> Option<serde_json::Value> {
        let length = u32::from_le_bytes(bytes.get(12..16)?.try_into().ok()?) as usize;
        serde_json::from_slice(bytes.get(20..20 + length)?).ok()
    })();
    let Some(json) = json else { return out };
    for material in json["materials"].as_array().into_iter().flatten() {
        let scale = material["pbrMetallicRoughness"]["baseColorTexture"]["extensions"]
            ["KHR_texture_transform"]["scale"]
            .as_array()
            .or_else(|| {
                material["normalTexture"]["extensions"]["KHR_texture_transform"]["scale"]
                    .as_array()
            })
            .and_then(|s| Some([s.first()?.as_f64()?, s.get(1)?.as_f64()?]))
            .unwrap_or([1.0, 1.0]);
        out.push(scale);
    }
    out
}

const IDENTITY: [[f32; 4]; 4] = [
    [1.0, 0.0, 0.0, 0.0],
    [0.0, 1.0, 0.0, 0.0],
    [0.0, 0.0, 1.0, 0.0],
    [0.0, 0.0, 0.0, 1.0],
];

fn mat_mul(a: [[f32; 4]; 4], b: [[f32; 4]; 4]) -> [[f32; 4]; 4] {
    let mut out = [[0.0f32; 4]; 4];
    for c in 0..4 {
        for r in 0..4 {
            out[c][r] = (0..4).map(|k| a[k][r] * b[c][k]).sum();
        }
    }
    out
}

fn apply(m: [[f32; 4]; 4], v: [f32; 3]) -> V3 {
    let mut out = [0.0f64; 3];
    for r in 0..3 {
        out[r] = (m[0][r] * v[0] + m[1][r] * v[1] + m[2][r] * v[2] + m[3][r]) as f64;
    }
    out
}

fn load_obj(path: &str, part: Option<&str>) -> Result<Vec<Tri>, Box<dyn std::error::Error>> {
    let text = std::fs::read_to_string(path)?;
    let digits = |s: &str| s.chars().filter(|c| c.is_ascii_digit()).collect::<String>();
    let want = part.map(digits);
    let mut verts: Vec<V3> = Vec::new();
    let mut out = Vec::new();
    let mut keep = want.is_none();
    for line in text.lines() {
        let mut it = line.split_ascii_whitespace();
        match it.next() {
            Some("v") => {
                let c: Vec<f64> = it.take(3).filter_map(|s| s.parse().ok()).collect();
                if c.len() == 3 {
                    verts.push([c[0], c[1], c[2]]);
                }
            }
            Some("g") | Some("o") => {
                let name = it.collect::<Vec<_>>().join(" ");
                keep = match &want {
                    None => true,
                    Some(w) => digits(&name).contains(w.as_str()),
                };
            }
            Some("f") if keep => {
                let idx: Vec<usize> = it
                    .filter_map(|s| s.split('/').next().and_then(|n| n.parse::<i64>().ok()))
                    .map(|i| if i < 0 { (verts.len() as i64 + i) as usize } else { i as usize - 1 })
                    .collect();
                for k in 1..idx.len().saturating_sub(1) {
                    if idx[0] < verts.len() && idx[k] < verts.len() && idx[k + 1] < verts.len() {
                        // An OBJ from the comparison mesher carries no
                        // appearance worth reading, so it draws plain.
                        out.push(Tri {
                            at: [verts[idx[0]], verts[idx[k]], verts[idx[k + 1]]],
                            base: PLAIN.0,
                            metallic: PLAIN.1,
                            roughness: PLAIN.2,
                            uv: None,
                            maps: None,
                        });
                    }
                }
            }
            _ => {}
        }
    }
    Ok(out)
}

/// A PNG with no compression: stored deflate blocks and an adler checksum.
///
/// Writing the format by hand keeps this example free of an image dependency,
/// and the files are only looked at once.
fn write_png(
    path: &str,
    width: usize,
    height: usize,
    pixels: &[[u8; 3]],
) -> std::io::Result<()> {
    let mut raw: Vec<u8> = Vec::with_capacity(height * (1 + width * 3));
    for y in 0..height {
        raw.push(0); // filter: none
        for x in 0..width {
            raw.extend_from_slice(&pixels[y * width + x]);
        }
    }

    let mut z: Vec<u8> = vec![0x78, 0x01];
    for (i, chunk) in raw.chunks(65_535).enumerate() {
        let last = (i + 1) * 65_535 >= raw.len();
        z.push(if last { 1 } else { 0 });
        z.extend_from_slice(&(chunk.len() as u16).to_le_bytes());
        z.extend_from_slice(&(!(chunk.len() as u16)).to_le_bytes());
        z.extend_from_slice(chunk);
    }
    let (mut a, mut b) = (1u32, 0u32);
    for byte in &raw {
        a = (a + *byte as u32) % 65_521;
        b = (b + a) % 65_521;
    }
    z.extend_from_slice(&((b << 16) | a).to_be_bytes());

    let chunk = |kind: &[u8; 4], data: &[u8]| -> Vec<u8> {
        let mut out = (data.len() as u32).to_be_bytes().to_vec();
        out.extend_from_slice(kind);
        out.extend_from_slice(data);
        let mut crc = 0xffff_ffffu32;
        for byte in kind.iter().chain(data) {
            crc ^= *byte as u32;
            for _ in 0..8 {
                crc = if crc & 1 != 0 { (crc >> 1) ^ 0xedb8_8320 } else { crc >> 1 };
            }
        }
        out.extend_from_slice(&(!crc).to_be_bytes());
        out
    };

    let mut ihdr = (width as u32).to_be_bytes().to_vec();
    ihdr.extend_from_slice(&(height as u32).to_be_bytes());
    ihdr.extend_from_slice(&[8, 2, 0, 0, 0]); // 8-bit, truecolour

    let mut file = std::fs::File::create(path)?;
    file.write_all(&[0x89, b'P', b'N', b'G', 0x0d, 0x0a, 0x1a, 0x0a])?;
    file.write_all(&chunk(b"IHDR", &ihdr))?;
    file.write_all(&chunk(b"IDAT", &z))?;
    file.write_all(&chunk(b"IEND", &[]))?;
    Ok(())
}
