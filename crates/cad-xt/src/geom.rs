//! Lowering XT geometry entities into [`cad_ir`] curves and surfaces.
//!
//! Field positions follow sch_13006 — every geometry type in the sample corpus
//! annotates as "base as-is". All curves and surfaces share the common prefix
//! `node_id(d) attribs(p) owner(p) next(p) previous(p) geometric_owner(p)
//! sense(c)`, so type-specific data starts at field 7.
//!
//! The one non-obvious mapping is the cone. Parasolid stores a point on the
//! axis, the sine and cosine of the half-angle, and a radius at that point,
//! with the cone *narrowing* in the +axis direction; `cad_ir`'s cone opens
//! along +axis, so the half-angle is negated.

use cad_ir::brep::{Curve, Curve2, NurbsCurve, NurbsCurve2, NurbsSurface, Surface};
use cad_ir::math::{Frame, Interval, Vec2, Vec3};
use rustc_hash::FxHashMap;
use xt_parser::entity::RawEntity;
use xt_parser::schema as xt;

/// Entity lookup by handle.
pub type Index<'a> = FxHashMap<usize, &'a RawEntity>;

fn v3(e: &RawEntity, i: usize) -> Vec3 {
    let a = e.fields.get(i).map(|f| f.as_vec3()).unwrap_or([0.0; 3]);
    Vec3::new(a[0], a[1], a[2])
}

fn f64_at(e: &RawEntity, i: usize) -> f64 {
    e.fields.get(i).map(|f| f.as_f64()).unwrap_or(0.0)
}

fn ptr(e: &RawEntity, i: usize) -> usize {
    e.fields.get(i).map(|f| f.as_ptr()).unwrap_or(0)
}

/// The geometry entity's own sense character, at common field 6.
pub fn geom_sense(e: &RawEntity) -> char {
    e.fields.get(6).map(|f| f.as_char()).unwrap_or('+')
}

/// Lower a surface entity, or say why it cannot be.
pub fn surface(e: &RawEntity, index: &Index) -> Result<Surface, String> {
    match e.type_id {
        xt::PLANE => {
            // Parasolid stores point + normal only; the in-plane axes are
            // derived canonically. Any orthonormal completion works because
            // the trim loops are inverted through the same frame.
            Ok(Surface::Plane {
                frame: Frame::new(v3(e, 7), v3(e, 8), v3(e, 8).any_perpendicular()),
            })
        }
        xt::CYLINDER => Ok(Surface::Cylinder {
            frame: Frame::new(v3(e, 7), v3(e, 8), v3(e, 10)),
            radius: f64_at(e, 9),
        }),
        xt::CONE => {
            let sin_ha = f64_at(e, 10);
            let cos_ha = f64_at(e, 11);
            Ok(Surface::Cone {
                frame: Frame::new(v3(e, 7), v3(e, 8), v3(e, 12)),
                radius: f64_at(e, 9),
                // Parasolid's cone narrows along +axis; cad_ir's opens.
                half_angle: -sin_ha.atan2(cos_ha),
            })
        }
        xt::SPHERE => Ok(Surface::Sphere {
            frame: Frame::new(v3(e, 7), v3(e, 9), v3(e, 10)),
            radius: f64_at(e, 8),
        }),
        xt::TORUS => Ok(Surface::Torus {
            frame: Frame::new(v3(e, 7), v3(e, 8), v3(e, 11)),
            major_radius: f64_at(e, 9),
            minor_radius: f64_at(e, 10),
        }),
        xt::B_SURFACE => {
            let inner = index
                .get(&ptr(e, 7))
                .ok_or("B_SURFACE points at nothing")?;
            nurbs_surface(inner, index).map(Surface::Nurbs)
        }
        xt::NURBS_SURF => nurbs_surface(e, index).map(Surface::Nurbs),
        other => Err(format!("surface type {other} not lowered yet")),
    }
}

/// Lower a curve entity, or say why it cannot be.
pub fn curve(e: &RawEntity, index: &Index) -> Result<Curve, String> {
    match e.type_id {
        xt::LINE => Ok(Curve::Line {
            origin: v3(e, 7),
            direction: v3(e, 8),
        }),
        xt::CIRCLE => Ok(Curve::Circle {
            frame: Frame::new(v3(e, 7), v3(e, 8), v3(e, 9)),
            radius: f64_at(e, 10),
        }),
        xt::ELLIPSE => Ok(Curve::Ellipse {
            frame: Frame::new(v3(e, 7), v3(e, 8), v3(e, 9)),
            semi_major: f64_at(e, 10),
            semi_minor: f64_at(e, 11),
        }),
        xt::B_CURVE => {
            let inner = index.get(&ptr(e, 7)).ok_or("B_CURVE points at nothing")?;
            nurbs_curve(inner, index).map(Curve::Nurbs)
        }
        xt::NURBS_CURVE => nurbs_curve(e, index).map(Curve::Nurbs),
        xt::TRIMMED_CURVE => {
            let basis = index
                .get(&ptr(e, 7))
                .ok_or("TRIMMED_CURVE points at nothing")?;
            let base = curve(basis, index)?;
            let (t0, t1) = (f64_at(e, 10), f64_at(e, 11));
            Ok(Curve::Trimmed {
                range: Interval::new(t0.min(t1), t0.max(t1)),
                base: Box::new(base),
            })
        }
        // An intersection curve's exact form needs both surfaces; its CHART is
        // the exact points the modeller evaluated on it, which as a polyline
        // is faithful to within the chart's own spacing.
        xt::INTERSECTION => {
            let chart = index
                .get(&ptr(e, 8))
                .ok_or("INTERSECTION has no chart")?;
            let points = chart_points(chart);
            if points.len() < 2 {
                return Err("intersection chart has fewer than two points".into());
            }
            Ok(Curve::Polyline { points })
        }
        xt::SP_CURVE => {
            // A curve living on a surface, defined by a 2D spline in that
            // surface's parameter space.
            let surf_ptr = ptr(e, 7);
            let bcurve = index
                .get(&ptr(e, 8))
                .ok_or("SP_CURVE has no parameter curve")?;
            let n2 = nurbs_curve2(bcurve, index)?;
            Ok(Curve::OnSurface {
                surface: cad_ir::brep::SurfaceId(surf_ptr as u32),
                pcurve: Curve2::Nurbs(n2),
            })
        }
        other => Err(format!("curve type {other} not lowered yet")),
    }
}

/// A fin's pcurve, when its curve pointer is an SP_CURVE.
pub fn pcurve_of(e: &RawEntity, index: &Index) -> Option<Curve2> {
    if e.type_id != xt::SP_CURVE {
        return None;
    }
    let bcurve = index.get(&ptr(e, 8))?;
    nurbs_curve2(bcurve, index).ok().map(Curve2::Nurbs)
}

/// CHART (40): var_f64 is `[t, x, y, z]` runs per the sch_13006 layout.
fn chart_points(chart: &RawEntity) -> Vec<Vec3> {
    let d = &chart.var_f64;
    // Points may be stored bare (x y z) or with a leading parameter (t x y z);
    // decide by divisibility, preferring the parameterised form.
    let stride = if d.len() % 4 == 0 { 4 } else if d.len() % 3 == 0 { 3 } else { 0 };
    if stride == 0 {
        return Vec::new();
    }
    d.chunks_exact(stride)
        .map(|c| {
            let o = stride - 3;
            Vec3::new(c[o], c[o + 1], c[o + 2])
        })
        .collect()
}

/// NURBS_CURVE fields: degree[0], n_vertices[1], vertex_dim[2], n_knots[3],
/// knot_type[4], periodic[5], closed[6], rational[7], (form[8]), vertices[9],
/// knot_mult[10], knot[11].
fn nurbs_curve(e: &RawEntity, index: &Index) -> Result<NurbsCurve, String> {
    if e.type_id != xt::NURBS_CURVE {
        return Err(format!("expected NURBS_CURVE, got type {}", e.type_id));
    }
    let degree = f64_at(e, 0) as usize;
    let n_verts = f64_at(e, 1) as usize;
    let dim = f64_at(e, 2) as usize;
    let periodic = e.fields.get(5).map(|f| f.as_bool()).unwrap_or(false);
    let closed = e.fields.get(6).map(|f| f.as_bool()).unwrap_or(false);
    let rational = e.fields.get(7).map(|f| f.as_bool()).unwrap_or(false);

    let raw = index
        .get(&ptr(e, 9))
        .map(|v| v.var_f64.as_slice())
        .unwrap_or(&[]);
    let knots = expanded_knots(e, 10, 11, index)?;

    let (control_points, weights) = split_poles(raw, n_verts, dim, rational)?;
    Ok(NurbsCurve {
        degree,
        control_points,
        weights,
        knots,
        closed: closed || periodic,
    })
}

/// The 2D form of a NURBS curve, for SP_CURVE parameter-space geometry.
fn nurbs_curve2(e: &RawEntity, index: &Index) -> Result<NurbsCurve2, String> {
    let e = if e.type_id == xt::B_CURVE {
        index
            .get(&ptr(e, 7))
            .ok_or("B_CURVE points at nothing")?
    } else {
        &e
    };
    if e.type_id != xt::NURBS_CURVE {
        return Err(format!("expected NURBS_CURVE, got type {}", e.type_id));
    }
    let degree = f64_at(e, 0) as usize;
    let n_verts = f64_at(e, 1) as usize;
    let dim = f64_at(e, 2) as usize;
    let rational = e.fields.get(7).map(|f| f.as_bool()).unwrap_or(false);
    let raw = index
        .get(&ptr(e, 9))
        .map(|v| v.var_f64.as_slice())
        .unwrap_or(&[]);
    let knots = expanded_knots(e, 10, 11, index)?;

    // 2D control points: dim 2, or dim 3 homogeneous when rational.
    let per = if raw.is_empty() || n_verts == 0 {
        return Err("SP_CURVE parameter spline has no vertices".into());
    } else {
        dim.max(raw.len() / n_verts)
    };
    let mut control_points = Vec::with_capacity(n_verts);
    let mut weights = Vec::new();
    for chunk in raw.chunks_exact(per).take(n_verts) {
        if rational && per >= 3 {
            let w = chunk[per - 1];
            let inv = if w.abs() > 1e-300 { 1.0 / w } else { 1.0 };
            control_points.push(Vec2::new(chunk[0] * inv, chunk[1] * inv));
            weights.push(w);
        } else {
            control_points.push(Vec2::new(chunk[0], chunk[1]));
        }
    }
    Ok(NurbsCurve2 {
        degree,
        control_points,
        weights,
        knots,
    })
}

/// NURBS_SURF fields: u_periodic[0], v_periodic[1], u_degree[2], v_degree[3],
/// n_u_vertices[4], n_v_vertices[5], rational[10], u_closed[11], v_closed[12],
/// vertex_dim[14], vertices[15], u_knot_mult[16], v_knot_mult[17],
/// u_knot[18], v_knot[19].
fn nurbs_surface(e: &RawEntity, index: &Index) -> Result<NurbsSurface, String> {
    if e.type_id != xt::NURBS_SURF {
        return Err(format!("expected NURBS_SURF, got type {}", e.type_id));
    }
    let u_periodic = e.fields.get(0).map(|f| f.as_bool()).unwrap_or(false);
    let v_periodic = e.fields.get(1).map(|f| f.as_bool()).unwrap_or(false);
    let u_degree = f64_at(e, 2) as usize;
    let v_degree = f64_at(e, 3) as usize;
    let n_u = f64_at(e, 4) as usize;
    let n_v = f64_at(e, 5) as usize;
    let rational = e.fields.get(10).map(|f| f.as_bool()).unwrap_or(false);
    let u_closed = e.fields.get(11).map(|f| f.as_bool()).unwrap_or(false);
    let v_closed = e.fields.get(12).map(|f| f.as_bool()).unwrap_or(false);
    let dim = f64_at(e, 14) as usize;

    let raw = index
        .get(&ptr(e, 15))
        .map(|v| v.var_f64.as_slice())
        .unwrap_or(&[]);
    let u_knots = expanded_knots(e, 16, 18, index)?;
    let v_knots = expanded_knots(e, 17, 19, index)?;

    let total = n_u * n_v;
    let (flat, flat_w) = split_poles(raw, total, dim, rational)?;

    // The control grid is written with U varying fastest, so point (i, j)
    // — u index i, v index j — sits at j·n_u + i.
    let mut control_points = vec![vec![Vec3::ZERO; n_v]; n_u];
    let mut weights = if rational {
        vec![vec![1.0f64; n_v]; n_u]
    } else {
        Vec::new()
    };
    for j in 0..n_v {
        for i in 0..n_u {
            let k = j * n_u + i;
            control_points[i][j] = *flat.get(k).unwrap_or(&Vec3::ZERO);
            if rational {
                weights[i][j] = flat_w.get(k).copied().unwrap_or(1.0);
            }
        }
    }

    Ok(NurbsSurface {
        u_degree,
        v_degree,
        control_points,
        weights,
        u_knots,
        v_knots,
        u_closed: u_closed || u_periodic,
        v_closed: v_closed || v_periodic,
    })
}

/// KNOT_MULT (i16 array) + KNOT_SET (f64 array) → the full knot vector.
fn expanded_knots(
    e: &RawEntity,
    mult_field: usize,
    set_field: usize,
    index: &Index,
) -> Result<Vec<f64>, String> {
    let mults = index
        .get(&ptr(e, mult_field))
        .map(|m| m.var_i16.as_slice())
        .unwrap_or(&[]);
    let knots = index
        .get(&ptr(e, set_field))
        .map(|k| k.var_f64.as_slice())
        .unwrap_or(&[]);
    if knots.is_empty() {
        return Err("knot set is empty".into());
    }
    let mut out = Vec::new();
    for (i, &k) in knots.iter().enumerate() {
        let m = mults.get(i).copied().unwrap_or(1).max(0) as usize;
        out.extend(std::iter::repeat_n(k, m));
    }
    Ok(out)
}

/// Split a flat pole array into points and weights.
///
/// Rational poles are homogeneous `[x·w, y·w, z·w, w]` (2D: `[x·w, y·w, w]`).
fn split_poles(
    raw: &[f64],
    n: usize,
    dim: usize,
    rational: bool,
) -> Result<(Vec<Vec3>, Vec<f64>), String> {
    if n == 0 {
        return Err("spline has no control points".into());
    }
    let per = if dim > 0 { dim } else { raw.len() / n };
    if raw.len() < n * per {
        return Err(format!(
            "pole array holds {} floats, {n}×{per} needed",
            raw.len()
        ));
    }
    let mut points = Vec::with_capacity(n);
    let mut weights = Vec::new();
    for chunk in raw.chunks_exact(per).take(n) {
        if rational && per >= 4 {
            let w = chunk[per - 1];
            let inv = if w.abs() > 1e-300 { 1.0 / w } else { 1.0 };
            points.push(Vec3::new(chunk[0] * inv, chunk[1] * inv, chunk[2] * inv));
            weights.push(w);
        } else {
            points.push(Vec3::new(
                chunk[0],
                chunk.get(1).copied().unwrap_or(0.0),
                chunk.get(2).copied().unwrap_or(0.0),
            ));
        }
    }
    Ok((points, weights))
}
