//! B-spline and NURBS evaluation.
//!
//! One generic de Boor implementation over an abstract "point" so the 3D
//! curve, the 2D parameter-space curve and the surface's two directions all
//! share it. A second implementation per case is a second place for an
//! off-by-one in the knot span to hide.
//!
//! Rational curves are evaluated in homogeneous space — weight the control
//! points, run the same algorithm, divide at the end — which is the whole
//! reason NURBS are defined the way they are, and is exact rather than an
//! approximation of the rational form.

/// A point that de Boor can blend.
///
/// Implemented for `[f64; N]`, which covers 2D, 3D, and the 4D homogeneous
/// forms of both.
pub trait Blend: Copy {
    fn zero() -> Self;
    fn scale(self, s: f64) -> Self;
    fn add(self, other: Self) -> Self;
    fn sub(self, other: Self) -> Self;
}

impl<const N: usize> Blend for [f64; N] {
    fn zero() -> Self {
        [0.0; N]
    }
    fn scale(mut self, s: f64) -> Self {
        for v in &mut self {
            *v *= s;
        }
        self
    }
    fn add(mut self, other: Self) -> Self {
        for (a, b) in self.iter_mut().zip(other) {
            *a += b;
        }
        self
    }
    fn sub(mut self, other: Self) -> Self {
        for (a, b) in self.iter_mut().zip(other) {
            *a -= b;
        }
        self
    }
}

/// The valid parameter range of a knot vector of the given degree.
///
/// A knot vector has `degree` extra knots clamped onto each end; the curve only
/// exists between them.
pub fn domain(knots: &[f64], degree: usize) -> Option<(f64, f64)> {
    if knots.len() < 2 * (degree + 1) {
        // Too short to describe even a single span. A malformed file can do
        // this, and reading past the end would be worse than declining.
        return None;
    }
    let lo = knots[degree];
    let hi = knots[knots.len() - 1 - degree];
    if hi > lo { Some((lo, hi)) } else { None }
}

/// Index of the knot span containing `t`.
///
/// Returns the `i` with `knots[i] <= t < knots[i+1]`, clamped into the valid
/// range so the last point of the curve evaluates on the last span rather than
/// falling off the end.
pub fn find_span(knots: &[f64], degree: usize, n_control: usize, t: f64) -> usize {
    // The last span index is `n_control - 1`; anything at or past the upper
    // domain bound belongs to it.
    let high = n_control - 1;
    if t >= knots[high] {
        return high;
    }
    let low = degree;
    if t <= knots[low] {
        return low;
    }
    // Binary search, the standard NURBS-book span search.
    let (mut lo, mut hi) = (low, high);
    let mut mid = (lo + hi) / 2;
    while t < knots[mid] || t >= knots[mid + 1] {
        if t < knots[mid] {
            hi = mid;
        } else {
            lo = mid;
        }
        let next = (lo + hi) / 2;
        if next == mid {
            break;
        }
        mid = next;
    }
    mid
}

/// Evaluate a B-spline at `t` by de Boor's algorithm.
///
/// `control` must hold at least `knots.len() - degree - 1` points.
pub fn de_boor<P: Blend>(degree: usize, control: &[P], knots: &[f64], t: f64) -> P {
    if control.is_empty() {
        return P::zero();
    }
    if degree == 0 {
        let span = find_span(knots, 0, control.len(), t);
        return control[span.min(control.len() - 1)];
    }

    let span = find_span(knots, degree, control.len(), t);
    // The `degree + 1` control points influencing this span.
    let mut d: Vec<P> = (0..=degree)
        .map(|j| {
            let idx = (span + j).saturating_sub(degree);
            control[idx.min(control.len() - 1)]
        })
        .collect();

    for r in 1..=degree {
        for j in (r..=degree).rev() {
            let i = span + j - degree;
            let lo = knots.get(i).copied().unwrap_or(0.0);
            let hi = knots.get(i + degree + 1 - r).copied().unwrap_or(lo);
            let denom = hi - lo;
            // A zero span means a repeated knot; the blend degenerates to the
            // later point, which is exactly what a multiplicity means.
            let alpha = if denom.abs() > f64::EPSILON {
                (t - lo) / denom
            } else {
                0.0
            };
            d[j] = d[j - 1].scale(1.0 - alpha).add(d[j].scale(alpha));
        }
    }
    d[degree]
}

/// Evaluate a B-spline and its first derivative at `t`.
///
/// The derivative of a degree-`p` B-spline is a degree-`p-1` B-spline over the
/// same knots with control points `p·(P[i+1] − P[i]) / (u[i+p+1] − u[i+1])`.
/// Building it explicitly is exact, where a finite difference would trade
/// accuracy for a step size nobody can choose well across six orders of
/// magnitude of model scale.
pub fn de_boor_with_derivative<P: Blend>(
    degree: usize,
    control: &[P],
    knots: &[f64],
    t: f64,
) -> (P, P) {
    let value = de_boor(degree, control, knots, t);
    if degree == 0 || control.len() < 2 {
        return (value, P::zero());
    }

    let mut dctl = Vec::with_capacity(control.len() - 1);
    for i in 0..control.len() - 1 {
        let lo = knots.get(i + 1).copied().unwrap_or(0.0);
        let hi = knots.get(i + degree + 1).copied().unwrap_or(lo);
        let denom = hi - lo;
        dctl.push(if denom.abs() > f64::EPSILON {
            control[i + 1].sub(control[i]).scale(degree as f64 / denom)
        } else {
            P::zero()
        });
    }
    // The derivative spline drops one knot from each end.
    let dknots = &knots[1..knots.len().saturating_sub(1)];
    let derivative = de_boor(degree - 1, &dctl, dknots, t);
    (value, derivative)
}

/// Evaluate a rational curve given homogeneous control points `[x·w, y·w, z·w, w]`.
///
/// Returns the Euclidean point and its derivative, applying the quotient rule
/// to the homogeneous derivative.
pub fn rational_point_and_derivative(
    degree: usize,
    homogeneous: &[[f64; 4]],
    knots: &[f64],
    t: f64,
) -> ([f64; 3], [f64; 3]) {
    let (c, dc) = de_boor_with_derivative(degree, homogeneous, knots, t);
    let w = c[3];
    if w.abs() < 1e-300 {
        return ([c[0], c[1], c[2]], [0.0; 3]);
    }
    let inv = 1.0 / w;
    let p = [c[0] * inv, c[1] * inv, c[2] * inv];
    // Quotient rule: d/dt (A/w) = (A' - (A/w)·w') / w
    let d = [
        (dc[0] - p[0] * dc[3]) * inv,
        (dc[1] - p[1] * dc[3]) * inv,
        (dc[2] - p[2] * dc[3]) * inv,
    ];
    (p, d)
}

/// Build homogeneous control points from Euclidean points and weights.
///
/// An empty weight list means non-rational, so every weight is 1.
pub fn to_homogeneous(points: &[[f64; 3]], weights: &[f64]) -> Vec<[f64; 4]> {
    points
        .iter()
        .enumerate()
        .map(|(i, p)| {
            let w = weights.get(i).copied().unwrap_or(1.0);
            [p[0] * w, p[1] * w, p[2] * w, w]
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A clamped cubic Bézier: four control points, knots 0,0,0,0,1,1,1,1.
    fn bezier() -> (usize, Vec<[f64; 3]>, Vec<f64>) {
        (
            3,
            vec![
                [0.0, 0.0, 0.0],
                [0.0, 1.0, 0.0],
                [1.0, 1.0, 0.0],
                [1.0, 0.0, 0.0],
            ],
            vec![0.0, 0.0, 0.0, 0.0, 1.0, 1.0, 1.0, 1.0],
        )
    }

    fn close(a: [f64; 3], b: [f64; 3], eps: f64) -> bool {
        a.iter().zip(b).all(|(x, y)| (x - y).abs() < eps)
    }

    #[test]
    fn a_clamped_spline_interpolates_its_end_points() {
        let (d, c, k) = bezier();
        assert!(close(de_boor(d, &c, &k, 0.0), c[0], 1e-12));
        assert!(close(de_boor(d, &c, &k, 1.0), c[3], 1e-12));
    }

    #[test]
    fn de_boor_matches_the_bernstein_form() {
        let (d, c, k) = bezier();
        for i in 0..=20 {
            let t = i as f64 / 20.0;
            let (mt, b) = (1.0 - t, t);
            let mut want = [0.0f64; 3];
            let coeff = [mt * mt * mt, 3.0 * mt * mt * b, 3.0 * mt * b * b, b * b * b];
            for (j, cp) in c.iter().enumerate() {
                for axis in 0..3 {
                    want[axis] += coeff[j] * cp[axis];
                }
            }
            assert!(
                close(de_boor(d, &c, &k, t), want, 1e-12),
                "t={t} got {:?} want {want:?}",
                de_boor(d, &c, &k, t)
            );
        }
    }

    #[test]
    fn the_derivative_matches_a_central_difference() {
        let (d, c, k) = bezier();
        for i in 1..20 {
            let t = i as f64 / 20.0;
            let h = 1e-6;
            let a = de_boor(d, &c, &k, t - h);
            let b = de_boor(d, &c, &k, t + h);
            let fd = [
                (b[0] - a[0]) / (2.0 * h),
                (b[1] - a[1]) / (2.0 * h),
                (b[2] - a[2]) / (2.0 * h),
            ];
            let (_, an) = de_boor_with_derivative(d, &c, &k, t);
            assert!(close(an, fd, 1e-6), "t={t} analytic {an:?} vs fd {fd:?}");
        }
    }

    #[test]
    fn a_rational_quarter_circle_stays_on_the_circle() {
        // The standard weight-1/sqrt(2) quadratic representation of a 90° arc.
        let w = std::f64::consts::FRAC_1_SQRT_2;
        let pts = [[1.0, 0.0, 0.0], [1.0, 1.0, 0.0], [0.0, 1.0, 0.0]];
        let hom = to_homogeneous(&pts, &[1.0, w, 1.0]);
        let knots = [0.0, 0.0, 0.0, 1.0, 1.0, 1.0];
        for i in 0..=16 {
            let t = i as f64 / 16.0;
            let (p, _) = rational_point_and_derivative(2, &hom, &knots, t);
            let r = (p[0] * p[0] + p[1] * p[1]).sqrt();
            assert!((r - 1.0).abs() < 1e-12, "t={t} r={r}");
        }
    }

    #[test]
    fn a_rational_derivative_is_tangent_to_the_circle() {
        let w = std::f64::consts::FRAC_1_SQRT_2;
        let pts = [[1.0, 0.0, 0.0], [1.0, 1.0, 0.0], [0.0, 1.0, 0.0]];
        let hom = to_homogeneous(&pts, &[1.0, w, 1.0]);
        let knots = [0.0, 0.0, 0.0, 1.0, 1.0, 1.0];
        for i in 1..16 {
            let t = i as f64 / 16.0;
            let (p, d) = rational_point_and_derivative(2, &hom, &knots, t);
            // On a circle the radius and the tangent are perpendicular.
            let dot = p[0] * d[0] + p[1] * d[1];
            assert!(dot.abs() < 1e-9, "t={t} dot={dot}");
        }
    }

    #[test]
    fn span_search_lands_in_the_right_interval() {
        let knots = [0.0, 0.0, 0.0, 1.0, 2.0, 3.0, 3.0, 3.0];
        let n = knots.len() - 2 - 1; // degree 2
        for (t, want) in [(0.0, 2), (0.5, 2), (1.0, 3), (1.5, 3), (2.5, 4), (3.0, 4)] {
            assert_eq!(find_span(&knots, 2, n, t), want, "t={t}");
        }
    }

    #[test]
    fn domain_is_the_clamped_interior() {
        assert_eq!(domain(&[0.0, 0.0, 0.0, 1.0, 2.0, 2.0, 2.0], 2), Some((0.0, 2.0)));
        // Too few knots to describe a span.
        assert_eq!(domain(&[0.0, 1.0], 2), None);
        // A degenerate range is not a domain.
        assert_eq!(domain(&[0.0, 0.0, 0.0, 0.0], 1), None);
    }

    #[test]
    fn a_multiple_interior_knot_still_evaluates() {
        // A cubic with a triple interior knot is C0 there; a naive zero-span
        // divide would produce NaN.
        let degree = 3;
        let knots = [0., 0., 0., 0., 1., 1., 1., 2., 2., 2., 2.];
        let control: Vec<[f64; 3]> = (0..7).map(|i| [i as f64, 0.0, 0.0]).collect();
        for i in 0..=20 {
            let t = 2.0 * i as f64 / 20.0;
            let p = de_boor(degree, &control, &knots, t);
            assert!(p[0].is_finite(), "t={t} produced {p:?}");
        }
    }

    #[test]
    fn a_degree_one_spline_is_the_polyline_through_its_points() {
        let control = [[0.0, 0.0, 0.0], [2.0, 0.0, 0.0], [2.0, 3.0, 0.0]];
        let knots = [0.0, 0.0, 1.0, 2.0, 2.0];
        assert!(close(de_boor(1, &control, &knots, 0.5), [1.0, 0.0, 0.0], 1e-12));
        assert!(close(de_boor(1, &control, &knots, 1.5), [2.0, 1.5, 0.0], 1e-12));
    }
}
