//! The minimum linear algebra the pipeline needs.
//!
//! Deliberately not a dependency on a general maths crate: the whole surface
//! area is a point, a direction, a rigid placement and a 4×4 transform, all of
//! which the readers, the tessellator and the writers must agree on exactly.
//! Owning the definitions keeps that agreement checkable.
//!
//! Everything is `f64`. CAD models routinely span six orders of magnitude
//! between a fillet radius and an assembly bounding box, and `f32` loses the
//! small end. Conversion to `f32` happens once, at the writer.

/// A point or vector in 3D.
#[derive(Debug, Clone, Copy, PartialEq, Default)]
pub struct Vec3 {
    pub x: f64,
    pub y: f64,
    pub z: f64,
}

impl Vec3 {
    pub const ZERO: Vec3 = Vec3::new(0.0, 0.0, 0.0);
    pub const X: Vec3 = Vec3::new(1.0, 0.0, 0.0);
    pub const Y: Vec3 = Vec3::new(0.0, 1.0, 0.0);
    pub const Z: Vec3 = Vec3::new(0.0, 0.0, 1.0);

    pub const fn new(x: f64, y: f64, z: f64) -> Vec3 {
        Vec3 { x, y, z }
    }

    pub fn from_slice(v: &[f64]) -> Vec3 {
        Vec3 {
            x: v.first().copied().unwrap_or(0.0),
            y: v.get(1).copied().unwrap_or(0.0),
            z: v.get(2).copied().unwrap_or(0.0),
        }
    }

    pub fn to_array(self) -> [f64; 3] {
        [self.x, self.y, self.z]
    }

    pub fn dot(self, o: Vec3) -> f64 {
        self.x * o.x + self.y * o.y + self.z * o.z
    }

    pub fn cross(self, o: Vec3) -> Vec3 {
        Vec3::new(
            self.y * o.z - self.z * o.y,
            self.z * o.x - self.x * o.z,
            self.x * o.y - self.y * o.x,
        )
    }

    pub fn length(self) -> f64 {
        self.dot(self).sqrt()
    }

    pub fn length_squared(self) -> f64 {
        self.dot(self)
    }

    /// Unit vector, or `None` when the vector is too short to have a direction.
    pub fn try_normalized(self) -> Option<Vec3> {
        let n = self.length();
        if n.is_finite() && n > f64::MIN_POSITIVE.sqrt() {
            Some(self * (1.0 / n))
        } else {
            None
        }
    }

    /// Unit vector, falling back to `fallback` for a degenerate input.
    ///
    /// CAD files do contain zero-length direction vectors — usually a
    /// degenerate seam — and stopping the whole conversion for one is worse
    /// than continuing with a defined axis.
    pub fn normalized_or(self, fallback: Vec3) -> Vec3 {
        self.try_normalized().unwrap_or(fallback)
    }

    /// Any unit vector perpendicular to `self`.
    ///
    /// Picks the axis `self` is least aligned with, so the cross product never
    /// degenerates.
    pub fn any_perpendicular(self) -> Vec3 {
        let a = if self.x.abs() < self.y.abs() && self.x.abs() < self.z.abs() {
            Vec3::X
        } else if self.y.abs() < self.z.abs() {
            Vec3::Y
        } else {
            Vec3::Z
        };
        self.cross(a).normalized_or(Vec3::X)
    }

    pub fn lerp(self, o: Vec3, t: f64) -> Vec3 {
        self + (o - self) * t
    }

    pub fn min(self, o: Vec3) -> Vec3 {
        Vec3::new(self.x.min(o.x), self.y.min(o.y), self.z.min(o.z))
    }

    pub fn max(self, o: Vec3) -> Vec3 {
        Vec3::new(self.x.max(o.x), self.y.max(o.y), self.z.max(o.z))
    }

    pub fn is_finite(self) -> bool {
        self.x.is_finite() && self.y.is_finite() && self.z.is_finite()
    }
}

impl std::ops::Add for Vec3 {
    type Output = Vec3;
    fn add(self, o: Vec3) -> Vec3 {
        Vec3::new(self.x + o.x, self.y + o.y, self.z + o.z)
    }
}

impl std::ops::Sub for Vec3 {
    type Output = Vec3;
    fn sub(self, o: Vec3) -> Vec3 {
        Vec3::new(self.x - o.x, self.y - o.y, self.z - o.z)
    }
}

impl std::ops::Mul<f64> for Vec3 {
    type Output = Vec3;
    fn mul(self, s: f64) -> Vec3 {
        Vec3::new(self.x * s, self.y * s, self.z * s)
    }
}

impl std::ops::Neg for Vec3 {
    type Output = Vec3;
    fn neg(self) -> Vec3 {
        Vec3::new(-self.x, -self.y, -self.z)
    }
}

impl std::ops::AddAssign for Vec3 {
    fn add_assign(&mut self, o: Vec3) {
        *self = *self + o;
    }
}

/// A point in a surface's parameter space.
#[derive(Debug, Clone, Copy, PartialEq, Default)]
pub struct Vec2 {
    pub u: f64,
    pub v: f64,
}

impl Vec2 {
    pub const fn new(u: f64, v: f64) -> Vec2 {
        Vec2 { u, v }
    }
}

impl std::ops::Sub for Vec2 {
    type Output = Vec2;
    fn sub(self, o: Vec2) -> Vec2 {
        Vec2::new(self.u - o.u, self.v - o.v)
    }
}

impl std::ops::Add for Vec2 {
    type Output = Vec2;
    fn add(self, o: Vec2) -> Vec2 {
        Vec2::new(self.u + o.u, self.v + o.v)
    }
}

impl std::ops::Mul<f64> for Vec2 {
    type Output = Vec2;
    fn mul(self, s: f64) -> Vec2 {
        Vec2::new(self.u * s, self.v * s)
    }
}

/// A right-handed orthonormal frame: an origin plus a Z axis and an X axis.
///
/// Every analytic surface and curve in both STEP and Parasolid is defined
/// relative to one of these, so keeping a single canonical form means the
/// tessellator never has to know which reader produced a surface.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Frame {
    pub origin: Vec3,
    /// Local +Z: a cylinder's axis, a plane's normal, a circle's normal.
    pub axis: Vec3,
    /// Local +X: where a rotational parameter of 0 points.
    pub ref_dir: Vec3,
}

impl Default for Frame {
    fn default() -> Self {
        Frame::IDENTITY
    }
}

impl Frame {
    pub const IDENTITY: Frame = Frame {
        origin: Vec3::ZERO,
        axis: Vec3::Z,
        ref_dir: Vec3::X,
    };

    /// Build a frame, orthonormalising the inputs.
    ///
    /// `ref_dir` is projected off `axis` by Gram-Schmidt, which is what both
    /// STEP's `AXIS2_PLACEMENT_3D` and Parasolid's surface frames specify; a
    /// file whose ref_dir is not exactly perpendicular is common and harmless.
    /// A ref_dir parallel to the axis carries no information, so an arbitrary
    /// perpendicular is substituted rather than producing a NaN frame.
    pub fn new(origin: Vec3, axis: Vec3, ref_dir: Vec3) -> Frame {
        let axis = axis.normalized_or(Vec3::Z);
        let projected = ref_dir - axis * ref_dir.dot(axis);
        let ref_dir = projected
            .try_normalized()
            .unwrap_or_else(|| axis.any_perpendicular());
        Frame {
            origin,
            axis,
            ref_dir,
        }
    }

    /// Local +Y, completing the right-handed frame.
    pub fn y_dir(&self) -> Vec3 {
        self.axis.cross(self.ref_dir)
    }

    /// Map a point from frame-local coordinates into world coordinates.
    pub fn point(&self, local: Vec3) -> Vec3 {
        self.origin + self.ref_dir * local.x + self.y_dir() * local.y + self.axis * local.z
    }

    /// Map a direction from frame-local into world coordinates.
    pub fn direction(&self, local: Vec3) -> Vec3 {
        self.ref_dir * local.x + self.y_dir() * local.y + self.axis * local.z
    }

    /// The point on the frame's XY plane at polar angle `theta` and radius `r`.
    pub fn polar(&self, r: f64, theta: f64) -> Vec3 {
        self.origin + self.ref_dir * (r * theta.cos()) + self.y_dir() * (r * theta.sin())
    }
}

/// A 4×4 affine transform, stored row-major with an implicit `0 0 0 1` row.
///
/// Assemblies nest placements, so transforms must compose; a rigid
/// origin-plus-frame cannot express the mirror and scale that some exporters
/// legitimately write, which is why this is a full matrix.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Transform {
    /// `m[row][col]`, rows 0..3 of a 4×4 matrix.
    pub m: [[f64; 4]; 3],
}

impl Default for Transform {
    fn default() -> Self {
        Transform::IDENTITY
    }
}

impl Transform {
    pub const IDENTITY: Transform = Transform {
        m: [
            [1.0, 0.0, 0.0, 0.0],
            [0.0, 1.0, 0.0, 0.0],
            [0.0, 0.0, 1.0, 0.0],
        ],
    };

    /// The transform taking frame-local coordinates into world coordinates.
    pub fn from_frame(f: &Frame) -> Transform {
        let x = f.ref_dir;
        let y = f.y_dir();
        let z = f.axis;
        Transform {
            m: [
                [x.x, y.x, z.x, f.origin.x],
                [x.y, y.y, z.y, f.origin.y],
                [x.z, y.z, z.z, f.origin.z],
            ],
        }
    }

    pub fn from_translation(t: Vec3) -> Transform {
        let mut m = Transform::IDENTITY;
        m.m[0][3] = t.x;
        m.m[1][3] = t.y;
        m.m[2][3] = t.z;
        m
    }

    pub fn from_scale(s: f64) -> Transform {
        Transform {
            m: [
                [s, 0.0, 0.0, 0.0],
                [0.0, s, 0.0, 0.0],
                [0.0, 0.0, s, 0.0],
            ],
        }
    }

    /// Apply to a position.
    pub fn point(&self, p: Vec3) -> Vec3 {
        Vec3::new(
            self.m[0][0] * p.x + self.m[0][1] * p.y + self.m[0][2] * p.z + self.m[0][3],
            self.m[1][0] * p.x + self.m[1][1] * p.y + self.m[1][2] * p.z + self.m[1][3],
            self.m[2][0] * p.x + self.m[2][1] * p.y + self.m[2][2] * p.z + self.m[2][3],
        )
    }

    /// Apply to a direction, ignoring translation.
    pub fn direction(&self, d: Vec3) -> Vec3 {
        Vec3::new(
            self.m[0][0] * d.x + self.m[0][1] * d.y + self.m[0][2] * d.z,
            self.m[1][0] * d.x + self.m[1][1] * d.y + self.m[1][2] * d.z,
            self.m[2][0] * d.x + self.m[2][1] * d.y + self.m[2][2] * d.z,
        )
    }

    /// `self` then `other`, i.e. `other ∘ self`.
    pub fn then(&self, other: &Transform) -> Transform {
        let mut m = [[0.0f64; 4]; 3];
        for (r, row) in m.iter_mut().enumerate() {
            for (c, cell) in row.iter_mut().enumerate().take(3) {
                *cell = other.m[r][0] * self.m[0][c]
                    + other.m[r][1] * self.m[1][c]
                    + other.m[r][2] * self.m[2][c];
            }
            row[3] = other.m[r][0] * self.m[0][3]
                + other.m[r][1] * self.m[1][3]
                + other.m[r][2] * self.m[2][3]
                + other.m[r][3];
        }
        Transform { m }
    }

    /// Determinant of the linear part.
    ///
    /// A negative value means the transform mirrors, which flips every
    /// triangle's winding — the writers must react to it or the model renders
    /// inside out.
    pub fn determinant(&self) -> f64 {
        let m = &self.m;
        m[0][0] * (m[1][1] * m[2][2] - m[1][2] * m[2][1])
            - m[0][1] * (m[1][0] * m[2][2] - m[1][2] * m[2][0])
            + m[0][2] * (m[1][0] * m[2][1] - m[1][1] * m[2][0])
    }

    pub fn is_mirroring(&self) -> bool {
        self.determinant() < 0.0
    }

    /// True when this is the identity to within `eps`.
    pub fn is_identity(&self, eps: f64) -> bool {
        let id = Transform::IDENTITY;
        self.m
            .iter()
            .flatten()
            .zip(id.m.iter().flatten())
            .all(|(a, b)| (a - b).abs() <= eps)
    }

    /// Column-major 16-element form, the layout glTF and USD both want.
    pub fn to_column_major(&self) -> [f64; 16] {
        let m = &self.m;
        [
            m[0][0], m[1][0], m[2][0], 0.0, //
            m[0][1], m[1][1], m[2][1], 0.0, //
            m[0][2], m[1][2], m[2][2], 0.0, //
            m[0][3], m[1][3], m[2][3], 1.0,
        ]
    }
}

/// An axis-aligned bounding box.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Aabb {
    pub min: Vec3,
    pub max: Vec3,
}

impl Default for Aabb {
    fn default() -> Self {
        Aabb::EMPTY
    }
}

impl Aabb {
    /// The empty box, which absorbs any point correctly.
    pub const EMPTY: Aabb = Aabb {
        min: Vec3::new(f64::INFINITY, f64::INFINITY, f64::INFINITY),
        max: Vec3::new(f64::NEG_INFINITY, f64::NEG_INFINITY, f64::NEG_INFINITY),
    };

    pub fn is_empty(&self) -> bool {
        self.min.x > self.max.x || self.min.y > self.max.y || self.min.z > self.max.z
    }

    pub fn add_point(&mut self, p: Vec3) {
        self.min = self.min.min(p);
        self.max = self.max.max(p);
    }

    pub fn union(&self, o: &Aabb) -> Aabb {
        if self.is_empty() {
            return *o;
        }
        if o.is_empty() {
            return *self;
        }
        Aabb {
            min: self.min.min(o.min),
            max: self.max.max(o.max),
        }
    }

    pub fn size(&self) -> Vec3 {
        if self.is_empty() {
            Vec3::ZERO
        } else {
            self.max - self.min
        }
    }

    pub fn centre(&self) -> Vec3 {
        if self.is_empty() {
            Vec3::ZERO
        } else {
            (self.min + self.max) * 0.5
        }
    }

    /// Length of the box diagonal — the natural scale of a model, and what a
    /// relative tessellation tolerance is measured against.
    pub fn diagonal(&self) -> f64 {
        self.size().length()
    }

    pub fn transformed(&self, t: &Transform) -> Aabb {
        if self.is_empty() {
            return *self;
        }
        let mut out = Aabb::EMPTY;
        for i in 0..8 {
            let c = Vec3::new(
                if i & 1 == 0 { self.min.x } else { self.max.x },
                if i & 2 == 0 { self.min.y } else { self.max.y },
                if i & 4 == 0 { self.min.z } else { self.max.z },
            );
            out.add_point(t.point(c));
        }
        out
    }
}

/// A closed interval of a curve or surface parameter.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Interval {
    pub lo: f64,
    pub hi: f64,
}

impl Interval {
    pub const fn new(lo: f64, hi: f64) -> Interval {
        Interval { lo, hi }
    }

    pub const UNIT: Interval = Interval::new(0.0, 1.0);

    pub fn span(&self) -> f64 {
        self.hi - self.lo
    }

    pub fn at(&self, t: f64) -> f64 {
        self.lo + (self.hi - self.lo) * t
    }

    pub fn contains(&self, t: f64) -> bool {
        t >= self.lo && t <= self.hi
    }
}

pub const TAU: f64 = std::f64::consts::TAU;

#[cfg(test)]
mod tests {
    use super::*;

    fn close(a: Vec3, b: Vec3) -> bool {
        (a - b).length() < 1e-12
    }

    #[test]
    fn frame_orthonormalises_a_skewed_ref_dir() {
        // ref_dir leaning into the axis is common and must be projected out.
        let f = Frame::new(Vec3::ZERO, Vec3::Z, Vec3::new(1.0, 0.0, 0.5));
        assert!(close(f.ref_dir, Vec3::X));
        assert!(close(f.y_dir(), Vec3::Y));
        assert!(f.axis.dot(f.ref_dir).abs() < 1e-15);
    }

    #[test]
    fn frame_survives_a_ref_dir_parallel_to_the_axis() {
        let f = Frame::new(Vec3::ZERO, Vec3::Z, Vec3::Z);
        assert!(f.ref_dir.is_finite());
        assert!((f.ref_dir.length() - 1.0).abs() < 1e-12);
        assert!(f.axis.dot(f.ref_dir).abs() < 1e-12);
    }

    #[test]
    fn frame_polar_walks_the_local_xy_circle() {
        let f = Frame::new(Vec3::new(1.0, 2.0, 3.0), Vec3::Z, Vec3::X);
        assert!(close(f.polar(2.0, 0.0), Vec3::new(3.0, 2.0, 3.0)));
        assert!(close(f.polar(2.0, TAU / 4.0), Vec3::new(1.0, 4.0, 3.0)));
    }

    #[test]
    fn transform_composition_matches_sequential_application() {
        let a = Transform::from_translation(Vec3::new(1.0, 0.0, 0.0));
        let b = Transform::from_frame(&Frame::new(Vec3::ZERO, Vec3::Z, Vec3::Y));
        let p = Vec3::new(2.0, 3.0, 4.0);
        assert!(close(a.then(&b).point(p), b.point(a.point(p))));
    }

    #[test]
    fn from_frame_maps_local_axes_onto_the_frame() {
        let f = Frame::new(Vec3::new(5.0, 0.0, 0.0), Vec3::Y, Vec3::Z);
        let t = Transform::from_frame(&f);
        assert!(close(t.point(Vec3::ZERO), f.origin));
        assert!(close(t.direction(Vec3::X), f.ref_dir));
        assert!(close(t.direction(Vec3::Z), f.axis));
    }

    #[test]
    fn mirroring_is_detected_by_the_determinant() {
        let mut mirror = Transform::IDENTITY;
        mirror.m[0][0] = -1.0;
        assert!(mirror.is_mirroring());
        assert!(!Transform::IDENTITY.is_mirroring());
        assert!(!Transform::from_scale(2.0).is_mirroring());
    }

    #[test]
    fn column_major_puts_translation_in_the_last_column() {
        let t = Transform::from_translation(Vec3::new(7.0, 8.0, 9.0));
        let c = t.to_column_major();
        assert_eq!(&c[12..16], &[7.0, 8.0, 9.0, 1.0]);
    }

    #[test]
    fn an_empty_box_absorbs_its_first_point_exactly() {
        let mut b = Aabb::EMPTY;
        assert!(b.is_empty());
        b.add_point(Vec3::new(1.0, 2.0, 3.0));
        assert!(!b.is_empty());
        assert_eq!(b.min, b.max);
        assert_eq!(b.diagonal(), 0.0);
    }

    #[test]
    fn a_transformed_box_encloses_the_rotated_corners() {
        let b = Aabb {
            min: Vec3::ZERO,
            max: Vec3::new(1.0, 1.0, 1.0),
        };
        let rot = Transform::from_frame(&Frame::new(Vec3::ZERO, Vec3::Z, Vec3::Y));
        let t = b.transformed(&rot);
        assert!((t.size().x - 1.0).abs() < 1e-12);
        assert!((t.diagonal() - b.diagonal()).abs() < 1e-12);
    }

    #[test]
    fn any_perpendicular_is_perpendicular_for_every_axis() {
        for v in [
            Vec3::X,
            Vec3::Y,
            Vec3::Z,
            Vec3::new(1.0, 1.0, 1.0).normalized_or(Vec3::Z),
            Vec3::new(-0.3, 0.9, -0.1).normalized_or(Vec3::Z),
        ] {
            let p = v.any_perpendicular();
            assert!((p.length() - 1.0).abs() < 1e-12);
            assert!(v.dot(p).abs() < 1e-12, "v={v:?} p={p:?}");
        }
    }
}
