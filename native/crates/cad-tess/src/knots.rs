//! Where a piecewise surface or curve stops being one polynomial.
//!
//! A NURBS is a different polynomial on every knot span. A sampler that steps
//! across several spans at a time cannot see the shape between them, however
//! fine the step is in parameter — the samples land where they land and the
//! chord between them cuts whatever lies in the way. On a helical sweep this
//! is the difference between a wire and a smudge: the pilot assembly's spring
//! is a single face of 594 spans along its length, and a grid capped at 96
//! lines cuts six spans at a time, leaving the mesh 3.5 mm from the surface it
//! stands for on a wire 1.2 mm thick.
//!
//! So the breaks are read off the knot vector rather than guessed at. Not all
//! of them survive: a reader is free to write a flat sheet as a hundred spans,
//! and a break that the surface does not actually bend across earns nothing.
//! Each candidate is kept only if dropping it would move the chord further
//! than the tolerance already in force.

use cad_ir::brep::{Curve, NurbsCurve, NurbsSurface, Surface};
use cad_ir::math::Vec3;

/// Which parameter direction of a surface is being sampled.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Axis {
    U,
    V,
}

/// The distinct knots of `surface` in one direction, strictly inside `(lo, hi)`.
///
/// Returns an empty list for every surface that is one polynomial throughout,
/// which is all of the analytic ones.
pub fn surface_breaks(surface: &Surface, axis: Axis, lo: f64, hi: f64) -> Vec<f64> {
    match surface {
        Surface::Nurbs(n) => inside(nurbs_knots(n, axis), lo, hi),
        Surface::Offset { base, .. } => surface_breaks(base, axis, lo, hi),
        Surface::RectangularTrimmed { base, .. } => surface_breaks(base, axis, lo, hi),
        // A sweep carries its profile's breaks across the swept direction: a
        // spline profile extruded or revolved is piecewise in the direction
        // the profile runs, and smooth along the sweep.
        Surface::LinearExtrusion { profile, .. } | Surface::Revolution { profile, .. } => {
            match axis {
                Axis::U => inside(curve_knots(profile), lo, hi),
                Axis::V => Vec::new(),
            }
        }
        _ => Vec::new(),
    }
}

/// The distinct knots of `curve`, strictly inside `(lo, hi)`.
pub fn curve_breaks(curve: &Curve, lo: f64, hi: f64) -> Vec<f64> {
    inside(curve_knots(curve), lo, hi)
}

fn nurbs_knots(n: &NurbsSurface, axis: Axis) -> Vec<f64> {
    match axis {
        Axis::U => n.u_knots.clone(),
        Axis::V => n.v_knots.clone(),
    }
}

fn curve_knots(curve: &Curve) -> Vec<f64> {
    match curve {
        Curve::Nurbs(NurbsCurve { knots, .. }) => knots.clone(),
        Curve::Trimmed { base, .. } => curve_knots(base),
        Curve::Composite { segments } => segments
            .iter()
            .flat_map(|s| [s.range.lo, s.range.hi])
            .collect(),
        _ => Vec::new(),
    }
}

/// Distinct values strictly between `lo` and `hi`, in order.
///
/// A knot repeated to raise multiplicity is one break, not several, and a knot
/// sitting on the range's own end is the end, not an interior break.
fn inside(mut knots: Vec<f64>, lo: f64, hi: f64) -> Vec<f64> {
    let (lo, hi) = if lo <= hi { (lo, hi) } else { (hi, lo) };
    let width = hi - lo;
    if !(width > 0.0) {
        return Vec::new();
    }
    // Two knots closer together than this cannot be told apart by any sampler
    // working in this range, and keeping both only makes a zero-area triangle.
    let epsilon = width * 1e-9;
    knots.retain(|k| k.is_finite() && *k > lo + epsilon && *k < hi - epsilon);
    knots.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
    knots.dedup_by(|a, b| (*a - *b).abs() <= epsilon);
    knots
}

/// Drop breaks the geometry does not bend across.
///
/// Walks forward keeping a running start, and extends across each candidate as
/// long as the chord from the start to the candidate's successor stays within
/// `sag` of the curve between them. The first candidate that fails is kept and
/// becomes the new start. What survives is the smallest subset of the file's
/// own breaks that still holds the chord to the tolerance — so a sheet written
/// as a hundred redundant spans costs nothing, and a spring keeps every span
/// it needs.
pub fn thin_breaks(breaks: &[f64], lo: f64, hi: f64, sag: f64, at: &dyn Fn(f64) -> Vec3) -> Vec<f64> {
    if breaks.is_empty() || !(sag > 0.0) {
        return breaks.to_vec();
    }
    let mut kept: Vec<f64> = Vec::with_capacity(breaks.len());
    let mut start = lo;
    let mut i = 0;
    while i < breaks.len() {
        let next = breaks.get(i + 1).copied().unwrap_or(hi);
        if deviation(at, start, next) > sag {
            kept.push(breaks[i]);
            start = breaks[i];
        }
        i += 1;
    }
    kept
}

/// How far the geometry leaves the chord from `a` to `b`, sampled inside.
///
/// Three interior samples rather than one: a span that returns to its chord at
/// the midpoint — which a full turn of a helix does exactly — reads as flat to
/// a single midpoint test, and that is the case this whole module exists for.
fn deviation(at: &dyn Fn(f64) -> Vec3, a: f64, b: f64) -> f64 {
    let (pa, pb) = (at(a), at(b));
    let axis = pb - pa;
    let len2 = axis.length_squared();
    let mut worst: f64 = 0.0;
    for k in 1..4 {
        let t = k as f64 / 4.0;
        let p = at(a + (b - a) * t);
        let d = if len2 > 0.0 {
            let s = ((p - pa).dot(axis) / len2).clamp(0.0, 1.0);
            (p - (pa + axis * s)).length()
        } else {
            (p - pa).length()
        };
        worst = worst.max(d);
    }
    worst
}

/// Merge break parameters with `n` even divisions of `[lo, hi]`.
///
/// The even divisions are what a smooth-but-curved direction needs; the breaks
/// are what a piecewise one needs. A face is usually both, so it gets both.
pub fn merge_even(breaks: &[f64], lo: f64, hi: f64, n: usize) -> Vec<f64> {
    let mut all: Vec<f64> = Vec::with_capacity(breaks.len() + n + 1);
    for i in 0..=n {
        all.push(lo + (hi - lo) * i as f64 / n.max(1) as f64);
    }
    all.extend_from_slice(breaks);
    all.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
    let epsilon = (hi - lo).abs() * 1e-9;
    all.dedup_by(|a, b| (*a - *b).abs() <= epsilon);
    all
}

/// Merge break parameters into an already-chosen set of divisions.
pub fn merge_given(breaks: &[f64], mut all: Vec<f64>) -> Vec<f64> {
    if all.len() < 2 {
        return all;
    }
    let (lo, hi) = (all[0], all[all.len() - 1]);
    all.extend_from_slice(breaks);
    all.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
    let epsilon = (hi - lo).abs() * 1e-9;
    all.dedup_by(|a, b| (*a - *b).abs() <= epsilon);
    all
}
