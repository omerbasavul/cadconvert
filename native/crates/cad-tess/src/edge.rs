//! Edge discretisation — the step that makes the mesh watertight.
//!
//! Every edge is sampled exactly once, before any face is triangulated, and
//! both faces meeting at it are handed the same `f64` points. Nothing later in
//! the pipeline recomputes a boundary position, so there is no opportunity for
//! two faces to disagree about where their shared edge is.

use crate::options::Resolved;
use cad_ir::brep::{Curve, Edge, EdgeId, Solid};
use cad_ir::math::{Interval, Vec3};
use rayon::prelude::*;

/// One edge sampled into a polyline, running from its start vertex to its end.
#[derive(Debug, Clone, Default)]
pub struct Chain {
    /// At least two points. The first and last are the edge's vertices exactly.
    pub points: Vec<Vec3>,
    /// The curve parameter of each point, in the same order.
    pub params: Vec<f64>,
}

impl Chain {
    pub fn len(&self) -> usize {
        self.points.len()
    }

    pub fn is_empty(&self) -> bool {
        self.points.is_empty()
    }

    /// The points in traversal order for a half-edge.
    pub fn oriented(&self, forward: bool) -> Vec<(Vec3, f64)> {
        let it = self.points.iter().copied().zip(self.params.iter().copied());
        if forward {
            it.collect()
        } else {
            it.rev().collect()
        }
    }
}

/// Discretise every edge of a solid, in parallel.
pub fn discretise_all(solid: &Solid, options: &Resolved) -> Vec<Chain> {
    // The body's own extent, used to catch an edge whose parameter range was
    // recovered as the wrong arc. A reader inverts an edge's two vertices onto
    // its curve and has to choose which of the two arcs between them the edge
    // is; on a nearly degenerate conic the two vertices can invert to the same
    // parameter, and the choice becomes a coin toss between a hair-thin arc and
    // a five-metre one inside a half-metre body.
    let reference = solid.geometric_bounds();
    let chains: Vec<Chain> = solid
        .edges
        .par_iter()
        .map(|e| {
            // Each edge is chorded against its own size, not the model's: a
            // two-millimetre glyph outline in a half-metre assembly has to be
            // drawn as a glyph, not as a smudge the size of the tolerance.
            let local = options.for_extent(edge_extent(solid, e));
            let chain = discretise(solid, e, &local);
            repair_runaway(solid, e, chain, &reference, &local)
        })
        .collect();

    // How far each chain actually departs from the curve it stands for: the
    // true sagitta of every chord, measured against the curve rather than
    // predicted from a tolerance. This is the number a viewer sees as
    // faceting.
    if std::env::var_os("CAD_TESS_SAG").is_some() {
        for (ei, (e, c)) in solid.edges.iter().zip(&chains).enumerate() {
            if c.params.len() < 2 {
                continue;
            }
            let curve = &solid.curves[e.curve.index()];
            let mut worst = 0.0f64;
            let mut chord_len = 0.0f64;
            for w in c.params.windows(2) {
                let (pa, pb) = (curve.point_at(w[0]), curve.point_at(w[1]));
                let mid = curve.point_at(0.5 * (w[0] + w[1]));
                let ch = pb - pa;
                let len = ch.length();
                let dev = if len > 1e-300 {
                    let t = ((mid - pa).dot(ch) / (len * len)).clamp(0.0, 1.0);
                    (mid - (pa + ch * t)).length()
                } else {
                    (mid - pa).length()
                };
                if dev > worst {
                    worst = dev;
                    chord_len = len;
                }
            }
            let radius = match curve {
                Curve::Circle { radius, .. } => radius.abs(),
                _ => 0.0,
            };
            let inner = match curve {
                Curve::Trimmed { base, .. } => format!("{:?}", std::mem::discriminant(&**base)),
                _ => String::new(),
            };
            println!(
                "[sag] {worst:.6} {radius:.4} {chord_len:.4} {} {:?}{inner} range=[{:.6},{:.6}] nat=[{:.4},{:.4}] closed={} edge={ei}",
                c.params.len() - 1,
                std::mem::discriminant(curve),
                e.range.lo, e.range.hi,
                curve.natural_range().lo, curve.natural_range().hi,
                curve.is_closed()
            );
        }
    }
    chains
}

/// How far apart two chain points may be and still count as one.
///
/// Two bounds, and the tighter wins. The file states a tolerance for each edge
/// and each of the pair carries it, so points within a small multiple of it
/// cannot be told apart by anything the file says. And the mesh is allowed to
/// leave the curve by the sag anyway, so a pair closer than a fraction of that
/// carries no shape it could show. Taking the smaller keeps the merge inside
/// both statements — it never discards more than the file admits is uncertain,
/// and never more than the mesh would smooth away regardless.
const MERGE_TOLERANCES: f64 = 4.0;
const MERGE_SAG: f64 = 0.5;

/// Replace a chain that leaves the body with a better-founded one.
///
/// Tries the complementary arc first, since a wrong choice between the two arcs
/// of a periodic curve is the cause; falls back to the straight chord between
/// the vertices, which is wrong but bounded, and is at least the right shape
/// for the short arc it was supposed to be.
fn repair_runaway(
    solid: &Solid,
    edge: &Edge,
    chain: Chain,
    reference: &cad_ir::math::Aabb,
    options: &Resolved,
) -> Chain {
    // The same trap as the patch gate: a body whose extent collapsed to a
    // point cannot say that anything ran away from it.
    if reference.is_empty() || !(reference.diagonal() > 0.0) {
        return chain;
    }
    let centre = reference.centre();
    let limit = reference.diagonal() + options.sag * 16.0;
    let escapes = |c: &Chain| c.points.iter().any(|p| (*p - centre).length() > limit);
    if !escapes(&chain) {
        return chain;
    }

    let curve = &solid.curves[edge.curve.index()];
    if let Some(period) = curve.period() {
        // The complement runs from where this arc ends back round to where it
        // began.
        let complement = Interval::new(edge.range.hi, edge.range.lo + period);
        if complement.span() > 0.0 {
            let alternative = discretise(
                solid,
                &Edge {
                    range: complement,
                    ..edge.clone()
                },
                options,
            );
            if !escapes(&alternative) {
                return alternative;
            }
        }
    }

    let start = solid.vertices[edge.start.index()];
    let end = solid.vertices[edge.end.index()];

    // The range itself is what ran away, so before giving up on the curve, ask
    // it where its own two vertices actually sit. A very elongated conic — a
    // plane cutting a cylinder at a shallow angle gives an ellipse kilometres
    // long — inverts two vertices a millimetre apart to nearly the same
    // parameter, and the reader then cannot tell a hair-thin arc from the whole
    // turn. Sweeping the natural range for the nearest parameter to each vertex
    // answers it: the short arc between them is the edge, and on this pilot it
    // is the one edge in 26,535 that needed asking.
    let natural = curve.natural_range();
    if natural.span().is_finite() && natural.span() > 0.0 {
        let nearest = |target: Vec3| -> f64 {
            let steps = 512;
            (0..=steps)
                .map(|i| natural.at(i as f64 / steps as f64))
                .map(|t| (t, (curve.point_at(t) - target).length()))
                .min_by(|a, b| a.1.partial_cmp(&b.1).unwrap_or(std::cmp::Ordering::Equal))
                .map(|(t, _)| t)
                .unwrap_or(natural.lo)
        };
        let (a, b) = (nearest(start), nearest(end));
        let short = if a <= b {
            Interval::new(a, b)
        } else {
            Interval::new(b, a)
        };
        if short.span() > 0.0 {
            let arc = discretise(
                solid,
                &Edge {
                    range: short,
                    ..edge.clone()
                },
                options,
            );
            // It has to reach both vertices as well as stay inside the body;
            // an arc that does neither is no better than the chord.
            let reaches = |p: Vec3| {
                arc.points
                    .iter()
                    .any(|q| (*q - p).length() <= options.sag.max(edge.tolerance * 10.0))
            };
            if !escapes(&arc) && reaches(start) && reaches(end) {
                return arc;
            }
        }
    }

    // Nothing on the curve is usable. The chord between the vertices is, and
    // its parameters have to say so: leaving the runaway range on it would
    // have the chain claim a full turn where it draws a straight line, and
    // anything reading the parameters — a probe, a pcurve, a later repair —
    // would be told a story the points contradict.
    Chain {
        points: vec![start, end],
        params: vec![edge.range.lo, edge.range.lo],
    }
}

/// How big an edge is, coarsely — enough to size its own tolerance.
///
/// Its two vertices and a handful of points along it: the curve cannot be
/// sampled properly until the tolerance is known, and the tolerance needs the
/// size, so the size is taken from a fixed few.
fn edge_extent(solid: &Solid, edge: &Edge) -> f64 {
    let curve = &solid.curves[edge.curve.index()];
    let mut b = cad_ir::math::Aabb::EMPTY;
    b.add_point(solid.vertices[edge.start.index()]);
    b.add_point(solid.vertices[edge.end.index()]);
    if let Some(range) = usable_range(curve, edge.range) {
        const PROBES: usize = 8;
        for i in 0..=PROBES {
            let p = curve.point_at(range.at(i as f64 / PROBES as f64));
            if p.x.is_finite() && p.y.is_finite() && p.z.is_finite() {
                b.add_point(p);
            }
        }
    }
    let size = b.size();
    size.x.max(size.y).max(size.z)
}

/// Sample one edge.
pub fn discretise(solid: &Solid, edge: &Edge, options: &Resolved) -> Chain {
    let curve = &solid.curves[edge.curve.index()];
    let start = solid.vertices[edge.start.index()];
    let end = solid.vertices[edge.end.index()];

    let Some(range) = usable_range(curve, edge.range) else {
        // The range is unrecoverable and the curve has no bounded domain to
        // fall back on — an unbounded line whose two trim parameters came out
        // equal. Sampling its "natural" range would place points a trillion
        // millimetres away and swallow the whole model's bounding box, so the
        // honest answer is the chord between the vertices.
        return Chain {
            points: vec![start, end],
            params: vec![edge.range.lo, edge.range.hi],
        };
    };
    let mut params = sample_params(curve, range, options);
    if params.len() < 2 {
        params = vec![range.lo, range.hi];
    }

    let mut points: Vec<Vec3> = params.iter().map(|&t| curve.point_at(t)).collect();

    // Orient start-to-end. `Edge::same_sense` says whether the edge agrees with
    // its curve, but the geometry is the more reliable witness: a reader that
    // recovered the range by inverting both vertices has already encoded the
    // direction in the range, and trusting the flag over the points is how an
    // edge ends up traversed backwards in one face and forwards in the other.
    let degenerate_ends = (end - start).length_squared() <= (edge.tolerance * 10.0).powi(2);
    if !degenerate_ends {
        let head_first = (points[0] - start).length_squared();
        let head_last = (points[points.len() - 1] - start).length_squared();
        if head_last < head_first {
            points.reverse();
            params.reverse();
        }
    } else if !edge.same_sense {
        // A closed edge has no near-end to compare, so the flag is all there
        // is; getting it wrong reverses the loop, not the geometry.
        points.reverse();
        params.reverse();
    }

    // Pin the ends to the vertices. Two edges meeting at a vertex must agree
    // about it to the bit, and evaluating each curve there gives answers that
    // differ in the last few ulps.
    let last = points.len() - 1;
    points[0] = start;
    points[last] = end;

    // A curve carried as a sampled polyline — which is how a Parasolid
    // parameter-space curve arrives, its points being where the reader chose
    // to evaluate a surface, not where any criterion asked for them — is
    // sampled evenly in its own parameter,
    // and where that parameter runs unevenly the samples pile up: on the pilot
    // assembly 20,368 of its chain segments are shorter than a hundredth of
    // the sag, against 64 on the STEP side. Points that fine cannot carry
    // shape the mesh is allowed to show, but each one makes a sliver triangle,
    // and where the face across the edge cuts it differently, a crack. They
    // are dropped only where dropping them provably moves the chain less than
    // a hundredth of the tolerance it is already held to, so the curve is kept
    // a hundred times more faithfully than the mesh around it.
    if is_sampled(curve) {
        thin(
            &mut points,
            &mut params,
            options.sag * 0.01,
            options.angle,
            options.min_edge_segments + 1,
        );
    }

    // Two chain points closer together than the edge's own stated tolerance
    // are one point as far as anything downstream can tell, and the pair is
    // what a triangulation turns into a sliver: on the pilot, a handful of
    // them at one corner of a 256 mm body left several faces meeting in a way
    // no orientation can satisfy — sixteen unsatisfiable constraints, and a
    // 4 µm tangle of reversed and non-manifold edges. Dropping the second of
    // each pair is safe precisely because it happens *here*: the chain is
    // built once and both faces along the edge receive it, so neither can
    // split what the other merged. The ends are never dropped — they are the
    // vertices two faces have to agree about to the bit.
    let floor = (edge.tolerance.max(0.0) * MERGE_TOLERANCES).min(options.sag * MERGE_SAG);
    if floor > 0.0 && points.len() > 2 {
        let mut kept_points = Vec::with_capacity(points.len());
        let mut kept_params = Vec::with_capacity(params.len());
        kept_points.push(points[0]);
        kept_params.push(params[0]);
        for i in 1..points.len() - 1 {
            let last = *kept_points.last().expect("seeded above");
            if (points[i] - last).length() > floor {
                kept_points.push(points[i]);
                kept_params.push(params[i]);
            }
        }
        let last = *kept_points.last().expect("seeded above");
        let end = points[points.len() - 1];
        // If the final point is too close to what precedes it, the point
        // before it goes rather than the vertex itself.
        if (end - last).length() <= floor && kept_points.len() > 1 {
            kept_points.pop();
            kept_params.pop();
        }
        kept_points.push(end);
        kept_params.push(params[params.len() - 1]);
        if kept_points.len() >= 2 {
            points = kept_points;
            params = kept_params;
        }
    }

    Chain { points, params }
}

/// Whether a curve's points are a reader's samples rather than a criterion's.
///
/// A polyline is what a parameter-space curve becomes when it is read: its
/// points are wherever the reader evaluated the surface, spaced by the curve's
/// own parameter, which says nothing about how much shape lies between them.
/// Every other curve here is evaluated on demand, at parameters the sag and
/// angle criteria chose, and those points are all load-bearing.
fn is_sampled(curve: &Curve) -> bool {
    match curve {
        Curve::Polyline { .. } => true,
        Curve::Trimmed { base, .. } => is_sampled(base),
        _ => false,
    }
}

/// Drop points a polyline does not need, to a bounded deviation.
///
/// Douglas-Peucker, against both of the criteria the sampling itself was held
/// to: the point furthest from the chord is kept if it is further than
/// `tolerance`, and so is any point where the chain turns by more than `angle`
/// across the span being replaced. Distance alone is not enough — a shallow
/// arc can sit within any distance you like while turning through a corner,
/// and the angular limit is there precisely so a curve gets points a chord
/// test would not ask for. Whatever is dropped was within `tolerance` of the
/// line that replaces it and turned less than `angle` doing it, so the chain
/// keeps both guarantees the tessellator makes. The ends are never candidates
/// — they are the edge's vertices, which two faces have to agree about to the
/// bit.
fn thin(
    points: &mut Vec<Vec3>,
    params: &mut Vec<f64>,
    tolerance: f64,
    angle: f64,
    floor: usize,
) {
    if points.len() < 3 || !(tolerance > 0.0) {
        return;
    }
    let mut keep = vec![false; points.len()];
    keep[0] = true;
    keep[points.len() - 1] = true;
    let mut stack = vec![(0usize, points.len() - 1)];
    while let Some((a, b)) = stack.pop() {
        if b <= a + 1 {
            continue;
        }
        let (pa, pb) = (points[a], points[b]);
        let axis = pb - pa;
        let len2 = axis.length_squared();
        let mut worst = (0.0f64, a);
        for (i, p) in points.iter().enumerate().take(b).skip(a + 1) {
            let d = if len2 > 0.0 {
                let t = ((*p - pa).dot(axis) / len2).clamp(0.0, 1.0);
                (*p - (pa + axis * t)).length()
            } else {
                (*p - pa).length()
            };
            if d > worst.0 {
                worst = (d, i);
            }
        }
        // How far the chain turns across this span: the angle from the first
        // segment round to the last, taken through the chord. That is the same
        // quantity the subdivision measured between the tangents at the span's
        // two ends — over an arc it comes to the arc's own turn — and it needs
        // no curve to ask, only the points.
        let ends = points[a..=b]
            .windows(2)
            .filter_map(|w| (w[1] - w[0]).try_normalized());
        let first = ends.clone().next();
        let last = ends.last();
        let turn = match (first, last, axis.try_normalized()) {
            (Some(f), Some(l), Some(chord)) => {
                f.dot(chord).clamp(-1.0, 1.0).acos() + chord.dot(l).clamp(-1.0, 1.0).acos()
            }
            _ => 0.0,
        };
        if worst.0 > tolerance || turn > angle {
            keep[worst.1] = true;
            stack.push((a, worst.1));
            stack.push((worst.1, b));
        }
    }
    // The segment floor is the tessellator's own, and it is there so a
    // straight edge on a curved face still gives the triangulation something
    // to hold on to. Thinning must not take that away, so where it would, the
    // points it dropped are put back evenly.
    let kept = keep.iter().filter(|k| **k).count();
    if kept < floor.min(points.len()) {
        let want = floor.min(points.len());
        for i in 0..want {
            keep[i * (points.len() - 1) / (want - 1).max(1)] = true;
        }
    }
    let mut i = 0;
    points.retain(|_| {
        i += 1;
        keep[i - 1]
    });
    let mut j = 0;
    params.retain(|_| {
        j += 1;
        keep[j - 1]
    });
}

/// Clamp an edge's range into the curve's actual domain.
///
/// A range that is empty, reversed or outside the domain — all of which appear
/// in real files — is replaced by the curve's own range rather than producing
/// an edge with no points.
fn usable_range(curve: &Curve, range: Interval) -> Option<Interval> {
    let natural = curve.natural_range();
    if !range.lo.is_finite() || !range.hi.is_finite() || range.span() <= 0.0 {
        // Only fall back to the natural range when it is a real domain. Lines,
        // parabolas and hyperbolas report a nominal one spanning the whole
        // representable model space, which is not something to sample.
        let bounded = natural.span().is_finite() && natural.span().abs() < 1e9;
        return bounded.then_some(natural);
    }
    // A periodic curve legitimately runs past its natural upper bound.
    if curve.period().is_some() {
        return Some(range);
    }
    let slack = natural.span().abs() * 1e-9;
    Some(Interval::new(
        range.lo.max(natural.lo - slack),
        range.hi.min(natural.hi + slack),
    ))
}

/// Choose parameters along `range` meeting both the sag and angle criteria.
fn sample_params(curve: &Curve, range: Interval, options: &Resolved) -> Vec<f64> {
    // Analytic arcs have a closed-form step, which beats bisecting to find the
    // same answer and gives evenly spaced points instead of a binary ladder —
    // but only when the closed form is given the right radius. On this pilot
    // one ellipse arrived with semi-axes the formula could make nothing of and
    // came back as a single segment for a full turn, which drew a closed curve
    // as one chord: a chain of two coincident points, standing 4.9 metres from
    // the curve it stood for, and the largest single error left in the model.
    // So the answer is checked against the criterion it was meant to satisfy,
    // and where it fails the curve goes the long way round instead.
    if let Some(n) = analytic_segments(curve, range, options) {
        let params: Vec<f64> = (0..=n).map(|i| range.at(i as f64 / n as f64)).collect();
        let worst = params
            .windows(2)
            .map(|w| {
                let (a, b) = (curve.point_at(w[0]), curve.point_at(w[1]));
                let m = curve.point_at(0.5 * (w[0] + w[1]));
                (m - (a + b) * 0.5).length()
            })
            .fold(0.0f64, f64::max);
        if worst <= options.sag {
            return params;
        }
    }

    // Bisection alone cannot find shape it steps over. A spline written in
    // many spans — the helical edge of a spring or a thread runs to hundreds —
    // returns to its own chord at the midpoint once per turn, and a subdivider
    // that asks only about the midpoint reads that as flat and stops. So the
    // curve's own breaks seed the walk, and bisection refines between them.
    let breaks = if std::env::var_os("CAD_TESS_NO_KNOTS").is_some() {
        Vec::new()
    } else {
        crate::knots::thin_breaks(
        &crate::knots::curve_breaks(curve, range.lo, range.hi),
        range.lo,
        range.hi,
        options.sag,
        &|t| curve.point_at(t),
        )
    };

    // The walk has to run the range's own way round. A trimmed curve is free
    // to be given with its high parameter first, and breaks read off an
    // ascending knot vector would then step backwards through it, handing the
    // face a boundary that doubles back on itself.
    let mut ordered = breaks;
    if range.hi < range.lo {
        ordered.reverse();
    }

    let mut out = Vec::with_capacity(16 + ordered.len());
    out.push(range.lo);
    let mut left = range.lo;
    for b in ordered.iter().copied().chain(std::iter::once(range.hi)) {
        subdivide(curve, left, b, options, options.max_depth, &mut out);
        if b != range.hi {
            out.push(b);
        }
        left = b;
    }
    out.push(range.hi);

    // Enforce the segment floor by splitting uniformly if the criteria were
    // satisfied too easily — a straight edge on a curved face still needs
    // interior points for the face's triangulation to have anything to work
    // with.
    if out.len() - 1 < options.min_edge_segments {
        let n = options.min_edge_segments;
        return (0..=n).map(|i| range.at(i as f64 / n as f64)).collect();
    }
    out
}

/// Segment count for a curve whose curvature is known in closed form.
fn analytic_segments(curve: &Curve, range: Interval, options: &Resolved) -> Option<usize> {
    match curve {
        Curve::Line { .. } => Some(options.min_edge_segments.max(1)),
        Curve::Circle { radius, .. } => Some(options.segments_for_arc(*radius, range.span())),
        Curve::Ellipse {
            semi_major,
            semi_minor,
            ..
        } => {
            // The tightest curvature on an ellipse is at the end of the major
            // axis, radius b²/a. Sampling for that everywhere costs a few
            // extra points and never under-resolves.
            // Both are lengths; a file is free to write either negative and
            // the tightest-curvature formula is meaningless if one slips
            // through with its sign.
            let a = semi_major.abs().max(semi_minor.abs());
            let b = semi_major.abs().min(semi_minor.abs());
            let tightest = if a > 0.0 { b * b / a } else { 0.0 };
            Some(options.segments_for_arc(tightest.max(b), range.span()))
        }
        Curve::Trimmed { base, .. } => analytic_segments(base, range, options),
        _ => None,
    }
}

/// Bisect until both the chord deviation and the turn angle are within budget.
fn subdivide(curve: &Curve, a: f64, b: f64, options: &Resolved, depth: u32, out: &mut Vec<f64>) {
    if depth == 0 {
        return;
    }
    let m = 0.5 * (a + b);
    let pa = curve.point_at(a);
    let pb = curve.point_at(b);
    let pm = curve.point_at(m);

    let chord = pb - pa;
    let len = chord.length();
    let deviation = if len > 1e-300 {
        let t = ((pm - pa).dot(chord) / (len * len)).clamp(0.0, 1.0);
        (pm - (pa + chord * t)).length()
    } else {
        (pm - pa).length()
    };

    let turn = match (curve.tangent_at(a), curve.tangent_at(b)) {
        (Some(ta), Some(tb)) => ta.dot(tb).clamp(-1.0, 1.0).acos(),
        _ => 0.0,
    };

    if deviation <= options.sag && turn <= options.angle {
        return;
    }
    subdivide(curve, a, m, options, depth - 1, out);
    out.push(m);
    subdivide(curve, m, b, options, depth - 1, out);
}

/// The chains of a solid, addressed by edge.
pub trait ChainLookup {
    fn chain(&self, id: EdgeId) -> &Chain;
}

impl ChainLookup for Vec<Chain> {
    fn chain(&self, id: EdgeId) -> &Chain {
        &self[id.index()]
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::Options;
    use cad_ir::brep::{CurveId, VertexId};
    use cad_ir::math::{Frame, TAU};

    fn solid_with(curve: Curve, start: Vec3, end: Vec3, range: Interval, same_sense: bool) -> Solid {
        Solid {
            vertices: vec![start, end],
            curves: vec![curve],
            edges: vec![Edge {
                start: VertexId(0),
                end: VertexId(1),
                curve: CurveId(0),
                same_sense,
                range,
                tolerance: 1e-9,
            }],
            tolerance: 1e-9,
            ..Default::default()
        }
    }

    #[test]
    fn a_straight_edge_gets_its_two_end_points() {
        let s = solid_with(
            Curve::Line {
                origin: Vec3::ZERO,
                direction: Vec3::new(10.0, 0.0, 0.0),
            },
            Vec3::ZERO,
            Vec3::new(10.0, 0.0, 0.0),
            Interval::UNIT,
            true,
        );
        let chains = discretise_all(&s, &Options::default().resolve(10.0));
        assert_eq!(chains[0].len(), 2);
        assert_eq!(chains[0].points[0], Vec3::ZERO);
        assert_eq!(chains[0].points[1], Vec3::new(10.0, 0.0, 0.0));
    }

    #[test]
    fn end_points_are_pinned_to_the_vertices_exactly() {
        // A vertex that is a hair off the curve — which real files contain —
        // must still be reproduced bit-for-bit, or the two edges meeting there
        // disagree and the mesh opens up.
        let off = Vec3::new(10.0, 1e-9, 0.0);
        let s = solid_with(
            Curve::Line {
                origin: Vec3::ZERO,
                direction: Vec3::new(10.0, 0.0, 0.0),
            },
            Vec3::ZERO,
            off,
            Interval::UNIT,
            true,
        );
        let chains = discretise_all(&s, &Options::default().resolve(10.0));
        let last = chains[0].points.last().copied().unwrap();
        assert_eq!(last.x.to_bits(), off.x.to_bits());
        assert_eq!(last.y.to_bits(), off.y.to_bits());
    }

    #[test]
    fn an_arc_is_sampled_within_the_sag_tolerance() {
        let s = solid_with(
            Curve::Circle {
                frame: Frame::IDENTITY,
                radius: 50.0,
            },
            Vec3::new(50.0, 0.0, 0.0),
            Vec3::new(0.0, 50.0, 0.0),
            Interval::new(0.0, TAU / 4.0),
            true,
        );
        let opts = Options {
            linear_deflection: 0.05,
            relative: false,
            angular_deflection: std::f64::consts::PI,
            ..Options::default()
        }
        .resolve(1.0);
        let chain = &discretise_all(&s, &opts)[0];
        assert!(chain.len() > 5);
        for w in chain.points.windows(2) {
            // Midpoint of the chord vs the true arc.
            let mid = (w[0] + w[1]) * 0.5;
            let sag = 50.0 - mid.length();
            assert!(sag <= 0.05 + 1e-9, "sag {sag} exceeds tolerance");
        }
    }

    #[test]
    fn a_chain_runs_from_the_start_vertex_to_the_end_vertex() {
        // Range ordered lo..hi, but the start vertex is at the high end.
        let s = solid_with(
            Curve::Line {
                origin: Vec3::ZERO,
                direction: Vec3::new(10.0, 0.0, 0.0),
            },
            Vec3::new(10.0, 0.0, 0.0),
            Vec3::ZERO,
            Interval::UNIT,
            false,
        );
        let chain = &discretise_all(&s, &Options::default().resolve(10.0))[0];
        assert_eq!(chain.points[0], Vec3::new(10.0, 0.0, 0.0));
        assert_eq!(*chain.points.last().unwrap(), Vec3::ZERO);
    }

    #[test]
    fn a_closed_edge_keeps_both_ends_at_the_same_vertex() {
        let mut s = solid_with(
            Curve::Circle {
                frame: Frame::IDENTITY,
                radius: 3.0,
            },
            Vec3::new(3.0, 0.0, 0.0),
            Vec3::new(3.0, 0.0, 0.0),
            Interval::new(0.0, TAU),
            true,
        );
        s.edges[0].end = VertexId(0);
        s.vertices.truncate(1);
        let chain = &discretise_all(&s, &Options::default().resolve(6.0))[0];
        assert_eq!(chain.points[0], chain.points[chain.len() - 1]);
        assert!(chain.len() > 8, "a full circle needs more than {} points", chain.len());
    }

    #[test]
    fn oriented_reverses_both_points_and_parameters() {
        let c = Chain {
            points: vec![Vec3::ZERO, Vec3::X, Vec3::Y],
            params: vec![0.0, 0.5, 1.0],
        };
        let fwd = c.oriented(true);
        let rev = c.oriented(false);
        assert_eq!(fwd[0].0, Vec3::ZERO);
        assert_eq!(rev[0].0, Vec3::Y);
        assert_eq!(rev[0].1, 1.0);
        assert_eq!(rev[2].1, 0.0);
    }

    #[test]
    fn a_degenerate_range_falls_back_to_the_whole_curve() {
        let s = solid_with(
            Curve::Circle {
                frame: Frame::IDENTITY,
                radius: 2.0,
            },
            Vec3::new(2.0, 0.0, 0.0),
            Vec3::new(2.0, 0.0, 0.0),
            Interval::new(0.5, 0.5),
            true,
        );
        let chain = &discretise_all(&s, &Options::default().resolve(4.0))[0];
        assert!(chain.len() > 4, "collapsed to {} points", chain.len());
    }

    #[test]
    fn thinning_drops_only_what_carries_no_shape() {
        // A quarter arc sampled far more finely than anything asks for: the
        // shape it describes has to survive, the surplus points must not.
        const N: usize = 400;
        let mut points: Vec<Vec3> = (0..=N)
            .map(|i| {
                let a = std::f64::consts::FRAC_PI_2 * i as f64 / N as f64;
                Vec3::new(10.0 * a.cos(), 10.0 * a.sin(), 0.0)
            })
            .collect();
        let mut params: Vec<f64> = (0..=N).map(|i| i as f64 / N as f64).collect();
        let before = points.clone();

        let tolerance = 0.01;
        thin(&mut points, &mut params, tolerance, 15f64.to_radians(), 2);

        assert!(points.len() < before.len() / 4, "kept {}", points.len());
        assert_eq!(points.len(), params.len());
        assert_eq!(points[0], before[0]);
        assert_eq!(points[points.len() - 1], before[before.len() - 1]);

        // Every original point is still within the tolerance of the chain
        // that replaced it — which is the whole claim being made.
        for q in &before {
            let mut best = f64::INFINITY;
            for w in points.windows(2) {
                let d = w[1] - w[0];
                let len2 = d.length_squared();
                let t = if len2 > 0.0 {
                    ((*q - w[0]).dot(d) / len2).clamp(0.0, 1.0)
                } else {
                    0.0
                };
                best = best.min((*q - (w[0] + d * t)).length());
            }
            assert!(best <= tolerance, "a point moved {best}");
        }
    }

    #[test]
    fn thinning_leaves_a_straight_run_alone_when_the_floor_asks() {
        // Nothing to drop by shape, but the segment floor still has to hold.
        let mut points: Vec<Vec3> = (0..=8).map(|i| Vec3::new(i as f64, 0.0, 0.0)).collect();
        let mut params: Vec<f64> = (0..=8).map(|i| i as f64).collect();
        thin(&mut points, &mut params, 0.01, 15f64.to_radians(), 5);
        assert!(points.len() >= 5, "kept {}", points.len());
        assert_eq!(points.len(), params.len());
    }

    #[test]
    fn the_angular_limit_forces_subdivision_a_loose_sag_would_not() {
        let curve = Curve::Circle {
            frame: Frame::IDENTITY,
            radius: 1.0,
        };
        let s = solid_with(
            curve,
            Vec3::new(1.0, 0.0, 0.0),
            Vec3::new(1.0, 0.0, 0.0),
            Interval::new(0.0, TAU),
            true,
        );
        let loose = Options {
            linear_deflection: 100.0,
            relative: false,
            angular_deflection: 15f64.to_radians(),
            ..Options::default()
        }
        .resolve(1.0);
        let chain = &discretise_all(&s, &loose)[0];
        // A full turn at 15 degrees is 24 segments.
        assert!(chain.len() >= 24, "only {} points", chain.len());
    }
}
