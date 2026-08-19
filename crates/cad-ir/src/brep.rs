//! The format-neutral boundary representation.
//!
//! Every reader lowers into this and the tessellator only ever reads this, so
//! adding an input format never touches the tessellator and improving the
//! tessellator never touches a reader.
//!
//! Two decisions shape it:
//!
//! * **Edges are shared, not duplicated per face.** Two faces meeting at an
//!   edge name the same [`EdgeId`]. That is what lets the tessellator
//!   discretise each edge exactly once and hand both faces the identical chain
//!   of points, which is the only way to get a watertight mesh out of
//!   independently triangulated faces.
//! * **Geometry is stored in flat arenas addressed by index.** B-Rep topology
//!   is cyclic — face → loop → edge → face — so a tree of owning pointers
//!   cannot express it and a graph of `Rc<RefCell<…>>` would make the
//!   tessellator's inner loop chase pointers. Indices keep it flat, cheap to
//!   clone, and trivially parallelisable over faces.

use crate::math::{Aabb, Frame, Interval, Vec2, Vec3};

macro_rules! id_type {
    ($($name:ident => $doc:literal),* $(,)?) => {
        $(
            #[doc = $doc]
            #[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
            pub struct $name(pub u32);

            impl $name {
                pub fn index(self) -> usize {
                    self.0 as usize
                }
            }

            impl From<usize> for $name {
                fn from(v: usize) -> Self {
                    $name(v as u32)
                }
            }
        )*
    };
}

id_type! {
    SurfaceId => "Index into [`Solid::surfaces`].",
    CurveId   => "Index into [`Solid::curves`].",
    VertexId  => "Index into [`Solid::vertices`].",
    EdgeId    => "Index into [`Solid::edges`].",
    FaceId    => "Index into [`Solid::faces`].",
}

/// One connected body: a shell or set of shells bounding a volume or sheet.
#[derive(Debug, Clone, Default)]
pub struct Solid {
    /// The part name, from the source file where it has one.
    pub name: String,
    pub body_type: BodyType,
    pub vertices: Vec<Vec3>,
    pub curves: Vec<Curve>,
    pub surfaces: Vec<Surface>,
    pub edges: Vec<Edge>,
    pub faces: Vec<Face>,
    /// Which faces form each shell. The first shell of a solid body is its
    /// outer shell; the rest are voids.
    pub shells: Vec<Shell>,
    /// The tolerance the source file guaranteed its geometry to, in model
    /// units. Tessellating finer than this manufactures detail that is not in
    /// the data.
    pub tolerance: f64,
}

/// What a body bounds.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum BodyType {
    /// A closed shell bounding a volume.
    #[default]
    Solid,
    /// Open or closed faces with no enclosed volume.
    Sheet,
    /// Edges only, no faces.
    Wire,
    /// Isolated vertices.
    Point,
}

/// A set of faces forming one connected boundary.
#[derive(Debug, Clone, Default)]
pub struct Shell {
    pub faces: Vec<FaceId>,
    /// True when the shell is closed — the outer shell of a solid, or a void.
    pub closed: bool,
    /// True when the shell bounds a cavity rather than the body's exterior.
    pub is_void: bool,
}

/// A trimmed patch of a surface.
#[derive(Debug, Clone)]
pub struct Face {
    pub surface: SurfaceId,
    /// False when the face's outward normal is opposite the surface normal.
    ///
    /// Getting this wrong inverts lighting on that face and nothing else, so it
    /// is easy to miss and worth stating: it must be combined with the shell's
    /// own orientation before a triangle's winding is decided.
    pub same_sense: bool,
    /// Trim loops. Exactly one is the outer boundary; the rest are holes.
    pub bounds: Vec<Bound>,
}

/// One closed trim loop on a face.
#[derive(Debug, Clone)]
pub struct Bound {
    /// True for the loop enclosing the face's material, false for a hole.
    pub outer: bool,
    /// The loop's edges in order. An empty list with a `vertex` set is a
    /// degenerate loop at a cone apex or sphere pole.
    pub halves: Vec<HalfEdge>,
    /// Set instead of `halves` for a single-vertex loop.
    pub vertex: Option<VertexId>,
}

/// An edge as traversed by one particular loop.
#[derive(Debug, Clone)]
pub struct HalfEdge {
    pub edge: EdgeId,
    /// True when this loop walks the edge from its start vertex to its end.
    pub forward: bool,
    /// The edge's curve in this face's parameter space, when the file supplies
    /// one.
    ///
    /// A pcurve removes all guesswork from placing the edge in UV, which
    /// matters on periodic surfaces where inverting a 3D point onto the surface
    /// is ambiguous by a full period. When it is absent the tessellator has to
    /// invert, and the seam handling has to be careful.
    pub pcurve: Option<Curve2>,
}

/// A topological edge, shared by the faces that meet along it.
#[derive(Debug, Clone)]
pub struct Edge {
    pub start: VertexId,
    pub end: VertexId,
    pub curve: CurveId,
    /// False when the curve runs from `end` to `start`.
    pub same_sense: bool,
    /// The curve parameters at `start` and `end`.
    ///
    /// Resolved by the reader, because recovering them by inverting the vertex
    /// points is both slower and ambiguous on a closed curve.
    pub range: Interval,
    /// Per-edge tolerance where the source file gives one, else the solid's.
    pub tolerance: f64,
}

/// A 3D curve.
#[derive(Debug, Clone)]
pub enum Curve {
    Line {
        origin: Vec3,
        /// Not normalised: its length is the parameterisation's scale, which
        /// STEP's `VECTOR` magnitude and Parasolid's direction both carry.
        direction: Vec3,
    },
    Circle {
        frame: Frame,
        radius: f64,
    },
    Ellipse {
        frame: Frame,
        /// Along `frame.ref_dir`.
        semi_major: f64,
        /// Along `frame.y_dir()`.
        semi_minor: f64,
    },
    Parabola {
        frame: Frame,
        focal_dist: f64,
    },
    Hyperbola {
        frame: Frame,
        semi_major: f64,
        semi_minor: f64,
    },
    Polyline {
        points: Vec<Vec3>,
    },
    Nurbs(NurbsCurve),
    /// A curve restricted to part of another curve's range.
    Trimmed {
        base: Box<Curve>,
        range: Interval,
    },
    /// Several curves joined end to end, each with its own parameter range.
    Composite {
        segments: Vec<CompositeSegment>,
    },
    /// A curve that only exists on a surface, used where a file gives no
    /// usable 3D form. The tessellator evaluates it through the surface.
    OnSurface {
        surface: SurfaceId,
        pcurve: Curve2,
    },
}

/// One piece of a composite curve.
#[derive(Debug, Clone)]
pub struct CompositeSegment {
    pub curve: Curve,
    pub range: Interval,
    /// False when the segment is traversed against its own parameterisation.
    pub same_sense: bool,
}

/// A curve in a surface's `(u, v)` parameter space.
#[derive(Debug, Clone)]
pub enum Curve2 {
    Line { origin: Vec2, direction: Vec2 },
    Polyline { points: Vec<Vec2> },
    Nurbs(NurbsCurve2),
    /// The pcurve is the 3D curve pushed onto the surface; the tessellator
    /// inverts. Recorded explicitly so a missing pcurve is visible rather than
    /// silently absent.
    Implied,
}

/// A NURBS curve in 3D.
#[derive(Debug, Clone)]
pub struct NurbsCurve {
    pub degree: usize,
    pub control_points: Vec<Vec3>,
    /// Empty for a non-rational curve. Otherwise one weight per control point.
    pub weights: Vec<f64>,
    /// The full knot vector, already expanded by multiplicity.
    pub knots: Vec<f64>,
    pub closed: bool,
}

/// A NURBS curve in parameter space.
#[derive(Debug, Clone)]
pub struct NurbsCurve2 {
    pub degree: usize,
    pub control_points: Vec<Vec2>,
    pub weights: Vec<f64>,
    pub knots: Vec<f64>,
}

/// A surface.
#[derive(Debug, Clone)]
pub enum Surface {
    /// `S(u, v) = origin + u·x + v·y`, normal along `frame.axis`.
    Plane {
        frame: Frame,
    },
    /// `S(u, v) = frame.polar(radius, u) + axis·v`.
    Cylinder {
        frame: Frame,
        radius: f64,
    },
    /// A cone opening along `+axis`. `radius` is measured at the frame origin,
    /// so an apex-at-origin cone has `radius == 0`.
    Cone {
        frame: Frame,
        radius: f64,
        half_angle: f64,
    },
    Sphere {
        frame: Frame,
        radius: f64,
    },
    /// `minor_radius > major_radius` gives the self-intersecting "apple" torus
    /// that STEP writes as `DEGENERATE_TOROIDAL_SURFACE`.
    Torus {
        frame: Frame,
        major_radius: f64,
        minor_radius: f64,
    },
    Nurbs(NurbsSurface),
    /// A curve swept along a straight line.
    LinearExtrusion {
        profile: Box<Curve>,
        direction: Vec3,
    },
    /// A curve revolved about an axis.
    Revolution {
        profile: Box<Curve>,
        frame: Frame,
    },
    /// Another surface displaced along its normal.
    Offset {
        base: Box<Surface>,
        distance: f64,
    },
    /// Another surface restricted to a parameter rectangle.
    RectangularTrimmed {
        base: Box<Surface>,
        u: Interval,
        v: Interval,
    },
}

/// A NURBS surface.
#[derive(Debug, Clone)]
pub struct NurbsSurface {
    pub u_degree: usize,
    pub v_degree: usize,
    /// `control_points[i][j]` is the point at `u` index `i`, `v` index `j`.
    pub control_points: Vec<Vec<Vec3>>,
    /// Empty for a non-rational surface, else the same shape as
    /// `control_points`.
    pub weights: Vec<Vec<f64>>,
    /// Full knot vectors, already expanded by multiplicity.
    pub u_knots: Vec<f64>,
    pub v_knots: Vec<f64>,
    pub u_closed: bool,
    pub v_closed: bool,
}

impl Solid {
    pub fn face(&self, id: FaceId) -> &Face {
        &self.faces[id.index()]
    }

    pub fn edge(&self, id: EdgeId) -> &Edge {
        &self.edges[id.index()]
    }

    pub fn surface(&self, id: SurfaceId) -> &Surface {
        &self.surfaces[id.index()]
    }

    pub fn curve(&self, id: CurveId) -> &Curve {
        &self.curves[id.index()]
    }

    pub fn vertex(&self, id: VertexId) -> Vec3 {
        self.vertices[id.index()]
    }

    /// A bound from the solid's topological vertices alone.
    ///
    /// This is the honest scale of a body. [`Solid::rough_bounds`] also takes in
    /// spline control points, and a rational curve representing a near-degenerate
    /// conic legitimately places a control point thousands of times further out
    /// than the curve ever goes — pinned back by a weight near zero. Sizing a
    /// relative tolerance off that would coarsen the whole model.
    ///
    /// Empty for a body whose faces are all periodic, such as a plain shaft,
    /// which genuinely has no vertices; callers must fall back.
    pub fn vertex_bounds(&self) -> Aabb {
        let mut b = Aabb::EMPTY;
        for v in &self.vertices {
            b.add_point(*v);
        }
        b
    }

    /// A trustworthy bound on where the body actually is.
    ///
    /// Takes the topological vertices and the extents of the analytic conics,
    /// and deliberately takes neither spline control points nor edge parameter
    /// ranges. Control points can sit metres outside a curve they only pull on
    /// through a near-zero weight; parameter ranges are the very thing a caller
    /// wanting this bound is usually trying to check. Conics are included
    /// because a body whose faces are all periodic — a plain shaft, a washer —
    /// has almost no vertices, and its circles are exactly where it is.
    ///
    /// Falls back to [`Solid::rough_bounds`] for a body made only of splines.
    pub fn geometric_bounds(&self) -> Aabb {
        let mut b = self.vertex_bounds();

        // Circles always count: every circle of a B-Rep is an intersection
        // ring sitting on the body, and its extent is bounded by its own
        // radius. They are also the *only* extent witness a turned part has —
        // its edges are all closed circles, whose seam vertices cluster at one
        // angle and make the vertex box a thin sliver of the truth.
        for c in &self.curves {
            if let Curve::Circle { frame, radius } = c {
                let reach = radius.abs();
                if reach.is_finite() && frame.origin.is_finite() {
                    let r = Vec3::new(reach, reach, reach);
                    b.add_point(frame.origin - r);
                    b.add_point(frame.origin + r);
                }
            }
        }

        // Ellipses only when vertices are too few to settle the extent: a
        // cylinder cut by a nearly parallel plane produces an ellipse metres
        // across of which the model uses a millimetre, and taking its full
        // extent would drag the reference far outside the real body.
        if self.vertices.len() < 8 {
            for c in &self.curves {
                if let Curve::Ellipse {
                    frame,
                    semi_major,
                    semi_minor,
                } = c
                {
                    let reach = semi_major.abs().max(semi_minor.abs());
                    if reach.is_finite() && frame.origin.is_finite() {
                        let r = Vec3::new(reach, reach, reach);
                        b.add_point(frame.origin - r);
                        b.add_point(frame.origin + r);
                    }
                }
            }
        }
        if b.is_empty() { self.rough_bounds() } else { b }
    }

    /// A bound on the solid, from its vertices and any explicit control points.
    ///
    /// Cheap and conservative: it does not evaluate curves or surfaces, so a
    /// bulging spline may extend slightly past it. That is fine for its only
    /// job, which is setting the scale a relative tolerance is measured
    /// against.
    pub fn rough_bounds(&self) -> Aabb {
        let mut b = Aabb::EMPTY;
        for v in &self.vertices {
            b.add_point(*v);
        }
        for c in &self.curves {
            match c {
                Curve::Polyline { points } => points.iter().for_each(|p| b.add_point(*p)),
                Curve::Nurbs(n) => n.control_points.iter().for_each(|p| b.add_point(*p)),
                _ => {}
            }
        }
        for s in &self.surfaces {
            if let Surface::Nurbs(n) = s {
                for row in &n.control_points {
                    row.iter().for_each(|p| b.add_point(*p));
                }
            }
        }
        b
    }

    /// Every face reachable through the shells, in shell order.
    ///
    /// Faces not referenced by any shell are unreachable geometry and are not
    /// returned; readers should not produce them.
    pub fn shell_faces(&self) -> impl Iterator<Item = (usize, FaceId)> + '_ {
        self.shells
            .iter()
            .enumerate()
            .flat_map(|(i, s)| s.faces.iter().map(move |f| (i, *f)))
    }

    /// Total number of half-edges, a good proxy for tessellation cost.
    pub fn half_edge_count(&self) -> usize {
        self.faces
            .iter()
            .flat_map(|f| f.bounds.iter())
            .map(|b| b.halves.len())
            .sum()
    }
}

impl Face {
    /// The outer trim loop, if the reader identified one.
    pub fn outer_bound(&self) -> Option<&Bound> {
        self.bounds.iter().find(|b| b.outer)
    }

    /// The hole loops.
    pub fn inner_bounds(&self) -> impl Iterator<Item = &Bound> {
        self.bounds.iter().filter(|b| !b.outer)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ids_round_trip_through_usize() {
        let f: FaceId = 7usize.into();
        assert_eq!(f.index(), 7);
        assert_eq!(f, FaceId(7));
    }

    #[test]
    fn rough_bounds_covers_vertices_and_control_points() {
        let mut s = Solid {
            vertices: vec![Vec3::ZERO, Vec3::new(1.0, 1.0, 1.0)],
            ..Default::default()
        };
        s.curves.push(Curve::Polyline {
            points: vec![Vec3::new(-2.0, 0.0, 0.0)],
        });
        let b = s.rough_bounds();
        assert_eq!(b.min, Vec3::new(-2.0, 0.0, 0.0));
        assert_eq!(b.max, Vec3::new(1.0, 1.0, 1.0));
    }

    #[test]
    fn geometric_bounds_covers_a_body_that_has_no_vertices() {
        use crate::math::Frame;
        // A plain shaft: two circles, no vertices at all.
        let s = Solid {
            curves: vec![
                Curve::Circle {
                    frame: Frame::new(Vec3::ZERO, Vec3::Z, Vec3::X),
                    radius: 10.0,
                },
                Curve::Circle {
                    frame: Frame::new(Vec3::new(0.0, 0.0, 25.0), Vec3::Z, Vec3::X),
                    radius: 10.0,
                },
            ],
            ..Default::default()
        };
        assert!(s.vertex_bounds().is_empty());
        let b = s.geometric_bounds();
        // Falling back to the conics is exactly what a body with no vertices
        // needs, and only such a body gets it.
        assert!(!b.is_empty());
        assert!(b.size().x >= 20.0, "got {:?}", b.size());
        assert!(b.size().z >= 25.0, "got {:?}", b.size());
    }

    #[test]
    fn geometric_bounds_prefers_vertices_once_there_are_enough() {
        use crate::math::Frame;
        let s = Solid {
            vertices: (0..12)
                .map(|i| Vec3::new(i as f64, 0.0, 0.0))
                .collect(),
            // A vast near-degenerate ellipse the body barely touches.
            curves: vec![Curve::Ellipse {
                frame: Frame::new(Vec3::new(0.0, 0.0, 5000.0), Vec3::Z, Vec3::X),
                semi_major: 4000.0,
                semi_minor: 0.1,
            }],
            ..Default::default()
        };
        assert!(s.geometric_bounds().diagonal() < 20.0, "{:?}", s.geometric_bounds());
    }

    #[test]
    fn geometric_bounds_ignores_a_far_flung_control_point() {
        let s = Solid {
            vertices: vec![Vec3::ZERO, Vec3::new(10.0, 0.0, 0.0)],
            curves: vec![Curve::Nurbs(NurbsCurve {
                degree: 2,
                // The middle control point of a near-degenerate rational conic,
                // held back by a weight close to zero.
                control_points: vec![
                    Vec3::ZERO,
                    Vec3::new(5000.0, 0.0, 0.0),
                    Vec3::new(10.0, 0.0, 0.0),
                ],
                weights: vec![1.0, 1e-4, 1.0],
                knots: vec![0.0, 0.0, 0.0, 1.0, 1.0, 1.0],
                closed: false,
            })],
            ..Default::default()
        };
        assert!(s.rough_bounds().size().x > 4000.0);
        assert!(s.geometric_bounds().size().x < 11.0);
    }

    #[test]
    fn rough_bounds_of_an_empty_solid_is_empty() {
        assert!(Solid::default().rough_bounds().is_empty());
    }

    #[test]
    fn a_face_separates_its_outer_loop_from_its_holes() {
        let f = Face {
            surface: SurfaceId(0),
            same_sense: true,
            bounds: vec![
                Bound {
                    outer: false,
                    halves: vec![],
                    vertex: None,
                },
                Bound {
                    outer: true,
                    halves: vec![],
                    vertex: None,
                },
            ],
        };
        assert!(f.outer_bound().is_some());
        assert_eq!(f.inner_bounds().count(), 1);
    }

    #[test]
    fn shell_faces_walks_every_shell() {
        let s = Solid {
            faces: vec![],
            shells: vec![
                Shell {
                    faces: vec![FaceId(0), FaceId(1)],
                    closed: true,
                    is_void: false,
                },
                Shell {
                    faces: vec![FaceId(2)],
                    closed: true,
                    is_void: true,
                },
            ],
            ..Default::default()
        };
        let got: Vec<_> = s.shell_faces().collect();
        assert_eq!(got, vec![(0, FaceId(0)), (0, FaceId(1)), (1, FaceId(2))]);
    }

    #[test]
    fn half_edge_count_sums_every_loop() {
        let he = || HalfEdge {
            edge: EdgeId(0),
            forward: true,
            pcurve: None,
        };
        let s = Solid {
            faces: vec![Face {
                surface: SurfaceId(0),
                same_sense: true,
                bounds: vec![
                    Bound {
                        outer: true,
                        halves: (0..4).map(|_| he()).collect(),
                        vertex: None,
                    },
                    Bound {
                        outer: false,
                        halves: (0..3).map(|_| he()).collect(),
                        vertex: None,
                    },
                ],
            }],
            ..Default::default()
        };
        assert_eq!(s.half_edge_count(), 7);
    }
}
