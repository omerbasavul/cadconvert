//! USDZ: the same scene, for the tools that read USD rather than glTF.
//!
//! A USDZ is a zip archive that may not compress anything and whose file data
//! must start on a 64-byte boundary, so that a reader can memory-map a texture
//! or a mesh straight out of the package. Inside it goes one USD file and the
//! images it names.
//!
//! # The materials map across almost exactly
//!
//! `UsdPreviewSurface` and glTF's metallic-roughness model are the same model
//! with different spellings: base colour, metallic, roughness, opacity, index
//! of refraction, a tangent-space normal map. Everything this project recovers
//! survives the crossing. Two details do not line up on their own:
//!
//! * **Texture coordinates run the other way up.** glTF puts the origin at the
//!   top left; USD puts it at the bottom left. The exporter negates `v` once,
//!   for glTF, so USD gets it back here.
//! * **Tiling is a shader node, not an attribute.** glTF carries the repeat in
//!   `KHR_texture_transform`; USD wires a `UsdTransform2d` between the
//!   coordinate reader and the texture. Same number, more prims.
//!
//! # Why the text format
//!
//! USD has two encodings and this writes the text one, which costs size: the
//! pilot assembly is around three times its glTF. The binary crate format is
//! not a serialisation of the same tree — it is its own compressed container
//! with a token table, a path table and a bespoke integer encoding — and
//! writing one that other implementations read is a project of its own. The
//! text form is USD's own, every reader takes it, and it can be turned into
//! the binary one by `usdcat` in a step this does not have to own.

use crate::{ExportError, Options, Result};
use cad_ir::material::Material;
use cad_ir::mesh::Mesh;
use cad_ir::scene::{NodeId, Scene};
use std::fmt::Write as _;
use std::io::Write as _;
use std::path::Path;

pub fn write_file<P: AsRef<Path>>(scene: &Scene, options: &Options, path: P) -> Result<u64> {
    let bytes = write_bytes(scene, options)?;
    let mut file = std::fs::File::create(path.as_ref())?;
    file.write_all(&bytes)?;
    Ok(bytes.len() as u64)
}

pub fn write_bytes(scene: &Scene, options: &Options) -> Result<Vec<u8>> {
    if scene.geometry.iter().all(|g| g.mesh.is_none()) {
        return Err(ExportError::NoMesh);
    }
    let mut package = Package::default();

    // Images first, so the USD file can name them by the path they will have.
    let image_paths: Vec<String> = scene
        .images
        .iter()
        .enumerate()
        .map(|(i, image)| format!("textures/{i}.{}", image.mime.extension()))
        .collect();

    // The crate encoding unless something asks for the text one. A USDZ may
    // not compress anything, so the difference between the two is the
    // difference in the file: the pilot is 172 MB as text.
    let (name, bytes) = if options.usd_text {
        (default_file_name(scene, "usda"), write_usda(scene, options, &image_paths).into_bytes())
    } else {
        (
            default_file_name(scene, "usdc"),
            crate::usdc::write_scene(scene, options, &image_paths),
        )
    };

    // The USD file must be the archive's first entry: that is how a reader
    // knows which of the files in the package is the scene.
    package.add(name, bytes);
    for (image, path) in scene.images.iter().zip(&image_paths) {
        package.add(path.clone(), image.bytes.clone());
    }
    Ok(package.finish())
}

fn default_file_name(scene: &Scene, extension: &str) -> String {
    let stem = Path::new(&scene.meta.source)
        .file_stem()
        .map(|s| s.to_string_lossy().to_string())
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| "scene".into());
    format!("{}.{extension}", sanitise(&stem))
}

/// A USD prim name: alphanumerics and underscores, never starting with a digit.
///
/// Applied to body names out of CAD files, which carry spaces, dots and
/// part numbers that begin with a digit — `910 2001 007` is a prim called
/// `_910_2001_007`.
pub(crate) fn sanitise(raw: &str) -> String {
    let mut out = String::with_capacity(raw.len());
    for c in raw.chars() {
        out.push(if c.is_ascii_alphanumeric() { c } else { '_' });
    }
    if out.is_empty() {
        out.push('_');
    }
    if out.starts_with(|c: char| c.is_ascii_digit()) {
        out.insert(0, '_');
    }
    out
}

/// Names unique within one prim's children, the way [`Names`] is for glTF.
#[derive(Default)]
pub(crate) struct Names {
    seen: std::collections::HashMap<String, u32>,
}

impl Names {
    pub(crate) fn unique(&mut self, raw: &str) -> String {
        let base = sanitise(raw);
        let n = self.seen.entry(base.clone()).or_insert(0);
        *n += 1;
        if *n == 1 {
            base
        } else {
            format!("{base}_{n}")
        }
    }
}

fn write_usda(scene: &Scene, options: &Options, image_paths: &[String]) -> String {
    let mut out = String::with_capacity(1 << 20);
    let t = options.root_transform();
    let m = t.m;

    // metersPerUnit 1 and Y-up, with the conversion in the root transform, so
    // that this agrees exactly with what the glTF writer produces rather than
    // relying on every reader to honour a unit and an axis.
    let _ = write!(
        out,
        "#usda 1.0\n(\n    defaultPrim = \"root\"\n    metersPerUnit = 1\n    upAxis = \"Y\"\n"
    );
    if !scene.meta.source.is_empty() {
        // `doc`, not `comment`: a comment is not layer metadata in USD and
        // the stage fails to open outright.
        let _ = writeln!(out, "    doc = \"converted from {}\"", scene.meta.source);
    }
    out.push_str(")\n\n");

    // Prototypes live under a class, which USD does not image, so a mesh is
    // written once however many places it is instanced.
    let used: Vec<usize> = (0..scene.geometry.len())
        .filter(|&i| scene.geometry[i].mesh.as_ref().is_some_and(|m| !m.is_empty()))
        .collect();
    let mut geometry_names = vec![String::new(); scene.geometry.len()];
    let mut names = Names::default();
    for &i in &used {
        geometry_names[i] = names.unique(&scene.geometry[i].name);
    }

    out.push_str("class \"Prototypes\"\n{\n");
    for &i in &used {
        let mesh = scene.geometry[i].mesh.as_ref().expect("filtered above");
        write_mesh(&mut out, &geometry_names[i], mesh, scene, 1);
    }
    out.push_str("}\n\n");

    let _ = writeln!(out, "def Xform \"root\" (\n    kind = \"component\"\n)\n{{");
    let _ = writeln!(
        out,
        "    matrix4d xformOp:transform = ( ({}, {}, {}, 0), ({}, {}, {}, 0), ({}, {}, {}, 0), ({}, {}, {}, 1) )",
        m[0][0], m[1][0], m[2][0],
        m[0][1], m[1][1], m[2][1],
        m[0][2], m[1][2], m[2][2],
        m[0][3], m[1][3], m[2][3],
    );
    out.push_str("    uniform token[] xformOpOrder = [\"xformOp:transform\"]\n\n");

    write_materials(&mut out, scene, image_paths, 1);

    let mut names = Names::default();
    for &root in &scene.roots {
        write_node(&mut out, scene, root, &geometry_names, &mut names, 1);
    }
    out.push_str("}\n");
    out
}

fn indent(out: &mut String, depth: usize) {
    for _ in 0..depth {
        out.push_str("    ");
    }
}

fn write_node(
    out: &mut String,
    scene: &Scene,
    id: NodeId,
    geometry_names: &[String],
    names: &mut Names,
    depth: usize,
) {
    let node = scene.node(id);
    let has_geometry = node
        .geometry
        .map(|g| !geometry_names[g.index()].is_empty())
        .unwrap_or(false);
    if !has_geometry && node.children.is_empty() {
        return;
    }
    let name = names.unique(&node.name);

    indent(out, depth);
    if let (true, Some(g)) = (has_geometry, node.geometry) {
        // A typeless `def` takes its type from what it references, and a
        // reference into a class prim brings the whole mesh with it. Marked
        // instanceable so a reader shares one prototype between placements —
        // 46 meshes stand behind 64 placements in the pilot.
        let _ = writeln!(
            out,
            "def \"{name}\" (\n{}    instanceable = true\n{}    prepend references = </Prototypes/{}>\n{})\n",
            "    ".repeat(depth),
            "    ".repeat(depth),
            geometry_names[g.index()],
            "    ".repeat(depth),
        );
        indent(out, depth);
    } else {
        let _ = writeln!(out, "def Xform \"{name}\"");
        indent(out, depth);
    }
    out.push_str("{\n");

    if !node.transform.is_identity(1e-12) {
        let m = node.transform.m;
        indent(out, depth + 1);
        let _ = writeln!(
            out,
            "matrix4d xformOp:transform = ( ({}, {}, {}, 0), ({}, {}, {}, 0), ({}, {}, {}, 0), ({}, {}, {}, 1) )",
            m[0][0], m[1][0], m[2][0],
            m[0][1], m[1][1], m[2][1],
            m[0][2], m[1][2], m[2][2],
            m[0][3], m[1][3], m[2][3],
        );
        indent(out, depth + 1);
        out.push_str("uniform token[] xformOpOrder = [\"xformOp:transform\"]\n");
    }

    // An instanced prim's contents are the prototype's, so children of a
    // geometry node go beside it rather than inside it. The scene's geometry
    // nodes are leaves in practice; this keeps the file valid if one is not.
    let mut child_names = Names::default();
    for &child in &node.children {
        write_node(out, scene, child, geometry_names, &mut child_names, depth + 1);
    }

    indent(out, depth);
    out.push_str("}\n");
}

/// Numbers as USD wants them: enough digits to be exact for an `f32`, and no
/// exponent-free padding.
fn f(v: f32) -> String {
    if v == v.trunc() && v.abs() < 1e7 {
        format!("{v:.0}")
    } else {
        let mut s = format!("{v}");
        if !s.contains(['.', 'e', 'E']) {
            s.push_str(".0");
        }
        s
    }
}

fn write_mesh(out: &mut String, name: &str, mesh: &Mesh, scene: &Scene, depth: usize) {
    indent(out, depth);
    // A prim that carries a material binding has to say so: without the
    // MaterialBindingAPI applied, usdchecker calls the binding an error and a
    // reader is entitled to ignore it.
    let binds_here = mesh.parts.len() == 1;
    if binds_here {
        let _ = writeln!(out, "def Mesh \"{name}\" (");
        indent(out, depth);
        out.push_str("    prepend apiSchemas = [\"MaterialBindingAPI\"]\n");
        indent(out, depth);
        out.push_str(")\n");
        indent(out, depth);
    } else {
        let _ = writeln!(out, "def Mesh \"{name}\"");
        indent(out, depth);
    }
    out.push_str("{\n");
    let d = depth + 1;

    // Every face a triangle. USD wants the count per face and then the corners
    // run together.
    indent(out, d);
    let _ = writeln!(out, "int[] faceVertexCounts = [{}]", Repeat(3, mesh.triangle_count()));

    indent(out, d);
    out.push_str("int[] faceVertexIndices = [");
    for (i, index) in mesh.indices.iter().enumerate() {
        if i > 0 {
            out.push_str(", ");
        }
        let _ = write!(out, "{index}");
    }
    out.push_str("]\n");

    indent(out, d);
    out.push_str("point3f[] points = [");
    for (i, p) in mesh.positions.iter().enumerate() {
        if i > 0 {
            out.push_str(", ");
        }
        let _ = write!(out, "({}, {}, {})", f(p[0]), f(p[1]), f(p[2]));
    }
    out.push_str("]\n");

    if mesh.normals.len() == mesh.positions.len() {
        indent(out, d);
        out.push_str("normal3f[] normals = [");
        for (i, n) in mesh.normals.iter().enumerate() {
            if i > 0 {
                out.push_str(", ");
            }
            let _ = write!(out, "({}, {}, {})", f(n[0]), f(n[1]), f(n[2]));
        }
        out.push_str("] (\n");
        indent(out, d + 1);
        out.push_str("interpolation = \"vertex\"\n");
        indent(out, d);
        out.push_str(")\n");
    }

    if mesh.uvs.len() == mesh.positions.len() {
        indent(out, d);
        out.push_str("texCoord2f[] primvars:st = [");
        for (i, uv) in mesh.uvs.iter().enumerate() {
            if i > 0 {
                out.push_str(", ");
            }
            // glTF's origin is top left and USD's is bottom left. The exporter
            // negated v once on the way out; this puts it back.
            let _ = write!(out, "({}, {})", f(uv[0]), f(-uv[1]));
        }
        out.push_str("] (\n");
        indent(out, d + 1);
        out.push_str("interpolation = \"vertex\"\n");
        indent(out, d);
        out.push_str(")\n");
    }

    indent(out, d);
    out.push_str("uniform token subdivisionScheme = \"none\"\n");

    let double_sided = mesh
        .parts
        .iter()
        .filter_map(|p| scene.materials.get(p.material as usize))
        .any(|m| m.double_sided);
    if double_sided {
        indent(out, d);
        out.push_str("uniform bool doubleSided = 1\n");
    }

    // One material for the whole mesh, or a subset per run of faces.
    match mesh.parts.as_slice() {
        [] => {}
        [only] => {
            indent(out, d);
            let _ = writeln!(
                out,
                "rel material:binding = </root/Looks/{}>",
                material_name(scene, only.material)
            );
        }
        parts => {
            indent(out, d);
            out.push_str("uniform token subsetFamily:materialBind:familyType = \"partition\"\n");
            for (i, part) in parts.iter().enumerate() {
                indent(out, d);
                let _ = writeln!(out, "def GeomSubset \"part_{i}\" (");
                indent(out, d);
                out.push_str("    prepend apiSchemas = [\"MaterialBindingAPI\"]\n");
                indent(out, d);
                out.push_str(")\n");
                indent(out, d);
                out.push_str("{\n");
                indent(out, d + 1);
                out.push_str("uniform token elementType = \"face\"\n");
                indent(out, d + 1);
                out.push_str("uniform token familyName = \"materialBind\"\n");
                indent(out, d + 1);
                out.push_str("int[] indices = [");
                let first = part.start as usize / 3;
                for k in 0..(part.count as usize / 3) {
                    if k > 0 {
                        out.push_str(", ");
                    }
                    let _ = write!(out, "{}", first + k);
                }
                out.push_str("]\n");
                indent(out, d + 1);
                let _ = writeln!(
                    out,
                    "rel material:binding = </root/Looks/{}>",
                    material_name(scene, part.material)
                );
                indent(out, d);
                out.push_str("}\n");
            }
        }
    }

    indent(out, depth);
    out.push_str("}\n");
}

/// `n` repeated `count` times, comma separated, without building the vector.
struct Repeat(u32, usize);

impl std::fmt::Display for Repeat {
    fn fmt(&self, out: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        for i in 0..self.1 {
            if i > 0 {
                out.write_str(", ")?;
            }
            write!(out, "{}", self.0)?;
        }
        Ok(())
    }
}

pub(crate) fn material_name(scene: &Scene, index: u32) -> String {
    scene
        .materials
        .get(index as usize)
        .map(|m| format!("{}_{index}", sanitise(&m.name)))
        .unwrap_or_else(|| format!("material_{index}"))
}

fn write_materials(out: &mut String, scene: &Scene, image_paths: &[String], depth: usize) {
    indent(out, depth);
    out.push_str("def Scope \"Looks\"\n");
    indent(out, depth);
    out.push_str("{\n");

    let materials: Vec<(String, &Material)> = if scene.materials.is_empty() {
        Vec::new()
    } else {
        scene
            .materials
            .iter()
            .enumerate()
            .map(|(i, m)| (material_name(scene, i as u32), m))
            .collect()
    };

    for (name, material) in materials {
        write_material(out, &name, material, image_paths, depth + 1);
    }

    indent(out, depth);
    out.push_str("}\n\n");
}

fn write_material(
    out: &mut String,
    name: &str,
    material: &Material,
    image_paths: &[String],
    depth: usize,
) {
    let path = format!("/root/Looks/{name}");
    let d = depth + 1;
    indent(out, depth);
    let _ = writeln!(out, "def Material \"{name}\"");
    indent(out, depth);
    out.push_str("{\n");
    indent(out, d);
    let _ = writeln!(out, "token outputs:surface.connect = <{path}/surface.outputs:surface>");

    let textures = material.textures;
    let tile = textures.tile_mm();

    // The coordinate reader and the tiling, written once and shared by both
    // images: USD wires these as prims rather than carrying them on the
    // texture the way glTF does.
    if !textures.is_empty() {
        indent(out, d);
        out.push_str("def Shader \"stReader\"\n");
        indent(out, d);
        out.push_str("{\n");
        indent(out, d + 1);
        out.push_str("uniform token info:id = \"UsdPrimvarReader_float2\"\n");
        indent(out, d + 1);
        // A string, not a token: the shader definition says so and
        // usdchecker checks it.
        out.push_str("string inputs:varname = \"st\"\n");
        indent(out, d + 1);
        out.push_str("float2 outputs:result\n");
        indent(out, d);
        out.push_str("}\n");

        if let Some([w, h]) = tile {
            indent(out, d);
            out.push_str("def Shader \"stTransform\"\n");
            indent(out, d);
            out.push_str("{\n");
            indent(out, d + 1);
            out.push_str("uniform token info:id = \"UsdTransform2d\"\n");
            indent(out, d + 1);
            let _ = writeln!(out, "float2 inputs:in.connect = <{path}/stReader.outputs:result>");
            indent(out, d + 1);
            // Coordinates are millimetres of surface; one repeat per tile.
            let _ = writeln!(out, "float2 inputs:scale = ({}, {})", f(1.0 / w), f(1.0 / h));
            indent(out, d + 1);
            out.push_str("float2 outputs:result\n");
            indent(out, d);
            out.push_str("}\n");
        }
    }

    let st_source = if tile.is_some() {
        format!("{path}/stTransform.outputs:result")
    } else {
        format!("{path}/stReader.outputs:result")
    };

    if let Some(id) = textures.base_colour {
        let c = material.base_color;
        write_texture(
            out,
            Texture {
                name: "diffuseTexture",
                file: image_paths.get(id.index()).map(String::as_str).unwrap_or(""),
                st_source: &st_source,
                // The part's own colour, applied to the grain the way glTF's
                // base colour factor is.
                scale: Some([c[0], c[1], c[2], 1.0]),
                bias: None,
                // The default, stated: a colour image is gamma-encoded.
                colour_space: "sRGB",
            },
            d,
        );
    }
    if let Some(id) = textures.normal {
        write_texture(
            out,
            Texture {
                name: "normalTexture",
                file: image_paths.get(id.index()).map(String::as_str).unwrap_or(""),
                st_source: &st_source,
                // An eight-bit normal map holds 0..1 and a normal runs -1..1.
                // glTF's readers apply that remapping themselves; USD's expect
                // it spelled out on the texture, and without it every normal
                // in the file is read as pointing into the surface.
                scale: Some([2.0, 2.0, 2.0, 1.0]),
                bias: Some([-1.0, -1.0, -1.0, 0.0]),
                // Never sRGB: these are vectors, not colours, and
                // linearising them bends every one of them.
                colour_space: "raw",
            },
            d,
        );
    }

    indent(out, d);
    out.push_str("def Shader \"surface\"\n");
    indent(out, d);
    out.push_str("{\n");
    let s = d + 1;
    indent(out, s);
    out.push_str("uniform token info:id = \"UsdPreviewSurface\"\n");

    indent(out, s);
    if textures.base_colour.is_some() {
        let _ = writeln!(
            out,
            "color3f inputs:diffuseColor.connect = <{path}/diffuseTexture.outputs:rgb>"
        );
    } else {
        let c = material.base_color;
        let _ = writeln!(
            out,
            "color3f inputs:diffuseColor = ({}, {}, {})",
            f(c[0]),
            f(c[1]),
            f(c[2])
        );
    }
    if textures.normal.is_some() {
        indent(out, s);
        let _ = writeln!(
            out,
            "normal3f inputs:normal.connect = <{path}/normalTexture.outputs:rgb>"
        );
    }
    indent(out, s);
    let _ = writeln!(out, "float inputs:metallic = {}", f(material.metallic));
    indent(out, s);
    let _ = writeln!(out, "float inputs:roughness = {}", f(material.roughness));
    if (material.ior - 1.5).abs() > 1e-3 {
        indent(out, s);
        let _ = writeln!(out, "float inputs:ior = {}", f(material.ior));
    }
    if material.alpha < 1.0 {
        indent(out, s);
        let _ = writeln!(out, "float inputs:opacity = {}", f(material.alpha));
    }
    if material.emissive.iter().any(|&c| c > 0.0) {
        indent(out, s);
        let e = material.emissive;
        let _ = writeln!(
            out,
            "color3f inputs:emissiveColor = ({}, {}, {})",
            f(e[0]),
            f(e[1]),
            f(e[2])
        );
    }
    indent(out, s);
    out.push_str("token outputs:surface\n");
    indent(out, d);
    out.push_str("}\n");

    indent(out, depth);
    out.push_str("}\n");
}

struct Texture<'a> {
    name: &'a str,
    file: &'a str,
    st_source: &'a str,
    scale: Option<[f32; 4]>,
    bias: Option<[f32; 4]>,
    colour_space: &'a str,
}

fn write_texture(out: &mut String, texture: Texture, depth: usize) {
    indent(out, depth);
    let _ = writeln!(out, "def Shader \"{}\"", texture.name);
    indent(out, depth);
    out.push_str("{\n");
    let d = depth + 1;
    indent(out, d);
    out.push_str("uniform token info:id = \"UsdUVTexture\"\n");
    indent(out, d);
    let _ = writeln!(out, "asset inputs:file = @{}@", texture.file);
    indent(out, d);
    let _ = writeln!(out, "float2 inputs:st.connect = <{}>", texture.st_source);
    indent(out, d);
    out.push_str("token inputs:wrapS = \"repeat\"\n");
    indent(out, d);
    out.push_str("token inputs:wrapT = \"repeat\"\n");
    indent(out, d);
    let _ = writeln!(
        out,
        "token inputs:sourceColorSpace = \"{}\"",
        texture.colour_space
    );
    if let Some(s) = texture.scale {
        indent(out, d);
        let _ = writeln!(
            out,
            "float4 inputs:scale = ({}, {}, {}, {})",
            f(s[0]), f(s[1]), f(s[2]), f(s[3])
        );
    }
    if let Some(b) = texture.bias {
        indent(out, d);
        let _ = writeln!(
            out,
            "float4 inputs:bias = ({}, {}, {}, {})",
            f(b[0]), f(b[1]), f(b[2]), f(b[3])
        );
    }
    indent(out, d);
    out.push_str("float3 outputs:rgb\n");
    indent(out, depth);
    out.push_str("}\n");
}

/// The zip a USDZ is: every entry stored, and every entry's data starting on a
/// 64-byte boundary so a reader can map it in place.
#[derive(Default)]
struct Package {
    out: Vec<u8>,
    entries: Vec<Entry>,
}

struct Entry {
    name: String,
    offset: u32,
    size: u32,
    crc: u32,
}

impl Package {
    fn add(&mut self, name: String, data: Vec<u8>) {
        let header_at = self.out.len();
        let name_bytes = name.as_bytes();

        // The data must land on a 64-byte boundary, and the only place to put
        // padding is the entry's extra field — which is why the size of that
        // field is worked out before the header is written rather than after.
        let fixed = 30 + name_bytes.len();
        let mut extra = (64 - (header_at + fixed) % 64) % 64;
        // An extra field is at least four bytes: a two-byte id and a two-byte
        // length. Less than that and the padding has to be a whole block more.
        if extra != 0 && extra < 4 {
            extra += 64;
        }

        let crc = crc32(&data);
        self.out.extend_from_slice(&0x0403_4b50u32.to_le_bytes()); // local file header
        self.out.extend_from_slice(&20u16.to_le_bytes()); // version needed
        self.out.extend_from_slice(&0u16.to_le_bytes()); // flags
        self.out.extend_from_slice(&0u16.to_le_bytes()); // stored
        self.out.extend_from_slice(&0u16.to_le_bytes()); // time
        self.out.extend_from_slice(&0u16.to_le_bytes()); // date
        self.out.extend_from_slice(&crc.to_le_bytes());
        self.out.extend_from_slice(&(data.len() as u32).to_le_bytes());
        self.out.extend_from_slice(&(data.len() as u32).to_le_bytes());
        self.out.extend_from_slice(&(name_bytes.len() as u16).to_le_bytes());
        self.out.extend_from_slice(&(extra as u16).to_le_bytes());
        self.out.extend_from_slice(name_bytes);
        if extra >= 4 {
            // 0xFFFF is the id reserved for "anything a reader does not know",
            // which is what this is: alignment and nothing else.
            self.out.extend_from_slice(&0xFFFFu16.to_le_bytes());
            self.out.extend_from_slice(&((extra - 4) as u16).to_le_bytes());
            self.out.resize(self.out.len() + extra - 4, 0);
        }
        debug_assert_eq!(self.out.len() % 64, 0, "USDZ data must be 64-byte aligned");

        self.entries.push(Entry {
            name,
            offset: header_at as u32,
            size: data.len() as u32,
            crc,
        });
        self.out.extend_from_slice(&data);
    }

    fn finish(mut self) -> Vec<u8> {
        let directory_at = self.out.len();
        for entry in &self.entries {
            let name = entry.name.as_bytes();
            self.out.extend_from_slice(&0x0201_4b50u32.to_le_bytes()); // central directory
            self.out.extend_from_slice(&20u16.to_le_bytes()); // version made by
            self.out.extend_from_slice(&20u16.to_le_bytes()); // version needed
            self.out.extend_from_slice(&0u16.to_le_bytes()); // flags
            self.out.extend_from_slice(&0u16.to_le_bytes()); // stored
            self.out.extend_from_slice(&0u16.to_le_bytes()); // time
            self.out.extend_from_slice(&0u16.to_le_bytes()); // date
            self.out.extend_from_slice(&entry.crc.to_le_bytes());
            self.out.extend_from_slice(&entry.size.to_le_bytes());
            self.out.extend_from_slice(&entry.size.to_le_bytes());
            self.out.extend_from_slice(&(name.len() as u16).to_le_bytes());
            self.out.extend_from_slice(&0u16.to_le_bytes()); // extra length
            self.out.extend_from_slice(&0u16.to_le_bytes()); // comment length
            self.out.extend_from_slice(&0u16.to_le_bytes()); // disk number
            self.out.extend_from_slice(&0u16.to_le_bytes()); // internal attributes
            self.out.extend_from_slice(&0u32.to_le_bytes()); // external attributes
            self.out.extend_from_slice(&entry.offset.to_le_bytes());
            self.out.extend_from_slice(name);
        }
        let directory_size = self.out.len() - directory_at;

        self.out.extend_from_slice(&0x0605_4b50u32.to_le_bytes()); // end of directory
        self.out.extend_from_slice(&0u16.to_le_bytes());
        self.out.extend_from_slice(&0u16.to_le_bytes());
        self.out.extend_from_slice(&(self.entries.len() as u16).to_le_bytes());
        self.out.extend_from_slice(&(self.entries.len() as u16).to_le_bytes());
        self.out.extend_from_slice(&(directory_size as u32).to_le_bytes());
        self.out.extend_from_slice(&(directory_at as u32).to_le_bytes());
        self.out.extend_from_slice(&0u16.to_le_bytes()); // comment length
        self.out
    }
}

fn crc32(data: &[u8]) -> u32 {
    let mut c = 0xFFFF_FFFFu32;
    for &byte in data {
        let mut x = (c ^ byte as u32) & 0xFF;
        for _ in 0..8 {
            x = if x & 1 != 0 { 0xEDB8_8320 ^ (x >> 1) } else { x >> 1 };
        }
        c = x ^ (c >> 8);
    }
    c ^ 0xFFFF_FFFF
}

#[cfg(test)]
mod tests {
    use super::*;
    use cad_ir::material::MaterialClass;
    use cad_ir::mesh::MeshPart;
    use cad_ir::scene::{Geometry, Node};

    fn scene() -> Scene {
        let mut s = Scene::default();
        s.add_material(Material::from_class(MaterialClass::Steel, "steel"));
        let mesh = Mesh {
            positions: vec![[0.0; 3], [10.0, 0.0, 0.0], [0.0, 10.0, 0.0]],
            normals: vec![[0.0, 0.0, 1.0]; 3],
            uvs: vec![],
            indices: vec![0, 1, 2],
            parts: vec![MeshPart { material: 0, start: 0, count: 3 }],
        };
        let g = s.add_geometry(Geometry {
            name: "910 2001 007".into(),
            brep: None,
            mesh: Some(mesh),
            material: None,
            face_materials: vec![],
        });
        for i in 0..2 {
            let n = s.add_node(Node {
                name: format!("body {i}"),
                geometry: Some(g),
                ..Default::default()
            });
            s.roots.push(n);
        }
        s
    }

    fn entries(bytes: &[u8]) -> Vec<(String, usize, usize)> {
        // Walk the local headers, which is what a USDZ reader does.
        let mut out = Vec::new();
        let mut at = 0usize;
        while at + 30 <= bytes.len()
            && u32::from_le_bytes(bytes[at..at + 4].try_into().unwrap()) == 0x0403_4b50
        {
            let size = u32::from_le_bytes(bytes[at + 18..at + 22].try_into().unwrap()) as usize;
            let name_len = u16::from_le_bytes(bytes[at + 26..at + 28].try_into().unwrap()) as usize;
            let extra_len = u16::from_le_bytes(bytes[at + 28..at + 30].try_into().unwrap()) as usize;
            let name = String::from_utf8_lossy(&bytes[at + 30..at + 30 + name_len]).into_owned();
            let data_at = at + 30 + name_len + extra_len;
            out.push((name, data_at, size));
            at = data_at + size;
        }
        out
    }

    #[test]
    fn every_entry_is_stored_and_starts_on_a_sixty_four_byte_boundary() {
        // The whole point of the format: a reader maps a texture straight out
        // of the package, which it can only do if nothing is compressed and
        // everything is aligned.
        let mut s = scene();
        s.add_image(cad_ir::image::Image {
            name: "grain.png".into(),
            mime: cad_ir::image::Mime::Png,
            width: 2,
            height: 2,
            bytes: cad_ir::image::encode_png(2, 2, &[200; 16]),
        });
        let bytes = write_bytes(&s, &Options::default()).unwrap();

        let found = entries(&bytes);
        assert_eq!(found.len(), 2, "the scene and one image");
        for (name, at, _) in &found {
            assert_eq!(at % 64, 0, "{name} starts at {at}, not a multiple of 64");
        }
        // Compression method zero, in every local header.
        let mut at = 0usize;
        while at + 30 <= bytes.len()
            && u32::from_le_bytes(bytes[at..at + 4].try_into().unwrap()) == 0x0403_4b50
        {
            assert_eq!(
                u16::from_le_bytes(bytes[at + 8..at + 10].try_into().unwrap()),
                0,
                "a USDZ may not compress"
            );
            let size = u32::from_le_bytes(bytes[at + 18..at + 22].try_into().unwrap()) as usize;
            let name_len = u16::from_le_bytes(bytes[at + 26..at + 28].try_into().unwrap()) as usize;
            let extra_len = u16::from_le_bytes(bytes[at + 28..at + 30].try_into().unwrap()) as usize;
            at += 30 + name_len + extra_len + size;
        }
    }

    /// The text encoding, which is what the tests below are about. The
    /// default is the binary one; see [`crate::usdc`].
    fn text() -> Options {
        Options { usd_text: true, ..Options::default() }
    }

    #[test]
    fn the_usd_file_comes_first_and_names_the_scene() {
        // Whichever encoding, the scene is the archive's first entry: that is
        // how a reader knows which of the files in the package it is.
        for (options, extension) in [(Options::default(), ".usdc"), (text(), ".usda")] {
            let bytes = write_bytes(&scene(), &options).unwrap();
            let found = entries(&bytes);
            assert!(
                found[0].0.ends_with(extension),
                "expected {extension} first, got {}",
                found[0].0
            );
        }
    }

    #[test]
    fn a_part_number_becomes_a_prim_name_usd_will_accept() {
        // `910 2001 007` is not an identifier: spaces, and it begins with a
        // digit. Both are refused by USD, silently in some readers.
        assert_eq!(sanitise("910 2001 007"), "_910_2001_007");
        assert_eq!(sanitise("body.1-a"), "body_1_a");
        assert_eq!(sanitise(""), "_");

        let bytes = write_bytes(&scene(), &text()).unwrap();
        let written = String::from_utf8_lossy(&bytes);
        assert!(written.contains("def Mesh \"_910_2001_007\""));
    }

    #[test]
    fn one_mesh_stands_behind_both_placements() {
        // The text encoding instances; the binary one writes the mesh at each
        // placement, because a reference is a composition arc whose encoding
        // has not been read off a file yet. See [`crate::usdc::scene`].
        let bytes = write_bytes(&scene(), &text()).unwrap();
        let written = String::from_utf8_lossy(&bytes);
        assert_eq!(written.matches("def Mesh ").count(), 1, "the mesh is written once");
        assert_eq!(
            written.matches("prepend references = </Prototypes/_910_2001_007>").count(),
            2,
            "and referenced by both nodes"
        );
        assert_eq!(written.matches("instanceable = true").count(), 2);
    }

    #[test]
    fn the_binary_encoding_is_the_default_and_is_smaller() {
        // On anything with a mesh in it. The crate format carries a token
        // table, six sections and a table of contents whatever the scene, so
        // on a single triangle it loses — 2 542 bytes against 1 649 — and the
        // two cross somewhere in the low hundreds of triangles. What matters
        // is which side a real part falls on: the pilot assembly is 50 MB
        // binary against 172 MB text.
        let mut s = Scene::default();
        s.add_material(Material::from_class(MaterialClass::Steel, "steel"));
        let mut mesh = Mesh {
            positions: Vec::new(),
            normals: Vec::new(),
            uvs: Vec::new(),
            indices: Vec::new(),
            parts: Vec::new(),
        };
        for i in 0..2_000u32 {
            let x = i as f32 * 0.37;
            mesh.positions
                .extend_from_slice(&[[x, 0.0, 0.0], [x + 1.0, 0.0, 0.0], [x, 1.0, 0.0]]);
            mesh.normals.extend_from_slice(&[[0.0, 0.0, 1.0]; 3]);
            let base = i * 3;
            mesh.indices.extend_from_slice(&[base, base + 1, base + 2]);
        }
        mesh.parts = vec![MeshPart { material: 0, start: 0, count: 6_000 }];
        let g = s.add_geometry(Geometry {
            name: "plate".into(),
            brep: None,
            mesh: Some(mesh),
            material: None,
            face_materials: vec![],
        });
        let n = s.add_node(Node { name: "plate".into(), geometry: Some(g), ..Default::default() });
        s.roots.push(n);

        let small = write_bytes(&s, &Options::default()).unwrap();
        let large = write_bytes(&s, &text()).unwrap();
        // How much smaller depends on the numbers: a coordinate like 0.37
        // costs four characters and one like -157.03421 costs ten, so a
        // synthetic mesh flatters the text form. This one gives 148 KB
        // against 206 KB; the pilot, whose coordinates are real, gives 50 MB
        // against 172.
        assert!(
            small.len() < large.len(),
            "binary {} is not under text {}",
            small.len(),
            large.len()
        );
        assert!(entries(&small)[0].0.ends_with(".usdc"));
    }

    #[test]
    fn a_scene_with_no_mesh_is_refused_rather_than_written_empty() {
        let s = Scene::default();
        assert!(matches!(
            write_bytes(&s, &Options::default()),
            Err(ExportError::NoMesh)
        ));
    }

    #[test]
    fn the_crc_of_every_entry_is_the_crc_of_its_bytes() {
        // A wrong CRC makes the package unreadable to anything that checks,
        // and readable to anything that does not — the worst pair.
        let bytes = write_bytes(&scene(), &Options::default()).unwrap();
        let mut at = 0usize;
        while at + 30 <= bytes.len()
            && u32::from_le_bytes(bytes[at..at + 4].try_into().unwrap()) == 0x0403_4b50
        {
            let stated = u32::from_le_bytes(bytes[at + 14..at + 18].try_into().unwrap());
            let size = u32::from_le_bytes(bytes[at + 18..at + 22].try_into().unwrap()) as usize;
            let name_len = u16::from_le_bytes(bytes[at + 26..at + 28].try_into().unwrap()) as usize;
            let extra_len = u16::from_le_bytes(bytes[at + 28..at + 30].try_into().unwrap()) as usize;
            let data_at = at + 30 + name_len + extra_len;
            assert_eq!(stated, crc32(&bytes[data_at..data_at + size]));
            at = data_at + size;
        }
    }
}
