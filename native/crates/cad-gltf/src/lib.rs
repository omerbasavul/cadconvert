//! glTF 2.0 in: `.glb` and `.gltf` → the cad-ir scene.
//!
//! The other readers lower a boundary representation and hand it to the
//! tessellator. A glTF has no boundary representation to lower — it *is* the
//! mesh, already triangulated by whatever wrote it — so this reader fills
//! [`Geometry::mesh`] directly and leaves [`Geometry::brep`] empty, which the
//! tessellator reads as "nothing to do" and the writers read as "ready".
//!
//! What crosses: the node tree with its transforms, every triangle primitive,
//! the metallic-roughness materials with their colour and normal images, and
//! the two extensions this workspace's own writer emits (`KHR_mesh_quantization`
//! and `KHR_texture_transform`) plus the two that carry material facts
//! (`KHR_materials_ior`, `KHR_materials_transmission`). What does not cross is
//! named in the [`Report`] — a texture in a slot the scene has no word for, a
//! primitive that is lines rather than triangles — rather than dropped in
//! silence, and a file that *requires* something this cannot read is refused
//! by that thing's name.
//!
//! Units and axes: glTF is metres and Y up; the scene is millimetres and Z up.
//! The conversion is baked into the vertices and conjugated into every node
//! transform, so the mesh a writer sees is in the same space every other
//! reader produces and the tolerances downstream mean what they say.

#![forbid(unsafe_code)]

use std::path::{Path, PathBuf};

use cad_ir::image;
use cad_ir::material::ImageId;
use cad_ir::{Geometry, Material, MaterialId, Mesh, MeshPart, Meta, Node, NodeId, Scene, Transform};

/// Why a file could not be read.
#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("reading {path}: {source}")]
    Io {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },
    /// The file is not glTF, or is glTF that does not hold together.
    #[error("{0}")]
    Gltf(String),
    /// The file requires something this reader does not implement. The
    /// message names it, because "unsupported" alone sends the caller to read
    /// the file by hand.
    #[error("{0}")]
    Unsupported(String),
    /// Data that contradicts itself: an index past its buffer, an accessor of
    /// the wrong shape.
    #[error("{0}")]
    Malformed(String),
}

pub type Result<T> = std::result::Result<T, Error>;

/// What the reader could not carry across, in words.
///
/// A file that read and warned is not a failure; a caller who drops the
/// warning ships a part with its texture missing and no idea why.
#[derive(Debug, Default, Clone)]
pub struct Report {
    pub warnings: Vec<String>,
}

impl Report {
    /// Say something once. Ten primitives without normals are one fact.
    fn warn(&mut self, text: String) {
        if !self.warnings.contains(&text) {
            self.warnings.push(text);
        }
    }
}

/// Read a `.glb` or `.gltf` file into a scene.
///
/// A `.gltf` may name buffers and images beside it by relative URI; they are
/// read relative to the file's own directory.
pub fn scene_from_file(path: &Path) -> Result<(Scene, Report)> {
    let bytes = std::fs::read(path).map_err(|source| Error::Io {
        path: path.to_path_buf(),
        source,
    })?;
    scene_from_bytes(&bytes, path.parent(), &path.display().to_string())
}

/// Read glTF from memory.
///
/// `base` is where relative URIs resolve; `None` refuses them, since a file
/// that arrived as bytes has nothing beside it. `source` is recorded in
/// [`Meta::source`] and names the file in messages.
pub fn scene_from_bytes(bytes: &[u8], base: Option<&Path>, source: &str) -> Result<(Scene, Report)> {
    let gltf = open(bytes)?;
    let document = &gltf.document;
    refuse_unknown_required_extensions(document)?;

    let mut report = Report::default();
    let buffers = load_buffers(document, gltf.blob.as_deref(), base)?;

    let mut scene = Scene {
        meta: Meta {
            source: source.to_string(),
            ..Meta::default()
        },
        ..Scene::default()
    };

    let images = load_images(document, &buffers, base, &mut scene, &mut report)?;
    // Materials in the file's order, so a primitive's index is the scene's
    // index. A material nobody uses costs a few bytes in the output and
    // nothing else.
    for (i, m) in document.materials().enumerate() {
        let material = material_from(&m, i, &images, &mut report);
        scene.materials.push(material);
    }
    // Reserved for primitives that name no material; added only if one does.
    let mut default_material: Option<MaterialId> = None;

    for (i, m) in document.meshes().enumerate() {
        let name = m.name().map(str::to_string).unwrap_or_else(|| format!("mesh_{i}"));
        let mut mesh = Mesh::default();
        for p in m.primitives() {
            let material = match p.material().index() {
                Some(index) => MaterialId(index as u32),
                None => *default_material.get_or_insert_with(|| {
                    let id = MaterialId(scene.materials.len() as u32);
                    scene.materials.push(Material::unknown());
                    id
                }),
            };
            if let Some(part) = read_primitive(&p, &buffers, &name, material, &mut report)? {
                mesh.append(&part);
            }
        }
        scene.geometry.push(Geometry {
            name,
            brep: None,
            mesh: if mesh.is_empty() { None } else { Some(mesh) },
            material: None,
            face_materials: Vec::new(),
        });
    }

    // Nodes in the file's order too, for the same reason: children are
    // indices. The axis fix is conjugated into every node — see `conjugate`
    // — so a node's transform still composes with its parent's exactly as
    // the file said, only in the scene's space.
    for (i, n) in document.nodes().enumerate() {
        let node = Node {
            name: n.name().map(str::to_string).unwrap_or_else(|| format!("node_{i}")),
            transform: conjugate(&transform_of(&n)),
            children: n.children().map(|c| NodeId(c.index() as u32)).collect(),
            geometry: n.mesh().map(|m| cad_ir::GeometryId(m.index() as u32)),
            material: None,
        };
        scene.nodes.push(node);
    }
    scene.roots = match document.default_scene().or_else(|| document.scenes().next()) {
        Some(s) => s.nodes().map(|n| NodeId(n.index() as u32)).collect(),
        // No scene at all: every node that is nobody's child is a root.
        None => {
            let mut is_child = vec![false; scene.nodes.len()];
            for n in &scene.nodes {
                for c in &n.children {
                    if let Some(slot) = is_child.get_mut(c.index()) {
                        *slot = true;
                    }
                }
            }
            (0..scene.nodes.len())
                .filter(|&i| !is_child[i])
                .map(|i| NodeId(i as u32))
                .collect()
        }
    };

    if document.animations().len() > 0 {
        report.warn("animations are not carried; the scene is its rest pose".into());
    }
    if document.skins().len() > 0 {
        report.warn("skins are not carried; skinned meshes are read unposed".into());
    }

    Ok((scene, report))
}

/// Parse, validated where the validator can be trusted.
///
/// The `gltf` crate's validation refuses any file whose `extensionsRequired`
/// names an extension it does not know — and this workspace's own writer puts
/// `KHR_mesh_quantization` there, because a viewer that ignores quantisation
/// draws nonsense. So a file refused *only* for that is parsed again without
/// validation, and [`refuse_unknown_required_extensions`] then applies this
/// reader's own list. Any other validation failure stands: an index out of
/// bounds is a file that does not hold together.
fn open(bytes: &[u8]) -> Result<gltf::Gltf> {
    match gltf::Gltf::from_slice(bytes) {
        Ok(g) => Ok(g),
        Err(gltf::Error::Validation(errors))
            if errors
                .iter()
                .all(|(_, e)| matches!(e, gltf::json::validation::Error::Unsupported)) =>
        {
            gltf::Gltf::from_slice_without_validation(bytes).map_err(|e| Error::Gltf(e.to_string()))
        }
        Err(gltf::Error::Validation(errors)) => {
            let named: Vec<String> = errors
                .iter()
                .take(5)
                .map(|(path, e)| format!("{path}: {e}"))
                .collect();
            Err(Error::Gltf(format!(
                "the glTF does not hold together: {}",
                named.join("; ")
            )))
        }
        Err(e) => Err(Error::Gltf(e.to_string())),
    }
}

/// The extensions a file may *require* and still be read here.
const READABLE_REQUIRED: &[&str] = &[
    "KHR_mesh_quantization",
    "KHR_texture_transform",
    "KHR_materials_ior",
    "KHR_materials_transmission",
];

fn refuse_unknown_required_extensions(document: &gltf::Document) -> Result<()> {
    for ext in document.extensions_required() {
        if READABLE_REQUIRED.contains(&ext) {
            continue;
        }
        return Err(Error::Unsupported(match ext {
            "KHR_draco_mesh_compression" => "the mesh is Draco-compressed (KHR_draco_mesh_compression); \
                 export it without compression"
                .to_string(),
            "EXT_meshopt_compression" => "the mesh is meshopt-compressed (EXT_meshopt_compression); \
                 export it without compression"
                .to_string(),
            "KHR_texture_basisu" => {
                "the textures are KTX2/Basis (KHR_texture_basisu); export them as PNG or JPEG".to_string()
            }
            other => format!("the file requires {other}, which this reader does not implement"),
        }));
    }
    Ok(())
}

// ── Buffers and images ────────────────────────────────────────────────────

fn load_buffers(document: &gltf::Document, blob: Option<&[u8]>, base: Option<&Path>) -> Result<Vec<Vec<u8>>> {
    let mut out = Vec::with_capacity(document.buffers().len());
    for b in document.buffers() {
        let data = match b.source() {
            gltf::buffer::Source::Bin => blob
                .map(<[u8]>::to_vec)
                .ok_or_else(|| Error::Malformed(format!("buffer {} is the binary chunk, and there is none", b.index())))?,
            gltf::buffer::Source::Uri(uri) => read_uri(uri, base)?,
        };
        // A GLB pads its chunk to four bytes, so longer is fine. Shorter is a
        // buffer the file promised and did not deliver.
        if data.len() < b.length() {
            return Err(Error::Malformed(format!(
                "buffer {} holds {} bytes, {} promised",
                b.index(),
                data.len(),
                b.length()
            )));
        }
        out.push(data);
    }
    Ok(out)
}

/// Each image as an id in the scene, or `None` where it could not be read —
/// with the reason already in the report. A texture that names such an image
/// is dropped by the material that uses it.
fn load_images(
    document: &gltf::Document,
    buffers: &[Vec<u8>],
    base: Option<&Path>,
    scene: &mut Scene,
    report: &mut Report,
) -> Result<Vec<Option<ImageId>>> {
    let mut out = Vec::with_capacity(document.images().len());
    for img in document.images() {
        let name = img.name().map(str::to_string).unwrap_or_else(|| format!("image_{}", img.index()));
        let bytes: Vec<u8> = match img.source() {
            gltf::image::Source::View { view, .. } => {
                let data = &buffers[view.buffer().index()];
                let end = view.offset() + view.length();
                if end > data.len() {
                    return Err(Error::Malformed(format!("image {name} runs past its buffer")));
                }
                data[view.offset()..end].to_vec()
            }
            gltf::image::Source::Uri { uri, .. } => read_uri(uri, base)?,
        };
        match image::load(&name, &bytes) {
            Ok(image) => out.push(Some(scene.add_image(image))),
            Err(e) => {
                report.warn(format!("image {name} dropped: {e}"));
                out.push(None);
            }
        }
    }
    Ok(out)
}

fn read_uri(uri: &str, base: Option<&Path>) -> Result<Vec<u8>> {
    if let Some(rest) = uri.strip_prefix("data:") {
        let Some((_, payload)) = rest.split_once(";base64,") else {
            return Err(Error::Unsupported("a data URI that is not base64".into()));
        };
        return base64_decode(payload).ok_or_else(|| Error::Malformed("a data URI whose base64 does not decode".into()));
    }
    let Some(base) = base else {
        return Err(Error::Unsupported(format!(
            "the file refers to {uri} beside it, and was read from memory with nothing beside it"
        )));
    };
    let path = base.join(percent_decode(uri));
    std::fs::read(&path).map_err(|source| Error::Io { path, source })
}

/// `%20` and friends. glTF URIs are RFC 3986, so a space in a file name
/// arrives escaped.
fn percent_decode(uri: &str) -> String {
    let bytes = uri.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'%'
            && i + 2 < bytes.len()
            && let (Some(h), Some(l)) = (hex(bytes[i + 1]), hex(bytes[i + 2]))
        {
            out.push(h << 4 | l);
            i += 3;
        } else {
            out.push(bytes[i]);
            i += 1;
        }
    }
    String::from_utf8_lossy(&out).into_owned()
}

fn hex(b: u8) -> Option<u8> {
    match b {
        b'0'..=b'9' => Some(b - b'0'),
        b'a'..=b'f' => Some(b - b'a' + 10),
        b'A'..=b'F' => Some(b - b'A' + 10),
        _ => None,
    }
}

/// Standard alphabet, padding optional, whitespace ignored. Forty lines
/// against a dependency that would bring an image decoder with it.
fn base64_decode(text: &str) -> Option<Vec<u8>> {
    fn value(c: u8) -> Option<u32> {
        Some(match c {
            b'A'..=b'Z' => (c - b'A') as u32,
            b'a'..=b'z' => (c - b'a') as u32 + 26,
            b'0'..=b'9' => (c - b'0') as u32 + 52,
            b'+' | b'-' => 62,
            b'/' | b'_' => 63,
            _ => return None,
        })
    }
    let mut out = Vec::with_capacity(text.len() / 4 * 3);
    let mut acc: u32 = 0;
    let mut bits = 0;
    for &c in text.as_bytes() {
        if c == b'=' || c.is_ascii_whitespace() {
            continue;
        }
        acc = (acc << 6) | value(c)?;
        bits += 6;
        if bits >= 8 {
            bits -= 8;
            out.push((acc >> bits) as u8);
            acc &= (1 << bits) - 1;
        }
    }
    Some(out)
}

// ── Geometry ──────────────────────────────────────────────────────────────

/// One primitive as a mesh of its own, or `None` for one that is not triangles.
fn read_primitive(
    p: &gltf::Primitive,
    buffers: &[Vec<u8>],
    mesh_name: &str,
    material: MaterialId,
    report: &mut Report,
) -> Result<Option<Mesh>> {
    use gltf::mesh::Mode;
    let mode = p.mode();
    if !matches!(mode, Mode::Triangles | Mode::TriangleStrip | Mode::TriangleFan) {
        report.warn(format!(
            "{mesh_name}: a {mode:?} primitive is not a surface and was left out"
        ));
        return Ok(None);
    }
    if p.morph_targets().len() > 0 {
        report.warn(format!("{mesh_name}: morph targets are not carried; the base shape is read"));
    }
    if p.get(&gltf::Semantic::Colors(0)).is_some() {
        report.warn(format!(
            "{mesh_name}: per-vertex colours are not carried; the material's colour stands"
        ));
    }

    let positions = p
        .get(&gltf::Semantic::Positions)
        .ok_or_else(|| Error::Malformed(format!("{mesh_name}: a primitive without positions")))?;
    let positions = read_vec3(&positions, buffers, mesh_name, "positions")?;
    let normals = match p.get(&gltf::Semantic::Normals) {
        Some(a) => read_vec3(&a, buffers, mesh_name, "normals")?,
        None => Vec::new(),
    };
    let uvs = match p.get(&gltf::Semantic::TexCoords(0)) {
        Some(a) => read_vec2(&a, buffers, mesh_name)?,
        None => Vec::new(),
    };
    if !normals.is_empty() && normals.len() != positions.len() {
        return Err(Error::Malformed(format!(
            "{mesh_name}: {} normals for {} positions",
            normals.len(),
            positions.len()
        )));
    }
    if !uvs.is_empty() && uvs.len() != positions.len() {
        return Err(Error::Malformed(format!(
            "{mesh_name}: {} texture coordinates for {} positions",
            uvs.len(),
            positions.len()
        )));
    }

    let corners: Vec<u32> = match p.indices() {
        Some(a) => read_indices(&a, buffers, mesh_name)?,
        None => (0..positions.len() as u32).collect(),
    };
    if let Some(&bad) = corners.iter().find(|&&i| i as usize >= positions.len()) {
        return Err(Error::Malformed(format!(
            "{mesh_name}: index {bad} into {} vertices",
            positions.len()
        )));
    }
    let indices = match mode {
        Mode::Triangles => {
            let whole = corners.len() / 3 * 3;
            if whole != corners.len() {
                report.warn(format!(
                    "{mesh_name}: {} trailing indices do not make a triangle",
                    corners.len() - whole
                ));
            }
            corners[..whole].to_vec()
        }
        Mode::TriangleStrip => strip_to_list(&corners),
        Mode::TriangleFan => fan_to_list(&corners),
        _ => unreachable!("filtered above"),
    };
    if indices.is_empty() {
        return Ok(None);
    }

    let mut mesh = Mesh {
        positions: positions.iter().map(|&p| bake_point(p)).collect(),
        normals: normals.iter().map(|&n| bake_direction(n)).collect(),
        uvs,
        parts: vec![MeshPart {
            material: material.0,
            start: 0,
            count: indices.len() as u32,
        }],
        indices,
    };
    if mesh.normals.is_empty() {
        // Computed rather than left empty: a writer that finds no normals
        // emits none, and a viewer then shades the part flat and wrong.
        mesh.recompute_normals();
        report.warn(format!("{mesh_name}: normals were not in the file and were computed"));
    }
    Ok(Some(mesh))
}

/// A strip alternates winding from one triangle to the next, and the list
/// must not: the writers promise counter-clockwise seen from outside.
fn strip_to_list(corners: &[u32]) -> Vec<u32> {
    let mut out = Vec::with_capacity(corners.len().saturating_sub(2) * 3);
    for (i, w) in corners.windows(3).enumerate() {
        if i % 2 == 0 {
            out.extend_from_slice(&[w[0], w[1], w[2]]);
        } else {
            out.extend_from_slice(&[w[1], w[0], w[2]]);
        }
    }
    out
}

fn fan_to_list(corners: &[u32]) -> Vec<u32> {
    let mut out = Vec::with_capacity(corners.len().saturating_sub(2) * 3);
    if let Some(&hub) = corners.first() {
        for w in corners[1..].windows(2) {
            out.extend_from_slice(&[hub, w[0], w[1]]);
        }
    }
    out
}

// ── Accessors ─────────────────────────────────────────────────────────────

fn read_vec3(a: &gltf::Accessor, buffers: &[Vec<u8>], mesh: &str, what: &str) -> Result<Vec<[f32; 3]>> {
    if a.dimensions() != gltf::accessor::Dimensions::Vec3 {
        return Err(Error::Malformed(format!("{mesh}: {what} are {:?}, not VEC3", a.dimensions())));
    }
    let flat = read_floats(a, buffers, 3, mesh, what)?;
    Ok(flat.chunks_exact(3).map(|c| [c[0], c[1], c[2]]).collect())
}

fn read_vec2(a: &gltf::Accessor, buffers: &[Vec<u8>], mesh: &str) -> Result<Vec<[f32; 2]>> {
    if a.dimensions() != gltf::accessor::Dimensions::Vec2 {
        return Err(Error::Malformed(format!(
            "{mesh}: texture coordinates are {:?}, not VEC2",
            a.dimensions()
        )));
    }
    let flat = read_floats(a, buffers, 2, mesh, "texture coordinates")?;
    Ok(flat.chunks_exact(2).map(|c| [c[0], c[1]]).collect())
}

fn read_indices(a: &gltf::Accessor, buffers: &[Vec<u8>], mesh: &str) -> Result<Vec<u32>> {
    use gltf::accessor::DataType;
    if a.dimensions() != gltf::accessor::Dimensions::Scalar {
        return Err(Error::Malformed(format!("{mesh}: indices are {:?}, not SCALAR", a.dimensions())));
    }
    let width = match a.data_type() {
        DataType::U8 => 1,
        DataType::U16 => 2,
        DataType::U32 => 4,
        other => return Err(Error::Malformed(format!("{mesh}: indices stored as {other:?}"))),
    };
    let data = raw(a, buffers, width, mesh, "indices")?;
    let mut out = Vec::with_capacity(a.count());
    for i in 0..a.count() {
        let o = data.base + i * data.stride;
        let bytes = &data.bytes[o..o + width];
        out.push(match width {
            1 => bytes[0] as u32,
            2 => u16::from_le_bytes([bytes[0], bytes[1]]) as u32,
            _ => u32::from_le_bytes([bytes[0], bytes[1], bytes[2], bytes[3]]),
        });
    }
    Ok(out)
}

/// An accessor of any component type as floats, `dims` per element.
///
/// The integer types are what `KHR_mesh_quantization` stores: a normalised
/// integer stands for a fraction of its own full scale (a byte normal), an
/// unnormalised one for the number itself (a 16-bit position on a grid whose
/// scale sits on the node above, which every transform here applies).
fn read_floats(a: &gltf::Accessor, buffers: &[Vec<u8>], dims: usize, mesh: &str, what: &str) -> Result<Vec<f32>> {
    use gltf::accessor::DataType;
    let width = match a.data_type() {
        DataType::I8 | DataType::U8 => 1,
        DataType::I16 | DataType::U16 => 2,
        DataType::U32 | DataType::F32 => 4,
    };
    let normalised = a.normalized();
    let data = raw(a, buffers, width * dims, mesh, what)?;
    let mut out = Vec::with_capacity(a.count() * dims);
    for i in 0..a.count() {
        let at = data.base + i * data.stride;
        for k in 0..dims {
            let o = at + k * width;
            let b = &data.bytes[o..o + width];
            out.push(match a.data_type() {
                DataType::I8 => {
                    let x = b[0] as i8 as f32;
                    if normalised { (x / 127.0).max(-1.0) } else { x }
                }
                DataType::U8 => {
                    let x = b[0] as f32;
                    if normalised { x / 255.0 } else { x }
                }
                DataType::I16 => {
                    let x = i16::from_le_bytes([b[0], b[1]]) as f32;
                    if normalised { (x / 32767.0).max(-1.0) } else { x }
                }
                DataType::U16 => {
                    let x = u16::from_le_bytes([b[0], b[1]]) as f32;
                    if normalised { x / 65535.0 } else { x }
                }
                DataType::U32 => u32::from_le_bytes([b[0], b[1], b[2], b[3]]) as f32,
                DataType::F32 => f32::from_le_bytes([b[0], b[1], b[2], b[3]]),
            });
        }
    }
    Ok(out)
}

/// Where an accessor's bytes are, checked once so the loops above can index
/// without looking.
struct Raw<'a> {
    bytes: &'a [u8],
    base: usize,
    stride: usize,
}

fn raw<'a>(a: &gltf::Accessor, buffers: &'a [Vec<u8>], element: usize, mesh: &str, what: &str) -> Result<Raw<'a>> {
    if a.sparse().is_some() {
        return Err(Error::Unsupported(format!(
            "{mesh}: {what} use a sparse accessor, which this reader does not implement"
        )));
    }
    let Some(view) = a.view() else {
        // Legal glTF: an accessor with no view is all zeros. Nothing to do
        // with it here that is not a guess.
        return Err(Error::Unsupported(format!("{mesh}: {what} have no buffer view")));
    };
    let bytes = buffers
        .get(view.buffer().index())
        .ok_or_else(|| Error::Malformed(format!("{mesh}: {what} name a buffer the file does not have")))?;
    let base = view.offset() + a.offset();
    let stride = view.stride().unwrap_or(element);
    if stride < element {
        return Err(Error::Malformed(format!("{mesh}: {what} have a stride smaller than an element")));
    }
    let end = if a.count() == 0 { base } else { base + (a.count() - 1) * stride + element };
    let limit = (view.offset() + view.length()).min(bytes.len());
    if end > limit {
        return Err(Error::Malformed(format!(
            "{mesh}: {what} run past their buffer ({end} of {limit} bytes)"
        )));
    }
    Ok(Raw { bytes, base, stride })
}

// ── Materials ─────────────────────────────────────────────────────────────

fn material_from(m: &gltf::Material, index: usize, images: &[Option<ImageId>], report: &mut Report) -> Material {
    let name = m.name().map(str::to_string).unwrap_or_else(|| format!("material_{index}"));
    let pbr = m.pbr_metallic_roughness();
    let c = pbr.base_color_factor();
    // glTF's factors are already linear, which is the one thing about colour
    // the two formats agree on.
    let mut out = Material::from_colour(name.clone(), [c[0], c[1], c[2]], c[3]);
    out.metallic = pbr.metallic_factor();
    out.roughness = pbr.roughness_factor();
    out.emissive = m.emissive_factor();
    out.double_sided = m.double_sided();
    if let Some(ior) = m.ior() {
        out.ior = ior;
    }
    if let Some(t) = m.transmission() {
        out.transmission = t.transmission_factor();
    }
    match m.alpha_mode() {
        gltf::material::AlphaMode::Opaque => out.alpha = 1.0,
        gltf::material::AlphaMode::Blend => {}
        gltf::material::AlphaMode::Mask => {
            report.warn(format!(
                "{name}: alpha mask is not carried; the cut-out reads as a blend"
            ));
        }
    }

    if let Some(info) = pbr.base_color_texture() {
        out.textures.base_colour = image_for(&name, "colour", &info, images, report);
        if let Some(tt) = info.texture_transform() {
            apply_texture_transform(&name, &tt, &mut out, report);
        }
    }
    if let Some(n) = m.normal_texture() {
        if n.tex_coord() != 0 {
            report.warn(format!("{name}: the normal map uses texture set {}, and only set 0 is read", n.tex_coord()));
        } else if let Some(id) = images.get(n.texture().source().index()).copied().flatten() {
            out.textures.normal = Some(id);
            out.textures.set_normal_scale(n.scale());
        }
    }
    if pbr.metallic_roughness_texture().is_some() {
        report.warn(format!(
            "{name}: the metallic-roughness texture is not carried; the factors stand"
        ));
    }
    if m.occlusion_texture().is_some() {
        report.warn(format!("{name}: the occlusion texture is not carried"));
    }
    if m.emissive_texture().is_some() {
        report.warn(format!("{name}: the emissive texture is not carried; the factor stands"));
    }
    out
}

fn image_for(
    material: &str,
    slot: &str,
    info: &gltf::texture::Info,
    images: &[Option<ImageId>],
    report: &mut Report,
) -> Option<ImageId> {
    if info.tex_coord() != 0 {
        report.warn(format!(
            "{material}: the {slot} texture uses texture set {}, and only set 0 is read",
            info.tex_coord()
        ));
        return None;
    }
    images.get(info.texture().source().index()).copied().flatten()
}

/// `KHR_texture_transform` as this workspace's writer emits it: a scale of
/// `1/tile` over coordinates in millimetres. Read back, the tile is `1/scale`,
/// and a file from elsewhere with plain 0..1 coordinates and a tiling scale
/// gets the same treatment — the writer emits the scale it was given either
/// way. An offset or a rotation has no home in the scene and is named.
fn apply_texture_transform(material: &str, tt: &gltf::texture::TextureTransform, out: &mut Material, report: &mut Report) {
    let [sx, sy] = tt.scale();
    if tt.offset() != [0.0, 0.0] || tt.rotation() != 0.0 {
        report.warn(format!(
            "{material}: the texture's offset or rotation is not carried; its scale is"
        ));
    }
    if sx.is_finite() && sy.is_finite() && sx != 0.0 && sy != 0.0 && (sx, sy) != (1.0, 1.0) {
        out.textures.set_tile_mm(Some([1.0 / sx, 1.0 / sy]));
    }
}

// ── Space ─────────────────────────────────────────────────────────────────

/// Metres to millimetres, Y up to Z up.
///
/// The inverse of what the glTF writer does on the way out: it maps a scene
/// point `(x, y, z)` to `(x, z, −y) / 1000`, so a file point `(X, Y, Z)` is
/// `(X, −Z, Y) × 1000`.
const SCALE: f64 = 1000.0;
const FIX: Transform = Transform {
    m: [
        [SCALE, 0.0, 0.0, 0.0],
        [0.0, 0.0, -SCALE, 0.0],
        [0.0, SCALE, 0.0, 0.0],
    ],
};
const FIX_INVERSE: Transform = Transform {
    m: [
        [1.0 / SCALE, 0.0, 0.0, 0.0],
        [0.0, 0.0, 1.0 / SCALE, 0.0],
        [0.0, -1.0 / SCALE, 0.0, 0.0],
    ],
};

fn bake_point(p: [f32; 3]) -> [f32; 3] {
    let q = FIX.point(cad_ir::Vec3::new(p[0] as f64, p[1] as f64, p[2] as f64));
    [q.x as f32, q.y as f32, q.z as f32]
}

/// A direction turns with the axes and does not scale.
fn bake_direction(n: [f32; 3]) -> [f32; 3] {
    [n[0], -n[2], n[1]]
}

/// A node transform in the scene's space.
///
/// With the vertices baked, a node's transform `T` must act on baked points
/// as `T` acted on the file's: `FIX ∘ T ∘ FIX⁻¹`. Conjugating every node
/// rather than fixing only the roots keeps each node's transform meaningful
/// on its own — a caller reading a node's placement gets millimetres.
fn conjugate(t: &Transform) -> Transform {
    FIX_INVERSE.then(t).then(&FIX)
}

/// The file's column-major 4×4 as the scene's row-major 3×4.
fn transform_of(n: &gltf::Node) -> Transform {
    let c = n.transform().matrix();
    let mut m = [[0.0f64; 4]; 3];
    for (r, row) in m.iter_mut().enumerate() {
        for (col, cell) in row.iter_mut().enumerate() {
            *cell = c[col][r] as f64;
        }
    }
    Transform { m }
}

#[cfg(test)]
mod tests {
    use super::*;
    use cad_ir::Vec3;

    // ── helpers ──────────────────────────────────────────────────────────

    fn base64_encode(bytes: &[u8]) -> String {
        const A: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
        let mut out = String::new();
        for chunk in bytes.chunks(3) {
            let b = [chunk[0], *chunk.get(1).unwrap_or(&0), *chunk.get(2).unwrap_or(&0)];
            let n = (b[0] as u32) << 16 | (b[1] as u32) << 8 | b[2] as u32;
            out.push(A[(n >> 18) as usize & 63] as char);
            out.push(A[(n >> 12) as usize & 63] as char);
            out.push(if chunk.len() > 1 { A[(n >> 6) as usize & 63] as char } else { '=' });
            out.push(if chunk.len() > 2 { A[n as usize & 63] as char } else { '=' });
        }
        out
    }

    /// A `.gltf` holding one mesh with the given positions and (optionally)
    /// indices, in one primitive of the given mode, with one material.
    fn tiny_gltf(positions: &[[f32; 3]], indices: Option<&[u16]>, mode: u32, extra_material: &str) -> String {
        let mut bin: Vec<u8> = Vec::new();
        for p in positions {
            for v in p {
                bin.extend_from_slice(&v.to_le_bytes());
            }
        }
        let pos_len = bin.len();
        let mut index_json = String::new();
        let mut index_accessor = String::new();
        if let Some(ix) = indices {
            for i in ix {
                bin.extend_from_slice(&i.to_le_bytes());
            }
            while bin.len() % 4 != 0 {
                bin.push(0);
            }
            index_json = format!(
                r#",{{"buffer":0,"byteOffset":{pos_len},"byteLength":{}}}"#,
                ix.len() * 2
            );
            index_accessor = format!(
                r#",{{"bufferView":1,"componentType":5123,"count":{},"type":"SCALAR"}}"#,
                ix.len()
            );
        }
        let (min, max) = positions.iter().fold(([f32::MAX; 3], [f32::MIN; 3]), |(lo, hi), p| {
            (
                [lo[0].min(p[0]), lo[1].min(p[1]), lo[2].min(p[2])],
                [hi[0].max(p[0]), hi[1].max(p[1]), hi[2].max(p[2])],
            )
        });
        let indices_ref = if indices.is_some() { r#","indices":1"# } else { "" };
        format!(
            r#"{{"asset":{{"version":"2.0"}},"scene":0,"scenes":[{{"nodes":[0]}}],
"nodes":[{{"mesh":0,"name":"part","translation":[1,2,3]}}],
"meshes":[{{"name":"tri","primitives":[{{"attributes":{{"POSITION":0}}{indices_ref},"mode":{mode},"material":0}}]}}],
"materials":[{{"name":"paint","pbrMetallicRoughness":{{"baseColorFactor":[0.2,0.4,0.6,1.0],"metallicFactor":1.0,"roughnessFactor":0.3}},"doubleSided":true{extra_material}}}],
"accessors":[{{"bufferView":0,"componentType":5126,"count":{},"type":"VEC3","min":[{},{},{}],"max":[{},{},{}]}}{index_accessor}],
"bufferViews":[{{"buffer":0,"byteOffset":0,"byteLength":{pos_len}}}{index_json}],
"buffers":[{{"byteLength":{},"uri":"data:application/octet-stream;base64,{}"}}]}}"#,
            positions.len(),
            min[0],
            min[1],
            min[2],
            max[0],
            max[1],
            max[2],
            bin.len(),
            base64_encode(&bin)
        )
    }

    fn read(json: &str) -> (Scene, Report) {
        scene_from_bytes(json.as_bytes(), None, "test.gltf").expect("reads")
    }

    fn sample_scene() -> Scene {
        let path = Path::new(env!("CARGO_MANIFEST_DIR")).join("../cad-convert/tests/samples/small.x_t");
        let opts = cad_xt::LowerOptions {
            materials: cad_ir::MaterialResolver::default(),
        };
        let (mut scene, _) = cad_xt::scene_from_file(&path, &opts).expect("the sample reads");
        cad_tess::tessellate_scene(&mut scene, &cad_tess::Options::default());
        scene
    }

    fn world_bounds(scene: &Scene) -> cad_ir::Aabb {
        let mut b = cad_ir::Aabb::EMPTY;
        for i in scene.instances() {
            if let Some(mesh) = &scene.geometry[i.geometry.index()].mesh {
                for p in &mesh.positions {
                    b.add_point(i.transform.point(Vec3::new(p[0] as f64, p[1] as f64, p[2] as f64)));
                }
            }
        }
        b
    }

    // ── the crossing ─────────────────────────────────────────────────────

    #[test]
    fn a_glb_this_workspace_wrote_reads_back_to_the_same_mesh() {
        let original = sample_scene();
        let glb = cad_export::glb::write_bytes(&original, &cad_export::Options::default()).unwrap();
        let (back, report) = scene_from_bytes(&glb, None, "small.glb").unwrap();

        assert_eq!(back.stored_triangle_count(), original.stored_triangle_count());
        assert!(report.warnings.is_empty(), "{:?}", report.warnings);

        // The same millimetres, in the same axes: the writer's conversion and
        // the reader's undo each other to float precision.
        let (a, b) = (world_bounds(&original), world_bounds(&back));
        for (x, y) in [(a.min, b.min), (a.max, b.max)] {
            assert!((x.x - y.x).abs() < 1e-3 && (x.y - y.y).abs() < 1e-3 && (x.z - y.z).abs() < 1e-3, "{x:?} vs {y:?}");
        }
        let first = &original.materials[0];
        let same = back.materials.iter().any(|m| {
            (m.base_color[0] - first.base_color[0]).abs() < 1e-3
                && (m.metallic - first.metallic).abs() < 1e-3
                && (m.roughness - first.roughness).abs() < 1e-3
        });
        assert!(same, "the first material's colour and finish survive");
    }

    #[test]
    fn a_compact_glb_reads_back_within_its_own_grid() {
        let original = sample_scene();
        let glb = cad_export::glb::write_bytes(&original, &cad_export::Options::compact()).unwrap();
        let (back, _) = scene_from_bytes(&glb, None, "compact.glb").unwrap();
        assert_eq!(back.stored_triangle_count(), original.stored_triangle_count());
        // Sixteen bits over each mesh's own box: the grid step is the box over
        // 65535, and a corner can sit half a step off either way.
        let (a, b) = (world_bounds(&original), world_bounds(&back));
        let step = a.diagonal() / 65535.0;
        assert!((a.min - b.min).length() <= step && (a.max - b.max).length() <= step);
    }

    #[test]
    fn a_draco_file_is_refused_by_name() {
        let json = r#"{"asset":{"version":"2.0"},"extensionsRequired":["KHR_draco_mesh_compression"],"extensionsUsed":["KHR_draco_mesh_compression"]}"#;
        let err = scene_from_bytes(json.as_bytes(), None, "draco.gltf").err().expect("refused");
        assert!(matches!(err, Error::Unsupported(_)), "{err}");
        assert!(err.to_string().contains("Draco"), "{err}");
    }

    #[test]
    fn metres_become_millimetres_and_y_up_becomes_z_up() {
        // A triangle in the file's XY plane, one metre on a side, and a node
        // one, two and three metres away.
        let json = tiny_gltf(&[[0.0, 0.0, 0.0], [1.0, 0.0, 0.0], [0.0, 1.0, 0.0]], None, 4, "");
        let (scene, _) = read(&json);
        let mesh = scene.geometry[0].mesh.as_ref().unwrap();
        // The file's Y is the scene's Z.
        assert_eq!(mesh.positions[2], [0.0, 0.0, 1000.0]);
        // The node's translation turns with the axes and scales with the unit.
        let i = scene.instances();
        assert_eq!(i.len(), 1);
        let origin = i[0].transform.point(Vec3::new(0.0, 0.0, 0.0));
        assert!((origin - Vec3::new(1000.0, -3000.0, 2000.0)).length() < 1e-9, "{origin:?}");
        // And a triangle facing the file's +Z faces the scene's −Y: the normal
        // was computed after baking, on baked points.
        let n = mesh.normals[0];
        assert!((n[1] + 1.0).abs() < 1e-6, "{n:?}");
    }

    #[test]
    fn strips_and_fans_become_lists_that_all_wind_the_same_way() {
        let quad = [[0.0, 0.0, 0.0], [1.0, 0.0, 0.0], [0.0, 1.0, 0.0], [1.0, 1.0, 0.0]];
        for mode in [5u32, 6u32] {
            let order: [u16; 4] = if mode == 5 { [0, 1, 2, 3] } else { [0, 1, 3, 2] };
            let json = tiny_gltf(&quad, Some(&order), mode, "");
            let (scene, _) = read(&json);
            let mesh = scene.geometry[0].mesh.as_ref().unwrap();
            assert_eq!(mesh.triangle_count(), 2, "mode {mode}");
            // Both triangles face the same way when their normals agree.
            let normal = |t: usize| {
                let p = |i: usize| {
                    let v = mesh.positions[mesh.indices[t * 3 + i] as usize];
                    Vec3::new(v[0] as f64, v[1] as f64, v[2] as f64)
                };
                (p(1) - p(0)).cross(p(2) - p(0)).normalized_or(Vec3::new(0.0, 0.0, 1.0))
            };
            assert!(normal(0).dot(normal(1)) > 0.99, "mode {mode} winds inconsistently");
        }
    }

    #[test]
    fn a_primitive_without_normals_gets_them_computed_and_says_so() {
        let json = tiny_gltf(&[[0.0, 0.0, 0.0], [1.0, 0.0, 0.0], [0.0, 1.0, 0.0]], None, 4, "");
        let (scene, report) = read(&json);
        let mesh = scene.geometry[0].mesh.as_ref().unwrap();
        assert_eq!(mesh.normals.len(), mesh.positions.len());
        assert!(report.warnings.iter().any(|w| w.contains("normals")), "{:?}", report.warnings);
    }

    #[test]
    fn a_material_keeps_its_name_colour_finish_and_extensions() {
        let ext = r#","extensions":{"KHR_materials_ior":{"ior":1.33},"KHR_materials_transmission":{"transmissionFactor":0.75}}"#;
        let json = tiny_gltf(&[[0.0, 0.0, 0.0], [1.0, 0.0, 0.0], [0.0, 1.0, 0.0]], None, 4, ext)
            .replace(r#""asset":{"version":"2.0"}"#, r#""asset":{"version":"2.0"},"extensionsUsed":["KHR_materials_ior","KHR_materials_transmission"]"#);
        let (scene, _) = read(&json);
        let m = &scene.materials[0];
        assert_eq!(m.name, "paint");
        assert_eq!(m.base_color, [0.2, 0.4, 0.6]);
        assert_eq!(m.metallic, 1.0);
        assert!((m.roughness - 0.3).abs() < 1e-6);
        assert!(m.double_sided);
        assert!((m.ior - 1.33).abs() < 1e-6);
        assert!((m.transmission - 0.75).abs() < 1e-6);
        assert_eq!(scene.geometry[0].mesh.as_ref().unwrap().parts[0].material, 0);
    }

    #[test]
    fn a_primitive_that_names_no_material_gets_the_neutral_one() {
        let json = tiny_gltf(&[[0.0, 0.0, 0.0], [1.0, 0.0, 0.0], [0.0, 1.0, 0.0]], None, 4, "")
            .replace(r#","material":0"#, "");
        let (scene, _) = read(&json);
        assert_eq!(scene.materials.len(), 2, "the file's one, and the neutral one");
        assert_eq!(scene.geometry[0].mesh.as_ref().unwrap().parts[0].material, 1);
        assert_eq!(scene.materials[1].name, "default");
    }

    #[test]
    fn an_accessor_past_its_buffer_is_an_error_and_not_a_short_read() {
        let json = tiny_gltf(&[[0.0, 0.0, 0.0], [1.0, 0.0, 0.0], [0.0, 1.0, 0.0]], None, 4, "")
            .replace(r#""count":3,"type":"VEC3""#, r#""count":4,"type":"VEC3""#);
        // Validation may refuse this first; either way it is not a mesh.
        let err = scene_from_bytes(json.as_bytes(), None, "short.gltf").err().expect("refused");
        assert!(matches!(err, Error::Malformed(_) | Error::Gltf(_)), "{err}");
    }

    #[test]
    fn a_bin_beside_a_gltf_is_read_by_the_file_s_own_directory() {
        let dir = std::env::temp_dir().join("cad-gltf-beside");
        let _ = std::fs::create_dir_all(&dir);
        let json = tiny_gltf(&[[0.0, 0.0, 0.0], [1.0, 0.0, 0.0], [0.0, 1.0, 0.0]], None, 4, "");
        // Move the buffer out of the data URI and into a file with a space in
        // its name, which a URI spells `%20`.
        let start = json.find("data:").unwrap();
        let end = json[start..].find('"').unwrap() + start;
        let payload = &json[start..end];
        let bytes = base64_decode(payload.split_once(";base64,").unwrap().1).unwrap();
        std::fs::write(dir.join("my part.bin"), &bytes).unwrap();
        let json = json.replace(payload, "my%20part.bin");
        let path = dir.join("part.gltf");
        std::fs::write(&path, &json).unwrap();

        let (scene, _) = scene_from_file(&path).expect("reads with its bin");
        assert_eq!(scene.stored_triangle_count(), 1);
        let from_memory = scene_from_bytes(json.as_bytes(), None, "part.gltf");
        assert!(matches!(from_memory, Err(Error::Unsupported(_))), "nothing beside bytes");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_line_primitive_is_named_and_left_out() {
        let json = tiny_gltf(&[[0.0, 0.0, 0.0], [1.0, 0.0, 0.0]], None, 1, "");
        let (scene, report) = read(&json);
        assert!(scene.geometry[0].mesh.is_none());
        assert!(report.warnings.iter().any(|w| w.contains("Lines")), "{:?}", report.warnings);
    }

    #[test]
    fn base64_round_trips_and_refuses_junk() {
        for n in 0..20 {
            let bytes: Vec<u8> = (0..n).map(|i| (i * 37 % 251) as u8).collect();
            assert_eq!(base64_decode(&base64_encode(&bytes)).unwrap(), bytes);
        }
        assert!(base64_decode("not base64!").is_none());
    }
}
