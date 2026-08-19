//! Evaluating and inverting the IR's surfaces.
//!
//! Parameterisations follow ISO 10303-42, which Parasolid matches for the
//! analytic forms.
//!
//! Normals are analytic wherever a closed form exists, rather than
//! `∂u × ∂v` everywhere. The cross product vanishes exactly where a surface is
//! most likely to be tessellated badly — a sphere's poles, a cone's apex, a
//! torus's degenerate ring — and a zero-length normal there turns into either a
//! black triangle or a NaN. The analytic form is defined at those points.

use crate::brep::{Curve, NurbsSurface, Surface};
use crate::eval::nurbs;
use crate::math::{Frame, Interval, TAU, Vec2, Vec3};

/// The parameter domain of a surface, and whether each direction wraps.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Domain {
    pub u: Interval,
    pub v: Interval,
    /// The `u` period, when advancing by it returns the same point.
    pub u_period: Option<f64>,
    pub v_period: Option<f64>,
}

/// The bound used where a parameter direction is mathematically unbounded.
///
/// A cylinder extends forever along its axis; only its trim loops give it a
/// height. Sampling needs *some* interval, and this one is far past any model
/// while staying well inside `f64`'s exact-integer range.
const UNBOUNDED: f64 = 1e12;

impl Surface {
    /// The surface's natural parameter domain.
    pub fn domain(&self) -> Domain {
        let full_turn = Interval::new(0.0, TAU);
        let unbounded = Interval::new(-UNBOUNDED, UNBOUNDED);
        match self {
            Surface::Plane { .. } => Domain {
                u: unbounded,
                v: unbounded,
                u_period: None,
                v_period: None,
            },
            Surface::Cylinder { .. } | Surface::Cone { .. } => Domain {
                u: full_turn,
                v: unbounded,
                u_period: Some(TAU),
                v_period: None,
            },
            Surface::Sphere { .. } => Domain {
                u: full_turn,
                v: Interval::new(-std::f64::consts::FRAC_PI_2, std::f64::consts::FRAC_PI_2),
                u_period: Some(TAU),
                v_period: None,
            },
            Surface::Torus { .. } => Domain {
                u: full_turn,
                v: full_turn,
                u_period: Some(TAU),
                v_period: Some(TAU),
            },
            Surface::Nurbs(n) => {
                let u = nurbs::domain(&n.u_knots, n.u_degree)
                    .map(|(a, b)| Interval::new(a, b))
                    .unwrap_or(Interval::UNIT);
                let v = nurbs::domain(&n.v_knots, n.v_degree)
                    .map(|(a, b)| Interval::new(a, b))
                    .unwrap_or(Interval::UNIT);
                Domain {
                    u,
                    v,
                    u_period: n.u_closed.then(|| u.span()),
                    v_period: n.v_closed.then(|| v.span()),
                }
            }
            Surface::LinearExtrusion { profile, .. } => Domain {
                u: profile.natural_range(),
                v: unbounded,
                u_period: profile.period(),
                v_period: None,
            },
            Surface::Revolution { profile, .. } => Domain {
                u: full_turn,
                v: profile.natural_range(),
                u_period: Some(TAU),
                v_period: profile.period(),
            },
            Surface::Offset { base, .. } => base.domain(),
            Surface::RectangularTrimmed { base, u, v } => {
                let d = base.domain();
                Domain {
                    u: *u,
                    v: *v,
                    // Restricting a periodic direction to less than a full
                    // period makes it non-periodic; a seam only exists if the
                    // trim still spans the whole turn.
                    u_period: d.u_period.filter(|p| (u.span() - p).abs() < 1e-9),
                    v_period: d.v_period.filter(|p| (v.span() - p).abs() < 1e-9),
                }
            }
        }
    }

    /// The point at `(u, v)`.
    pub fn point_at(&self, uv: Vec2) -> Vec3 {
        let (u, v) = (uv.u, uv.v);
        match self {
            Surface::Plane { frame } => frame.origin + frame.ref_dir * u + frame.y_dir() * v,
            Surface::Cylinder { frame, radius } => frame.polar(*radius, u) + frame.axis * v,
            Surface::Cone {
                frame,
                radius,
                half_angle,
            } => {
                let r = radius + v * half_angle.tan();
                frame.polar(r, u) + frame.axis * v
            }
            Surface::Sphere { frame, radius } => {
                frame.polar(radius * v.cos(), u) + frame.axis * (radius * v.sin())
            }
            Surface::Torus {
                frame,
                major_radius,
                minor_radius,
            } => {
                let r = major_radius + minor_radius * v.cos();
                frame.polar(r, u) + frame.axis * (minor_radius * v.sin())
            }
            Surface::Nurbs(n) => nurbs_surface_point(n, u, v),
            Surface::LinearExtrusion { profile, direction } => {
                profile.point_at(u) + *direction * v
            }
            Surface::Revolution { profile, frame } => {
                rotate_about(profile.point_at(v), frame, u)
            }
            Surface::Offset { base, distance } => {
                base.point_at(uv) + base.normal_at(uv) * *distance
            }
            Surface::RectangularTrimmed { base, .. } => base.point_at(uv),
        }
    }

    /// The partial derivatives `(∂P/∂u, ∂P/∂v)` at `(u, v)`.
    pub fn derivatives_at(&self, uv: Vec2) -> (Vec3, Vec3) {
        let (u, v) = (uv.u, uv.v);
        match self {
            Surface::Plane { frame } => (frame.ref_dir, frame.y_dir()),
            Surface::Cylinder { frame, radius } => (
                frame.ref_dir * (-radius * u.sin()) + frame.y_dir() * (radius * u.cos()),
                frame.axis,
            ),
            Surface::Cone {
                frame,
                radius,
                half_angle,
            } => {
                let tan = half_angle.tan();
                let r = radius + v * tan;
                let e_r = frame.ref_dir * u.cos() + frame.y_dir() * u.sin();
                let e_t = frame.ref_dir * -u.sin() + frame.y_dir() * u.cos();
                (e_t * r, e_r * tan + frame.axis)
            }
            Surface::Sphere { frame, radius } => {
                let e_r = frame.ref_dir * u.cos() + frame.y_dir() * u.sin();
                let e_t = frame.ref_dir * -u.sin() + frame.y_dir() * u.cos();
                (
                    e_t * (radius * v.cos()),
                    e_r * (-radius * v.sin()) + frame.axis * (radius * v.cos()),
                )
            }
            Surface::Torus {
                frame,
                major_radius,
                minor_radius,
            } => {
                let e_r = frame.ref_dir * u.cos() + frame.y_dir() * u.sin();
                let e_t = frame.ref_dir * -u.sin() + frame.y_dir() * u.cos();
                let r = major_radius + minor_radius * v.cos();
                (
                    e_t * r,
                    e_r * (-minor_radius * v.sin()) + frame.axis * (minor_radius * v.cos()),
                )
            }
            Surface::Nurbs(n) => nurbs_surface_derivatives(n, u, v),
            Surface::LinearExtrusion { profile, direction } => {
                (profile.derivative_at(u), *direction)
            }
            Surface::Revolution { profile, frame } => {
                let p = profile.point_at(v);
                let rotated = rotate_about(p, frame, u);
                // Rotating about the axis: the u-derivative is ω × r, with ω
                // the unit axis and r the offset from the axis line.
                let radial = rotated - frame.origin;
                let along = frame.axis * radial.dot(frame.axis);
                let du = frame.axis.cross(radial - along);
                let dv = rotate_direction(profile.derivative_at(v), frame, u);
                (du, dv)
            }
            // An offset surface's exact derivative needs the base's second
            // derivatives. A central difference on the offset point is used
            // instead, sized to the surface's own parameter scale.
            Surface::Offset { base, .. } => {
                let du_h = step_for(base.domain().u);
                let dv_h = step_for(base.domain().v);
                let du = (self.point_at(Vec2::new(u + du_h, v))
                    - self.point_at(Vec2::new(u - du_h, v)))
                    * (0.5 / du_h);
                let dv = (self.point_at(Vec2::new(u, v + dv_h))
                    - self.point_at(Vec2::new(u, v - dv_h)))
                    * (0.5 / dv_h);
                (du, dv)
            }
            Surface::RectangularTrimmed { base, .. } => base.derivatives_at(uv),
        }
    }

    /// The unit normal at `(u, v)`, following the surface's own orientation.
    ///
    /// A face may reverse it; that is [`crate::brep::Face::same_sense`]'s job,
    /// not this one's.
    pub fn normal_at(&self, uv: Vec2) -> Vec3 {
        let (u, v) = (uv.u, uv.v);
        match self {
            Surface::Plane { frame } => frame.axis,
            Surface::Cylinder { frame, radius } => {
                let e_r = frame.ref_dir * u.cos() + frame.y_dir() * u.sin();
                // A negative radius flips which side is outside.
                if *radius < 0.0 { -e_r } else { e_r }
            }
            Surface::Cone {
                frame, half_angle, ..
            } => {
                // Defined at the apex, where ∂u × ∂v is zero.
                let e_r = frame.ref_dir * u.cos() + frame.y_dir() * u.sin();
                (e_r * half_angle.cos() - frame.axis * half_angle.sin())
                    .normalized_or(frame.axis)
            }
            Surface::Sphere { frame, radius } => {
                // Defined at both poles, where ∂u vanishes.
                let n = frame.polar(v.cos(), u) - frame.origin + frame.axis * v.sin();
                let n = n.normalized_or(frame.axis);
                if *radius < 0.0 { -n } else { n }
            }
            Surface::Torus {
                frame,
                minor_radius,
                ..
            } => {
                let e_r = frame.ref_dir * u.cos() + frame.y_dir() * u.sin();
                let n = e_r * v.cos() + frame.axis * v.sin();
                if *minor_radius < 0.0 { -n } else { n }
            }
            // Displacing along the normal does not turn it. The exception is
            // an offset larger than the base's local radius of curvature,
            // where the surface folds through itself; that is a defect in the
            // source model, not something to compensate for here.
            Surface::Offset { base, .. } => base.normal_at(uv),
            Surface::RectangularTrimmed { base, .. } => base.normal_at(uv),
            _ => {
                let (du, dv) = self.derivatives_at(uv);
                match du.cross(dv).try_normalized() {
                    Some(n) => n,
                    // A degenerate parameter line — the pole of a spline patch
                    // whose whole row of control points coincides. Step inside
                    // the domain and use the neighbouring normal, which is the
                    // limit the degenerate point approaches.
                    None => self.normal_near(uv),
                }
            }
        }
    }

    /// The normal just inside the domain from `uv`, for degenerate points.
    fn normal_near(&self, uv: Vec2) -> Vec3 {
        let d = self.domain();
        let hu = step_for(d.u);
        let hv = step_for(d.v);
        for (du, dv) in [(hu, 0.0), (-hu, 0.0), (0.0, hv), (0.0, -hv), (hu, hv)] {
            let p = Vec2::new(
                (uv.u + du).clamp(d.u.lo, d.u.hi),
                (uv.v + dv).clamp(d.v.lo, d.v.hi),
            );
            let (a, b) = self.derivatives_at(p);
            if let Some(n) = a.cross(b).try_normalized() {
                return n;
            }
        }
        Vec3::Z
    }

    /// The parameters at which the surface passes through `p`.
    ///
    /// `hint` seeds the spline search and disambiguates a periodic direction.
    pub fn invert(&self, p: Vec3, hint: Option<Vec2>) -> Option<Vec2> {
        match self {
            Surface::Plane { frame } => {
                let d = p - frame.origin;
                Some(Vec2::new(d.dot(frame.ref_dir), d.dot(frame.y_dir())))
            }
            Surface::Cylinder { frame, .. } => {
                let d = p - frame.origin;
                Some(Vec2::new(angle_in(frame, d), d.dot(frame.axis)))
            }
            Surface::Cone { frame, .. } => {
                let d = p - frame.origin;
                Some(Vec2::new(angle_in(frame, d), d.dot(frame.axis)))
            }
            Surface::Sphere { frame, radius } => {
                let d = p - frame.origin;
                if radius.abs() < 1e-300 {
                    return None;
                }
                let z = (d.dot(frame.axis) / radius).clamp(-1.0, 1.0);
                Some(Vec2::new(angle_in(frame, d), z.asin()))
            }
            Surface::Torus {
                frame,
                major_radius,
                ..
            } => {
                let d = p - frame.origin;
                let u = angle_in(frame, d);
                let z = d.dot(frame.axis);
                let radial = (d - frame.axis * z).length() - major_radius;
                Some(Vec2::new(u, wrap_tau(z.atan2(radial))))
            }
            Surface::LinearExtrusion { profile, direction } => {
                let d2 = direction.length_squared();
                if d2 < 1e-300 {
                    return None;
                }
                // The extrusion parameter is the displacement along the axis;
                // removing it lands the point back on the profile curve.
                let v = (p - profile.point_at(profile.natural_range().lo)).dot(*direction) / d2;
                let on_profile = p - *direction * v;
                let u = profile.param_of(on_profile, hint.map(|h| h.u))?;
                Some(Vec2::new(u, v))
            }
            Surface::Revolution { profile, frame } => {
                let d = p - frame.origin;
                let u = angle_in(frame, d);
                // Rotating the point back onto the profile's own plane.
                let unrotated = rotate_about(p, frame, -u);
                let v = profile.param_of(unrotated, hint.map(|h| h.v))?;
                Some(Vec2::new(u, v))
            }
            Surface::RectangularTrimmed { base, .. } => base.invert(p, hint),
            Surface::Offset { base, .. } => {
                // Close enough to seed, since the offset is along the normal
                // and so does not move the parameters much.
                base.invert(p, hint)
            }
            Surface::Nurbs(_) => self.invert_by_search(p, hint),
        }
    }

    /// Grid search then Newton, for surfaces with no closed-form inverse.
    fn invert_by_search(&self, p: Vec3, hint: Option<Vec2>) -> Option<Vec2> {
        let d = self.domain();
        const N: usize = 12;

        let mut best = hint.unwrap_or(Vec2::new(d.u.at(0.5), d.v.at(0.5)));
        let mut best_d2 = (self.point_at(best) - p).length_squared();
        for i in 0..=N {
            for j in 0..=N {
                let uv = Vec2::new(d.u.at(i as f64 / N as f64), d.v.at(j as f64 / N as f64));
                let dist = (self.point_at(uv) - p).length_squared();
                if dist < best_d2 {
                    best_d2 = dist;
                    best = uv;
                }
            }
        }

        // Newton on the 2×2 system ∇(½‖S(u,v) − p‖²) = 0. The Gauss-Newton
        // approximation drops the second-derivative term, which costs a little
        // convergence rate near a highly curved patch and buys not needing
        // second derivatives at all.
        let mut uv = best;
        for _ in 0..32 {
            let diff = self.point_at(uv) - p;
            let (du, dv) = self.derivatives_at(uv);
            let (a, b, c) = (du.dot(du), du.dot(dv), dv.dot(dv));
            let (e, f) = (diff.dot(du), diff.dot(dv));
            let det = a * c - b * b;
            if det.abs() < 1e-300 {
                break;
            }
            let step_u = (e * c - f * b) / det;
            let step_v = (a * f - b * e) / det;
            let next = Vec2::new(
                (uv.u - step_u).clamp(d.u.lo, d.u.hi),
                (uv.v - step_v).clamp(d.v.lo, d.v.hi),
            );
            let moved = next - uv;
            uv = next;
            if moved.u.abs() < 1e-13 && moved.v.abs() < 1e-13 {
                break;
            }
        }
        Some(uv)
    }
}

/// A step size a hundred-thousandth of a parameter direction's span.
///
/// Sized to the domain rather than fixed, because a spline patch may run 0..1
/// while a cylinder's axis runs in millimetres.
fn step_for(range: Interval) -> f64 {
    let span = range.span().abs().min(UNBOUNDED);
    (span * 1e-5).max(1e-9)
}

fn nurbs_surface_point(n: &NurbsSurface, u: f64, v: f64) -> Vec3 {
    // Evaluate along v for each u row, then along u through the results — the
    // standard tensor-product evaluation.
    let rational = !n.weights.is_empty();
    let rows: Vec<[f64; 4]> = n
        .control_points
        .iter()
        .enumerate()
        .map(|(i, row)| {
            let hom: Vec<[f64; 4]> = row
                .iter()
                .enumerate()
                .map(|(j, p)| {
                    let w = if rational {
                        n.weights.get(i).and_then(|r| r.get(j)).copied().unwrap_or(1.0)
                    } else {
                        1.0
                    };
                    [p.x * w, p.y * w, p.z * w, w]
                })
                .collect();
            nurbs::de_boor(n.v_degree, &hom, &n.v_knots, v)
        })
        .collect();
    let c = nurbs::de_boor(n.u_degree, &rows, &n.u_knots, u);
    if c[3].abs() < 1e-300 {
        Vec3::new(c[0], c[1], c[2])
    } else {
        Vec3::new(c[0] / c[3], c[1] / c[3], c[2] / c[3])
    }
}

fn nurbs_surface_derivatives(n: &NurbsSurface, u: f64, v: f64) -> (Vec3, Vec3) {
    let rational = !n.weights.is_empty();
    let hom_rows: Vec<Vec<[f64; 4]>> = n
        .control_points
        .iter()
        .enumerate()
        .map(|(i, row)| {
            row.iter()
                .enumerate()
                .map(|(j, p)| {
                    let w = if rational {
                        n.weights.get(i).and_then(|r| r.get(j)).copied().unwrap_or(1.0)
                    } else {
                        1.0
                    };
                    [p.x * w, p.y * w, p.z * w, w]
                })
                .collect()
        })
        .collect();

    // Along v: value and derivative of each u row.
    let mut row_val = Vec::with_capacity(hom_rows.len());
    let mut row_dv = Vec::with_capacity(hom_rows.len());
    for row in &hom_rows {
        let (val, d) = nurbs::de_boor_with_derivative(n.v_degree, row, &n.v_knots, v);
        row_val.push(val);
        row_dv.push(d);
    }

    let (s, s_u) = nurbs::de_boor_with_derivative(n.u_degree, &row_val, &n.u_knots, u);
    let s_v = nurbs::de_boor(n.u_degree, &row_dv, &n.u_knots, u);

    // Divide out the weight, applying the quotient rule to each direction.
    let w = s[3];
    if w.abs() < 1e-300 {
        return (
            Vec3::new(s_u[0], s_u[1], s_u[2]),
            Vec3::new(s_v[0], s_v[1], s_v[2]),
        );
    }
    let inv = 1.0 / w;
    let p = Vec3::new(s[0] * inv, s[1] * inv, s[2] * inv);
    let du = Vec3::new(
        (s_u[0] - p.x * s_u[3]) * inv,
        (s_u[1] - p.y * s_u[3]) * inv,
        (s_u[2] - p.z * s_u[3]) * inv,
    );
    let dv = Vec3::new(
        (s_v[0] - p.x * s_v[3]) * inv,
        (s_v[1] - p.y * s_v[3]) * inv,
        (s_v[2] - p.z * s_v[3]) * inv,
    );
    (du, dv)
}

/// Rotate `p` about the frame's axis line by `angle`, using Rodrigues.
fn rotate_about(p: Vec3, frame: &Frame, angle: f64) -> Vec3 {
    let k = frame.axis;
    let r = p - frame.origin;
    let (s, c) = angle.sin_cos();
    frame.origin + r * c + k.cross(r) * s + k * (k.dot(r) * (1.0 - c))
}

/// Rotate a direction about the frame's axis, ignoring the origin.
fn rotate_direction(d: Vec3, frame: &Frame, angle: f64) -> Vec3 {
    let k = frame.axis;
    let (s, c) = angle.sin_cos();
    d * c + k.cross(d) * s + k * (k.dot(d) * (1.0 - c))
}

fn angle_in(frame: &Frame, d: Vec3) -> f64 {
    wrap_tau(d.dot(frame.y_dir()).atan2(d.dot(frame.ref_dir)))
}

fn wrap_tau(a: f64) -> f64 {
    let a = a % TAU;
    if a < 0.0 { a + TAU } else { a }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn close(a: Vec3, b: Vec3, eps: f64) -> bool {
        (a - b).length() < eps
    }

    /// Every surface's analytic normal must agree with ∂u × ∂v wherever that
    /// cross product is non-degenerate. This is the check that catches a
    /// transposed axis in either formula.
    fn normal_agrees_with_cross(s: &Surface, uv: Vec2) {
        let (du, dv) = s.derivatives_at(uv);
        let Some(cross) = du.cross(dv).try_normalized() else {
            return;
        };
        let n = s.normal_at(uv);
        assert!(
            (n - cross).length() < 1e-7,
            "at {uv:?}: analytic {n:?} vs cross {cross:?}"
        );
    }

    #[test]
    fn a_plane_is_its_own_frame() {
        let s = Surface::Plane {
            frame: Frame::new(Vec3::new(1.0, 2.0, 3.0), Vec3::Z, Vec3::X),
        };
        assert!(close(
            s.point_at(Vec2::new(2.0, 5.0)),
            Vec3::new(3.0, 7.0, 3.0),
            1e-15
        ));
        assert_eq!(s.normal_at(Vec2::new(0.0, 0.0)), Vec3::Z);
        let uv = s.invert(Vec3::new(3.0, 7.0, 3.0), None).unwrap();
        assert!((uv.u - 2.0).abs() < 1e-12 && (uv.v - 5.0).abs() < 1e-12);
    }

    #[test]
    fn a_cylinder_evaluates_inverts_and_normals_consistently() {
        let s = Surface::Cylinder {
            frame: Frame::new(Vec3::ZERO, Vec3::Z, Vec3::X),
            radius: 3.0,
        };
        for i in 0..8 {
            let uv = Vec2::new(TAU * i as f64 / 8.0, 2.5);
            let p = s.point_at(uv);
            assert!(((p.x * p.x + p.y * p.y).sqrt() - 3.0).abs() < 1e-12);
            assert!((p.z - 2.5).abs() < 1e-12);
            let back = s.invert(p, None).unwrap();
            assert!((back.u - uv.u).abs() < 1e-12 && (back.v - uv.v).abs() < 1e-12);
            // The normal points away from the axis.
            assert!(s.normal_at(uv).dot(Vec3::new(p.x, p.y, 0.0)) > 0.0);
            normal_agrees_with_cross(&s, uv);
        }
    }

    #[test]
    fn a_cone_has_a_defined_normal_at_its_apex() {
        // radius 0 at the frame origin makes the origin the apex.
        let s = Surface::Cone {
            frame: Frame::new(Vec3::ZERO, Vec3::Z, Vec3::X),
            radius: 0.0,
            half_angle: 0.5,
        };
        let apex = s.point_at(Vec2::new(1.0, 0.0));
        assert!(close(apex, Vec3::ZERO, 1e-15));
        let n = s.normal_at(Vec2::new(1.0, 0.0));
        assert!(n.is_finite() && (n.length() - 1.0).abs() < 1e-12);
        for i in 0..8 {
            normal_agrees_with_cross(&s, Vec2::new(TAU * i as f64 / 8.0, 2.0));
        }
    }

    #[test]
    fn a_sphere_has_a_defined_normal_at_both_poles() {
        let s = Surface::Sphere {
            frame: Frame::new(Vec3::ZERO, Vec3::Z, Vec3::X),
            radius: 2.0,
        };
        for pole in [
            std::f64::consts::FRAC_PI_2,
            -std::f64::consts::FRAC_PI_2,
        ] {
            let n = s.normal_at(Vec2::new(0.0, pole));
            assert!((n.length() - 1.0).abs() < 1e-12, "pole normal {n:?}");
            assert!((n.z.abs() - 1.0).abs() < 1e-9, "pole normal {n:?}");
        }
        for i in 0..8 {
            let uv = Vec2::new(TAU * i as f64 / 8.0, 0.4);
            assert!((s.point_at(uv).length() - 2.0).abs() < 1e-12);
            normal_agrees_with_cross(&s, uv);
            let back = s.invert(s.point_at(uv), None).unwrap();
            assert!((back.u - uv.u).abs() < 1e-10 && (back.v - uv.v).abs() < 1e-10);
        }
    }

    #[test]
    fn a_torus_round_trips_through_both_angles() {
        let s = Surface::Torus {
            frame: Frame::new(Vec3::new(0.0, 0.0, 1.0), Vec3::Z, Vec3::X),
            major_radius: 5.0,
            minor_radius: 1.5,
        };
        for i in 0..6 {
            for j in 0..6 {
                let uv = Vec2::new(TAU * i as f64 / 6.0, TAU * j as f64 / 6.0);
                let p = s.point_at(uv);
                let back = s.invert(p, None).unwrap();
                assert!(
                    (back.u - uv.u).abs() < 1e-10 && (back.v - uv.v).abs() < 1e-10,
                    "uv={uv:?} back={back:?}"
                );
                normal_agrees_with_cross(&s, uv);
            }
        }
    }

    #[test]
    fn a_degenerate_torus_still_evaluates() {
        // minor > major: the self-intersecting form STEP writes as
        // DEGENERATE_TOROIDAL_SURFACE.
        let s = Surface::Torus {
            frame: Frame::IDENTITY,
            major_radius: 0.6,
            minor_radius: 1.0,
        };
        for i in 0..12 {
            let uv = Vec2::new(0.3, TAU * i as f64 / 12.0);
            assert!(s.point_at(uv).is_finite());
            assert!((s.normal_at(uv).length() - 1.0).abs() < 1e-12);
        }
    }

    #[test]
    fn an_extrusion_recovers_both_parameters() {
        let s = Surface::LinearExtrusion {
            profile: Box::new(Curve::Circle {
                frame: Frame::IDENTITY,
                radius: 2.0,
            }),
            direction: Vec3::new(0.0, 0.0, 1.0),
        };
        let uv = Vec2::new(1.1, 4.0);
        let p = s.point_at(uv);
        let back = s.invert(p, None).unwrap();
        assert!((back.u - uv.u).abs() < 1e-9 && (back.v - uv.v).abs() < 1e-9, "{back:?}");
        normal_agrees_with_cross(&s, uv);
    }

    #[test]
    fn a_revolution_of_a_line_is_a_cone_and_matches_one() {
        // A line from (1,0,0) heading up and out, revolved about Z.
        let profile = Curve::Line {
            origin: Vec3::new(1.0, 0.0, 0.0),
            direction: Vec3::new(0.5, 0.0, 1.0),
        };
        let rev = Surface::Revolution {
            profile: Box::new(profile),
            frame: Frame::new(Vec3::ZERO, Vec3::Z, Vec3::X),
        };
        let cone = Surface::Cone {
            frame: Frame::new(Vec3::ZERO, Vec3::Z, Vec3::X),
            radius: 1.0,
            half_angle: 0.5f64.atan(),
        };
        for i in 0..8 {
            let u = TAU * i as f64 / 8.0;
            for v in [0.0, 1.0, 3.0] {
                // The revolution's v is the line parameter; the cone's v is
                // height, which the line reaches at the same parameter here.
                let a = rev.point_at(Vec2::new(u, v));
                let b = cone.point_at(Vec2::new(u, v));
                assert!(close(a, b, 1e-9), "u={u} v={v}: {a:?} vs {b:?}");
            }
            normal_agrees_with_cross(&rev, Vec2::new(u, 1.0));
        }
    }

    /// A flat 3×3 bicubic-ish patch, degree 1 in both directions.
    fn bilinear_patch() -> Surface {
        Surface::Nurbs(NurbsSurface {
            u_degree: 1,
            v_degree: 1,
            control_points: vec![
                vec![Vec3::new(0.0, 0.0, 0.0), Vec3::new(0.0, 4.0, 0.0)],
                vec![Vec3::new(3.0, 0.0, 0.0), Vec3::new(3.0, 4.0, 2.0)],
            ],
            weights: vec![],
            u_knots: vec![0.0, 0.0, 1.0, 1.0],
            v_knots: vec![0.0, 0.0, 1.0, 1.0],
            u_closed: false,
            v_closed: false,
        })
    }

    #[test]
    fn a_spline_patch_interpolates_its_corners() {
        let s = bilinear_patch();
        assert!(close(s.point_at(Vec2::new(0.0, 0.0)), Vec3::ZERO, 1e-12));
        assert!(close(
            s.point_at(Vec2::new(1.0, 1.0)),
            Vec3::new(3.0, 4.0, 2.0),
            1e-12
        ));
        assert_eq!(s.domain().u, Interval::UNIT);
    }

    #[test]
    fn a_spline_patch_derivative_matches_a_finite_difference() {
        let s = bilinear_patch();
        let uv = Vec2::new(0.35, 0.6);
        let h = 1e-6;
        let (du, dv) = s.derivatives_at(uv);
        let fdu = (s.point_at(Vec2::new(uv.u + h, uv.v)) - s.point_at(Vec2::new(uv.u - h, uv.v)))
            * (0.5 / h);
        let fdv = (s.point_at(Vec2::new(uv.u, uv.v + h)) - s.point_at(Vec2::new(uv.u, uv.v - h)))
            * (0.5 / h);
        assert!(close(du, fdu, 1e-6), "{du:?} vs {fdu:?}");
        assert!(close(dv, fdv, 1e-6), "{dv:?} vs {fdv:?}");
    }

    #[test]
    fn a_spline_patch_inverts_by_search() {
        let s = bilinear_patch();
        for (u, v) in [(0.2, 0.3), (0.75, 0.1), (0.5, 0.9)] {
            let uv = Vec2::new(u, v);
            let back = s.invert(s.point_at(uv), None).unwrap();
            assert!(
                (back.u - u).abs() < 1e-6 && (back.v - v).abs() < 1e-6,
                "want {uv:?} got {back:?}"
            );
        }
    }

    #[test]
    fn trimming_a_full_turn_keeps_the_seam_but_a_partial_one_removes_it() {
        let cyl = Surface::Cylinder {
            frame: Frame::IDENTITY,
            radius: 1.0,
        };
        let full = Surface::RectangularTrimmed {
            base: Box::new(cyl.clone()),
            u: Interval::new(0.0, TAU),
            v: Interval::new(0.0, 5.0),
        };
        assert_eq!(full.domain().u_period, Some(TAU));
        let half = Surface::RectangularTrimmed {
            base: Box::new(cyl),
            u: Interval::new(0.0, TAU / 2.0),
            v: Interval::new(0.0, 5.0),
        };
        assert_eq!(half.domain().u_period, None);
    }

    #[test]
    fn an_offset_surface_sits_the_offset_distance_from_its_base() {
        let base = Surface::Cylinder {
            frame: Frame::IDENTITY,
            radius: 2.0,
        };
        let s = Surface::Offset {
            base: Box::new(base.clone()),
            distance: 0.5,
        };
        for i in 0..6 {
            let uv = Vec2::new(TAU * i as f64 / 6.0, 1.0);
            let d = (s.point_at(uv) - base.point_at(uv)).length();
            assert!((d - 0.5).abs() < 1e-12, "offset distance was {d}");
        }
    }
}
