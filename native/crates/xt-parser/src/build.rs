//! Build typed IR from raw entities.
//!
//! Phase 2 of parsing: walk the raw entity list, resolve cross-references,
//! and produce `Vec<XtBody>` with full topology and geometry.

use std::collections::HashMap;

use crate::entity::{Entities, RawEntity};
use crate::error::Result;
use crate::schema;
use crate::types::*;

/// Build typed bodies from a raw entity list.
pub fn build_bodies(entities: &Entities) -> Result<Vec<XtBody>> {
    // Index entities by (type_id, entity_index) for unambiguous lookup.
    // Multiple entities can share an entity_index (e.g. ATTRIBUTE and BODY both at idx=1).
    let mut by_type_index: HashMap<(u16, usize), &RawEntity> = HashMap::new();
    // Also a simple index for cases where type is known but index may collide.
    let mut by_index: HashMap<usize, &RawEntity> = HashMap::new();
    for e in entities.iter() {
        by_type_index.insert((e.type_id, e.index), e);
        by_index.insert(e.index, e);
    }

    // Find all BODY entities.
    let body_entities: Vec<&RawEntity> = entities
        .iter()
        .filter(|e| e.type_id == schema::BODY)
        .collect();

    let mut bodies = Vec::new();
    for be in &body_entities {
        match build_one_body(be, &by_index, &by_type_index, entities) {
            Ok(b) => bodies.push(b),
            Err(e) => eprintln!("warning: skipping body at index {}: {}", be.index, e),
        }
    }

    Ok(bodies)
}

fn build_one_body(
    body_ent: &RawEntity,
    idx: &HashMap<usize, &RawEntity>,
    by_ti: &HashMap<(u16, usize), &RawEntity>,
    entities: &Entities,
) -> Result<XtBody> {
    let f = entities.fields(body_ent);

    // Field layout depends on the PS version and annotation diffs applied.
    //
    // Base schema / sch_13006 (23 transmitted fields, 0-indexed):
    //   0=highest_node_id, 1=attr, 2=attr_chains, 3=surface, 4=curve, 5=point,
    //   6=key, 7=res_size, 8=res_linear, 9=ref_instance, 10=next, 11=prev,
    //   12=state, 13=owner, 14=body_type, 15=nom_geom_state, 16=shell,
    //   17=bsurf, 18=bcurve, 19=bpoint, 20=region, 21=edge, 22=vertex
    //
    // PS30+ annotated (34 fields from Onshape/SolidWorks):
    //   Annotation diffs insert extra fields, shifting positions.
    //   Verified from entity dumps: field[14]=body_type, field[18]=shell,
    //   field[19]=bsurf, field[20]=bcurve, field[21]=bpoint, field[24]=region.
    let (res_size_fi, res_linear_fi, body_type_fi,
         shell_fi, surf_fi, curve_fi, point_fi, region_fi) =
        if f.len() >= 30 {
            // PS30+ annotated layout (34 fields)
            (9, 10, 14, 18, 19, 20, 21, 24)
        } else {
            // Base schema layout (23 fields)
            (7, 8, 14, 16, 3, 4, 5, 20)
        };

    let node_id = f.get(0).map(|v| v.as_i64()).unwrap_or(0);
    let res_size = f.get(res_size_fi).map(|v| v.as_f64()).unwrap_or(1e3);
    let res_linear = f.get(res_linear_fi).map(|v| v.as_f64()).unwrap_or(1e-6);
    let body_type_raw = f.get(body_type_fi).map(|v| v.as_byte()).unwrap_or(0);
    // The four the format defines. 7 and 12 stood here for sheet and wire and
    // are not body types at all, so every sheet body in the corpus read as
    // Unknown(3) — including the one in 500.081UB.x_t, whose second body is a
    // half-micron planar sheet that the file really does contain.
    let body_type = match body_type_raw {
        1 => XtBodyType::Solid,
        2 => XtBodyType::Wire,
        3 => XtBodyType::Sheet,
        6 => XtBodyType::General,
        _ => XtBodyType::Unknown(body_type_raw),
    };

    let mut body = XtBody {
        node_id,
        body_type,
        res_size,
        res_linear,
        regions: Vec::new(),
        shells: Vec::new(),
        surfaces: HashMap::new(),
        curves: HashMap::new(),
        points: HashMap::new(),
        vertices: HashMap::new(),
        edges: HashMap::new(),
    };

    // Register geometry chains.
    register_geometry_chain(f.get(surf_fi).map(|v| v.as_ptr()).unwrap_or(0), idx, entities, &mut body);
    register_curve_chain(f.get(curve_fi).map(|v| v.as_ptr()).unwrap_or(0), idx, entities, &mut body);
    register_point_chain(f.get(point_fi).map(|v| v.as_ptr()).unwrap_or(0), idx, entities, &mut body);

    // Build regions
    let region_ptr = f.get(region_fi).map(|v| v.as_ptr()).unwrap_or(0);
    build_regions(region_ptr, by_ti, entities, &mut body)?;

    // Build shells → faces → loops → fins
    let shell_ptr = f.get(shell_fi).map(|v| v.as_ptr()).unwrap_or(0);
    build_shells(shell_ptr, idx, by_ti, entities, &mut body)?;

    Ok(body)
}

// ── Geometry registration ────────────────────────────────────────────────────

fn register_geometry_chain(
    mut ptr: usize,
    idx: &HashMap<usize, &RawEntity>,
    entities: &Entities,
    body: &mut XtBody,
) {
    let mut visited = std::collections::HashSet::new();
    while ptr != 0 && visited.insert(ptr) {
        if let Some(ent) = idx.get(&ptr) {
            if let Some(surf) = build_surface(ent, idx, entities) {
                body.surfaces.insert(ptr, surf);
            }
            // next surface in chain: field[3]
            ptr = entities.fields(ent).get(3).map(|v| v.as_ptr()).unwrap_or(0);
        } else {
            break;
        }
    }
}

fn register_curve_chain(
    mut ptr: usize,
    idx: &HashMap<usize, &RawEntity>,
    entities: &Entities,
    body: &mut XtBody,
) {
    let mut visited = std::collections::HashSet::new();
    while ptr != 0 && visited.insert(ptr) {
        if let Some(ent) = idx.get(&ptr) {
            if let Some(curve) = build_curve(ent, idx, entities) {
                body.curves.insert(ptr, curve);
            }
            ptr = entities.fields(ent).get(3).map(|v| v.as_ptr()).unwrap_or(0);
        } else {
            break;
        }
    }
}

fn register_point_chain(
    mut ptr: usize,
    idx: &HashMap<usize, &RawEntity>,
    entities: &Entities,
    body: &mut XtBody,
) {
    let mut visited = std::collections::HashSet::new();
    while ptr != 0 && visited.insert(ptr) {
        if let Some(ent) = idx.get(&ptr) {
            if ent.type_id == schema::POINT {
                let pos = entities.fields(ent).get(5).map(|v| v.as_vec3()).unwrap_or([0.0; 3]);
                body.points.insert(ptr, pos);
            }
            ptr = entities.fields(ent).get(3).map(|v| v.as_ptr()).unwrap_or(0);
        } else {
            break;
        }
    }
}

// ── Surface builders ─────────────────────────────────────────────────────────

fn build_surface(
    ent: &RawEntity,
    idx: &HashMap<usize, &RawEntity>,
    entities: &Entities,
) -> Option<XtSurface> {
    let f = entities.fields(ent);
    match ent.type_id {
        schema::PLANE => {
            let point = f.get(7)?.as_vec3();
            let normal = f.get(8)?.as_vec3();
            Some(XtSurface::Plane(XtPlane {
                basis: XtBasisSet {
                    location: point,
                    axis: normal,
                    ref_direction: [0.0; 3], // not stored for plane
                },
            }))
        }
        schema::CYLINDER => {
            // Reference 5.2.2.2: pvec[7], axis[8], radius[9], x_axis[10]
            let point = f.get(7)?.as_vec3();
            let axis = f.get(8)?.as_vec3();
            let radius = f.get(9)?.as_f64();
            let ref_dir = f.get(10)?.as_vec3();
            Some(XtSurface::Cylinder(XtCylinder {
                basis: XtBasisSet {
                    location: point,
                    axis,
                    ref_direction: ref_dir,
                },
                radius,
            }))
        }
        schema::CONE => {
            // Reference 5.2.2.3: pvec[7], axis[8], radius[9],
            //   sin_half_angle[10], cos_half_angle[11], x_axis[12]
            let apex = f.get(7)?.as_vec3();
            let axis = f.get(8)?.as_vec3();
            let radius = f.get(9)?.as_f64();
            let sin_ha = f.get(10)?.as_f64();
            let cos_ha = f.get(11)?.as_f64();
            let ref_dir = f.get(12)?.as_vec3();
            let semi_angle = sin_ha.atan2(cos_ha);
            Some(XtSurface::Cone(XtCone {
                basis: XtBasisSet {
                    location: apex,
                    axis,
                    ref_direction: ref_dir,
                },
                radius,
                semi_angle,
            }))
        }
        schema::SPHERE => {
            // Reference 5.2.2.4: centre[7], radius[8], axis[9], x_axis[10]
            let centre = f.get(7)?.as_vec3();
            let radius = f.get(8)?.as_f64();
            let axis = f.get(9)?.as_vec3();
            let ref_dir = f.get(10)?.as_vec3();
            Some(XtSurface::Sphere(XtSphere {
                basis: XtBasisSet {
                    location: centre,
                    axis,
                    ref_direction: ref_dir,
                },
                radius,
            }))
        }
        schema::TORUS => {
            // Reference 5.2.2.5: centre[7], axis[8], major_r[9], minor_r[10], x_axis[11]
            let centre = f.get(7)?.as_vec3();
            let axis = f.get(8)?.as_vec3();
            let major_r = f.get(9)?.as_f64();
            let minor_r = f.get(10)?.as_f64();
            let ref_dir = f.get(11)?.as_vec3();
            Some(XtSurface::Torus(XtTorus {
                basis: XtBasisSet {
                    location: centre,
                    axis,
                    ref_direction: ref_dir,
                },
                major_radius: major_r,
                minor_radius: minor_r,
            }))
        }
        schema::B_SURFACE => {
            // Resolve NURBS_SURF sub-entity
            let nurbs_ptr = f.get(7)?.as_ptr();
            build_bspline_surface(nurbs_ptr, idx, entities)
        }
        schema::SWEPT_SURF => {
            let curve_ptr = f.get(7)?.as_ptr();
            let direction = f.get(8)?.as_vec3();
            Some(XtSurface::Swept(XtSweptSurface {
                profile_curve_key: curve_ptr,
                direction,
            }))
        }
        schema::SPUN_SURF => {
            let curve_ptr = f.get(7)?.as_ptr();
            let axis_pt = f.get(8)?.as_vec3();
            let axis_dir = f.get(9)?.as_vec3();
            Some(XtSurface::Spun(XtSpunSurface {
                profile_curve_key: curve_ptr,
                axis_point: axis_pt,
                axis_direction: axis_dir,
            }))
        }
        schema::OFFSET_SURF => {
            // Reference 5.2.2.8: check[7](c), true_offset[8](l), surface[9](p),
            //   offset[10](f), scale[11](f)
            let base_ptr = f.get(9)?.as_ptr();
            let offset = f.get(10)?.as_f64();
            Some(XtSurface::Offset(XtOffsetSurface {
                base_surface_key: base_ptr,
                offset_distance: offset,
            }))
        }
        schema::BLENDED_EDGE => {
            // Reference 5.2.2.6: blend_type[7](c), surface[8,9](p×2),
            //   spine[10](p), range[11,12](f×2), ...
            let _blend_type = f.get(7)?.as_char();
            let surf1_ptr = f.get(8)?.as_ptr();
            let surf2_ptr = f.get(9)?.as_ptr();
            let spine_ptr = f.get(10)?.as_ptr();
            let range_start = f.get(11)?.as_f64();
            Some(XtSurface::Blend(XtBlendSurface {
                spine_curve_key: spine_ptr,
                support_surface_keys: [surf1_ptr, surf2_ptr],
                range_start,
            }))
        }
        _ => None,
    }
}

fn build_bspline_surface(
    nurbs_ptr: usize,
    idx: &HashMap<usize, &RawEntity>,
    entities: &Entities,
) -> Option<XtSurface> {
    let nurbs = idx.get(&nurbs_ptr)?;
    if nurbs.type_id != schema::NURBS_SURF {
        return None;
    }
    let f = entities.fields(nurbs);

    // NURBS_SURF layout from sch_13006 (20 transmitted fields):
    //   [0]=u_periodic(l), [1]=v_periodic(l), [2]=u_degree(n), [3]=v_degree(n),
    //   [4]=n_u_vertices(d), [5]=n_v_vertices(d), [6]=u_knot_type(u), [7]=v_knot_type(u),
    //   [8]=n_u_knots(d), [9]=n_v_knots(d), [10]=rational(l),
    //   [11]=u_closed(l), [12]=v_closed(l), [13]=surface_form(u), [14]=vertex_dim(n),
    //   [15]=bspline_vertices(p), [16]=u_knot_mult(p), [17]=v_knot_mult(p),
    //   [18]=u_knots(p), [19]=v_knots(p)
    let u_periodic = f.get(0)?.as_bool();
    let v_periodic = f.get(1)?.as_bool();
    let u_degree = f.get(2)?.as_i64() as u32;
    let v_degree = f.get(3)?.as_i64() as u32;
    let n_u_verts = f.get(4)?.as_i64() as usize;
    let n_v_verts = f.get(5)?.as_i64() as usize;
    let vertex_dim = f.get(14)?.as_i64() as usize;
    let rational = f.get(10)?.as_bool();
    let u_closed = f.get(11)?.as_bool();
    let v_closed = f.get(12)?.as_bool();

    let verts_ptr = f.get(15)?.as_ptr();
    let u_kmult_ptr = f.get(16)?.as_ptr();
    let v_kmult_ptr = f.get(17)?.as_ptr();
    let u_kset_ptr = f.get(18)?.as_ptr();
    let v_kset_ptr = f.get(19)?.as_ptr();

    let raw_verts = idx.get(&verts_ptr).map(|e| entities.var_f64(e).to_vec()).unwrap_or_default();
    let u_mults: Vec<i16> = idx.get(&u_kmult_ptr).map(|e| entities.var_i16(e).to_vec()).unwrap_or_default();
    let u_knot_vals: Vec<f64> = idx.get(&u_kset_ptr).map(|e| entities.var_f64(e).to_vec()).unwrap_or_default();
    let v_mults: Vec<i16> = idx.get(&v_kmult_ptr).map(|e| entities.var_i16(e).to_vec()).unwrap_or_default();
    let v_knot_vals: Vec<f64> = idx.get(&v_kset_ptr).map(|e| entities.var_f64(e).to_vec()).unwrap_or_default();

    let u_knots = expand_knots(&u_knot_vals, &u_mults);
    let v_knots = expand_knots(&v_knot_vals, &v_mults);

    let dim = vertex_dim;
    let (poles, weights) = reshape_poles(&raw_verts, n_u_verts * n_v_verts, dim, rational);

    Some(XtSurface::BSpline(XtBSplineSurface {
        u_degree,
        v_degree,
        n_u_vertices: n_u_verts,
        n_v_vertices: n_v_verts,
        u_knots,
        v_knots,
        poles,
        weights,
        u_periodic,
        v_periodic,
        u_closed,
        v_closed,
    }))
}

// ── Curve builders ───────────────────────────────────────────────────────────

fn build_curve(
    ent: &RawEntity,
    idx: &HashMap<usize, &RawEntity>,
    entities: &Entities,
) -> Option<XtCurve> {
    let f = entities.fields(ent);
    match ent.type_id {
        schema::LINE => {
            let pos = f.get(7)?.as_vec3();
            let dir = f.get(8)?.as_vec3();
            Some(XtCurve::Line(XtLine {
                position: pos,
                direction: dir,
            }))
        }
        schema::CIRCLE => {
            let centre = f.get(7)?.as_vec3();
            let axis = f.get(8)?.as_vec3();
            let ref_dir = f.get(9)?.as_vec3();
            let radius = f.get(10)?.as_f64();
            Some(XtCurve::Circle(XtCircle {
                basis: XtBasisSet {
                    location: centre,
                    axis,
                    ref_direction: ref_dir,
                },
                radius,
            }))
        }
        schema::ELLIPSE => {
            // sch_13006.s_t: sense[6](c), centre[7](v), normal[8](v),
            //   x_axis[9](v), major_radius[10](f), minor_radius[11](f)
            let centre = f.get(7)?.as_vec3();
            let axis = f.get(8)?.as_vec3();
            let ref_dir = f.get(9)?.as_vec3();
            let semi_major = f.get(10)?.as_f64();
            let semi_minor = f.get(11)?.as_f64();
            Some(XtCurve::Ellipse(XtEllipse {
                basis: XtBasisSet {
                    location: centre,
                    axis,
                    ref_direction: ref_dir,
                },
                major_radius: semi_major,
                minor_radius: semi_minor,
            }))
        }
        schema::B_CURVE => {
            let nurbs_ptr = f.get(7)?.as_ptr();
            build_bspline_curve(nurbs_ptr, idx, entities)
        }
        schema::INTERSECTION => {
            // P2 at [7] stores only surf1 (surf2 discarded by read_field_value).
            // chart lands at [8], start at [9], end at [10].
            let surf1 = f.get(7)?.as_ptr();
            // surf2 is not available (discarded by P2 collapsing)
            let chart_ptr = f.get(8)?.as_ptr();
            let approx = if let Some(chart) = idx.get(&chart_ptr) {
                chart_to_points(entities.var_f64(chart))
            } else {
                Vec::new()
            };
            Some(XtCurve::Intersection(XtIntersectionCurve {
                surface_keys: [surf1, 0],
                approx_points: approx,
            }))
        }
        schema::SP_CURVE => {
            let surface_ptr = f.get(7)?.as_ptr();
            let bcurve_ptr = f.get(8)?.as_ptr();
            // The B_CURVE for an SP_CURVE is a 2D curve in parameter space.
            let curve_2d = build_bspline_curve(bcurve_ptr, idx, entities)?;
            Some(XtCurve::SPCurve(XtSPCurve {
                surface_key: surface_ptr,
                curve_2d: match curve_2d {
                    XtCurve::BSpline(b) => b,
                    _ => return None,
                },
            }))
        }
        schema::TRIMMED_CURVE => {
            // sch_13006: basis_curve[7](p), point_1[8](v), point_2[9](v),
            //   parm_1[10](f), parm_2[11](f)
            let basis_ptr = f.get(7)?.as_ptr();
            let t0 = f.get(10)?.as_f64();
            let t1 = f.get(11)?.as_f64();
            Some(XtCurve::Trimmed(XtTrimmedCurve {
                basis_curve_key: basis_ptr,
                t_start: t0,
                t_end: t1,
            }))
        }
        _ => None,
    }
}

fn build_bspline_curve(
    nurbs_ptr: usize,
    idx: &HashMap<usize, &RawEntity>,
    entities: &Entities,
) -> Option<XtCurve> {
    // The pointer might be to B_CURVE → NURBS_CURVE, or directly to NURBS_CURVE
    let ent = idx.get(&nurbs_ptr)?;
    let nurbs = if ent.type_id == schema::B_CURVE {
        let inner_ptr = entities.fields(ent).get(7)?.as_ptr();
        idx.get(&inner_ptr)?
    } else if ent.type_id == schema::NURBS_CURVE {
        ent
    } else {
        return None;
    };

    let f = entities.fields(nurbs);
    let degree = f.get(0)?.as_i64() as u32;
    let n_verts = f.get(1)?.as_i64() as usize;
    let vert_dim = f.get(2)?.as_i64() as usize;
    let _n_knots = f.get(3)?.as_i64() as usize;
    let periodic = f.get(5)?.as_bool();
    let closed = f.get(6)?.as_bool();
    let rational = f.get(7)?.as_bool();

    let verts_ptr = f.get(9)?.as_ptr();
    let kmult_ptr = f.get(10)?.as_ptr();
    let kset_ptr = f.get(11)?.as_ptr();

    let raw_verts = idx.get(&verts_ptr).map(|e| entities.var_f64(e).to_vec()).unwrap_or_default();
    let mults: Vec<i16> = idx.get(&kmult_ptr).map(|e| entities.var_i16(e).to_vec()).unwrap_or_default();
    let knot_vals: Vec<f64> = idx.get(&kset_ptr).map(|e| entities.var_f64(e).to_vec()).unwrap_or_default();

    let knots = expand_knots(&knot_vals, &mults);
    let (poles, weights) = reshape_poles(&raw_verts, n_verts, vert_dim, rational);

    Some(XtCurve::BSpline(XtBSplineCurve {
        degree,
        knots,
        poles,
        weights,
        periodic,
        closed,
    }))
}

// ── Topology builders ────────────────────────────────────────────────────────

fn build_regions(
    ptr: usize,
    by_ti: &HashMap<(u16, usize), &RawEntity>,
    entities: &Entities,
    body: &mut XtBody,
) -> Result<()> {
    let mut visited = std::collections::HashSet::new();
    while ptr != 0 && visited.insert(ptr) {
        if let Some(ent) = by_ti.get(&(schema::REGION, ptr)) {
            // sch_13006 REGION fields:
            //   0:D(node_id), 1:P(attr), 2:P(body), 3:P(next),
            //   4:P(prev), 5:P(shell), 6:C(type='S'/'V')
            let is_solid = entities.fields(ent).get(6).map(|v| v.as_char()) == Some('S');
            let shell_ptr = entities.fields(ent).get(5).map(|v| v.as_ptr()).unwrap_or(0);
            let mut shell_indices = Vec::new();
            let mut sp = shell_ptr;
            let mut sv = std::collections::HashSet::new();
            while sp != 0 && sv.insert(sp) {
                shell_indices.push(sp);
                if let Some(se) = by_ti.get(&(schema::SHELL, sp)) {
                    // SHELL next_shell: field[3]
                    sp = entities.fields(se).get(3).map(|v| v.as_ptr()).unwrap_or(0);
                } else {
                    break;
                }
            }
            body.regions.push(XtRegion {
                is_solid,
                shell_indices,
            });
            break;
        } else {
            break;
        }
    }
    Ok(())
}

fn build_shells(
    mut ptr: usize,
    idx: &HashMap<usize, &RawEntity>,
    by_ti: &HashMap<(u16, usize), &RawEntity>,
    entities: &Entities,
    body: &mut XtBody,
) -> Result<()> {
    let mut visited = std::collections::HashSet::new();
    while ptr != 0 && visited.insert(ptr) {
        if let Some(shell_ent) = by_ti.get(&(schema::SHELL, ptr)) {
            // SHELL fields (9 total):
            //   [0]=node_id(d), [1]=attribs(p), [2]=body(p), [3]=next_shell(p),
            //   [4]=face(p), [5]=edge(p), [6]=vertex(p), [7]=region(p), [8]=front_face(p)
            //
            // The face chain pointer (field[4]) may collide with another entity type at
            // the same index (e.g. BODY or ATTRIBUTE). Use a type-aware scan instead:
            // find the first FACE whose shell back-pointer (field[6]) matches this shell.
            let shell_idx = ptr;

            // Try to find a face entry point: prefer field[4] if it resolves to a FACE.
            let initial_face_ptr = entities.fields(shell_ent).get(4).map(|v| v.as_ptr()).unwrap_or(0);
            let face_start = if by_ti.contains_key(&(schema::FACE, initial_face_ptr)) {
                initial_face_ptr
            } else {
                // Fallback: scan all entities for a FACE with shell back-ptr == shell_idx.
                // FACE field[6] is the shell back-pointer.
                entities
                    .iter()
                    .find(|e| {
                        e.type_id == schema::FACE
                            && entities.fields(e).get(6).map(|v| v.as_ptr()).unwrap_or(0) == shell_idx
                    })
                    .map(|e| e.index)
                    .unwrap_or(0)
            };

            let faces = build_faces(face_start, idx, by_ti, entities, body)?;
            body.shells.push(XtShell { faces });

            // next_shell: SHELL field[3]
            ptr = entities.fields(shell_ent).get(3).map(|v| v.as_ptr()).unwrap_or(0);
        } else {
            break;
        }
    }
    Ok(())
}

fn build_faces(
    mut ptr: usize,
    idx: &HashMap<usize, &RawEntity>,
    by_ti: &HashMap<(u16, usize), &RawEntity>,
    entities: &Entities,
    body: &mut XtBody,
) -> Result<Vec<XtFace>> {
    let mut faces = Vec::new();
    let mut visited = std::collections::HashSet::new();

    while ptr != 0 && visited.insert(ptr) {
        if let Some(face_ent) = by_ti.get(&(schema::FACE, ptr)) {
            // FACE fields (14 total):
            //   [0]=node_id(d), [1]=attribs(p), [2]=tolerance(f),
            //   [3]=next(p), [4]=previous(p), [5]=loop(p),
            //   [6]=shell(p), [7]=surface(p), [8]=sense(c),
            //   [9]=next_on_surface(p), [10]=prev_on_surface(p), [11]=next_front(p),
            //   [12]=prev_front(p), [13]=front_shell(p)
            let f = entities.fields(face_ent);
            let node_id = f.get(0).map(|v| v.as_i64()).unwrap_or(0);
            let tolerance = f.get(2).map(|v| v.as_f64()).unwrap_or(0.0);
            let surface_ptr = f.get(7).map(|v| v.as_ptr()).unwrap_or(0);
            let face_sense_ch = f.get(8).map(|v| v.as_char()).unwrap_or('+');

            // If surface not yet registered, try to register it now.
            if surface_ptr != 0 && !body.surfaces.contains_key(&surface_ptr) {
                if let Some(se) = idx.get(&surface_ptr) {
                    if let Some(surf) = build_surface(se, idx, entities) {
                        body.surfaces.insert(surface_ptr, surf);
                    }
                }
            }

            // Combine face sense with surface geometry sense.
            // Surface sense is at field 6 of the surface entity.
            let geom_sense = idx
                .get(&surface_ptr)
                .and_then(|se| entities.fields(se).get(6))
                .map(|v| v.as_char())
                .unwrap_or('+');
            let effective_sense = if (face_sense_ch == 'R' || face_sense_ch == '-')
                ^ (geom_sense == '-')
            {
                XtSense::Reversed
            } else {
                XtSense::Forward
            };

            // Build loops starting from field 5 (first loop).
            let loop_ptr = f.get(5).map(|v| v.as_ptr()).unwrap_or(0);
            let loops = build_loops(loop_ptr, idx, entities, by_ti, body)?;

            faces.push(XtFace {
                node_id,
                tolerance,
                surface_key: surface_ptr,
                sense: effective_sense,
                loops,
            });

            // Next face in shell chain (field 3).
            ptr = f.get(3).map(|v| v.as_ptr()).unwrap_or(0);
        } else {
            break;
        }
    }

    Ok(faces)
}

fn build_loops(
    mut ptr: usize,
    idx: &HashMap<usize, &RawEntity>,
    entities: &Entities,
    by_ti: &HashMap<(u16, usize), &RawEntity>,
    body: &mut XtBody,
) -> Result<Vec<XtLoop>> {
    let mut loops = Vec::new();
    let mut visited = std::collections::HashSet::new();

    while ptr != 0 && visited.insert(ptr) {
        if let Some(loop_ent) = by_ti.get(&(schema::LOOP, ptr)) {
            // LOOP fields (5 total):
            //   [0]=node_id(d), [1]=attribs(p), [2]=halfedge(p), [3]=face(p), [4]=next(p)
            let fin_ptr = entities.fields(loop_ent).get(2).map(|v| v.as_ptr()).unwrap_or(0);

            let fins = build_fins(fin_ptr, idx, entities, by_ti, body)?;

            let kind = if loops.is_empty() {
                XtLoopKind::Outer
            } else {
                XtLoopKind::Inner
            };

            loops.push(XtLoop { kind, fins });

            // next loop: LOOP field[4]
            ptr = entities.fields(loop_ent).get(4).map(|v| v.as_ptr()).unwrap_or(0);
        } else {
            break;
        }
    }

    Ok(loops)
}

fn build_fins(
    first_ptr: usize,
    idx: &HashMap<usize, &RawEntity>,
    entities: &Entities,
    by_ti: &HashMap<(u16, usize), &RawEntity>,
    body: &mut XtBody,
) -> Result<Vec<XtFin>> {
    let mut fins = Vec::new();
    let mut ptr = first_ptr;
    let mut count = 0;
    let max_fins = 100_000;

    loop {
        if ptr == 0 || count >= max_fins {
            break;
        }
        if let Some(fin_ent) = by_ti.get(&(schema::FIN, ptr)) {
            // HALFEDGE/FIN fields (10 total, NO node_id):
            //   [0]=attribs(p), [1]=loop(p), [2]=forward(p), [3]=backward(p),
            //   [4]=vertex(p), [5]=other(p), [6]=edge(p), [7]=curve/pcurve(p),
            //   [8]=next_at_vx(p), [9]=sense(c)
            //
            // Pre-V20 schemas omit the leading attribs pointer, shifting every
            // index down by one (see schema::standard_schema). The same FIN in
            // 500.076.x_t (V24) and 500.079UB.x_t (V10) differs only by that
            // leading `0`, so keying off the field count is exact.
            let f = entities.fields(fin_ent);
            let a = if f.len() >= 10 { 0 } else { 1 };
            let edge_ptr = f.get(6 - a).map(|v| v.as_ptr()).unwrap_or(0);
            let vertex_ptr = f.get(4 - a).map(|v| v.as_ptr()).unwrap_or(0);
            let pcurve_ptr = f.get(7 - a).map(|v| v.as_ptr()).unwrap_or(0);
            let sense_ch = f.get(9 - a).map(|v| v.as_char()).unwrap_or('+');
            let sense = if sense_ch == '-' {
                XtSense::Reversed
            } else {
                XtSense::Forward
            };

            // Register edge on-the-fly using type-safe lookup.
            if edge_ptr != 0 && !body.edges.contains_key(&edge_ptr) {
                if let Some(edge_ent) = by_ti.get(&(schema::EDGE, edge_ptr)) {
                    // EDGE fields (10 total):
                    //   [0]=node_id(d), [1]=attribs(p), [2]=tolerance(f), [3]=halfedge(p),
                    //   [4]=previous(p), [5]=next(p), [6]=curve(p), [7]=next_on_curve(p),
                    //   [8]=prev_on_curve(p), [9]=owner(p)
                    // Note: sense is encoded in the halfedge pointer sign, not a separate field.
                    let ef = &entities.fields(edge_ent);
                    let e_node_id = ef.get(0).map(|v| v.as_i64()).unwrap_or(0);
                    let curve_ptr = ef.get(6).map(|v| v.as_ptr()).unwrap_or(0);
                    let e_sense = XtSense::Forward;
                    let e_tolerance = ef.get(2).map(|v| v.as_f64()).unwrap_or(0.0);

                    // Register the edge's curve.
                    if curve_ptr != 0 && !body.curves.contains_key(&curve_ptr) {
                        if let Some(ce) = idx.get(&curve_ptr) {
                            if let Some(c) = build_curve(ce, idx, entities) {
                                body.curves.insert(curve_ptr, c);
                            }
                        }
                    }

                    body.edges.insert(
                        edge_ptr,
                        XtEdge {
                            node_id: e_node_id,
                            curve_key: if curve_ptr != 0 { Some(curve_ptr) } else { None },
                            tolerance: e_tolerance,
                            sense: e_sense,
                        },
                    );
                }
            }

            // Register vertex on-the-fly.
            if vertex_ptr != 0 && !body.vertices.contains_key(&vertex_ptr) {
                if let Some(vert_ent) = by_ti.get(&(schema::VERTEX, vertex_ptr)) {
                    // VERTEX fields (8 total):
                    //   [0]=node_id(d), [1]=attribs(p), [2]=halfedge(p), [3]=previous(p),
                    //   [4]=next(p), [5]=point(p), [6]=tolerance(f), [7]=owner(p)
                    let vf = &entities.fields(vert_ent);
                    let v_node_id = vf.get(0).map(|v| v.as_i64()).unwrap_or(0);
                    let point_ptr = vf.get(5).map(|v| v.as_ptr()).unwrap_or(0);
                    let v_tolerance = vf.get(6).map(|v| v.as_f64()).unwrap_or(0.0);

                    // Register the point.
                    if point_ptr != 0 && !body.points.contains_key(&point_ptr) {
                        if let Some(pe) = by_ti.get(&(schema::POINT, point_ptr)) {
                            let pos = entities.fields(pe).get(5).map(|v| v.as_vec3()).unwrap_or([0.0; 3]);
                            body.points.insert(point_ptr, pos);
                        }
                    }

                    body.vertices.insert(
                        vertex_ptr,
                        XtVertex {
                            node_id: v_node_id,
                            point_key: point_ptr,
                            tolerance: v_tolerance,
                        },
                    );
                }
            }

            // Register pcurve.
            if pcurve_ptr != 0 && !body.curves.contains_key(&pcurve_ptr) {
                if let Some(pce) = idx.get(&pcurve_ptr) {
                    if let Some(c) = build_curve(pce, idx, entities) {
                        body.curves.insert(pcurve_ptr, c);
                    }
                }
            }

            fins.push(XtFin {
                edge_key: edge_ptr,
                vertex_key: if vertex_ptr != 0 {
                    Some(vertex_ptr)
                } else {
                    None
                },
                sense,
                pcurve_key: if pcurve_ptr != 0 {
                    Some(pcurve_ptr)
                } else {
                    None
                },
            });

            // Follow forward pointer (circular linked list): FIN field[2]=forward(next_fin).
            let next_ptr = f.get(2 - a).map(|v| v.as_ptr()).unwrap_or(0);
            if next_ptr == first_ptr || next_ptr == 0 {
                break;
            }
            ptr = next_ptr;
            count += 1;
        } else {
            break;
        }
    }

    Ok(fins)
}

// ── Utility ──────────────────────────────────────────────────────────────────

/// Expand distinct knot values + multiplicities into the full knot vector.
fn expand_knots(distinct: &[f64], mults: &[i16]) -> Vec<f64> {
    let mut knots = Vec::new();
    for (val, &mult) in distinct.iter().zip(mults.iter()) {
        for _ in 0..mult.max(0) {
            knots.push(*val);
        }
    }
    knots
}

/// Reshape flat control point array into (poles, optional weights).
///
/// - vertex_dim=2: 2D curves → z=0
/// - vertex_dim=3: 3D non-rational
/// - vertex_dim=3 + rational: weight is 3rd component, divide x,y by weight
/// - vertex_dim=4: homogeneous rational [x*w, y*w, z*w, w]
fn reshape_poles(
    raw: &[f64],
    n_verts: usize,
    vertex_dim: usize,
    rational: bool,
) -> (Vec<[f64; 3]>, Option<Vec<f64>>) {
    if vertex_dim == 0 || raw.len() < n_verts * vertex_dim {
        return (Vec::new(), None);
    }

    let mut poles = Vec::with_capacity(n_verts);
    let mut weights = if rational {
        Some(Vec::with_capacity(n_verts))
    } else {
        None
    };

    for i in 0..n_verts {
        let base = i * vertex_dim;
        match vertex_dim {
            2 => {
                if rational {
                    // 2D rational: [u*w, v*w, w] — no, dim=2 means [u, v]
                    // Actually for 2D curves, dim=2 means non-homogeneous 2D
                    poles.push([raw[base], raw[base + 1], 0.0]);
                } else {
                    poles.push([raw[base], raw[base + 1], 0.0]);
                }
            }
            3 => {
                if rational {
                    // dim=3 rational: [x*w, y*w, w] (2D homogeneous)
                    let w = raw[base + 2];
                    if w.abs() > 1e-30 {
                        poles.push([raw[base] / w, raw[base + 1] / w, 0.0]);
                    } else {
                        poles.push([raw[base], raw[base + 1], 0.0]);
                    }
                    if let Some(ref mut ws) = weights {
                        ws.push(w);
                    }
                } else {
                    poles.push([raw[base], raw[base + 1], raw[base + 2]]);
                }
            }
            4 => {
                // 4D homogeneous: [x*w, y*w, z*w, w]
                let w = raw[base + 3];
                if w.abs() > 1e-30 {
                    poles.push([
                        raw[base] / w,
                        raw[base + 1] / w,
                        raw[base + 2] / w,
                    ]);
                } else {
                    poles.push([raw[base], raw[base + 1], raw[base + 2]]);
                }
                if let Some(ref mut ws) = weights {
                    ws.push(w);
                }
            }
            _ => {
                // Higher dimensions: take first 3 as xyz
                poles.push([raw[base], raw[base + 1], raw[base + 2]]);
                if rational && vertex_dim > 3 {
                    if let Some(ref mut ws) = weights {
                        ws.push(raw[base + vertex_dim - 1]);
                    }
                }
            }
        }
    }

    (poles, weights)
}

/// Convert CHART variable-length data to 3D points.
fn chart_to_points(data: &[f64]) -> Vec<[f64; 3]> {
    data.chunks_exact(3)
        .map(|c| [c[0], c[1], c[2]])
        .collect()
}
