//! Evaluating and inverting the IR's curves.
//!
//! Every parameterisation here follows ISO 10303-42, which Parasolid also
//! matches for the analytic forms, so a curve lowered from either reader
//! evaluates identically.
//!
//! Inversion — recovering the parameter of a known point — matters as much as
//! evaluation: a STEP `EDGE_CURVE` names its end *vertices*, not the parameters
//! at which they sit, and getting those parameters wrong makes an edge span the
//! wrong arc of its circle. Analytic curves invert in closed form; only splines
//! need the sample-and-refine path.

use crate::brep::{CompositeSegment, Curve, NurbsCurve};
use crate::eval::nurbs;
use crate::math::{Interval, TAU, Vec3};

/// How far outside its domain a parameter may stray before it is rejected.
///
/// Files do write trim parameters a rounding error past the end of a knot
/// vector, and refusing those would drop real edges.
const DOMAIN_SLACK: f64 = 1e-9;

impl Curve {
    /// The parameter interval over which the curve is defined.
    ///
    /// Unbounded curves report a range wide enough to cover any edge that could
    /// reasonably be trimmed out of them; the topology layer always narrows it
    /// with the real end points.
    pub fn natural_range(&self) -> Interval {
        match self {
            Curve::Line { .. } => Interval::new(-1e12, 1e12),
            Curve::Circle { .. } | Curve::Ellipse { .. } => Interval::new(0.0, TAU),
            Curve::Parabola { .. } | Curve::Hyperbola { .. } => Interval::new(-1e6, 1e6),
            Curve::Polyline { points } => {
                Interval::new(0.0, points.len().saturating_sub(1) as f64)
            }
            Curve::Nurbs(n) => match nurbs::domain(&n.knots, n.degree) {
                Some((lo, hi)) => Interval::new(lo, hi),
                None => Interval::UNIT,
            },
            Curve::Trimmed { range, .. } => *range,
            Curve::Composite { segments } => Interval::new(0.0, segments.len() as f64),
            Curve::OnSurface { .. } => Interval::UNIT,
        }
    }

    /// True when the curve closes on itself over its natural range.
    pub fn is_closed(&self) -> bool {
        match self {
            Curve::Circle { .. } | Curve::Ellipse { .. } => true,
            Curve::Nurbs(n) => n.closed,
            Curve::Polyline { points } => {
                points.len() > 2
                    && (points[0] - points[points.len() - 1]).length_squared() < 1e-20
            }
            Curve::Trimmed { base, range } => {
                base.is_closed() && (range.span().abs() - base.natural_range().span()).abs() < 1e-9
            }
            _ => false,
        }
    }

    /// True when the parameter wraps — advancing by the period returns the same
    /// point. Only the conics are periodic.
    pub fn period(&self) -> Option<f64> {
        match self {
            Curve::Circle { .. } | Curve::Ellipse { .. } => Some(TAU),
            Curve::Trimmed { base, .. } => base.period(),
            _ => None,
        }
    }

    /// The point at parameter `t`.
    pub fn point_at(&self, t: f64) -> Vec3 {
        match self {
            Curve::Line { origin, direction } => *origin + *direction * t,
            Curve::Circle { frame, radius } => frame.polar(*radius, t),
            Curve::Ellipse {
                frame,
                semi_major,
                semi_minor,
            } => {
                frame.origin
                    + frame.ref_dir * (*semi_major * t.cos())
                    + frame.y_dir() * (*semi_minor * t.sin())
            }
            // ISO 10303-42: C(u) = O + F·u²·x + 2F·u·y.
            Curve::Parabola { frame, focal_dist } => {
                frame.origin
                    + frame.ref_dir * (*focal_dist * t * t)
                    + frame.y_dir() * (2.0 * *focal_dist * t)
            }
            // ISO 10303-42: C(u) = O + a·cosh(u)·x + b·sinh(u)·y.
            Curve::Hyperbola {
                frame,
                semi_major,
                semi_minor,
            } => {
                frame.origin
                    + frame.ref_dir * (*semi_major * t.cosh())
                    + frame.y_dir() * (*semi_minor * t.sinh())
            }
            Curve::Polyline { points } => polyline_point(points, t),
            Curve::Nurbs(n) => nurbs_point_derivative(n, t).0,
            Curve::Trimmed { base, .. } => base.point_at(t),
            Curve::Composite { segments } => composite_at(segments, t).0,
            Curve::OnSurface { .. } => Vec3::ZERO,
        }
    }

    /// The first derivative at `t`, i.e. the unnormalised tangent.
    pub fn derivative_at(&self, t: f64) -> Vec3 {
        match self {
            Curve::Line { direction, .. } => *direction,
            Curve::Circle { frame, radius } => {
                frame.ref_dir * (-*radius * t.sin()) + frame.y_dir() * (*radius * t.cos())
            }
            Curve::Ellipse {
                frame,
                semi_major,
                semi_minor,
            } => {
                frame.ref_dir * (-*semi_major * t.sin()) + frame.y_dir() * (*semi_minor * t.cos())
            }
            Curve::Parabola { frame, focal_dist } => {
                frame.ref_dir * (2.0 * *focal_dist * t) + frame.y_dir() * (2.0 * *focal_dist)
            }
            Curve::Hyperbola {
                frame,
                semi_major,
                semi_minor,
            } => {
                frame.ref_dir * (*semi_major * t.sinh()) + frame.y_dir() * (*semi_minor * t.cosh())
            }
            Curve::Polyline { points } => polyline_derivative(points, t),
            Curve::Nurbs(n) => nurbs_point_derivative(n, t).1,
            Curve::Trimmed { base, .. } => base.derivative_at(t),
            Curve::Composite { segments } => composite_at(segments, t).1,
            Curve::OnSurface { .. } => Vec3::ZERO,
        }
    }

    /// The unit tangent at `t`, or `None` at a cusp.
    pub fn tangent_at(&self, t: f64) -> Option<Vec3> {
        self.derivative_at(t).try_normalized()
    }

    /// The parameter at which the curve passes through `p`.
    ///
    /// `hint` seeds the search for the spline case and disambiguates the
    /// periodic case; pass the other end of the edge where you have it.
    pub fn param_of(&self, p: Vec3, hint: Option<f64>) -> Option<f64> {
        match self {
            Curve::Line { origin, direction } => {
                let d2 = direction.length_squared();
                (d2 > 0.0).then(|| (p - *origin).dot(*direction) / d2)
            }
            Curve::Circle { frame, .. } => Some(angle_in_frame(frame, p)),
            Curve::Ellipse {
                frame,
                semi_major,
                semi_minor,
            } => {
                let d = p - frame.origin;
                let x = d.dot(frame.ref_dir);
                let y = d.dot(frame.y_dir());
                // Normalising by the semi-axes turns the ellipse back into the
                // unit circle, where the angle is the parameter.
                if *semi_major == 0.0 || *semi_minor == 0.0 {
                    return None;
                }
                Some(wrap_tau((y / *semi_minor).atan2(x / *semi_major)))
            }
            Curve::Parabola { frame, focal_dist } => {
                let y = (p - frame.origin).dot(frame.y_dir());
                (*focal_dist != 0.0).then(|| y / (2.0 * *focal_dist))
            }
            Curve::Hyperbola {
                frame, semi_minor, ..
            } => {
                let y = (p - frame.origin).dot(frame.y_dir());
                (*semi_minor != 0.0).then(|| (y / *semi_minor).asinh())
            }
            Curve::Polyline { points } => polyline_param(points, p),
            Curve::Nurbs(_) => self.param_by_search(p, hint),
            Curve::Trimmed { base, range } => {
                let t = base.param_of(p, hint.or(Some(range.lo)))?;
                // A periodic base can report the same point one period away
                // from the trim; bring it back into range.
                Some(match base.period() {
                    Some(period) => nearest_congruent(t, range, period),
                    None => t,
                })
            }
            Curve::Composite { .. } => self.param_by_search(p, hint),
            Curve::OnSurface { .. } => None,
        }
    }

    /// Sample-then-refine inversion, for curves with no closed form.
    ///
    /// The coarse sweep finds the right basin — Newton alone would converge to
    /// whichever root it started nearest, which on a wiggly spline is often the
    /// wrong one — and Newton then converges quadratically inside it.
    fn param_by_search(&self, p: Vec3, hint: Option<f64>) -> Option<f64> {
        let range = self.natural_range();
        const SAMPLES: usize = 64;

        let mut best = hint.unwrap_or(range.lo);
        let mut best_d2 = (self.point_at(best) - p).length_squared();
        for i in 0..=SAMPLES {
            let t = range.at(i as f64 / SAMPLES as f64);
            let d2 = (self.point_at(t) - p).length_squared();
            if d2 < best_d2 {
                best_d2 = d2;
                best = t;
            }
        }

        // Newton on f(t) = (C(t) − p)·C'(t), whose roots are the parameters at
        // which the curve is closest to (or furthest from) p.
        let mut t = best;
        for _ in 0..24 {
            let c = self.point_at(t);
            let d1 = self.derivative_at(t);
            let diff = c - p;
            let f = diff.dot(d1);
            // Second derivative by a central difference on the first: the
            // curves needing this path are splines, where an analytic second
            // derivative buys nothing the step size does not already limit.
            let h = (range.span().abs() * 1e-5).max(1e-9);
            let d2 = (self.derivative_at(t + h) - self.derivative_at(t - h)) * (0.5 / h);
            let fp = d1.length_squared() + diff.dot(d2);
            if fp.abs() < 1e-300 {
                break;
            }
            let step = f / fp;
            let next = (t - step).clamp(range.lo, range.hi);
            if (next - t).abs() < 1e-14 * range.span().abs().max(1.0) {
                t = next;
                break;
            }
            t = next;
        }
        Some(t)
    }

    /// Split the curve into a polyline whose chord never departs from the true
    /// curve by more than `sag`.
    ///
    /// Returns parameters, not points, so a caller that also needs tangents can
    /// evaluate once instead of twice. The recursion bisects wherever the
    /// midpoint's deviation is too large, which adapts sample density to
    /// curvature — a straight span costs two samples however long it is.
    pub fn discretise(&self, range: Interval, sag: f64, max_depth: u32) -> Vec<f64> {
        let mut out = vec![range.lo];
        subdivide(self, range.lo, range.hi, sag.max(1e-12), max_depth, &mut out);
        out.push(range.hi);
        out
    }

    /// True when `t` lies within the curve's domain, allowing for the rounding
    /// slack real files contain.
    pub fn contains_param(&self, t: f64) -> bool {
        let r = self.natural_range();
        let slack = DOMAIN_SLACK * r.span().abs().max(1.0);
        t >= r.lo - slack && t <= r.hi + slack
    }
}

/// Recover the parameter interval an edge spans from its two end points.
///
/// Exchange formats name an edge's end *vertices*, not the parameters at which
/// they sit, so the interval has to be recovered by inversion — and on a
/// periodic curve the answer is ambiguous by a full period. The rules, each
/// paid for by a bug on real files (see the STEP reader's history):
///
/// * `forward` decides which of the two arcs between the points the edge is.
/// * Coincident end points mean the whole closed curve only when the
///   *parameters* also coincide — a huge near-degenerate ellipse has short
///   arcs whose ends sit closer in space than the model tolerance.
/// * A geometrically closed but non-periodic curve (a full circle exported as
///   one spline) whose ends invert to the same parameter spans its whole
///   domain rather than collapsing to nothing.
pub fn recover_edge_range(
    curve: &Curve,
    start: crate::math::Vec3,
    end: crate::math::Vec3,
    forward: bool,
    tolerance: f64,
) -> Interval {
    let natural = curve.natural_range();
    let Some(t0) = curve.param_of(start, Some(natural.lo)) else {
        return natural;
    };
    let Some(t1) = curve.param_of(end, Some(t0)) else {
        return natural;
    };

    let vertex_tol = (tolerance * 10.0).max(1e-9);
    let coincident = (end - start).length_squared() <= vertex_tol * vertex_tol;

    match curve.period() {
        Some(period) => {
            let gap = ((t1 - t0) % period + period) % period;
            let same_parameter = gap <= period * 1e-6 || gap >= period * (1.0 - 1e-6);
            if coincident && same_parameter {
                Interval::new(t0, t0 + period)
            } else if forward {
                let mut hi = t1;
                while hi <= t0 + 1e-12 {
                    hi += period;
                }
                Interval::new(t0, hi)
            } else {
                let mut hi = t0;
                while hi <= t1 + 1e-12 {
                    hi += period;
                }
                Interval::new(t1, hi)
            }
        }
        None => {
            let span = Interval::new(t0.min(t1), t0.max(t1));
            let degenerate = span.span() <= 1e-12 * natural.span().abs().max(1.0);
            let closes = (curve.point_at(natural.hi) - curve.point_at(natural.lo))
                .length_squared()
                <= vertex_tol * vertex_tol;
            if degenerate && coincident && closes {
                natural
            } else {
                span
            }
        }
    }
}

fn subdivide(curve: &Curve, a: f64, b: f64, sag: f64, depth: u32, out: &mut Vec<f64>) {
    if depth == 0 {
        return;
    }
    let m = 0.5 * (a + b);
    let pa = curve.point_at(a);
    let pb = curve.point_at(b);
    let pm = curve.point_at(m);
    // Distance from the true midpoint to the chord. For a chord shorter than
    // the tolerance there is nothing left to resolve.
    let chord = pb - pa;
    let len = chord.length();
    let deviation = if len > 1e-300 {
        let t = ((pm - pa).dot(chord) / (len * len)).clamp(0.0, 1.0);
        (pm - (pa + chord * t)).length()
    } else {
        (pm - pa).length()
    };
    if deviation <= sag {
        return;
    }
    subdivide(curve, a, m, sag, depth - 1, out);
    out.push(m);
    subdivide(curve, m, b, sag, depth - 1, out);
}

fn nurbs_point_derivative(n: &NurbsCurve, t: f64) -> (Vec3, Vec3) {
    let points: Vec<[f64; 3]> = n.control_points.iter().map(|p| p.to_array()).collect();
    if n.weights.is_empty() {
        let (p, d) = nurbs::de_boor_with_derivative(n.degree, &points, &n.knots, t);
        (Vec3::from_slice(&p), Vec3::from_slice(&d))
    } else {
        let hom = nurbs::to_homogeneous(&points, &n.weights);
        let (p, d) = nurbs::rational_point_and_derivative(n.degree, &hom, &n.knots, t);
        (Vec3::from_slice(&p), Vec3::from_slice(&d))
    }
}

/// A polyline is parameterised by index: `t = 1.5` is halfway along its second
/// segment.
fn polyline_point(points: &[Vec3], t: f64) -> Vec3 {
    if points.is_empty() {
        return Vec3::ZERO;
    }
    if points.len() == 1 {
        return points[0];
    }
    let last = points.len() - 1;
    let t = t.clamp(0.0, last as f64);
    let i = (t.floor() as usize).min(last - 1);
    points[i].lerp(points[i + 1], t - i as f64)
}

fn polyline_derivative(points: &[Vec3], t: f64) -> Vec3 {
    if points.len() < 2 {
        return Vec3::ZERO;
    }
    let last = points.len() - 1;
    let i = (t.clamp(0.0, last as f64).floor() as usize).min(last - 1);
    points[i + 1] - points[i]
}

fn polyline_param(points: &[Vec3], p: Vec3) -> Option<f64> {
    if points.len() < 2 {
        return points.is_empty().then_some(0.0).or(Some(0.0));
    }
    let mut best = 0.0;
    let mut best_d2 = f64::INFINITY;
    for i in 0..points.len() - 1 {
        let a = points[i];
        let seg = points[i + 1] - a;
        let len2 = seg.length_squared();
        let u = if len2 > 0.0 {
            ((p - a).dot(seg) / len2).clamp(0.0, 1.0)
        } else {
            0.0
        };
        let d2 = (p - (a + seg * u)).length_squared();
        if d2 < best_d2 {
            best_d2 = d2;
            best = i as f64 + u;
        }
    }
    Some(best)
}

/// A composite curve is parameterised by segment index, like a polyline.
fn composite_at(segments: &[CompositeSegment], t: f64) -> (Vec3, Vec3) {
    if segments.is_empty() {
        return (Vec3::ZERO, Vec3::ZERO);
    }
    let last = segments.len() - 1;
    let t = t.clamp(0.0, segments.len() as f64);
    let i = (t.floor() as usize).min(last);
    let local = t - i as f64;
    let seg = &segments[i];
    // A reversed segment is walked from its high parameter to its low one, and
    // its derivative flips with it.
    let (u, sign) = if seg.same_sense {
        (seg.range.at(local), 1.0)
    } else {
        (seg.range.at(1.0 - local), -1.0)
    };
    (
        seg.curve.point_at(u),
        seg.curve.derivative_at(u) * (sign * seg.range.span()),
    )
}

/// The polar angle of `p` in the frame's XY plane, in `[0, τ)`.
fn angle_in_frame(frame: &crate::math::Frame, p: Vec3) -> f64 {
    let d = p - frame.origin;
    wrap_tau(d.dot(frame.y_dir()).atan2(d.dot(frame.ref_dir)))
}

fn wrap_tau(a: f64) -> f64 {
    let a = a % TAU;
    if a < 0.0 { a + TAU } else { a }
}

/// Shift `t` by whole periods until it lands in or nearest to `range`.
fn nearest_congruent(t: f64, range: &Interval, period: f64) -> f64 {
    if period <= 0.0 {
        return t;
    }
    let mut best = t;
    let mut best_d = f64::INFINITY;
    // A trim range never spans more than a few periods, so a small sweep either
    // side of the direct offset covers every candidate.
    let base = ((range.lo - t) / period).floor();
    for k in -1..=2 {
        let cand = t + (base + k as f64) * period;
        let d = if range.contains(cand) {
            0.0
        } else {
            (cand - range.lo).abs().min((cand - range.hi).abs())
        };
        if d < best_d {
            best_d = d;
            best = cand;
        }
    }
    best
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::math::Frame;

    fn close(a: Vec3, b: Vec3, eps: f64) -> bool {
        (a - b).length() < eps
    }

    #[test]
    fn a_line_inverts_exactly() {
        let c = Curve::Line {
            origin: Vec3::new(1.0, 2.0, 3.0),
            direction: Vec3::new(2.0, 0.0, 0.0),
        };
        let p = c.point_at(3.0);
        assert!(close(p, Vec3::new(7.0, 2.0, 3.0), 1e-15));
        assert!((c.param_of(p, None).unwrap() - 3.0).abs() < 1e-15);
    }

    #[test]
    fn a_circle_evaluates_and_inverts_around_the_full_turn() {
        let c = Curve::Circle {
            frame: Frame::new(Vec3::new(0.0, 0.0, 5.0), Vec3::Z, Vec3::X),
            radius: 2.0,
        };
        for i in 0..16 {
            let t = TAU * i as f64 / 16.0;
            let p = c.point_at(t);
            assert!(((p - Vec3::new(0.0, 0.0, 5.0)).length() - 2.0).abs() < 1e-12);
            assert!((c.param_of(p, None).unwrap() - t).abs() < 1e-12, "t={t}");
            // The tangent is perpendicular to the radius.
            let r = p - Vec3::new(0.0, 0.0, 5.0);
            assert!(c.derivative_at(t).dot(r).abs() < 1e-12);
        }
    }

    #[test]
    fn an_ellipse_inverts_through_the_semi_axis_normalisation() {
        let c = Curve::Ellipse {
            frame: Frame::new(Vec3::ZERO, Vec3::Z, Vec3::X),
            semi_major: 4.0,
            semi_minor: 1.0,
        };
        for i in 1..16 {
            let t = TAU * i as f64 / 16.0;
            let p = c.point_at(t);
            assert!((c.param_of(p, None).unwrap() - t).abs() < 1e-12, "t={t}");
        }
    }

    #[test]
    fn a_parabola_matches_its_iso_parameterisation() {
        let f = 2.0;
        let c = Curve::Parabola {
            frame: Frame::new(Vec3::ZERO, Vec3::Z, Vec3::X),
            focal_dist: f,
        };
        // At u the point is (F·u², 2F·u), so y² = 4F·x.
        for u in [-2.0, -0.5, 0.0, 1.0, 3.0] {
            let p = c.point_at(u);
            assert!((p.y * p.y - 4.0 * f * p.x).abs() < 1e-9, "u={u} p={p:?}");
            assert!((c.param_of(p, None).unwrap() - u).abs() < 1e-12);
        }
    }

    #[test]
    fn a_hyperbola_satisfies_its_own_equation() {
        let (a, b) = (3.0, 2.0);
        let c = Curve::Hyperbola {
            frame: Frame::new(Vec3::ZERO, Vec3::Z, Vec3::X),
            semi_major: a,
            semi_minor: b,
        };
        for u in [-1.5, 0.0, 0.7, 2.0] {
            let p = c.point_at(u);
            let lhs = p.x * p.x / (a * a) - p.y * p.y / (b * b);
            assert!((lhs - 1.0).abs() < 1e-9, "u={u}");
            assert!((c.param_of(p, None).unwrap() - u).abs() < 1e-12);
        }
    }

    #[test]
    fn a_polyline_is_parameterised_by_index() {
        let c = Curve::Polyline {
            points: vec![
                Vec3::ZERO,
                Vec3::new(2.0, 0.0, 0.0),
                Vec3::new(2.0, 4.0, 0.0),
            ],
        };
        assert_eq!(c.natural_range(), Interval::new(0.0, 2.0));
        assert!(close(c.point_at(0.5), Vec3::new(1.0, 0.0, 0.0), 1e-15));
        assert!(close(c.point_at(1.25), Vec3::new(2.0, 1.0, 0.0), 1e-15));
        assert!((c.param_of(Vec3::new(2.0, 1.0, 0.0), None).unwrap() - 1.25).abs() < 1e-12);
    }

    #[test]
    fn a_spline_inverts_by_search_to_working_precision() {
        let c = Curve::Nurbs(NurbsCurve {
            degree: 3,
            control_points: vec![
                Vec3::new(0.0, 0.0, 0.0),
                Vec3::new(1.0, 3.0, 0.0),
                Vec3::new(4.0, -2.0, 1.0),
                Vec3::new(6.0, 1.0, 0.0),
            ],
            weights: vec![],
            knots: vec![0.0, 0.0, 0.0, 0.0, 1.0, 1.0, 1.0, 1.0],
            closed: false,
        });
        for i in 0..=10 {
            let t = i as f64 / 10.0;
            let p = c.point_at(t);
            let got = c.param_of(p, None).unwrap();
            assert!((got - t).abs() < 1e-7, "t={t} got={got}");
        }
    }

    #[test]
    fn discretise_puts_no_samples_on_a_straight_line() {
        let c = Curve::Line {
            origin: Vec3::ZERO,
            direction: Vec3::new(1000.0, 0.0, 0.0),
        };
        let ts = c.discretise(Interval::UNIT, 0.01, 16);
        assert_eq!(ts.len(), 2, "a straight chord needs no subdivision");
    }

    #[test]
    fn discretise_respects_the_sag_tolerance_on_a_circle() {
        let c = Curve::Circle {
            frame: Frame::IDENTITY,
            radius: 10.0,
        };
        for sag in [1.0, 0.1, 0.01] {
            let ts = c.discretise(Interval::new(0.0, TAU), sag, 20);
            // Every chord's true midpoint must sit within `sag` of the chord.
            for w in ts.windows(2) {
                let (a, b) = (w[0], w[1]);
                let pa = c.point_at(a);
                let pb = c.point_at(b);
                let pm = c.point_at(0.5 * (a + b));
                let mid_chord = (pa + pb) * 0.5;
                assert!(
                    (pm - mid_chord).length() <= sag * 1.001,
                    "sag={sag} deviation too large"
                );
            }
            // Finer tolerance must not produce fewer samples.
            assert!(ts.len() >= 4, "sag={sag} produced {} samples", ts.len());
        }
        let coarse = c.discretise(Interval::new(0.0, TAU), 1.0, 20).len();
        let fine = c.discretise(Interval::new(0.0, TAU), 0.01, 20).len();
        assert!(fine > coarse);
    }

    #[test]
    fn a_trimmed_periodic_curve_inverts_into_its_own_range() {
        // An arc from 350° to 370°, i.e. across the seam.
        let base = Curve::Circle {
            frame: Frame::IDENTITY,
            radius: 1.0,
        };
        let range = Interval::new(TAU * 350.0 / 360.0, TAU * 370.0 / 360.0);
        let c = Curve::Trimmed {
            base: Box::new(base),
            range,
        };
        // 5°, which the base reports as 0.0873 rad — one full turn below range.
        let p = c.point_at(TAU * 365.0 / 360.0);
        let t = c.param_of(p, None).unwrap();
        assert!(range.contains(t), "t={t} fell outside {range:?}");
    }

    #[test]
    fn closedness_reflects_the_geometry() {
        assert!(
            Curve::Circle {
                frame: Frame::IDENTITY,
                radius: 1.0
            }
            .is_closed()
        );
        assert!(
            !Curve::Line {
                origin: Vec3::ZERO,
                direction: Vec3::X
            }
            .is_closed()
        );
        assert!(
            Curve::Polyline {
                points: vec![Vec3::ZERO, Vec3::X, Vec3::Y, Vec3::ZERO]
            }
            .is_closed()
        );
    }

    #[test]
    fn recover_edge_range_handles_arcs_seams_and_full_circles() {
        use crate::eval::curve::recover_edge_range;
        let circle = Curve::Circle {
            frame: Frame::IDENTITY,
            radius: 10.0,
        };
        let p = |t: f64| circle.point_at(t);

        // A quarter arc, forward: 0.5 → 2.0.
        let r = recover_edge_range(&circle, p(0.5), p(2.0), true, 1e-6);
        assert!((r.lo - 0.5).abs() < 1e-9 && (r.hi - 2.0).abs() < 1e-9);

        // The same points traversed the other way is the complementary arc.
        let r = recover_edge_range(&circle, p(0.5), p(2.0), false, 1e-6);
        assert!((r.lo - 2.0).abs() < 1e-9);
        assert!((r.hi - (0.5 + TAU)).abs() < 1e-9, "{r:?}");

        // Coincident ends at the same parameter: the whole circle.
        let r = recover_edge_range(&circle, p(1.0), p(1.0), true, 1e-6);
        assert!((r.span() - TAU).abs() < 1e-9);

        // An arc crossing the seam.
        let r = recover_edge_range(&circle, p(6.0), p(0.5), true, 1e-6);
        assert!((r.lo - 6.0).abs() < 1e-9);
        assert!((r.hi - (0.5 + TAU)).abs() < 1e-9);
    }

    #[test]
    fn a_composite_curve_walks_its_segments_including_reversed_ones() {
        let seg = |a: Vec3, b: Vec3, same_sense: bool| CompositeSegment {
            curve: Curve::Line {
                origin: a,
                direction: b - a,
            },
            range: Interval::UNIT,
            same_sense,
        };
        let c = Curve::Composite {
            segments: vec![
                seg(Vec3::ZERO, Vec3::new(1.0, 0.0, 0.0), true),
                // Written from (1,1,0) back to (1,0,0), traversed reversed.
                seg(Vec3::new(1.0, 1.0, 0.0), Vec3::new(1.0, 0.0, 0.0), false),
            ],
        };
        assert_eq!(c.natural_range(), Interval::new(0.0, 2.0));
        assert!(close(c.point_at(0.0), Vec3::ZERO, 1e-15));
        assert!(close(c.point_at(1.0), Vec3::new(1.0, 0.0, 0.0), 1e-15));
        assert!(close(c.point_at(2.0), Vec3::new(1.0, 1.0, 0.0), 1e-15));
        // The second segment advances in +y despite being written in -y.
        assert!(c.derivative_at(1.5).y > 0.0);
    }
}
