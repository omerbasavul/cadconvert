//! Triangulating one trimmed face.
//!
//! The face's trim loops are mapped into the surface's parameter space, the
//! region they enclose is filled by a constrained Delaunay triangulation, and
//! every resulting parameter point is lifted back to 3D. Three things make this
//! harder than it sounds, and all three occur in the pilot file:
//!
//! **No pcurves.** The exporter writes only 3D curves, so every boundary point
//! has to be inverted onto the surface. Done point-by-point that is ambiguous
//! on a periodic surface — a point at u = 0 is equally at u = 2π — so the chain
//! is unwrapped as it is built, each point placed in the branch nearest its
//! predecessor.
//!
//! **No `FACE_OUTER_BOUND`.** Every loop arrives as a plain `FACE_BOUND`, so
//! which one is the outer boundary and which are holes has to be decided
//! geometrically, by parameter-space area.
//!
//! **No seam edges.** A cylinder's lateral face is bounded by its two circles
//! and nothing else, so its loops do not close in parameter space — they run
//! from `u₀` to `u₀ + 2π`. Such a face is filled as the strip between its
//! wrapping loops, with the seam supplied here rather than by the file.

use crate::edge::Chain;
use crate::options::Resolved;
use cad_ir::brep::{Bound, FaceId, Solid, Surface};
use cad_ir::eval::surface::Domain;
use cad_ir::math::{Vec2, Vec3};
use spade::{ConstrainedDelaunayTriangulation, Point2, Triangulation};

/// A triangulated face, in its own vertex numbering.
#[derive(Debug, Default, Clone)]
pub struct Patch {
    pub positions: Vec<[f32; 3]>,
    pub normals: Vec<[f32; 3]>,
    pub indices: Vec<u32>,
}

/// A trim loop expressed in parameter space, alongside its exact 3D points.
#[derive(Debug, Clone)]
struct UvLoop {
    uv: Vec<Vec2>,
    /// The cached 3D point for each parameter point. Boundary vertices keep
    /// these rather than being re-evaluated, which is what keeps shared edges
    /// crack-free.
    xyz: Vec<Vec3>,
    /// Net parameter travel in u, in periods. Zero for a loop that closes.
    wrap: i32,
    /// Signed area in parameter space; positive is counter-clockwise.
    area: f64,
}

/// Triangulate one face.
pub fn tessellate(
    solid: &Solid,
    fid: FaceId,
    edges: &[Chain],
    options: &Resolved,
) -> Result<Patch, String> {
    let face = solid.face(fid);
    let surface = solid.surface(face.surface);
    let domain = surface.domain();

    // Parameter space is anisotropic — on a cylinder u is radians and v is
    // millimetres — and a Delaunay triangulation of raw parameters produces
    // slivers. Scaling both axes to comparable 3D lengths fixes that, and the
    // scale is divided back out before anything is evaluated.
    let scale = parameter_scale(surface, &domain);

    let mut loops = Vec::new();
    for bound in &face.bounds {
        if let Some(l) = build_loop(bound, surface, &domain, edges)? {
            loops.push(l);
        }
    }

    // A loop that wraps *and* encloses area is already a closed polygon in the
    // unrolled strip: its implicit closing edge runs along the seam, which is
    // precisely the boundary the file left implicit. Only a wrapping loop that
    // encloses nothing is a bare ring — a circle at constant v — and those are
    // the ones that need a partner ring or a pole to close against.
    let ring_area = loops
        .iter()
        .map(|l| l.area.abs())
        .fold(0.0f64, f64::max)
        * 1e-6;
    let wrapping: Vec<usize> = loops
        .iter()
        .enumerate()
        .filter(|(_, l)| l.wrap != 0 && l.area.abs() <= ring_area.max(1e-12))
        .map(|(i, _)| i)
        .collect();

    if std::env::var_os("CAD_TESS_TRACE").is_some() {
        eprintln!(
            "[face {}] surface={} bounds={} domain u=[{:.3},{:.3}] v=[{:.4e},{:.4e}] \
             u_period={:?} halves=[{}] loops={:?}",
            fid.0,
            surface_kind(surface),
            face.bounds.len(),
            domain.u.lo,
            domain.u.hi,
            domain.v.lo,
            domain.v.hi,
            domain.u_period,
            face.bounds
                .iter()
                .map(|b| {
                    b.halves
                        .iter()
                        .map(|h| {
                            let e = solid.edge(h.edge);
                            format!(
                                "{}{}",
                                if h.forward { '+' } else { '-' },
                                curve_kind(solid.curve(e.curve))
                            )
                        })
                        .collect::<Vec<_>>()
                        .join(",")
                })
                .collect::<Vec<_>>()
                .join(" | "),
            loops
                .iter()
                .map(|l| format!(
                    "{{n={} wrap={} area={:.3e} u=[{:.3},{:.3}] v=[{:.4},{:.4}]}}",
                    l.uv.len(),
                    l.wrap,
                    l.area,
                    span(l.uv.iter().map(|p| p.u)).0,
                    span(l.uv.iter().map(|p| p.u)).1,
                    span(l.uv.iter().map(|p| p.v)).0,
                    span(l.uv.iter().map(|p| p.v)).1,
                ))
                .collect::<Vec<_>>()
        );
    }

    if std::env::var_os("CAD_TESS_TRACE_UV").is_some()
        && loops.iter().filter(|l| l.wrap != 0).count() == 1
    {
        for (i, l) in loops.iter().enumerate() {
            eprintln!(
                "  [{}] loop {i}: wrap={} n={} area={:.4} halves={}",
                surface_kind(surface),
                l.wrap,
                l.uv.len(),
                l.area,
                face.bounds
                    .iter()
                    .map(|b| b
                        .halves
                        .iter()
                        .map(|h| format!(
                            "{}{}",
                            if h.forward { '+' } else { '-' },
                            curve_kind(solid.curve(solid.edge(h.edge).curve))
                        ))
                        .collect::<Vec<_>>()
                        .join(","))
                    .collect::<Vec<_>>()
                    .join(" | ")
            );
            for (uv, xyz) in l.uv.iter().zip(&l.xyz) {
                eprintln!(
                    "    u={:9.5} v={:9.5}   xyz=({:9.4},{:9.4},{:9.4})",
                    uv.u, uv.v, xyz.x, xyz.y, xyz.z
                );
            }
        }
    }

    let (outer, holes) = if loops.is_empty() {
        // A whole sphere or torus is written with no bounds at all: the surface
        // is closed in both directions, so there is nothing to trim.
        (full_domain_loop(surface, &domain, options)?, Vec::new())
    } else if wrapping.is_empty() {
        closed_region(&mut loops)?
    } else {
        wrapped_region(&mut loops, &wrapping, surface, &domain, options)?
    };

    if outer.uv.len() < 3 {
        return Err(format!(
            "outer boundary has only {} parameter points",
            outer.uv.len()
        ));
    }

    // The boundary's own extent, kept so the finished patch can be checked
    // against it.
    let mut boundary = cad_ir::math::Aabb::EMPTY;
    for p in outer.xyz.iter().chain(holes.iter().flat_map(|h| h.xyz.iter())) {
        boundary.add_point(*p);
    }

    let patch = triangulate(surface, &domain, outer, holes, scale, options, face.same_sense)?;

    // A patch may legitimately bulge past its boundary — a spherical cap
    // reaches a radius above the circle that bounds it — but not by much more
    // than the boundary's own size. Anything further means the parameter-space
    // region was wrong, and emitting it would drag the whole model's bounding
    // box out with it and coarsen every relative tolerance downstream.
    if !boundary.is_empty() {
        let allowance = boundary.diagonal().max(options.sag * 16.0);
        let centre = boundary.centre();
        let limit = boundary.diagonal() * 0.5 + allowance;
        let mut worst = 0.0f64;
        for p in &patch.positions {
            let d = (cad_ir::math::Vec3::new(p[0] as f64, p[1] as f64, p[2] as f64) - centre)
                .length();
            worst = worst.max(d);
        }
        if worst > limit {
            return Err(format!(
                "patch reaches {worst:.1} from its boundary centre, but the boundary is only \
                 {:.1} across",
                boundary.diagonal()
            ));
        }
    }

    Ok(patch)
}

/// Map one bound into parameter space.
fn build_loop(
    bound: &Bound,
    surface: &Surface,
    domain: &Domain,
    edges: &[Chain],
) -> Result<Option<UvLoop>, String> {
    // A single-vertex loop marks a degenerate point — a cone apex, a sphere
    // pole. It bounds no area and contributes no constraint.
    if bound.vertex.is_some() && bound.halves.is_empty() {
        return Ok(None);
    }

    let mut xyz: Vec<Vec3> = Vec::new();
    for half in &bound.halves {
        let chain = edges
            .get(half.edge.index())
            .ok_or_else(|| format!("edge {} has no discretisation", half.edge.0))?;
        if chain.points.len() < 2 {
            continue;
        }
        let pts = chain.oriented(half.forward);
        // Drop the joining point, which the previous half-edge already added.
        let skip = usize::from(!xyz.is_empty());
        xyz.extend(pts.into_iter().skip(skip).map(|(p, _)| p));
    }
    if xyz.len() < 3 {
        return Ok(None);
    }

    // The closing point is kept through the inversion. It coincides with the
    // first point in 3D, but on a periodic surface it lands a whole period away
    // in parameter space — and there it is a genuine, distinct boundary vertex.
    // Dropping it before unwrapping is what turns a full cylindrical band into
    // an open path with its last edge missing.
    let mut uv = unwrap_chain(surface, domain, &xyz);
    let closes_in_3d = (xyz[xyz.len() - 1] - xyz[0]).length_squared() < 1e-24;

    let wrap = net_wrap(&uv, domain);
    if closes_in_3d && wrap == 0 {
        // An ordinary closed loop: the repeat is redundant and would give the
        // polygon a zero-length edge.
        xyz.pop();
        uv.pop();
    }
    if uv.len() < 3 {
        return Ok(None);
    }

    let area = signed_area(&uv);
    Ok(Some(UvLoop {
        uv,
        xyz,
        wrap,
        area,
    }))
}

/// Invert a 3D chain onto the surface, keeping the parameter path continuous.
///
/// Each point is placed in the branch nearest its predecessor, so a loop that
/// crosses the seam produces `… 6.1, 6.2, 6.4 …` rather than `… 6.1, 6.2,
/// 0.1 …`. Without this every periodic face is torn in half.
fn unwrap_chain(surface: &Surface, domain: &Domain, xyz: &[Vec3]) -> Vec<Vec2> {
    let mut out = Vec::with_capacity(xyz.len());
    let mut previous: Option<Vec2> = None;
    for &p in xyz {
        let mut uv = surface.invert(p, previous).unwrap_or_default();
        if let Some(prev) = previous {
            if let Some(period) = domain.u_period {
                uv.u = nearest_branch(uv.u, prev.u, period);
            }
            if let Some(period) = domain.v_period {
                uv.v = nearest_branch(uv.v, prev.v, period);
            }
        }
        previous = Some(uv);
        out.push(uv);
    }
    out
}

/// Shift `value` by whole periods to sit as close to `reference` as possible.
fn nearest_branch(value: f64, reference: f64, period: f64) -> f64 {
    if period <= 0.0 {
        return value;
    }
    value - period * ((value - reference) / period).round()
}

/// How many whole periods the loop travels in u, rounded.
fn net_wrap(uv: &[Vec2], domain: &Domain) -> i32 {
    let Some(period) = domain.u_period else {
        return 0;
    };
    if uv.len() < 2 {
        return 0;
    }
    // The chain still carries its closing point, so the travel is simply the
    // distance from the first parameter to the last.
    let travel = uv[uv.len() - 1].u - uv[0].u;
    let periods = travel / period;
    let whole = periods.round();
    // Only a near-exact whole number of turns counts. A sliver face whose
    // boundary inverts erratically can accumulate 0.9 of a period without
    // going anywhere, and treating that as a wrap sends it down the
    // seam-closing path, which then invents a boundary it does not have.
    if whole.abs() >= 1.0 && (periods - whole).abs() < 0.15 {
        whole as i32
    } else {
        0
    }
}

/// Twice the signed area of a parameter-space polygon; positive is CCW.
fn signed_area(uv: &[Vec2]) -> f64 {
    let mut a = 0.0;
    for i in 0..uv.len() {
        let p = uv[i];
        let q = uv[(i + 1) % uv.len()];
        a += p.u * q.v - q.u * p.v;
    }
    a * 0.5
}

/// Pick the outer loop by parameter-space area and return the rest as holes.
fn closed_region(loops: &mut Vec<UvLoop>) -> Result<(UvLoop, Vec<UvLoop>), String> {
    if loops.is_empty() {
        return Err("face has no usable trim loops".into());
    }
    let outer_index = loops
        .iter()
        .enumerate()
        .max_by(|a, b| {
            a.1.area
                .abs()
                .partial_cmp(&b.1.area.abs())
                .unwrap_or(std::cmp::Ordering::Equal)
        })
        .map(|(i, _)| i)
        .unwrap_or(0);
    let outer = loops.remove(outer_index);
    Ok((outer, std::mem::take(loops)))
}

/// Build the region for a face whose loops wrap the periodic direction.
///
/// The file supplies no seam, so one is added here: the region is the strip
/// between the wrapping loops, closed on the left and right by segments of the
/// parameter lines `u = u₀` and `u = u₀ + period`.
fn wrapped_region(
    loops: &mut Vec<UvLoop>,
    wrapping: &[usize],
    surface: &Surface,
    domain: &Domain,
    options: &Resolved,
) -> Result<(UvLoop, Vec<UvLoop>), String> {
    let Some(period) = domain.u_period else {
        return Err("a loop wraps but the surface is not periodic in u".into());
    };
    if wrapping.len() > 2 {
        return Err(format!(
            "{} loops wrap the seam; only one or two can be closed automatically",
            wrapping.len()
        ));
    }

    // Take the wrapping loops out, leaving the rest as holes.
    let mut rings: Vec<UvLoop> = Vec::new();
    for &i in wrapping.iter().rev() {
        rings.push(loops.remove(i));
    }
    let holes = std::mem::take(loops);

    // Put every ring on the same branch and running the same way in u.
    let u0 = rings
        .iter()
        .flat_map(|r| r.uv.iter())
        .map(|p| p.u)
        .fold(f64::INFINITY, f64::min);
    for r in &mut rings {
        if r.wrap < 0 {
            r.uv.reverse();
            r.xyz.reverse();
            r.wrap = -r.wrap;
        }
        let shift = period * ((r.uv[0].u - u0) / period).round();
        for p in &mut r.uv {
            p.u -= shift;
        }
    }
    rings.sort_by(|a, b| {
        mean_v(&a.uv)
            .partial_cmp(&mean_v(&b.uv))
            .unwrap_or(std::cmp::Ordering::Equal)
    });

    let (lower, upper) = match rings.len() {
        2 => {
            let upper = rings.pop().expect("checked length");
            let lower = rings.pop().expect("checked length");
            (lower, upper)
        }
        1 => {
            // One ring means the face runs from it to a pole, which only a
            // v-bounded surface can do. Synthesise the missing ring on the
            // domain edge the face's winding points toward.
            let ring = rings.pop().expect("checked length");
            let v_mid = mean_v(&ring.uv);
            let toward_high = (domain.v.hi - v_mid).abs() < (v_mid - domain.v.lo).abs();
            let v_edge = if toward_high { domain.v.hi } else { domain.v.lo };
            if !v_edge.is_finite() || v_edge.abs() > 1e11 {
                return Err(format!(
                    "one wrapping loop at v={v_mid:.4}, but the nearer domain edge is \
                     v={v_edge:.3e} — the surface does not close on that side"
                ));
            }
            // Closing onto a domain edge only makes sense when it is near the
            // face. A shallow cone's apex can be kilometres away, and running
            // the face out to it would swallow the whole model's bounds.
            let reach = (v_edge - v_mid).abs();
            let girth = ring
                .xyz
                .iter()
                .map(|p| (*p - ring.xyz[0]).length())
                .fold(0.0f64, f64::max);
            if girth > 0.0 && reach > girth * 64.0 {
                return Err(format!(
                    "one wrapping loop, and the domain edge it would close onto is \
                     {reach:.1} away from a loop only {girth:.1} across"
                ));
            }
            let edge_ring = pole_ring(surface, &ring, v_edge);
            if toward_high {
                (ring, edge_ring)
            } else {
                (edge_ring, ring)
            }
        }
        _ => return Err("no wrapping loop after filtering".into()),
    };

    let strip = close_strip(lower, upper, period, surface, options);
    Ok((strip, holes))
}

/// The boundary of a face that trims nothing: the whole parameter rectangle.
///
/// Only possible when both directions are bounded, which in practice means a
/// closed surface — a sphere or a torus. The `u = u_hi` column is evaluated at
/// `u_lo`, so the seam it introduces is closed to the bit.
fn full_domain_loop(
    surface: &Surface,
    domain: &Domain,
    options: &Resolved,
) -> Result<UvLoop, String> {
    let bounded = |i: cad_ir::math::Interval| {
        i.span().is_finite() && i.span().abs() < 1e9 && i.span() > 0.0
    };
    if !bounded(domain.u) || !bounded(domain.v) {
        return Err("face has no trim loops and its surface is unbounded".into());
    }

    let (u_lo, u_hi) = (domain.u.lo, domain.u.hi);
    let (v_lo, v_hi) = (domain.v.lo, domain.v.hi);
    let nu = direction_steps(surface, domain, Axis::U, u_lo, u_hi, 0.5 * (v_lo + v_hi), options)
        .clamp(3, 256);
    let nv = direction_steps(surface, domain, Axis::V, v_lo, v_hi, 0.5 * (u_lo + u_hi), options)
        .clamp(3, 256);

    let mut uv = Vec::with_capacity(2 * (nu + nv));
    let mut xyz = Vec::with_capacity(uv.capacity());
    // The u period, when there is one, is exactly the domain width here.
    let wrap_u = |u: f64| if domain.u_period.is_some() && u >= u_hi { u_lo } else { u };
    let wrap_v = |v: f64| if domain.v_period.is_some() && v >= v_hi { v_lo } else { v };

    let mut push = |u: f64, v: f64, uv: &mut Vec<Vec2>, xyz: &mut Vec<Vec3>| {
        uv.push(Vec2::new(u, v));
        xyz.push(surface.point_at(Vec2::new(wrap_u(u), wrap_v(v))));
    };

    for i in 0..nu {
        push(u_lo + (u_hi - u_lo) * i as f64 / nu as f64, v_lo, &mut uv, &mut xyz);
    }
    for j in 0..nv {
        push(u_hi, v_lo + (v_hi - v_lo) * j as f64 / nv as f64, &mut uv, &mut xyz);
    }
    for i in (1..=nu).rev() {
        push(u_lo + (u_hi - u_lo) * i as f64 / nu as f64, v_hi, &mut uv, &mut xyz);
    }
    for j in (1..=nv).rev() {
        push(u_lo, v_lo + (v_hi - v_lo) * j as f64 / nv as f64, &mut uv, &mut xyz);
    }

    Ok(UvLoop {
        area: signed_area(&uv),
        wrap: 0,
        uv,
        xyz,
    })
}

fn mean_v(uv: &[Vec2]) -> f64 {
    if uv.is_empty() {
        return 0.0;
    }
    uv.iter().map(|p| p.v).sum::<f64>() / uv.len() as f64
}

/// A synthetic ring along a `v` domain edge, matching another ring's u samples.
fn pole_ring(surface: &Surface, like: &UvLoop, v: f64) -> UvLoop {
    let uv: Vec<Vec2> = like.uv.iter().map(|p| Vec2::new(p.u, v)).collect();
    let xyz: Vec<Vec3> = uv.iter().map(|&p| surface.point_at(p)).collect();
    UvLoop {
        area: signed_area(&uv),
        wrap: like.wrap,
        uv,
        xyz,
    }
}

/// Join two rings into one closed boundary with seam segments at both ends.
fn close_strip(
    lower: UvLoop,
    upper: UvLoop,
    period: f64,
    surface: &Surface,
    options: &Resolved,
) -> UvLoop {
    let u_lo = lower.uv[0].u;
    let u_hi = u_lo + period;

    let mut uv = Vec::with_capacity(lower.uv.len() + upper.uv.len() + 8);
    let mut xyz = Vec::with_capacity(uv.capacity());

    // Along the lower ring, left to right.
    for (p, q) in lower.uv.iter().zip(&lower.xyz) {
        uv.push(*p);
        xyz.push(*q);
    }
    // Close the lower ring onto the right-hand seam.
    let lower_end = Vec2::new(u_hi, lower.uv[0].v);
    uv.push(lower_end);
    xyz.push(lower.xyz[0]);

    // Up the right-hand seam, evaluated on the left so the two columns agree.
    seam_segment(
        &mut uv,
        &mut xyz,
        surface,
        u_hi,
        u_lo,
        lower_end.v,
        upper.uv[0].v,
        options,
    );

    // Back along the upper ring, right to left.
    let upper_start = Vec2::new(u_hi, upper.uv[0].v);
    uv.push(upper_start);
    xyz.push(upper.xyz[0]);
    for (p, q) in upper.uv.iter().zip(&upper.xyz).rev() {
        if uv.last().is_some_and(|l| (l.u - p.u).abs() < 1e-15 && (l.v - p.v).abs() < 1e-15) {
            continue;
        }
        uv.push(*p);
        xyz.push(*q);
    }

    // Down the left-hand seam, the same points in the opposite order.
    seam_segment(
        &mut uv,
        &mut xyz,
        surface,
        u_lo,
        u_lo,
        upper.uv[0].v,
        lower.uv[0].v,
        options,
    );

    UvLoop {
        area: signed_area(&uv),
        wrap: 0,
        uv,
        xyz,
    }
}

/// Interior points along a seam line, so the strip's ends are not one long edge.
///
/// `u` places the points in parameter space; `u_eval` is where they are
/// evaluated. The two differ by exactly one period on the right-hand seam, and
/// keeping the evaluation canonical is what makes the two columns of the strip
/// bit-identical in 3D — without it the seam is a hairline crack.
fn seam_segment(
    uv: &mut Vec<Vec2>,
    xyz: &mut Vec<Vec3>,
    surface: &Surface,
    u: f64,
    u_eval: f64,
    v_from: f64,
    v_to: f64,
    options: &Resolved,
) {
    let steps = v_steps(surface, u_eval, v_from, v_to, options);
    for i in 1..steps {
        let t = i as f64 / steps as f64;
        let v = v_from + (v_to - v_from) * t;
        uv.push(Vec2::new(u, v));
        xyz.push(surface.point_at(Vec2::new(u_eval, v)));
    }
}

/// How many segments the seam needs, from the surface's curvature along v.
fn v_steps(surface: &Surface, u: f64, v_from: f64, v_to: f64, options: &Resolved) -> usize {
    match surface {
        // Straight in v; one segment is exact.
        Surface::Cylinder { .. } | Surface::Cone { .. } | Surface::Plane { .. } => {
            options.min_edge_segments.max(1)
        }
        Surface::Sphere { radius, .. } => options.segments_for_arc(*radius, v_to - v_from),
        Surface::Torus { minor_radius, .. } => {
            options.segments_for_arc(*minor_radius, v_to - v_from)
        }
        _ => {
            // Sample the isoparametric line and count what the sag needs.
            let probe = |t: f64| surface.point_at(Vec2::new(u, v_from + (v_to - v_from) * t));
            adaptive_steps(&probe, options)
        }
    }
}

/// Segments an arbitrary parameterised path needs to stay within the sag.
fn adaptive_steps(probe: &dyn Fn(f64) -> Vec3, options: &Resolved) -> usize {
    let mut n = options.min_edge_segments.max(1);
    while n < 256 {
        let mut worst: f64 = 0.0;
        for i in 0..n {
            let a = probe(i as f64 / n as f64);
            let b = probe((i + 1) as f64 / n as f64);
            let m = probe((i as f64 + 0.5) / n as f64);
            worst = worst.max((m - (a + b) * 0.5).length());
        }
        if worst <= options.sag {
            break;
        }
        n *= 2;
    }
    n
}

/// The 3D length a unit step in each parameter direction covers, at the middle
/// of the domain.
fn parameter_scale(surface: &Surface, domain: &Domain) -> Vec2 {
    let mid = Vec2::new(
        clamp_finite(domain.u.at(0.5)),
        clamp_finite(domain.v.at(0.5)),
    );
    let (du, dv) = surface.derivatives_at(mid);
    let su = du.length();
    let sv = dv.length();
    // A degenerate direction — a sphere's pole — would scale to zero and
    // collapse the triangulation onto a line.
    Vec2::new(
        if su.is_finite() && su > 1e-12 { su } else { 1.0 },
        if sv.is_finite() && sv > 1e-12 { sv } else { 1.0 },
    )
}

fn clamp_finite(v: f64) -> f64 {
    if v.is_finite() { v.clamp(-1e9, 1e9) } else { 0.0 }
}

/// Fill the region and lift it to 3D.
#[allow(clippy::too_many_arguments)]
fn triangulate(
    surface: &Surface,
    domain: &Domain,
    outer: UvLoop,
    holes: Vec<UvLoop>,
    scale: Vec2,
    options: &Resolved,
    same_sense: bool,
) -> Result<Patch, String> {
    let mut cdt: ConstrainedDelaunayTriangulation<Point2<f64>> =
        ConstrainedDelaunayTriangulation::new();

    // Parameter points, their exact 3D positions where one is known, and the
    // handle spade gave them.
    let mut uv_of: Vec<Vec2> = Vec::new();
    let mut xyz_of: Vec<Option<Vec3>> = Vec::new();

    let insert = |cdt: &mut ConstrainedDelaunayTriangulation<Point2<f64>>,
                      uv_of: &mut Vec<Vec2>,
                      xyz_of: &mut Vec<Option<Vec3>>,
                      uv: Vec2,
                      xyz: Option<Vec3>|
     -> Option<usize> {
        let p = Point2::new(uv.u * scale.u, uv.v * scale.v);
        if !p.x.is_finite() || !p.y.is_finite() {
            return None;
        }
        let handle = cdt.insert(p).ok()?;
        let index = handle.index();
        if index >= uv_of.len() {
            uv_of.resize(index + 1, Vec2::default());
            xyz_of.resize(index + 1, None);
        }
        uv_of[index] = uv;
        // A boundary point's cached position wins; a later interior insertion
        // that lands on the same handle must not overwrite it.
        if xyz_of[index].is_none() {
            xyz_of[index] = xyz;
        }
        Some(index)
    };

    let constrain = |cdt: &mut ConstrainedDelaunayTriangulation<Point2<f64>>,
                         handles: &[usize]| {
        for w in 0..handles.len() {
            let a = handles[w];
            let b = handles[(w + 1) % handles.len()];
            if a == b {
                continue;
            }
            let (ha, hb) = (
                spade::handles::FixedVertexHandle::from_index(a),
                spade::handles::FixedVertexHandle::from_index(b),
            );
            if cdt.can_add_constraint(ha, hb) {
                cdt.add_constraint(ha, hb);
            }
        }
    };

    let mut boundary_handles = Vec::new();
    for (uv, xyz) in outer.uv.iter().zip(&outer.xyz) {
        if let Some(h) = insert(&mut cdt, &mut uv_of, &mut xyz_of, *uv, Some(*xyz)) {
            boundary_handles.push(h);
        }
    }
    if boundary_handles.len() < 3 {
        return Err("outer boundary collapsed to fewer than three distinct points".into());
    }
    constrain(&mut cdt, &boundary_handles);

    for hole in &holes {
        let mut hs = Vec::new();
        for (uv, xyz) in hole.uv.iter().zip(&hole.xyz) {
            if let Some(h) = insert(&mut cdt, &mut uv_of, &mut xyz_of, *uv, Some(*xyz)) {
                hs.push(h);
            }
        }
        if hs.len() >= 3 {
            constrain(&mut cdt, &hs);
        }
    }

    if options.interior_points {
        for uv in interior_samples(surface, domain, &outer, &holes, options) {
            insert(&mut cdt, &mut uv_of, &mut xyz_of, uv, None);
        }
    }

    // Keep the triangles whose centroid lies inside the outer loop and outside
    // every hole. Flood-filling across constraint edges would be faster, but
    // the seam segments this module inserts are constraints too, and a fill
    // would have to know which of them are boundaries — the containment test
    // has no such ambiguity.
    let mut patch = Patch::default();
    let mut remap = vec![u32::MAX; uv_of.len()];
    let mut kept = 0usize;

    for tri in cdt.inner_faces() {
        let vs = tri.vertices();
        let idx = [vs[0].index(), vs[1].index(), vs[2].index()];
        let centroid = Vec2::new(
            (uv_of[idx[0]].u + uv_of[idx[1]].u + uv_of[idx[2]].u) / 3.0,
            (uv_of[idx[0]].v + uv_of[idx[1]].v + uv_of[idx[2]].v) / 3.0,
        );
        if !contains(&outer.uv, centroid) {
            continue;
        }
        if holes.iter().any(|h| contains(&h.uv, centroid)) {
            continue;
        }

        let mut corner = [0u32; 3];
        for (k, &i) in idx.iter().enumerate() {
            if remap[i] == u32::MAX {
                let uv = uv_of[i];
                let p = xyz_of[i].unwrap_or_else(|| surface.point_at(uv));
                let mut n = surface.normal_at(uv);
                if !same_sense {
                    n = -n;
                }
                remap[i] = patch.positions.len() as u32;
                patch.positions.push([p.x as f32, p.y as f32, p.z as f32]);
                patch.normals.push([n.x as f32, n.y as f32, n.z as f32]);
            }
            corner[k] = remap[i];
        }

        // spade emits every inner face counter-clockwise in its own
        // coordinates, which are the scaled parameters. A CCW parameter
        // triangle has its 3D normal along ∂u × ∂v, i.e. the surface normal;
        // a face that reverses the surface must reverse the winding with it.
        if same_sense {
            patch.indices.extend_from_slice(&corner);
        } else {
            patch.indices.extend_from_slice(&[corner[0], corner[2], corner[1]]);
        }
        kept += 1;
    }

    if kept == 0 {
        return Err(format!(
            "no triangle centroid fell inside the boundary ({} boundary points, {} holes)",
            outer.uv.len(),
            holes.len()
        ));
    }
    Ok(patch)
}

/// Even-odd point-in-polygon test in parameter space.
fn contains(poly: &[Vec2], p: Vec2) -> bool {
    let mut inside = false;
    let n = poly.len();
    for i in 0..n {
        let a = poly[i];
        let b = poly[(i + 1) % n];
        if (a.v > p.v) != (b.v > p.v) {
            let t = (p.v - a.v) / (b.v - a.v);
            if p.u < a.u + t * (b.u - a.u) {
                inside = !inside;
            }
        }
    }
    inside
}

/// Interior parameter samples, spaced by what the surface's curvature needs.
///
/// A plane needs none — its boundary triangulation is already exact — which is
/// most faces in a mechanical model and most of the saving.
fn interior_samples(
    surface: &Surface,
    domain: &Domain,
    outer: &UvLoop,
    holes: &[UvLoop],
    options: &Resolved,
) -> Vec<Vec2> {
    if matches!(surface, Surface::Plane { .. }) {
        return Vec::new();
    }

    let (u_lo, u_hi) = span(outer.uv.iter().map(|p| p.u));
    let (v_lo, v_hi) = span(outer.uv.iter().map(|p| p.v));
    if !(u_hi > u_lo && v_hi > v_lo) {
        return Vec::new();
    }

    let nu = direction_steps(surface, domain, Axis::U, u_lo, u_hi, 0.5 * (v_lo + v_hi), options);
    let nv = direction_steps(surface, domain, Axis::V, v_lo, v_hi, 0.5 * (u_lo + u_hi), options);
    // A grid finer than this buys nothing a viewer can see and costs file size
    // on every part of an assembly at once.
    let nu = nu.min(96);
    let nv = nv.min(96);
    if nu <= 1 && nv <= 1 {
        return Vec::new();
    }

    let mut out = Vec::with_capacity((nu * nv) / 2);
    for i in 1..nu {
        for j in 1..nv {
            let uv = Vec2::new(
                u_lo + (u_hi - u_lo) * i as f64 / nu as f64,
                v_lo + (v_hi - v_lo) * j as f64 / nv as f64,
            );
            if !contains(&outer.uv, uv) {
                continue;
            }
            if holes.iter().any(|h| contains(&h.uv, uv)) {
                continue;
            }
            out.push(uv);
        }
    }
    out
}

enum Axis {
    U,
    V,
}

/// Segments one parameter direction needs across `[lo, hi]`.
fn direction_steps(
    surface: &Surface,
    _domain: &Domain,
    axis: Axis,
    lo: f64,
    hi: f64,
    other: f64,
    options: &Resolved,
) -> usize {
    let span = hi - lo;
    match (surface, &axis) {
        // Ruled in v: straight lines need no interior samples along it.
        (Surface::Cylinder { .. } | Surface::Cone { .. }, Axis::V) => 1,
        (Surface::Cylinder { radius, .. }, Axis::U) => options.segments_for_arc(*radius, span),
        (Surface::Cone {
            radius, half_angle, ..
        }, Axis::U) => {
            // The radius varies along the cone; the widest end sets the step.
            let r = (radius + other * half_angle.tan()).abs().max(radius.abs());
            options.segments_for_arc(r, span)
        }
        (Surface::Sphere { radius, .. }, Axis::U) => {
            // A parallel's radius shrinks toward the poles.
            options.segments_for_arc(radius * other.cos().abs().max(1e-3), span)
        }
        (Surface::Sphere { radius, .. }, Axis::V) => options.segments_for_arc(*radius, span),
        (Surface::Torus {
            major_radius,
            minor_radius,
            ..
        }, Axis::U) => options.segments_for_arc(major_radius + minor_radius.abs(), span),
        (Surface::Torus { minor_radius, .. }, Axis::V) => {
            options.segments_for_arc(*minor_radius, span)
        }
        _ => {
            let probe = |t: f64| {
                let p = match axis {
                    Axis::U => Vec2::new(lo + span * t, other),
                    Axis::V => Vec2::new(other, lo + span * t),
                };
                surface.point_at(p)
            };
            adaptive_steps(&probe, options)
        }
    }
}

fn span(values: impl Iterator<Item = f64>) -> (f64, f64) {
    let mut lo = f64::INFINITY;
    let mut hi = f64::NEG_INFINITY;
    for v in values {
        lo = lo.min(v);
        hi = hi.max(v);
    }
    (lo, hi)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn signed_area_is_positive_for_a_counter_clockwise_square() {
        let square = [
            Vec2::new(0.0, 0.0),
            Vec2::new(1.0, 0.0),
            Vec2::new(1.0, 1.0),
            Vec2::new(0.0, 1.0),
        ];
        assert!((signed_area(&square) - 1.0).abs() < 1e-15);
        let mut reversed = square;
        reversed.reverse();
        assert!((signed_area(&reversed) + 1.0).abs() < 1e-15);
    }

    #[test]
    fn containment_handles_a_square_with_a_hole_shape() {
        let square = [
            Vec2::new(0.0, 0.0),
            Vec2::new(4.0, 0.0),
            Vec2::new(4.0, 4.0),
            Vec2::new(0.0, 4.0),
        ];
        assert!(contains(&square, Vec2::new(2.0, 2.0)));
        assert!(!contains(&square, Vec2::new(5.0, 2.0)));
        assert!(!contains(&square, Vec2::new(2.0, -1.0)));
    }

    #[test]
    fn nearest_branch_picks_the_closest_period() {
        // 0.1 next to 6.2 on a 2*pi period belongs one turn up.
        let tau = std::f64::consts::TAU;
        let got = nearest_branch(0.1, 6.2, tau);
        assert!((got - (0.1 + tau)).abs() < 1e-12, "got {got}");
        assert!((nearest_branch(3.0, 3.1, tau) - 3.0).abs() < 1e-12);
    }

    #[test]
    fn net_wrap_counts_a_full_turn() {
        let tau = std::f64::consts::TAU;
        let domain = Domain {
            u: cad_ir::math::Interval::new(0.0, tau),
            v: cad_ir::math::Interval::new(0.0, 1.0),
            u_period: Some(tau),
            v_period: None,
        };
        let ring: Vec<Vec2> = (0..12)
            .map(|i| Vec2::new(tau * i as f64 / 12.0, 0.0))
            .collect();
        assert_eq!(net_wrap(&ring, &domain), 1);

        let closed = vec![
            Vec2::new(0.0, 0.0),
            Vec2::new(1.0, 0.0),
            Vec2::new(1.0, 1.0),
            Vec2::new(0.0, 1.0),
        ];
        assert_eq!(net_wrap(&closed, &domain), 0);
    }

    #[test]
    fn parameter_scale_never_collapses_a_direction() {
        use cad_ir::math::Frame;
        let s = Surface::Sphere {
            frame: Frame::IDENTITY,
            radius: 5.0,
        };
        let scale = parameter_scale(&s, &s.domain());
        assert!(scale.u > 0.0 && scale.v > 0.0);
        assert!(scale.u.is_finite() && scale.v.is_finite());
    }
}

/// A short name for a surface variant, for diagnostics.
fn surface_kind(s: &Surface) -> &'static str {
    match s {
        Surface::Plane { .. } => "plane",
        Surface::Cylinder { .. } => "cylinder",
        Surface::Cone { .. } => "cone",
        Surface::Sphere { .. } => "sphere",
        Surface::Torus { .. } => "torus",
        Surface::Nurbs(_) => "nurbs",
        Surface::LinearExtrusion { .. } => "extrusion",
        Surface::Revolution { .. } => "revolution",
        Surface::Offset { .. } => "offset",
        Surface::RectangularTrimmed { .. } => "trimmed",
    }
}

/// A short name for a curve variant, for diagnostics.
fn curve_kind(c: &cad_ir::brep::Curve) -> &'static str {
    use cad_ir::brep::Curve::*;
    match c {
        Line { .. } => "line",
        Circle { .. } => "circ",
        Ellipse { .. } => "elli",
        Parabola { .. } => "para",
        Hyperbola { .. } => "hyp",
        Polyline { .. } => "poly",
        Nurbs(_) => "nurb",
        Trimmed { .. } => "trim",
        Composite { .. } => "comp",
        OnSurface { .. } => "onsurf",
    }
}
