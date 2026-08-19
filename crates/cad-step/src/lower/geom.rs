//! Lowering STEP geometry entities into [`cad_ir`] curves and surfaces.
//!
//! Two shapes of the same entity have to be handled everywhere. A simple
//! instance lists every inherited attribute:
//!
//! ```text
//! #7=B_SPLINE_CURVE_WITH_KNOTS('',3,(#1,#2,#3,#4),.UNSPECIFIED.,.F.,.F.,
//!                              (4,4),(0.,1.),.UNSPECIFIED.);
//! ```
//!
//! while a complex instance splits the attributes across its parts, each
//! listing only what its own subtype declares — and a rational curve or
//! surface is *always* complex, because rationality is a sibling subtype:
//!
//! ```text
//! #7=(BOUNDED_CURVE()B_SPLINE_CURVE(3,(#1,#2,#3,#4),.UNSPECIFIED.,.F.,.F.)
//!     B_SPLINE_CURVE_WITH_KNOTS((4,4),(0.,1.),.UNSPECIFIED.)
//!     CURVE()GEOMETRIC_REPRESENTATION_ITEM()RATIONAL_B_SPLINE_CURVE((1.,0.8,0.8,1.))
//!     REPRESENTATION_ITEM(''));
//! ```
//!
//! Reading the complex form as if it were simple silently shifts every
//! attribute by one, which produces a plausible-looking curve of the wrong
//! degree — so the two paths are kept explicit rather than unified.

use crate::error::{Result, StepError};
use crate::kind::Kind;
use crate::{Args, StepFile};
use cad_ir::{Curve, Curve2, Frame, Interval, NurbsCurve, NurbsCurve2, NurbsSurface, Surface};
use cad_ir::{Vec2, Vec3};

/// Read a `CARTESIAN_POINT`.
pub fn point(file: &StepFile, id: u32) -> Result<Vec3> {
    let mut a = file.args_checked(id, Kind::CartesianPoint)?;
    a.skip()?; // name
    let mut xyz = Vec::new();
    a.next_f64_list(&mut xyz)?;
    Ok(Vec3::from_slice(&xyz))
}

/// Read a `CARTESIAN_POINT` in a surface's parameter space.
pub fn point2(file: &StepFile, id: u32) -> Result<Vec2> {
    let mut a = file.args_checked(id, Kind::CartesianPoint)?;
    a.skip()?; // name
    let mut uv = Vec::new();
    a.next_f64_list(&mut uv)?;
    Ok(Vec2::new(
        uv.first().copied().unwrap_or(0.0),
        uv.get(1).copied().unwrap_or(0.0),
    ))
}

/// Read a `DIRECTION`, unnormalised.
pub fn direction(file: &StepFile, id: u32) -> Result<Vec3> {
    let mut a = file.args_checked(id, Kind::Direction)?;
    a.skip()?; // name
    let mut d = Vec::new();
    a.next_f64_list(&mut d)?;
    Ok(Vec3::from_slice(&d))
}

fn direction2(file: &StepFile, id: u32) -> Result<Vec2> {
    let mut a = file.args_checked(id, Kind::Direction)?;
    a.skip()?; // name
    let mut d = Vec::new();
    a.next_f64_list(&mut d)?;
    Ok(Vec2::new(
        d.first().copied().unwrap_or(0.0),
        d.get(1).copied().unwrap_or(0.0),
    ))
}

/// Read a `VECTOR`, returning direction × magnitude.
///
/// The magnitude is the parameterisation's scale: a `LINE` advances by the full
/// vector per unit of parameter, so dropping it would make every trim range on
/// that line wrong by a constant factor.
pub fn vector(file: &StepFile, id: u32) -> Result<Vec3> {
    let mut a = file.args_checked(id, Kind::Vector)?;
    a.skip()?; // name
    let dir = direction(file, a.next_ref()?)?;
    let magnitude = a.next_measure_f64()?;
    Ok(dir.normalized_or(Vec3::X) * magnitude)
}

fn vector2(file: &StepFile, id: u32) -> Result<Vec2> {
    let mut a = file.args_checked(id, Kind::Vector)?;
    a.skip()?; // name
    let d = direction2(file, a.next_ref()?)?;
    let magnitude = a.next_measure_f64()?;
    let len = (d.u * d.u + d.v * d.v).sqrt();
    Ok(if len > 0.0 {
        Vec2::new(d.u / len * magnitude, d.v / len * magnitude)
    } else {
        Vec2::new(magnitude, 0.0)
    })
}

/// Read an `AXIS2_PLACEMENT_3D`.
///
/// Both direction attributes are optional; STEP's defaults are +Z and +X.
pub fn placement(file: &StepFile, id: u32) -> Result<Frame> {
    let mut a = file.args_checked(id, Kind::Axis2Placement3d)?;
    a.skip()?; // name
    let origin = point(file, a.next_ref()?)?;
    let axis = match a.next_opt_ref()? {
        Some(r) => direction(file, r)?,
        None => Vec3::Z,
    };
    let ref_dir = match a.next_opt_ref()? {
        Some(r) => direction(file, r)?,
        None => Vec3::X,
    };
    Ok(Frame::new(origin, axis, ref_dir))
}

/// Read an `AXIS1_PLACEMENT` — origin and axis only.
pub fn axis1(file: &StepFile, id: u32) -> Result<Frame> {
    let mut a = file.args_checked(id, Kind::Axis1Placement)?;
    a.skip()?; // name
    let origin = point(file, a.next_ref()?)?;
    let axis = match a.next_opt_ref()? {
        Some(r) => direction(file, r)?,
        None => Vec3::Z,
    };
    Ok(Frame::new(origin, axis, axis.any_perpendicular()))
}

/// A placement that may be 2D or 3D, as `CIRCLE` and friends allow.
fn conic_frame(file: &StepFile, id: u32) -> Result<Frame> {
    match file.kind_of(id) {
        Kind::Axis2Placement3d => placement(file, id),
        Kind::Axis2Placement2d => {
            // A 2D placement appears only inside a pcurve's definitional
            // representation, where the "plane" is the surface's UV space.
            let mut a = file.args_checked(id, Kind::Axis2Placement2d)?;
            a.skip()?;
            let o = point2(file, a.next_ref()?)?;
            let r = match a.next_opt_ref()? {
                Some(r) => direction2(file, r)?,
                None => Vec2::new(1.0, 0.0),
            };
            Ok(Frame::new(
                Vec3::new(o.u, o.v, 0.0),
                Vec3::Z,
                Vec3::new(r.u, r.v, 0.0),
            ))
        }
        _ => Err(StepError::WrongKind {
            id,
            actual: file
                .get(id)
                .map(|e| {
                    if e.kind == Kind::Other {
                        file.keyword(e).to_string()
                    } else {
                        e.kind.as_str().to_string()
                    }
                })
                .unwrap_or_else(|| "(dangling)".into()),
            expected: "AXIS2_PLACEMENT_3D",
        }),
    }
}

// ---------------------------------------------------------------------------
// Surfaces
// ---------------------------------------------------------------------------

/// Lower any surface entity.
pub fn surface(file: &StepFile, id: u32) -> Result<Surface> {
    let e = file.require(id)?;
    match e.kind {
        Kind::Plane => {
            let mut a = file.args_of(e);
            a.skip()?;
            Ok(Surface::Plane {
                frame: placement(file, a.next_ref()?)?,
            })
        }
        Kind::CylindricalSurface => {
            let mut a = file.args_of(e);
            a.skip()?;
            let frame = placement(file, a.next_ref()?)?;
            Ok(Surface::Cylinder {
                frame,
                radius: a.next_measure_f64()?,
            })
        }
        Kind::ConicalSurface => {
            let mut a = file.args_of(e);
            a.skip()?;
            let frame = placement(file, a.next_ref()?)?;
            let radius = a.next_measure_f64()?;
            let half_angle = a.next_measure_f64()?;
            Ok(Surface::Cone {
                frame,
                radius,
                half_angle,
            })
        }
        Kind::SphericalSurface => {
            let mut a = file.args_of(e);
            a.skip()?;
            let frame = placement(file, a.next_ref()?)?;
            Ok(Surface::Sphere {
                frame,
                radius: a.next_measure_f64()?,
            })
        }
        // A degenerate torus has minor > major and self-intersects; its
        // `select_outer` flag chooses a sheet. The trim loops already select
        // the patch that is actually used, so the flag changes nothing here.
        Kind::ToroidalSurface | Kind::DegenerateToroidalSurface => {
            let mut a = file.args_of(e);
            a.skip()?;
            let frame = placement(file, a.next_ref()?)?;
            let major_radius = a.next_measure_f64()?;
            let minor_radius = a.next_measure_f64()?;
            Ok(Surface::Torus {
                frame,
                major_radius,
                minor_radius,
            })
        }
        Kind::SurfaceOfLinearExtrusion => {
            let mut a = file.args_of(e);
            a.skip()?;
            let profile = curve(file, a.next_ref()?)?;
            Ok(Surface::LinearExtrusion {
                profile: Box::new(profile),
                direction: vector(file, a.next_ref()?)?,
            })
        }
        Kind::SurfaceOfRevolution => {
            let mut a = file.args_of(e);
            a.skip()?;
            let profile = curve(file, a.next_ref()?)?;
            Ok(Surface::Revolution {
                profile: Box::new(profile),
                frame: axis1(file, a.next_ref()?)?,
            })
        }
        Kind::OffsetSurface => {
            let mut a = file.args_of(e);
            a.skip()?;
            let base = surface(file, a.next_ref()?)?;
            Ok(Surface::Offset {
                base: Box::new(base),
                distance: a.next_measure_f64()?,
            })
        }
        Kind::RectangularTrimmedSurface => {
            let mut a = file.args_of(e);
            a.skip()?;
            let base = surface(file, a.next_ref()?)?;
            let u1 = a.next_measure_f64()?;
            let u2 = a.next_measure_f64()?;
            let v1 = a.next_measure_f64()?;
            let v2 = a.next_measure_f64()?;
            Ok(Surface::RectangularTrimmed {
                base: Box::new(base),
                u: Interval::new(u1.min(u2), u1.max(u2)),
                v: Interval::new(v1.min(v2), v1.max(v2)),
            })
        }
        Kind::BSplineSurfaceWithKnots => Ok(Surface::Nurbs(nurbs_surface_simple(file, id)?)),
        Kind::Complex => Ok(Surface::Nurbs(nurbs_surface_complex(file, id)?)),
        _ => Err(unsupported(file, id, "surface")),
    }
}

/// `B_SPLINE_SURFACE_WITH_KNOTS` written as a simple, non-rational instance.
fn nurbs_surface_simple(file: &StepFile, id: u32) -> Result<NurbsSurface> {
    let mut a = file.args_checked(id, Kind::BSplineSurfaceWithKnots)?;
    a.skip()?; // name
    let base = read_surface_base(file, &mut a)?;
    let knots = read_surface_knots(&mut a)?;
    assemble_surface(base, knots, Vec::new())
}

/// A complex instance bundling the B-spline subtypes, possibly with weights.
fn nurbs_surface_complex(file: &StepFile, id: u32) -> Result<NurbsSurface> {
    let e = file.require(id)?;
    let parts = file.complex_parts(e)?;

    let mut base = None;
    let mut knots = None;
    let mut weights = Vec::new();
    for (kind, mut a) in parts {
        match kind {
            Kind::BSplineSurface => base = Some(read_surface_base(file, &mut a)?),
            Kind::BSplineSurfaceWithKnots => knots = Some(read_surface_knots(&mut a)?),
            Kind::RationalBSplineSurface => {
                let mut grid = Vec::new();
                a.next_f64_grid(&mut grid)?;
                weights = grid;
            }
            _ => {}
        }
    }

    let (Some(base), Some(knots)) = (base, knots) else {
        return Err(StepError::Record {
            offset: 0,
            detail: format!(
                "#{id} is a complex instance without both B_SPLINE_SURFACE and \
                 B_SPLINE_SURFACE_WITH_KNOTS parts"
            ),
        });
    };
    assemble_surface(base, knots, weights)
}

struct SurfaceBase {
    u_degree: usize,
    v_degree: usize,
    control_points: Vec<Vec<Vec3>>,
    u_closed: bool,
    v_closed: bool,
}

struct SurfaceKnots {
    u_mult: Vec<i64>,
    v_mult: Vec<i64>,
    u_knots: Vec<f64>,
    v_knots: Vec<f64>,
}

/// `u_degree, v_degree, control_points_list, surface_form, u_closed, v_closed,
/// self_intersect` — the attributes `B_SPLINE_SURFACE` itself declares.
fn read_surface_base(file: &StepFile, a: &mut Args<'_>) -> Result<SurfaceBase> {
    let u_degree = a.next_i64()?.max(0) as usize;
    let v_degree = a.next_i64()?.max(0) as usize;
    let mut grid = Vec::new();
    a.next_ref_grid(&mut grid)?;
    let mut control_points = Vec::with_capacity(grid.len());
    for row in &grid {
        let mut r = Vec::with_capacity(row.len());
        for &p in row {
            r.push(point(file, p)?);
        }
        control_points.push(r);
    }
    a.skip()?; // surface_form
    let u_closed = a.next_bool()?.unwrap_or(false);
    let v_closed = a.next_bool()?.unwrap_or(false);
    a.skip()?; // self_intersect
    Ok(SurfaceBase {
        u_degree,
        v_degree,
        control_points,
        u_closed,
        v_closed,
    })
}

/// `u_multiplicities, v_multiplicities, u_knots, v_knots, knot_spec`.
fn read_surface_knots(a: &mut Args<'_>) -> Result<SurfaceKnots> {
    let mut u_mult = Vec::new();
    let mut v_mult = Vec::new();
    let mut u_knots = Vec::new();
    let mut v_knots = Vec::new();
    a.next_i64_list(&mut u_mult)?;
    a.next_i64_list(&mut v_mult)?;
    a.next_f64_list(&mut u_knots)?;
    a.next_f64_list(&mut v_knots)?;
    Ok(SurfaceKnots {
        u_mult,
        v_mult,
        u_knots,
        v_knots,
    })
}

fn assemble_surface(
    base: SurfaceBase,
    knots: SurfaceKnots,
    weights: Vec<Vec<f64>>,
) -> Result<NurbsSurface> {
    Ok(NurbsSurface {
        u_degree: base.u_degree,
        v_degree: base.v_degree,
        control_points: base.control_points,
        weights,
        u_knots: expand_knots(&knots.u_knots, &knots.u_mult),
        v_knots: expand_knots(&knots.v_knots, &knots.v_mult),
        u_closed: base.u_closed,
        v_closed: base.v_closed,
    })
}

// ---------------------------------------------------------------------------
// Curves
// ---------------------------------------------------------------------------

/// Lower any 3D curve entity.
pub fn curve(file: &StepFile, id: u32) -> Result<Curve> {
    let e = file.require(id)?;
    match e.kind {
        Kind::Line => {
            let mut a = file.args_of(e);
            a.skip()?;
            let origin = point(file, a.next_ref()?)?;
            Ok(Curve::Line {
                origin,
                direction: vector(file, a.next_ref()?)?,
            })
        }
        Kind::Circle => {
            let mut a = file.args_of(e);
            a.skip()?;
            let frame = conic_frame(file, a.next_ref()?)?;
            Ok(Curve::Circle {
                frame,
                radius: a.next_measure_f64()?,
            })
        }
        Kind::Ellipse => {
            let mut a = file.args_of(e);
            a.skip()?;
            let frame = conic_frame(file, a.next_ref()?)?;
            Ok(Curve::Ellipse {
                frame,
                semi_major: a.next_measure_f64()?,
                semi_minor: a.next_measure_f64()?,
            })
        }
        Kind::Hyperbola => {
            let mut a = file.args_of(e);
            a.skip()?;
            let frame = conic_frame(file, a.next_ref()?)?;
            Ok(Curve::Hyperbola {
                frame,
                semi_major: a.next_measure_f64()?,
                semi_minor: a.next_measure_f64()?,
            })
        }
        Kind::Parabola => {
            let mut a = file.args_of(e);
            a.skip()?;
            let frame = conic_frame(file, a.next_ref()?)?;
            Ok(Curve::Parabola {
                frame,
                focal_dist: a.next_measure_f64()?,
            })
        }
        Kind::Polyline => {
            let mut a = file.args_of(e);
            a.skip()?;
            let mut refs = Vec::new();
            a.next_ref_list(&mut refs)?;
            let mut points = Vec::with_capacity(refs.len());
            for r in refs {
                points.push(point(file, r)?);
            }
            Ok(Curve::Polyline { points })
        }
        Kind::BSplineCurveWithKnots => Ok(Curve::Nurbs(nurbs_curve_simple(file, id)?)),
        Kind::TrimmedCurve => trimmed_curve(file, id),
        Kind::CompositeCurve => composite_curve(file, id),

        // `SURFACE_CURVE(name, curve_3d, associated_geometry,
        //  master_representation)` — the 3D form is the first attribute, and
        // is the one to use; the associated pcurves are picked up separately by
        // the topology layer, which knows which face is asking.
        Kind::SurfaceCurve | Kind::SeamCurve | Kind::IntersectionCurve => {
            let mut a = file.args_of(e);
            a.skip()?;
            curve(file, a.next_ref()?)
        }

        // An offset curve's 3D form is the base displaced along a reference
        // direction. Approximating it by its base would silently move an edge,
        // so it is refused rather than guessed at.
        Kind::OffsetCurve3d => Err(unsupported(file, id, "curve")),

        Kind::Complex => Ok(Curve::Nurbs(nurbs_curve_complex(file, id)?)),
        _ => Err(unsupported(file, id, "curve")),
    }
}

/// `TRIMMED_CURVE(name, basis_curve, trim_1, trim_2, sense_agreement,
/// master_representation)`.
///
/// Each trim is a set that may hold a parameter value, a cartesian point, or
/// both; `master_representation` says which to believe. The parameter is taken
/// where present because a point has to be inverted onto the curve, which is
/// ambiguous on a closed one.
fn trimmed_curve(file: &StepFile, id: u32) -> Result<Curve> {
    let mut a = file.args_checked(id, Kind::TrimmedCurve)?;
    a.skip()?; // name
    let base = curve(file, a.next_ref()?)?;
    let t1 = trim_parameter(&mut a)?;
    let t2 = trim_parameter(&mut a)?;
    let same_sense = a.next_bool()?.unwrap_or(true);

    let (lo, hi) = match (t1, t2) {
        (Some(x), Some(y)) if same_sense => (x, y),
        (Some(x), Some(y)) => (y, x),
        // Without parameters the trim is expressed only as points, which the
        // topology layer resolves from the edge's own vertices.
        _ => return Ok(base),
    };
    Ok(Curve::Trimmed {
        base: Box::new(base),
        range: Interval::new(lo, hi),
    })
}

/// Pull the `PARAMETER_VALUE(…)` out of a trim set, ignoring any point.
fn trim_parameter(a: &mut Args<'_>) -> Result<Option<f64>> {
    let v = a.next_value()?;
    let items: &[crate::Value<'_>] = match &v {
        crate::Value::List(items) => items,
        other => std::slice::from_ref(other),
    };
    for item in items {
        match item {
            crate::Value::Typed(kw, inner) if kw.eq_ignore_ascii_case("PARAMETER_VALUE") => {
                if let Some(x) = inner.first().and_then(|x| x.as_f64()) {
                    return Ok(Some(x));
                }
            }
            crate::Value::Real(x) => return Ok(Some(*x)),
            crate::Value::Int(x) => return Ok(Some(*x as f64)),
            _ => {}
        }
    }
    Ok(None)
}

/// `COMPOSITE_CURVE(name, segments, self_intersect)`.
fn composite_curve(file: &StepFile, id: u32) -> Result<Curve> {
    let mut a = file.args_checked(id, Kind::CompositeCurve)?;
    a.skip()?; // name
    let mut refs = Vec::new();
    a.next_ref_list(&mut refs)?;

    let mut segments = Vec::with_capacity(refs.len());
    for r in refs {
        let mut s = file.args_checked(r, Kind::CompositeCurveSegment)?;
        s.skip()?; // transition
        let same_sense = s.next_bool()?.unwrap_or(true);
        let parent = s.next_ref()?;
        let c = curve(file, parent)?;
        // A segment inherits its parent curve's own range; a trimmed parent
        // already carries one, and anything else spans its natural domain,
        // which the tessellator derives from the curve type.
        let range = match &c {
            Curve::Trimmed { range, .. } => *range,
            _ => Interval::UNIT,
        };
        segments.push(cad_ir::brep::CompositeSegment {
            curve: c,
            range,
            same_sense,
        });
    }
    Ok(Curve::Composite { segments })
}

fn nurbs_curve_simple(file: &StepFile, id: u32) -> Result<NurbsCurve> {
    let mut a = file.args_checked(id, Kind::BSplineCurveWithKnots)?;
    a.skip()?; // name
    let base = read_curve_base(file, &mut a)?;
    let (mult, knots) = read_curve_knots(&mut a)?;
    Ok(NurbsCurve {
        degree: base.0,
        control_points: base.1,
        weights: Vec::new(),
        knots: expand_knots(&knots, &mult),
        closed: base.2,
    })
}

fn nurbs_curve_complex(file: &StepFile, id: u32) -> Result<NurbsCurve> {
    let e = file.require(id)?;
    let parts = file.complex_parts(e)?;

    let mut base = None;
    let mut knots = None;
    let mut weights = Vec::new();
    for (kind, mut a) in parts {
        match kind {
            Kind::BSplineCurve => base = Some(read_curve_base(file, &mut a)?),
            Kind::BSplineCurveWithKnots => knots = Some(read_curve_knots(&mut a)?),
            Kind::RationalBSplineCurve => a.next_f64_list(&mut weights)?,
            _ => {}
        }
    }

    let (Some(base), Some((mult, kn))) = (base, knots) else {
        return Err(StepError::Record {
            offset: 0,
            detail: format!(
                "#{id} is a complex instance without both B_SPLINE_CURVE and \
                 B_SPLINE_CURVE_WITH_KNOTS parts"
            ),
        });
    };
    Ok(NurbsCurve {
        degree: base.0,
        control_points: base.1,
        weights,
        knots: expand_knots(&kn, &mult),
        closed: base.2,
    })
}

/// `degree, control_points_list, curve_form, closed_curve, self_intersect`.
fn read_curve_base(file: &StepFile, a: &mut Args<'_>) -> Result<(usize, Vec<Vec3>, bool)> {
    let degree = a.next_i64()?.max(0) as usize;
    let mut refs = Vec::new();
    a.next_ref_list(&mut refs)?;
    let mut cps = Vec::with_capacity(refs.len());
    for r in refs {
        cps.push(point(file, r)?);
    }
    a.skip()?; // curve_form
    let closed = a.next_bool()?.unwrap_or(false);
    a.skip()?; // self_intersect
    Ok((degree, cps, closed))
}

/// `knot_multiplicities, knots, knot_spec`.
fn read_curve_knots(a: &mut Args<'_>) -> Result<(Vec<i64>, Vec<f64>)> {
    let mut mult = Vec::new();
    let mut knots = Vec::new();
    a.next_i64_list(&mut mult)?;
    a.next_f64_list(&mut knots)?;
    Ok((mult, knots))
}

/// Expand distinct knots plus multiplicities into the full knot vector.
///
/// STEP stores `((4,1,4),(0.,0.5,1.))`; every evaluation algorithm wants
/// `(0,0,0,0,0.5,1,1,1,1)`.
pub fn expand_knots(distinct: &[f64], multiplicities: &[i64]) -> Vec<f64> {
    let total: usize = multiplicities.iter().map(|m| (*m).max(0) as usize).sum();
    let mut out = Vec::with_capacity(total.max(distinct.len()));
    for (i, &k) in distinct.iter().enumerate() {
        let m = multiplicities.get(i).copied().unwrap_or(1).max(0) as usize;
        out.extend(std::iter::repeat_n(k, m));
    }
    out
}

// ---------------------------------------------------------------------------
// Parameter-space curves
// ---------------------------------------------------------------------------

/// Lower a `PCURVE`'s definitional curve into surface parameter space.
///
/// `PCURVE(name, basis_surface, reference_to_curve)` where the reference is a
/// `DEFINITIONAL_REPRESENTATION` holding one 2D curve.
pub fn pcurve(file: &StepFile, id: u32) -> Result<Option<Curve2>> {
    let mut a = file.args_checked(id, Kind::Pcurve)?;
    a.skip()?; // name
    a.skip()?; // basis_surface — the caller already knows which face this is
    let def = a.next_ref()?;

    let mut d = file.args_checked(def, Kind::DefinitionalRepresentation)?;
    d.skip()?; // name
    let mut items = Vec::new();
    d.next_ref_list(&mut items)?;
    let Some(&first) = items.first() else {
        return Ok(None);
    };
    curve2(file, first).map(Some)
}

/// Lower a 2D curve.
fn curve2(file: &StepFile, id: u32) -> Result<Curve2> {
    let e = file.require(id)?;
    match e.kind {
        Kind::Line => {
            let mut a = file.args_of(e);
            a.skip()?;
            let origin = point2(file, a.next_ref()?)?;
            Ok(Curve2::Line {
                origin,
                direction: vector2(file, a.next_ref()?)?,
            })
        }
        Kind::Polyline => {
            let mut a = file.args_of(e);
            a.skip()?;
            let mut refs = Vec::new();
            a.next_ref_list(&mut refs)?;
            let mut points = Vec::with_capacity(refs.len());
            for r in refs {
                points.push(point2(file, r)?);
            }
            Ok(Curve2::Polyline { points })
        }
        Kind::BSplineCurveWithKnots => {
            let mut a = file.args_checked(id, Kind::BSplineCurveWithKnots)?;
            a.skip()?;
            let (degree, cps, mult, knots) = read_curve2_body(file, &mut a)?;
            Ok(Curve2::Nurbs(NurbsCurve2 {
                degree,
                control_points: cps,
                weights: Vec::new(),
                knots: expand_knots(&knots, &mult),
            }))
        }
        Kind::Complex => {
            let parts = file.complex_parts(e)?;
            let mut degree = 0;
            let mut cps = Vec::new();
            let mut mult = Vec::new();
            let mut knots = Vec::new();
            let mut weights = Vec::new();
            for (kind, mut a) in parts {
                match kind {
                    Kind::BSplineCurve => {
                        degree = a.next_i64()?.max(0) as usize;
                        let mut refs = Vec::new();
                        a.next_ref_list(&mut refs)?;
                        cps.clear();
                        for r in refs {
                            cps.push(point2(file, r)?);
                        }
                        // curve_form, closed_curve, self_intersect follow, but
                        // nothing downstream reads them for a pcurve.
                    }
                    Kind::BSplineCurveWithKnots => {
                        a.next_i64_list(&mut mult)?;
                        a.next_f64_list(&mut knots)?;
                    }
                    Kind::RationalBSplineCurve => a.next_f64_list(&mut weights)?,
                    _ => {}
                }
            }
            Ok(Curve2::Nurbs(NurbsCurve2 {
                degree,
                control_points: cps,
                weights,
                knots: expand_knots(&knots, &mult),
            }))
        }
        // A 2D circle or conic in UV space is legal but vanishingly rare, and
        // approximating it wrongly would move a trim boundary. Fall back to
        // inverting the 3D curve, which is always available.
        _ => Ok(Curve2::Implied),
    }
}

fn read_curve2_body(
    file: &StepFile,
    a: &mut Args<'_>,
) -> Result<(usize, Vec<Vec2>, Vec<i64>, Vec<f64>)> {
    let degree = a.next_i64()?.max(0) as usize;
    let mut refs = Vec::new();
    a.next_ref_list(&mut refs)?;
    let mut cps = Vec::with_capacity(refs.len());
    for r in refs {
        cps.push(point2(file, r)?);
    }
    a.skip()?; // curve_form
    a.skip()?; // closed
    a.skip()?; // self_intersect
    let mut mult = Vec::new();
    let mut knots = Vec::new();
    a.next_i64_list(&mut mult)?;
    a.next_f64_list(&mut knots)?;
    Ok((degree, cps, mult, knots))
}

fn unsupported(file: &StepFile, id: u32, what: &'static str) -> StepError {
    StepError::WrongKind {
        id,
        actual: file
            .get(id)
            .map(|e| {
                if e.kind == Kind::Other {
                    file.keyword(e).to_string()
                } else {
                    e.kind.as_str().to_string()
                }
            })
            .unwrap_or_else(|| "(dangling)".into()),
        expected: what,
    }
}
