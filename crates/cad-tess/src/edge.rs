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
    solid
        .edges
        .par_iter()
        .map(|e| {
            let chain = discretise(solid, e, options);
            repair_runaway(solid, e, chain, &reference, options)
        })
        .collect()
}

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
    if reference.is_empty() {
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
    Chain {
        points: vec![start, end],
        params: vec![edge.range.lo, edge.range.hi],
    }
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

    Chain { points, params }
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
    // same answer and gives evenly spaced points instead of a binary ladder.
    if let Some(n) = analytic_segments(curve, range, options) {
        return (0..=n).map(|i| range.at(i as f64 / n as f64)).collect();
    }

    let mut out = Vec::with_capacity(16);
    out.push(range.lo);
    subdivide(curve, range.lo, range.hi, options, options.max_depth, &mut out);
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
            let a = semi_major.abs().max(*semi_minor);
            let b = semi_major.abs().min(*semi_minor);
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
