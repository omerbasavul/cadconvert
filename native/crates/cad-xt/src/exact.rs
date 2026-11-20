//! Geometry a Parasolid file defines without stating.
//!
//! Most of what a `.x_t` holds is written out: a plane is a point and a
//! normal, a spline is its poles and knots, and reading it is reading it. Three
//! things are not. An intersection curve is "where this surface meets that
//! one", with a handful of sampled points — its *chart* — to say which of the
//! places they meet is meant. An offset surface is another surface moved a
//! distance along its normal. A rolling-ball blend is the surface a ball of a
//! stated radius sweeps while it touches two others, and its two boundaries
//! are the tracks where it touches them. None of these has a formula in the
//! file; each has a definition, and the modeller that wrote the file evaluates
//! the definition whenever it needs a point.
//!
//! So does this. Every surface the file can name — stated or defined — is read
//! here as a [`Field`]: something that says, of any point in space, how far it
//! is from the surface and which way is away. That one question is enough,
//! because every definition above is a statement about distances:
//!
//! * a surface is where its own distance is zero;
//! * an offset is where the base's distance is the offset;
//! * a blend is where the distance to the ball's centre line is the radius,
//!   and the centre line is where the distances to the two blended surfaces
//!   are both the radius;
//! * a blend's boundary is where the ball that touches one surface here
//!   touches the other as well;
//! * and an intersection curve is where two fields are zero together.
//!
//! The chart is what makes that last one a computation rather than a search.
//! Its points are on the curve — measured on a mannequin's 621 intersections,
//! to sixteen decimal places — and they are in order, so between any two of
//! them the curve is the one place, in the plane half way along, where both
//! fields vanish. That is three equations in three unknowns with a starting
//! point a chord's bow away from the answer, and Newton closes it in a few
//! steps.
//!
//! A blend's own surface is built the same way: its centre line is a curve
//! like any other, each cross-section is the circular arc between the ball's
//! two contacts — which a rational quadratic is exactly — and along the line
//! the sections are joined by cubics that leave each as fast as it is really
//! changing, checked against the definition between them and divided until
//! they hold. See [`Blend::surface`].
//!
//! Nothing here is fitted, guessed or rebuilt from what lies around it. Every
//! reading of the file's conventions that the format does not document — which
//! side of a surface an offset lies on, which of a blend's two surfaces a
//! boundary belongs to, how a surface carries on past its own edge — is
//! settled by the file's own points, which either sit where the reading says
//! or do not. Where a definition cannot be read, or will not close on the
//! file's own points, the answer is `None`, and the caller keeps whatever it
//! had before.

use crate::geom::{self, Index};
use cad_ir::brep::Surface;
use cad_ir::eval::surface::Domain;
use cad_ir::math::{Vec2, Vec3};
use rustc_hash::FxHashMap;
use std::cell::{Cell, OnceCell, RefCell};
use std::rc::Rc;
use xt_parser::entity::{Entities, RawEntity};
use xt_parser::schema as xt;

/// How far from a field's surface a point may be and still be on it: a
/// hundredth of a micron, in the metres a Parasolid file is written in, which
/// is the modeller's own linear resolution. The file's points are on their
/// surfaces to a millionth of that.
pub const RESOLUTION: f64 = 1e-8;

/// What a field says about one point.
#[derive(Debug, Clone, Copy)]
pub struct Reading {
    /// How far the point is from the field's surface, signed.
    pub distance: f64,
    /// The unit direction in which that distance grows.
    pub uphill: Vec3,
}

/// Where a field last looked, so the next look can start from there.
///
/// A surface can pass the same place twice — a limb written as one sheet
/// rolled round until its ends overlap does — and then "the nearest point" is
/// the wrong question: a curve lies on one of the two sheets and stays on it.
/// What keeps it there is that each reading starts from the last.
#[derive(Debug, Clone, Copy)]
pub enum Hint {
    /// The point asked about, where on the surface it landed, and what the
    /// reading was.
    Foot(Option<(Vec3, Vec2, f64)>),
    /// The ball's centre last found, and how far along its line that was.
    Centre(Option<Vec3>, Option<f64>),
}

/// A surface, read as the distance from it.
#[derive(Debug, Clone)]
pub enum Field {
    /// A surface the file states outright.
    Surface {
        surface: Surface,
        at: Cell<Option<(Vec3, Vec2, f64)>>,
    },
    /// Everything at one signed distance from another field.
    Offset { base: Box<Field>, by: f64 },
    /// The surface a rolling ball sweeps.
    Blend(Box<Blend>),
    /// The boundary of a blend on one of the two surfaces it blends: zero
    /// where the ball touching the near surface here touches the far one too.
    Rail(Box<Rail>),
}

/// A rolling-ball blend, as the file defines it.
#[derive(Debug, Clone)]
pub struct Blend {
    pub a: Field,
    pub b: Field,
    /// The signed distance at which the ball's centre rides off each surface.
    pub ra: f64,
    pub rb: f64,
    pub radius: f64,
    /// The file's own samples of the centre line, in order.
    pub spine: Vec<Vec3>,
    /// Which of them are its chart; the others are the line's own ends,
    /// where the chart stops short of them. See [`Fields::charted`].
    charted: std::ops::Range<usize>,
    centre: Cell<Option<Vec3>>,
    /// Where along the centre line that centre was.
    along: Cell<Option<f64>>,
    /// The centre line itself, computed the first time the blend is read as
    /// a surface and kept — by every copy of the blend, and by every reading
    /// of the same blend in the file. See [`Blend::centre_on_line`].
    laid: Rc<Laid>,
    /// Which side of the first contact the arc last opened on, for the
    /// sections where the two contacts are one point and cannot say.
    way: Cell<f64>,
}

/// A blend's centre line, once it has been asked for.
#[derive(Debug, Default)]
struct Laid {
    line: OnceCell<Option<cad_ir::brep::Curve>>,
    /// Set while the line is being computed. Computing it reads the two
    /// blended surfaces, and if one of them is a blend that is itself rolled
    /// against this one, reading it asks for this line again: asked while it
    /// is still being made, the answer is that there is none yet.
    under_way: Cell<bool>,
}

/// One boundary of a blend.
#[derive(Debug, Clone)]
pub struct Rail {
    pub near: Field,
    pub far: Field,
    pub r_near: f64,
    pub r_far: f64,
}

impl Field {
    pub fn surface(surface: Surface) -> Field {
        Field::Surface {
            surface,
            at: Cell::new(None),
        }
    }

    /// The distance of `x` from this field's surface, and which way is away.
    pub fn read(&self, x: Vec3) -> Option<Reading> {
        self.read_near(x, 0.0)
    }

    /// [`Field::read`], for a point expected to be about `expect` from the
    /// surface.
    ///
    /// The expectation matters only where a surface passes a place twice and
    /// there is nothing to say which pass is meant: an offset's points are the
    /// offset from *their* sheet, and a nearer sheet is not theirs.
    pub fn read_near(&self, x: Vec3, expect: f64) -> Option<Reading> {
        if !x.is_finite() {
            return None;
        }
        match self {
            Field::Surface { surface, at } => {
                // A surface with a formula says outright where every normal
                // through the point meets it, and the one meant is the one
                // the point stands the expected distance from.
                if let Some(feet) = feet(surface, x) {
                    let (distance, uphill) = feet
                        .into_iter()
                        .min_by(|a, b| (a.0 - expect).abs().total_cmp(&(b.0 - expect).abs()))?;
                    return Some(Reading { distance, uphill });
                }
                let measure = |uv: Vec2| {
                    let (point, normal) = beyond(surface, x, uv);
                    let distance = (x - point).dot(normal);
                    (distance.is_finite() && normal.is_finite()).then_some((uv, distance, normal))
                };
                // From where the last reading landed, a stride at a time,
                // never leaving the sheet it was on. Distance cannot change
                // faster than the point moves, so a stride whose reading has
                // is not of the same sheet, and the following stops there.
                let followed = at.get().and_then(|(was, from, then)| {
                    let strides = (((x - was).length() / STRIDE).ceil() as usize).clamp(1, 96);
                    let (mut uv, mut reading, mut here) = (from, then, was);
                    let mut normal = Vec3::ZERO;
                    for k in 1..=strides {
                        let y = if k == strides { x } else { was + (x - was) * (k as f64 / strides as f64) };
                        uv = surface.nearest_from(y, uv)?;
                        let (point, n) = beyond(surface, y, uv);
                        let d = (y - point).dot(n);
                        if !d.is_finite() || (d - reading).abs() > (y - here).length() * 1.001 + 1e-12 {
                            return None;
                        }
                        (reading, here, normal) = (d, y, n);
                    }
                    normal.is_finite().then_some((uv, reading, normal))
                });
                let found = match followed {
                    Some(f) => f,
                    // Nothing to follow, or it would not be followed: the
                    // whole surface is searched, and where that finds another
                    // pass of it than the one expected, so be it — the caller
                    // judges the reading, not this.
                    None => surface.nearest(x, None).and_then(measure)?,
                };
                at.set(Some((x, found.0, found.1)));
                Some(Reading {
                    distance: found.1,
                    uphill: found.2,
                })
            }
            Field::Offset { base, by } => {
                let r = base.read_near(x, expect + by)?;
                Some(Reading {
                    distance: r.distance - by,
                    uphill: r.uphill,
                })
            }
            Field::Blend(blend) => {
                let (centre, along) = blend.centre_for(x)?;
                let out = x - centre;
                let length = out.length();
                // On the centre line itself — which, for a blend of no
                // radius, is on the surface — every direction square to the
                // line is as much "away" as any other.
                let away = out
                    .try_normalized()
                    .or_else(|| along.cross(Vec3::X).try_normalized())
                    .or_else(|| along.cross(Vec3::Y).try_normalized())?;
                // The ball's section is a circle, and the normal through a
                // point meets a circle twice: on the point's side of the
                // centre and on the far side. See [`feet`].
                let near = (length - blend.radius, away);
                let far = (-(length + blend.radius), -away);
                let (distance, uphill) =
                    if (near.0 - expect).abs() <= (far.0 - expect).abs() { near } else { far };
                Some(Reading { distance, uphill })
            }
            Field::Rail(rail) => rail.read(x),
        }
    }

    /// Everything this field remembers about where it last looked.
    pub fn memory(&self) -> Vec<Hint> {
        let mut out = Vec::new();
        self.remember(&mut out);
        out
    }

    fn remember(&self, out: &mut Vec<Hint>) {
        match self {
            Field::Surface { at, .. } => out.push(Hint::Foot(at.get())),
            Field::Offset { base, .. } => base.remember(out),
            Field::Blend(b) => {
                out.push(Hint::Centre(b.centre.get(), b.along.get()));
                b.a.remember(out);
                b.b.remember(out);
            }
            Field::Rail(r) => {
                r.near.remember(out);
                r.far.remember(out);
            }
        }
    }

    /// Put back what [`Field::memory`] took.
    pub fn recall(&self, memory: &[Hint]) {
        self.restore(&mut memory.iter());
    }

    fn restore(&self, from: &mut std::slice::Iter<'_, Hint>) {
        match self {
            Field::Surface { at, .. } => {
                if let Some(Hint::Foot(h)) = from.next() {
                    at.set(*h);
                }
            }
            Field::Offset { base, .. } => base.restore(from),
            Field::Blend(b) => {
                if let Some(Hint::Centre(c, t)) = from.next() {
                    b.centre.set(*c);
                    b.along.set(*t);
                }
                b.a.restore(from);
                b.b.restore(from);
            }
            Field::Rail(r) => {
                r.near.restore(from);
                r.far.restore(from);
            }
        }
    }

    /// Move what a surface remembers from the point it last read to the foot
    /// of that point on the surface itself.
    ///
    /// The next reading is followed from wherever the last one was, a stride
    /// at a time. A point known to be next to the foot is read in one stride
    /// from the foot, where from the point the foot was found for — a ball's
    /// centre, a whole radius off — it is as many strides as the radius is
    /// long.
    fn to_foot(&self) {
        match self {
            Field::Surface { surface, at } => {
                if let Some((x, uv, _)) = at.get() {
                    let (foot, _) = beyond(surface, x, uv);
                    at.set(Some((foot, uv, 0.0)));
                }
            }
            Field::Offset { base, .. } => base.to_foot(),
            Field::Blend(_) | Field::Rail(_) => {}
        }
    }

    /// Start the next reading from nothing.
    pub fn forget(&self) {
        match self {
            Field::Surface { at, .. } => at.set(None),
            Field::Offset { base, .. } => base.forget(),
            Field::Blend(b) => {
                b.centre.set(None);
                b.along.set(None);
                b.a.forget();
                b.b.forget();
            }
            Field::Rail(r) => {
                r.near.forget();
                r.far.forget();
            }
        }
    }
}

/// Where the normals through `x` meet a surface that has a formula: for each,
/// how far `x` stands from the surface along it, and the surface's own normal
/// there. `None` for a surface that has none, or a point with no one answer —
/// on a cylinder's axis, at a sphere's centre.
///
/// "The distance from a surface" is usually taken to the nearest point, and
/// for a point near the surface that is the right one. It is not what an
/// offset or a rolling ball means. A two-millimetre ball rolled against a
/// cylinder of one millimetre radius, from the side the cylinder curves away
/// from, has its centre *past the cylinder's axis*: a millimetre beyond it,
/// two from the contact, and no distance at all from the cylinder's far wall,
/// which is the nearest. The centre is two millimetres along a normal — not
/// the nearest one. A cylinder has two normals through any point, a torus
/// four, and the reading is taken on the one the caller expects.
fn feet(surface: &Surface, x: Vec3) -> Option<Vec<(f64, Vec3)>> {
    let at = |foot: Vec3| -> Option<(f64, Vec3)> {
        let uv = surface.invert(foot, None)?;
        let normal = surface.normal_at(uv);
        let distance = (x - surface.point_at(uv)).dot(normal);
        (distance.is_finite() && normal.is_finite()).then_some((distance, normal))
    };
    // The unit direction from an axis out to `x`.
    let out_from = |origin: Vec3, axis: Vec3| {
        let r = x - origin;
        (r - axis * r.dot(axis)).try_normalized()
    };
    match surface {
        Surface::Cylinder { frame, .. } | Surface::Cone { frame, .. } => {
            // Each is straight along its length: the normals through `x` meet
            // it on the two rulings in the plane of `x` and the axis, one on
            // the side `x` is on and one on the other.
            // On the axis every ruling is as near as any other.
            out_from(frame.origin, frame.axis)?;
            let near = surface.invert(x, None)?;
            let mut found = Vec::with_capacity(2);
            for u in [near.u, near.u + std::f64::consts::PI] {
                let (a, b) = (surface.point_at(Vec2::new(u, 0.0)), surface.point_at(Vec2::new(u, 1.0)));
                let along = (b - a).try_normalized()?;
                found.push(at(a + along * (x - a).dot(along))?);
            }
            Some(found)
        }
        Surface::Sphere { frame, radius } => {
            let out = (x - frame.origin).try_normalized()?;
            Some(vec![at(frame.origin + out * *radius)?, at(frame.origin - out * *radius)?])
        }
        Surface::Torus { frame, major_radius, minor_radius } => {
            let out = out_from(frame.origin, frame.axis)?;
            let mut found = Vec::with_capacity(4);
            for side in [1.0, -1.0] {
                let centre = frame.origin + out * (*major_radius * side);
                let spoke = (x - centre).try_normalized()?;
                found.push(at(centre + spoke * *minor_radius)?);
                found.push(at(centre - spoke * *minor_radius)?);
            }
            Some(found)
        }
        _ => None,
    }
}

/// How far a reading is followed in one go: two millimetres, in the metres
/// the file is written in. Short enough that the nearest point moves a little
/// way along the sheet it is on rather than a long way to somewhere else.
const STRIDE: f64 = 2e-3;

/// A spline patch at `(u, v)`, carried on in a straight line past its edges.
///
/// A spline stops at the ends of its knots; the surface it stands for does
/// not. An offset is taken of the whole sheet and a blend is rolled against
/// it past its edge, and out there the modeller carries the patch on
/// tangentially: from the last point of the patch, along the patch's own
/// direction there. Which rule it uses is not documented and was read off a
/// mannequin's file, whose blends run up to 50 mm off the ends of its limbs.
/// Carried tangentially, a centre line's own points sit at the ball's radius
/// from the surface to thirteen places, 40 mm past the edge; carried as the
/// polynomial the last span is — the other plausible rule — a point a
/// millimetre past the edge already misses by eight microns.
///
/// Past a corner both directions are carried, one after the other, which is
/// the tangent plane's own twist and not just its two slopes.
fn carried(surface: &Surface, d: &Domain, q: Vec2) -> Vec3 {
    carried_with_slopes(surface, d, q).0
}

/// [`carried`], with how the carried patch changes along each parameter.
///
/// Past an edge the patch is a straight line in the direction that left it,
/// so its slope that way is the edge's own, and its slope the other way is
/// the edge's with the twist added in proportion to how far out it is. That
/// is written down here rather than measured by stepping either side: the
/// solve that looks for the nearest point of a carried patch asks for the
/// slopes five times a step, and measured, each asking was five evaluations
/// of the patch — three fifths of all the time a file full of blends rolled
/// off the ends of patches took to read.
fn carried_with_slopes(surface: &Surface, d: &Domain, q: Vec2) -> (Vec3, Vec3, Vec3) {
    let hold = |t: f64, lo: f64, hi: f64, period: Option<f64>| {
        if period.is_some() { t } else { t.clamp(lo, hi) }
    };
    let on = Vec2::new(
        hold(q.u, d.u.lo, d.u.hi, d.u_period),
        hold(q.v, d.v.lo, d.v.hi, d.v_period),
    );
    let (over_u, over_v) = (q.u - on.u, q.v - on.v);
    let at = surface.point_at(on);
    let (su, sv) = surface.derivatives_at(on);
    if over_u == 0.0 && over_v == 0.0 {
        return (at, su, sv);
    }
    // The twist: how the slope along u changes with v, taken from two steps
    // inside the patch because there is nothing outside it to ask — two, so
    // that what the slope's own curvature adds to one step is taken off again
    // by the other.
    let h = d.v.span().abs() * 1e-4;
    let inward = if over_v > 0.0 || (over_v == 0.0 && on.v + 2.0 * h > d.v.hi) { -h } else { h };
    let (one, _) = surface.derivatives_at(Vec2::new(on.u, on.v + inward));
    let (two, _) = surface.derivatives_at(Vec2::new(on.u, on.v + 2.0 * inward));
    let twist = (one * 4.0 - two - su * 3.0) * (0.5 / inward);
    (
        at + su * over_u + sv * over_v + twist * (over_u * over_v),
        su + twist * over_v,
        sv + twist * over_u,
    )
}

/// The point of `surface` nearest `x` and the normal there, taken on the
/// patch carried past its own edge where the nearest point lies beyond it.
fn beyond(surface: &Surface, x: Vec3, uv: Vec2) -> (Vec3, Vec3) {
    let here = (surface.point_at(uv), surface.normal_at(uv));
    // A patch has an edge to be carried past at its last knot, and a swept
    // surface at the end of the curve that was swept: a profile carried on
    // along its own tangent sweeps a surface carried on along its own, so the
    // one rule serves both. `204 201 013-51` has a 0.2 mm blend whose centre
    // line starts 2.8 mm before the profile of the surface it rolls on does.
    if !matches!(surface, Surface::Nurbs(_) | Surface::LinearExtrusion { .. }) {
        return here;
    }
    let d = surface.domain();
    let (du, dv) = surface.derivatives_at(uv);
    let gap = x - here.0;
    // Past an edge: at it, with the point still lying outward along it.
    let past = |at: f64, lo: f64, hi: f64, period: Option<f64>, t: Vec3| {
        period.is_none()
            && ((at <= lo && gap.dot(t) < 0.0) || (at >= hi && gap.dot(t) > 0.0))
            && gap.dot(t).abs() > 1e-12 * t.length().max(1e-300)
    };
    if !past(uv.u, d.u.lo, d.u.hi, d.u_period, du) && !past(uv.v, d.v.lo, d.v.hi, d.v_period, dv) {
        return here;
    }
    let frame = |q: Vec2| carried_with_slopes(surface, &d, q);
    // Which way is downhill from `q`, and how far the point is from there.
    let slope = |q: Vec2| {
        let (p, pu, pv) = frame(q);
        let off = p - x;
        (off.dot(pu), off.dot(pv), off.length_squared(), pu, pv)
    };
    // Newton proper, with the way the slope itself changes taken by
    // difference. The step that treats the surface as flat is not enough
    // here: a point on an offset stands off its surface by the offset, which
    // for a blend is the ball's radius, and where the surface curves about as
    // tightly as that the flat step closes a fraction of the gap each round
    // and is still microns out when it gives up — a kink in the reading
    // exactly where a curve runs off the end of a patch.
    let (ku, kv) = (d.u.span().abs() * 1e-4, d.v.span().abs() * 1e-4);
    let mut q = uv;
    let mut reach = gap.length_squared();
    for _ in 0..40 {
        let (e, f, _, pu, pv) = slope(q);
        let (east, west) = (slope(Vec2::new(q.u + ku, q.v)), slope(Vec2::new(q.u - ku, q.v)));
        let (north, south) = (slope(Vec2::new(q.u, q.v + kv)), slope(Vec2::new(q.u, q.v - kv)));
        let mut a = (east.0 - west.0) / (2.0 * ku);
        let mut c = (north.1 - south.1) / (2.0 * kv);
        let mut b = 0.5 * ((north.0 - south.0) / (2.0 * kv) + (east.1 - west.1) / (2.0 * ku));
        if !(a > 0.0 && c > 0.0 && a * c - b * b > 0.0) {
            (a, b, c) = (pu.dot(pu), pu.dot(pv), pv.dot(pv));
        }
        let det = a * c - b * b;
        if det.abs() < 1e-300 {
            break;
        }
        let (su, sv) = ((e * c - f * b) / det, (a * f - b * e) / det);
        // Only ever nearer.
        let mut scale = 1.0;
        let mut moved = false;
        for _ in 0..12 {
            let trial = Vec2::new(q.u - su * scale, q.v - sv * scale);
            let there = (carried(surface, &d, trial) - x).length_squared();
            if there <= reach {
                moved = trial.u != q.u || trial.v != q.v;
                q = trial;
                reach = there;
                break;
            }
            scale *= 0.5;
        }
        if !moved
            || ((su * scale).abs() <= 1e-15 * d.u.span().abs().max(1e-300)
                && (sv * scale).abs() <= 1e-15 * d.v.span().abs().max(1e-300))
        {
            break;
        }
    }
    let (p, pu, pv) = frame(q);
    match pu.cross(pv).try_normalized() {
        // The same way round as the patch's own normal at its edge.
        Some(n) if p.is_finite() => (p, if n.dot(here.1) < 0.0 { -n } else { n }),
        _ => here,
    }
}

impl Blend {
    /// Where the ball's centre is when the ball's own section passes through
    /// `x`, and which way the centre line runs there.
    ///
    /// The centre line is where the centre is the stated distance from both
    /// surfaces at once, so it runs along the cross product of their two
    /// directions; the section through `x` is the one whose plane, square to
    /// that, contains `x`. Three conditions on the centre's three coordinates.
    pub fn centre_for(&self, x: Vec3) -> Option<(Vec3, Vec3)> {
        if let Some(found) = self.centre_on_line(x) {
            self.centre.set(Some(found.0));
            return Some(found);
        }
        // A centre found for a neighbouring point is the best place to start,
        // if it is anywhere near; otherwise the file's own samples are.
        let near_enough = |c: Vec3| (c - x).length() <= (self.radius * 3.0).max(STRIDE);
        let found = match self.centre.get().filter(|c| near_enough(*c)) {
            Some(seed) => self.settle(seed, x),
            None => None,
        }
        .or_else(|| self.settle(nearest_on(&self.spine, x)?, x))?;
        self.centre.set(Some(found.0));
        Some(found)
    }

    /// The blend's centre line as a curve, computed once.
    fn laid_line(&self) -> Option<&cad_ir::brep::Curve> {
        if self.laid.line.get().is_none() {
            if self.laid.under_way.replace(true) {
                return None;
            }
            let (f, g) = self.offsets();
            let line = self
                .line(&f, &g)
                .and_then(|stations| cubic(&stations))
                .map(cad_ir::brep::Curve::Nurbs);
            let _ = self.laid.line.set(line);
            self.laid.under_way.set(false);
        }
        self.laid.line.get()?.as_ref()
    }

    /// [`Blend::centre_for`], by way of the centre line itself.
    ///
    /// The section through a point is the one centred where the centre line
    /// passes nearest to it, so with the line in hand the centre is found by
    /// looking along one curve — and then put exactly on the two offsets, in
    /// the plane square to the line there, as any point of a curve is.
    ///
    /// Solving for the centre directly asks three things of it at once: the
    /// radius from each surface, and the point abeam. The third is the weak
    /// one. A point at the centre of a bend in the line is abeam of every
    /// centre on the bend — a ball sitting in the hole of a blend that runs
    /// round a boss touches it all the way round — and near such a place the
    /// solve has nothing to hold on to, while the *distance* to the line,
    /// which is all a reading wants, has hardly changed at all. Two blends of
    /// the pilot assembly end exactly there.
    ///
    /// `None` where the line did not compute or the point lies off the end of
    /// it; the caller then solves as it always has.
    fn centre_on_line(&self, x: Vec3) -> Option<(Vec3, Vec3)> {
        let line = self.laid_line()?;
        let range = line.natural_range();
        let span = range.span();
        if !(span > 0.0) {
            return None;
        }
        // From where the last reading was, a few steps of Newton; from
        // nothing, the whole line is looked along.
        let lean = |t: f64| {
            let (at, d) = (line.point_at(t), line.derivative_at(t));
            ((at - x).dot(d), d.length_squared(), at, d)
        };
        let nearby = |from: f64| -> Option<f64> {
            let mut t = from;
            for _ in 0..12 {
                let (g, speed, at, _) = lean(t);
                if !(speed > 0.0) {
                    return None;
                }
                let h = span * 1e-6;
                let curl = (line.derivative_at(t + h) - line.derivative_at(t - h)) * (0.5 / h);
                let slope = speed + (at - x).dot(curl);
                // Abeam already — or abeam of everything, which is the same.
                if g.abs() <= 1e-13 * speed.sqrt() * (1.0 + (at - x).length()) {
                    return Some(t);
                }
                if !(slope > speed * 1e-9) {
                    return None;
                }
                // No further in one go than a reading is followed.
                let step = (g / slope).clamp(-STRIDE / speed.sqrt(), STRIDE / speed.sqrt());
                let next = (t - step).clamp(range.lo, range.hi);
                if (next - t).abs() <= 1e-15 * span {
                    return Some(next);
                }
                t = next;
            }
            let (g, speed, at, _) = lean(t);
            (g.abs() <= 1e-9 * speed.sqrt() * (1.0 + (at - x).length())).then_some(t)
        };
        let t = self
            .along
            .get()
            .and_then(nearby)
            .or_else(|| line.param_of(x, None))?;
        let (g, speed, on_line, d) = lean(t);
        let along = d.try_normalized()?;
        // Off the end of the line: no section of what was laid passes
        // through the point.
        let at_end = (t - range.lo).abs() <= 1e-12 * span || (range.hi - t).abs() <= 1e-12 * span;
        if at_end && g.abs() > 1e-9 * speed.sqrt() * (1.0 + (on_line - x).length()) {
            return None;
        }
        self.along.set(Some(t));
        // The laid line is the centre line to within the resolution, and
        // abeam of the point as the *laid* line runs — a cubic whose
        // direction is the true line's to a few parts in a hundred thousand.
        // For a point a ball's radius away that is a few hundredths of a
        // nanometre of distance, which is nothing to a reading and too much
        // to a boundary of a blend, which is found from the difference of two
        // readings a little way apart. So the centre is settled from here as
        // it always was — on both offsets, abeam as the line itself runs —
        // and the laid line's part in it is that the settling starts beside
        // the answer instead of somewhere along the chart.
        if let Some(settled) = self.settle(on_line, x)
            && (settled.0 - on_line).length() <= 1e-5
        {
            return Some(settled);
        }
        // Where it will not settle, or settles somewhere else, the point is
        // at the centre of a bend in the line: abeam of all of it, and as
        // near abeam here as anywhere. The centre is then only put on the
        // two offsets, in the plane square to the laid line.
        let mut c = on_line;
        for _ in 0..8 {
            let (Some(a), Some(b)) = (self.a.read_near(c, self.ra), self.b.read_near(c, self.rb)) else {
                return Some((on_line, along));
            };
            let Some(step) = solve(
                [a.uphill, b.uphill, along],
                [self.ra - a.distance, self.rb - b.distance, (on_line - c).dot(along)],
            ) else {
                return Some((on_line, along));
            };
            c = c + step;
            // Further than the line can be out, and it is not being put on
            // the line any more.
            if (c - on_line).length() > 1e-6 {
                return Some((on_line, along));
            }
            if step.length() <= 1e-13 * (1.0 + c.length()) {
                break;
            }
        }
        Some((c, along))
    }

    fn settle(&self, seed: Vec3, x: Vec3) -> Option<(Vec3, Vec3)> {
        let mut c = seed;
        // How much of a step along the centre line the section's lean is
        // worth. One for a straight line. On a line that curves, the section
        // planes fan out from its centre of curvature, and a point a radius
        // off the line on the outside of the bend swings past twice as fast
        // as the centre moves when the bend's own radius is the ball's: taken
        // as one, each step there overshoots by exactly as much as it was
        // short, and the solve swaps two wrong answers for ever — which is
        // what a 60 mm blend round a mannequin's hip did. It is measured
        // instead, from what the last step bought.
        let mut worth = 1.0f64;
        let mut before: Option<(f64, f64)> = None;
        for _ in 0..64 {
            let (a, b) = (self.a.read_near(c, self.ra)?, self.b.read_near(c, self.rb)?);
            let along = a.uphill.cross(b.uphill).try_normalized()?;
            let lean = (x - c).dot(along);
            if let Some((was, moved)) = before
                && moved.abs() > 1e-15
            {
                let seen = (was - lean) / moved;
                if seen.is_finite() {
                    worth = seen.clamp(0.05, 20.0);
                }
            }
            let step = solve(
                [a.uphill, b.uphill, along],
                [self.ra - a.distance, self.rb - b.distance, lean / worth],
            )?;
            // A step longer than the ball is not a correction, it is a leap.
            let length = step.length();
            let most = if self.radius > 0.0 { self.radius } else { STRIDE };
            let step = if length > most { step * (most / length) } else { step };
            before = Some((lean, step.dot(along)));
            c = c + step;
            if length <= 1e-13 * (1.0 + c.length()) {
                return Some((c, along));
            }
        }
        // Slowly is still an answer if it is one; anything else is not.
        let (a, b) = (self.a.read_near(c, self.ra)?, self.b.read_near(c, self.rb)?);
        let along = a.uphill.cross(b.uphill).try_normalized()?;
        let there = (a.distance - self.ra).abs() <= RESOLUTION * 1e-2
            && (b.distance - self.rb).abs() <= RESOLUTION * 1e-2
            && (x - c).dot(along).abs() <= RESOLUTION * 1e-2;
        there.then_some((c, along))
    }

    /// The two points at which the ball centred on `centre` touches the
    /// surfaces it blends.
    pub fn contacts(&self, centre: Vec3) -> Option<(Vec3, Vec3)> {
        let (a, b) = (
            self.a.read_near(centre, self.ra)?,
            self.b.read_near(centre, self.rb)?,
        );
        Some((centre - a.uphill * a.distance, centre - b.uphill * b.distance))
    }
}

/// One cross-section of a blend: the poles of the circular arc it is, each
/// with the weight that makes the quadratics through them a circle exactly.
type Rib = Vec<[f64; 4]>;

/// A weighted sum of ribs.
fn rib_sum(terms: &[(&Rib, f64)]) -> Rib {
    let mut out = vec![[0.0; 4]; terms[0].0.len()];
    for (rib, scale) in terms {
        for (o, r) in out.iter_mut().zip(rib.iter()) {
            for k in 0..4 {
                o[k] += r[k] * scale;
            }
        }
    }
    out
}

fn rib_point(h: &[f64; 4]) -> Vec3 {
    Vec3::new(h[0] / h[3], h[1] / h[3], h[2] / h[3])
}

/// The point half way round a rib.
fn rib_crown(rib: &Rib) -> Vec3 {
    let pieces = (rib.len() - 1) / 2;
    if pieces % 2 == 0 {
        // An even number of pieces meet at the middle.
        return rib_point(&rib[pieces]);
    }
    let k = pieces - 1;
    let mut h = [0.0; 4];
    for i in 0..4 {
        h[i] = 0.25 * rib[k][i] + 0.5 * rib[k + 1][i] + 0.25 * rib[k + 2][i];
    }
    rib_point(&h)
}

/// A section of a blend with how it changes along the centre line.
#[derive(Clone)]
struct Frame {
    /// Where the ball's centre is, and which way the centre line runs.
    station: Station,
    rib: Rib,
    /// The rib's rate of change per unit of its own travel: of how far its
    /// fastest pole moves, or the centre if that moves further.
    turn: Rib,
}

/// How far the furthest-moving pole of one rib is from where it is in
/// another.
fn rib_travel(a: &Rib, b: &Rib) -> f64 {
    a.iter()
        .zip(b)
        .map(|(p, q)| (rib_point(p) - rib_point(q)).length())
        .fold(0.0, f64::max)
}

/// How far the surface moves between two sections: the furthest any one of
/// their poles does, or their centres if that is further.
fn travel(a: &Frame, b: &Frame) -> f64 {
    rib_travel(&a.rib, &b.rib).max((b.station.at - a.station.at).length())
}

/// The point nearest `x` on the cubic that leaves `a` and arrives at `b`,
/// each in its own direction. (`t` is where the caller expects it, and is
/// not needed: the whole span is looked along.)
///
/// The stations of a centre line are the centre line to within
/// [`RESOLUTION`], so the distance to this is the distance to the centre
/// line, with nothing to solve but one number.
fn foot_on_cubic(a: &Station, b: &Station, x: Vec3, t: f64) -> Vec3 {
    let length = (b.at - a.at).length();
    let p = [
        a.at,
        a.at + a.along * (length / 3.0),
        b.at - b.along * (length / 3.0),
        b.at,
    ];
    let at = |t: f64| {
        let s = 1.0 - t;
        let point = p[0] * (s * s * s) + p[1] * (3.0 * s * s * t) + p[2] * (3.0 * s * t * t) + p[3] * (t * t * t);
        let first = (p[1] - p[0]) * (3.0 * s * s) + (p[2] - p[1]) * (6.0 * s * t) + (p[3] - p[2]) * (3.0 * t * t);
        let second = (p[2] - p[1] * 2.0 + p[0]) * (6.0 * s) + (p[3] - p[2] * 2.0 + p[1]) * (6.0 * t);
        (point, first, second)
    };
    // Closed in on from both ends of the span before Newton is let near it.
    // A point of the blend is a whole radius from the line, and a span of
    // the line may be a few hundredths of a millimetre: seen from that far,
    // the least bend in so short a cubic — and at an end where two surfaces
    // come to touch, the line's direction is only as good as a chord — makes
    // the distance nearly flat along it, and Newton started anywhere but on
    // the answer leaves the span altogether.
    let off = |t: f64| (at(t).0 - x).length_squared();
    const GOLDEN: f64 = 0.618_033_988_749_894_9;
    let (mut lo, mut hi) = (0.0f64, 1.0f64);
    let (mut p, mut q) = (hi - GOLDEN * (hi - lo), lo + GOLDEN * (hi - lo));
    let (mut at_p, mut at_q) = (off(p), off(q));
    for _ in 0..40 {
        if at_p <= at_q {
            hi = q;
            (q, at_q) = (p, at_p);
            p = hi - GOLDEN * (hi - lo);
            at_p = off(p);
        } else {
            lo = p;
            (p, at_p) = (q, at_q);
            q = lo + GOLDEN * (hi - lo);
            at_q = off(q);
        }
    }
    let closed_in = if at_p <= at_q { p } else { q };
    let _ = t;
    let mut t = closed_in;
    for _ in 0..6 {
        let (point, first, second) = at(t);
        let out = point - x;
        let slope = first.dot(first) + out.dot(second);
        if !(slope > 0.0) {
            break;
        }
        let next = (t - out.dot(first) / slope).clamp(0.0, 1.0);
        if (next - t).abs() <= 1e-14 {
            t = next;
            break;
        }
        t = next;
    }
    if off(t) > off(closed_in) {
        t = closed_in;
    }
    at(t).0
}

impl Blend {
    /// The two surfaces the ball's centre rides on: each blended surface,
    /// stood off by the ball's radius.
    fn offsets(&self) -> (Field, Field) {
        let f = Field::Offset { base: Box::new(self.a.clone()), by: self.ra };
        let g = Field::Offset { base: Box::new(self.b.clone()), by: self.rb };
        f.forget();
        g.forget();
        (f, g)
    }

    /// The ball's centre line, as the stations of a cubic.
    ///
    /// The line is where the centre is the radius from both surfaces: the
    /// meeting of the two offsets, which is a curve like any other here.
    pub fn centre_line(&self) -> Option<Vec<Station>> {
        let (f, g) = self.offsets();
        self.line(&f, &g)
    }

    /// The centre line to its own ends, or failing that as far as its chart —
    /// or, failing that, as far as its chart can be read.
    ///
    /// A chart runs a little past the faces it serves, and a blend rolled
    /// against another can run out exactly where the other does. The last
    /// few microns of its chart are then points whose section of the other
    /// blend lies past that blend's end — and, where the ball has come to
    /// sit at the centre of the other's bend, a section that runs away along
    /// it without limit. Such a point is not on a surface that is there to
    /// be read, and the line is taken up to the last point that is. Whether
    /// that is far enough is for whoever wanted the line: see
    /// [`Blend::reaches`].
    fn line(&self, f: &Field, g: &Field) -> Option<Vec<Station>> {
        let chart = &self.spine[self.charted.clone()];
        stations(f, g, &self.spine)
            .or_else(|| (chart.len() < self.spine.len()).then(|| stations(f, g, chart)).flatten())
            .or_else(|| {
                (1..=4usize)
                    .filter(|short| chart.len() >= short + 2)
                    .flat_map(|short| [&chart[..chart.len() - short], &chart[short..]])
                    .find_map(|part| stations(f, g, part))
            })
    }

    /// Whether a centre line runs as far as every point of `reach`: no point
    /// lies beyond an end of it.
    fn reaches(&self, line: &[Station], reach: &[Vec3]) -> bool {
        let closed = line.len() > 2 && (line[line.len() - 1].at - line[0].at).length() <= RESOLUTION;
        if closed || line.len() < 2 {
            return true;
        }
        let within = (self.radius * 4.0).powi(2);
        // A boundary is where its own curve puts it, to the resolution; past
        // an end by more than a tenth of a micron it is past the end.
        const SHORT: f64 = 1e-7;
        for (end, outward) in [(&line[0], -1.0), (&line[line.len() - 1], 1.0)] {
            if self.closes_at(end) {
                continue;
            }
            for p in reach {
                let from_end = (*p - end.at).length_squared();
                let lean = (*p - end.at).dot(end.along) * outward;
                if from_end > within || lean <= SHORT {
                    continue;
                }
                if line.iter().all(|s| (s.at - *p).length_squared() >= from_end * (1.0 - 1e-12)) {
                    return false;
                }
            }
        }
        true
    }

    /// The directions from the ball's centre at `st` to its two contacts.
    ///
    /// The centre is on both offsets, so each contact is straight down the
    /// offset's own direction from it: nothing is solved for here, only read.
    fn spokes(&self, f: &Field, g: &Field, st: &Station) -> Option<(Vec3, Vec3)> {
        f.recall(&st.memory.0);
        g.recall(&st.memory.1);
        let (a, b) = (f.read(st.at)?, g.read(st.at)?);
        Some((a.uphill * -self.ra.signum(), b.uphill * -self.rb.signum()))
    }

    /// The section whose ball is centred at `st`, in `pieces` quadratics.
    ///
    /// An arc of a circle is a rational quadratic: its middle pole is where
    /// the tangents at its two ends cross, weighted by the cosine of half the
    /// angle between them. A ball in a corner touches the two sides less than
    /// half a turn apart — a half turn less the corner's own angle — so the
    /// arc between the contacts is the short one; it is still cut in two
    /// where it is wide, because the middle pole runs off to infinity as the
    /// arc nears the half turn.
    fn rib(&self, f: &Field, g: &Field, st: &Station, pieces: usize) -> Option<Rib> {
        let (ua, ub) = self.spokes(f, g, st)?;
        let cos = ua.dot(ub).clamp(-1.0, 1.0);
        let opening = cos.acos();
        // A quarter turn on from the first contact, towards the second.
        // Where the two contacts are one point there is no "towards", and the
        // section before says which side the arc was opening on.
        let side = st.along.cross(ua);
        let toward = ub - ua * cos;
        let quarter = if toward.length() > 1e-7 {
            let quarter = toward.try_normalized()?;
            self.way.set(if side.dot(quarter) >= 0.0 { 1.0 } else { -1.0 });
            quarter
        } else {
            side.try_normalized()? * self.way.get()
        };
        let step = opening / pieces as f64;
        if step >= 0.95 * std::f64::consts::PI {
            return None;
        }
        let weight = (step * 0.5).cos();
        let spoke = |angle: f64| ua * angle.cos() + quarter * angle.sin();
        let on = |dir: Vec3| st.at + dir * self.radius;
        let mut rib = Vec::with_capacity(2 * pieces + 1);
        let first = on(ua);
        rib.push([first.x, first.y, first.z, 1.0]);
        for k in 0..pieces {
            let (from, to) = (spoke(step * k as f64), spoke(step * (k + 1) as f64));
            // Where the tangents at the two ends of this piece cross.
            let corner = st.at + (from + to) * (self.radius / (1.0 + step.cos()));
            rib.push([corner.x * weight, corner.y * weight, corner.z * weight, weight]);
            let end = on(to);
            rib.push([end.x, end.y, end.z, 1.0]);
        }
        Some(rib)
    }

    /// Whether `st` is the line's own end rather than a point of its chart:
    /// the place the two blended surfaces come to touch and the blend closes
    /// to nothing. See [`Fields::charted`].
    fn closes_at(&self, st: &Station) -> bool {
        let is = |p: Option<&Vec3>| p.is_some_and(|p| (*p - st.at).length() <= 1e-12);
        (self.charted.start > 0 && is(self.spine.first()))
            || (self.charted.end < self.spine.len() && is(self.spine.last()))
    }

    /// The section where the blend closes: every pole at the one point the
    /// ball touches both surfaces at.
    ///
    /// Read off the first surface alone. At such a place the second contact
    /// is the first, and when the second surface is itself a blend the ball
    /// has come to sit where that blend's own sections all meet — it is
    /// touched all the way round, and which way "towards the contact" is
    /// there, the surface cannot say.
    fn closed_rib(&self, f: &Field, g: &Field, st: &Station, pieces: usize) -> Option<Rib> {
        f.recall(&st.memory.0);
        g.recall(&st.memory.1);
        let at = match f.read(st.at) {
            Some(a) => st.at - a.uphill * self.ra,
            None => st.at - g.read(st.at)?.uphill * self.rb,
        };
        Some(vec![[at.x, at.y, at.z, 1.0]; 2 * pieces + 1])
    }

    /// The centre a little way along the line from `st`: `by` forward, or
    /// back if it is negative.
    fn beside(&self, f: &Field, g: &Field, st: &Station, by: f64) -> Option<Station> {
        f.recall(&st.memory.0);
        g.recall(&st.memory.1);
        let at = meet(f, g, st.at + st.along * by, st.along, by.abs() * 4.0)?;
        Some(Station {
            at,
            along: heading(f, g, at, st.along),
            memory: Rc::new((f.memory(), g.memory())),
        })
    }

    /// The section at `st` and how fast it is changing.
    ///
    /// The rate is taken between two sections a little either side: near
    /// enough that their difference is the rate, far enough that it is not
    /// noise. `h` is how far along the centre line that is to begin with.
    ///
    /// The rate is kept *per unit of the surface's own travel*, not of the
    /// centre's. Along most of a blend the two are the same thing. They part
    /// company where a blend closes: the ball's two contacts come together
    /// as the square root of what is left of the centre line, so against the
    /// centre's travel the sections change without limit, and no cubic in
    /// that follows them — a span there was halved down to a tenth of a
    /// nanometre and still did not hold. Against the travel of the fastest
    /// point of the section itself they change at an ordinary rate all the
    /// way to the end.
    fn frame(&self, f: &Field, g: &Field, st: &Station, pieces: usize, h: f64) -> Option<Frame> {
        let rib = self.rib(f, g, st, pieces)?;
        let side = |by: f64| -> Option<(f64, Rib)> {
            let next = self.beside(f, g, st, by)?;
            Some(((next.at - st.at).dot(st.along), self.rib(f, g, &next, pieces)?))
        };
        let rate = |far: &Rib, near: &Rib, ds: f64| {
            (ds.abs() > 1e-12).then(|| rib_sum(&[(far, 1.0 / ds), (near, -1.0 / ds)]))
        };
        // The two sections are taken closer wherever the surface moves a
        // long way for a short step of the centre: a hundredth of a
        // millimetre of the surface apart, whatever that is of the line.
        let mut h = h;
        let mut sides = (side(h), side(-h));
        for _ in 0..8 {
            let moved = match &sides {
                (Some((_, ahead)), Some((_, behind))) => rib_travel(ahead, behind),
                (Some((_, one)), None) | (None, Some((_, one))) => rib_travel(one, &rib),
                (None, None) => 0.0,
            };
            if moved <= 2e-5 || h <= 1e-11 {
                break;
            }
            h *= (1e-5 / moved).max(0.05);
            sides = (side(h), side(-h));
        }
        // Neither side, at a section that is itself there, is the end of a
        // blend that closes: one way is past the end, and the other is too
        // close to where its two contacts become one for the surfaces to say
        // which way is which. Nearer still there may be a section; and if
        // there is none, the section is given no rate at all — it is still
        // exactly where it is, and what lies between it and the next is
        // checked against the blend like every other span.
        let mut tries = 0;
        while matches!(sides, (None, None)) && tries < 4 {
            h *= 0.125;
            sides = (side(h), side(-h));
            tries += 1;
        }
        let along_line = match sides {
            (Some((sp, rp)), Some((sm, rm))) => rate(&rp, &rm, sp - sm)?,
            // At an end of the line only one side may exist.
            (Some((s, r)), None) | (None, Some((s, r))) => rate(&r, &rib, s)?,
            (None, None) => rib_sum(&[(&rib, 0.0)]),
        };
        // How fast the fastest pole moves for the centre's unit: at least
        // the centre's own one.
        let speed = rib
            .iter()
            .zip(&along_line)
            .map(|(pole, rate)| {
                let at = rib_point(pole);
                (Vec3::new(rate[0], rate[1], rate[2]) - at * rate[3]).length() / pole[3]
            })
            .fold(1.0, f64::max);
        let turn = rib_sum(&[(&along_line, 1.0 / speed)]);
        Some(Frame { station: st.clone(), rib, turn })
    }

    /// Whether the cubic through two frames is the blend between them.
    ///
    /// Its two edges have to be on the two surfaces, and edges and crown
    /// alike the ball's radius from the centre line. A cubic that agrees with
    /// a curve at both ends, in place and direction, is furthest from it in
    /// the middle — if the curve is smooth. A blend over a surface made of
    /// patches is not, quite: its sections change their rate of change where
    /// the ball crosses from one patch to the next, and a span with such a
    /// place in it errs to one side of the middle. So it is asked at the
    /// quarters as well.
    fn holds(&self, f: &Field, g: &Field, a: &Frame, b: &Frame) -> Option<bool> {
        // Held to half of what is claimed: three places asked are not every
        // place, and a span that is just inside the resolution at its
        // quarters is just outside it between them.
        const WITHIN: f64 = RESOLUTION * 0.5;
        let length = travel(a, b);
        for t in [0.5, 0.25, 0.75] {
            let s = 1.0 - t;
            let section = rib_sum(&[
                (&a.rib, s * s * (1.0 + 2.0 * t)),
                (&b.rib, t * t * (1.0 + 2.0 * s)),
                (&a.turn, s * s * t * length),
                (&b.turn, -t * t * s * length),
            ]);
            let first = rib_point(&section[0]);
            let last = rib_point(&section[section.len() - 1]);
            let crown = rib_crown(&section);
            for x in [first, last, crown] {
                let centre = foot_on_cubic(&a.station, &b.station, x, t);
                if ((x - centre).length() - self.radius).abs() > WITHIN {
                    return Some(false);
                }
            }
            // Read from the contact next door, not from the centre a whole
            // radius away: the edge is on the surface, and so is the contact.
            let near = if t < 0.6 { &a.station } else { &b.station };
            f.recall(&near.memory.0);
            g.recall(&near.memory.1);
            f.to_foot();
            g.to_foot();
            let on_a = (f.read(first)?.distance + self.ra).abs();
            let on_b = (g.read(last)?.distance + self.rb).abs();
            if on_a > WITHIN || on_b > WITHIN {
                return Some(false);
            }
        }
        Some(true)
    }

    /// How far the surface built for this blend is from being the blend,
    /// asked at points it was not built from: the worst of how far its points
    /// are from the ball's radius off the centre line, how far the centre
    /// line there is off the two offsets it is the meeting of, and how far
    /// the surface's two edges are off the two surfaces blended.
    ///
    /// `along` is how many of its spans are asked, and `across` how many
    /// places across each.
    pub fn departure(&self, reach: &[Vec3], along: usize, across: usize) -> Option<f64> {
        let (f, g) = self.offsets();
        let (surface, line) = self.sweep(&f, &g, reach)?;
        let knots: Vec<f64> = surface.u_knots[3..].iter().step_by(3).copied().collect();
        let surface = Surface::Nurbs(surface);
        let spans = line.len() - 1;
        let mut worst = 0.0f64;
        for i in 0..along.min(spans) {
            let k = i * spans / along.min(spans);
            let (a, b) = (&line[k], &line[k + 1]);
            // Neither end of the span, nor its middle, which is where the
            // surface was checked as it was built.
            for t in [0.19, 0.81] {
                let u = knots[k] + (knots[k + 1] - knots[k]) * t;
                for j in 0..=across {
                    let v = j as f64 / across as f64;
                    let q = surface.point_at(Vec2::new(u, v));
                    let centre = foot_on_cubic(a, b, q, t);
                    worst = worst.max(((q - centre).length() - self.radius).abs());
                    f.recall(&a.memory.0);
                    g.recall(&a.memory.1);
                    if std::env::var_os("XT_EXACT_PROBE").is_some() {
                        let radius = ((q - centre).length() - self.radius).abs();
                        let (on_f, on_g) = (f.read(centre)?.distance.abs(), g.read(centre)?.distance.abs());
                        if radius.max(on_f).max(on_g) > RESOLUTION {
                            eprintln!(
                                "[exact] departure: span {k} of {spans} ({:.3e} of centre, {:.3e} of parameter), t {t}, v {v:.3}: {radius:.2e} off the radius, centre {on_f:.2e} {on_g:.2e} off the offsets",
                                (b.at - a.at).length(),
                                knots[k + 1] - knots[k]
                            );
                        }
                        f.recall(&a.memory.0);
                        g.recall(&a.memory.1);
                    }
                    worst = worst.max(f.read(centre)?.distance.abs()).max(g.read(centre)?.distance.abs());
                    if j == 0 {
                        f.to_foot();
                        worst = worst.max((f.read(q)?.distance + self.ra).abs());
                    } else if j == across {
                        g.to_foot();
                        worst = worst.max((g.read(q)?.distance + self.rb).abs());
                    }
                }
            }
        }
        Some(worst)
    }

    /// The blend as a surface: a cubic along the centre line through its
    /// sections, each section the circular arc it is.
    ///
    /// Along the line the sections are computed, not sampled from anything —
    /// each is the ball at that centre and its two contacts — and between two
    /// of them the surface is the cubic that leaves each as fast as the
    /// section there is really changing. It is checked half way between, on
    /// both edges and on the crown, against the blend's own definition, and
    /// a section is added wherever it is not yet that to within
    /// [`RESOLUTION`].
    ///
    /// The first row of poles is the track on the first surface and the last
    /// the track on the second, so the blend's two boundaries are curves of
    /// this surface as well as of the two it blends.
    ///
    /// `reach` are points of the faces that lie on the blend. The file's chart
    /// of a centre line is a sample of it and need not run its whole length:
    /// where a face reaches past the chart's end, the line is carried on to
    /// it.
    pub fn surface(&self, reach: &[Vec3]) -> Option<cad_ir::brep::NurbsSurface> {
        // A blend of no radius is an edge, and has no surface.
        if !(self.radius > 0.0) {
            return None;
        }
        let (f, g) = self.offsets();
        Some(self.sweep(&f, &g, reach)?.0)
    }

    /// The centre line carried on past the ends of its chart, as far as the
    /// points of `reach` that lie beyond them.
    ///
    /// A point lies beyond an end when that end's centre is the nearest to it
    /// of all the line's and it is still ahead of that centre's section. The
    /// line is then walked on, the way the curve between two charted points
    /// is found, until the point is abeam or the line gives out — as it does
    /// where a blend closes to nothing between two surfaces that come to
    /// touch.
    fn carried_to(&self, f: &Field, g: &Field, mut line: Vec<Station>, reach: &[Vec3]) -> Vec<Station> {
        let closed = line.len() > 2 && (line[line.len() - 1].at - line[0].at).length() <= RESOLUTION;
        if reach.is_empty() || closed {
            return line;
        }
        let within = (self.radius * 4.0).powi(2);
        for onward in [false, true] {
            let outward = if onward { 1.0 } else { -1.0 };
            for _ in 0..256 {
                let Some(end) = (if onward { line.last() } else { line.first() }) else { break };
                // Where the blend closes, the line has ended, whatever lies
                // ahead of its last section: the sections there all but share
                // a centre, and a point of the blend a whole radius away is
                // "ahead" of it by where it happens to be on the ball.
                if self.closes_at(end) {
                    break;
                }
                let mut ahead = 0.0f64;
                for p in reach {
                    let from_end = (*p - end.at).length_squared();
                    let lean = (*p - end.at).dot(end.along) * outward;
                    if from_end > within || lean <= ahead {
                        continue;
                    }
                    if line.iter().all(|s| (s.at - *p).length_squared() >= from_end * (1.0 - 1e-12)) {
                        ahead = lean;
                    }
                }
                if ahead <= RESOLUTION {
                    break;
                }
                let step = ahead.min(STRIDE);
                let probe = std::env::var_os("XT_EXACT_PROBE").is_some();
                let Some(next) = self.beside(f, g, end, step * outward) else {
                    if probe {
                        eprintln!("[exact] blend r={:.4}: a face reaches {ahead:.3e} past the {} of the centre line, and the line will not go on", self.radius, if onward { "end" } else { "start" });
                    }
                    break;
                };
                // Further on, or it is not the line going on.
                if (next.at - end.at).dot(end.along) * outward < step * 0.5 {
                    if probe {
                        eprintln!("[exact] blend r={:.4}: a face reaches {ahead:.3e} past the {} of the centre line, and the next centre is not further on", self.radius, if onward { "end" } else { "start" });
                    }
                    break;
                }
                if onward {
                    line.push(next);
                } else {
                    line.insert(0, next);
                }
            }
        }
        line
    }

    /// [`Blend::surface`], with the centres of the sections it was built
    /// through.
    ///
    /// The line is taken as far as it will go — to its own ends, and on to
    /// where its faces reach — and the surface is built along all of it if it
    /// will be; if not, along what the file charted, which is what there was
    /// before any of the rest was asked for.
    fn sweep(&self, f: &Field, g: &Field, reach: &[Vec3]) -> Option<(cad_ir::brep::NurbsSurface, Vec<Station>)> {
        let probe = std::env::var_os("XT_EXACT_PROBE").is_some();
        let Some(line) = self.line(f, g) else {
            if probe {
                eprintln!("[exact] blend: the centre line did not compute");
            }
            return None;
        };
        let carried = self.carried_to(f, g, line.clone(), reach);
        if probe && carried.len() != line.len() {
            eprintln!("[exact] blend: centre line carried {} sections past its chart", carried.len() - line.len());
        }
        // A surface that stops short of a face that lies on it is not that
        // face's surface, and is not offered as one.
        if !self.reaches(&carried, reach) {
            if probe {
                eprintln!("[exact] blend: the centre line stops short of a face on the blend");
            }
            return None;
        }

        let further = carried.len() != line.len();
        self.swept(f, g, carried)
            .or_else(|| {
                (further && self.reaches(&line, reach))
                    .then(|| self.swept(f, g, line))
                    .flatten()
            })
            .or_else(|| {
                (self.charted.len() < self.spine.len())
                    .then(|| stations(f, g, &self.spine[self.charted.clone()]))
                    .flatten()
                    .filter(|chart| self.reaches(chart, reach))
                    .and_then(|chart| self.swept(f, g, chart))
            })
    }

    /// The surface through the sections centred along `line`.
    fn swept(&self, f: &Field, g: &Field, line: Vec<Station>) -> Option<(cad_ir::brep::NurbsSurface, Vec<Station>)> {
        let probe = std::env::var_os("XT_EXACT_PROBE").is_some();
        // How wide the widest section opens says how many pieces every
        // section is cut into; they all have to be cut alike to be joined.
        let mut widest = 0.0f64;
        for st in &line {
            let (ua, ub) = self.spokes(f, g, st)?;
            widest = widest.max(ua.dot(ub).clamp(-1.0, 1.0).acos());
        }
        let pieces = if widest > 0.55 * std::f64::consts::PI { 2 } else { 1 };
        if probe {
            eprintln!(
                "[exact] blend: centre line of {} stations, sections up to {:.1} degrees, in {pieces} piece(s)",
                line.len(),
                widest.to_degrees()
            );
        }
        // A hundredth of a millimetre either side of a section is how its
        // rate of change is taken — on a centre line that is long enough to
        // have one. Where three blends meet at a corner the ball does not
        // travel, it turns: its centre moves two microns while its contacts
        // swing a millimetre round it, and a step of ten microns along a
        // line two long is a step off the end of it. There the step is a
        // fraction of the line.
        let run: f64 = line.windows(2).map(|w| (w[1].at - w[0].at).length()).sum();
        let h = (run / 64.0).min(1e-5);
        if !(h > 1e-11) {
            return None;
        }
        let mut frames: Vec<Frame> = Vec::with_capacity(line.len() * 2);
        for st in &line {
            if self.closes_at(st) {
                // Where the blend closes its section is a point. Read as any
                // other section it comes out as one if the surfaces can still
                // say where the two contacts are; where they cannot — what it
                // comes out as is wider than a point could be — it is put
                // there outright.
                let read = self.frame(f, g, st, pieces, h).filter(|frame| {
                    let (first, last) = (rib_point(&frame.rib[0]), rib_point(&frame.rib[frame.rib.len() - 1]));
                    (first - last).length() <= self.radius * 0.02
                });
                let frame = match read {
                    Some(frame) => frame,
                    None => {
                        let rib = self.closed_rib(f, g, st, pieces)?;
                        let turn = rib_sum(&[(&rib, 0.0)]);
                        Frame { station: st.clone(), rib, turn }
                    }
                };
                frames.push(frame);
                continue;
            }
            let Some(frame) = self.frame(f, g, st, pieces, h) else {
                if probe {
                    eprintln!("[exact] blend: no section at [{:.5},{:.5},{:.5}]", st.at.x, st.at.y, st.at.z);
                }
                return None;
            };
            frames.push(frame);
        }
        // An end section that could be given no rate of its own is given the
        // one the chord to its neighbour has. With none at all the surface
        // would stand still there in both its directions at once — a point
        // nothing can be found on by following a slope, which is how every
        // point on a surface is found.
        let n = frames.len();
        for (end, next) in [(0, 1), (n - 1, n - 2)] {
            if n < 2 || frames[end].turn.iter().any(|pole| pole.iter().any(|v| *v != 0.0)) {
                continue;
            }
            let (from, to) = if end < next { (end, next) } else { (next, end) };
            let reach = travel(&frames[from], &frames[to]);
            if reach > 0.0 {
                frames[end].turn = rib_sum(&[(&frames[to].rib, 1.0 / reach), (&frames[from].rib, -1.0 / reach)]);
            }
        }
        // Add sections until every span holds.
        let mut i = 0;
        let mut added = 0usize;
        while i + 1 < frames.len() {
            let (a, b) = (&frames[i], &frames[i + 1]);
            let chord = b.station.at - a.station.at;
            let length = chord.length();
            // A span that cannot be checked is not known to hold, and is
            // halved like one that does not. One across which nothing moves
            // further than the shortest span is a span: measured by the
            // surface, not the centre, which at a corner hardly moves at all.
            if self.holds(f, g, a, b).unwrap_or(false) || travel(a, b) <= SHORTEST / 4.0 {
                i += 1;
                continue;
            }
            added += 1;
            if added > 200_000 {
                return None;
            }
            let across = chord.try_normalized()?;
            let guess = (a.station.at + b.station.at) * 0.5 + (a.station.along - b.station.along) * (length / 8.0);
            let from = |s: &Station| {
                f.recall(&s.memory.0);
                g.recall(&s.memory.1);
                meet(f, g, guess, across, length)
            };
            // The last stretch of a blend that closes cannot always be
            // divided: the nearer its two contacts are to being one, the less
            // the surfaces say about where a section between here and the end
            // would be. What is left by then is small — the blend's own width
            // there, a few microns of surface between a section that is
            // exact and a point that is — and it is taken as the cubic
            // between them, unchecked, if it is no more than a twentieth of
            // a millimetre. Further from the end than that, or anywhere but
            // at an end, a span that will not divide is a blend that did not
            // build.
            let at_an_end = i == 0 || i + 2 == frames.len();
            let small = travel(a, b) <= 5e-5;
            let between = from(&a.station).or_else(|| from(&b.station)).and_then(|at| {
                let between = Station {
                    at,
                    along: heading(f, g, at, across),
                    memory: Rc::new((f.memory(), g.memory())),
                };
                self.frame(f, g, &between, pieces, h)
            });
            let Some(between) = between else {
                if at_an_end && small {
                    if probe {
                        eprintln!("[exact] blend: closes over a last span {:.3e} wide, taken unchecked", travel(a, b));
                    }
                    i += 1;
                    continue;
                }
                if probe {
                    eprintln!("[exact] blend: no section half way along a span {length:.3e} long");
                }
                return None;
            };
            // A section that is no nearer either end than they are to each
            // other has not divided the span.
            let (to_a, to_b, whole) = (travel(a, &between), travel(&between, b), travel(a, b));
            if !(to_a < whole && to_b < whole) && at_an_end && small {
                if probe {
                    eprintln!("[exact] blend: closes over a last span {whole:.3e} wide, taken unchecked");
                }
                i += 1;
                continue;
            }
            frames.insert(i + 1, between);
        }
        let n = frames.len();
        if n < 2 {
            return None;
        }
        let mut u_knots = vec![0.0; 4];
        let mut rows: Vec<Rib> = vec![frames[0].rib.clone()];
        let mut s = 0.0;
        for k in 0..n - 1 {
            let (a, b) = (&frames[k], &frames[k + 1]);
            let length = (b.station.at - a.station.at).length();
            if !(length > 0.0) {
                return None;
            }
            // The parameter runs with the surface, as far as its fastest
            // point goes, so that a blend turning on the spot still has a
            // parameter range to be found in.
            let reach = travel(a, b);
            s += reach;
            rows.push(rib_sum(&[(&a.rib, 1.0), (&a.turn, reach / 3.0)]));
            rows.push(rib_sum(&[(&b.rib, 1.0), (&b.turn, -reach / 3.0)]));
            rows.push(b.rib.clone());
            u_knots.extend(std::iter::repeat_n(s, if k + 2 == n { 4 } else { 3 }));
        }
        let mut control_points = Vec::with_capacity(rows.len());
        let mut weights = Vec::with_capacity(rows.len());
        for row in &rows {
            if row.iter().any(|h| !(h[3] > 0.0) || h.iter().any(|v| !v.is_finite())) {
                return None;
            }
            control_points.push(row.iter().map(rib_point).collect::<Vec<_>>());
            weights.push(row.iter().map(|h| h[3]).collect::<Vec<_>>());
        }
        let mut v_knots = vec![0.0; 3];
        for k in 1..pieces {
            let at = k as f64 / pieces as f64;
            v_knots.extend([at, at]);
        }
        v_knots.extend([1.0; 3]);
        let surface = cad_ir::brep::NurbsSurface {
            u_degree: 3,
            v_degree: 2,
            control_points,
            weights,
            u_knots,
            v_knots,
            u_closed: n > 2 && (frames[n - 1].station.at - frames[0].station.at).length() <= RESOLUTION,
            v_closed: false,
        };
        Some((surface, frames.into_iter().map(|frame| frame.station).collect()))
    }
}

impl Rail {
    /// How far the ball touching `near` at the foot of `y` is from touching
    /// `far` as well.
    fn miss(&self, y: Vec3) -> Option<(f64, Reading, Reading)> {
        let n = self.near.read(y)?;
        let foot = y - n.uphill * n.distance;
        let centre = foot + n.uphill * self.r_near;
        let f = self.far.read_near(centre, self.r_far)?;
        Some((f.distance - self.r_far, n, f))
    }

    fn read(&self, x: Vec3) -> Option<Reading> {
        let (miss, n, _) = self.miss(x)?;
        // The boundary is a curve on `near`, so the miss is a function on
        // `near`, and which way it grows there — and how fast — is its
        // gradient in that surface. Both depend on how `near` curves as well
        // as on where `far` is: the ball's centre moves more or less than the
        // point it touches at, by a different amount in each direction, so
        // the far surface's direction laid flat is not the gradient's unless
        // `near` curves the same way all round. It is measured, across two
        // directions square to each other and to `near`'s own.
        let e1 = n.uphill.any_perpendicular().try_normalized()?;
        let e2 = n.uphill.cross(e1);
        let h = (self.r_near.abs() * 1e-3).max(1e-7);
        let slope = |e: Vec3| -> Option<f64> {
            let (ahead, _, _) = self.miss(x + e * h)?;
            let (behind, _, _) = self.miss(x - e * h)?;
            Some((ahead - behind) / (2.0 * h))
        };
        let gradient = e1 * slope(e1)? + e2 * slope(e2)?;
        let steep = gradient.length();
        if !steep.is_finite() || steep < 1e-9 {
            return None;
        }
        Some(Reading {
            distance: miss / steep,
            uphill: gradient * (1.0 / steep),
        })
    }
}

/// The point of a polyline nearest `x`.
fn nearest_on(points: &[Vec3], x: Vec3) -> Option<Vec3> {
    let mut best: Option<(f64, Vec3)> = None;
    let mut offer = |q: Vec3| {
        let d = (q - x).length_squared();
        if best.is_none_or(|(b, _)| d < b) {
            best = Some((d, q));
        }
    };
    for w in points.windows(2) {
        let seg = w[1] - w[0];
        let len2 = seg.length_squared();
        let t = if len2 > 0.0 {
            ((x - w[0]).dot(seg) / len2).clamp(0.0, 1.0)
        } else {
            0.0
        };
        offer(w[0] + seg * t);
    }
    if let [only] = points {
        offer(*only);
    }
    best.map(|(_, q)| q)
}

/// Solve three linear equations whose rows are three directions.
///
/// By Cramer's rule where the rows stand well apart. Where two of them are
/// nearly one — two surfaces that touch rather than cross, which is what the
/// end of many an intersection is — the equations they stand for are nearly
/// the same equation, and the least step that satisfies what can be satisfied
/// is taken instead of an enormous one that satisfies nothing.
fn solve(rows: [Vec3; 3], rhs: [f64; 3]) -> Option<Vec3> {
    let [r0, r1, r2] = rows;
    let det = r0.dot(r1.cross(r2));
    if det.abs() > 1e-4 {
        let step = (r1.cross(r2) * rhs[0] + r2.cross(r0) * rhs[1] + r0.cross(r1) * rhs[2])
            * (1.0 / det);
        return step.is_finite().then_some(step);
    }
    // step = Rᵀ (R Rᵀ + λ)⁻¹ rhs: the minimum-norm step, damped.
    let g = |a: Vec3, b: Vec3| a.dot(b);
    let lambda = 1e-10;
    let m = [
        [g(r0, r0) + lambda, g(r0, r1), g(r0, r2)],
        [g(r1, r0), g(r1, r1) + lambda, g(r1, r2)],
        [g(r2, r0), g(r2, r1), g(r2, r2) + lambda],
    ];
    let col = |k: usize| Vec3::new(m[0][k], m[1][k], m[2][k]);
    let b = Vec3::new(rhs[0], rhs[1], rhs[2]);
    let (c0, c1, c2) = (col(0), col(1), col(2));
    let d = c0.dot(c1.cross(c2));
    if d.abs() < 1e-300 {
        return None;
    }
    let y = [
        b.dot(c1.cross(c2)) / d,
        c0.dot(b.cross(c2)) / d,
        c0.dot(c1.cross(b)) / d,
    ];
    let step = r0 * y[0] + r1 * y[1] + r2 * y[2];
    step.is_finite().then_some(step)
}

/// The point where two fields are zero together, in the plane through `x0`
/// square to `along`.
///
/// `reach` is how far from `x0` the answer may be: the caller knows the two
/// points the curve runs between, and a solve that lands further off than
/// they are apart has found some other place the surfaces meet.
pub fn meet(f: &Field, g: &Field, x0: Vec3, along: Vec3, reach: f64) -> Option<Vec3> {
    let mut x = x0;
    let mut last = f64::INFINITY;
    for _ in 0..64 {
        let (a, b) = (f.read(x)?, g.read(x)?);
        let step = solve(
            [a.uphill, b.uphill, along],
            [-a.distance, -b.distance, (x0 - x).dot(along)],
        )?;
        let length = step.length();
        let step = if length > reach { step * (reach / length) } else { step };
        x = x + step;
        if (x - x0).length() > reach * 2.0 {
            return None;
        }
        // Done when the step is rounding, or has stopped getting smaller at
        // a size that is already nothing.
        if length <= 1e-13 * (1.0 + x.length()) || (length <= RESOLUTION * 1e-3 && length >= last) {
            break;
        }
        last = length;
    }
    let (a, b) = (f.read(x)?, g.read(x)?);
    (a.distance.abs() <= RESOLUTION && b.distance.abs() <= RESOLUTION).then_some(x)
}

/// The shortest span a curve is divided into.
const SHORTEST: f64 = 2e-5;

/// What two fields remember, kept with the point they remembered it at.
type Memory = Rc<(Vec<Hint>, Vec<Hint>)>;

/// One point of a defined curve, and the way the curve runs through it.
#[derive(Debug, Clone)]
pub struct Station {
    pub at: Vec3,
    /// Unit tangent, pointing the way the chart runs.
    pub along: Vec3,
    memory: Memory,
}

/// Read every point of a chart on both fields, each from its neighbour.
///
/// The chart's points are on the curve, so each must read as on both fields;
/// one that does not means the fields are not what the file means, and the
/// curve is not attempted. They are read outward from whichever point reads
/// best with nothing to go on, because the ends of a chart are where a curve
/// is most likely to have run off the edge of a surface and onto a part of it
/// that some other part lies nearer than.
fn anchor(f: &Field, g: &Field, chart: &[Vec3]) -> Option<Vec<Memory>> {
    let off = |p: Vec3| Some(f.read(p)?.distance.abs().max(g.read(p)?.distance.abs()));
    let mut start: Option<(usize, f64)> = None;
    for (i, p) in chart.iter().enumerate() {
        f.forget();
        g.forget();
        if let Some(o) = off(*p)
            && start.is_none_or(|(_, best)| o < best)
        {
            start = Some((i, o));
            if o <= RESOLUTION * 1e-3 {
                break;
            }
        }
    }
    let (start, best) = start?;
    if best > RESOLUTION {
        return None;
    }
    let mut memories: Vec<Option<Memory>> = vec![None; chart.len()];
    let take = |i: usize, from: Option<&Memory>| -> Option<Memory> {
        match from {
            Some(m) => {
                f.recall(&m.0);
                g.recall(&m.1);
            }
            None => {
                f.forget();
                g.forget();
            }
        }
        let followed = off(chart[i]).filter(|o| *o <= RESOLUTION);
        if followed.is_none() {
            // Not where its neighbour led: ask afresh before giving up.
            f.forget();
            g.forget();
            off(chart[i]).filter(|o| *o <= RESOLUTION)?;
        }
        Some(Rc::new((f.memory(), g.memory())))
    };
    memories[start] = Some(take(start, None)?);
    for i in start + 1..chart.len() {
        let from = memories[i - 1].clone();
        memories[i] = Some(take(i, from.as_ref())?);
    }
    for i in (0..start).rev() {
        let from = memories[i + 1].clone();
        memories[i] = Some(take(i, from.as_ref())?);
    }
    memories.into_iter().collect()
}

/// How far a chart's points stand from the two fields it is the meeting of,
/// read as the curve itself is read: `None` where they cannot be.
pub fn chart_off(f: &Field, g: &Field, chart: &[Vec3]) -> Option<f64> {
    let memories = anchor(f, g, chart)?;
    let mut worst = 0.0f64;
    for (p, m) in chart.iter().zip(&memories) {
        f.recall(&m.0);
        g.recall(&m.1);
        worst = worst.max(f.read(*p)?.distance.abs().max(g.read(*p)?.distance.abs()));
    }
    Some(worst)
}

/// Which way the curve two fields meet along runs through `x`.
///
/// The curve lies in both surfaces, so it runs square to both their
/// directions: along their cross product. `toward` says which of the two ways
/// that is, and stands in for the cross product where the surfaces touch
/// rather than cross and have none.
fn heading(f: &Field, g: &Field, x: Vec3, toward: Vec3) -> Vec3 {
    let crossed = match (f.read(x), g.read(x)) {
        (Some(a), Some(b)) => a.uphill.cross(b.uphill),
        _ => return toward,
    };
    if crossed.length() < 1e-4 {
        return toward;
    }
    let t = crossed.normalized_or(toward);
    if t.dot(toward) < 0.0 { -t } else { t }
}

/// The curve two fields meet along, through the points of its chart, as the
/// stations of a cubic that is the curve to within [`RESOLUTION`].
///
/// A chord between two points of a curve misses it by the square of their
/// spacing; a cubic that also leaves each point in the curve's own direction
/// misses it by the fourth power. So a micron takes a point every few tenths
/// of a millimetre if they are joined by straight lines, and a hundredth of a
/// micron takes one every few millimetres if they are joined by cubics — and
/// the direction costs nothing, being the cross product of two directions the
/// solve has already found. Each span is checked where a cubic errs most, at
/// its middle, against the curve itself, and halved until it holds.
///
/// `None` when any stretch cannot be computed; a curve is not returned in part.
pub fn stations(f: &Field, g: &Field, chart: &[Vec3]) -> Option<Vec<Station>> {
    if chart.len() < 2 {
        return None;
    }
    let memories = anchor(f, g, chart)?;
    let n = chart.len();
    let closed = n > 2 && (chart[n - 1] - chart[0]).length() <= RESOLUTION;
    // Each chart point leaves in the direction of the chords either side of
    // it, which is near enough to say which way the cross product points.
    let toward = |i: usize| -> Option<Vec3> {
        let before = if i > 0 { Some(i - 1) } else if closed { Some(n - 2) } else { None };
        let after = if i + 1 < n { Some(i + 1) } else if closed { Some(1) } else { None };
        let chord = |a: usize, b: usize| (chart[b] - chart[a]).try_normalized();
        match (before.and_then(|b| chord(b, i)), after.and_then(|a| chord(i, a))) {
            (Some(p), Some(q)) => (p + q).try_normalized().or(Some(q)),
            (Some(p), None) => Some(p),
            (None, Some(q)) => Some(q),
            (None, None) => None,
        }
    };
    let mut given = Vec::with_capacity(n);
    for (i, p) in chart.iter().enumerate() {
        f.recall(&memories[i].0);
        g.recall(&memories[i].1);
        given.push(Station {
            at: *p,
            along: heading(f, g, *p, toward(i)?),
            memory: memories[i].clone(),
        });
    }
    if closed {
        // One point, one direction: the curve leaves where it arrives.
        given[n - 1].along = given[0].along;
    }
    let mut out = Vec::with_capacity(n * 4);
    out.push(given[0].clone());
    for w in given.windows(2) {
        span(f, g, &w[0], &w[1], 24, &mut out)?;
        out.push(w[1].clone());
    }
    Some(out)
}

/// The stations strictly between `a` and `b` that the cubic through them
/// needs, in order.
fn span(f: &Field, g: &Field, a: &Station, b: &Station, depth: u32, out: &mut Vec<Station>) -> Option<()> {
    let chord = b.at - a.at;
    let length = chord.length();
    let Some(across) = chord.try_normalized() else {
        return Some(());
    };
    // A span of twenty microns is a span: a cubic through two exact points
    // that close, leaving each in the curve's own direction, has nowhere to
    // be wrong by a hundredth of one. Without this a solve whose own noise is
    // near the resolution asks for another halving for ever.
    if length <= SHORTEST {
        return Some(());
    }
    // The cubic's own middle: the chord's, moved by the difference of the two
    // end directions.
    let guess = (a.at + b.at) * 0.5 + (a.along - b.along) * (length / 8.0);
    let from = |s: &Station| {
        f.recall(&s.memory.0);
        g.recall(&s.memory.1);
        meet(f, g, guess, across, length)
    };
    let Some(found) = from(a).or_else(|| from(b)) else {
        if std::env::var_os("XT_EXACT_PROBE").is_some() {
            f.recall(&a.memory.0);
            g.recall(&a.memory.1);
            eprintln!(
                "[exact] no point between [{:.6},{:.6},{:.6}] and [{:.6},{:.6},{:.6}], {:.6} apart: at the guess the fields read {:?} and {:?}",
                a.at.x, a.at.y, a.at.z, b.at.x, b.at.y, b.at.z, length,
                f.read(guess).map(|r| r.distance),
                g.read(guess).map(|r| r.distance),
            );
        }
        return None;
    };
    if (found - guess).length() <= RESOLUTION {
        return Some(());
    }
    if depth == 0 {
        if std::env::var_os("XT_EXACT_PROBE").is_some() {
            eprintln!("[exact] still {:.3e} off after every halving, on a span {:.3e} long", (found - guess).length(), length);
        }
        return None;
    }
    let mid = Station {
        at: found,
        along: heading(f, g, found, across),
        memory: Rc::new((f.memory(), g.memory())),
    };
    span(f, g, a, &mid, depth - 1, out)?;
    out.push(mid.clone());
    span(f, g, &mid, b, depth - 1, out)
}

/// The cubic through a run of stations, parameterised by the length of the
/// chords between them.
///
/// One Bezier span per pair, leaving each station along its own direction at
/// unit speed, so the curve is smooth across every station and its parameter
/// is very nearly its own arc length — which is what the file's trimmed
/// curves measure their windows in.
pub fn cubic(stations: &[Station]) -> Option<cad_ir::brep::NurbsCurve> {
    if stations.len() < 2 {
        return None;
    }
    let mut knots = vec![0.0; 4];
    let mut control = vec![stations[0].at];
    let mut s = 0.0;
    for (i, w) in stations.windows(2).enumerate() {
        let length = (w[1].at - w[0].at).length();
        if !(length > 0.0) {
            return None;
        }
        s += length;
        control.push(w[0].at + w[0].along * (length / 3.0));
        control.push(w[1].at - w[1].along * (length / 3.0));
        control.push(w[1].at);
        let last = i + 2 == stations.len();
        knots.extend(std::iter::repeat_n(s, if last { 4 } else { 3 }));
    }
    let n = stations.len();
    Some(cad_ir::brep::NurbsCurve {
        degree: 3,
        control_points: control,
        weights: Vec::new(),
        knots,
        closed: n > 2 && (stations[n - 1].at - stations[0].at).length() <= RESOLUTION,
    })
}

/// The fields of one file's surfaces.
pub struct Fields<'a> {
    entities: &'a Entities,
    index: &'a Index<'a>,
    stated: RefCell<FxHashMap<usize, Option<Surface>>>,
    /// Each blend's centre line, by entity: see [`Blend::laid_line`].
    laid: RefCell<FxHashMap<usize, Rc<Laid>>>,
}

fn ptr(entities: &Entities, e: &RawEntity, i: usize) -> usize {
    entities.fields(e).get(i).map(|f| f.as_ptr()).unwrap_or(0)
}

fn real(entities: &Entities, e: &RawEntity, i: usize) -> f64 {
    entities.fields(e).get(i).map(|f| f.as_f64()).unwrap_or(f64::NAN)
}

fn median(mut values: Vec<f64>) -> Option<f64> {
    values.retain(|v| v.is_finite());
    if values.is_empty() {
        return None;
    }
    values.sort_by(f64::total_cmp);
    Some(values[values.len() / 2])
}

impl<'a> Fields<'a> {
    pub fn new(entities: &'a Entities, index: &'a Index<'a>) -> Self {
        Fields {
            entities,
            index,
            stated: RefCell::new(FxHashMap::default()),
            laid: RefCell::new(FxHashMap::default()),
        }
    }

    fn stated(&self, e: &RawEntity) -> Option<Surface> {
        if let Some(hit) = self.stated.borrow().get(&e.index) {
            return hit.clone();
        }
        let lowered = geom::surface(self.entities, e, self.index).ok();
        self.stated.borrow_mut().insert(e.index, lowered.clone());
        lowered
    }

    /// The field of the surface entity `handle`.
    ///
    /// `witnesses` are points the file itself places at the distance
    /// `standoff` from that surface — a chart's points for a surface the chart
    /// lies on, at no distance; a blend's centre line for the surfaces it
    /// blends, at the radius. They are what settles the one thing a
    /// definition leaves to convention: which side of a surface an offset or
    /// a ball's centre lies on. The file says which by where it puts its own
    /// points, and that is read rather than assumed.
    pub fn of(&self, handle: usize, witnesses: &[Vec3], standoff: f64) -> Option<Field> {
        let e = self.index.get(&handle)?;
        match e.type_id {
            xt::OFFSET_SURF => {
                let base = self.of(ptr(self.entities, e, 9), &[], 0.0)?;
                let size = real(self.entities, e, 10).abs();
                if !size.is_finite() {
                    return None;
                }
                // How far the witnesses are from standing where they say, if
                // the offset is `by`: read on the normal each would be on.
                let miss = |by: f64| {
                    let seen = median(
                        witnesses
                            .iter()
                            .filter_map(|p| {
                                [by + standoff, by - standoff]
                                    .into_iter()
                                    .filter_map(|expect| {
                                        base.forget();
                                        Some((base.read_near(*p, expect)?.distance - expect).abs())
                                    })
                                    .min_by(f64::total_cmp)
                            })
                            .collect(),
                    );
                    base.forget();
                    seen
                };
                let by = match (miss(size), miss(-size)) {
                    // The side on which the witnesses stand where they say.
                    (Some(plus), Some(minus)) => {
                        if plus <= minus { size } else { -size }
                    }
                    // No witness: the entity's own sense, as an offset surface
                    // read on its own has always been taken.
                    _ => {
                        let stated = real(self.entities, e, 10);
                        if geom::geom_sense(self.entities, e) == '-' { -stated } else { stated }
                    }
                };
                Some(Field::Offset {
                    base: Box::new(base),
                    by,
                })
            }
            xt::BLENDED_EDGE => self.blend(handle, witnesses).map(|b| Field::Blend(Box::new(b))),
            xt::BLEND_BOUND => {
                let blend = self.blend(ptr(self.entities, e, 8), witnesses)?;
                // Which of the two surfaces this boundary lies on: the one the
                // witnesses are on, or failing them the one the entity names.
                let on = |f: &Field| {
                    let seen = median(
                        witnesses
                            .iter()
                            .filter_map(|p| f.read(*p).map(|r| r.distance.abs()))
                            .collect(),
                    );
                    f.forget();
                    seen
                };
                let first = match (on(&blend.a), on(&blend.b)) {
                    (Some(a), Some(b)) => a <= b,
                    _ => geom::int_at(self.entities, e, 7) == 0,
                };
                let Blend { a, b, ra, rb, .. } = blend;
                let rail = if first {
                    Rail { near: a, far: b, r_near: ra, r_far: rb }
                } else {
                    Rail { near: b, far: a, r_near: rb, r_far: ra }
                };
                Some(Field::Rail(Box::new(rail)))
            }
            _ => self.stated(e).map(Field::surface),
        }
    }

    /// The points the file gives of an intersection curve, in order: its
    /// chart, and at either end the curve's own end where the chart stops
    /// short of it. Returned with which of the points are the chart.
    ///
    /// A chart is the modeller's sample of a curve, and where the curve ends
    /// at a place the two surfaces touch rather than cross — a blend closing
    /// to nothing, two fillets running into each other — it stops short: the
    /// direction of a curve there is the cross product of two normals that
    /// have become one, and the modeller does not sample what it cannot take
    /// a direction at. The end itself is written separately, as a limit of
    /// kind `T`, up to half a millimetre past the chart on the files here.
    /// Leaving it out left the curve that much short of the vertex it ends
    /// at, and the edge was closed with a straight line the curve is not.
    fn charted(&self, curve: &RawEntity) -> Option<(Vec<Vec3>, std::ops::Range<usize>)> {
        let chart = self
            .index
            .get(&ptr(self.entities, curve, 8))
            .filter(|c| c.type_id == xt::CHART)?;
        let mut points = geom::chart_points(self.entities, chart);
        let count = points.len();
        if count < 2 {
            return None;
        }
        // LIMIT: [0] kind, [2] the end itself.
        let end = |i: usize| -> Option<Vec3> {
            let limit = self.index.get(&ptr(self.entities, curve, i)).filter(|l| l.type_id == xt::LIMIT)?;
            let fields = self.entities.fields(limit);
            if fields.first().map(|f| f.as_char()) != Some('T') {
                return None;
            }
            let at = fields.get(2).map(|f| f.as_vec3())?;
            let at = Vec3::new(at[0], at[1], at[2]);
            at.is_finite().then_some(at)
        };
        let mut first = 0;
        if let Some(at) = end(9)
            && (at - points[0]).length() > RESOLUTION
        {
            points.insert(0, at);
            first = 1;
        }
        if let Some(at) = end(10)
            && (at - points[points.len() - 1]).length() > RESOLUTION
        {
            points.push(at);
        }
        Some((points, first..first + count))
    }

    /// Points along a blend's centre line, in order.
    ///
    /// Where the line is the meeting of two offsets with no formula, the file
    /// gives a chart of it. Where it has a formula — the line between two
    /// planes, the circle or ellipse round a hole, a spline — it gives that,
    /// and the points are taken off it. A straight line has no ends of its
    /// own; `reach` are points the blend has to reach, and it is taken as far
    /// along the line as they are.
    fn spine(&self, curve: &RawEntity, reach: &[Vec3]) -> Option<(Vec<Vec3>, std::ops::Range<usize>)> {
        if curve.type_id == xt::INTERSECTION {
            return self.charted(curve);
        }
        let line = geom::curve(self.entities, curve, self.index).ok()?;
        let (range, count) = match &line {
            cad_ir::brep::Curve::Line { origin, direction } => {
                let scale = direction.length_squared();
                if !(scale > 0.0) {
                    return None;
                }
                let along = reach.iter().map(|p| (*p - *origin).dot(*direction) / scale);
                let (lo, hi) = along.fold((f64::INFINITY, f64::NEG_INFINITY), |(lo, hi), t| (lo.min(t), hi.max(t)));
                if !(hi > lo) {
                    return None;
                }
                // A little further than the furthest, so that nothing asked
                // of the very end falls off it.
                let spare = (hi - lo) * 1e-3;
                (cad_ir::math::Interval::new(lo - spare, hi + spare), 2)
            }
            other => {
                let natural = other.natural_range();
                if !(natural.span().is_finite() && natural.span().abs() < 1e9) {
                    return None;
                }
                match other.period() {
                    Some(period) => (cad_ir::math::Interval::new(natural.lo, natural.lo + period), 17),
                    None => (natural, 33),
                }
            }
        };
        let mut points: Vec<Vec3> = (0..count)
            .map(|i| line.point_at(range.at(i as f64 / (count - 1) as f64)))
            .collect();
        // A closed line arrives exactly where it left.
        if line.period().is_some()
            && let Some(first) = points.first().copied()
            && let Some(last) = points.last_mut()
            && (*last - first).length() <= RESOLUTION
        {
            *last = first;
        }
        let all = 0..points.len();
        points.iter().all(|p| p.is_finite()).then_some((points, all))
    }

    /// The rolling-ball blend the entity `handle` defines.
    ///
    /// `reach` are points the blend is known to pass through, which are only
    /// needed where its centre line is a straight line: see [`Fields::spine`].
    pub fn blend(&self, handle: usize, reach: &[Vec3]) -> Option<Blend> {
        let e = self.index.get(&handle).filter(|e| e.type_id == xt::BLENDED_EDGE)?;
        // 'R' is a ball rolled between two surfaces. 'E' is a ball rolled
        // between a surface and an *edge* — a cliff-edge blend — and the file
        // writes the edge as what it is the limit of: a rolling-ball blend of
        // no radius between the two surfaces that meet at it. So the two are
        // one definition here, and the edge is read like any other surface,
        // as the distance from it.
        if !matches!(self.entities.fields(e).get(7).map(|f| f.as_char()), Some('R' | 'E')) {
            return None;
        }
        // range[11], range[12]: how far the ball's centre rides off each
        // surface. A rolling ball rides the same distance off both.
        let (first, second) = (real(self.entities, e, 11), real(self.entities, e, 12));
        let radius = first.abs();
        if !(radius.is_finite() && radius >= 0.0)
            || (second.abs() - radius).abs() > radius * 1e-9
        {
            return None;
        }
        let spine_curve = self.index.get(&ptr(self.entities, e, 10))?;
        let (spine, charted) = self.spine(spine_curve, reach)?;
        if spine.len() < 2 {
            return None;
        }
        // One centre line per blend, however many times the blend is read —
        // except along a straight line, which is as long as whoever asks
        // needs it to be.
        let laid = if spine_curve.type_id == xt::LINE {
            Rc::default()
        } else {
            self.laid.borrow_mut().entry(handle).or_default().clone()
        };
        // The chart is what bears witness: a line's own end is where the
        // two surfaces come to touch, and one of them may itself be a blend
        // that has run out by then and has nothing to say there.
        let chart = &spine[charted.clone()];
        let a = self.of(ptr(self.entities, e, 8), chart, radius)?;
        let b = self.of(ptr(self.entities, e, 9), chart, radius)?;
        // Which side of each surface the centre rides on is where the file's
        // own samples of the centre line are: the side on which they read as
        // the radius off, on the normal a centre on that side would be on.
        let side = |f: &Field| -> Option<f64> {
            let miss = |by: f64| {
                // Each from nothing. Read one after another they follow one
                // another, and a first point that lands on the wrong part of
                // a surface takes every later one there with it — which is
                // right for a curve and wrong for a show of hands.
                let seen = median(
                    chart
                        .iter()
                        .filter_map(|c| {
                            f.forget();
                            f.read_near(*c, by).map(|r| (r.distance - by).abs())
                        })
                        .collect(),
                );
                f.forget();
                seen
            };
            let (plus, minus) = (miss(radius)?, miss(-radius)?);
            let (by, off) = if plus <= minus { (radius, plus) } else { (-radius, minus) };
            (off <= radius * 1e-3 + 1e-7).then_some(by)
        };
        let (ra, rb) = (side(&a)?, side(&b)?);
        Some(Blend {
            a,
            b,
            ra,
            rb,
            radius,
            spine,
            charted,
            centre: Cell::new(None),
            along: Cell::new(None),
            laid,
            way: Cell::new(1.0),
        })
    }

    /// The points of an intersection curve — its chart and its own ends —
    /// the two fields it is the meeting of, and which of the points are the
    /// chart.
    pub fn intersection(&self, handle: usize) -> Option<(Vec<Vec3>, Field, Field, std::ops::Range<usize>)> {
        let e = self.index.get(&handle).filter(|e| e.type_id == xt::INTERSECTION)?;
        let (points, charted) = self.charted(e)?;
        let second = self.entities.extra(e).first().map(|f| f.as_ptr()).unwrap_or(0);
        let chart = &points[charted.clone()];
        let f = self.of(ptr(self.entities, e, 7), chart, 0.0)?;
        let g = self.of(second, chart, 0.0)?;
        Some((points, f, g, charted))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use cad_ir::math::Frame;

    fn plane(origin: Vec3, normal: Vec3, across: Vec3) -> Field {
        Field::surface(Surface::Plane { frame: Frame::new(origin, normal, across) })
    }

    /// A ball of `radius` rolled in the corner between the planes `x = 0` and
    /// `y = 0`, from `z = 0` to `z = length`.
    fn corner(radius: f64, length: f64) -> Blend {
        Blend {
            a: plane(Vec3::ZERO, Vec3::X, Vec3::Y),
            b: plane(Vec3::ZERO, Vec3::Y, Vec3::Z),
            ra: radius,
            rb: radius,
            radius,
            spine: vec![Vec3::new(radius, radius, 0.0), Vec3::new(radius, radius, length)],
            charted: 0..2,
            centre: Cell::new(None),
            along: Cell::new(None),
            laid: Rc::default(),
            way: Cell::new(1.0),
        }
    }

    #[test]
    fn the_meeting_of_a_sphere_and_a_plane_is_computed_as_the_circle_it_is() {
        // A unit sphere cut by the plane z = 0.6: a circle of radius 0.8,
        // charted at five points a quarter of the way round.
        let sphere = Field::surface(Surface::Sphere { frame: Frame::new(Vec3::ZERO, Vec3::Z, Vec3::X), radius: 1.0 });
        let cut = plane(Vec3::new(0.0, 0.0, 0.6), Vec3::Z, Vec3::X);
        let chart: Vec<Vec3> = (0..5)
            .map(|i| {
                let a = i as f64 * std::f64::consts::FRAC_PI_8;
                Vec3::new(0.8 * a.cos(), 0.8 * a.sin(), 0.6)
            })
            .collect();
        let stations = stations(&sphere, &cut, &chart).expect("the curve computes");
        let curve = cad_ir::brep::Curve::Nurbs(cubic(&stations).expect("and is a cubic"));
        let range = curve.natural_range();
        for i in 0..=200 {
            let p = curve.point_at(range.at(i as f64 / 200.0));
            assert!((p.length() - 1.0).abs() <= RESOLUTION, "off the sphere by {:e}", p.length() - 1.0);
            assert!((p.z - 0.6).abs() <= RESOLUTION, "off the plane by {:e}", p.z - 0.6);
        }
        // It runs from the chart's first point to its last, and no further.
        assert!((curve.point_at(range.lo) - chart[0]).length() <= 1e-12);
        assert!((curve.point_at(range.hi) - chart[4]).length() <= 1e-12);
    }

    #[test]
    fn a_ball_rolled_in_a_square_corner_sweeps_a_quarter_of_a_cylinder() {
        let (radius, length) = (0.01, 0.05);
        let blend = corner(radius, length);
        let surface = Surface::Nurbs(blend.surface(&[]).expect("the blend builds"));
        let d = surface.domain();
        for i in 0..=20 {
            for j in 0..=10 {
                let uv = Vec2::new(d.u.at(i as f64 / 20.0), d.v.at(j as f64 / 10.0));
                let p = surface.point_at(uv);
                // The radius from the ball's centre line, which is the line
                // x = y = radius.
                let out = ((p.x - radius).powi(2) + (p.y - radius).powi(2)).sqrt();
                assert!((out - radius).abs() <= RESOLUTION, "{:e} off the cylinder", out - radius);
                // Its first edge on the first plane, its last on the second.
                if j == 0 {
                    assert!(p.x.abs() <= RESOLUTION, "first edge {:e} off its plane", p.x);
                }
                if j == 10 {
                    assert!(p.y.abs() <= RESOLUTION, "last edge {:e} off its plane", p.y);
                }
            }
        }
        // End to end along the corner.
        assert!(surface.point_at(Vec2::new(d.u.lo, 0.5)).z.abs() <= 1e-12);
        assert!((surface.point_at(Vec2::new(d.u.hi, 0.5)).z - length).abs() <= 1e-12);
        assert!(blend.departure(&[], 8, 6).expect("it can be checked") <= RESOLUTION);
    }

    #[test]
    fn a_blend_is_carried_as_far_as_the_faces_on_it_reach() {
        let (radius, length) = (0.01, 0.05);
        let blend = corner(radius, length);
        // A face whose corner is 3 mm past the end of the charted centre
        // line, on the blend's own surface.
        let beyond = Vec3::new(0.0, radius, length + 0.003);
        let surface = Surface::Nurbs(blend.surface(&[beyond]).expect("the blend builds"));
        let d = surface.domain();
        let end = surface.point_at(Vec2::new(d.u.hi, 0.0));
        assert!((end - beyond).length() <= 1e-7, "the surface stops {:e} short", (end - beyond).length());
    }

    #[test]
    fn a_ball_wider_than_the_cylinder_it_rolls_on_is_read_on_the_far_normal() {
        // A two-millimetre ball against a cylinder of one millimetre radius
        // has its centre a millimetre *past the axis*: two from the contact,
        // and on the cylinder's far wall, which is the nearest point to it.
        let cylinder = Field::surface(Surface::Cylinder { frame: Frame::new(Vec3::ZERO, Vec3::Z, Vec3::X), radius: 1e-3 });
        let centre = Vec3::new(1e-3, 0.0, 0.004);
        let nearest = cylinder.read(centre).expect("it reads");
        assert!(nearest.distance.abs() <= 1e-15, "the nearest wall is where the centre is");
        // Asked for the normal it is two millimetres along, it is.
        let far = [2e-3, -2e-3]
            .into_iter()
            .filter_map(|expect| Some((cylinder.read_near(centre, expect)?.distance - expect).abs()))
            .fold(f64::INFINITY, f64::min);
        assert!(far <= 1e-15, "the far wall reads {far:e} off two millimetres");
    }

    #[test]
    fn a_blend_is_read_as_the_distance_from_its_centre_line() {
        let (radius, length) = (0.01, 0.05);
        let blend = Field::Blend(Box::new(corner(radius, length)));
        // Four millimetres from the centre line, half way along it: six
        // inside the ball's surface.
        let inside = blend.read(Vec3::new(radius + 0.004, radius, 0.02)).expect("it reads");
        assert!((inside.distance + 0.006).abs() <= 1e-12, "{:e}", inside.distance + 0.006);
        assert!((inside.uphill - Vec3::X).length() <= 1e-9);
        // Past the end of the charted line the two planes go on, and so does
        // the blend between them.
        let beyond = blend.read(Vec3::new(radius, radius + 0.013, length + 0.004)).expect("it reads");
        assert!((beyond.distance - 0.003).abs() <= 1e-12, "{:e}", beyond.distance - 0.003);
        // A ball wider than the blend, rolled inside it, is read on the far
        // side of its section: three millimetres off the line, thirteen from
        // the far wall.
        let far = blend.read_near(Vec3::new(radius - 0.003, radius, 0.03), -0.013).expect("it reads");
        assert!((far.distance + 0.013).abs() <= 1e-12, "{:e}", far.distance + 0.013);
        assert!((far.uphill - Vec3::X).length() <= 1e-9);
    }

    #[test]
    fn a_blend_of_no_radius_is_the_edge_its_two_surfaces_meet_at() {
        let edge = Field::Blend(Box::new(corner(0.0, 0.05)));
        // Three and four millimetres from the two planes: five from the edge.
        let reading = edge.read(Vec3::new(0.003, 0.004, 0.02)).expect("it reads");
        assert!((reading.distance - 0.005).abs() <= 1e-12, "{:e}", reading.distance - 0.005);
        assert!((reading.uphill - Vec3::new(0.6, 0.8, 0.0)).length() <= 1e-9);
        // And a ball rolled against it — a cliff-edge blend — has its centre
        // that far from it, whichever way round the offset is written.
        for by in [0.005, -0.005] {
            let ball = Field::Offset { base: Box::new(edge.clone()), by };
            assert!(ball.read(Vec3::new(0.003, 0.004, 0.02)).expect("it reads").distance.abs() <= 1e-12);
        }
    }
}
