//! Walking the XT topology chains into a [`cad_ir::Solid`].
//!
//! The chain layout, from sch_13006 (field indices verified against the whole
//! sample corpus by the original parser's STEP cross-validation):
//!
//! ```text
//! BODY → shell → FACE.next chain → LOOP.next chain → FIN cycle → EDGE → VERTEX
//! ```
//!
//! Two things XT does *not* store have to be reconstructed:
//!
//! * **Edge end points.** An XT edge names no vertices; each FIN carries the
//!   vertex the loop *enters* the edge at, so an edge's two ends are its fin's
//!   vertex and the next fin's vertex. A fin with no vertex at all is a closed
//!   edge — a full circle — and gets a synthetic vertex at its curve's seam.
//! * **Edge parameter ranges.** Recovered the same way the STEP reader does:
//!   a TRIMMED_CURVE hands the interval over directly, everything else is
//!   inverted from the end points via [`cad_ir::eval::curve::recover_edge_range`].
//!
//! BODY field positions vary by schema generation (23, 27, 34 and 36-field
//! layouts exist in the corpus), so they are not hard-coded: the shell and
//! region pointers are found by *probing* — a candidate field qualifies only if
//! it points at an entity of the right type. That works for every generation at
//! once and fails loudly on a layout whose pointers resolve nowhere.

use crate::geom::{self, Index};
use cad_ir::brep::{
    BodyType, Bound, Curve, CurveId, Edge, EdgeId, Face, FaceId, HalfEdge, Shell, Solid,
    SurfaceId, VertexId,
};
use cad_ir::eval::curve::recover_edge_range;
use cad_ir::math::{Interval, Vec3};
use rustc_hash::FxHashMap;
use xt_parser::entity::RawEntity;
use xt_parser::schema as xt;

/// A dropped sub-entity and why.
#[derive(Debug, Clone)]
pub struct Skip {
    pub entity: usize,
    pub reason: String,
}

/// One lowered body plus its bookkeeping.
pub struct LoweredBody {
    pub solid: Solid,
    /// XT face handle for each [`FaceId`], for attribute lookup.
    pub face_sources: Vec<usize>,
    /// XT body handle.
    pub body_handle: usize,
    pub skipped: Vec<Skip>,
}

fn ptr(e: &RawEntity, i: usize) -> usize {
    e.fields.get(i).map(|f| f.as_ptr()).unwrap_or(0)
}

fn chr(e: &RawEntity, i: usize) -> char {
    e.fields.get(i).map(|f| f.as_char()).unwrap_or('+')
}

fn f64_at(e: &RawEntity, i: usize) -> f64 {
    e.fields.get(i).map(|f| f.as_f64()).unwrap_or(f64::NAN)
}

/// Lower every BODY in the entity list.
pub fn lower_bodies(entities: &[RawEntity], tolerance: f64) -> Vec<LoweredBody> {
    let index: Index = entities.iter().map(|e| (e.index, e)).collect();
    entities
        .iter()
        .filter(|e| e.type_id == xt::BODY)
        .map(|body| lower_body(body, &index, tolerance))
        .collect()
}

fn lower_body(body: &RawEntity, index: &Index, default_tol: f64) -> LoweredBody {
    let mut b = Lowering {
        index,
        solid: Solid {
            tolerance: default_tol,
            ..Default::default()
        },
        surfaces: FxHashMap::default(),
        curves: FxHashMap::default(),
        vertices: FxHashMap::default(),
        edges: FxHashMap::default(),
        face_sources: Vec::new(),
        skipped: Vec::new(),
    };

    // The body's shell and region pointers, found by probing: any pointer
    // field whose target is a SHELL / REGION. Probing instead of fixed indices
    // is what makes the 23-, 27-, 34- and 36-field BODY layouts all work.
    let mut shells: Vec<usize> = Vec::new();
    let mut body_type_candidates: Vec<u8> = Vec::new();
    for f in &body.fields {
        let p = f.as_ptr();
        if p != 0 {
            if let Some(t) = index.get(&p) {
                match t.type_id {
                    xt::SHELL => shells.push(p),
                    xt::REGION => {
                        // Follow the region chain; each region may own a shell.
                        let mut r = p;
                        let mut seen = rustc_hash::FxHashSet::default();
                        while r != 0 && seen.insert(r) {
                            let Some(re) = index.get(&r) else { break };
                            if re.type_id != xt::REGION {
                                break;
                            }
                            let s = ptr(re, 5);
                            if s != 0 && index.get(&s).is_some_and(|e| e.type_id == xt::SHELL) {
                                shells.push(s);
                            }
                            r = ptr(re, 3);
                        }
                    }
                    _ => {}
                }
            }
        }
        if let xt_parser::entity::FieldVal::Byte(v) = f {
            body_type_candidates.push(*v);
        }
    }
    shells.sort_unstable();
    shells.dedup();

    // Body type: the byte field holding a known code. 1=solid, 3 behaves as a
    // sheet in the corpus, 7=sheet, 12=wire.
    b.solid.body_type = body_type_candidates
        .iter()
        .find_map(|&v| match v {
            1 => Some(BodyType::Solid),
            3 | 7 => Some(BodyType::Sheet),
            12 => Some(BodyType::Wire),
            _ => None,
        })
        .unwrap_or(BodyType::Solid);

    for shell in shells {
        b.lower_shell(shell);
    }

    LoweredBody {
        face_sources: b.face_sources,
        skipped: b.skipped,
        solid: b.solid,
        body_handle: body.index,
    }
}

struct Lowering<'a> {
    index: &'a Index<'a>,
    solid: Solid,
    surfaces: FxHashMap<usize, SurfaceId>,
    curves: FxHashMap<usize, CurveId>,
    vertices: FxHashMap<usize, VertexId>,
    /// Edge handle → (id, built_reversed). `built_reversed` is set when the
    /// stored curve runs along the fin that *created* the edge rather than the
    /// `+` convention — the tolerant fallback samples the creating fin's own
    /// parameter curve — and tells later fins to flip their traversal.
    edges: FxHashMap<usize, (EdgeId, bool)>,
    face_sources: Vec<usize>,
    skipped: Vec<Skip>,
}

impl<'a> Lowering<'a> {
    fn skip(&mut self, entity: usize, reason: impl Into<String>) {
        self.skipped.push(Skip {
            entity,
            reason: reason.into(),
        });
    }

    fn lower_shell(&mut self, shell: usize) {
        let Some(se) = self.index.get(&shell) else {
            return;
        };
        // SHELL: [3]=next is unused here (bodies list their shells), [4]=face.
        let mut face = ptr(se, 4);
        // A shell whose face pointer collides with a non-FACE entity: scan for
        // a face whose shell back-pointer ([6]) is this shell.
        if !self.index.get(&face).is_some_and(|e| e.type_id == xt::FACE) {
            face = self
                .index
                .values()
                .find(|e| e.type_id == xt::FACE && ptr(e, 6) == shell)
                .map(|e| e.index)
                .unwrap_or(0);
        }

        let mut faces = Vec::new();
        let mut seen = rustc_hash::FxHashSet::default();
        while face != 0 && seen.insert(face) {
            let Some(fe) = self.index.get(&face).filter(|e| e.type_id == xt::FACE) else {
                break;
            };
            match self.lower_face(fe) {
                Ok(Some(fid)) => faces.push(fid),
                Ok(None) => {}
                Err(reason) => self.skip(face, reason),
            }
            face = ptr(fe, 3);
        }

        self.solid.shells.push(Shell {
            faces,
            closed: self.solid.body_type == BodyType::Solid,
            is_void: false,
        });
    }

    fn lower_face(&mut self, fe: &RawEntity) -> Result<Option<FaceId>, String> {
        // FACE: [2]=tolerance, [5]=loop, [7]=surface, [8]=sense.
        let surface_ptr = ptr(fe, 7);
        if surface_ptr == 0 {
            return Err("face has no surface".into());
        }
        let surface = self.intern_surface(surface_ptr)?;

        // The face sense char composes with the surface's own sense char.
        let face_reversed = matches!(chr(fe, 8), 'R' | '-');
        let geom_reversed = self
            .index
            .get(&surface_ptr)
            .map(|se| geom::geom_sense(se) == '-')
            .unwrap_or(false);
        let same_sense = !(face_reversed ^ geom_reversed);

        let mut bounds = Vec::new();
        let mut lp = ptr(fe, 5);
        let mut seen = rustc_hash::FxHashSet::default();
        while lp != 0 && seen.insert(lp) {
            let Some(le) = self.index.get(&lp).filter(|e| e.type_id == xt::LOOP) else {
                break;
            };
            match self.lower_loop(le) {
                Ok(Some(bound)) => bounds.push(bound),
                Ok(None) => {}
                Err(reason) => self.skip(lp, reason),
            }
            lp = ptr(le, 4);
        }
        if bounds.is_empty() {
            return Err("face has no usable loops".into());
        }
        bounds[0].outer = true;

        let fid = FaceId(self.solid.faces.len() as u32);
        self.solid.faces.push(Face {
            surface,
            same_sense,
            bounds,
        });
        self.face_sources.push(fe.index);
        Ok(Some(fid))
    }

    /// LOOP [2] points into the fin cycle; fins link via their forward
    /// pointer and each carries the vertex the loop enters its edge at.
    fn lower_loop(&mut self, le: &RawEntity) -> Result<Option<Bound>, String> {
        let first = ptr(le, 2);
        if first == 0 {
            return Ok(None);
        }

        // Collect the fin cycle. Pre-V20 fins lack the leading attribs
        // pointer, shifting every index down one; key off the field count.
        let mut cycle: Vec<&RawEntity> = Vec::new();
        let mut fin = first;
        let mut seen = rustc_hash::FxHashSet::default();
        while fin != 0 && seen.insert(fin) {
            let Some(fe) = self.index.get(&fin).filter(|e| e.type_id == xt::FIN) else {
                break;
            };
            cycle.push(fe);
            let a = usize::from(fe.fields.len() < 10);
            fin = ptr(fe, 2 - a);
        }
        if cycle.is_empty() {
            return Ok(None);
        }

        // The vertex each fin starts at, in cycle order.
        let starts: Vec<usize> = cycle
            .iter()
            .map(|fe| {
                let a = usize::from(fe.fields.len() < 10);
                self.vertex_position_handle(ptr(fe, 4 - a))
            })
            .collect();

        let mut halves = Vec::with_capacity(cycle.len());
        for (i, fe) in cycle.iter().enumerate() {
            let a = usize::from(fe.fields.len() < 10);
            let edge_ptr = ptr(fe, 6 - a);
            let sense = chr(fe, 9 - a);
            let pcurve_ptr = ptr(fe, 7 - a);
            if edge_ptr == 0 {
                continue;
            }
            // Measured, not assumed: a fin's vertex is the one its edge
            // *arrives* at. On edge 466273 of the Solid Edge file the line
            // runs (0,0,1), its two true ends differ only in z, and pairing
            // each fin with the NEXT fin's vertex produced "ends" 0.2 mm apart
            // perpendicular to the line — one edge ahead of the truth.
            let start = starts[(i + starts.len() - 1) % starts.len()];
            let end = starts[i];
            let forward = sense != '-';
            match self.intern_edge(edge_ptr, start, end, forward, pcurve_ptr) {
                Ok((edge, built_reversed)) => {
                    let pcurve = self
                        .index
                        .get(&pcurve_ptr)
                        .and_then(|pe| geom::pcurve_of(pe, self.index));
                    halves.push(HalfEdge {
                        edge,
                        forward: forward ^ built_reversed,
                        pcurve,
                    });
                }
                Err(reason) => self.skip(edge_ptr, reason),
            }
        }
        if halves.is_empty() {
            return Ok(None);
        }
        Ok(Some(Bound {
            outer: false,
            halves,
            vertex: None,
        }))
    }

    /// The vertex handle's POINT position handle, or 0 when the fin has none.
    fn vertex_position_handle(&self, vertex: usize) -> usize {
        if vertex == 0 {
            return 0;
        }
        self.index
            .get(&vertex)
            .filter(|e| e.type_id == xt::VERTEX)
            .map(|_| vertex)
            .unwrap_or(0)
    }

    fn vertex_point(&self, vertex: usize) -> Option<Vec3> {
        let ve = self.index.get(&vertex)?;
        let pe = self.index.get(&ptr(ve, 5))?;
        let a = pe.fields.get(5).map(|f| f.as_vec3())?;
        Some(Vec3::new(a[0], a[1], a[2]))
    }

    fn intern_vertex(&mut self, handle: usize, position: Vec3) -> VertexId {
        if handle != 0
            && let Some(&v) = self.vertices.get(&handle)
        {
            return v;
        }
        let id = VertexId(self.solid.vertices.len() as u32);
        self.solid.vertices.push(position);
        if handle != 0 {
            self.vertices.insert(handle, id);
        }
        id
    }

    /// Build or reuse the [`Edge`] for an XT edge handle.
    ///
    /// `start`/`end` are the vertex handles as this fin walks the edge;
    /// `forward` says whether that walk agrees with the fin's `+` sense. The
    /// stored edge always runs in the `+` direction, so a reversed fin hands
    /// its vertices over swapped.
    fn intern_edge(
        &mut self,
        edge_ptr: usize,
        start: usize,
        end: usize,
        forward: bool,
        fin_pcurve: usize,
    ) -> Result<(EdgeId, bool), String> {
        if let Some(&e) = self.edges.get(&edge_ptr) {
            return Ok(e);
        }
        let Some(ee) = self.index.get(&edge_ptr).filter(|e| e.type_id == xt::EDGE) else {
            return Err(format!("fin's edge {edge_ptr} is not an EDGE"));
        };
        // EDGE: [2]=tolerance, [6]=curve. A tolerant edge has no 3D curve at
        // all — its geometry lives in each fin's SP_CURVE — so the fin's
        // parameter-space curve, sampled through its face's surface, stands in.
        let curve_ptr = ptr(ee, 6);
        let curve_id = if curve_ptr != 0 {
            self.intern_curve(curve_ptr)?
        } else if fin_pcurve != 0 {
            self.intern_tolerant_curve(edge_ptr, fin_pcurve)?
        } else {
            return Err(format!("edge {edge_ptr} has neither a curve nor a fin pcurve"));
        };

        let (fwd_start, fwd_end) = if forward { (start, end) } else { (end, start) };

        let curve = &self.solid.curves[curve_id.index()];
        let (p0, p1, closed_no_vertex) = match (
            if fwd_start != 0 { self.vertex_point(fwd_start) } else { None },
            if fwd_end != 0 { self.vertex_point(fwd_end) } else { None },
        ) {
            (Some(a), Some(b)) => (a, b, false),
            // No vertices at all: a closed edge (a full circle). Anchor the
            // synthetic vertex at the curve's own seam.
            _ => {
                let t0 = curve.natural_range().lo;
                let p = curve.point_at(t0);
                (p, p, true)
            }
        };

        let tol = f64_at(ee, 2);
        let tolerance = if tol.is_finite() && tol > 0.0 {
            tol
        } else {
            self.solid.tolerance
        };

        // A trimmed curve carries its interval; everything else is recovered
        // from the end points.
        let range = match curve {
            Curve::Trimmed { range, .. } => *range,
            _ if closed_no_vertex => curve.natural_range(),
            _ => recover_edge_range(curve, p0, p1, true, tolerance),
        };

        // Recovery can genuinely fail on an INTERSECTION edge: its chart is
        // the modeller's sparse evaluation of the curve and may not reach the
        // edge's ends at all. The fin's own parameter curve covers exactly
        // this edge, so sample it through the face's surface and use that —
        // range, geometry and end points all come from the samples.
        let (curve_id, range, p0, p1, rebuilt) =
            if range.span().is_finite() && range.span() > 0.0 {
                (curve_id, range, p0, p1, false)
            } else if fin_pcurve != 0 {
                let cid = self.intern_tolerant_curve(edge_ptr, fin_pcurve)?;
                let c = &self.solid.curves[cid.index()];
                let natural = c.natural_range();
                let (a, b) = (c.point_at(natural.lo), c.point_at(natural.hi));
                (cid, natural, a, b, true)
            } else {
                let kind = match curve {
                    Curve::Line { .. } => "line",
                    Curve::Circle { .. } => "circle",
                    Curve::Ellipse { .. } => "ellipse",
                    Curve::Polyline { .. } => "chart-polyline",
                    Curve::Nurbs(_) => "nurbs",
                    Curve::Trimmed { .. } => "trimmed",
                    _ => "other",
                };
                return Err(format!(
                    "edge {edge_ptr}: unrecoverable {kind} range (span {:.3e}, ends {:.4} apart)",
                    range.span(),
                    (p1 - p0).length()
                ));
            };

        // The rebuilt samples run in the creating fin's direction; when that
        // fin was reversed the stored curve runs against the edge's `+`
        // convention, and later fins must flip their traversal.
        let built_reversed = rebuilt && !forward;

        // A rebuilt edge owns synthetic end vertices — its ends are sample
        // points within tolerance of the model's vertices, not the vertices
        // themselves. A normal edge keeps the model's vertex identities.
        let (sv, ev) = if rebuilt {
            let sv = self.intern_vertex(0, p0);
            let ev = if (p1 - p0).length_squared() < 1e-24 {
                sv
            } else {
                self.intern_vertex(0, p1)
            };
            (sv, ev)
        } else {
            let sv = self.intern_vertex(fwd_start, p0);
            let ev = if closed_no_vertex || fwd_end == fwd_start {
                sv
            } else {
                self.intern_vertex(fwd_end, p1)
            };
            (sv, ev)
        };

        let id = EdgeId(self.solid.edges.len() as u32);
        self.solid.edges.push(Edge {
            start: sv,
            end: ev,
            curve: curve_id,
            same_sense: true,
            range,
            tolerance,
        });
        self.edges.insert(edge_ptr, (id, built_reversed));
        Ok((id, built_reversed))
    }

    /// The stand-in curve for a tolerant edge: its fin's SP_CURVE sampled to
    /// a 3D polyline. Cached under a key that cannot collide with real curve
    /// handles because an entity handle is never zero.
    fn intern_tolerant_curve(
        &mut self,
        edge_ptr: usize,
        fin_pcurve: usize,
    ) -> Result<CurveId, String> {
        let Some(pe) = self.index.get(&fin_pcurve) else {
            return Err(format!("edge {edge_ptr}: fin pcurve {fin_pcurve} does not exist"));
        };
        let curve = geom::sp_curve_polyline(pe, self.index)?;
        if std::env::var_os("XT_CURVE_TRACE").is_some()
            && let Curve::Polyline { points } = &curve
            && points.iter().any(|p| p.length() > 100.0)
        {
            eprintln!(
                "[far-tolerant] edge {edge_ptr} pcurve {fin_pcurve} type {} first={:?}",
                pe.type_id,
                points.first()
            );
        }
        let id = CurveId(self.solid.curves.len() as u32);
        self.solid.curves.push(curve);
        Ok(id)
    }

    fn intern_surface(&mut self, handle: usize) -> Result<SurfaceId, String> {
        if let Some(&s) = self.surfaces.get(&handle) {
            return Ok(s);
        }
        let Some(se) = self.index.get(&handle) else {
            return Err(format!("surface {handle} does not exist"));
        };
        let surface = geom::surface(se, self.index)?;
        // An SP_CURVE inside this surface references it by XT handle; the
        // lowering rewrites those after interning, so nothing to fix here for
        // surfaces themselves.
        if let cad_ir::brep::Surface::Nurbs(n) = &surface
            && (n.control_points.is_empty() || n.u_knots.len() < 2)
        {
            return Err("spline surface has no geometry".into());
        }
        let id = SurfaceId(self.solid.surfaces.len() as u32);
        self.solid.surfaces.push(surface);
        self.surfaces.insert(handle, id);
        Ok(id)
    }

    fn intern_curve(&mut self, handle: usize) -> Result<CurveId, String> {
        if let Some(&c) = self.curves.get(&handle) {
            return Ok(c);
        }
        let Some(ce) = self.index.get(&handle) else {
            return Err(format!("curve {handle} does not exist"));
        };
        let mut curve = geom::curve(ce, self.index)?;
        if std::env::var_os("XT_CURVE_TRACE").is_some()
            && let Curve::Polyline { points } = &curve
            && points.iter().any(|p| p.length() > 100.0)
        {
            eprintln!(
                "[far-curve] handle {handle} type {} first={:?}",
                ce.type_id,
                points.first()
            );
        }
        // An SP_CURVE references its surface by XT handle; map it to the
        // interned SurfaceId so the tessellator can evaluate through it.
        if let Curve::OnSurface { surface, .. } = &mut curve {
            let xt_handle = surface.0 as usize;
            let interned = self.intern_surface(xt_handle)?;
            *surface = interned;
        }
        let id = CurveId(self.solid.curves.len() as u32);
        self.solid.curves.push(curve);
        self.curves.insert(handle, id);
        Ok(id)
    }
}

/// The default modelling tolerance when a body does not state one.
pub const DEFAULT_TOLERANCE: f64 = 1e-5;

/// A rough interval sanity bound, re-exported for tests.
pub fn finite(i: Interval) -> bool {
    i.span().is_finite() && i.span() > 0.0
}
