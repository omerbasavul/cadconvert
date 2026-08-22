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
pub(crate) fn int_at(e: &RawEntity, i: usize) -> usize {
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
                // The half-angle arrives split into its sine and cosine, and
                // it opens the same way cad_ir's does: measured over this
                // assembly's 1,207 conical boundary loops, taking it as
                // written leaves 131 with a point off the surface and the
                // worst 0.7 mm out, while negating it — which the code did,
                // on the belief that Parasolid narrows where cad_ir opens —
                // leaves 1,059 wrong and the worst 36.8 mm out. Every chamfer
                // and countersink in the model was the wrong cone.
                half_angle: sin_ha.atan2(cos_ha),
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
            let lowered = curve(profile, index)?;
            Ok(Surface::LinearExtrusion {
                profile: Box::new(lowered),
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
        // OFFSET_SURF: base surface at [9], the offset distance at [10].
        // A face on one is a real face — a wall thickened from another, a
        // relieved seat — and skipping it left its whole boundary open on
        // every neighbour.
        xt::OFFSET_SURF => {
            let base = index
                .get(&ptr(e, 9))
                .ok_or("OFFSET_SURF has no base surface")?;
            let distance = f64_at(e, 10);
            if !distance.is_finite() {
                return Err("OFFSET_SURF has no offset distance".into());
            }
            let signed = if geom_sense(e) == '-' { -distance } else { distance };
            let lowered = surface(base, index)?;
            if std::env::var_os("XT_OFFSET_TRACE").is_some() {
                let shape = match &lowered {
                    cad_ir::brep::Surface::Nurbs(n) => format!(
                        "nurbs {}x{} deg {}x{} u[{:.4},{:.4}] v[{:.4},{:.4}] closed u={} v={}",
                        n.control_points.len(),
                        n.control_points.first().map_or(0, |r| r.len()),
                        n.u_degree,
                        n.v_degree,
                        n.u_knots.first().copied().unwrap_or(0.0),
                        n.u_knots.last().copied().unwrap_or(0.0),
                        n.v_knots.first().copied().unwrap_or(0.0),
                        n.v_knots.last().copied().unwrap_or(0.0),
                        n.u_closed,
                        n.v_closed,
                    ),
                    other => format!("{:?}", std::mem::discriminant(other)),
                };
                println!("[offset] #{} distance {signed:.6} base {shape}", e.index);
            }
            Ok(Surface::Offset {
                base: Box::new(lowered),
                distance: signed,
            })
        }
        // BLEND_BOUND names a blend and which of its two boundaries this is:
        // the surface it stands for is the mating surface the ball touches
        // there. It carries no geometry of its own, only that reference, and
        // not following it left it as the commonest reason an intersection
        // could not be computed — 59 appears in more of those than every
        // other type together.
        xt::BLEND_BOUND => {
            let blend = index
                .get(&ptr(e, 8))
                .filter(|b| b.type_id == xt::BLENDED_EDGE)
                .ok_or("BLEND_BOUND does not name a blend")?;
            let which = int_at(e, 7).min(1);
            let mate = index
                .get(&ptr(blend, 8 + which))
                .ok_or("the blend has no surface on that side")?;
            surface(mate, index)
        }
        // A blend can arrive as a face's own surface and not only as the thing
        // a BLEND_BOUND points at, and `blend_surface` would lower it. It is
        // not worth it here: four faces of the pilot assembly take this path,
        // and rolling the ball for them took lowering from 7.4 to 99.7
        // seconds, left one of the four unmeshable, and opened two edges that
        // were closed. The Coons rebuild those faces fall back to is within
        // 15 µm of the STEP reading of the same fillets.
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
            // An SP_CURVE has no closed form: it becomes a polyline by being
            // sampled, and the sampling is indexed 0..n. The trim window is in
            // the *spline's* parameter, so wrapping the polyline in a
            // `Trimmed` carrying that window hands the two of them different
            // parameterisations — and the range then names a sliver of the
            // start rather than the piece it meant. One face of the pilot was
            // lost to exactly this: an edge whose range was [0, 55] over a
            // curve whose own range was [0, 0.0159], sampled to two points
            // that were then overwritten by its vertices. `sp_curve_polyline`
            // already takes the window and samples the piece it names, so the
            // trim is applied where it can be understood and no wrapper is
            // needed.
            if basis.type_id == xt::SP_CURVE {
                return sp_curve_polyline(e, index);
            }
            let base = curve(basis, index)?;
            let (t0, t1) = (f64_at(e, 10), f64_at(e, 11));
            let range = Interval::new(t0.min(t1), t0.max(t1));
            // A curve with no closed form becomes a polyline, indexed 0..n by
            // the samples that were taken. The window that trims it is in
            // whatever the *source* was parameterised by, and for these it is
            // **arc length**: measured over the 245 such curves in this
            // assembly, the window's span divided by the polyline's own 3D
            // length has a median of 0.9972 and an upper quartile of 1.0002.
            // It is a length, in the same units as the points.
            //
            // Applied as an index window instead, `[0, 0.0159]` over 55
            // segments names three ten-thousandths of the curve. The rail then
            // samples to two points, the chain is overwritten by the edge's
            // own vertices, and the loop comes out a spur — which is how one
            // face of 11,212 came to have no boundary at all.
            //
            // A window narrower than a single segment of a many-segment
            // polyline is not an index window, so it is read as the length it
            // is and walked onto the index the polyline is actually addressed
            // by. That keeps a genuine part-window — one of these runs from
            // 0.4 mm to 49.4 mm along a curve 49.8 mm long — instead of
            // rounding it up to the whole curve.
            if let Curve::Polyline { points } = &base {
                let segments = points.len().saturating_sub(1);
                if segments > 1 && range.span() < 1.0 {
                    // Most of these windows say "all of it": the median ratio
                    // of the window's span to the polyline's own length is
                    // 0.997. Where that is so, taking the curve whole is both
                    // simpler and better — walking the length onto the index
                    // exactly leaves a chain that no longer reaches the edge's
                    // own vertices, `discretise` pins the ends to them, and the
                    // distortion opened fifty half-edges against five.
                    //
                    // A quarter of them say something else, down to a ratio
                    // of 0.9, and those look like real part-windows. Walking
                    // one onto the index exactly was measured: it changes
                    // neither the open-edge count nor the non-manifold count,
                    // and moves the two readers apart from 0.0139 mm to
                    // 0.0152 mm. Reading it more faithfully does not make the
                    // mesh better, so the simpler rule stands.
                    return Ok(base);
                }
            }
            Ok(Curve::Trimmed {
                range,
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
            if std::env::var_os("XT_POLY_TRACE").is_some() { eprintln!("[poly] trimmed-curve {} points", points.len()); }
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
/// Walk the curve where two surfaces meet, from one end to the other.
///
/// Parasolid writes an intersection curve twice: as the two surfaces it lies
/// on, which is the definition, and as a *chart* — a handful of sampled points
/// with a stated chordal error, which on this file is measured in millimetres.
/// The chart is what the reader could reach until now, and drawing straight
/// between its samples puts a chord where an arc belongs; on a face whose
/// boundary that chord cuts across, the region stops being readable at all.
///
/// The definition is computable. At any point on both surfaces the curve runs
/// along the cross product of the two normals, so the walk is: step along it,
/// fall back onto both surfaces, repeat. Falling back is an alternation —
/// nearest point on one, then on the other — which converges wherever the two
/// meet at an angle, and is refused where they do not.
///
/// `None` means the walk did not arrive, and the caller keeps the chart.
pub fn intersection_polyline(
    a: &Surface,
    b: &Surface,
    from: Vec3,
    to: Vec3,
    tolerance: f64,
) -> Option<Curve> {
    // Land a point on both surfaces at once, and say where it landed on each.
    // The parameters are handed back for the tangent, not fed forward as a
    // hint: seeding the next inversion from the last one is faster and was
    // measured at three more open edges, because a hinted solve will follow a
    // branch a fresh one would not.
    let settle = |mut p: Vec3, hint: (Option<Vec2>, Option<Vec2>)| -> Option<(Vec3, f64, Vec2, Vec2)> {
        let (mut ua, mut ub) = (hint.0, hint.1);
        for _ in 0..12 {
            let before = p;
            let va = a.invert(p, None)?;
            p = a.point_at(va);
            ua = Some(va);
            let vb = b.invert(p, None)?;
            p = b.point_at(vb);
            ub = Some(vb);
            if (p - before).length() <= tolerance * 1e-3 {
                break;
            }
        }
        let (va, vb) = (ua?, ub?);
        let off = (a.point_at(va) - p).length().max((b.point_at(vb) - p).length());
        Some((p, off, va, vb))
    };
    let along = |va: Vec2, vb: Vec2| -> Option<Vec3> {
        a.normal_at(va).cross(b.normal_at(vb)).try_normalized()
    };

    let (start, off, mut va, mut vb) = settle(from, (None, None))?;
    if off > tolerance || (start - from).length() > tolerance {
        return None;
    }
    let span = (to - from).length();
    if !(span > tolerance) {
        return None;
    }

    // A step small enough that the chord it cuts stays well inside the
    // tolerance on anything but a hairpin, and short enough that thirty-two
    // of them cover a straight run.
    let mut step = span / 32.0;
    let mut points = vec![from];
    let mut p = start;
    let mut heading = along(va, vb)?;
    if heading.dot(to - from) < 0.0 {
        heading = -heading;
    }
    for _ in 0..512 {
        if (p - to).length() <= step {
            break;
        }
        let Some((next, off, na, nb)) = settle(p + heading * step, (Some(va), Some(vb))) else {
            return None;
        };
        if off > tolerance {
            return None;
        }
        let moved = next - p;
        let Some(direction) = moved.try_normalized() else {
            return None;
        };
        // A step that turned too far is a step that cut a corner; take it
        // again shorter. One that barely turned can afford to grow.
        let turn = direction.dot(heading).clamp(-1.0, 1.0).acos();
        if turn > 0.15 && step > span * 1e-4 {
            step *= 0.5;
            continue;
        }
        if turn < 0.02 {
            step = (step * 1.3).min(span / 8.0);
        }
        p = next;
        va = na;
        vb = nb;
        heading = along(va, vb)?;
        if heading.dot(direction) < 0.0 {
            heading = -heading;
        }
        points.push(p);
    }
    if (p - to).length() > step * 2.0 {
        return None;
    }
    points.push(to);
    if std::env::var_os("XT_POLY_TRACE").is_some() { eprintln!("[poly] chart-or-walk {} points", points.len()); }
    (points.len() > 2).then(|| Curve::Polyline { points })
}

/// Lower a surface for the purpose of computing a curve on it.
///
/// The same as [`surface`], except that a rolling-ball blend is built rather
/// than refused. Building one costs a marching solve, and a face on a blend
/// does not need it — it is rebuilt from its own boundary, which is cheaper
/// and is what the rest of this crate does. But an *edge* whose curve is the
/// meeting of a blend and something else has no other description at all, and
/// there are two orders of magnitude fewer of those than there are blend
/// faces: 72 against 2,211 on the pilot assembly, nine seconds against ninety.
///
/// So the expensive reading is done where it is the only reading, and not
/// where a cheaper one is already right.
pub fn surface_for_curve(e: &RawEntity, index: &Index) -> Result<Surface, String> {
    if e.type_id == xt::BLENDED_EDGE {
        return blend_surface(e, index);
    }
    if e.type_id == xt::BLEND_BOUND
        && let Some(blend) = index
            .get(&ptr(e, 8))
            .filter(|b| b.type_id == xt::BLENDED_EDGE)
    {
        // The boundary of a blend is the blend, restricted; for computing a
        // curve on it the blend itself is what is wanted.
        return blend_surface(blend, index);
    }
    surface(e, index)
}

/// The rolling ball's centre line: where a ball of the blend's radius sits so
/// that it touches both of the surfaces being blended.
///
/// The centre is a radius along the first surface's normal from the point the
/// ball touches it, so the centre line is the ball's own contact track on that
/// surface, lifted. Walking the track costs one inversion of each surface a
/// step; walking the two offset surfaces' intersection instead — which is the
/// same curve — costs a nested fixed point per inversion and was measured at
/// more than two minutes on this assembly against under a second.
///
/// Which way the centre lies off the surface is not stated, so both are tried
/// and judged against the file's own sparse sampling of the same curve.
fn blend_spine(be: &RawEntity, index: &Index, radius: f64) -> Option<(Vec<Vec3>, Surface, Surface)> {
    let a = surface(index.get(&ptr(be, 8))?, index).ok()?;
    let b = surface(index.get(&ptr(be, 9))?, index).ok()?;

    // The file's own samples of the spine: where it starts, where it ends, and
    // what the walk is not allowed to stray from.
    let spine = index.get(&ptr(be, 10))?;
    let chart = index.get(&ptr(spine, 8)).filter(|c| c.type_id == xt::CHART)?;
    let samples = chart_points(chart);
    if samples.len() < 2 {
        return None;
    }
    let (head, tail) = (samples[0], samples[samples.len() - 1]);
    let slack = f64_at(chart, 3).abs().max((tail - head).length() * 0.25) + radius * 0.01;
    let near_samples = |q: Vec3| {
        samples
            .windows(2)
            .map(|w| {
                let d = w[1] - w[0];
                let len2 = d.length_squared();
                let t = if len2 > 0.0 {
                    ((q - w[0]).dot(d) / len2).clamp(0.0, 1.0)
                } else {
                    0.0
                };
                (q - (w[0] + d * t)).length()
            })
            .fold(f64::INFINITY, f64::min)
            <= slack
    };

    let probe = std::env::var_os("XT_SPINE_PROBE").is_some();
    if probe {
        eprintln!(
            "[spine] {} chart samples, ends {:.4} apart, slack {:.4}, r {:.4}",
            samples.len(),
            (tail - head).length(),
            slack,
            radius
        );
    }
    // A blend running right round a closed feature has a closed spine — its
    // two ends are the same point — and a walk needs two. The file's own
    // samples supply a third: walk to the far side and back, and the loop is
    // covered in two halves.
    let closed = (tail - head).length() <= radius * 0.01;
    let waypoints: Vec<Vec3> = if closed && samples.len() >= 3 {
        vec![head, samples[samples.len() / 2], head]
    } else {
        vec![head, tail]
    };

    for swap in [false, true] {
        let (near, far) = if swap { (&b, &a) } else { (&a, &b) };
        for sign in [1.0f64, -1.0] {
            // Each leg runs between where two consecutive waypoints touch this
            // surface, which is where they land on it.
            let mut points: Vec<Vec3> = Vec::new();
            let mut walked = true;
            for leg in waypoints.windows(2) {
                let from = match near.invert(leg[0], None) {
                    Some(uv) => near.point_at(uv),
                    None => {
                        walked = false;
                        break;
                    }
                };
                let to = match near.invert(leg[1], None) {
                    Some(uv) => near.point_at(uv),
                    None => {
                        walked = false;
                        break;
                    }
                };
                match blend_rail_from(near, far, radius, sign, from, to, radius * 0.01, slack) {
                    Some(Curve::Polyline { points: part }) => {
                        let skip = usize::from(!points.is_empty());
                        points.extend(part.into_iter().skip(skip));
                    }
                    _ => {
                        walked = false;
                        break;
                    }
                }
            }
            if !walked || points.len() < 2 {
                if probe {
                    eprintln!("[spine] swap={swap} sign={sign}: no track");
                }
                continue;
            }
            let centres: Vec<Vec3> = points
                .iter()
                .filter_map(|p| {
                    let uv = near.invert(*p, None)?;
                    Some(near.point_at(uv) + near.normal_at(uv) * (radius * sign))
                })
                .collect();
            if probe {
                let worst = centres
                    .iter()
                    .map(|c| {
                        samples
                            .iter()
                            .map(|q| (*c - *q).length())
                            .fold(f64::INFINITY, f64::min)
                    })
                    .fold(0.0f64, f64::max);
                eprintln!(
                    "[spine] swap={swap} sign={sign}: {} centres, worst {worst:.4} from the samples",
                    centres.len()
                );
            }
            if centres.len() == points.len() && centres.iter().all(|c| near_samples(*c)) {
                let (first, second) = (near.clone(), far.clone());
                return Some((centres, first, second));
            }
        }
    }
    None
}

/// The blend surface itself, built from the ball that makes it.
///
/// A constant-radius rolling-ball blend is the envelope of a ball of that
/// radius rolling in the crease between two surfaces, so it is completely
/// determined by where the ball goes and what it touches. The spine says where
/// it goes; at each station the ball's contacts are its nearest points on the
/// two surfaces, and the surface between them is the arc of that radius.
/// Nothing here is approximated — the only sampling is how finely the spine is
/// walked and how finely the section is cut, which is the same choice the mesh
/// makes everywhere else.
///
/// Stored as a degree-one grid through those arcs, so downstream it is an
/// ordinary surface: invertible exactly, cell by cell, and tessellatable with
/// no special case anywhere.
pub fn blend_surface(be: &RawEntity, index: &Index) -> Result<Surface, String> {

    let blend_type = be.fields.get(7).map(|f| f.as_char()).unwrap_or('?');
    if blend_type != 'R' {
        return Err(format!("blend type {blend_type:?} is not a rolling ball"));
    }
    let radius = f64_at(be, 11).abs();
    if !(radius.is_finite() && radius > 0.0) {
        return Err("the blend states no radius".into());
    }
    let (spine, a, b) =
        blend_spine(be, index, radius).ok_or("the ball's centre line could not be walked")?;
    if spine.len() < 2 {
        return Err("the centre line came out too short".into());
    }

    // Across the section: enough steps that the arc's own chord stays well
    // inside the radius, which is the finest anything downstream asks for.
    const ACROSS: usize = 12;
    let mut grid: Vec<Vec<Vec3>> = Vec::with_capacity(spine.len());
    for centre in &spine {
        let pa = a.point_at(a.invert(*centre, None).ok_or("a contact does not invert")?);
        let pb = b.point_at(b.invert(*centre, None).ok_or("a contact does not invert")?);
        let (u, w) = (pa - *centre, pb - *centre);
        // Both contacts have to be a radius away, or this is not the ball the
        // file describes.
        if (u.length() - radius).abs() > radius * 0.05
            || (w.length() - radius).abs() > radius * 0.05
        {
            return Err("the ball does not touch both surfaces along its own centre line".into());
        }
        let eu = u.try_normalized().ok_or("a contact sits on the centre")?;
        let perp = (w - eu * w.dot(eu))
            .try_normalized()
            .ok_or("the section has no width")?;
        let sweep = w.dot(perp).atan2(w.dot(eu));
        grid.push(
            (0..=ACROSS)
                .map(|j| {
                    let t = sweep * j as f64 / ACROSS as f64;
                    *centre + (eu * t.cos() + perp * t.sin()) * radius
                })
                .collect(),
        );
    }

    let knots = |n: usize| {
        let mut k = vec![0.0];
        k.extend((0..=n).map(|i| i as f64 / n as f64));
        k.push(1.0);
        k
    };
    let rows = grid.len() - 1;
    Ok(Surface::Nurbs(cad_ir::brep::NurbsSurface {
        u_degree: 1,
        v_degree: 1,
        control_points: grid,
        weights: Vec::new(),
        u_knots: knots(rows),
        v_knots: knots(ACROSS),
        u_closed: false,
        v_closed: false,
    }))
}

/// Walk the track a rolling ball leaves on one of the surfaces it touches.
///
/// A blend's rail is not the intersection of two surfaces — it is where the
/// ball *touches* one of them, and the two surfaces meet there tangentially or
/// not at all. What defines it is still one equation: standing at a point of
/// `near`, put the ball's centre a radius along the normal, and ask how far
/// that centre is from `far`. On the rail the answer is the radius exactly, so
/// the rail is a level set on `near`, and walking a level set is stepping
/// along it and correcting back with the gradient.
///
/// `None` when the walk does not arrive, and the caller keeps whatever it had.
pub fn blend_rail_polyline(
    near: &Surface,
    far: &Surface,
    radius: f64,
    sign: f64,
    from: Vec3,
    to: Vec3,
    tolerance: f64,
) -> Option<Curve> {
    blend_rail_from(near, far, radius, sign, from, to, tolerance, tolerance)
}

/// The same walk, told how far the starting point may be from the track.
///
/// An edge's vertex is exact and the track has to pass through it. A spine's
/// end is not: it comes from the file's own sparse chart, which states its
/// error in millimetres, so correcting onto the track legitimately moves it —
/// and holding it to the vertex's standard refused every blend on the pilot
/// assembly.
#[allow(clippy::too_many_arguments)]
pub fn blend_rail_from(
    near: &Surface,
    far: &Surface,
    radius: f64,
    sign: f64,
    from: Vec3,
    to: Vec3,
    tolerance: f64,
    start_slack: f64,
) -> Option<Curve> {
    // How far the ball standing at `uv` is from touching the other surface.
    let miss = |uv: Vec2| -> Option<f64> {
        let centre = near.point_at(uv) + near.normal_at(uv) * (radius * sign);
        Some((far.point_at(far.invert(centre, None)?) - centre).length() - radius)
    };
    let domain = near.domain();
    let h = Vec2::new(
        (domain.u.span().abs() * 1e-5).max(1e-9),
        (domain.v.span().abs() * 1e-5).max(1e-9),
    );
    // One-sided, from a value the caller already has: the correction only
    // needs a direction to descend in, and paying for the second side of each
    // difference doubled the cost of every blend for no measurable accuracy.
    let gradient = |uv: Vec2, here: f64| -> Option<Vec2> {
        let du = miss(Vec2::new(uv.u + h.u, uv.v))? - here;
        let dv = miss(Vec2::new(uv.u, uv.v + h.v))? - here;
        Some(Vec2::new(du / h.u, dv / h.v))
    };
    // Newton along the gradient until the ball touches.
    let settle = |mut uv: Vec2| -> Option<Vec2> {
        for _ in 0..8 {
            let g = miss(uv)?;
            // Good enough is good enough. Chasing a hundredth of the tolerance
            // that will be accepted anyway meant one more gradient, and a
            // gradient that could not be computed threw away an answer already
            // inside it — a ball six microns from touching, on a tolerance of
            // fifteen, reported as never touching at all.
            if g.abs() <= tolerance {
                return Some(uv);
            }
            let Some(d) = gradient(uv, g) else {
                return None;
            };
            let scale = d.u * d.u + d.v * d.v;
            if !(scale > 0.0) {
                return None;
            }
            uv = Vec2::new(uv.u - g * d.u / scale, uv.v - g * d.v / scale);
        }
        (miss(uv)?.abs() <= tolerance).then_some(uv)
    };

    let probe = std::env::var_os("XT_RAIL_PROBE").is_some();
    let Some(start) = near.invert(from, None) else {
        if probe {
            eprintln!("[rail] the start does not invert onto the near surface");
        }
        return None;
    };
    if probe {
        let off = (near.point_at(start) - from).length();
        let g = miss(start).unwrap_or(f64::NAN);
        eprintln!("[rail] start off-surface {off:.6}, ball misses by {g:.6}, r={radius:.6}");
    }
    let Some(mut uv) = settle(start) else {
        if probe {
            eprintln!("[rail] the ball never comes to touch from there");
        }
        return None;
    };
    if (near.point_at(uv) - from).length() > start_slack {
        if probe {
            eprintln!(
                "[rail] correcting moved the start {:.6} off it, past the {start_slack:.6} allowed",
                (near.point_at(uv) - from).length()
            );
        }
        return None;
    }
    let span = (to - from).length();
    if !(span > tolerance) {
        return None;
    }
    let mut step = span / 32.0;
    let mut points = vec![near.point_at(uv)];
    let mut previous = points[0];
    let mut heading: Option<Vec3> = None;
    for _ in 0..512 {
        let p = near.point_at(uv);
        if (p - to).length() <= step {
            break;
        }
        // Along the level set: across the gradient, in the surface's own
        // parameters, then carried into space by its derivatives.
        let d = gradient(uv, miss(uv)?)?;
        let (du, dv) = near.derivatives_at(uv);
        let mut way = du * -d.v + dv * d.u;
        if let Some(previous_way) = heading {
            if way.dot(previous_way) < 0.0 {
                way = -way;
            }
        } else if way.dot(to - from) < 0.0 {
            way = -way;
        }
        let direction = way.try_normalized()?;
        // Step in space, back to parameters, then back onto the track. A step
        // that lands somewhere the surface cannot be inverted, or from which
        // the correction will not converge, is a step that was too long — the
        // walk shortens it and tries again rather than giving up, which is the
        // difference between arriving and not on a track that turns sharply.
        let Some(next) = near
            .invert(p + direction * step, Some(uv))
            .and_then(&settle)
        else {
            if step > span * 1e-4 {
                step *= 0.5;
                continue;
            }
            return None;
        };
        let moved = near.point_at(next) - p;
        let Some(went) = moved.try_normalized() else {
            return None;
        };
        let turn = went.dot(direction).clamp(-1.0, 1.0).acos();
        if turn > 0.15 && step > span * 1e-4 {
            step *= 0.5;
            continue;
        }
        if turn < 0.02 {
            step = (step * 1.3).min(span / 8.0);
        }
        uv = next;
        heading = Some(went);
        previous = near.point_at(uv);
        points.push(previous);
    }
    if (previous - to).length() > step * 2.0 {
        return None;
    }
    points.push(to);
    if std::env::var_os("XT_POLY_TRACE").is_some() { eprintln!("[poly] sp-curve {} points", points.len()); }
    (points.len() > 2).then(|| Curve::Polyline { points })
}

/// The blend surface an `SP_CURVE` lives on, and whether it runs across the
/// blend rather than along it.
///
/// A parameter curve on a `BLENDED_EDGE` is written in the blend's own `(u, v)`,
/// with `u` the spine's parameter and `v` running from nought at the first
/// mating surface's contact to one at the second's. Measured on the pilot
/// assembly, the ones this crate cannot otherwise read are all straight lines
/// of two control points at constant `u` — `(u₀, 0) → (u₀, 1)` — which is to
/// say they are cross-sections of the blend, one whole sweep of the rolling
/// ball from one contact to the other. That is the shape a caller can rebuild
/// from the edge's own two vertices without evaluating the blend at all.
pub fn blend_cross_section<'a>(e: &RawEntity, index: &Index<'a>) -> Option<&'a RawEntity> {
    blend_parameter_curve(e, index).and_then(|(surf, across, _)| across.then_some(surf))
}

/// A parameter curve on a blend, and which way it runs.
///
/// Returns the blend surface, whether the curve runs *across* the blend — a
/// cross-section, `v` sweeping from one contact to the other — and, when it
/// runs *along* it instead, which of the two mating surfaces it is the ball's
/// contact track on: `false` for the first, `true` for the second.
pub fn blend_parameter_curve<'a>(
    e: &RawEntity,
    index: &Index<'a>,
) -> Option<(&'a RawEntity, bool, bool)> {
    let e = if e.type_id == xt::TRIMMED_CURVE {
        index.get(&ptr(e, 7))?
    } else {
        e
    };
    if e.type_id != xt::SP_CURVE {
        return None;
    }
    let surf = index.get(&ptr(e, 7))?;
    if surf.type_id != xt::BLENDED_EDGE {
        return None;
    }
    let probe = std::env::var_os("XT_SECTION_PROBE").is_some();
    let n2 = match nurbs_curve2(index.get(&ptr(e, 8))?, index) {
        Ok(n) => n,
        Err(why) => {
            if probe {
                eprintln!("[section] blend pcurve unreadable: {why}");
            }
            return None;
        }
    };
    // Across the blend, not along it: every control point at the same `u`.
    let u0 = n2.control_points.first()?.u;
    let span = |f: fn(&cad_ir::math::Vec2) -> f64| {
        n2.control_points
            .iter()
            .map(f)
            .fold((f64::INFINITY, f64::NEG_INFINITY), |(a, b), y| (a.min(y), b.max(y)))
    };
    let (vlo, vhi) = span(|p| p.v);
    let (ulo, uhi) = span(|p| p.u);
    // A cross-section sweeps `v` from one contact to the other while `u`
    // barely moves — but "barely" is relative, not absolute: the spine
    // parameter of a short blend is itself a small number, and these curves
    // drift across it by a ten-thousandth while `v` runs the whole way. Judged
    // against a fixed epsilon, every one of them read as something else.
    let _ = u0;
    let flat_u = uhi - ulo <= (vhi - vlo) * 0.05;
    let across = flat_u && vhi - vlo > 0.5;
    if probe {
        eprintln!(
            "[section] blend pcurve {} the blend: {} points u[{ulo:.6},{uhi:.6}] v[{vlo:.6},{vhi:.6}]",
            if across { "crosses" } else { "runs along" },
            n2.control_points.len()
        );
    }
    // Along the blend instead: `v` pinned at one end or the other, which is
    // the ball's contact track on that mating surface.
    let far_side = vlo > 0.5;
    let along = !across && (vhi - vlo) <= 0.05 && (uhi - ulo) > 0.0;
    (across || along).then_some((surf, across, far_side))
}

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
        if std::env::var_os("XT_SPC_FIELDS").is_some() {
            println!(
                "[trim] #{} fields {:?}",
                e.index,
                e.fields
                    .iter()
                    .enumerate()
                    .map(|(i, f)| format!("{i}:{f:?}"))
                    .collect::<Vec<_>>()
                    .join(" ")
            );
        }
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
        if std::env::var_os("XT_SPC_WINDOW").is_some() {
            let overlap = whi.min(hi) - wlo.max(lo);
            println!(
                "[spcwin] domain [{lo:.4},{hi:.4}] window [{wlo:.4},{whi:.4}] overlap {overlap:.4}"
            );
        }
        // A window that does not meet the spline's domain at all cannot be
        // read in the spline's parameterisation, whatever it means in the
        // writer's. Clamping it anyway collapses the sample range to a single
        // parameter, and the curve comes out as twenty-five copies of one
        // point — which leaves the face that names it with no boundary to trim
        // against, and the face is then lost outright. Of 2,433 SP_CURVEs with
        // a window in this assembly exactly three are like this, each with the
        // domain [0, 1] and the window [-1, 0]: the same width, shifted a
        // whole domain. The curve itself is sound, so it is used whole. That
        // is the only information the entity actually carries.
        if whi.min(hi) - wlo.max(lo) <= 0.0 {
            if std::env::var_os("XT_SPC_WINDOW").is_some() {
                println!(
                    "[spcwin] #{} window [{wlo:.4},{whi:.4}] misses domain [{lo:.4},{hi:.4}] \
                     on surface #{} (type {})",
                    e.index, surf_entity.index, surf_entity.type_id
                );
            }
        } else if n2.degree == 1 && n2.control_points.len() == 2 {
            // A parameter line — two control points, degree one — is exact
            // wherever it is extended, and on a surface periodic in the
            // direction it runs it is a whole circle. The window the trim
            // asks for is honoured past the spline's own knots: edge 664 of
            // `201 201 003-51` is a closed edge on a torus whose parameter
            // line is written over [0.75, 1.0] and trimmed to (0.75, 1.75);
            // clipped to its knots it sampled a quarter turn, its polyline
            // ended 23 mm short of the vertex it shares with its own start,
            // and the torus face it bounds was 12 mm off its surface. Of 135
            // closed stand-in edges in the pilot, seven did not close.
            lo = wlo;
            hi = whi;
        } else {
            // The trim window is within the spline's own domain; a hair of
            // slack for the usual last-ulp writers.
            lo = wlo.max(lo - 1e-9);
            hi = whi.min(hi + 1e-9);
        }
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
    let at = |t: f64| -> Vec3 {
        let uvw = cad_ir::eval::nurbs::de_boor(n2.degree, &hom, knots, t);
        let inv = if uvw[2].abs() > 1e-300 { 1.0 / uvw[2] } else { 1.0 };
        let (a, b) = (uvw[0] * inv, uvw[1] * inv);
        let uv = if swap { Vec2::new(b, a) } else { Vec2::new(a, b) };
        surf.point_at(uv)
    };
    // Even steps in parameter, then each chord is bisected until its middle
    // sits within the chord tolerance of the surface point there. A fixed
    // count is blind to the geometry: 2,445 of this assembly's 26,533 edges
    // have no curve of their own and stand on this sampler, 1,685 of them at
    // the floor of 24 steps — which on a 15 mm circle is a 15° chord 1.6 mm
    // off the arc, and one such ring put a torus 12 mm off its surface.
    const CHORD_TOLERANCE: f64 = 1e-5 * 20.0;
    const MOST: usize = 1024;
    let mut ts: Vec<f64> = (0..=samples).map(|k| lo + (hi - lo) * k as f64 / samples as f64).collect();
    let mut pts: Vec<Vec3> = ts.iter().map(|t| at(*t)).collect();
    let mut i = 0;
    while i + 1 < ts.len() && ts.len() < MOST {
        let tm = 0.5 * (ts[i] + ts[i + 1]);
        let m = at(tm);
        let chord_mid = (pts[i] + pts[i + 1]) * 0.5;
        if (m - chord_mid).length() > CHORD_TOLERANCE {
            ts.insert(i + 1, tm);
            pts.insert(i + 1, m);
        } else {
            i += 1;
        }
    }
    if std::env::var_os("XT_POLY_TRACE").is_some() {
        // The raw mid-chord departures of the first segments, so the
        // threshold can be judged against what is actually there.
        let first: Vec<String> = (0..ts.len().saturating_sub(1).min(3))
            .map(|i| {
                let m = at(0.5 * (ts[i] + ts[i + 1]));
                format!("{:.2e}", (m - (pts[i] + pts[i + 1]) * 0.5).length())
            })
            .collect();
        eprintln!(
            "[poly] spc#{} surf-type={} knots=[{:.4}..{:.4}] window={:?} bisected {} -> {} deg={} cps={} span={:.4} first-mid-departures=[{}]",
            e.index, surf_entity.type_id, lo, hi, window, samples + 1, pts.len(), n2.degree, n2.control_points.len(),
            (pts[pts.len() - 1] - pts[0]).length(), first.join(" ")
        );
    }
    let points = pts;
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
        // Both failures are worth seeing: a curve that runs metres away, and
        // one that goes nowhere at all. The second is the quieter of the two
        // and it is what leaves a face with no boundary to trim against.
        if (hi - lo).length() > 10.0 || (hi - lo).length() < 1e-9 {
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
    if std::env::var_os("XT_POLY_TRACE").is_some() { eprintln!("[poly] last {} points", points.len()); }
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
    // Field 6 is the chart's first point. The schema calls it `hvec`, and
    // leaving it out was measured: `chart_count` counts it, 5,357 of 5,357
    // charts come up one short without it, seven faces fail to mesh and the
    // Parasolid reading opens 551 half-edges. It is the first point.
    //
    // Except on four charts of the pilot, where it is 0.7–1.4 km from an
    // edge whose vertices are millimetres apart — a spike of that length
    // went into two edges. Those are told apart by the data alone: the first
    // point sits further from the second than the whole rest of the chart
    // spans, by a hundredfold. A chart that long with a step that short is
    // not a curve; the point is dropped and the chart stands on the rest.
    if let Some(first) = chart.fields.get(6).map(|f| f.as_vec3())
        && first.iter().all(|v| v.is_finite())
    {
        let first = Vec3::new(first[0], first[1], first[2]);
        let rest: Vec<Vec3> = chart
            .var_f64()
            .chunks_exact(3)
            .map(|c| Vec3::new(c[0], c[1], c[2]))
            .collect();
        let plausible = match (rest.first(), rest.last()) {
            (Some(a), Some(b)) if rest.len() >= 2 => {
                let extent = rest
                    .iter()
                    .map(|p| (*p - *a).length())
                    .fold((*b - *a).length(), f64::max)
                    .max(1e-9);
                (first - *a).length() <= extent * 100.0
            }
            _ => true,
        };
        if plausible {
            out.push(first);
        } else if std::env::var_os("XT_CHART_TRACE").is_some() {
            eprintln!("[chart] #{} dropped a first point {:.1} m from the rest", chart.index, (first - rest[0]).length());
        }
    }
    for c in chart.var_f64().chunks_exact(3) {
        out.push(Vec3::new(c[0], c[1], c[2]));
    }
    if std::env::var_os("XT_CHART_STATS").is_some() && out.len() >= 3 {
        let d01 = (out[1] - out[0]).length();
        let d12 = (out[2] - out[1]).length();
        eprintln!("[chartstat] first-to-second {d01:.6} second-to-third {d12:.6} ratio {:.2}", d01 / d12.max(1e-12));
    }
    if std::env::var_os("XT_CHART_TRACE").is_some()
        && out.iter().any(|p| p.length() > 10.0)
    {
        eprintln!(
            "[chart] #{} {} fields, var_f64 {} values, first field 6 = {:?}, points: {:?}",
            chart.index,
            chart.fields.len(),
            chart.var_f64().len(),
            chart.fields.get(6),
            out.iter().map(|p| format!("({:.3},{:.3},{:.3})", p.x, p.y, p.z)).collect::<Vec<_>>()
        );
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
        .map(|v| v.var_f64())
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
        .map(|v| v.var_f64())
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
        .map(|v| v.var_f64())
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

    if std::env::var_os("XT_NURBS_TRACE").is_some() {
        // A surface closed in u must bring the two ends of its valid domain
        // to the same place. If it does not, the wrap the periodic form needs
        // is missing and the ends are being evaluated as if clamped.
        let probe = NurbsSurface {
            u_degree,
            v_degree,
            control_points: control_points.clone(),
            weights: weights.clone(),
            u_knots: u_knots.clone(),
            v_knots: v_knots.clone(),
            u_closed: u_closed || u_periodic,
            v_closed: v_closed || v_periodic,
        };
        let surf = cad_ir::brep::Surface::Nurbs(probe);
        let d = surf.domain();
        let vm = 0.5 * (d.v.lo + d.v.hi);
        let gap = (surf.point_at(Vec2::new(d.u.lo, vm)) - surf.point_at(Vec2::new(d.u.hi, vm)))
            .length();
        if (u_closed || u_periodic) && gap > 1e-6 {
            println!("[nurbs-gap] #{} closed in u but its ends are {gap:.6} apart", e.index);
        }
    }
    if std::env::var_os("XT_NURBS_TRACE").is_some() {
        println!(
            "[nurbs] #{} {n_u}x{n_v} deg {u_degree}x{v_degree} periodic u={u_periodic} \
             v={v_periodic} closed u={u_closed} v={v_closed} knots {}x{} \
             u[{:.4},{:.4}] v[{:.4},{:.4}]",
            e.index,
            u_knots.len(),
            v_knots.len(),
            u_knots.first().copied().unwrap_or(0.0),
            u_knots.last().copied().unwrap_or(0.0),
            v_knots.first().copied().unwrap_or(0.0),
            v_knots.last().copied().unwrap_or(0.0),
        );
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
        .map(|m| m.var_i16())
        .unwrap_or(&[]);
    let knots = index
        .get(&ptr(e, set_field))
        .map(|k| k.var_f64())
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
