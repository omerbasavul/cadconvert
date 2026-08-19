//! Recovering the assembly tree, and lowering a whole STEP file into a
//! [`cad_ir::Scene`].
//!
//! AP214 spells a parent-child placement across five linked entities, and none
//! of them holds both ends:
//!
//! ```text
//! NEXT_ASSEMBLY_USAGE_OCCURRENCE(…, parent_pd, child_pd, …)   who contains whom
//! PRODUCT_DEFINITION_SHAPE(…, definition -> the NAUO)          names that occurrence
//! CONTEXT_DEPENDENT_SHAPE_REPRESENTATION(rel, that PDS)        ties it to geometry
//!   rel = (REPRESENTATION_RELATIONSHIP(…, rep_1, rep_2)
//!          REPRESENTATION_RELATIONSHIP_WITH_TRANSFORMATION(idt)
//!          SHAPE_REPRESENTATION_RELATIONSHIP())               a complex instance
//! ITEM_DEFINED_TRANSFORMATION(…, placement_1, placement_2)     the actual pose
//! ```
//!
//! The transform maps `rep_1`'s space into `rep_2`'s, so the child's placement
//! is `M(placement_2) · M(placement_1)⁻¹`. Exporters do write a non-identity
//! `placement_1`, so dropping the inverse — which is tempting because it is
//! usually the identity — misplaces exactly the instances that are hardest to
//! notice.

use crate::error::Result;
use crate::kind::Kind;
use crate::lower::geom;
use crate::lower::topo::{self, SolidBuilder};
use crate::{presentation, units, StepFile};
use cad_ir::brep::FaceId;
use cad_ir::material::Material;
use cad_ir::math::Transform;
use cad_ir::scene::{Geometry, GeometryId, MaterialId, Meta, Node, NodeId, Scene, Unit};
use rustc_hash::{FxHashMap, FxHashSet};

/// What the lowering produced besides the scene itself.
#[derive(Debug, Default, Clone)]
pub struct Report {
    /// Sub-entities dropped, with why.
    pub skipped: Vec<topo::Skip>,
    /// Complaints from [`topo::diagnose`], per geometry name.
    pub diagnostics: Vec<(String, Vec<String>)>,
    /// Styled items whose chain yielded no colour.
    pub unresolved_styles: usize,
    /// Products that resolved to no geometry at all.
    pub empty_products: Vec<String>,
}

/// Lower a whole STEP file into a scene.
pub fn to_scene(file: &StepFile) -> Result<(Scene, Report)> {
    let units = units::resolve(file)?;
    let styles = presentation::resolve(file)?;
    let mut report = Report {
        unresolved_styles: styles.unresolved.len(),
        ..Default::default()
    };

    let mut scene = Scene {
        meta: Meta {
            source: file.file_name(),
            authoring_tool: file.originating_system(),
            unit: Unit::Millimetre,
            tolerance: units.uncertainty * units.length_to_mm,
        },
        ..Default::default()
    };

    let index = Index::build(file)?;

    // One node per product definition, geometry attached where it has any.
    let mut node_of_pd: FxHashMap<u32, NodeId> = FxHashMap::default();
    for &pd in &index.product_definitions {
        let name = index.product_name(file, pd)?;
        let geometry = build_geometry(file, &index, &styles, &units, pd, &name, &mut scene, &mut report)?;
        if geometry.is_none() && !index.has_children(pd) {
            report.empty_products.push(name.clone());
        }
        let id = scene.add_node(Node {
            name,
            transform: Transform::IDENTITY,
            children: Vec::new(),
            geometry,
            material: None,
        });
        node_of_pd.insert(pd, id);
    }

    // Wire the tree from the usage occurrences.
    let mut is_child: FxHashSet<u32> = FxHashSet::default();
    for occ in &index.occurrences {
        let (Some(&parent), Some(&child)) =
            (node_of_pd.get(&occ.parent_pd), node_of_pd.get(&occ.child_pd))
        else {
            continue;
        };
        is_child.insert(occ.child_pd);
        // A product used more than once needs a node per placement; the first
        // use takes the shared node and later ones get a copy, so instancing
        // survives without two placements fighting over one transform.
        let node = if scene.node(child).transform.is_identity(0.0)
            && !scene.node(child).children.is_empty()
            || occ.first_use
        {
            scene.nodes[child.index()].transform = occ.transform;
            child
        } else {
            let template = scene.node(child).clone();
            scene.add_node(Node {
                transform: occ.transform,
                ..template
            })
        };
        scene.nodes[parent.index()].children.push(node);
    }

    scene.roots = index
        .product_definitions
        .iter()
        .filter(|pd| !is_child.contains(pd))
        .filter_map(|pd| node_of_pd.get(pd).copied())
        .collect();
    // A file whose occurrences form a cycle would leave no root; falling back
    // to every node keeps its geometry reachable instead of emitting nothing.
    if scene.roots.is_empty() {
        scene.roots = node_of_pd.values().copied().collect();
        scene.roots.sort();
    }

    if units.length_to_mm != 1.0 {
        scale_scene(&mut scene, units.length_to_mm);
    }

    Ok((scene, report))
}

/// Build the geometry for one product definition, if it has any.
fn build_geometry(
    file: &StepFile,
    index: &Index,
    styles: &presentation::Styles,
    units: &units::Units,
    pd: u32,
    name: &str,
    scene: &mut Scene,
    report: &mut Report,
) -> Result<Option<GeometryId>> {
    let reps = index.shape_representations(pd);
    if reps.is_empty() {
        return Ok(None);
    }

    let mut builder = SolidBuilder::new(file, units.uncertainty);
    let mut any = false;
    let mut solid_ids: Vec<u32> = Vec::new();
    for rep in reps {
        let Ok(mut a) = file.args(rep) else { continue };
        if a.skip().is_err() {
            continue;
        }
        let mut items = Vec::new();
        if a.next_ref_list(&mut items).is_err() {
            continue;
        }
        for item in items {
            if !topo::is_shape_item(file.kind_of(item)) {
                continue;
            }
            if builder.add_item(item)? {
                any = true;
                solid_ids.push(item);
            }
        }
    }
    if !any {
        return Ok(None);
    }

    let face_sources = builder.face_sources();
    let (mut solid, skipped) = builder.finish();
    solid.name = name.to_string();
    report.skipped.extend(skipped);
    let complaints = topo::diagnose(&solid);
    if !complaints.is_empty() {
        report.diagnostics.push((name.to_string(), complaints));
    }

    // Per-face materials, falling back outward: the face's own style, then the
    // solid's, then the representation's.
    let mut face_materials = vec![None; solid.faces.len()];
    let mut assigned = 0usize;
    for (fid, step_id) in &face_sources {
        let mut chain = vec![*step_id];
        chain.extend(solid_ids.iter().copied());
        chain.extend(index.shape_representations(pd));
        if let Some(app) = styles.lookup(chain) {
            let m = material_for(app, name);
            let id = scene.add_material(m);
            if let Some(slot) = face_materials.get_mut(fid.index()) {
                *slot = Some(id);
                assigned += 1;
            }
        }
    }

    // Where every face landed on the same material, hoist it: one material for
    // the geometry is one draw call, and the writers can skip per-face runs.
    let uniform = face_materials
        .first()
        .copied()
        .flatten()
        .filter(|first| face_materials.iter().all(|m| *m == Some(*first)));
    let (material, face_materials) = match uniform {
        Some(m) => (Some(m), Vec::new()),
        None if assigned == 0 => (Some(scene.add_material(Material::unknown())), Vec::new()),
        None => (None, face_materials),
    };

    let gid = scene.add_geometry(Geometry {
        name: name.to_string(),
        brep: Some(solid),
        mesh: None,
        material,
        face_materials,
    });
    Ok(Some(gid))
}

/// Turn a resolved appearance into a material.
///
/// STEP carries no engineering material, so this is the colour-only rung of the
/// fallback ladder; a name from a sidecar or a native file is applied later,
/// over the top of this.
fn material_for(app: presentation::Appearance, part: &str) -> Material {
    let mut m = Material::from_colour(format!("{part}-{}", app.srgb_hex()), app.linear_rgb(), app.alpha);
    m.name = format!("colour-{}", app.srgb_hex());
    m
}

/// Scale every geometry and placement from file units into millimetres.
fn scale_scene(scene: &mut Scene, factor: f64) {
    for g in &mut scene.geometry {
        let Some(solid) = g.brep.as_mut() else { continue };
        for v in &mut solid.vertices {
            *v = *v * factor;
        }
        for c in &mut solid.curves {
            scale_curve(c, factor);
        }
        for s in &mut solid.surfaces {
            scale_surface(s, factor);
        }
        solid.tolerance *= factor;
        for e in &mut solid.edges {
            e.tolerance *= factor;
        }
    }
    for n in &mut scene.nodes {
        for r in 0..3 {
            n.transform.m[r][3] *= factor;
        }
    }
}

fn scale_curve(c: &mut cad_ir::brep::Curve, f: f64) {
    use cad_ir::brep::Curve::*;
    match c {
        Line { origin, direction } => {
            *origin = *origin * f;
            *direction = *direction * f;
        }
        Circle { frame, radius } => {
            frame.origin = frame.origin * f;
            *radius *= f;
        }
        Ellipse {
            frame,
            semi_major,
            semi_minor,
        } => {
            frame.origin = frame.origin * f;
            *semi_major *= f;
            *semi_minor *= f;
        }
        Parabola { frame, focal_dist } => {
            frame.origin = frame.origin * f;
            *focal_dist *= f;
        }
        Hyperbola {
            frame,
            semi_major,
            semi_minor,
        } => {
            frame.origin = frame.origin * f;
            *semi_major *= f;
            *semi_minor *= f;
        }
        Polyline { points } => points.iter_mut().for_each(|p| *p = *p * f),
        Nurbs(n) => n.control_points.iter_mut().for_each(|p| *p = *p * f),
        Trimmed { base, .. } => scale_curve(base, f),
        Composite { segments } => segments.iter_mut().for_each(|s| scale_curve(&mut s.curve, f)),
        OnSurface { .. } => {}
    }
}

fn scale_surface(s: &mut cad_ir::brep::Surface, f: f64) {
    use cad_ir::brep::Surface::*;
    match s {
        Plane { frame } => frame.origin = frame.origin * f,
        Cylinder { frame, radius } => {
            frame.origin = frame.origin * f;
            *radius *= f;
        }
        Cone { frame, radius, .. } => {
            frame.origin = frame.origin * f;
            *radius *= f;
        }
        Sphere { frame, radius } => {
            frame.origin = frame.origin * f;
            *radius *= f;
        }
        Torus {
            frame,
            major_radius,
            minor_radius,
        } => {
            frame.origin = frame.origin * f;
            *major_radius *= f;
            *minor_radius *= f;
        }
        Nurbs(n) => n
            .control_points
            .iter_mut()
            .for_each(|row| row.iter_mut().for_each(|p| *p = *p * f)),
        LinearExtrusion { profile, direction } => {
            scale_curve(profile, f);
            *direction = *direction * f;
        }
        Revolution { profile, frame } => {
            scale_curve(profile, f);
            frame.origin = frame.origin * f;
        }
        Offset { base, distance } => {
            scale_surface(base, f);
            *distance *= f;
        }
        RectangularTrimmed { base, .. } => scale_surface(base, f),
    }
}

/// One parent-child placement.
struct Occurrence {
    parent_pd: u32,
    child_pd: u32,
    transform: Transform,
    /// True for a product's first appearance, which takes the shared node.
    first_use: bool,
}

/// Cross-references built once, so lowering does not rescan the file per query.
struct Index {
    product_definitions: Vec<u32>,
    /// PRODUCT_DEFINITION → its PRODUCT.
    product_of_pd: FxHashMap<u32, u32>,
    /// PRODUCT_DEFINITION → the shape representations holding its geometry.
    reps_of_pd: FxHashMap<u32, Vec<u32>>,
    occurrences: Vec<Occurrence>,
}

impl Index {
    fn build(file: &StepFile) -> Result<Index> {
        let mut product_definitions = Vec::new();
        let mut product_of_pd = FxHashMap::default();
        for e in file.by_kind(Kind::ProductDefinition) {
            product_definitions.push(e.id);
            let mut a = file.args_of(e);
            a.skip_n(2)?; // id, description
            if let Ok(formation) = a.next_ref()
                && let Ok(mut fa) = file.args(formation)
                && fa.skip_n(2).is_ok()
                && let Ok(product) = fa.next_ref()
            {
                product_of_pd.insert(e.id, product);
            }
        }

        // PRODUCT_DEFINITION_SHAPE → what it defines (a PD, or a NAUO).
        let mut pds_definition: FxHashMap<u32, u32> = FxHashMap::default();
        for e in file.by_kind(Kind::ProductDefinitionShape) {
            let mut a = file.args_of(e);
            if a.skip_n(2).is_ok()
                && let Ok(def) = a.next_ref()
            {
                pds_definition.insert(e.id, def);
            }
        }

        // SHAPE_DEFINITION_REPRESENTATION(definition, used_representation).
        let mut reps_of_pd: FxHashMap<u32, Vec<u32>> = FxHashMap::default();
        for e in file.by_kind(Kind::ShapeDefinitionRepresentation) {
            let mut a = file.args_of(e);
            let (Ok(def), Ok(rep)) = (a.next_ref(), a.next_ref()) else {
                continue;
            };
            let Some(&target) = pds_definition.get(&def) else {
                continue;
            };
            if file.kind_of(target) != Kind::ProductDefinition {
                continue;
            }
            let entry = reps_of_pd.entry(target).or_default();
            entry.push(rep);
            // The geometry usually lives in a related ADVANCED_BREP_SHAPE_-
            // REPRESENTATION rather than the SHAPE_REPRESENTATION named here.
            for related in related_representations(file, rep) {
                if !entry.contains(&related) {
                    entry.push(related);
                }
            }
        }

        // Placements, keyed by the NAUO they belong to.
        let mut transform_of_nauo: FxHashMap<u32, Transform> = FxHashMap::default();
        for e in file.by_kind(Kind::ContextDependentShapeRepresentation) {
            let mut a = file.args_of(e);
            let (Ok(rel), Ok(product_rel)) = (a.next_ref(), a.next_ref()) else {
                continue;
            };
            let Some(&nauo) = pds_definition.get(&product_rel) else {
                continue;
            };
            if let Some(t) = transform_from_relationship(file, rel)? {
                transform_of_nauo.insert(nauo, t);
            }
        }

        let mut occurrences = Vec::new();
        let mut seen_child: FxHashSet<u32> = FxHashSet::default();
        for e in file.by_kind(Kind::NextAssemblyUsageOccurrence) {
            let mut a = file.args_of(e);
            if a.skip_n(3).is_err() {
                continue;
            }
            let (Ok(parent_pd), Ok(child_pd)) = (a.next_ref(), a.next_ref()) else {
                continue;
            };
            occurrences.push(Occurrence {
                parent_pd,
                child_pd,
                transform: transform_of_nauo
                    .get(&e.id)
                    .copied()
                    .unwrap_or(Transform::IDENTITY),
                first_use: seen_child.insert(child_pd),
            });
        }

        Ok(Index {
            product_definitions,
            product_of_pd,
            reps_of_pd,
            occurrences,
        })
    }

    fn product_name(&self, file: &StepFile, pd: u32) -> Result<String> {
        let Some(&product) = self.product_of_pd.get(&pd) else {
            return Ok(format!("product-{pd}"));
        };
        let mut a = file.args(product)?;
        let id = a.next_str()?.into_owned();
        let name = a.next_str()?.into_owned();
        Ok(if name.is_empty() || name == id {
            id
        } else {
            format!("{id} {name}")
        })
    }

    fn shape_representations(&self, pd: u32) -> Vec<u32> {
        self.reps_of_pd.get(&pd).cloned().unwrap_or_default()
    }

    fn has_children(&self, pd: u32) -> bool {
        self.occurrences.iter().any(|o| o.parent_pd == pd)
    }
}

/// Representations linked to `rep` by a `SHAPE_REPRESENTATION_RELATIONSHIP`.
fn related_representations(file: &StepFile, rep: u32) -> Vec<u32> {
    let mut out = Vec::new();
    for e in file.by_kind(Kind::ShapeRepresentationRelationship) {
        let mut a = file.args_of(e);
        if a.skip_n(2).is_err() {
            continue;
        }
        let (Ok(r1), Ok(r2)) = (a.next_ref(), a.next_ref()) else {
            continue;
        };
        if r1 == rep {
            out.push(r2);
        } else if r2 == rep {
            out.push(r1);
        }
    }
    out
}

/// The placement carried by a representation relationship, if it has one.
fn transform_from_relationship(file: &StepFile, rel: u32) -> Result<Option<Transform>> {
    let e = file.require(rel)?;

    // The transformation operator lives on the …_WITH_TRANSFORMATION subtype,
    // which in a complex instance is one part among several.
    let operator = if e.kind == Kind::Complex {
        match file.complex_part(e, Kind::RepresentationRelationshipWithTransformation)? {
            Some(mut a) => a.next_ref().ok(),
            None => None,
        }
    } else if e.kind == Kind::RepresentationRelationshipWithTransformation {
        let mut a = file.args_of(e);
        // Simple form lists the inherited attributes first.
        a.skip_n(4).ok().and_then(|_| a.next_ref().ok())
    } else {
        None
    };

    let Some(operator) = operator else {
        return Ok(None);
    };
    if file.kind_of(operator) != Kind::ItemDefinedTransformation {
        return Ok(None);
    }

    let mut a = file.args(operator)?;
    a.skip_n(2)?; // name, description
    let (Ok(item1), Ok(item2)) = (a.next_ref(), a.next_ref()) else {
        return Ok(None);
    };
    let f1 = geom::placement(file, item1)?;
    let f2 = geom::placement(file, item2)?;
    let m1 = Transform::from_frame(&f1);
    let m2 = Transform::from_frame(&f2);
    // Map out of rep_1's frame and into rep_2's.
    Ok(m1.try_inverse().map(|inv| inv.then(&m2)))
}

/// Face-to-material assignment, exposed for callers that re-resolve styles.
pub type FaceMaterials = Vec<Option<MaterialId>>;

/// The face ids a geometry's `face_materials` is indexed by.
pub fn face_ids(count: usize) -> impl Iterator<Item = FaceId> {
    (0..count).map(|i| FaceId(i as u32))
}
