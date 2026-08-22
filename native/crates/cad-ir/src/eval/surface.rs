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

use crate::brep::{NurbsSurface, Surface};
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
            Surface::Cylinder { .. } => Domain {
                u: full_turn,
                v: unbounded,
                u_period: Some(TAU),
                v_period: None,
            },
            // A cone is bounded on one side by its apex, where the radius
            // reaches zero. Reporting it as unbounded loses the one v value a
            // conical face can legitimately close onto, which is exactly what a
            // countersink or a chamfer that runs to a point needs.
            Surface::Cone {
                radius, half_angle, ..
            } => {
                let tan = half_angle.tan();
                let v = if tan.abs() > 1e-12 {
                    let apex = -radius / tan;
                    if tan > 0.0 {
                        Interval::new(apex, UNBOUNDED)
                    } else {
                        Interval::new(-UNBOUNDED, apex)
                    }
                } else {
                    unbounded
                };
                Domain {
                    u: full_turn,
                    v,
                    u_period: Some(TAU),
                    v_period: None,
                }
            }
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
            Surface::Nurbs(n) => {
                let (u, v) = nurbs_parameter(n, u, v);
                nurbs_surface_point(n, u, v)
            }
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
            Surface::Nurbs(n) => {
                let (u, v) = nurbs_parameter(n, u, v);
                nurbs_surface_derivatives(n, u, v)
            }
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
                // The latitude is the direction's own, not the direction's
                // length measured against the nominal radius. Dividing by the
                // radius asks "how far up would a point *on* the sphere have
                // to be", which is only the same question when the point is on
                // it — and inversion is asked about points that are not, every
                // time a boundary point arrives from a neighbouring face's
                // curve or a chord's midpoint is tested. The error is
                // `(1 − |d|/R) / cos(latitude)`, so it is nothing at the
                // equator and unbounded at the pole: on this pilot it read a
                // triangle 0.25 mm off a 400 mm sphere as 5.86 mm off, which
                // was the largest remaining faceting figure in the model.
                let len = d.length();
                let z = if len > 1e-300 {
                    (d.dot(frame.axis) / len).clamp(-1.0, 1.0)
                } else {
                    return None;
                };
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
                // Sliding the point back along the sweep lands it on the
                // profile — but only once the slide is the right length, and
                // the length depends on where along the profile the point sits
                // whenever the profile is not square to the sweep. Measuring
                // that slide from the profile's start assumes it is square, and
                // for a profile with no natural start — a line, whose range is
                // the stand-in for unbounded — the start is a point far outside
                // the model and the slide comes out astronomical. Solving for
                // the profile parameter first needs no start at all: across the
                // sweep the surface is just the profile, so the parameter is
                // fixed there, and the slide follows from it.
                let axis = direction.try_normalized()?;
                let across = |w: Vec3| w - axis * w.dot(axis);
                // Solving for the profile parameter first needs no start at
                // all: across the sweep the surface is just the profile, so
                // the parameter is fixed there, and the slide follows from it.
                //
                // A hint steers the profile's own inversion, and on a profile
                // that doubles back it steers it to the wrong turn — the solve
                // then converges neatly onto a parameter a couple of hundred
                // millimetres from the point. Trying the unhinted start as
                // well, and keeping whichever lands closer, costs one extra
                // solve and removes that failure.
                let mut best: Option<(f64, f64)> = None;
                for seed in [
                    profile.param_of(p, hint.map(|h| h.u)),
                    profile.param_of(p, None),
                ]
                .into_iter()
                .flatten()
                {
                    let mut u = seed;
                    for _ in 0..24 {
                        // Everything along the sweep is free, so only what is
                        // left across it has to vanish; that is one equation
                        // in u, and its Gauss-Newton step is the profile's own
                        // across-sweep run divided into the gap still to close.
                        let gap = across(p - profile.point_at(u));
                        let run = across(profile.derivative_at(u));
                        let scale = run.length_squared();
                        if scale < 1e-300 {
                            break;
                        }
                        let step = gap.dot(run) / scale;
                        u += step;
                        if step.abs() <= 1e-13 * (1.0 + u.abs()) {
                            break;
                        }
                    }
                    let residual = across(p - profile.point_at(u)).length();
                    if best.is_none_or(|(_, r)| residual < r) {
                        best = Some((u, residual));
                    }
                }
                let u = best?.0;
                let v = (p - profile.point_at(u)).dot(*direction) / d2;
                Some(Vec2::new(u, v))
            }
            Surface::Revolution { profile, frame } => {
                // `point_at` rotates the *profile* by `u`, so `u` is measured
                // from the half-plane the profile lies in — not from the
                // frame's reference direction. Taking the absolute angle
                // instead is only right when the profile happens to lie along
                // `ref_dir`, and inverting and evaluating then disagree by
                // whatever angle it actually sits at.
                //
                // Face 1341 of `200 201 003-51` is a 34° strip of a 2 mm
                // surface of revolution whose profile sits at π. Every one of
                // its thirteen boundary points inverted to a parameter whose
                // `point_at` was 3.96 mm away — twice the radius, the far side
                // of the axis — so the face was drawn as a 4 mm tube 16.6 mm
                // long standing proud of the crankcase wall: 525 triangles of
                // a blade the part does not have, 324 mm³ of material neither
                // the STEP reading nor OpenCASCADE has.
                //
                // The profile of a surface of revolution lies in one half-plane
                // through the axis, so its angle is one number; it is taken at
                // the middle of the profile, where a curve that wanders is
                // least unrepresentative.
                let mid = profile.natural_range();
                let seat = profile.point_at((mid.lo + mid.hi) * 0.5) - frame.origin;
                let base = angle_in(frame, seat);
                let u = wrap_tau(angle_in(frame, p - frame.origin) - base);
                // Rotating the point back onto the profile's own plane.
                let unrotated = rotate_about(p, frame, -u);
                let v = profile.param_of(unrotated, hint.map(|h| h.v))?;
                Some(Vec2::new(u, v))
            }
            Surface::RectangularTrimmed { base, .. } => base.invert(p, hint),
            Surface::Offset { base, distance } => {
                // `p` sits on the offset, not on the base: p = base(uv) +
                // n(uv)·d. Inverting `p` against the base directly answers a
                // different question and lands off by roughly the offset
                // wherever the base curves, which showed up as boundary
                // points two millimetres off their own face. Undo the offset
                // instead — subtract the normal at the current guess and
                // re-invert. The normal varies far more slowly than the
                // surface does, so the fixed point converges in a few passes.
                let mut uv = base.invert(p, hint)?;
                for _ in 0..16 {
                    let next = base.invert(p - base.normal_at(uv) * *distance, Some(uv))?;
                    let d = next - uv;
                    let moved = (d.u * d.u + d.v * d.v).sqrt();
                    uv = next;
                    if moved <= 1e-12 {
                        break;
                    }
                }
                Some(uv)
            }
            // A patch that is degree one both ways is a grid of flat quads,
            // and a flat quad can be inverted exactly. The general search
            // treats it as a spline and hunts for a parameter by sampling —
            // which on a grid built from a face's own boundary lands many
            // neighbouring boundary points on the same parameter, and the
            // triangulation then reads them as one point and slits the face
            // against every neighbour. Solving the cell instead removes the
            // guesswork from the one case where there is none to do.
            Surface::Nurbs(n) if n.u_degree == 1 && n.v_degree == 1 && n.weights.is_empty() => {
                invert_grid(n, p).or_else(|| self.invert_by_search(p, hint))
            }
            Surface::Nurbs(_) => self.invert_by_search(p, hint),
        }
    }

    /// The parameter of `p` nearest `seed`, solved locally.
    ///
    /// [`Surface::invert`] answers globally: for a spline it sweeps the whole
    /// domain and takes the best it finds, which is what you want for a point
    /// with no context. Walking a boundary there is context — the previous
    /// point's parameter — and following it matters more than finding the
    /// global optimum, because the boundary has to stay a single connected
    /// path in parameter space. This starts at the seed and stays there.
    ///
    /// Returns `None` when the local solve does not actually reach the point,
    /// so a caller can tell a real answer from a nearby one.
    pub fn invert_near(&self, p: Vec3, seed: Vec2, tolerance: f64) -> Option<Vec2> {
        let d = match self {
            Surface::Nurbs(_) | Surface::Offset { .. } | Surface::RectangularTrimmed { .. } => {
                self.domain()
            }
            // Everything else inverts in closed form, and the hint already
            // selects the branch where there is one to select.
            _ => {
                let uv = self.invert(p, Some(seed))?;
                return ((self.point_at(uv) - p).length() <= tolerance).then_some(uv);
            }
        };

        // A patch stitched together from flat cells — which is what a degree-1
        // spline is, and what a rebuilt boundary produces — has a crease at
        // every cell line, and Gauss-Newton walking into one stops with the
        // point still off the surface. Sweeping a few cells around where it
        // stopped and running again from the best of those gets past the
        // crease; three rounds of a window that halves each time covers the
        // neighbourhood without ever leaving it.
        let mut best = seed;
        let mut best_d2 = (self.point_at(best) - p).length_squared();
        // The window to search around a stalled solve is the surface's own
        // length scale, not a fixed slice of the domain: a spline's is the gap
        // between its knots, which on a patch rebuilt from a boundary is one
        // cell. Searching a tenth of the domain instead steps over dozens of
        // cells and lands on the same sample for neighbouring points, which is
        // the collision this whole path exists to avoid.
        let mut window = match self {
            Surface::Nurbs(n) => Vec2::new(
                knot_spacing(&n.u_knots, n.u_degree).unwrap_or(d.u.span().abs() * 0.1) * 2.0,
                knot_spacing(&n.v_knots, n.v_degree).unwrap_or(d.v.span().abs() * 0.1) * 2.0,
            ),
            _ => Vec2::new(d.u.span().abs() * 0.1, d.v.span().abs() * 0.1),
        };
        for round in 0..3 {
            let landed = self.newton_towards(p, best, &d);
            let at = (self.point_at(landed) - p).length_squared();
            if at < best_d2 {
                best_d2 = at;
                best = landed;
            }
            if round == 2 || best_d2 <= tolerance * tolerance {
                break;
            }
            const M: usize = 4;
            for i in 0..=M {
                for j in 0..=M {
                    let uv = Vec2::new(
                        (best.u + window.u * (i as f64 / M as f64 - 0.5)).clamp(d.u.lo, d.u.hi),
                        (best.v + window.v * (j as f64 / M as f64 - 0.5)).clamp(d.v.lo, d.v.hi),
                    );
                    let at = (self.point_at(uv) - p).length_squared();
                    if at < best_d2 {
                        best_d2 = at;
                        best = uv;
                    }
                }
            }
            window = Vec2::new(window.u * 0.5, window.v * 0.5);
        }
        // Handing back the seed unchanged is not an answer: the caller seeds
        // consecutive boundary points from the same place, so a solve that
        // makes no progress gives them one parameter between them, and the
        // triangulation reads two points as one. Say so instead, and let the
        // caller ask a different question.
        if best.u == seed.u && best.v == seed.v && best_d2 > 0.0 {
            return None;
        }
        (best_d2 <= tolerance * tolerance).then_some(best)
    }

    /// Seeded search then damped Newton, for surfaces with no closed-form
    /// inverse.
    ///
    /// The parameter this returns is used as if it named the point that went
    /// in, so getting it wrong is not a matter of precision: a loop inverted
    /// onto the wrong part of a patch lands somewhere else entirely in
    /// parameter space, and the face it bounds is then trimmed against a
    /// boundary that has nothing to do with it. Three things keep that from
    /// happening — seeding from the control net rather than a fixed sweep,
    /// refusing any Newton step that moves further from the target, and
    /// sweeping again around the best answer when one round does not land.
    fn invert_by_search(&self, p: Vec3, hint: Option<Vec2>) -> Option<Vec2> {
        let d = self.domain();
        const N: usize = 12;

        let mut best = hint.unwrap_or(Vec2::new(d.u.at(0.5), d.v.at(0.5)));
        let mut best_d2 = (self.point_at(best) - p).length_squared();
        let offer = |uv: Vec2, at: f64, best: &mut Vec2, best_d2: &mut f64| {
            if at < *best_d2 {
                *best_d2 = at;
                *best = uv;
            }
        };
        for i in 0..=N {
            for j in 0..=N {
                let uv = Vec2::new(d.u.at(i as f64 / N as f64), d.v.at(j as f64 / N as f64));
                let at = (self.point_at(uv) - p).length_squared();
                offer(uv, at, &mut best, &mut best_d2);
            }
        }

        // A spline never strays far from its control net, so the pole nearest
        // the target names very nearly the parameter that reaches it — a far
        // better guess than any sweep coarse enough to be affordable, and the
        // one that matters on a patch whose detail is finer than the sweep.
        if let Surface::Nurbs(n) = self {
            let mut nearest = f64::INFINITY;
            let mut seed = None;
            for (i, row) in n.control_points.iter().enumerate() {
                for (j, q) in row.iter().enumerate() {
                    let dist = (*q - p).length_squared();
                    if dist < nearest {
                        nearest = dist;
                        seed = Some(Vec2::new(
                            greville(&n.u_knots, n.u_degree, i),
                            greville(&n.v_knots, n.v_degree, j),
                        ));
                    }
                }
            }
            if let Some(uv) = seed {
                let at = (self.point_at(uv) - p).length_squared();
                offer(uv, at, &mut best, &mut best_d2);
            }
        }

        // Newton, then a sweep around whatever it reached, then Newton again.
        // Each round searches a window a fifth the width of the last, so three
        // rounds resolve a hundred and twenty-fifth of the domain — past the
        // point where a seed can be in the wrong knot span.
        let mut window = Vec2::new(d.u.span().abs(), d.v.span().abs());
        for round in 0..3 {
            let landed = self.newton_towards(p, best, &d);
            let at = (self.point_at(landed) - p).length_squared();
            offer(landed, at, &mut best, &mut best_d2);
            // Good enough when the residual is negligible against the window
            // still being searched, which is the only length this function
            // knows that scales with the surface.
            if round == 2 || best_d2 < (window.u.hypot(window.v) * 1e-9).powi(2) {
                break;
            }
            window = Vec2::new(window.u / 5.0, window.v / 5.0);
            const M: usize = 4;
            for i in 0..=M {
                for j in 0..=M {
                    let uv = Vec2::new(
                        (best.u + window.u * (i as f64 / M as f64 - 0.5)).clamp(d.u.lo, d.u.hi),
                        (best.v + window.v * (j as f64 / M as f64 - 0.5)).clamp(d.v.lo, d.v.hi),
                    );
                    let at = (self.point_at(uv) - p).length_squared();
                    offer(uv, at, &mut best, &mut best_d2);
                }
            }
        }
        Some(best)
    }

    /// Gauss-Newton towards `p` from `start`, backtracking on any step that
    /// does not improve.
    ///
    /// The undamped step is the right one near the solution and a wild one far
    /// from it; halving until it improves costs a few evaluations and removes
    /// the failure where an overshoot clamps against the domain edge and the
    /// iteration then reports that corner as the answer.
    pub(crate) fn newton_towards(&self, p: Vec3, start: Vec2, d: &Domain) -> Vec2 {
        let mut uv = start;
        let mut here = (self.point_at(uv) - p).length_squared();
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

            let mut scale = 1.0;
            let mut moved = Vec2::default();
            let mut improved = false;
            for _ in 0..8 {
                let next = Vec2::new(
                    (uv.u - step_u * scale).clamp(d.u.lo, d.u.hi),
                    (uv.v - step_v * scale).clamp(d.v.lo, d.v.hi),
                );
                let there = (self.point_at(next) - p).length_squared();
                if there < here {
                    moved = next - uv;
                    uv = next;
                    here = there;
                    improved = true;
                    break;
                }
                scale *= 0.5;
            }
            if !improved {
                break;
            }
            if moved.u.abs() < 1e-13 && moved.v.abs() < 1e-13 {
                break;
            }
        }
        uv
    }
}

/// The narrowest gap between distinct knots inside a spline's domain.
///
/// This is the parameter length over which the surface can change shape, so it
/// is the scale any local search around a point has to work at.
fn knot_spacing(knots: &[f64], degree: usize) -> Option<f64> {
    let (lo, hi) = nurbs::domain(knots, degree)?;
    let mut narrowest = f64::INFINITY;
    for w in knots.windows(2) {
        let gap = w[1] - w[0];
        if gap > 0.0 && w[0] >= lo && w[1] <= hi {
            narrowest = narrowest.min(gap);
        }
    }
    narrowest.is_finite().then_some(narrowest)
}

/// The parameter a spline's `i`th pole pulls hardest at.
///
/// The Greville abscissa — the mean of the `degree` knots following the pole —
/// is where that pole's basis function peaks, so it is the parameter the pole
/// stands for.
fn greville(knots: &[f64], degree: usize, i: usize) -> f64 {
    if degree == 0 {
        return knots.get(i).copied().unwrap_or(0.0);
    }
    let mut sum = 0.0;
    let mut count = 0usize;
    for k in 1..=degree {
        if let Some(v) = knots.get(i + k) {
            sum += *v;
            count += 1;
        }
    }
    if count == 0 {
        knots.first().copied().unwrap_or(0.0)
    } else {
        sum / count as f64
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

/// Bring a parameter pair into the range the spline is actually defined on.
///
/// A B-spline says nothing outside its knots: evaluating there runs the basis
/// functions past their support and the point flies off the surface — a
/// hundred millimetres away on a patch a hundredth of a unit wide, measured on
/// this assembly. Parameters do arrive from outside, because a loop that
/// crosses a seam is unwrapped by whole periods to keep it continuous. So a
/// direction the surface closes in wraps, which is exact and what the loop
/// meant, and one it does not close in clamps, which is the nearest thing the
/// surface can answer.
fn nurbs_parameter(n: &NurbsSurface, u: f64, v: f64) -> (f64, f64) {
    let fold = |t: f64, knots: &[f64], degree: usize, closed: bool| {
        let Some((lo, hi)) = nurbs::domain(knots, degree) else {
            return t;
        };
        if t >= lo && t <= hi {
            return t;
        }
        let span = hi - lo;
        if closed && span > 0.0 {
            let k = ((t - lo) / span).floor();
            // The last span still belongs to the domain; keep the far edge
            // rather than folding it to the near one.
            (t - k * span).clamp(lo, hi)
        } else {
            t.clamp(lo, hi)
        }
    };
    (
        fold(u, &n.u_knots, n.u_degree, n.u_closed),
        fold(v, &n.v_knots, n.v_degree, n.v_closed),
    )
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
    use crate::brep::Curve;

    fn close(a: Vec3, b: Vec3, eps: f64) -> bool {
        (a - b).length() < eps
    }

    #[test]
    fn an_offset_surface_inverts_onto_itself_not_onto_its_base() {
        use crate::math::Frame;
        // A cylinder thickened by a millimetre. A point on the offset is a
        // millimetre off the base, so inverting it against the base answers a
        // different question — and the round trip then lands a further
        // millimetre away, which is what left offset faces two millimetres
        // off their own boundary.
        let base = Surface::Cylinder {
            frame: Frame::new(Vec3::ZERO, Vec3::Z, Vec3::X),
            radius: 10.0,
        };
        let s = Surface::Offset {
            base: Box::new(base),
            distance: 1.0,
        };
        for k in 0..8 {
            let uv = Vec2::new(k as f64 * 0.7, k as f64 * 1.3 - 4.0);
            let p = s.point_at(uv);
            let back = s.invert(p, None).expect("offset inverts");
            assert!(
                close(s.point_at(back), p, 1e-6),
                "offset round trip at {uv:?} gave {:?} for {p:?}",
                s.point_at(back)
            );
        }
    }

    #[test]
    fn a_degree_one_grid_inverts_cell_by_cell() {
        use crate::brep::NurbsSurface;
        // A curved grid of flat cells, the shape a face rebuilt from its own
        // boundary takes. Consecutive points along one cell have to come back
        // as distinct parameters: reading two of them as one is what slits a
        // rebuilt face against every neighbour.
        const N: usize = 8;
        let grid: Vec<Vec<Vec3>> = (0..=N)
            .map(|i| {
                (0..=N)
                    .map(|j| {
                        let (u, v) = (i as f64 / N as f64, j as f64 / N as f64);
                        Vec3::new(u * 10.0, v * 6.0, (u * 3.0).sin() + v * v)
                    })
                    .collect()
            })
            .collect();
        let mut knots = vec![0.0];
        knots.extend((0..=N).map(|k| k as f64 / N as f64));
        knots.push(1.0);
        let s = Surface::Nurbs(NurbsSurface {
            u_degree: 1,
            v_degree: 1,
            control_points: grid,
            weights: Vec::new(),
            u_knots: knots.clone(),
            v_knots: knots,
            u_closed: false,
            v_closed: false,
        });

        let mut previous: Option<Vec2> = None;
        for k in 0..20 {
            let uv = Vec2::new(0.31 + k as f64 * 0.001, 0.42);
            let p = s.point_at(uv);
            let back = s.invert(p, None).expect("grid inverts");
            assert!(
                close(s.point_at(back), p, 1e-9),
                "grid round trip at {uv:?} landed at {:?}",
                s.point_at(back)
            );
            if let Some(prev) = previous {
                assert!(back.u > prev.u, "two points collapsed onto one parameter");
            }
            previous = Some(back);
        }
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
    fn a_revolution_inverts_to_the_parameter_it_was_evaluated_at() {
        // The profile deliberately does not lie along the frame's reference
        // direction: it sits a quarter turn away, on the far side of the axis
        // from where `ref_dir` points. Inverting used to return the absolute
        // angle about the axis, so `point_at` of the answer landed at the
        // profile's own angle away from the point it started from — on the
        // pilot, twice a 2 mm radius, and the face was drawn as a tube
        // standing proud of the wall.
        for seat in [0.0, 1.0, std::f64::consts::PI, 4.5] {
            let (sin, cos) = seat.sin_cos();
            let profile = Curve::Line {
                origin: Vec3::new(2.0 * cos, 2.0 * sin, -5.0),
                direction: Vec3::new(0.0, 0.0, 10.0),
            };
            let s = Surface::Revolution {
                profile: Box::new(profile),
                frame: Frame::IDENTITY,
            };
            for &(u, v) in &[(0.0, 0.25), (0.7, 0.5), (3.0, 0.1), (5.9, 0.9)] {
                let uv = Vec2::new(u, v);
                let p = s.point_at(uv);
                let back = s.invert(p, None).expect("a point on the surface inverts");
                let there = s.point_at(back);
                assert!(
                    (there - p).length() < 1e-9,
                    "profile at {seat}: {uv:?} evaluated to {p:?}, inverted to {back:?}, \
                     which evaluates to {there:?}"
                );
            }
        }
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
    /// A profile swept at an angle to its own plane: the displacement along the
    /// sweep is not the same for every point of the profile, so measuring it
    /// from the profile's start puts the point off the curve and the parameter
    /// it inverts to is not the one it came from.
    #[test]
    fn extrusion_inverts_a_profile_that_is_not_square_to_the_sweep() {
        let profile = Curve::Line {
            origin: Vec3::new(0.0, 0.0, 0.0),
            direction: Vec3::new(1.0, 0.0, 1.0).try_normalized().unwrap(),
        };
        let s = Surface::LinearExtrusion {
            profile: Box::new(profile),
            direction: Vec3::new(0.0, 0.0, 1.0),
        };
        for &(u, v) in &[(0.0, 0.0), (3.0, 2.0), (-4.0, 7.5), (10.0, -3.25)] {
            let p = s.point_at(Vec2::new(u, v));
            let back = s.invert(p, None).expect("the point inverts");
            let round = s.point_at(back);
            assert!(
                (round - p).length() < 1e-9,
                "({u}, {v}) inverted to ({}, {}), which is {} away",
                back.u,
                back.v,
                (round - p).length()
            );
        }
    }

    /// A spline evaluated past its knots runs its basis functions off their
    /// support and the point leaves the surface. Parameters do arrive from
    /// outside, so the surface has to answer for them.
    #[test]
    fn nurbs_outside_its_knots_stays_on_the_surface() {
        let n = NurbsSurface {
            u_degree: 1,
            v_degree: 1,
            control_points: vec![
                vec![Vec3::new(0.0, 0.0, 0.0), Vec3::new(0.0, 1.0, 0.0)],
                vec![Vec3::new(1.0, 0.0, 0.0), Vec3::new(1.0, 1.0, 0.5)],
            ],
            weights: Vec::new(),
            u_knots: vec![0.0, 0.0, 1.0, 1.0],
            v_knots: vec![0.0, 0.0, 1.0, 1.0],
            u_closed: false,
            v_closed: false,
        };
        let s = Surface::Nurbs(n);
        let inside = s.point_at(Vec2::new(0.0, 0.0));
        let outside = s.point_at(Vec2::new(-0.4, 0.0));
        assert!(
            (outside - inside).length() < 1e-12,
            "a parameter below the knot range should clamp to the edge, got {outside:?}"
        );
        let far = s.point_at(Vec2::new(1.9, 1.6));
        let corner = s.point_at(Vec2::new(1.0, 1.0));
        assert!(
            (far - corner).length() < 1e-12,
            "a parameter above the knot range should clamp to the edge, got {far:?}"
        );
    }
}

/// The parameter of `p` on a degree-one grid, solved cell by cell.
///
/// For degree one the control points *are* the surface at the knots, so the
/// patch is the grid of bilinear quads between them and inversion is a
/// sequence of small exact problems rather than one global search. Every cell
/// is tried and the nearest wins, so the answer does not depend on where a
/// sweep happened to sample; `None` only if the grid is too small to have a
/// cell at all.
fn invert_grid(n: &NurbsSurface, p: Vec3) -> Option<Vec2> {
    let rows = n.control_points.len();
    let cols = n.control_points.first().map(|r| r.len())?;
    if rows < 2 || cols < 2 || n.u_knots.len() < rows + 2 || n.v_knots.len() < cols + 2 {
        return None;
    }
    // Degree one: control point k sits at knot k + 1.
    let (mut best, mut best_d) = (Vec2::new(n.u_knots[1], n.v_knots[1]), f64::INFINITY);
    for i in 0..rows - 1 {
        for j in 0..cols - 1 {
            let q = [
                n.control_points[i][j],
                n.control_points[i + 1][j],
                n.control_points[i][j + 1],
                n.control_points[i + 1][j + 1],
            ];
            // Skip a cell that cannot hold the point, cheaply, before solving.
            let mut lo = q[0];
            let mut hi = q[0];
            for c in &q[1..] {
                lo = Vec3::new(lo.x.min(c.x), lo.y.min(c.y), lo.z.min(c.z));
                hi = Vec3::new(hi.x.max(c.x), hi.y.max(c.y), hi.z.max(c.z));
            }
            let outside = (lo.x - p.x).max(p.x - hi.x).max(lo.y - p.y).max(p.y - hi.y)
                .max(lo.z - p.z)
                .max(p.z - hi.z)
                .max(0.0);
            if outside * outside > best_d {
                continue;
            }
            let (s, t, d) = nearest_on_bilinear(q, p);
            if d < best_d {
                best_d = d;
                best = Vec2::new(
                    n.u_knots[i + 1] + (n.u_knots[i + 2] - n.u_knots[i + 1]) * s,
                    n.v_knots[j + 1] + (n.v_knots[j + 2] - n.v_knots[j + 1]) * t,
                );
            }
        }
    }
    best_d.is_finite().then_some(best)
}

/// The `(s, t)` in the unit square nearest `p` on one bilinear cell, with the
/// squared distance. Corners are ordered `(0,0) (1,0) (0,1) (1,1)`.
fn nearest_on_bilinear(q: [Vec3; 4], p: Vec3) -> (f64, f64, f64) {
    let at = |s: f64, t: f64| {
        q[0] * ((1.0 - s) * (1.0 - t))
            + q[1] * (s * (1.0 - t))
            + q[2] * ((1.0 - s) * t)
            + q[3] * (s * t)
    };
    // Start from the better diagonal split, then a few clamped Gauss-Newton
    // steps. The cell is small and nearly flat, so this settles at once.
    let (mut s, mut t) = (0.5, 0.5);
    for _ in 0..12 {
        let r = at(s, t) - p;
        let ds = (q[1] - q[0]) * (1.0 - t) + (q[3] - q[2]) * t;
        let dt = (q[2] - q[0]) * (1.0 - s) + (q[3] - q[1]) * s;
        let (a, b, c) = (ds.dot(ds), ds.dot(dt), dt.dot(dt));
        let det = a * c - b * b;
        if det.abs() <= f64::MIN_POSITIVE {
            break;
        }
        let (u, v) = (ds.dot(r), dt.dot(r));
        let step_s = (c * u - b * v) / det;
        let step_t = (a * v - b * u) / det;
        let (ns, nt) = ((s - step_s).clamp(0.0, 1.0), (t - step_t).clamp(0.0, 1.0));
        if (ns - s).abs() + (nt - t).abs() <= 1e-15 {
            s = ns;
            t = nt;
            break;
        }
        s = ns;
        t = nt;
    }
    (s, t, (at(s, t) - p).length_squared())
}
