//! Lowers a Parasolid XT file into the format-neutral [`cad_ir`] scene.
//!
//! The standalone Parasolid path: `xt-parser` turns the token stream into raw
//! entities, this crate walks their topology chains and attribute graph into
//! the same [`Scene`] the STEP reader produces, and the shared tessellator and
//! writers take it from there. Nothing here needs a STEP twin — and for
//! appearance the dependency runs the other way: the XT file is the richer
//! source, carrying per-face colour *and* the designer's metal-vs-matte
//! reflectivity flags that STEP drops.

#![forbid(unsafe_code)]

pub mod geom;
pub mod topo;

use cad_ir::material_resolve::{ColourEvidence, MaterialResolver};
use cad_ir::math::Transform;
use cad_ir::scene::{Geometry, GeometryId, MaterialId, Meta, Node, NodeId, Scene, Unit};
use rustc_hash::FxHashMap;
use xt_parser::appearance;

/// Options for lowering.
#[derive(Debug, Clone, Default)]
pub struct LowerOptions {
    pub materials: MaterialResolver,
}

/// What the lowering produced besides the scene.
#[derive(Debug, Default, Clone)]
pub struct Report {
    pub skipped: Vec<topo::Skip>,
    /// Complaints per body name.
    pub diagnostics: Vec<(String, Vec<String>)>,
    /// Whether the entity stream stopped early, and where.
    pub truncated: Option<String>,
}

/// Errors that prevent lowering entirely.
#[derive(Debug, thiserror::Error)]
pub enum XtSceneError {
    #[error(transparent)]
    Parse(#[from] xt_parser::XtError),
    #[error("io error reading {path}: {source}")]
    Io {
        path: std::path::PathBuf,
        #[source]
        source: std::io::Error,
    },
}

/// Lower an XT file from disk.
pub fn scene_from_file<P: AsRef<std::path::Path>>(
    path: P,
    options: &LowerOptions,
) -> Result<(Scene, Report), XtSceneError> {
    let path = path.as_ref();
    let bytes = std::fs::read(path).map_err(|source| XtSceneError::Io {
        path: path.to_path_buf(),
        source,
    })?;
    // The header is not UTF-8 clean on every file (machine-locale dates); the
    // entity stream is pure ASCII, so a replacement character cannot touch
    // geometry.
    //
    // By value: `decode` takes the bytes rather than borrowing them, so a
    // clean file becomes a string without a copy and a dirty one is converted
    // into a buffer sized in advance. `String::from_utf8_lossy(&bytes)` here
    // held the bytes and a doubling string at once, and cost 72 MB on a 35 MB
    // file.
    let text = xt_parser::decode(bytes);
    // By value, so the parser can let it go the moment it has stripped a copy
    // of its own. Lent instead, this and that copy are both live for the whole
    // parse — 35 MB twice, at the point where the peak is.
    let mut scene = to_scene_owned(text, options)?;
    scene.0.meta.source = path
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_default();
    Ok(scene)
}

/// Lower XT text into a scene.
pub fn to_scene(text: &str, options: &LowerOptions) -> Result<(Scene, Report), XtSceneError> {
    to_scene_from(xt_parser::parse_raw(text)?, options)
}

/// Lower XT text into a scene, taking the text so it can be freed early.
pub fn to_scene_owned(
    text: String,
    options: &LowerOptions,
) -> Result<(Scene, Report), XtSceneError> {
    to_scene_from(xt_parser::parse_raw_owned(text)?, options)
}

fn to_scene_from(
    file: xt_parser::RawFile,
    options: &LowerOptions,
) -> Result<(Scene, Report), XtSceneError> {
    let mut report = Report {
        truncated: file.truncated.as_ref().map(|t| t.to_string()),
        ..Default::default()
    };

    // The attribute graph: per-face colour and reflectivity, per-body names.
    let hints = appearance::hints_from_entities(&file.entities);
    let face_appearance = per_face_appearance(&file.entities);
    let body_name: FxHashMap<usize, &str> = hints
        .body_names
        .iter()
        .map(|(h, n)| (*h, n.as_str()))
        .collect();

    let mut scene = Scene {
        meta: Meta {
            source: String::new(),
            authoring_tool: file.header.application.clone(),
            unit: Unit::Millimetre,
            tolerance: topo::DEFAULT_TOLERANCE * 1e3,
            ..Default::default()
        },
        ..Default::default()
    };

    // XT coordinates are metres; the shared scene convention is millimetres.
    const M_TO_MM: f64 = 1e3;

    let mut geometry_of_body: FxHashMap<usize, (GeometryId, String)> = FxHashMap::default();
    for lowered in topo::lower_bodies(&file.entities, topo::DEFAULT_TOLERANCE) {
        let topo::LoweredBody {
            mut solid,
            face_sources,
            body_handle,
            skipped,
        } = lowered;
        report.skipped.extend(skipped);
        if solid.faces.is_empty() {
            // A body whose every face failed to lower left the scene without a
            // word, and a whole part going missing is not a detail: the file
            // here holds 46 bodies and the scene held 44 before this said so.
            report.skipped.push(topo::Skip {
                entity: body_handle,
                reason: format!(
                    "body {:?} lowered to no faces at all and is not in the scene",
                    body_name.get(&body_handle).copied().unwrap_or("(unnamed)")
                ),
            });
            continue;
        }

        let name = body_name
            .get(&body_handle)
            .map(|n| n.to_string())
            .unwrap_or_else(|| format!("body-{body_handle}"));

        scale_solid(&mut solid, M_TO_MM);
        solid.name = name.clone();
        if std::env::var_os("XT_EDGE_TRACE").is_some() || std::env::var_os("XT_WALK_TRACE").is_some() {
            eprintln!("[body] handle={body_handle} name={name}");
        }

        let complaints = diagnose(&solid);
        if !complaints.is_empty() {
            report.diagnostics.push((name.clone(), complaints));
        }

        // Per-face materials straight from the face's own attributes — the
        // reflectivity here is the designer's, attached to this very face.
        let mut face_materials: Vec<Option<MaterialId>> = vec![None; solid.faces.len()];
        let mut assigned = 0usize;
        for (i, &src) in face_sources.iter().enumerate() {
            if let Some((colour, refl)) = face_appearance.get(&src) {
                let m = options
                    .materials
                    .resolve_with_reflectivity(&name, Some(*colour), *refl);
                face_materials[i] = Some(scene.add_material(m));
                assigned += 1;
            }
        }
        let uniform = face_materials
            .first()
            .copied()
            .flatten()
            .filter(|first| face_materials.iter().all(|m| *m == Some(*first)));
        let (material, face_materials) = match uniform {
            Some(m) => (Some(m), Vec::new()),
            None if assigned == 0 => (
                Some(scene.add_material(options.materials.resolve(&name, None))),
                Vec::new(),
            ),
            None => (None, face_materials),
        };

        let gid = scene.add_geometry(Geometry {
            name: name.clone(),
            brep: Some(solid),
            mesh: None,
            material,
            face_materials,
        });
        geometry_of_body.insert(body_handle, (gid, name));
    }

    place_instances(&file.entities, &geometry_of_body, &mut scene, &mut report);

    Ok((scene, report))
}

/// Build the assembly tree from INSTANCE (11) and TRANSFORM (100) entities.
///
/// Each instance places a part — a body or a whole assembly — under an owning
/// assembly, with an optional rigid transform. Bodies never referenced by any
/// instance stand alone at the root, which is also the whole story for
/// single-body exports.
fn place_instances(
    entities: &xt_parser::entity::Entities,
    geometry_of_body: &FxHashMap<usize, (GeometryId, String)>,
    scene: &mut Scene,
    report: &mut Report,
) {
    let index: FxHashMap<usize, &xt_parser::entity::RawEntity> =
        entities.iter().map(|e| (e.index, e)).collect();
    let ptr = |e: &xt_parser::entity::RawEntity, i: usize| {
        entities.fields(e).get(i).map(|f| f.as_ptr()).unwrap_or(0)
    };

    // INSTANCE: [3]=part (BODY or ASSEMBLY), [4]=transform, [5]=owner assembly.
    struct Inst {
        part: usize,
        owner: usize,
        transform: Transform,
    }
    let mut instances: Vec<Inst> = Vec::new();
    let mut instanced_bodies: rustc_hash::FxHashSet<usize> = Default::default();
    for e in entities.iter().filter(|e| e.type_id == 11) {
        let part = ptr(e, 3);
        let owner = ptr(e, 5);
        let transform = index
            .get(&ptr(e, 4))
            .filter(|t| t.type_id == 100)
            .map(|t| transform_of(entities, t))
            .unwrap_or(Transform::IDENTITY);
        if index.get(&part).is_some_and(|p| p.type_id == 12) {
            instanced_bodies.insert(part);
        }
        instances.push(Inst {
            part,
            owner,
            transform,
        });
    }

    if instances.is_empty() {
        for (gid, name) in geometry_of_body.values() {
            let node = scene.add_node(Node {
                name: name.clone(),
                geometry: Some(*gid),
                ..Default::default()
            });
            scene.roots.push(node);
        }
        return;
    }

    // One node per assembly, then one node per instance under its owner.
    let mut assembly_node: FxHashMap<usize, NodeId> = FxHashMap::default();
    for e in entities.iter().filter(|e| e.type_id == 10) {
        let id = scene.add_node(Node {
            name: format!("assembly-{}", e.index),
            ..Default::default()
        });
        assembly_node.insert(e.index, id);
    }

    let mut placed_assemblies: rustc_hash::FxHashSet<usize> = Default::default();
    for inst in &instances {
        let node = match index.get(&inst.part).map(|p| p.type_id) {
            Some(12) => {
                let Some((gid, name)) = geometry_of_body.get(&inst.part) else {
                    continue; // a body that lowered to nothing
                };
                scene.add_node(Node {
                    name: name.clone(),
                    transform: inst.transform,
                    geometry: Some(*gid),
                    ..Default::default()
                })
            }
            Some(10) => {
                let Some(&sub) = assembly_node.get(&inst.part) else {
                    continue;
                };
                // A sub-assembly node has a single parent in this model; a
                // second placement would need node duplication, which the
                // corpus does not exercise — count it rather than mis-place.
                if !placed_assemblies.insert(inst.part) {
                    report.diagnostics.push((
                        format!("assembly-{}", inst.part),
                        vec!["placed more than once; later placements dropped".into()],
                    ));
                    continue;
                }
                scene.add_node(Node {
                    name: format!("assembly-{}", inst.part),
                    transform: inst.transform,
                    children: vec![sub],
                    ..Default::default()
                })
            }
            _ => continue,
        };
        match assembly_node.get(&inst.owner) {
            Some(&owner) => scene.nodes[owner.index()].children.push(node),
            None => scene.roots.push(node),
        }
    }

    // Assemblies never placed anywhere are roots.
    for (handle, &node) in &assembly_node {
        if !placed_assemblies.contains(handle) {
            scene.roots.push(node);
        }
    }
    // Bodies no instance references stand alone.
    for (handle, (gid, name)) in geometry_of_body {
        if !instanced_bodies.contains(handle) {
            let node = scene.add_node(Node {
                name: name.clone(),
                geometry: Some(*gid),
                ..Default::default()
            });
            scene.roots.push(node);
        }
    }
}

/// TRANSFORM (100): `[4]` rotation matrix (nine floats), `[5]` translation,
/// `[6]` scale. Translation is in the file's metres and the scene is
/// millimetres, so it scales by a thousand; the rotation does not.
fn transform_of(entities: &xt_parser::entity::Entities, t: &xt_parser::entity::RawEntity) -> Transform {
    let m3 = entities.fields(t).get(4).and_then(|f| f.as_mat3()).unwrap_or([
        1.0, 0.0, 0.0, 0.0, 1.0, 0.0, 0.0, 0.0, 1.0,
    ]);
    let tr = entities
        .fields(t)
        .get(5)
        .map(|f| f.as_vec3())
        .unwrap_or([0.0; 3]);
    let scale = entities
        .fields(t)
        .get(6)
        .map(|f| f.as_f64())
        .filter(|s| s.is_finite() && *s > 0.0)
        .unwrap_or(1.0);
    let mut out = Transform::IDENTITY;
    for r in 0..3 {
        for c in 0..3 {
            out.m[r][c] = m3[r * 3 + c] * scale;
        }
        out.m[r][3] = tr[r] * 1e3;
    }
    out
}

/// Per-face colour and reflectivity from the attribute graph.
fn per_face_appearance(
    entities: &xt_parser::entity::Entities,
) -> FxHashMap<usize, (ColourEvidence, Option<f32>)> {
    let index: FxHashMap<usize, &xt_parser::entity::RawEntity> =
        entities.iter().map(|e| (e.index, e)).collect();

    let mut def_names: FxHashMap<usize, String> = FxHashMap::default();
    for e in entities.iter().filter(|e| e.type_id == 80) {
        let ident = entities.fields(e).get(1).map(|f| f.as_ptr()).unwrap_or(0);
        if let Some(id_e) = index.get(&ident) {
            def_names.insert(e.index, entities.var_char(id_e).iter().collect());
        }
    }

    let mut colour: FxHashMap<usize, [f32; 3]> = FxHashMap::default();
    let mut refl: FxHashMap<usize, f32> = FxHashMap::default();
    for e in entities.iter().filter(|e| e.type_id == 81) {
        let def = entities.fields(e).get(1).map(|f| f.as_ptr()).unwrap_or(0);
        let owner = entities.fields(e).get(2).map(|f| f.as_ptr()).unwrap_or(0);
        let Some(def_name) = def_names.get(&def) else {
            continue;
        };
        let mut floats: Vec<f64> = Vec::new();
        for &v in entities.var_ptr(e) {
            if let Some(ve) = index.get(&v) {
                floats.extend_from_slice(entities.var_f64(ve));
            }
        }
        match def_name.as_str() {
            "SDL/TYSA_COLOUR" if floats.len() >= 3 => {
                colour.insert(owner, [floats[0] as f32, floats[1] as f32, floats[2] as f32]);
            }
            "SDL/TYSA_REFLECTIVITY" if !floats.is_empty() => {
                refl.insert(owner, floats[0] as f32);
            }
            _ => {}
        }
    }

    let lin = |v: f32| {
        let v = v.clamp(0.0, 1.0);
        if v <= 0.040_45 {
            v / 12.92
        } else {
            ((v + 0.055) / 1.055).powf(2.4)
        }
    };
    colour
        .into_iter()
        .map(|(face, srgb)| {
            (
                face,
                (
                    ColourEvidence {
                        srgb,
                        linear: [lin(srgb[0]), lin(srgb[1]), lin(srgb[2])],
                        alpha: 1.0,
                    },
                    refl.get(&face).copied(),
                ),
            )
        })
        .collect()
}

/// Scale a solid's geometry uniformly (metres → millimetres).
fn scale_solid(solid: &mut cad_ir::brep::Solid, f: f64) {
    for v in &mut solid.vertices {
        *v = *v * f;
    }
    for c in &mut solid.curves {
        scale_curve(c, f);
    }
    for s in &mut solid.surfaces {
        scale_surface(s, f);
    }
    solid.tolerance *= f;
    for e in &mut solid.edges {
        e.tolerance *= f;
        // Parameter ranges of arc-length-parameterised curves do not scale;
        // ranges recovered before scaling stay valid because both the curve
        // and the vertices scaled together — angles are scale-free, and line
        // parameters scale with the direction vector, which scaled too.
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
        Composite { segments } => segments
            .iter_mut()
            .for_each(|s| scale_curve(&mut s.curve, f)),
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

/// The same cheap structural checks the STEP path runs.
fn diagnose(solid: &cad_ir::brep::Solid) -> Vec<String> {
    let mut out = Vec::new();
    let mut uses = vec![0usize; solid.edges.len()];
    for f in &solid.faces {
        for b in &f.bounds {
            for h in &b.halves {
                if let Some(u) = uses.get_mut(h.edge.index()) {
                    *u += 1;
                }
            }
        }
    }
    let dangling = uses.iter().filter(|&&u| u == 1).count();
    let unused = uses.iter().filter(|&&u| u == 0).count();
    let over = uses.iter().filter(|&&u| u > 2).count();
    if dangling > 0 || unused > 0 || over > 0 {
        out.push(format!(
            "edges: {} total, {dangling} used once, {unused} unused, {over} used 3+ times",
            solid.edges.len()
        ));
    }
    out
}
