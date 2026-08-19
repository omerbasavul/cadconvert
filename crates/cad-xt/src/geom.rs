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

/// Integer field — degrees, counts and dims arrive as `Short`, and reading
/// them through a float accessor that did not know `Short` is precisely how
/// every spline in the Solid Edge file flattened to degree zero.
fn int_at(e: &RawEntity, i: usize) -> usize {
    e.fields.get(i).map(|f| f.as_i64().max(0) as usize).unwrap_or(0)
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
            // pvec[7], normal[8], and — measured on the Solid Edge corpus,
            // against this parser's old belief that planes store no x-axis —
            // the in-plane reference direction at [9]. It is not optional
            // decoration: SP_CURVE parameter coordinates live in this exact
            // frame, and one plane in the corpus anchors its parameter origin
            // half a kilometre from the part, with the pcurves' v ≈ +500 m
            // bringing geometry back. An arbitrary completion axis turns those
            // coordinates into points a kilometre off the body.
            let normal = v3(e, 8);
            let stored_x = v3(e, 9);
            let ref_dir = if stored_x.length_squared() > 1e-12 {
                stored_x
            } else {
                normal.any_perpendicular()
            };
            Ok(Surface::Plane {
                frame: Frame::new(v3(e, 7), normal, ref_dir),
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
        xt::SWEPT_SURF => {
            let profile = index
                .get(&ptr(e, 7))
                .ok_or("SWEPT_SURF has no profile curve")?;
            Ok(Surface::LinearExtrusion {
                profile: Box::new(curve(profile, index)?),
                direction: v3(e, 8),
            })
        }
        xt::SPUN_SURF => {
            let profile = index
                .get(&ptr(e, 7))
                .ok_or("SPUN_SURF has no profile curve")?;
            let axis_point = v3(e, 8);
            let axis_dir = v3(e, 9);
            Ok(Surface::Revolution {
                profile: Box::new(curve(profile, index)?),
                frame: Frame::new(axis_point, axis_dir, axis_dir.any_perpendicular()),
            })
        }
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
        // As an edge's 3D geometry an SP_CURVE is materialised by sampling —
        // as a fin's parameter-space curve it stays 2D via [`pcurve_of`].
        xt::SP_CURVE => sp_curve_polyline(e, index),
        other => Err(format!("curve type {other} not lowered yet")),
    }
}

/// Materialise an SP_CURVE as a 3D polyline by sampling its parameter-space
/// spline through its surface.
///
/// Tolerant edges carry no 3D curve at all — their geometry lives entirely in
/// each fin's SP_CURVE — so without this, every tolerant edge is lost. The
/// sample density follows the spline's own complexity and the polyline then
/// behaves like any other curve: invertible, discretisable, range-recoverable.
pub fn sp_curve_polyline(e: &RawEntity, index: &Index) -> Result<Curve, String> {
    // A fin's parameter curve is routinely a TRIMMED_CURVE wrapping the
    // SP_CURVE, carrying the exact parameter window; unwrap and honour it.
    if e.type_id == xt::TRIMMED_CURVE {
        let basis = index
            .get(&ptr(e, 7))
            .ok_or("trimmed pcurve's basis does not exist")?;
        let (t0, t1) = (f64_at(e, 10), f64_at(e, 11));
        return sp_curve_polyline_over(basis, index, Some((t0.min(t1), t0.max(t1))));
    }
    sp_curve_polyline_over(e, index, None)
}

fn sp_curve_polyline_over(
    e: &RawEntity,
    index: &Index,
    window: Option<(f64, f64)>,
) -> Result<Curve, String> {
    if e.type_id != xt::SP_CURVE {
        return Err(format!("expected SP_CURVE, got type {}", e.type_id));
    }
    let surf_entity = index
        .get(&ptr(e, 7))
        .ok_or("SP_CURVE's surface does not exist")?;
    let surf = surface(surf_entity, index)?;
    let bcurve = index
        .get(&ptr(e, 8))
        .ok_or("SP_CURVE has no parameter curve")?;
    let n2 = nurbs_curve2(bcurve, index)?;

    let samples = (n2.control_points.len() * n2.degree.max(1) * 4).clamp(24, 128);
    let knots = &n2.knots;
    let (mut lo, mut hi) = if knots.len() >= 2 * (n2.degree + 1) {
        (knots[n2.degree], knots[knots.len() - 1 - n2.degree])
    } else {
        (0.0, 1.0)
    };
    if let Some((wlo, whi)) = window {
        // The trim window is within the spline's own domain; a hair of slack
        // for the usual last-ulp writers.
        lo = wlo.max(lo - 1e-9);
        hi = whi.min(hi + 1e-9);
    }
    if !(hi > lo) {
        return Err("SP_CURVE parameter spline has an empty domain".into());
    }

    let cps: Vec<[f64; 2]> = n2.control_points.iter().map(|p| [p.u, p.v]).collect();
    let hom: Vec<[f64; 3]> = cps
        .iter()
        .enumerate()
        .map(|(i, p)| {
            let w = n2.weights.get(i).copied().unwrap_or(1.0);
            [p[0] * w, p[1] * w, w]
        })
        .collect();

    // XT parameterises a spun surface as (u along the profile, v the angle);
    // cad_ir's Revolution is the transpose, so SP_CURVE coordinates on one
    // swap before evaluation. Verified empirically: without the swap, the
    // angle lands in the profile parameter of an unbounded profile line and
    // tolerant edges on spun faces sample hundreds of metres away.
    let swap = surf_entity.type_id == xt::SPUN_SURF;
    let mut points = Vec::with_capacity(samples + 1);
    for k in 0..=samples {
        let t = lo + (hi - lo) * k as f64 / samples as f64;
        let uvw = cad_ir::eval::nurbs::de_boor(n2.degree, &hom, knots, t);
        let inv = if uvw[2].abs() > 1e-300 { 1.0 / uvw[2] } else { 1.0 };
        let (a, b) = (uvw[0] * inv, uvw[1] * inv);
        let uv = if swap { Vec2::new(b, a) } else { Vec2::new(a, b) };
        points.push(surf.point_at(uv));
    }
    if points.len() < 2 {
        return Err("SP_CURVE sampled to fewer than two points".into());
    }
    if std::env::var_os("XT_SPC_TRACE").is_some() {
        let mut lo = points[0];
        let mut hi = points[0];
        for p in &points {
            lo = lo.min(*p);
            hi = hi.max(*p);
        }
        if (hi - lo).length() > 10.0 {
            let (ulo, uhi) = n2
                .control_points
                .iter()
                .fold((f64::INFINITY, f64::NEG_INFINITY), |(a, b), p| {
                    (a.min(p.u).min(p.v), b.max(p.u).max(p.v))
                });
            eprintln!(
                "[spc] #{} span {:.1} m  deg={} n={} weights={} knots[{}]=[{:.3}..{:.3}] window={:?} uv_cp=[{ulo:.3}..{uhi:.3}] surface type {}",
                e.index,
                (hi - lo).length(),
                n2.degree,
                n2.control_points.len(),
                n2.weights.len(),
                knots.len(),
                knots.first().copied().unwrap_or(0.0),
                knots.last().copied().unwrap_or(0.0),
                window,
                surf_entity.type_id,
            );
        }
    }
    Ok(Curve::Polyline { points })
}

/// A fin's pcurve, when its curve pointer is an SP_CURVE.
pub fn pcurve_of(e: &RawEntity, index: &Index) -> Option<Curve2> {
    if e.type_id != xt::SP_CURVE {
        return None;
    }
    let bcurve = index.get(&ptr(e, 8))?;
    nurbs_curve2(bcurve, index).ok().map(Curve2::Nurbs)
}

/// CHART (40): the first point lives in fixed field 6 (the h-vector the base
/// schema declares), and the remaining points are bare `x y z` runs in the
/// variable tail — measured on the Solid Edge corpus, where a three-point
/// chart is `fields[6] = p₀` plus six floats.
fn chart_points(chart: &RawEntity) -> Vec<Vec3> {
    let mut out = Vec::new();
    if let Some(first) = chart.fields.get(6).map(|f| f.as_vec3())
        && first.iter().all(|v| v.is_finite())
    {
        out.push(Vec3::new(first[0], first[1], first[2]));
    }
    for c in chart.var_f64.chunks_exact(3) {
        out.push(Vec3::new(c[0], c[1], c[2]));
    }

    // Some Solid Edge charts carry a scale-mixed prefix — entries hundreds of
    // metres out on a millimetre part, most plausibly homogeneous forms or
    // tangent data, followed by the actual points. One such entry poisons the
    // synthetic vertices, the body's reference box and the escape guard in
    // one stroke, so filter by consensus: the majority of a chart's entries
    // agree about the curve's scale, and anything an order of magnitude
    // outside that agreement is not a point on it.
    if out.len() >= 4 {
        let mut mags: Vec<f64> = out.iter().map(|p| p.length()).collect();
        mags.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
        let median = mags[mags.len() / 2].max(1e-12);
        let filtered: Vec<Vec3> = out
            .iter()
            .copied()
            .filter(|p| p.length() <= median * 20.0)
            .collect();
        if filtered.len() >= 2 {
            return filtered;
        }
    }
    out
}

/// NURBS_CURVE fields: degree[0], n_vertices[1], vertex_dim[2], n_knots[3],
/// knot_type[4], periodic[5], closed[6], rational[7], (form[8]), vertices[9],
/// knot_mult[10], knot[11].
fn nurbs_curve(e: &RawEntity, index: &Index) -> Result<NurbsCurve, String> {
    if e.type_id != xt::NURBS_CURVE {
        return Err(format!("expected NURBS_CURVE, got type {}", e.type_id));
    }
    let degree = int_at(e, 0);
    let n_verts = int_at(e, 1);
    let dim = int_at(e, 2);
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
    let degree = int_at(e, 0);
    let n_verts = int_at(e, 1);
    let dim = int_at(e, 2);
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
    let u_degree = int_at(e, 2);
    let v_degree = int_at(e, 3);
    let n_u = int_at(e, 4);
    let n_v = int_at(e, 5);
    let rational = e.fields.get(10).map(|f| f.as_bool()).unwrap_or(false);
    let u_closed = e.fields.get(11).map(|f| f.as_bool()).unwrap_or(false);
    let v_closed = e.fields.get(12).map(|f| f.as_bool()).unwrap_or(false);
    let dim = int_at(e, 14);

    let raw = index
        .get(&ptr(e, 15))
        .map(|v| v.var_f64.as_slice())
        .unwrap_or(&[]);
    let u_knots = expanded_knots(e, 16, 18, index)?;
    let v_knots = expanded_knots(e, 17, 19, index)?;

    let total = n_u * n_v;
    let (flat, flat_w) = split_poles(raw, total, dim, rational)?;

    // The pole grid is stored row-by-row in u — v varies fastest, so point
    // (i, j) sits at i·n_v + j. Decided by measurement, not the format notes:
    // inverting every spline face's boundary midpoint onto both layouts put
    // 1239 of 1343 faces within 0.01 mm this way and 245 the other.
    let mut control_points = vec![vec![Vec3::ZERO; n_v]; n_u];
    let mut weights = if rational {
        vec![vec![1.0f64; n_v]; n_u]
    } else {
        Vec::new()
    };
    for (i, row) in control_points.iter_mut().enumerate() {
        for (j, slot) in row.iter_mut().enumerate() {
            let k = i * n_v + j;
            *slot = *flat.get(k).unwrap_or(&Vec3::ZERO);
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
