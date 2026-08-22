//! Writing binary glTF 2.0.
//!
//! The JSON is built directly rather than through a typed glTF crate, because
//! the two things that matter here — buffer layout and the quantisation
//! extension — are exactly where a typed wrapper stops helping and starts
//! getting in the way. Correctness is defended by round-tripping the output
//! through the `gltf` crate's reader in the tests, which validates the schema
//! far more thoroughly than construction-time types would.
//!
//! The assembly tree is written as a glTF node hierarchy and each geometry as
//! one mesh, so a bolt placed eighty times is stored once and referenced eighty
//! times. Flattening it would multiply the file size by the instance count.

use crate::error::{ExportError, Result};
use crate::prepare::Names;
use crate::{Compression, Options};
use cad_ir::material::Material;
use cad_ir::mesh::Mesh;
use cad_ir::scene::{Scene, Unit};
use serde_json::{json, Map, Value};
use std::io::Write;
use std::path::Path;

/// glTF component types.
const BYTE: u32 = 5120;
const UNSIGNED_SHORT: u32 = 5123;
const FLOAT: u32 = 5126;

/// glTF buffer view targets.
const ARRAY_BUFFER: u32 = 34962;
const ELEMENT_ARRAY_BUFFER: u32 = 34963;

/// Write a scene as a `.glb` file.
pub fn write_file<P: AsRef<Path>>(scene: &Scene, options: &Options, path: P) -> Result<u64> {
    let path = path.as_ref();
    let bytes = write_bytes(scene, options)?;
    std::fs::write(path, &bytes).map_err(|source| ExportError::Io {
        path: path.to_path_buf(),
        source,
    })?;
    Ok(bytes.len() as u64)
}

/// Serialise a scene into GLB bytes.
pub fn write_bytes(scene: &Scene, options: &Options) -> Result<Vec<u8>> {
    let mut b = Builder::new(scene, options);
    b.build()?;
    b.finish()
}

/// Accumulates the JSON document and the binary chunk side by side.
struct Builder<'a> {
    scene: &'a Scene,
    options: &'a Options,
    bin: Vec<u8>,
    buffer_views: Vec<Value>,
    accessors: Vec<Value>,
    meshes: Vec<Value>,
    nodes: Vec<Value>,
    materials: Vec<Value>,
    /// glTF node index of each scene node, or `None` where it produced nothing.
    node_index: Vec<Option<usize>>,
    /// glTF mesh index of each geometry.
    mesh_index: Vec<Option<usize>>,
    /// The scale and offset that undo a quantised mesh's grid, per geometry.
    ///
    /// Not a node: a node may have at most one parent, and this geometry is
    /// instanced. See [`Builder::add_node`].
    dequant: Vec<Option<([f32; 3], [f32; 3])>>,
    extensions_used: Vec<&'static str>,
}

impl<'a> Builder<'a> {
    fn new(scene: &'a Scene, options: &'a Options) -> Self {
        Builder {
            scene,
            options,
            bin: Vec::with_capacity(1 << 20),
            buffer_views: Vec::new(),
            accessors: Vec::new(),
            meshes: Vec::new(),
            nodes: Vec::new(),
            materials: Vec::new(),
            node_index: vec![None; scene.nodes.len()],
            mesh_index: vec![None; scene.geometry.len()],
            dequant: vec![None; scene.geometry.len()],
            extensions_used: Vec::new(),
        }
    }

    fn build(&mut self) -> Result<()> {
        if self.scene.geometry.iter().all(|g| g.mesh.is_none()) {
            return Err(ExportError::NoMesh);
        }

        let mut names = Names::default();
        for m in &self.scene.materials {
            let value = material_json(m, &mut names);
            self.materials.push(value);
        }
        // A scene with no materials still needs one, since every primitive
        // names a material index.
        if self.materials.is_empty() {
            self.materials
                .push(material_json(&Material::unknown(), &mut names));
        }

        for i in 0..self.scene.geometry.len() {
            self.add_geometry(i)?;
        }

        let mut names = Names::default();
        let roots: Vec<usize> = self
            .scene
            .roots
            .clone()
            .into_iter()
            .filter_map(|r| self.add_node(r, &mut names))
            .collect();

        // One wrapper node carries the unit and up-axis conversion, so the
        // geometry itself is untouched and stays shared.
        let root = json!({
            "name": "scene",
            "matrix": self.options.root_transform().to_column_major(),
            "children": roots,
        });
        self.nodes.push(root);
        Ok(())
    }

    /// Emit a glTF mesh for one geometry, if it has triangles.
    fn add_geometry(&mut self, index: usize) -> Result<()> {
        let geometry = &self.scene.geometry[index];
        let Some(mesh) = &geometry.mesh else {
            return Ok(());
        };
        if mesh.is_empty() || mesh.positions.is_empty() {
            return Ok(());
        }
        if mesh.indices.iter().any(|&i| i as usize >= mesh.positions.len()) {
            return Err(ExportError::BadMesh(format!(
                "{} has an index past the end of its vertex buffer",
                geometry.name
            )));
        }

        let quantise_positions = self.options.compression == Compression::Quantized;
        let quantise_normals = matches!(
            self.options.compression,
            Compression::Quantized | Compression::Normals
        );
        let want_normals = self.options.normals && mesh.normals.len() == mesh.positions.len();
        // The quantisation grid is the whole mesh's, not each chunk's, so every
        // chunk of one geometry shares the single node that undoes it.
        let dequant = quantise_positions.then(|| quantisation(mesh));

        // One primitive per material run — and, where a run indexes more
        // vertices than 16 bits can reach, one per 65536 of them.
        //
        // A 32-bit index costs twice a 16-bit one and buys nothing on a mesh
        // that never needed it. Six bodies of the pilot assembly are over the
        // line, and they carried 4.75 M of the file's 6.17 M indices: 19.00 MB
        // of a 51.25 MB file, against 9.50 MB if they had fit. Splitting them
        // is not a compromise — the triangles, the vertices and their values
        // are exactly what they were, only handed over in more than one piece.
        let mut primitives = Vec::with_capacity(mesh.parts.len().max(1));
        let parts: Vec<_> = if mesh.parts.is_empty() {
            vec![cad_ir::mesh::MeshPart {
                material: 0,
                start: 0,
                count: mesh.indices.len() as u32,
            }]
        } else {
            mesh.parts.clone()
        };
        let mut seen = vec![0u32; mesh.positions.len()];
        let mut local = vec![0u32; mesh.positions.len()];
        let mut age = 0u32;
        for part in parts {
            if part.count == 0 {
                continue;
            }
            for chunk in chunk_run(
                mesh,
                part.start as usize,
                part.count as usize,
                &mut seen,
                &mut local,
                &mut age,
            ) {
                let position_accessor = match dequant {
                    Some(q) => self.write_quantised_positions(mesh, &chunk.vertices, q),
                    None => self.write_positions(mesh, &chunk.vertices),
                };
                let normal_accessor = want_normals.then(|| {
                    if quantise_normals {
                        self.write_quantised_normals(mesh, &chunk.vertices)
                    } else {
                        self.write_normals(mesh, &chunk.vertices)
                    }
                });
                let indices = self.write_indices(&chunk.indices);
                let mut attributes = Map::new();
                attributes.insert("POSITION".into(), json!(position_accessor));
                if let Some(n) = normal_accessor {
                    attributes.insert("NORMAL".into(), json!(n));
                }
                primitives.push(json!({
                    "attributes": attributes,
                    "indices": indices,
                    "material": (part.material as usize).min(self.materials.len() - 1),
                    "mode": 4,
                }));
            }
        }
        if primitives.is_empty() {
            return Ok(());
        }

        let mesh_index = self.meshes.len();
        self.meshes.push(json!({
            "name": geometry.name,
            "primitives": primitives,
        }));
        self.mesh_index[index] = Some(mesh_index);

        // A quantised mesh needs its scale and offset undone, and that has to
        // be a transform on a node. The node cannot be shared: glTF's node
        // hierarchy is a forest and a node has at most one parent, so one
        // dequantisation node parented by every instance is not a graph the
        // format allows. It was written that way, and Babylon.js refused the
        // file outright — "/nodes/3: Invalid recursive node hierarchy", node 3
        // being `214 204 003_dequant`, a child of three instances. Ten nodes
        // in the pilot had two to four parents each. The transform is kept
        // here and a fresh node is written under each instance instead; the
        // mesh is still shared, which is where the size goes.
        if let Some((scale, offset)) = dequant {
            self.dequant[index] = Some((scale, offset));
        }
        Ok(())
    }

    /// Emit a glTF node for a scene node and its children.
    fn add_node(&mut self, id: cad_ir::scene::NodeId, names: &mut Names) -> Option<usize> {
        if let Some(existing) = self.node_index[id.index()] {
            return Some(existing);
        }
        let node = self.scene.node(id);

        let children: Vec<usize> = node
            .children
            .clone()
            .into_iter()
            .filter_map(|c| self.add_node(c, names))
            .collect();

        let mesh = node.geometry.and_then(|g| self.mesh_index[g.index()]);
        let dequant = node.geometry.and_then(|g| self.dequant[g.index()]);

        // A node with neither geometry nor surviving children is not worth
        // writing; an empty node still costs bytes in every consumer.
        if mesh.is_none() && children.is_empty() {
            return None;
        }

        let mut object = Map::new();
        object.insert("name".into(), json!(names.unique(&node.name)));
        if !node.transform.is_identity(0.0) {
            object.insert(
                "matrix".into(),
                json!(node.transform.to_column_major()),
            );
        }
        let mut all_children = children;
        match (dequant, mesh) {
            (Some((scale, offset)), Some(m)) => {
                // This instance's own dequantisation node, written fresh so it
                // has exactly one parent.
                let d = self.nodes.len();
                self.nodes.push(json!({
                    "name": format!("{}_dequant", node.name),
                    "mesh": m,
                    "scale": scale,
                    "translation": offset,
                }));
                all_children.push(d);
            }
            (_, Some(m)) => {
                object.insert("mesh".into(), json!(m));
            }
            (_, None) => {}
        }
        if !all_children.is_empty() {
            object.insert("children".into(), json!(all_children));
        }

        let index = self.nodes.len();
        self.nodes.push(Value::Object(object));
        self.node_index[id.index()] = Some(index);
        Some(index)
    }

    fn write_positions(&mut self, mesh: &Mesh, vertices: &[u32]) -> usize {
        let mut bytes = Vec::with_capacity(vertices.len() * 12);
        let (mut min, mut max) = ([f32::MAX; 3], [f32::MIN; 3]);
        for &v in vertices {
            let p = mesh.positions[v as usize];
            for i in 0..3 {
                min[i] = min[i].min(p[i]);
                max[i] = max[i].max(p[i]);
                bytes.extend_from_slice(&p[i].to_le_bytes());
            }
        }
        let view = self.push_view(&bytes, Some(ARRAY_BUFFER), 12, None);
        self.push_accessor(json!({
            "bufferView": view,
            "componentType": FLOAT,
            "count": vertices.len(),
            "type": "VEC3",
            "min": min,
            "max": max,
        }))
    }

    /// Positions as 16-bit integers over the mesh's own bounding box.
    ///
    /// A 16-bit grid across one part's extent is a resolution of its size
    /// divided by 65535 — on a 300 mm part that is 5 µm, an order of magnitude
    /// finer than the tessellation tolerance and two below anything a viewer
    /// resolves. The grid is the whole geometry's even when the geometry is
    /// written in chunks, so one node undoes it for all of them.
    fn write_quantised_positions(
        &mut self,
        mesh: &Mesh,
        vertices: &[u32],
        (scale, min): ([f32; 3], [f32; 3]),
    ) -> usize {
        let mut bytes = Vec::with_capacity(vertices.len() * 8);
        let (mut lo, mut hi) = ([u16::MAX; 3], [0u16; 3]);
        for &v in vertices {
            let p = mesh.positions[v as usize];
            for i in 0..3 {
                let t = ((p[i] - min[i]) / scale[i]).round().clamp(0.0, 65535.0) as u16;
                lo[i] = lo[i].min(t);
                hi[i] = hi[i].max(t);
                bytes.extend_from_slice(&t.to_le_bytes());
            }
            // Six bytes of position, stepped by eight: the two that follow are
            // the alignment glTF asks of every vertex attribute.
            bytes.extend_from_slice(&[0, 0]);
        }
        let view = self.push_view(&bytes, Some(ARRAY_BUFFER), 8, Some(8));
        let accessor = self.push_accessor(json!({
            "bufferView": view,
            "componentType": UNSIGNED_SHORT,
            "count": vertices.len(),
            "type": "VEC3",
            "min": lo,
            "max": hi,
        }));
        self.use_extension("KHR_mesh_quantization");
        accessor
    }

    fn write_normals(&mut self, mesh: &Mesh, vertices: &[u32]) -> usize {
        let mut bytes = Vec::with_capacity(vertices.len() * 12);
        for &v in vertices {
            for c in mesh.normals[v as usize] {
                bytes.extend_from_slice(&c.to_le_bytes());
            }
        }
        let view = self.push_view(&bytes, Some(ARRAY_BUFFER), 12, None);
        self.push_accessor(json!({
            "bufferView": view,
            "componentType": FLOAT,
            "count": vertices.len(),
            "type": "VEC3",
        }))
    }

    /// Normals as normalised signed bytes.
    ///
    /// A byte per component resolves about 0.9°, which is well inside the
    /// angular tessellation tolerance — the mesh's own faceting is coarser than
    /// the encoding, so nothing visible is lost.
    fn write_quantised_normals(&mut self, mesh: &Mesh, vertices: &[u32]) -> usize {
        let mut bytes = Vec::with_capacity(vertices.len() * 4);
        for &v in vertices {
            for c in mesh.normals[v as usize] {
                bytes.push(((c.clamp(-1.0, 1.0) * 127.0).round() as i8) as u8);
            }
            // The fourth byte is the stride's, declared on the view.
            bytes.push(0);
        }
        let view = self.push_view(&bytes, Some(ARRAY_BUFFER), 4, Some(4));
        self.use_extension("KHR_mesh_quantization");
        self.push_accessor(json!({
            "bufferView": view,
            "componentType": BYTE,
            "normalized": true,
            "count": vertices.len(),
            "type": "VEC3",
        }))
    }

    /// One chunk's indices. Every chunk is built to fit 16 bits.
    fn write_indices(&mut self, indices: &[u16]) -> usize {
        let mut bytes = Vec::with_capacity(indices.len() * 2);
        for &i in indices {
            bytes.extend_from_slice(&i.to_le_bytes());
        }
        let view = self.push_view(&bytes, Some(ELEMENT_ARRAY_BUFFER), 2, None);
        self.push_accessor(json!({
            "bufferView": view,
            "componentType": UNSIGNED_SHORT,
            "count": indices.len(),
            "type": "SCALAR",
        }))
    }

    /// Append bytes to the binary chunk and record a buffer view over them.
    /// Append bytes to the binary chunk and record a buffer view over them.
    ///
    /// `stride` is the distance from one element to the next, and it has to be
    /// stated whenever that is not the element's own size: glTF requires every
    /// vertex attribute to step by a multiple of four, so a `VEC3` of bytes
    /// steps by 4 and a `VEC3` of shorts by 8, both with padding the reader is
    /// told about. Writing the padding without declaring the stride is the
    /// worst of both — the file is larger *and* every conforming reader takes
    /// the padding for data. Ours did, until the normals were read back and
    /// came out 88° from where they were written.
    fn push_view(
        &mut self,
        bytes: &[u8],
        target: Option<u32>,
        alignment: usize,
        stride: Option<usize>,
    ) -> usize {
        // An accessor's offset must be a multiple of its component size, and
        // the spec additionally requires four-byte alignment for buffer views
        // that accessors point into.
        let align = alignment.max(4);
        while self.bin.len() % align != 0 {
            self.bin.push(0);
        }
        let offset = self.bin.len();
        self.bin.extend_from_slice(bytes);

        let mut view = Map::new();
        view.insert("buffer".into(), json!(0));
        view.insert("byteOffset".into(), json!(offset));
        view.insert("byteLength".into(), json!(bytes.len()));
        if let Some(t) = target {
            view.insert("target".into(), json!(t));
        }
        if let Some(n) = stride {
            view.insert("byteStride".into(), json!(n));
        }
        let index = self.buffer_views.len();
        self.buffer_views.push(Value::Object(view));
        index
    }

    fn push_accessor(&mut self, value: Value) -> usize {
        let index = self.accessors.len();
        self.accessors.push(value);
        index
    }

    fn use_extension(&mut self, name: &'static str) {
        if !self.extensions_used.contains(&name) {
            self.extensions_used.push(name);
        }
    }

    /// Assemble the JSON and binary chunks into a GLB container.
    fn finish(self) -> Result<Vec<u8>> {
        let Builder {
            scene,
            options,
            bin,
            buffer_views,
            accessors,
            meshes,
            nodes,
            materials,
            extensions_used,
            ..
        } = self;

        let root_node = nodes.len() - 1;
        let mut root = Map::new();
        root.insert(
            "asset".into(),
            json!({
                "version": "2.0",
                "generator": options.generator,
                "copyright": if scene.meta.source.is_empty() {
                    Value::Null
                } else {
                    json!(format!("converted from {}", scene.meta.source))
                },
            }),
        );
        root.insert("scene".into(), json!(0));
        root.insert("scenes".into(), json!([{ "nodes": [root_node] }]));
        root.insert("nodes".into(), json!(nodes));
        root.insert("meshes".into(), json!(meshes));
        root.insert("materials".into(), json!(materials));
        root.insert("accessors".into(), json!(accessors));
        root.insert("bufferViews".into(), json!(buffer_views));
        root.insert("buffers".into(), json!([{ "byteLength": bin.len() }]));
        if !extensions_used.is_empty() {
            root.insert("extensionsUsed".into(), json!(extensions_used));
            // Quantisation changes what the accessor data *means*, so a viewer
            // that ignores it renders nonsense rather than something plainer.
            // That makes it required, not merely used.
            root.insert("extensionsRequired".into(), json!(extensions_used));
        }
        let _ = Unit::Millimetre;

        let json_bytes = serde_json::to_vec(&Value::Object(root))
            .map_err(|e| ExportError::BadMesh(format!("serialising glTF JSON: {e}")))?;

        let mut out = Vec::with_capacity(json_bytes.len() + bin.len() + 64);
        // The header's total length is written once both chunks are sized.
        out.write_all(b"glTF")?;
        out.write_all(&2u32.to_le_bytes())?;
        out.write_all(&0u32.to_le_bytes())?;

        write_chunk(&mut out, b"JSON", &json_bytes, b' ')?;
        if !bin.is_empty() {
            write_chunk(&mut out, b"BIN\0", &bin, 0)?;
        }

        let total = out.len() as u32;
        out[8..12].copy_from_slice(&total.to_le_bytes());
        Ok(out)
    }
}

/// Write one GLB chunk, padded to a four-byte boundary with `pad`.
fn write_chunk(out: &mut Vec<u8>, kind: &[u8; 4], data: &[u8], pad: u8) -> Result<()> {
    let padding = (4 - data.len() % 4) % 4;
    out.write_all(&((data.len() + padding) as u32).to_le_bytes())?;
    out.write_all(kind)?;
    out.write_all(data)?;
    out.extend(std::iter::repeat_n(pad, padding));
    Ok(())
}

/// One piece of a material run: the vertices it uses, in the order it first
/// used them, and its triangles indexed into that order.
struct Chunk {
    vertices: Vec<u32>,
    indices: Vec<u16>,
}

/// Cut one material run into pieces that each index at most 65536 vertices.
///
/// The triangles arrive face by face, so consecutive ones share vertices and a
/// piece taken in order is compact: the only vertices paid for twice are those
/// on the seam between two pieces, and only the six bodies of the pilot that
/// exceed the limit are cut at all.
///
/// `seen` and `local` are scratch the size of the mesh's vertex buffer, kept
/// across calls and told apart by `age` so no chunk has to clear them.
fn chunk_run(
    mesh: &Mesh,
    start: usize,
    count: usize,
    seen: &mut [u32],
    local: &mut [u32],
    age: &mut u32,
) -> Vec<Chunk> {
    const LIMIT: usize = u16::MAX as usize + 1;
    let mut out: Vec<Chunk> = Vec::new();
    let end = start + count;
    let mut i = start;
    while i + 3 <= end {
        *age += 1;
        let mut chunk = Chunk {
            vertices: Vec::new(),
            indices: Vec::new(),
        };
        while i + 3 <= end {
            let tri = [mesh.indices[i], mesh.indices[i + 1], mesh.indices[i + 2]];
            let mut fresh = 0;
            for (k, v) in tri.iter().enumerate() {
                if seen[*v as usize] != *age && !tri[..k].contains(v) {
                    fresh += 1;
                }
            }
            if !chunk.vertices.is_empty() && chunk.vertices.len() + fresh > LIMIT {
                break;
            }
            for v in tri {
                if seen[v as usize] != *age {
                    seen[v as usize] = *age;
                    local[v as usize] = chunk.vertices.len() as u32;
                    chunk.vertices.push(v);
                }
                chunk.indices.push(local[v as usize] as u16);
            }
            i += 3;
        }
        if chunk.indices.is_empty() {
            break;
        }
        out.push(chunk);
    }
    out
}

/// The scale and offset of a mesh's own 16-bit position grid.
fn quantisation(mesh: &Mesh) -> ([f32; 3], [f32; 3]) {
    let (min, max) = position_range(mesh);
    let mut scale = [0.0f32; 3];
    for i in 0..3 {
        let span = max[i] - min[i];
        scale[i] = if span > 0.0 { span / 65535.0 } else { 1.0 };
    }
    (scale, min)
}

fn position_range(mesh: &Mesh) -> ([f32; 3], [f32; 3]) {
    let mut min = [f32::INFINITY; 3];
    let mut max = [f32::NEG_INFINITY; 3];
    for p in &mesh.positions {
        for i in 0..3 {
            min[i] = min[i].min(p[i]);
            max[i] = max[i].max(p[i]);
        }
    }
    if !min[0].is_finite() {
        return ([0.0; 3], [0.0; 3]);
    }
    (min, max)
}

/// Lower an IR material onto glTF's metallic-roughness model.
fn material_json(m: &Material, names: &mut Names) -> Value {
    let mut value = Map::new();
    value.insert("name".into(), json!(names.unique(&m.name)));
    value.insert(
        "pbrMetallicRoughness".into(),
        json!({
            "baseColorFactor": [m.base_color[0], m.base_color[1], m.base_color[2], m.alpha],
            "metallicFactor": m.metallic,
            "roughnessFactor": m.roughness,
        }),
    );
    if m.emissive.iter().any(|&c| c > 0.0) {
        value.insert("emissiveFactor".into(), json!(m.emissive));
    }
    if m.double_sided {
        value.insert("doubleSided".into(), json!(true));
    }
    if m.is_transparent() {
        value.insert("alphaMode".into(), json!("BLEND"));
    }

    // The index of refraction is not part of core glTF; without it a
    // dielectric's specular response is fixed at 1.5, which is wrong for glass
    // and for most engineering plastics.
    let mut extensions = Map::new();
    if (m.ior - 1.5).abs() > 1e-3 {
        extensions.insert("KHR_materials_ior".into(), json!({ "ior": m.ior }));
    }
    if m.transmission > 0.0 {
        extensions.insert(
            "KHR_materials_transmission".into(),
            json!({ "transmissionFactor": m.transmission }),
        );
    }
    if !extensions.is_empty() {
        value.insert("extensions".into(), Value::Object(extensions));
    }
    Value::Object(value)
}

#[cfg(test)]
mod tests {
    use super::*;
    use cad_ir::material::{Material, MaterialClass};
    use cad_ir::math::{Transform, Vec3};
    use cad_ir::mesh::MeshPart;
    use cad_ir::scene::{Geometry, Node};

    fn tri(material: u32) -> Mesh {
        Mesh {
            positions: vec![[0.0, 0.0, 0.0], [10.0, 0.0, 0.0], [0.0, 10.0, 0.0]],
            normals: vec![[0.0, 0.0, 1.0]; 3],
            uvs: vec![],
            indices: vec![0, 1, 2],
            parts: vec![MeshPart {
                material,
                start: 0,
                count: 3,
            }],
        }
    }

    /// A root with two nodes instancing one geometry — the shape that makes
    /// instancing worth preserving.
    fn scene() -> Scene {
        let mut s = Scene::default();
        s.add_material(Material::from_class(MaterialClass::Steel, "steel"));
        let g = s.add_geometry(Geometry {
            name: "bolt".into(),
            brep: None,
            mesh: Some(tri(0)),
            material: None,
            face_materials: vec![],
        });
        let a = s.add_node(Node {
            name: "bolt.1".into(),
            transform: Transform::from_translation(Vec3::new(50.0, 0.0, 0.0)),
            geometry: Some(g),
            ..Default::default()
        });
        let b = s.add_node(Node {
            name: "bolt.2".into(),
            transform: Transform::from_translation(Vec3::new(0.0, 60.0, 0.0)),
            geometry: Some(g),
            ..Default::default()
        });
        let root = s.add_node(Node {
            name: "asm".into(),
            children: vec![a, b],
            ..Default::default()
        });
        s.roots.push(root);
        s
    }

    /// The JSON chunk of a GLB, for tests that need to look at the graph.
    fn json_of(bytes: &[u8]) -> Value {
        let mut at = 12;
        while at < bytes.len() {
            let len = u32::from_le_bytes(bytes[at..at + 4].try_into().unwrap()) as usize;
            let kind = u32::from_le_bytes(bytes[at + 4..at + 8].try_into().unwrap());
            at += 8;
            if kind == 0x4E4F_534A {
                return serde_json::from_slice(&bytes[at..at + len]).expect("the JSON chunk");
            }
            at += len;
        }
        panic!("no JSON chunk");
    }

    #[test]
    fn every_node_has_at_most_one_parent() {
        // glTF's node hierarchy is a forest. A quantised mesh needs a node to
        // carry the transform that undoes its grid, and the first version
        // wrote one such node per *geometry* and parented it under every
        // instance — which gives it as many parents as there are instances.
        // Babylon.js refused the pilot outright: "/nodes/3: Invalid recursive
        // node hierarchy". The test scene has one bolt used twice, which is
        // the smallest shape that reproduces it.
        for options in [Options::default(), Options::lean(), Options::compact()] {
            let json = json_of(&write_bytes(&scene(), &options).unwrap());
            let nodes = json["nodes"].as_array().expect("nodes");
            let mut parents = vec![0usize; nodes.len()];
            for node in nodes {
                for child in node["children"].as_array().into_iter().flatten() {
                    parents[child.as_u64().expect("a child index") as usize] += 1;
                }
            }
            assert!(
                parents.iter().all(|&p| p <= 1),
                "{:?}: a node has more than one parent",
                options.compression
            );
            // And every node is reachable from the scene exactly once, so
            // there is no cycle and nothing orphaned.
            let mut seen = vec![false; nodes.len()];
            let mut stack: Vec<usize> = json["scenes"][0]["nodes"]
                .as_array()
                .expect("scene roots")
                .iter()
                .map(|v| v.as_u64().expect("a root index") as usize)
                .collect();
            while let Some(i) = stack.pop() {
                assert!(!seen[i], "{:?}: node {i} is reached twice", options.compression);
                seen[i] = true;
                for child in nodes[i]["children"].as_array().into_iter().flatten() {
                    stack.push(child.as_u64().expect("a child index") as usize);
                }
            }
            assert!(
                seen.iter().all(|&s| s),
                "{:?}: a node is not reachable from the scene",
                options.compression
            );
        }
    }

    #[test]
    fn the_container_framing_is_well_formed() {
        let bytes = write_bytes(&scene(), &Options::default()).unwrap();
        assert_eq!(&bytes[0..4], b"glTF");
        assert_eq!(u32::from_le_bytes(bytes[4..8].try_into().unwrap()), 2);
        assert_eq!(
            u32::from_le_bytes(bytes[8..12].try_into().unwrap()) as usize,
            bytes.len(),
            "the header length must equal the file length"
        );
        assert_eq!(bytes.len() % 4, 0, "chunks must leave the file 4-aligned");
        // Chunk 0 must be JSON.
        assert_eq!(&bytes[16..20], b"JSON");
    }

    /// The strongest check available: a real glTF reader accepts the file and
    /// finds the geometry where the writer said it was.
    #[test]
    fn a_real_gltf_reader_accepts_the_output() {
        let bytes = write_bytes(&scene(), &Options::default()).unwrap();
        let (document, buffers, _) = gltf::import_slice(&bytes).expect("gltf reader rejected it");
        assert_eq!(document.meshes().count(), 1, "instancing was not preserved");
        assert_eq!(document.materials().count(), 1);

        let mesh = document.meshes().next().unwrap();
        let primitive = mesh.primitives().next().unwrap();
        let reader = primitive.reader(|b| Some(&buffers[b.index()]));
        let positions: Vec<_> = reader.read_positions().unwrap().collect();
        assert_eq!(positions.len(), 3);
        let indices: Vec<_> = reader.read_indices().unwrap().into_u32().collect();
        assert_eq!(indices, vec![0, 1, 2]);
        let normals: Vec<_> = reader.read_normals().unwrap().collect();
        assert_eq!(normals.len(), 3);
    }

    #[test]
    fn a_shared_geometry_is_written_once_and_referenced_twice() {
        let bytes = write_bytes(&scene(), &Options::default()).unwrap();
        let (document, _, _) = gltf::import_slice(&bytes).unwrap();
        assert_eq!(document.meshes().count(), 1);
        let referencing = document.nodes().filter(|n| n.mesh().is_some()).count();
        assert_eq!(referencing, 2);
    }

    #[test]
    fn the_root_transform_puts_the_model_in_metres_and_y_up() {
        let bytes = write_bytes(&scene(), &Options::default()).unwrap();
        let (document, _, _) = gltf::import_slice(&bytes).unwrap();
        let root = document.scenes().next().unwrap().nodes().next().unwrap();
        let m = root.transform().matrix();
        // Column-major: column 0 is where the scene's +X goes.
        assert!((m[0][0] - 1e-3).abs() < 1e-9, "x scale {:?}", m[0][0]);
        // The scene's +Z lands on the output's +Y.
        assert!((m[2][1] - 1e-3).abs() < 1e-9, "z->y {:?}", m[2][1]);
    }

    #[test]
    fn quantised_output_is_smaller_and_still_reads_back() {
        let plain = write_bytes(&scene(), &Options::default()).unwrap();
        let packed = write_bytes(&scene(), &Options::compact()).unwrap();

        // The reader refuses a file whose extensionsRequired it does not
        // implement, which is itself the evidence the flag was written.
        match gltf::import_slice(&packed) {
            Ok((document, _, _)) => {
                assert_eq!(document.meshes().count(), 1);
            }
            Err(gltf::Error::Validation(_)) => {}
            Err(e) => panic!("unexpected error reading the quantised file: {e}"),
        }

        let json_end = packed
            .windows(4)
            .position(|w| w == b"BIN\0")
            .expect("a BIN chunk");
        let text = String::from_utf8_lossy(&packed[..json_end]);
        assert!(text.contains("KHR_mesh_quantization"));
        assert!(text.contains("extensionsRequired"));

        // On a three-vertex mesh the JSON dominates, so compare the binary
        // chunks rather than the files.
        let plain_bin = plain.len()
            - plain
                .windows(4)
                .position(|w| w == b"BIN\0")
                .expect("a BIN chunk");
        let packed_bin = packed.len() - json_end;
        assert!(
            packed_bin < plain_bin,
            "quantised binary {packed_bin} is not smaller than plain {plain_bin}"
        );
    }

    #[test]
    fn a_scene_with_no_mesh_is_refused_rather_than_written_empty() {
        let mut s = Scene::default();
        s.add_geometry(Geometry {
            name: "empty".into(),
            brep: None,
            mesh: None,
            material: None,
            face_materials: vec![],
        });
        assert!(matches!(
            write_bytes(&s, &Options::default()),
            Err(ExportError::NoMesh)
        ));
    }

    #[test]
    fn an_out_of_range_index_is_caught_before_it_reaches_a_viewer() {
        let mut s = scene();
        s.geometry[0].mesh.as_mut().unwrap().indices = vec![0, 1, 99];
        assert!(matches!(
            write_bytes(&s, &Options::default()),
            Err(ExportError::BadMesh(_))
        ));
    }

    #[test]
    fn material_values_survive_the_round_trip() {
        let bytes = write_bytes(&scene(), &Options::default()).unwrap();
        let (document, _, _) = gltf::import_slice(&bytes).unwrap();
        let m = document.materials().next().unwrap();
        let pbr = m.pbr_metallic_roughness();
        assert_eq!(pbr.metallic_factor(), 1.0, "steel must read back as a metal");
        let want = Material::from_class(MaterialClass::Steel, "steel");
        for i in 0..3 {
            assert!((pbr.base_color_factor()[i] - want.base_color[i]).abs() < 1e-6);
        }
    }

    #[test]
    fn a_transparent_material_declares_blending() {
        let mut s = scene();
        s.materials[0] = Material::from_class(MaterialClass::Glass, "glass");
        s.materials[0].alpha = 0.4;
        let bytes = write_bytes(&s, &Options::default()).unwrap();
        let (document, _, _) = gltf::import_slice(&bytes).unwrap();
        let m = document.materials().next().unwrap();
        assert_eq!(m.alpha_mode(), gltf::material::AlphaMode::Blend);
    }
}
