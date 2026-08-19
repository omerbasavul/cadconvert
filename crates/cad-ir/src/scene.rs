//! The whole conversion: an assembly tree over shared geometry and materials.
//!
//! A [`Scene`] is what a reader returns and what a writer takes. Nodes form the
//! assembly tree and hold the transforms; geometry and materials live in flat
//! arenas so an assembly that places the same bolt eighty times stores one
//! bolt. Preserving that sharing all the way to the writer is the single
//! largest file-size lever in the pipeline, so instancing is in the model from
//! the start rather than recovered later by comparing meshes.

use crate::brep::Solid;
use crate::material::Material;
use crate::math::{Aabb, Transform};
use crate::mesh::Mesh;

/// Index into [`Scene::nodes`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct NodeId(pub u32);

/// Index into [`Scene::geometry`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct GeometryId(pub u32);

/// Index into [`Scene::materials`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct MaterialId(pub u32);

impl NodeId {
    pub fn index(self) -> usize {
        self.0 as usize
    }
}
impl GeometryId {
    pub fn index(self) -> usize {
        self.0 as usize
    }
}
impl MaterialId {
    pub fn index(self) -> usize {
        self.0 as usize
    }
}

/// A converted model.
#[derive(Debug, Clone, Default)]
pub struct Scene {
    pub nodes: Vec<Node>,
    /// Nodes with no parent.
    pub roots: Vec<NodeId>,
    pub geometry: Vec<Geometry>,
    pub materials: Vec<Material>,
    pub meta: Meta,
}

/// One placement in the assembly tree.
#[derive(Debug, Clone, Default)]
pub struct Node {
    pub name: String,
    /// Placement relative to the parent.
    pub transform: Transform,
    pub children: Vec<NodeId>,
    /// The geometry instanced here, if any. A node may be a pure grouping node.
    pub geometry: Option<GeometryId>,
    /// Overrides the geometry's own material assignment for this instance.
    ///
    /// Assemblies do colour the same part differently in different places.
    pub material: Option<MaterialId>,
}

/// A geometry payload, before or after tessellation.
#[derive(Debug, Clone)]
pub struct Geometry {
    pub name: String,
    /// The exact boundary representation, when a reader produced one.
    pub brep: Option<Solid>,
    /// The tessellated form. Filled by the tessellator; the writers need it.
    pub mesh: Option<Mesh>,
    /// Material for the whole geometry, when it is not assigned per face.
    pub material: Option<MaterialId>,
    /// Per-face material, indexed by [`crate::brep::FaceId`]. Empty when the
    /// whole geometry shares one material.
    pub face_materials: Vec<Option<MaterialId>>,
}

/// Provenance and unit information for the whole conversion.
#[derive(Debug, Clone)]
pub struct Meta {
    /// Source file path or name.
    pub source: String,
    /// The CAD system that wrote the source.
    pub authoring_tool: String,
    /// The unit every coordinate in this scene is expressed in.
    ///
    /// Readers normalise to millimetres. Writers convert to what their format
    /// wants — glTF and USD are both metres — so nothing downstream has to
    /// carry a scale factor around.
    pub unit: Unit,
    /// The source's modelling tolerance in scene units.
    pub tolerance: f64,
}

impl Default for Meta {
    fn default() -> Self {
        Meta {
            source: String::new(),
            authoring_tool: String::new(),
            unit: Unit::Millimetre,
            tolerance: 1e-3,
        }
    }
}

/// The length unit a scene's coordinates are in.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Unit {
    #[default]
    Millimetre,
    Metre,
}

impl Unit {
    /// Multiplier converting this unit to metres.
    pub fn to_metres(self) -> f64 {
        match self {
            Unit::Millimetre => 1e-3,
            Unit::Metre => 1.0,
        }
    }
}

impl Scene {
    /// Add a node and return its id.
    pub fn add_node(&mut self, node: Node) -> NodeId {
        let id = NodeId(self.nodes.len() as u32);
        self.nodes.push(node);
        id
    }

    /// Add a geometry payload and return its id.
    pub fn add_geometry(&mut self, geometry: Geometry) -> GeometryId {
        let id = GeometryId(self.geometry.len() as u32);
        self.geometry.push(geometry);
        id
    }

    /// Add a material, reusing an identical existing one.
    pub fn add_material(&mut self, material: Material) -> MaterialId {
        let key = material.dedup_key();
        if let Some(i) = self.materials.iter().position(|m| m.dedup_key() == key) {
            return MaterialId(i as u32);
        }
        let id = MaterialId(self.materials.len() as u32);
        self.materials.push(material);
        id
    }

    pub fn node(&self, id: NodeId) -> &Node {
        &self.nodes[id.index()]
    }

    pub fn geometry_of(&self, id: GeometryId) -> &Geometry {
        &self.geometry[id.index()]
    }

    pub fn material_of(&self, id: MaterialId) -> &Material {
        &self.materials[id.index()]
    }

    /// Walk the tree depth-first, yielding each node with its world transform.
    ///
    /// Iterative rather than recursive: an assembly tree's depth is data, and a
    /// pathological file must not overflow the stack.
    pub fn walk(&self) -> Vec<(NodeId, Transform)> {
        let mut out = Vec::with_capacity(self.nodes.len());
        let mut stack: Vec<(NodeId, Transform)> = self
            .roots
            .iter()
            .rev()
            .map(|&r| (r, Transform::IDENTITY))
            .collect();
        // A malformed file can make the tree a graph; visiting a node twice
        // would loop forever, so each is expanded at most once.
        let mut seen = vec![false; self.nodes.len()];
        while let Some((id, parent)) = stack.pop() {
            if std::mem::replace(&mut seen[id.index()], true) {
                continue;
            }
            let node = self.node(id);
            let world = node.transform.then(&parent);
            out.push((id, world));
            for &c in node.children.iter().rev() {
                stack.push((c, world));
            }
        }
        out
    }

    /// Every geometry instance in the scene with its world transform.
    pub fn instances(&self) -> Vec<Instance> {
        self.walk()
            .into_iter()
            .filter_map(|(id, world)| {
                let node = self.node(id);
                node.geometry.map(|geometry| Instance {
                    node: id,
                    geometry,
                    transform: world,
                    material: node.material,
                })
            })
            .collect()
    }

    /// World-space bounds taken from topological vertices only.
    ///
    /// The scale a relative tessellation tolerance should be measured against:
    /// it is available before any mesh exists, and unlike [`Scene::bounds`] it
    /// cannot be dragged out by a far-flung spline control point. Falls back to
    /// [`Scene::bounds`] for a scene whose bodies have no vertices at all.
    pub fn vertex_bounds(&self) -> Aabb {
        let mut b = Aabb::EMPTY;
        for inst in self.instances() {
            let g = self.geometry_of(inst.geometry);
            if let Some(solid) = &g.brep {
                let local = solid.vertex_bounds();
                if !local.is_empty() {
                    b = b.union(&local.transformed(&inst.transform));
                }
            }
        }
        if b.is_empty() { self.bounds() } else { b }
    }

    /// World-space bounds of every tessellated instance.
    pub fn bounds(&self) -> Aabb {
        let mut b = Aabb::EMPTY;
        for inst in self.instances() {
            let g = self.geometry_of(inst.geometry);
            let local = match (&g.mesh, &g.brep) {
                (Some(m), _) => m.bounds(),
                (None, Some(s)) => s.rough_bounds(),
                (None, None) => continue,
            };
            b = b.union(&local.transformed(&inst.transform));
        }
        b
    }

    /// Total triangles across every instance, counting repeats.
    pub fn triangle_count(&self) -> usize {
        self.instances()
            .iter()
            .filter_map(|i| self.geometry_of(i.geometry).mesh.as_ref())
            .map(Mesh::triangle_count)
            .sum()
    }

    /// Triangles stored, counting each shared geometry once.
    ///
    /// The gap between this and [`Scene::triangle_count`] is exactly what
    /// instancing saves.
    pub fn stored_triangle_count(&self) -> usize {
        self.geometry
            .iter()
            .filter_map(|g| g.mesh.as_ref())
            .map(Mesh::triangle_count)
            .sum()
    }
}

/// One placed geometry, flattened out of the tree.
#[derive(Debug, Clone, Copy)]
pub struct Instance {
    pub node: NodeId,
    pub geometry: GeometryId,
    pub transform: Transform,
    /// The node's material override, if it has one.
    pub material: Option<MaterialId>,
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::material::Material;
    use crate::math::Vec3;
    use crate::mesh::{Mesh, MeshPart};

    fn unit_tri() -> Mesh {
        Mesh {
            positions: vec![[0.0, 0.0, 0.0], [1.0, 0.0, 0.0], [0.0, 1.0, 0.0]],
            normals: vec![],
            uvs: vec![],
            indices: vec![0, 1, 2],
            parts: vec![MeshPart {
                material: 0,
                start: 0,
                count: 3,
            }],
        }
    }

    /// Root → two children, each instancing the same geometry at a different
    /// place. This is the shape every real assembly has.
    fn assembly() -> Scene {
        let mut s = Scene::default();
        let g = s.add_geometry(Geometry {
            name: "bolt".into(),
            brep: None,
            mesh: Some(unit_tri()),
            material: None,
            face_materials: vec![],
        });
        let a = s.add_node(Node {
            name: "bolt.1".into(),
            transform: Transform::from_translation(Vec3::new(10.0, 0.0, 0.0)),
            geometry: Some(g),
            ..Default::default()
        });
        let b = s.add_node(Node {
            name: "bolt.2".into(),
            transform: Transform::from_translation(Vec3::new(0.0, 20.0, 0.0)),
            geometry: Some(g),
            ..Default::default()
        });
        let root = s.add_node(Node {
            name: "asm".into(),
            transform: Transform::from_translation(Vec3::new(0.0, 0.0, 5.0)),
            children: vec![a, b],
            ..Default::default()
        });
        s.roots.push(root);
        s
    }

    #[test]
    fn walk_composes_transforms_down_the_tree() {
        let s = assembly();
        let walked = s.walk();
        assert_eq!(walked.len(), 3);
        let by_name: Vec<_> = walked
            .iter()
            .map(|(id, t)| (s.node(*id).name.as_str(), t.point(Vec3::ZERO)))
            .collect();
        assert_eq!(by_name[0].0, "asm");
        assert_eq!(by_name[0].1, Vec3::new(0.0, 0.0, 5.0));
        // The child's own translation composes with the root's.
        let bolt1 = by_name.iter().find(|(n, _)| *n == "bolt.1").unwrap();
        assert_eq!(bolt1.1, Vec3::new(10.0, 0.0, 5.0));
    }

    #[test]
    fn walk_terminates_on_a_cyclic_tree() {
        let mut s = assembly();
        // Point a child back at the root, which a malformed file can do.
        let root = s.roots[0];
        let child = s.node(root).children[0];
        s.nodes[child.index()].children.push(root);
        assert_eq!(s.walk().len(), 3);
    }

    #[test]
    fn instances_flatten_shared_geometry() {
        let s = assembly();
        let inst = s.instances();
        assert_eq!(inst.len(), 2);
        assert!(inst.iter().all(|i| i.geometry == GeometryId(0)));
        assert_eq!(s.triangle_count(), 2);
        assert_eq!(s.stored_triangle_count(), 1);
    }

    #[test]
    fn bounds_cover_every_placement() {
        let b = assembly().bounds();
        assert_eq!(b.min, Vec3::new(0.0, 0.0, 5.0));
        assert_eq!(b.max, Vec3::new(11.0, 21.0, 5.0));
    }

    #[test]
    fn identical_materials_are_deduplicated() {
        let mut s = Scene::default();
        let a = s.add_material(Material::unknown());
        let b = s.add_material(Material::unknown());
        assert_eq!(a, b);
        assert_eq!(s.materials.len(), 1);

        let mut other = Material::unknown();
        other.name = "steel".into();
        assert_ne!(s.add_material(other), a);
        assert_eq!(s.materials.len(), 2);
    }

    #[test]
    fn units_convert_to_metres() {
        assert_eq!(Unit::Millimetre.to_metres(), 1e-3);
        assert_eq!(Unit::Metre.to_metres(), 1.0);
    }

    #[test]
    fn an_empty_scene_has_empty_bounds_and_no_instances() {
        let s = Scene::default();
        assert!(s.bounds().is_empty());
        assert!(s.instances().is_empty());
        assert_eq!(s.triangle_count(), 0);
    }
}
