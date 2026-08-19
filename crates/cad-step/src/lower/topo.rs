//! Lowering STEP topology into a [`cad_ir::Solid`].
//!
//! The work that is not a straight field copy:
//!
//! * **Edge parameter ranges.** STEP names an edge's end *vertices*, never the
//!   parameters at which they sit on the curve. Those have to be recovered by
//!   inversion, and on a periodic curve the answer is ambiguous by a full
//!   period — an edge from 350° to 10° reads as a 340° arc going the wrong way
//!   unless the wrap is handled deliberately.
//! * **Loop direction.** Three separate flags compose: `FACE_BOUND.orientation`
//!   reverses the loop, `ORIENTED_EDGE.orientation` reverses an edge within it,
//!   and `EDGE_CURVE.same_sense` reverses the curve under the edge. Getting the
//!   product wrong turns a hole inside out.
//! * **Pcurve selection.** A `SURFACE_CURVE` carries a pcurve for each face it
//!   borders, so the right one has to be picked by matching the basis surface —
//!   taking the first would put half the trim loops in the wrong face's
//!   parameter space.
//! * **Deduplication.** Adjacent faces name the same `EDGE_CURVE`, and the
//!   whole point of the IR's shared edges is to keep it that way, so every
//!   sub-entity is interned by its STEP instance id.

use crate::error::{Result, StepError};
use crate::kind::Kind;
use crate::lower::geom;
use crate::StepFile;
use cad_ir::brep::{
    Bound, BodyType, Curve, Curve2, CurveId, Edge, EdgeId, Face, FaceId, HalfEdge, Shell, Solid,
    Surface, SurfaceId, VertexId,
};
use cad_ir::math::{Interval, TAU, Vec3};
use rustc_hash::FxHashMap;

/// Builds one [`Solid`] out of a STEP shape, interning shared sub-entities.
pub struct SolidBuilder<'a> {
    file: &'a StepFile,
    solid: Solid,
    surfaces: FxHashMap<u32, SurfaceId>,
    curves: FxHashMap<u32, CurveId>,
    vertices: FxHashMap<u32, VertexId>,
    edges: FxHashMap<u32, EdgeId>,
    faces: FxHashMap<u32, FaceId>,
    /// Entities that could not be lowered, with why. Collected rather than
    /// raised so one unsupported surface loses one face, not the whole part.
    pub skipped: Vec<Skip>,
}

/// A sub-entity that was dropped, and the reason.
#[derive(Debug, Clone)]
pub struct Skip {
    pub entity: u32,
    pub reason: String,
}

impl<'a> SolidBuilder<'a> {
    pub fn new(file: &'a StepFile, tolerance: f64) -> Self {
        SolidBuilder {
            file,
            solid: Solid {
                tolerance,
                ..Default::default()
            },
            surfaces: FxHashMap::default(),
            curves: FxHashMap::default(),
            vertices: FxHashMap::default(),
            edges: FxHashMap::default(),
            faces: FxHashMap::default(),
            skipped: Vec::new(),
        }
    }

    /// Lower a shape-representation item into the solid under construction.
    ///
    /// Accepts `MANIFOLD_SOLID_BREP`, `BREP_WITH_VOIDS`,
    /// `SHELL_BASED_SURFACE_MODEL` and a bare shell, which is what the various
    /// AP214 shape representations actually contain.
    pub fn add_item(&mut self, id: u32) -> Result<bool> {
        let e = self.file.require(id)?;
        match e.kind {
            Kind::ManifoldSolidBrep => {
                let mut a = self.file.args_of(e);
                self.solid.name = a.next_str()?.into_owned();
                let outer = a.next_ref()?;
                self.add_shell(outer, false)?;
                self.solid.body_type = BodyType::Solid;
                Ok(true)
            }
            Kind::BrepWithVoids => {
                let mut a = self.file.args_of(e);
                self.solid.name = a.next_str()?.into_owned();
                let outer = a.next_ref()?;
                self.add_shell(outer, false)?;
                let mut voids = Vec::new();
                a.next_ref_list(&mut voids)?;
                for v in voids {
                    // Each void is an ORIENTED_CLOSED_SHELL wrapping a shell.
                    let shell = self.unwrap_oriented_shell(v)?;
                    self.add_shell(shell, true)?;
                }
                self.solid.body_type = BodyType::Solid;
                Ok(true)
            }
            Kind::ShellBasedSurfaceModel => {
                let mut a = self.file.args_of(e);
                self.solid.name = a.next_str()?.into_owned();
                let mut shells = Vec::new();
                a.next_ref_list(&mut shells)?;
                for s in shells {
                    let s = self.unwrap_oriented_shell(s)?;
                    self.add_shell(s, false)?;
                }
                self.solid.body_type = BodyType::Sheet;
                Ok(true)
            }
            Kind::ClosedShell | Kind::OpenShell => {
                let closed = e.kind == Kind::ClosedShell;
                self.add_shell(id, false)?;
                self.solid.body_type = if closed {
                    BodyType::Solid
                } else {
                    BodyType::Sheet
                };
                Ok(true)
            }
            // Axis placements, styling anchors and annotation items all appear
            // in a shape representation's item list; they carry no geometry.
            _ => Ok(false),
        }
    }

    /// Finish, returning the solid.
    pub fn finish(self) -> (Solid, Vec<Skip>) {
        (self.solid, self.skipped)
    }

    /// The STEP instance id each face came from, for style lookup.
    pub fn face_sources(&self) -> Vec<(FaceId, u32)> {
        let mut v: Vec<(FaceId, u32)> = self.faces.iter().map(|(&k, &f)| (f, k)).collect();
        v.sort_by_key(|(f, _)| f.0);
        v
    }

    fn unwrap_oriented_shell(&self, id: u32) -> Result<u32> {
        match self.file.kind_of(id) {
            Kind::OrientedClosedShell => {
                let mut a = self.file.args(id)?;
                a.skip()?; // name
                let base = a.next_ref()?;
                Ok(base)
            }
            _ => Ok(id),
        }
    }

    fn add_shell(&mut self, id: u32, is_void: bool) -> Result<()> {
        let e = self.file.require(id)?;
        let closed = e.kind == Kind::ClosedShell;
        let mut a = self.file.args_of(e);
        a.skip()?; // name
        let mut face_refs = Vec::new();
        a.next_ref_list(&mut face_refs)?;

        let mut faces = Vec::with_capacity(face_refs.len());
        for f in face_refs {
            match self.add_face(f) {
                Ok(Some(fid)) => faces.push(fid),
                Ok(None) => {}
                Err(err) => self.skipped.push(Skip {
                    entity: f,
                    reason: err.to_string(),
                }),
            }
        }
        self.solid.shells.push(Shell {
            faces,
            closed,
            is_void,
        });
        Ok(())
    }

    fn add_face(&mut self, id: u32) -> Result<Option<FaceId>> {
        if let Some(&existing) = self.faces.get(&id) {
            return Ok(Some(existing));
        }
        let e = self.file.require(id)?;
        // ORIENTED_FACE wraps another face and flips it.
        if e.kind == Kind::OrientedFace {
            let mut a = self.file.args_of(e);
            a.skip()?; // name
            let base = a.next_ref()?;
            let orientation = {
                let mut b = self.file.args_of(e);
                b.skip_n(2)?;
                b.next_bool()?.unwrap_or(true)
            };
            let inner = self.add_face(base)?;
            if let (Some(fid), false) = (inner, orientation) {
                let f = &mut self.solid.faces[fid.index()];
                f.same_sense = !f.same_sense;
            }
            return Ok(inner);
        }
        if !matches!(e.kind, Kind::AdvancedFace | Kind::FaceSurface) {
            return Ok(None);
        }

        let mut a = self.file.args_of(e);
        a.skip()?; // name
        let mut bound_refs = Vec::new();
        a.next_ref_list(&mut bound_refs)?;
        let surface_ref = a.next_ref()?;
        let same_sense = a.next_bool()?.unwrap_or(true);

        let surface = self.intern_surface(surface_ref)?;

        let mut bounds = Vec::with_capacity(bound_refs.len());
        for b in bound_refs {
            match self.build_bound(b, surface_ref) {
                Ok(Some(bound)) => bounds.push(bound),
                Ok(None) => {}
                Err(err) => self.skipped.push(Skip {
                    entity: b,
                    reason: err.to_string(),
                }),
            }
        }
        if bounds.is_empty() {
            return Err(StepError::Record {
                offset: 0,
                detail: format!("#{id} has no usable bounds"),
            });
        }
        // Exactly one bound must be the outer one. Where a file marks none —
        // legal for a fully periodic face like a whole cylinder — the first is
        // taken, which is what its own loop ordering already implies.
        if !bounds.iter().any(|b| b.outer) {
            bounds[0].outer = true;
        }

        let fid = FaceId(self.solid.faces.len() as u32);
        self.solid.faces.push(Face {
            surface,
            same_sense,
            bounds,
        });
        self.faces.insert(id, fid);
        Ok(Some(fid))
    }

    /// `FACE_BOUND(name, bound, orientation)` / `FACE_OUTER_BOUND(…)`.
    fn build_bound(&mut self, id: u32, face_surface: u32) -> Result<Option<Bound>> {
        let e = self.file.require(id)?;
        let outer = e.kind == Kind::FaceOuterBound;
        if !matches!(e.kind, Kind::FaceBound | Kind::FaceOuterBound) {
            return Ok(None);
        }
        let mut a = self.file.args_of(e);
        a.skip()?; // name
        let loop_ref = a.next_ref()?;
        let orientation = a.next_bool()?.unwrap_or(true);

        let loop_e = self.file.require(loop_ref)?;
        match loop_e.kind {
            Kind::VertexLoop => {
                let mut la = self.file.args_of(loop_e);
                la.skip()?; // name
                let v = self.intern_vertex(la.next_ref()?)?;
                Ok(Some(Bound {
                    outer,
                    halves: Vec::new(),
                    vertex: Some(v),
                }))
            }
            Kind::EdgeLoop => {
                let mut la = self.file.args_of(loop_e);
                la.skip()?; // name
                let mut oriented = Vec::new();
                la.next_ref_list(&mut oriented)?;

                let mut halves = Vec::with_capacity(oriented.len());
                for oe in oriented {
                    if let Some(h) = self.build_half_edge(oe, face_surface)? {
                        halves.push(h);
                    }
                }
                // A reversed bound walks the same edges backwards, so the list
                // reverses *and* every edge's own direction flips.
                if !orientation {
                    halves.reverse();
                    for h in &mut halves {
                        h.forward = !h.forward;
                    }
                }
                if halves.is_empty() {
                    return Ok(None);
                }
                Ok(Some(Bound {
                    outer,
                    halves,
                    vertex: None,
                }))
            }
            _ => Ok(None),
        }
    }

    /// `ORIENTED_EDGE(name, edge_start, edge_end, edge_element, orientation)`.
    ///
    /// The two vertex attributes are derived — written `*` — because they are
    /// the underlying edge's, reordered by `orientation`.
    fn build_half_edge(&mut self, id: u32, face_surface: u32) -> Result<Option<HalfEdge>> {
        let e = self.file.require(id)?;
        if e.kind != Kind::OrientedEdge {
            return Ok(None);
        }
        let mut a = self.file.args_of(e);
        a.skip_n(3)?; // name, edge_start, edge_end
        let edge_ref = a.next_ref()?;
        let forward = a.next_bool()?.unwrap_or(true);

        let edge = self.intern_edge(edge_ref)?;
        let pcurve = self.pcurve_for(edge_ref, face_surface)?;
        Ok(Some(HalfEdge {
            edge,
            forward,
            pcurve,
        }))
    }

    /// The pcurve of `edge` in the parameter space of `face_surface`.
    ///
    /// `SURFACE_CURVE(name, curve_3d, associated_geometry, master)` lists one
    /// pcurve per bordering face; matching on the basis surface is what picks
    /// the right one.
    fn pcurve_for(&mut self, edge_ref: u32, face_surface: u32) -> Result<Option<Curve2>> {
        let mut a = self.file.args(edge_ref)?;
        a.skip()?; // name
        a.skip_n(2)?; // start, end vertices
        let Ok(geom_ref) = a.next_ref() else {
            return Ok(None);
        };
        if !matches!(
            self.file.kind_of(geom_ref),
            Kind::SurfaceCurve | Kind::SeamCurve | Kind::IntersectionCurve
        ) {
            return Ok(None);
        }

        let mut sc = self.file.args(geom_ref)?;
        sc.skip()?; // name
        sc.skip()?; // curve_3d
        let mut assoc = Vec::new();
        sc.next_ref_list(&mut assoc)?;

        for g in assoc {
            if self.file.kind_of(g) != Kind::Pcurve {
                continue;
            }
            let mut pa = self.file.args(g)?;
            pa.skip()?; // name
            let basis = pa.next_ref()?;
            if basis != face_surface {
                continue;
            }
            return geom::pcurve(self.file, g);
        }
        Ok(None)
    }

    fn intern_surface(&mut self, id: u32) -> Result<SurfaceId> {
        if let Some(&s) = self.surfaces.get(&id) {
            return Ok(s);
        }
        let surface = geom::surface(self.file, id)?;
        let sid = SurfaceId(self.solid.surfaces.len() as u32);
        self.solid.surfaces.push(surface);
        self.surfaces.insert(id, sid);
        Ok(sid)
    }

    fn intern_curve(&mut self, id: u32) -> Result<CurveId> {
        if let Some(&c) = self.curves.get(&id) {
            return Ok(c);
        }
        let curve = geom::curve(self.file, id)?;
        let cid = CurveId(self.solid.curves.len() as u32);
        self.solid.curves.push(curve);
        self.curves.insert(id, cid);
        Ok(cid)
    }

    fn intern_vertex(&mut self, id: u32) -> Result<VertexId> {
        if let Some(&v) = self.vertices.get(&id) {
            return Ok(v);
        }
        let mut a = self.file.args_checked(id, Kind::VertexPoint)?;
        a.skip()?; // name
        let p = geom::point(self.file, a.next_ref()?)?;
        let vid = VertexId(self.solid.vertices.len() as u32);
        self.solid.vertices.push(p);
        self.vertices.insert(id, vid);
        Ok(vid)
    }

    /// `EDGE_CURVE(name, edge_start, edge_end, edge_geometry, same_sense)`.
    fn intern_edge(&mut self, id: u32) -> Result<EdgeId> {
        if let Some(&e) = self.edges.get(&id) {
            return Ok(e);
        }
        let mut a = self.file.args_checked(id, Kind::EdgeCurve)?;
        a.skip()?; // name
        let start = self.intern_vertex(a.next_ref()?)?;
        let end = self.intern_vertex(a.next_ref()?)?;
        let curve_ref = a.next_ref()?;
        let same_sense = a.next_bool()?.unwrap_or(true);
        let curve = self.intern_curve(curve_ref)?;

        let range = self.edge_range(curve, start, end);

        let eid = EdgeId(self.solid.edges.len() as u32);
        self.solid.edges.push(Edge {
            start,
            end,
            curve,
            same_sense,
            range,
            tolerance: self.solid.tolerance,
        });
        self.edges.insert(id, eid);
        Ok(eid)
    }

    /// Recover the curve parameters of an edge's two vertices.
    fn edge_range(&self, curve: CurveId, start: VertexId, end: VertexId) -> Interval {
        let c = &self.solid.curves[curve.index()];
        let natural = c.natural_range();
        let p0 = self.solid.vertices[start.index()];
        let p1 = self.solid.vertices[end.index()];

        let Some(t0) = c.param_of(p0, Some(natural.lo)) else {
            return natural;
        };
        let Some(t1) = c.param_of(p1, Some(t0)) else {
            return natural;
        };

        // Both ends at the same point means the edge closes on itself: a seam,
        // or a whole circle written as one edge.
        let vertex_tol = (self.solid.tolerance * 10.0).max(1e-9);
        let closes_on_itself = (p1 - p0).length_squared() <= vertex_tol * vertex_tol;

        match c.period() {
            Some(period) => {
                if closes_on_itself {
                    Interval::new(t0, t0 + period)
                } else {
                    // Anything else advances forward from t0, so a t1 at or
                    // below it has wrapped past the seam.
                    let mut hi = t1;
                    while hi <= t0 + 1e-12 {
                        hi += period;
                    }
                    Interval::new(t0, hi)
                }
            }
            None => {
                // A spline can close geometrically without being periodic — a
                // full circle exported as one B-spline is the common case. Its
                // two vertices then invert to the same parameter, and taking
                // that literally would collapse the edge to nothing. Such an
                // edge spans the curve's whole domain.
                let span = Interval::new(t0.min(t1), t0.max(t1));
                let degenerate = span.span() <= 1e-12 * natural.span().abs().max(1.0);
                if degenerate && closes_on_itself && self.curve_closes(c, natural, vertex_tol) {
                    natural
                } else {
                    span
                }
            }
        }
    }

    /// True when a curve's two domain ends land on the same point.
    fn curve_closes(&self, c: &Curve, natural: Interval, tol: f64) -> bool {
        (c.point_at(natural.hi) - c.point_at(natural.lo)).length_squared() <= tol * tol
    }
}

/// True for the STEP entity kinds [`SolidBuilder::add_item`] accepts.
pub fn is_shape_item(kind: Kind) -> bool {
    matches!(
        kind,
        Kind::ManifoldSolidBrep
            | Kind::BrepWithVoids
            | Kind::ShellBasedSurfaceModel
            | Kind::ClosedShell
            | Kind::OpenShell
    )
}

/// The full turn, exposed so callers can reason about periodic edges.
pub const FULL_TURN: f64 = TAU;

/// Sanity-check a lowered solid, returning human-readable complaints.
///
/// Not an error path — a model that fails these is still worth converting, and
/// saying what is wrong beats refusing to produce anything.
pub fn diagnose(solid: &Solid) -> Vec<String> {
    let mut out = Vec::new();
    if solid.faces.is_empty() {
        out.push("solid has no faces".into());
    }
    for (i, f) in solid.faces.iter().enumerate() {
        if f.bounds.iter().filter(|b| b.outer).count() > 1 {
            out.push(format!("face {i} has more than one outer bound"));
        }
        if f.surface.index() >= solid.surfaces.len() {
            out.push(format!("face {i} names surface {} out of range", f.surface.0));
        }
    }
    for (i, e) in solid.edges.iter().enumerate() {
        if !e.range.span().is_finite() || e.range.span() <= 0.0 {
            out.push(format!("edge {i} has a degenerate range {:?}", e.range));
        }
    }
    // Every edge should be used by exactly two half-edges in a closed solid;
    // one means an open boundary, more than two means a non-manifold junction.
    let mut uses = vec![0usize; solid.edges.len()];
    for f in &solid.faces {
        for b in &f.bounds {
            for h in &b.halves {
                if let Some(u) = uses.get_mut(h.edge.index()) {
                    *u += 1;
                }
            }
        }
    }
    let dangling = uses.iter().filter(|&&u| u == 1).count();
    let non_manifold = uses.iter().filter(|&&u| u > 2).count();
    if solid.body_type == BodyType::Solid && dangling > 0 {
        out.push(format!("{dangling} edges are used by only one face"));
    }
    if non_manifold > 0 {
        out.push(format!("{non_manifold} edges are used by more than two faces"));
    }
    out
}

/// A tolerance-scaled comparison for coincident points.
pub fn same_point(a: Vec3, b: Vec3, tolerance: f64) -> bool {
    (a - b).length_squared() <= tolerance * tolerance
}

/// Re-exported so callers can name the surface type without depending on
/// `cad_ir::brep` directly.
pub type LoweredSurface = Surface;
pub type LoweredCurve = Curve;
