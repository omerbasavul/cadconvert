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
use xt_parser::entity::{Entities, RawEntity};
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

fn ptr(entities: &Entities, e: &RawEntity, i: usize) -> usize {
    entities.fields(e).get(i).map(|f| f.as_ptr()).unwrap_or(0)
}

fn chr(entities: &Entities, e: &RawEntity, i: usize) -> char {
    entities.fields(e).get(i).map(|f| f.as_char()).unwrap_or('+')
}

fn f64_at(entities: &Entities, e: &RawEntity, i: usize) -> f64 {
    entities.fields(e).get(i).map(|f| f.as_f64()).unwrap_or(f64::NAN)
}

/// Lower every BODY in the entity list.
pub fn lower_bodies(entities: &Entities, tolerance: f64) -> Vec<LoweredBody> {
    let index: Index = entities.iter().map(|e| (e.index, e)).collect();
    entities
        .iter()
        .filter(|e| e.type_id == xt::BODY)
        .map(|body| lower_body(entities, body, &index, tolerance))
        .collect()
}

fn lower_body(entities: &Entities, body: &RawEntity, index: &Index, default_tol: f64) -> LoweredBody {
    BODY_NO.with(|b| b.set(b.get() + 1));
    if std::env::var_os("XT_EDGE_TRACE").is_some() || std::env::var_os("XT_WALK_TRACE").is_some() {
        eprintln!("[body] #{} handle={}", BODY_NO.with(|b| b.get()), body.index);
    }
    let mut b = Lowering {
        entities,
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
        bridges: FxHashMap::default(),
        blend_surfaces: std::cell::RefCell::new(FxHashMap::default()),
        skipped: Vec::new(),
        rolled_faces: Vec::new(),
        blends_that_fell_back: rustc_hash::FxHashSet::default(),
    };

    // The body's shell and region pointers, found by probing: any pointer
    // field whose target is a SHELL / REGION. Probing instead of fixed indices
    // is what makes the 23-, 27-, 34- and 36-field BODY layouts all work.
    let mut shells: Vec<usize> = Vec::new();
    let mut body_type_candidates: Vec<u8> = Vec::new();
    for f in entities.fields(body) {
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
                            let s = ptr(entities, re, 5);
                            if s != 0 && index.get(&s).is_some_and(|e| e.type_id == xt::SHELL) {
                                shells.push(s);
                            }
                            r = ptr(entities, re, 3);
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

    // Body type: the byte field holding a known code — 1 solid, 2 wire,
    // 3 sheet, 6 general. This one was right about 3 by observation before the
    // numbers were read off the format, which is why a sheet body's boundary
    // has never been mistaken for a hole here even while the parser beside it
    // called the same body Unknown(3).
    b.solid.body_type = body_type_candidates
        .iter()
        .find_map(|&v| match v {
            1 => Some(BodyType::Solid),
            2 => Some(BodyType::Wire),
            3 => Some(BodyType::Sheet),
            _ => None,
        })
        .unwrap_or(BodyType::Solid);

    for shell in shells {
        b.lower_shell(shell);
    }
    let demoted = b.make_each_blend_of_one_mind();
    if std::env::var_os("XT_BLEND_PROBE").is_some() {
        eprintln!("[demote] {demoted} faces put back on the interpolation their siblings use");
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
    /// Where the fields are. An entity holds a range into this rather than a
    /// vector of its own; see [`xt_parser::entity::RawEntity::fields`].
    entities: &'a Entities,
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
    /// Straight edges made to close a loop the file left open, keyed by the
    /// pair of vertices they join so both faces are handed the same one.
    bridges: FxHashMap<(usize, usize), (EdgeId, bool)>,
    /// Blend surfaces already built, by entity. Building one is a marching
    /// solve, and the same blend bounds many edges — without this the walk
    /// pays for it once per edge instead of once per blend.
    blend_surfaces: std::cell::RefCell<FxHashMap<usize, Option<cad_ir::brep::Surface>>>,
    skipped: Vec<Skip>,
    /// Faces built from the ball's own geometry, each with the interpolation
    /// that would have been used instead — kept so a face can be put back on
    /// it if a sibling of the same blend could not be rolled.
    rolled_faces: Vec<(usize, SurfaceId, Vec<Vec<Vec3>>)>,
    /// Blends where at least one face fell back to the interpolation.
    blends_that_fell_back: rustc_hash::FxHashSet<usize>,
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
        let mut face = ptr(self.entities, se, 4);
        // A shell whose face pointer collides with a non-FACE entity: scan for
        // a face whose shell back-pointer ([6]) is this shell.
        if !self.index.get(&face).is_some_and(|e| e.type_id == xt::FACE) {
            face = self
                .index
                .values()
                .find(|e| e.type_id == xt::FACE && ptr(self.entities, e, 6) == shell)
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
            face = ptr(self.entities, fe, 3);
        }

        self.solid.shells.push(Shell {
            faces,
            closed: self.solid.body_type == BodyType::Solid,
            is_void: false,
        });
    }

    fn lower_face(&mut self, fe: &RawEntity) -> Result<Option<FaceId>, String> {
        // FACE: [2]=tolerance, [5]=loop, [7]=surface, [8]=sense.
        let surface_ptr = ptr(self.entities, fe, 7);
        if surface_ptr == 0 {
            return Err("face has no surface".into());
        }
        let surface = match self.intern_surface(surface_ptr) {
            Ok(s) => s,
            // Blend-family surfaces have no closed-form lowering; a face on
            // one is rebuilt as a Coons patch from its own boundary instead.
            Err(reason) => {
                if std::env::var_os("XT_NO_COONS").is_some() {
                    return Err(reason);
                }
                return self.lower_face_as_coons(fe, reason);
            }
        };

        // The face sense char composes with the surface's own sense char.
        let face_reversed = matches!(chr(self.entities, fe, 8), 'R' | '-');
        let geom_reversed = self
            .index
            .get(&surface_ptr)
            .map(|se| geom::geom_sense(self.entities, se) == '-')
            .unwrap_or(false);
        let same_sense = !(face_reversed ^ geom_reversed);

        let mut bounds = Vec::new();
        let mut lp = ptr(self.entities, fe, 5);
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
            lp = ptr(self.entities, le, 4);
        }
        if bounds.is_empty() {
            // A face that trims nothing is a real thing on a surface closed in
            // both directions: an O-ring is one toroidal face whose only bound
            // is a vertex, and the tessellator meshes it from the whole
            // parameter rectangle. Refusing it here cost two whole bodies of
            // the pilot assembly — every face they had was this — and they
            // left the scene without a word.
            let closed_both_ways = {
                let d = self.solid.surface(surface).domain();
                d.u_period.is_some() && d.v_period.is_some()
            };
            if !closed_both_ways {
                return Err("face has no usable loops".into());
            }
        }
        if let Some(first) = bounds.first_mut() {
            first.outer = true;
        }

        if std::env::var_os("XT_ON_SURFACE").is_some() {
            // Every edge of a face lies on that face's surface. Where it does
            // not, the file was read wrongly — the surface, the curve, or the
            // pairing between them — and no amount of care downstream recovers
            // it, so the check belongs here where the entity numbers are still
            // in hand.
            let surf = &self.solid.surfaces[surface.index()];
            let mut worst = 0.0f64;
            let mut culprit = 0u64;
            for h in bounds.iter().flat_map(|b| b.halves.iter()) {
                let e = &self.solid.edges[h.edge.index()];
                let c = &self.solid.curves[e.curve.index()];
                for k in 0..=8 {
                    let q = c.point_at(e.range.at(k as f64 / 8.0));
                    let d = surf
                        .invert(q, None)
                        .map(|uv| (surf.point_at(uv) - q).length())
                        .unwrap_or(f64::INFINITY);
                    if d > worst {
                        worst = d;
                        culprit = e.curve.0 as u64;
                    }
                }
            }
            if worst > 0.01 {
                eprintln!(
                    "[onsurf] {worst:.4} face=#{} surface=#{surface_ptr} curve_id={culprit} surf_kind={:?}",
                    fe.index,
                    std::mem::discriminant(surf)
                );
            }
        }

        let fid = FaceId(self.solid.faces.len() as u32);
        self.solid.faces.push(Face {
            surface,
            same_sense,
            bounds,
        });
        self.face_sources.push(fe.index);
        Ok(Some(fid))
    }

    /// Rebuild a face whose surface cannot be lowered, from its own boundary.
    ///
    /// The blend family — rolling-ball fillets — is the case: evaluating one
    /// exactly means contact-solving against both mating surfaces, but every
    /// edge bounding it already lies on geometry this crate lowers. A
    /// bilinearly blended Coons patch over the boundary reproduces those edges
    /// exactly and carries their profile across the interior, which is the
    /// blend's shape to within the variation between its end profiles.
    ///
    /// A blend band is topologically a quadrilateral, but the file rarely
    /// writes it with four edges: a rail broken at a tangent discontinuity, or
    /// a cap split by a neighbouring feature, gives five, eight, twenty. So
    /// the boundary is treated as one closed curve and cut at its four
    /// sharpest corners — which is where a quad's corners are — rather than at
    /// its edge junctions. Any side count from three upward works; three uses
    /// a degenerate fourth side, exactly as a triangular patch should.
    ///
    /// The result is stored as a degree-1 NURBS through the sampled grid, so
    /// downstream it is an ordinary surface: invertible, tessellatable, with
    /// no special case anywhere else in the pipeline.
    fn lower_face_as_coons(
        &mut self,
        fe: &RawEntity,
        original: String,
    ) -> Result<Option<FaceId>, String> {
        // The loops lower normally — their edges lie on neighbouring surfaces.
        let mut bounds = Vec::new();
        let mut lp = ptr(self.entities, fe, 5);
        let mut seen = rustc_hash::FxHashSet::default();
        while lp != 0 && seen.insert(lp) {
            let Some(le) = self.index.get(&lp).filter(|e| e.type_id == xt::LOOP) else {
                break;
            };
            if let Ok(Some(bound)) = self.lower_loop(le) {
                bounds.push(bound);
            }
            lp = ptr(self.entities, le, 4);
        }
        if bounds.is_empty() {
            return Err(format!("{original}; boundary rebuild found no loops"));
        }

        // The patch spans the largest loop; the rest stay as holes in it.
        let outer_index = bounds
            .iter()
            .enumerate()
            .max_by(|a, b| {
                self.loop_extent(a.1)
                    .partial_cmp(&self.loop_extent(b.1))
                    .unwrap_or(std::cmp::Ordering::Equal)
            })
            .map(|(i, _)| i)
            .unwrap_or(0);
        let outer = bounds.remove(outer_index);

        let ring = self.boundary_polyline(&outer);
        if ring.len() < 8 {
            return Err(format!(
                "{original}; boundary rebuild needs at least eight points, found {}",
                ring.len()
            ));
        }
        // Two loops on a blend may be the band's two rails rather than a
        // boundary with a hole in it. That is asked first, because when it is
        // true the Coons reading is not a rougher answer but a different
        // shape, and the boundary points then have nowhere sensible to land.
        if bounds.len() == 1 {
            // Both rails sampled finely and to the same standard, so the test
            // below measures the two curves and not the two discretisations.
            const BAND_SAMPLES: usize = 128;
            let fine_a = self.boundary_polyline_at(&outer, BAND_SAMPLES);
            let other = self.boundary_polyline_at(&bounds[0], BAND_SAMPLES);
            let n = fine_a.len().max(other.len()).clamp(16, 256);
            let ring = &fine_a;
            if let Some(grid) = self
                .rolling_ball_band(fe, ring, &other, n)
                .or_else(|| self.rolling_ball_band(fe, &other, ring, n))
            {
                if std::env::var_os("XT_BLEND_PROBE").is_some() {
                    eprintln!("[blend] band type=56 stations={n}");
                }
                return self.finish_rebuilt_face(fe, grid, outer, bounds, false);
            }
        }

        let corners = quad_corners(&ring);
        let sides = split_at_corners(&ring, corners);

        // Resample every side to the same count so the grid is rectangular.
        // The grid has to be at least as fine as the boundary it is rebuilt
        // from. A patch coarser than its own boundary puts many boundary
        // points inside one cell, and a cell of a degree-1 patch is a flat
        // quad: the points then invert onto nearly the same parameter and the
        // triangulation reads several of them as one, which slits the face
        // against every neighbour. Taking the count from the longest side
        // makes the densest side's points land on grid lines exactly.
        const N_MAX: usize = 96;
        let n = sides
            .iter()
            .map(|s| s.len())
            .max()
            .unwrap_or(16)
            .clamp(16, N_MAX);
        let (c0, c1, c2r, c3r) = (
            resample(&sides[0], n),
            resample(&sides[1], n),
            resample(&sides[2], n),
            resample(&sides[3], n),
        );
        let n_ref = n;
        // Sides 2 and 3 run backwards around the loop; flip them so all four
        // are indexed from the patch's own origin.
        let c2: Vec<Vec3> = c2r.iter().rev().copied().collect();
        let c3: Vec<Vec3> = c3r.iter().rev().copied().collect();

        let p00 = c0[0];
        let p10 = c0[n_ref];
        let p11 = c2[n_ref];
        let p01 = c2[0];

        // A rolling-ball blend's cross-section is a circular arc of the
        // radius the file states, not the straight line a Coons patch rules
        // between its rails. On a 17 mm fillet those differ by 5 mm at the
        // middle — the chord cuts the corner off. When the surface says it is
        // a rolling ball and the arc actually closes on both rails, the arc
        // is used; otherwise the Coons grid stands, as it must for the
        // blend forms this does not cover.
        // Any of the four sides may be the rail the ball rolls along — which
        // one depends on where the corner search cut the ring — so each is
        // offered. Sides 1 and 3 run across the patch, so a grid built from
        // them is stored the other way round.
        BEST_MISS.with(|m| m.set(f64::INFINITY));
        // What the face needs is a surface its own boundary sits on, so the
        // boundary is part of the test from the start rather than a filter on
        // whatever the first rail happened to produce.
        let limit = self.solid.tolerance * 3.0;
        // Where the four corners of the patch fall is a judgement — the
        // sharpest turns of a boundary that curves smoothly all the way round
        // are only a guess at them — and the rails handed to the ball depend
        // on it entirely. Both readings are offered, exactly as the Coons
        // rebuild in `cad-tess` offers both, and the boundary check decides:
        // a rail cut in the wrong place sweeps a sheet the face's own ends are
        // not on, which is what the remaining refusals looked like — a quarter
        // of the ring adrift, one side of four.
        let rails_for = |corners: [usize; 4]| {
            let sides = split_at_corners(&ring, corners);
            let n = sides.iter().map(|s| s.len()).max().unwrap_or(16).clamp(16, N_MAX);
            let along = |i: usize| resample(&sides[i], n);
            let back = |i: usize| {
                resample(&sides[i], n).iter().rev().copied().collect::<Vec<Vec3>>()
            };
            (along(0), along(1), back(2), back(3), n)
        };
        let roll_with = |corners: [usize; 4]| {
            let (r0, r1, r2, r3, n) = rails_for(corners);
            self.rolling_ball_grid(fe, &r0, n, &ring, limit)
                .or_else(|| self.rolling_ball_grid(fe, &r2, n, &ring, limit))
                .or_else(|| self.rolling_ball_grid(fe, &r1, n, &ring, limit).map(transpose))
                .or_else(|| self.rolling_ball_grid(fe, &r3, n, &ring, limit).map(transpose))
                .filter(|grid| {
                    let held = grid_holds(grid, &ring, limit);
                    if !held {
                        REFUSED_HERE.with(|c| c.set(true));
                        if std::env::var_os("XT_BLEND_PROBE").is_some() {
                            report_refusal(grid, &ring, corners, limit);
                        }
                    }
                    held
                })
        };
        let evenly = {
            let m = ring.len();
            [0, m / 4, m / 2, (3 * m) / 4]
        };
        // The rails a ball rolls along are its two contact tracks, and the file
        // says where those are: they are the parts of this face's boundary
        // that lie on the two surfaces the blend names. Cutting the ring at
        // the sharpest turns, or at four even points, only guesses at them —
        // and one refused face, dumped end to end, showed the guess splitting
        // a single rail in two: sides 0 and 1 both lay on the cone, 67 and 43
        // µm off it, while side 2 lay on the cylinder and side 3 on neither.
        // Rolling along half a rail sweeps a sheet covering half the face.
        //
        // So the tracks are read off the boundary instead, and offered first.
        // Asked last, not first: reading the tracks costs an inversion of
        // every boundary point onto both surfaces, and where a corner reading
        // already works that is paid for nothing. Measured, asking first:
        // 25.1 seconds against 15.8 for the same 568 faces.
        // What the track reading finds, measured: of 1,444 faces it names both
        // tracks for 856, one for 450 and none for 138, and the median track
        // is 45% of the ring — which is what a rail of a four-sided patch
        // should be. So identifying the rails is *not* what is wrong with it;
        // rolling along one is. That is where to look next.
        if std::env::var_os("XT_TRACK_PROBE").is_some() {
            let t = self.contact_tracks(fe, &ring);
            // And what happens when the ball is rolled along one: a track that
            // is found and still will not roll says the fault is in the roll,
            // not in the reading.
            if let Some(be) = self.index.get(&ptr(self.entities, fe, 7)) {
                let radius = f64_at(self.entities, be, 11).abs();
                let tol = (radius * 0.01).max(self.solid.tolerance * 20.0);
                let mates: Vec<cad_ir::brep::Surface> = [ptr(self.entities, be, 8), ptr(self.entities, be, 9)]
                    .iter()
                    .filter_map(|q| self.index.get(q))
                    .filter_map(|e| geom::surface(self.entities, e, self.index).ok())
                    .collect();
                for (which, rail) in &t {
                    if mates.len() < 2 {
                        continue;
                    }
                    let (near, far) = (&mates[*which], &mates[1 - *which]);
                    // The track was chosen by lying on `near`, so that half of
                    // the test must pass. Does the ball reach `far`?
                    for sign in [1.0f64, -1.0] {
                        let mut reached = 0usize;
                        let mut worst = 0.0f64;
                        let mut hits: Vec<bool> = Vec::with_capacity(rail.len());
                        for p in rail.iter() {
                            let Some(uv) = near.invert(*p, None) else {
                                hits.push(false);
                                continue;
                            };
                            let centre = *p + near.normal_at(uv) * (radius * sign);
                            let Some(fuv) = far.invert(centre, None) else {
                                hits.push(false);
                                continue;
                            };
                            let out = ((far.point_at(fuv) - centre).length() - radius).abs();
                            worst = worst.max(out / tol);
                            hits.push(out <= tol);
                            if out <= tol {
                                reached += 1;
                            }
                        }
                        // Where the named mate lets go, does any surface of
                        // the body take over? If one does, splitting the face
                        // there would work; if none does, the ball's centre
                        // line itself is wrong past that point.
                        if std::env::var_os("XT_HANDOVER_PROBE").is_some() {
                            let mut taken = 0usize;
                            let mut dropped = 0usize;
                            for (p, hit) in rail.iter().zip(&hits) {
                                if *hit {
                                    continue;
                                }
                                let Some(uv) = near.invert(*p, None) else { continue };
                                let centre = *p + near.normal_at(uv) * (radius * sign);
                                let any = self.solid.surfaces.iter().any(|s| {
                                    s.invert(centre, None).is_some_and(|q| {
                                        ((s.point_at(q) - centre).length() - radius).abs() <= tol
                                    })
                                });
                                if any { taken += 1 } else { dropped += 1 }
                            }
                            if taken + dropped > 0 {
                                eprintln!("[handover] {taken} of {} missed points are held by some other surface of the body", taken + dropped);
                            }
                        }
                        // Contiguous or scattered: a run says the named pair
                        // stops applying partway along, a scatter says the
                        // pair is simply not this face's.
                        let mut run = 0usize;
                        let mut best_run = 0usize;
                        for h in &hits {
                            run = if *h { run + 1 } else { 0 };
                            best_run = best_run.max(run);
                        }
                        eprintln!(
                            "[reach] sign {sign:+.0}: the ball reaches the far surface at {reached} of {} track points, longest run {best_run}, worst miss {worst:.1}x the tolerance",
                            rail.len()
                        );
                    }
                }
            }
        }
        let rolled = roll_with(corners)
            .or_else(|| (evenly != corners).then(|| roll_with(evenly)).flatten())
            ;
        // One refused face, end to end. Which of the patch's four sides lies
        // on which of the blend's two mating surfaces is the whole question:
        // a rail is a contact track, and a side that lies on neither is a
        // cross-section. If no side lies on either, the surfaces named by the
        // blend are not the ones this face is between.
        if rolled.is_none()
            && std::env::var_os("XT_BLEND_DUMP").is_some()
            && DUMPED_ONE.with(|d| !d.get())
            && let Some(be) = self.index.get(&ptr(self.entities, fe, 7))
        {
            let mates = [ptr(self.entities, be, 8), ptr(self.entities, be, 9)].map(|q| {
                self.index.get(&q).and_then(|e| geom::surface(self.entities, e, self.index).ok())
            });
            let sides = split_at_corners(&ring, corners);
            let lies_on = |side: &[Vec3], m: &Option<cad_ir::brep::Surface>| match m {
                None => "not lowered".to_string(),
                Some(surf) => {
                    let worst = side
                        .iter()
                        .map(|q| {
                            surf.invert(*q, None)
                                .map(|uv| (surf.point_at(uv) - *q).length())
                                .unwrap_or(f64::INFINITY)
                        })
                        .fold(0.0f64, f64::max);
                    format!("{worst:.6}")
                }
            };
            eprintln!(
                "[dump] a refused face: radius {:.5}, ring {} points, mates {} and {}",
                f64_at(self.entities, be, 11).abs(),
                ring.len(),
                mates[0].as_ref().map(surface_name).unwrap_or("none"),
                mates[1].as_ref().map(surface_name).unwrap_or("none"),
            );
            for (i, side) in sides.iter().enumerate() {
                eprintln!(
                    "[dump]   side {i}: {} points, {:.4} long, off the first mate by {}, off the second by {}",
                    side.len(),
                    polyline_extent(side),
                    lies_on(side, &mates[0]),
                    lies_on(side, &mates[1]),
                );
            }
            // The decisive number for a rolling-ball face: both contact
            // tracks stand off the two mates' intersection by the same
            // distance, r·tan(θ/2) for dihedral θ. Measure each boundary
            // point's distance to the *other* mate — on a true fillet the
            // points on mate A all sit one fixed distance from mate B and
            // vice versa; a spread says the face is not a fixed-radius roll.
            if let (Some(a), Some(b)) = (&mates[0], &mates[1]) {
                let dist = |surf: &cad_ir::brep::Surface, q: Vec3| {
                    surf.invert(q, None).map(|uv| (surf.point_at(uv) - q).length()).unwrap_or(f64::NAN)
                };
                let mut on_a: Vec<f64> = Vec::new();
                let mut on_b: Vec<f64> = Vec::new();
                for q in &ring {
                    let (da, db) = (dist(a, *q), dist(b, *q));
                    if da < 1e-4 { on_a.push(db); }
                    if db < 1e-4 { on_b.push(da); }
                }
                let stats = |v: &mut Vec<f64>| {
                    v.sort_by(f64::total_cmp);
                    if v.is_empty() { "none".to_string() } else {
                        format!("{} pts, standoff min {:.5} median {:.5} max {:.5}", v.len(), v[0], v[v.len()/2], v[v.len()-1])
                    }
                };
                eprintln!("[standoff] points on mate A, distance to mate B: {}", stats(&mut on_a));
                eprintln!("[standoff] points on mate B, distance to mate A: {}", stats(&mut on_b));
                let r = f64_at(self.entities, be, 11).abs();
                eprintln!("[standoff] record radius {r:.5}; a fixed-radius roll would give one standoff for all of each");
            }
            // And as geometry, to be looked at: the ring, a sample of each
            // mate, and the grid the first reading produced, in one OBJ.
            if let Ok(path) = std::env::var("XT_BLEND_DUMP_OBJ") {
                use std::fmt::Write as _;
                let mut obj = String::new();
                let mut base = 1usize;
                let _ = writeln!(obj, "o ring");
                for q in &ring {
                    let _ = writeln!(obj, "v {} {} {}", q.x, q.y, q.z);
                }
                let _ = writeln!(obj, "l {}", (base..base + ring.len()).map(|i| i.to_string()).collect::<Vec<_>>().join(" "));
                base += ring.len();
                for (k, m) in mates.iter().enumerate() {
                    let Some(surf) = m else { continue };
                    let d = surf.domain();
                    let (ulo, uhi) = (d.u.lo.max(-0.05), d.u.hi.min(0.05));
                    let (vlo, vhi) = (d.v.lo.max(-0.05), d.v.hi.min(0.05));
                    let _ = writeln!(obj, "o mate{k}");
                    let n = 24;
                    for i in 0..=n {
                        for j in 0..=n {
                            let q = surf.point_at(cad_ir::Vec2::new(
                                ulo + (uhi - ulo) * i as f64 / n as f64,
                                vlo + (vhi - vlo) * j as f64 / n as f64,
                            ));
                            let _ = writeln!(obj, "v {} {} {}", q.x, q.y, q.z);
                        }
                    }
                    for i in 0..n {
                        for j in 0..n {
                            let a = base + i * (n + 1) + j;
                            let _ = writeln!(obj, "f {} {} {} {}", a, a + 1, a + n + 2, a + n + 1);
                        }
                    }
                    base += (n + 1) * (n + 1);
                }
                let _ = std::fs::write(&path, obj);
                eprintln!("[dump] wrote {path}");
            }
            DUMPED_ONE.with(|d| d.set(true));
        }

        // How near the best of the sixteen attempts came, in multiples of the
        // tolerance it had to meet. A face whose best attempt misses by a few
        // is a tolerance away from the ball's own geometry; one that misses by
        // a hundred is not this blend at all.
        if rolled.is_none() && std::env::var_os("XT_ROLL_BEST").is_some() {
            eprintln!("[rollbest] {:.3}", BEST_MISS.with(|m| m.get()));
        }
        BEST_MISS.with(|m| m.set(f64::INFINITY));
        let coons = || {
            let mut grid = vec![vec![Vec3::ZERO; n_ref + 1]; n_ref + 1];
            for (i, row) in grid.iter_mut().enumerate() {
                let u = i as f64 / n_ref as f64;
                for (j, slot) in row.iter_mut().enumerate() {
                    let v = j as f64 / n_ref as f64;
                    let ruled_v = c0[i] * (1.0 - v) + c2[i] * v;
                    let ruled_u = c3[j] * (1.0 - u) + c1[j] * u;
                    let bilinear = p00 * ((1.0 - u) * (1.0 - v))
                        + p10 * (u * (1.0 - v))
                        + p01 * ((1.0 - u) * v)
                        + p11 * (u * v);
                    *slot = ruled_v + ruled_u - bilinear;
                }
            }
            grid
        };

        // The ball's own two contacts are not the whole test. What the face
        // needs is a surface its *own boundary* sits on: the rails are the
        // file's edges, shared with the neighbours, and a grid that does not
        // carry them leaves the face trimmed against something it is not on.
        // Checking the ball alone is what let a looser acceptance tear the
        // mesh — the contacts were satisfied and the grid still was not the
        // face's.


        // One blend's faces are read one way or not at all. A face rolled from
        // the ball's own geometry beside a sibling interpolated from its
        // boundary leaves their shared edge in two places and the mesh parts
        // along it — measured at an intermediate acceptance: 164 open
        // half-edges. So the interpolation is built alongside every rolled
        // face, and a blend that could not roll all of its faces has the
        // rolled ones put back on it once the body is done.
        // Where no ball rolls, the section is still an arc. A Coons patch
        // rules a straight line between the two rails, which on a blend is
        // the chord of its arc: on the 1.0–1.5 mm fillets of the pilot the
        // chord sits 0.7–1.3 mm inside the surface at mid-section — the ten
        // worst-faceted faces of `204 201 013-51` are all such patches, and
        // OpenCASCADE's mesh finds a 1.39 mm hole at one of them. So before
        // falling back to the chord, each section is drawn as the arc of the
        // record's radius through its two rail points, bulging the way the
        // mates' normals say a ball of that radius would sit, and the
        // boundary gate judges it exactly as it judges a roll.
        // The gate the arc has to pass is the one the alternative would pass.
        // A Coons patch is taken on trust because it is built from the
        // boundary, yet measured on the faces where the arc just failed the
        // 30 µm gate, the Coons patch missed the same boundary points by the
        // same amount and would have failed it on 45 of 61. So the arc is
        // asked to be no worse than the Coons patch on this face — within the
        // gate, or within whatever the Coons patch itself manages.
        let fair_limit = {
            let g = coons();
            let coons_off = ring.iter().map(|q| point_to_grid(&g, *q)).fold(0.0f64, f64::max);
            // Five per cent over: "no worse than Coons" compared to the last
            // bit is an equality test, and face 946 of `204 201 013-51` failed
            // it at 30.01 µm against a Coons 29.99 µm — both printed 0.00003.
            limit.max(coons_off) * 1.05
        };
        // Which opposite pair of sides is the rail pair is `quad_corners`'
        // judgement, and on 340 faces of the pilot it takes the cross-sections
        // for rails: their half-chords come out 2.2–14.8 times the ball's
        // radius, which no rolling ball can produce — it touches both rails,
        // so their chord is 2r·cos(θ/2) and never wider than the ball. The
        // giveaway is the side lengths: the short pair measures 1.50 r where a
        // quarter-circle section of radius r measures 1.571, and the long pair
        // runs 10–31 r down the edge. So when the first pairing's sections do
        // not fit the ball, the other pairing is asked the same question, and
        // the boundary gate judges the answer exactly as before.
        let arced = if rolled.is_none() {
            let first = self
                .arc_sections(fe, &c0, &c2, &c1, &c3, n_ref, false)
                .or_else(|| self.arc_sections(fe, &c3, &c1, &c0, &c2, n_ref, false));
            match first {
                Some(g) if grid_holds(&g, &ring, fair_limit) => {
                    if std::env::var_os("XT_BLEND_PROBE").is_some() { eprintln!("[arc] carried, bulge as chosen"); }
                    Some(g)
                }
                Some(g) => {
                    // The side the arc bulges to is a judgement; try the other.
                    let flipped = self
                        .arc_sections(fe, &c0, &c2, &c1, &c3, n_ref, true)
                        .or_else(|| self.arc_sections(fe, &c3, &c1, &c0, &c2, n_ref, true));
                    match flipped {
                        Some(f) if grid_holds(&f, &ring, fair_limit) => {
                            if std::env::var_os("XT_BLEND_PROBE").is_some() { eprintln!("[arc] carried, bulge FLIPPED"); }
                            Some(f)
                        }
                        _ => {
                            if std::env::var_os("XT_BLEND_PROBE").is_some() {
                                let off = ring.iter().map(|q| point_to_grid(&g, *q)).fold(0.0f64, f64::max);
                                let off2 = flipped.as_ref().map(|f| ring.iter().map(|q| point_to_grid(f, *q)).fold(0.0f64, f64::max)).unwrap_or(f64::NAN);
                                // Split the miss: points that are on either rail
                                // (carried to the bit by construction) against the
                                // rest of the ring (the two cross-section ends).
                                let on_rail = |q: &Vec3| c0.iter().chain(c2.iter()).any(|r| (*r - *q).length() < 1e-7);
                                let rail_off = ring.iter().filter(|q| on_rail(q)).map(|q| point_to_grid(&g, *q)).fold(0.0f64, f64::max);
                                let end_off = ring.iter().filter(|q| !on_rail(q)).map(|q| point_to_grid(&g, *q)).fold(0.0f64, f64::max);
                                let n_rail = ring.iter().filter(|q| on_rail(q)).count();
                                let coons_grid = coons();
                                let coons_end = ring.iter().filter(|q| !on_rail(q)).map(|q| point_to_grid(&coons_grid, *q)).fold(0.0f64, f64::max);
                                // The arc's own sagitta at mid-patch, against the
                                // chord: what the ball's radius makes of this width.
                                let k = g.len() / 2;
                                let (a, b) = (g[k][0], g[k][g[k].len() - 1]);
                                let mid = g[k][g[k].len() / 2];
                                let chord_mid = (a + b) * 0.5;
                                let sag = (mid - chord_mid).length();
                                let chord = (b - a).length();
                                eprintln!("[arc] neither bulge carried: off by {off:.7} / {off2:.7} (fair gate {fair_limit:.7}, ratio {:.3}); on-rail pts {n_rail}/{} off {rail_off:.5}, end pts off {end_off:.5}; coons end pts off {coons_end:.5}; chord {chord:.5} sagitta {sag:.5}", off.min(off2) / fair_limit, ring.len());
                            }
                            None
                        }
                    }
                }
                None => None,
            }
        } else {
            None
        };
        let from_arc = rolled.is_none() && arced.is_some();
        ARCED_HERE.with(|c| c.set(from_arc));
        let rolled = rolled.or(arced);
        if std::env::var_os("XT_BLEND_PROBE").is_some() {
            let t = self.index.get(&ptr(self.entities, fe, 7)).map(|e| e.type_id).unwrap_or(0);
            let r = self.index.get(&ptr(self.entities, fe, 7)).map(|e| f64_at(self.entities, e, 11)).unwrap_or(0.0);
            // Two very different reasons to end up on the interpolation: a
            // grid was built and did not carry the face, or no ball ever
            // stayed on both surfaces long enough to build one.
            let why = if rolled.is_some() {
                if ARCED_HERE.with(|c| c.get()) { "arced" } else { "rolled" }
            } else if REFUSED_HERE.with(|c| c.get()) {
                "coons-after-refusal"
            } else {
                "coons-never-rolled"
            };
            eprintln!("[blend] {why} type={t} loops={} ring={} radius={r:.5} face={} entity={}", bounds.len() + 1, ring.len(), self.solid.faces.len(), fe.index);
            if std::env::var_os("XT_BLEND_FIELDS").is_some()
                && let Some(be) = self.index.get(&ptr(self.entities, fe, 7))
            {
                let fields: Vec<String> =
                    self.entities.fields(be).iter().map(|f| format!("{f:?}")).collect();
                eprintln!("[fields] {why} {}", fields.join(" "));
            }
        }
        if from_arc && std::env::var_os("XT_BLEND_PROBE").is_some() {
            eprintln!("[arc] kept as the face's surface");
        }

        let blend = ptr(self.entities, fe, 7);
        match rolled {
            Some(grid) => {
                let spare = coons();
                // `from_arc` is set when the arc construction stood in for a roll,
                // so a grid that is *not* from an arc here is one the ball rolled.
                let out = self.finish_rebuilt_face(fe, grid, outer, bounds, !from_arc);
                if let Ok(Some(fid)) = &out {
                    let sid = self.solid.faces[fid.0 as usize].surface;
                    self.rolled_faces.push((blend, sid, spare));
                }
                out
            }
            None => {
                if std::env::var_os("XT_STANDOFF_PROBE").is_some()
                    && let Some(be) = self.index.get(&ptr(self.entities, fe, 7))
                {
                    let r = f64_at(self.entities, be, 11).abs();
                    let mates: Vec<cad_ir::brep::Surface> = [ptr(self.entities, be, 8), ptr(self.entities, be, 9)]
                        .iter()
                        .filter_map(|q| self.index.get(q))
                        .filter_map(|e| geom::surface(self.entities, e, self.index).ok())
                        .collect();
                    if let [a, b] = mates.as_slice() {
                        let dist = |surf: &cad_ir::brep::Surface, q: Vec3| {
                            surf.invert(q, None).map(|uv| (surf.point_at(uv) - q).length()).unwrap_or(f64::NAN)
                        };
                        // The widest standoff either track reaches, against
                        // the radius: a real fillet's tracks stand a good
                        // fraction of r off the other mate; a seam-hugging
                        // sliver stays within a few tolerances of it.
                        // Not the widest point but the whole track: on a
                        // fixed-radius fillet every point of a track stands
                        // the same distance off the other mate. The spread of
                        // that distance along the track says whether this is
                        // such a fillet at all.
                        let mut a_off: Vec<f64> = Vec::new();
                        let mut b_off: Vec<f64> = Vec::new();
                        for q in &ring {
                            let (da, db) = (dist(a, *q), dist(b, *q));
                            if da < 1e-4 && db.is_finite() { a_off.push(db); }
                            if db < 1e-4 && da.is_finite() { b_off.push(da); }
                        }
                        let spread = |v: &mut Vec<f64>| -> Option<(f64, f64)> {
                            if v.len() < 3 { return None; }
                            v.sort_by(f64::total_cmp);
                            let med = v[v.len() / 2];
                            let iqr = v[v.len() * 3 / 4] - v[v.len() / 4];
                            Some((med / r.max(1e-12), iqr / med.max(1e-12)))
                        };
                        match (spread(&mut a_off), spread(&mut b_off)) {
                            (Some((ma, sa)), Some((mb, sb))) => {
                                eprintln!(
                                    "[standoff-class] track A median {ma:.2}r spread {sa:.2}  track B median {mb:.2}r spread {sb:.2}  pts {} {}",
                                    a_off.len(), b_off.len()
                                );
                                // A true fixed-radius fillet that still would
                                // not roll: take it apart once. Which pairing
                                // and sign reaches, and at how many of the
                                // track's points, with the best miss.
                                if sa < 0.25 && sb < 0.25 && DUMPED_TIGHT.with(|d| !d.get()) {
                                    DUMPED_TIGHT.with(|d| d.set(true));
                                    let tol = (r * 0.01).max(self.solid.tolerance * 20.0);
                                    for (label, near, far) in [("A→B", a, b), ("B→A", b, a)] {
                                        let track: Vec<Vec3> = ring
                                            .iter()
                                            .copied()
                                            .filter(|q| dist(near, *q) < 1e-4)
                                            .collect();
                                        for sign in [1.0f64, -1.0] {
                                            let mut ok = 0usize;
                                            let mut worst = 0.0f64;
                                            let mut missed_at: Vec<usize> = Vec::new();
                                            for (qi, q) in track.iter().enumerate() {
                                                let Some(uv) = near.invert(*q, None) else { continue };
                                                let c = *q + near.normal_at(uv) * (r * sign);
                                                let Some(fuv) = far.invert(c, None) else { worst = f64::INFINITY; continue };
                                                let out = ((far.point_at(fuv) - c).length() - r).abs();
                                                worst = worst.max(out / tol);
                                                if out <= tol { ok += 1; } else { missed_at.push(qi); }
                                            }
                                            eprintln!("[tight] {label} sign {sign:+.0}: {ok} of {} track points reach, worst miss {worst:.1}x tol, missed at {missed_at:?}", track.len());
                                            // The question that decides it: with the
                                            // tolerance this face needs, does the
                                            // grid carry the boundary?
                                            if ok * 2 > track.len() && worst.is_finite() {
                                                let needed = tol * (worst * 1.05);
                                                let n2 = track.len().clamp(16, 96);
                                                let verdict = match self.roll_with_tolerance(&track, near, far, r, sign, n2, needed) {
                                                    None => "still will not roll",
                                                    Some(g) if grid_holds(&g, &ring, self.solid.tolerance * 3.0) => "rolls and CARRIES the face",
                                                    Some(g) => {
                                                        let w = ring.iter().map(|q| point_to_grid(&g, *q)).fold(0.0f64, f64::max);
                                                        eprintln!("[tight]   boundary off the grid by up to {w:.6} (gate {:.6})", self.solid.tolerance * 3.0);
                                                        "rolls but does not carry the face"
                                                    }
                                                };
                                                eprintln!("[tight]   at {:.1}x tol: {verdict}", worst * 1.05);
                                            }
                                        }
                                    }
                                }
                            }
                            (Some((m, sp)), None) | (None, Some((m, sp))) => eprintln!(
                                "[standoff-class] one track only: median {m:.2}r spread {sp:.2}"
                            ),
                            (None, None) => eprintln!("[standoff-class] no point of the ring lies on either mate"),
                        }
                    }
                }
                self.blends_that_fell_back.insert(blend);
                self.finish_rebuilt_face(fe, coons(), outer, bounds, false)
            }
        }
    }

    /// The parts of a face's boundary that lie on the surfaces its blend names.
    ///
    /// Not used. It is kept because the reasoning behind it is sound and the
    /// measurement against it is the useful part: dumping one refused face end
    /// to end showed the corner search splitting a single rail in two — sides
    /// 0 and 1 both lay on the cone, 67 and 43 µm off it, while side 2 lay on
    /// the cylinder 39 µm off and side 3 on neither — so rolling along a
    /// "side" can mean rolling along half a rail. Reading the tracks off the
    /// boundary instead should fix that, and does not: on its own it rolls 76
    /// faces where the corner readings roll 518, and as a last resort after
    /// them it adds 50 for ten seconds, the worst rate of any change here.
    /// Something else is wrong with the runs it finds, and finding out what is
    /// the next thing to do.
    ///
    /// A rolling ball touches two surfaces, and the tracks where it touches
    /// them are edges of this very face. Each boundary point is asked which of
    /// the two it lies on — within the same tolerance the roll itself uses —
    /// and the longest unbroken run on each is that surface's track. A point
    /// on neither is a cross-section end, which is what separates the runs.
    #[allow(dead_code, reason = "kept with its measurement; see the note above")]
    fn contact_tracks(&self, fe: &RawEntity, ring: &[Vec3]) -> Vec<(usize, Vec<Vec3>)> {
        let Some(be) = self.index.get(&ptr(self.entities, fe, 7)) else { return Vec::new() };
        if be.type_id != xt::BLENDED_EDGE {
            return Vec::new();
        }
        let radius = f64_at(self.entities, be, 11).abs();
        if !(radius.is_finite() && radius > 0.0) {
            return Vec::new();
        }
        let tolerance = (radius * 0.01).max(self.solid.tolerance * 20.0);
        let mates: Vec<cad_ir::brep::Surface> = [ptr(self.entities, be, 8), ptr(self.entities, be, 9)]
            .iter()
            .filter_map(|q| self.index.get(q))
            .filter_map(|e| geom::surface(self.entities, e, self.index).ok())
            .collect();
        let mut out = Vec::new();
        for (which, surf) in mates.iter().enumerate() {
            let on: Vec<bool> = ring
                .iter()
                .map(|p| {
                    surf.invert(*p, None)
                        .is_some_and(|uv| (surf.point_at(uv) - *p).length() <= tolerance)
                })
                .collect();
            // The ring is closed, so a run may straddle its start.
            let n = on.len();
            let (mut best, mut best_len) = (0usize, 0usize);
            let mut i = 0;
            while i < n {
                if !on[i] {
                    i += 1;
                    continue;
                }
                let mut len = 0;
                while len < n && on[(i + len) % n] {
                    len += 1;
                }
                if len > best_len {
                    best_len = len;
                    best = i;
                }
                i += len.max(1);
            }
            if best_len >= 3 && best_len < n {
                out.push((which, (0..best_len).map(|k| ring[(best + k) % n]).collect()));
            }
        }
        out
    }

    /// Each section of the patch as a circular arc of the blend's radius
    /// through its two rail points, rather than the straight chord a Coons
    /// patch rules between them.
    ///
    /// The arc's centre is where a ball of radius `r` touching the near mate
    /// at `a` would sit: `a + n_a·r`, with `n_a` the mate's normal at `a`,
    /// signed so that the centre is also a radius from `b`. Both signs are
    /// tried and the one whose centre is nearer to `r` from `b` is taken; if
    /// neither is within a fifth of `r`, the section is not an arc of that
    /// radius and the patch is refused.
    fn arc_sections(
        &self,
        fe: &RawEntity,
        c0: &[Vec3],
        c2: &[Vec3],
        c1: &[Vec3],
        c3: &[Vec3],
        n: usize,
        flip: bool,
    ) -> Option<Vec<Vec<Vec3>>> {
        let be = self.index.get(&ptr(self.entities, fe, 7))?;
        if be.type_id != xt::BLENDED_EDGE || chr(self.entities, be, 7) != 'R' {
            return None;
        }
        let r = f64_at(self.entities, be, 11).abs();
        if !(r.is_finite() && r > 0.0) {
            return None;
        }
        let note = |why: &str| {
            if std::env::var_os("XT_ARC_PROBE").is_some() {
                eprintln!("[arc-why] face={} {why}", self.solid.faces.len());
            }
        };
        // The arc needs no mate at all. Its two ends are the two rail points
        // and its radius is the record's, which fixes the centre up to a
        // choice of side: the two centres at distance `r` from both ends lie
        // either way along the perpendicular bisector of the chord, in the
        // plane of the section. The section plane is spanned by the chord and
        // the rail's own direction there — a blend's sections are normal to
        // the rail it rolls along — and the side is the one bulging *away*
        // from the body, which the mates would say if they could be lowered
        // and the neighbouring rails say just as well: the centre that sits
        // on the same side of the chord as the face's interior does not.
        //
        // Asking the mates for it was the first version, and 423 of the 926
        // faces that reach here have a mate that is itself a blend and will
        // not lower, so they never got an arc at all.
        if c0.len() < 3 || c2.len() != c0.len() {
            note("rails too short or unequal");
            return None;
        }
        let mut grid = Vec::with_capacity(c0.len());
        let mut worst_fit = 0.0f64;
        let m = c0.len();
        for (i, (a, b)) in c0.iter().zip(c2).enumerate() {
            let chord = *b - *a;
            let half = chord.length() * 0.5;
            if half > r {
                let fit = (half - r) / r;
                worst_fit = worst_fit.max(fit);
                if fit > 0.2 {
                    // Which pair of sides is the rail pair is `quad_corners`'
                    // judgement, and a fillet's two rails can never be more
                    // than 2r apart: the ball touches both, so the chord is
                    // 2r·cos(θ/2). A chord wider than the ball is therefore
                    // not "the wrong radius" — it is the wrong pair. Report
                    // what the other pairing would have measured.
                    if std::env::var_os("XT_ARC_PROBE").is_some() {
                        let other = c1
                            .iter()
                            .zip(c3)
                            .map(|(a, b)| (*b - *a).length() * 0.5 / r)
                            .fold(0.0f64, f64::max);
                        let rails = c0.iter().zip(c2).map(|(a, b)| (*b - *a).length() * 0.5 / r).fold(0.0f64, f64::max);
                        let len0 = c0.windows(2).map(|w| (w[1] - w[0]).length()).sum::<f64>();
                        let len1 = c1.windows(2).map(|w| (w[1] - w[0]).length()).sum::<f64>();
                        note(&format!(
                            "the chord is wider than the ball: off by {fit:.2} r; half-chords this pairing {rails:.2} r, the other {other:.2} r; side lengths {:.2} r / {:.2} r",
                            len0 / r,
                            len1 / r
                        ));
                    } else {
                        note("the chord is wider than the ball");
                    }
                    return None;
                }
            }
            // Rail direction at this station, from the neighbours.
            let along = (c0[(i + 1).min(m - 1)] - c0[i.saturating_sub(1)]).try_normalized();
            let Some(along) = along else {
                note("the rail has no direction here");
                return None;
            };
            let Some(chord_dir) = chord.try_normalized() else {
                note("the two rails meet; no section");
                return None;
            };
            // The bisector direction, in the section plane, perpendicular to the chord.
            let bis = (along.cross(chord_dir)).try_normalized();
            let Some(bis) = bis else {
                note("the rail runs along the chord; no section plane");
                return None;
            };
            let h = (r * r - half * half).max(0.0).sqrt();
            let midpoint = *a + chord * 0.5;
            // Side: the centre lies opposite the face's interior. The
            // interior is towards the patch's own middle row — for the first
            // rail that is the direction to the far rail's midpoint at this
            // station, which is the chord itself; so the bulge is the side of
            // the chord the *other* rails' midpoints are not on. Use the
            // patch's central section as the reference.
            let centre_ref = {
                let k = m / 2;
                (c0[k] + c2[k]) * 0.5
            };
            let towards_face = centre_ref - midpoint;
            let away = towards_face.dot(bis) > 0.0;
            let centre = if away != flip { midpoint - bis * h } else { midpoint + bis * h };
            // If the chord is exactly a diameter the two choices coincide.
            let centre = if h == 0.0 { midpoint } else { centre };
            let (u, w) = (*a - centre, *b - centre);
            let eu = u.try_normalized()?;
            let perp = (w - eu * w.dot(eu)).try_normalized()?;
            let angle = w.dot(perp).atan2(w.dot(eu));
            // The arc ends exactly on `b` whatever the fit, so the boundary is
            // carried to the bit; only the interior is the arc's.
            let row: Vec<Vec3> = (0..=n)
                .map(|j| {
                    let t = angle * j as f64 / n as f64;
                    let q = centre + (eu * t.cos() + perp * t.sin()) * r;
                    if j == n { *b } else if j == 0 { *a } else { q }
                })
                .collect();
            grid.push(row);
        }
        // The arc carries the two rails to the bit and nothing else. A patch
        // has four sides: the first and last sections are the cross rails
        // `c3` and `c1`, and the face's own boundary runs along them —
        // measured, with the arcs alone the Coons patch missed the boundary's
        // end points by 0.02 µm and the arcs by 0.43 µm, and the gate refused
        // 385 of 389 arc patches *there* and nowhere else. So the ends are
        // the boundary exactly, and each interior section is blended from its
        // arc towards them by how near it is, the way a Coons patch blends a
        // ruled interior towards its sides.
        let m = grid.len();
        if c1.len() == n + 1 && c3.len() == n + 1 && m >= 2 {
            for (i, row) in grid.iter_mut().enumerate() {
                let u = i as f64 / (m - 1) as f64;
                for (j, q) in row.iter_mut().enumerate() {
                    // What the arc says minus what a straight rule between the
                    // cross rails would say, faded in by distance from them.
                    let side = c3[j] * (1.0 - u) + c1[j] * u;
                    let chord = c0[i] * (1.0 - j as f64 / n as f64) + c2[i] * (j as f64 / n as f64);
                    let bulge = *q - chord;
                    // The cross-rail rule already carries c0 and c2 at its
                    // ends (they share corners), so the bulge is added back
                    // scaled to vanish on the cross rails.
                    // A rule between the cross rails, plus the arc's bulge
                    // faded to nothing at those rails. `side` already carries
                    // c0 and c2 at j = 0 and j = n, since the rails share
                    // corners; the two rails are pinned regardless.
                    let w = (4.0 * u * (1.0 - u)).min(1.0);
                    *q = side + bulge * w;
                    if j == 0 {
                        *q = c0[i];
                    }
                    if j == n {
                        *q = c2[i];
                    }
                }
            }
            // first and last sections are the cross rails exactly
            grid[0] = c3.to_vec();
            grid[m - 1] = c1.to_vec();
        }
        note(&format!("built, worst fit {worst_fit:.3} r"));
        Some(grid)
    }

    /// Put every rolled face of a blend that could not roll them all back onto
    /// the interpolation its siblings use.
    fn make_each_blend_of_one_mind(&mut self) -> usize {
        let mut demoted = 0;
        let pending = std::mem::take(&mut self.rolled_faces);
        for (blend, sid, grid) in pending {
            if !self.blends_that_fell_back.contains(&blend) {
                continue;
            }
            let rows = grid.len().saturating_sub(1);
            let cols = grid.first().map(|r| r.len()).unwrap_or(0).saturating_sub(1);
            if rows < 1 || cols < 1 {
                continue;
            }
            let knots = |n: usize| {
                let mut k = vec![0.0];
                k.extend((0..=n).map(|i| i as f64 / n as f64));
                k.push(1.0);
                k
            };
            if let Some(flag) = self.solid.measured.get_mut(sid.0 as usize) {
                *flag = false;
            }
            self.solid.surfaces[sid.0 as usize] =
                cad_ir::brep::Surface::Nurbs(cad_ir::brep::NurbsSurface {
                    u_degree: 1,
                    v_degree: 1,
                    control_points: grid,
                    weights: Vec::new(),
                    u_knots: knots(rows),
                    v_knots: knots(cols),
                    u_closed: false,
                    v_closed: false,
                });
            demoted += 1;
        }
        demoted
    }

    /// Store a rebuilt grid as the face's surface and hand the face its loops.
    ///
    /// The grid is kept as a degree-one NURBS through its own points, so
    /// downstream it is an ordinary surface: invertible — exactly, cell by
    /// cell — tessellatable, with no special case anywhere else.
    fn finish_rebuilt_face(
        &mut self,
        fe: &RawEntity,
        grid: Vec<Vec<Vec3>>,
        outer: Bound,
        bounds: Vec<Bound>,
        measured: bool,
    ) -> Result<Option<FaceId>, String> {
        let rows = grid.len().saturating_sub(1);
        let cols = grid.first().map(|r| r.len()).unwrap_or(0).saturating_sub(1);
        if rows < 1 || cols < 1 {
            return Err("boundary rebuild produced an empty grid".into());
        }
        let knots = |n: usize| {
            let mut k = vec![0.0];
            k.extend((0..=n).map(|i| i as f64 / n as f64));
            k.push(1.0);
            k
        };
        // A grid of flat cells is stored degree one in both directions, and
        // the tessellator reads that as "rebuild me from my boundary" — it
        // never evaluates such a surface, so the grid reaches the mesh only as
        // the shape of the interior the rebuild fills in.
        //
        // Two ways of making the tessellator evaluate the arc instead have
        // been tried and measured, and both were worse than letting it rebuild:
        // degree two through the 96 samples (a quadratic B-spline smooths its
        // control polygon rather than passing through it — 2.07 M triangles to
        // 5.44 M, four open half-edges), and the exact rational arc, three
        // control points across the section with weight cos(θ/2) (2.07 M to
        // 3.30 M triangles, 0 open half-edges to 60, 11 non-manifold to 37,
        // points over 1 mm against OpenCASCADE 14 to 52). The second failed
        // for the reason the rebuild exists: the face's boundary is a curve of
        // the file, and a conic that meets a cross rail at three points does
        // not carry the rest of it, so every boundary point in between inverts
        // onto a surface it is not on and slits the face against its
        // neighbour. The boundary is authoritative; only the interior is the
        // arc's, and the interior is what the grid is for.
        let surface = cad_ir::brep::Surface::Nurbs(cad_ir::brep::NurbsSurface {
            u_degree: 1,
            v_degree: 1,
            control_points: grid,
            weights: Vec::new(),
            u_knots: knots(rows),
            v_knots: knots(cols),
            u_closed: false,
            v_closed: false,
        });
        let sid = SurfaceId(self.solid.surfaces.len() as u32);
        self.solid.surfaces.push(surface);
        // Only a grid the ball actually rolled is evidence about the interior.
        // An arc-sectioned grid is a construction from the record's radius and
        // a Coons patch is the boundary restated, and neither is a measurement.
        self.solid.measured.resize(self.solid.surfaces.len(), false);
        self.solid.measured[sid.index()] = measured;

        // The patch follows the loop's own winding, so the surface normal
        // already points the face's outward way.
        let mut all = vec![Bound {
            outer: true,
            ..outer
        }];
        for b in bounds {
            all.push(Bound { outer: false, ..b });
        }

        let fid = FaceId(self.solid.faces.len() as u32);
        self.solid.faces.push(Face {
            surface: sid,
            same_sense: true,
            bounds: all,
        });
        self.face_sources.push(fe.index);
        Ok(Some(fid))
    }

    /// The blend face's grid built from its true rolling-ball cross-sections,
    /// or `None` when the face is not one or the construction does not close.
    ///
    /// A constant-radius rolling-ball blend is the envelope of a ball of
    /// radius `r` rolling in the crease between two surfaces, so it is fully
    /// determined by one of its two rails: at a rail point the ball touches
    /// that surface, which puts its centre exactly `r` along the surface
    /// normal, and the far end of the cross-section is wherever that ball
    /// touches the other surface. Solving for the far contact rather than
    /// pairing the two rails station by station is what makes this exact —
    /// the rails are sampled by arc length independently, so station `i` on
    /// one is not the cross-section partner of station `i` on the other, and
    /// pairing them directly bends every section that is not symmetric.
    ///
    /// Which surface the rail lies on, and which way the centre lies off it,
    /// are not stated in the file, so all four combinations are tried and one
    /// is accepted only if the ball stays on the far surface along the whole
    /// rail. A face that fails is left to the Coons patch rather than forced
    /// into a shape it is not.
    fn rolling_ball_grid(
        &self,
        fe: &RawEntity,
        rail: &[Vec3],
        n: usize,
        ring: &[Vec3],
        limit: f64,
    ) -> Option<Vec<Vec<Vec3>>> {
        // Which of the preconditions turns a blend face away, counted rather
        // than guessed: seventy-two per cent of them fall back to a plain
        // Coons patch over the boundary, and that is where the Parasolid
        // reading still parts from the STEP one.
        let note = |why: &str| {
            if std::env::var_os("XT_ROLL_PROBE").is_some() {
                eprintln!("[roll] {why}");
            }
        };
        let be = self.index.get(&ptr(self.entities, fe, 7))?;
        if be.type_id != xt::BLENDED_EDGE {
            note("the face's surface is not a blend");
            return None;
        }
        if chr(self.entities, be, 7) != 'R' {
            note(&format!("the blend is type {:?}, not a rolling ball", chr(self.entities, be, 7)));
            return None;
        }
        // BLENDED_EDGE: [8],[9] the mating surfaces, [11] the radius.
        let radius = f64_at(self.entities, be, 11).abs();
        if !(radius.is_finite() && radius > 0.0) {
            note("the blend states no radius");
            return None;
        }
        let Some(ea) = self.index.get(&ptr(self.entities, be, 8)) else {
            note("the blend names no first surface");
            return None;
        };
        let Some(eb) = self.index.get(&ptr(self.entities, be, 9)) else {
            note("the blend names no second surface");
            return None;
        };
        // The surface a blend mates against is often another blend, and the
        // plain lowering refuses those: 1,692 of the attempts here fail on
        // exactly that, and the face falls back to a Coons patch over its own
        // boundary. Following them with `surface_for_curve`, which does roll a
        // blend out into a surface, was measured and is not worth it: fourteen
        // more faces roll, not one number against OpenCASCADE moves, and
        // lowering goes from 7.4 to 15.7 seconds. What actually turns these
        // faces away is the roll itself — 2,610 of the attempts end with the
        // ball unable to stay on both surfaces along the rail.
        let a = match geom::surface(self.entities, ea, self.index) {
            Ok(s) => s,
            Err(why) => {
                note(&format!("the first mating surface: {why}"));
                return None;
            }
        };
        let b = match geom::surface(self.entities, eb, self.index) {
            Ok(s) => s,
            Err(why) => {
                note(&format!("the second mating surface: {why}"));
                return None;
            }
        };
        let rolled = "the ball would not stay on both surfaces along the rail";
        // The first reading that rolls is taken, and the grid it produces is
        // checked against the face's own boundary by the caller. Ordering the
        // readings first is what makes that enough — see below.
        //
        // Making the *search* boundary-aware instead, carrying on to the next
        // reading and the next rail whenever a grid does not hold the face,
        // was built and measured:
        //
        // | | faces rolled | lowering |
        // |---|---|---|
        // | ordered, checked after *(kept)* | 468 | 14.0 s |
        // | unordered, checked after        | 385 | 12.3 s |
        // | ordered, checked inside         | 572 | 27.7 s |
        // | unordered, checked inside       | 585 | 26.0 s |
        //
        // The cost of searching is the extra *rolling*, not the test — that is
        // about a second of it — and neither seeding each solve from the last
        // nor bounding the sheet with a box before walking its cells shifts
        // it. Stopping at the two rails that run *along* the blend is worse
        // than any of them: 164 faces for 19.6 seconds, because most of what
        // rolls does so on a cross rail.
        //
        // Ordering buys 83 of those faces for 1.7 seconds where searching buys
        // 104 more for another 13.7. And both readings put every face on a
        // surface its boundary sits on: a grid that fails the check is
        // replaced by the interpolation, which carries the boundary exactly,
        // being built from it. The faces differ only in the fidelity of their
        // *interior*, which nothing measurable here can see.
        // Which surface the rail lies on, and which way the ball sits off it,
        // are not stated in the file, so all four readings are offered. Taking
        // whichever of them first satisfies the ball's two contacts picks the
        // wrong one often: measured, of the grids that then fail to carry the
        // face, the median has *half* its boundary adrift — the signature of a
        // sheet built to the right rail and the wrong far track.
        //
        // So they are put in order first, by where the far contact lands at
        // the rail's own first point: the reading whose contact falls on the
        // face's boundary is the reading whose sheet will carry it. That is
        // four evaluations of one point, against four full rolls.
        // The same threshold `roll` will apply, so the ordering rejects what
        // it would reject rather than ranking it.
        let tolerance = (radius * 0.01).max(self.solid.tolerance * 20.0);
        let mut order = [(&a, &b, 1.0), (&a, &b, -1.0), (&b, &a, 1.0), (&b, &a, -1.0)];
        if let Some(first) = rail.first().copied() {
            let score = |(near, far, sign): &(&cad_ir::brep::Surface, &cad_ir::brep::Surface, f64)| {
                let Some(uv) = near.invert(first, None) else { return f64::INFINITY };
                if (near.point_at(uv) - first).length() > tolerance {
                    return f64::INFINITY;
                }
                let centre = first + near.normal_at(uv) * (radius * sign);
                let Some(fuv) = far.invert(centre, None) else { return f64::INFINITY };
                let contact = far.point_at(fuv);
                if ((contact - centre).length() - radius).abs() > tolerance {
                    return f64::INFINITY;
                }
                distance_to_polyline(contact, ring)
            };
            order.sort_by(|x, y| score(x).total_cmp(&score(y)));
        }
        let out = order
            .into_iter()
            .find_map(|(near, far, sign)| {
                let got = self.roll(rail, near, far, radius, sign, n, 1.0);
                // The prediction: the rail runs on past the face's own contact
                // track, so trimmed to the contiguous run the named mate
                // holds, the roll should carry the face where the full rail
                // does not.
                if std::env::var_os("XT_TRIM_PROBE").is_some() && got.is_none() {
                    let holds: Vec<bool> = rail
                        .iter()
                        .map(|p| {
                            near.invert(*p, None).is_some_and(|uv| {
                                (near.point_at(uv) - *p).length() <= tolerance && {
                                    let c = *p + near.normal_at(uv) * (radius * sign);
                                    far.invert(c, None).is_some_and(|q| {
                                        ((far.point_at(q) - c).length() - radius).abs() <= tolerance
                                    })
                                }
                            })
                        })
                        .collect();
                    let (mut best, mut len, mut i) = (0usize, 0usize, 0usize);
                    while i < holds.len() {
                        if !holds[i] { i += 1; continue; }
                        let start = i;
                        while i < holds.len() && holds[i] { i += 1; }
                        if i - start > len { best = start; len = i - start; }
                    }
                    if len >= 4 && len < rail.len() {
                        let trimmed: Vec<Vec3> = rail[best..best + len].to_vec();
                        let again = self.roll(&trimmed, near, far, radius, sign, n, 1.0);
                        let verdict = match &again {
                            None => "still will not roll",
                            Some(g) if grid_holds(g, ring, limit) => "rolls and carries the face",
                            Some(_) => "rolls but does not carry the face",
                        };
                        eprintln!("[trim] rail {} -> {len} points: {verdict}", rail.len());
                        // The last thing the record could be wrong about is
                        // the radius. A rolling ball's cross-section is an arc
                        // of that radius, so the distance from a rail point to
                        // the far contact is a chord of it: compare what the
                        // boundary says with what the record says.
                        if let Some(g) = &again {
                            let mid = &trimmed[len / 2];
                            let row = &g[len / 2];
                            let swept = (row[row.len() - 1] - row[0]).length();
                            let to_boundary = ring
                                .iter()
                                .filter(|q| (**q - *mid).length() > radius * 0.5)
                                .map(|q| (*q - *mid).length())
                                .fold(f64::INFINITY, f64::min);
                            // And the radius the *face* implies: a ball of
                            // radius r touching both mates at this rail point
                            // spans a chord c = 2 r sin(θ/2) across the
                            // dihedral θ; the boundary width is that chord,
                            // so r_face = r_record * (width / swept).
                            let implied = radius * to_boundary / swept.max(1e-12);
                            eprintln!(
                                "[radius] record {:.5}  arc chord swept {:.5}  nearest far boundary point from the rail {:.5}  implied {:.5}",
                                radius, swept, to_boundary, implied
                            );
                            // The direct test of that reading: roll the same
                            // rail with the implied radius and ask the
                            // boundary. If it carries the face, the record's
                            // radius is not this face's; if it does not, the
                            // implied radius was a wrong inference.
                            if implied.is_finite() && implied > 0.0 {
                                let n2 = trimmed.len().clamp(16, 96);
                                let verdict = match self.roll(&trimmed, near, far, implied, sign, n2, 1.0) {
                                    None => "will not roll at the implied radius",
                                    Some(g) if grid_holds(&g, ring, limit) => "rolls at the implied radius and carries the face",
                                    Some(_) => "rolls at the implied radius but does not carry the face",
                                };
                                eprintln!("[implied] {verdict}");
                            }
                        }
                    }
                }
                // When the sheet does not carry the face, ask whether a wider
                // arc of the same ball would: that separates "the ball is
                // wrong" from "the face reaches past the ball's two contacts".
                if std::env::var_os("XT_SWEEP_PROBE").is_some()
                    && got.as_ref().is_some_and(|g| !grid_holds(g, ring, limit))
                {
                    let wider = [1.5f64, 2.0, 3.0].iter().find(|w| {
                        self.roll(rail, near, far, radius, sign, n, **w)
                            .is_some_and(|g| grid_holds(&g, ring, limit))
                    });
                    match wider {
                        Some(w) => eprintln!("[sweep] a {w}x wider arc of the same ball carries the face"),
                        None => eprintln!("[sweep] no wider arc of this ball carries the face"),
                    }
                }
                let worst = ATTEMPT_WORST.with(|m| m.get());
                BEST_MISS.with(|m| m.set(m.get().min(worst)));
                let _ = (ring, limit);
                got
            });
        if out.is_none() {
            note(rolled);
        }
        // Landing each cross-section's far end on the face's other rail was
        // tried here, on the reading that the near edge is the file's own rail
        // and the far one is only solved. It did not close the gap it was
        // written for — the median grid corner stayed 0.52 mm from the nearest
        // ring point — and it cost grids that had been accepted: offered
        // 1034 → 910, used 209 → 195. So the drift is not in the far edge.
        //
        // What is left, measured but not closed: the reader's ring and the
        // tessellator's are two samplings of the same boundary, and a corner
        // exact on one need not be near a point of the other.
        out
    }

    /// A blend face bounded by two rails rather than by one loop with a hole.
    ///
    /// A fillet that runs all the way round a closed feature is an annulus:
    /// its two loops are the ball's two contact tracks, not an outer boundary
    /// and a hole cut out of it. Treating them the Coons way — span the larger,
    /// cut the smaller out — parameterises the face across a shape it does not
    /// have, and the boundary points then land on parameters the triangulation
    /// cannot separate. Rolling the ball along one rail gives the band its own
    /// parameterisation, running round in `u` and across in `v`.
    ///
    /// Whether the two loops really are rails is measured, not assumed: the
    /// far end of every cross-section has to land on the other loop.
    fn rolling_ball_band(
        &self,
        fe: &RawEntity,
        ring_a: &[Vec3],
        ring_b: &[Vec3],
        n: usize,
    ) -> Option<Vec<Vec<Vec3>>> {
        if ring_a.len() < 3 || ring_b.len() < 3 {
            return None;
        }
        let rail = resample_closed(ring_a, n);
        // A band carries its rail by construction; what has to be checked is
        // the far end, which the test below does against the other ring.
        let grid = self.rolling_ball_grid(fe, &rail, n, &rail, self.solid.tolerance * 3.0)?;
        // The band is only this if it ends where the file says it ends.
        let reach = polyline_extent(ring_b);
        let worst = grid
            .iter()
            .filter_map(|row| row.last())
            .map(|q| distance_to_polyline(*q, ring_b))
            .fold(0.0f64, f64::max);
        (worst <= reach * 0.01).then_some(grid)
    }

    /// Sweep the ball along `rail`, one cross-section per station.
    ///
    /// `None` as soon as a station does not work out: the ball has to sit `r`
    /// off the near surface and touch the far one at exactly `r`, and a
    /// hundredth of the radius is far tighter than any writer's rounding
    /// while still refusing a pairing that is simply wrong.
    /// `roll` with the contact tolerance given rather than derived. Probes only.
    fn roll_with_tolerance(
        &self,
        rail: &[Vec3],
        near: &cad_ir::brep::Surface,
        far: &cad_ir::brep::Surface,
        radius: f64,
        sign: f64,
        n: usize,
        tolerance: f64,
    ) -> Option<Vec<Vec<Vec3>>> {
        let mut grid = Vec::with_capacity(rail.len());
        let (mut nh, mut fh) = (None, None);
        for p in rail.iter().copied() {
            let uv = near.invert(p, nh)?;
            if (near.point_at(uv) - p).length() > tolerance { return None; }
            nh = Some(uv);
            let centre = p + near.normal_at(uv) * (radius * sign);
            let fuv = far.invert(centre, fh)?;
            fh = Some(fuv);
            let contact = far.point_at(fuv);
            if ((contact - centre).length() - radius).abs() > tolerance { return None; }
            let (u, w) = (p - centre, contact - centre);
            let eu = u.try_normalized()?;
            let perp = (w - eu * w.dot(eu)).try_normalized()?;
            let angle = w.dot(perp).atan2(w.dot(eu));
            grid.push((0..=n).map(|j| { let t = angle * j as f64 / n as f64; centre + (eu * t.cos() + perp * t.sin()) * radius }).collect());
        }
        Some(grid)
    }

    fn roll(
        &self,
        rail: &[Vec3],
        near: &cad_ir::brep::Surface,
        far: &cad_ir::brep::Surface,
        radius: f64,
        sign: f64,
        n: usize,
        sweep: f64,
    ) -> Option<Vec<Vec<Vec3>>> {
        // The ball has to touch both surfaces — but "touch" is judged against
        // what the file can answer, not against the radius alone. A hundredth
        // of the radius, on a small fillet, is finer than the body's own
        // stated tolerance, and the rail is a polyline sampled from the
        // boundary rather than the exact contact track. Of the faces that fell
        // back under that test, the best attempt's worst miss had a median of
        // **1.26 times** it, and three quarters were inside twice: near
        // misses, not different geometry.
        //
        // Loosening it alone was measured, in faces rolled out of 1,447:
        //
        // * radius x 1%            401 rolled, closed and manifold
        // * body tolerance x 3     575 rolled, **164 open half-edges**
        // * body tolerance x 10    760 rolled, closed and manifold
        // * body tolerance x 20    802 rolled, closed and manifold
        //
        // The middle tearing while both ends held was the puzzle, and it was
        // not one blend read two ways — `make_each_blend_of_one_mind` below
        // would have caught that, and it never fires. It was that the ball's
        // two contacts are not the whole test: a ball can sit correctly on
        // both mating surfaces along one rail and still sweep a sheet the
        // face's *other* edges are nowhere near.
        //
        // `grid_holds` asks that question directly — does every point of the
        // face's own boundary sit on the grid this ball swept — and with it
        // every threshold is closed and manifold, because the grids that would
        // have torn the mesh are exactly the ones it refuses. At this setting
        // 760 balls satisfy their contacts, **157 of them do not carry their
        // own face**, and 603 are kept. At the tight setting the same check
        // refuses 148 of the 401 that were being accepted before it existed.
        //
        // Being generous here is deliberate. The ball is the file's own
        // definition of the surface and the interpolation is ours, so with the
        // grid checked against the face, letting more candidates reach that
        // check costs time and buys fidelity, and nothing else.
        let tolerance = (radius * 0.01).max(self.solid.tolerance * 20.0);
        ATTEMPT_WORST.with(|m| m.set(0.0));
        let watching = std::env::var_os("XT_ROLL_TRACE").is_some();
        let mut grid = Vec::with_capacity(n + 1);
        // The rail is a continuous path and so is the ball's centre line, so
        // each solve starts from the last one's answer. Without the hint every
        // point pays for a search of the whole surface, and the search is run
        // for every pairing of every rail.
        let mut near_hint: Option<cad_ir::Vec2> = None;
        let mut far_hint: Option<cad_ir::Vec2> = None;
        for p in rail.iter().copied() {
            let Some(uv) = near.invert(p, near_hint) else {
                if watching {
                    eprintln!("[rolltrace] the rail does not invert onto the near surface");
                }
                return None;
            };
            let off = (near.point_at(uv) - p).length();
            ATTEMPT_WORST.with(|m| m.set(m.get().max(off / tolerance)));
            if off > tolerance {
                if watching {
                    eprintln!(
                        "[rolltrace] the rail sits {off:.6} off the near surface, tolerance {tolerance:.6}, radius {radius:.4}"
                    );
                }
                return None;
            }
            near_hint = Some(uv);
            let centre = p + near.normal_at(uv) * (radius * sign);
            // The far surface is whichever of the candidates the ball actually
            // touches here, not one named once for the whole rail. A blend
            // runs on past the surface its own record names — measured, the
            // named pair holds along a contiguous 70% of the track and then
            // stops — and where it goes next is a face this one already
            // shares an edge with.
            // The far contact, on the surface the blend's record names.
            //
            // Offering the neighbours' surfaces alongside it — a blend runs on
            // past its named mate, and where it goes is a face this one
            // already touches — was built and measured, and does not pay. By
            // proximity it is far worse (137 faces roll instead of 518,
            // because a neighbour can sit nearer than the true mate and the
            // sheet follows the wrong surface from the start); in order, the
            // named mate first and a neighbour only where it misses, it is
            // still slightly worse at 506 and two seconds slower — a
            // neighbour satisfies the distance test spuriously often enough to
            // let a roll finish with a wrong far track, where failing would
            // have sent the search to a rail that works.
            // The far contact, on the surface the blend's record names — and
            // only that one. Two ways of letting another surface stand in for
            // it were built and measured, and both are worse:
            //
            // * offered as equals (the named mate plus the neighbours across
            //   this face's edges): 137 faces roll by proximity, 506 in order,
            //   against 518 with the mate alone;
            // * as a handover — asked for only once the named mate has let go,
            //   then committed to — 506 again, at 41 seconds, and **not one of
            //   the 62 rolls that handed over carried the face's boundary**.
            //
            // That last number is the finding. 93% of the track points the
            // named mate misses *are* held by some other surface of the body,
            // so the ball's centre line is sound past the mate; but the sheet
            // it then sweeps belongs to the blend on that other surface, not
            // to this face. The face ends where its mate ends. What runs on
            // past it is the rail — the boundary polyline the corner search
            // handed over, which is longer than the face's own contact track.
            let Some(fuv) = far.invert(centre, far_hint) else {
                if watching {
                    eprintln!("[rolltrace] the ball's centre does not invert onto the far surface");
                }
                return None;
            };
            far_hint = Some(fuv);
            let contact = far.point_at(fuv);
            let out = ((contact - centre).length() - radius).abs();
            let reach = out + radius;
            ATTEMPT_WORST.with(|m| m.set(m.get().max(out / tolerance)));
            if out > tolerance {
                // Letting the rail's two end points miss and borrow a
                // neighbour's section was built and measured: on the fillet
                // it was built for, the misses were not at the ends at all —
                // A→B missed at track points 18, 19, 20 and 26 of 27, by
                // 1.9–3.1 tolerances, with the ball simply 60–90 µm off the
                // far mate mid-track — and of the ten grids the loose ends did
                // let through, none carried the face. Reverted.
                if watching {
                    eprintln!(
                        "[rolltrace] the ball reaches {reach:.6} to the far surface, wanted {radius:.6}, out by {:.6}, tolerance {tolerance:.6}",
                        out
                    );
                }
                return None;
            }
            // Sweep from the near contact to the far one about the centre.
            let (u, w) = (p - centre, contact - centre);
            let eu = u.try_normalized()?;
            let perp = (w - eu * w.dot(eu)).try_normalized()?;
            let angle = w.dot(perp).atan2(w.dot(eu));
            let row = (0..=n)
                .map(|j| {
                    // `sweep` widens the arc about its own middle, which is
                    // how the question "does this face reach past the ball's
                    // two contacts" is put to the geometry.
                    let f = (j as f64 / n as f64 - 0.5) * sweep + 0.5;
                    let t = angle * f;
                    centre + (eu * t.cos() + perp * t.sin()) * radius
                })
                .collect();
            grid.push(row);
        }
        Some(grid)
    }

    /// Diagonal of a loop's bounding box, for picking the outer one.
    fn loop_extent(&self, bound: &Bound) -> f64 {
        let mut b = cad_ir::math::Aabb::EMPTY;
        for h in &bound.halves {
            let e = self.solid.edge(h.edge);
            b.add_point(self.solid.vertex(e.start));
            b.add_point(self.solid.vertex(e.end));
        }
        b.diagonal()
    }

    /// The loop's edges walked head to tail as one closed polyline.
    fn boundary_polyline(&self, bound: &Bound) -> Vec<Vec3> {
        const PER_EDGE: usize = 16;
        self.boundary_polyline_at(bound, PER_EDGE)
    }

    /// The same walk at a chosen density. A loop written as one closed curve
    /// carries as much shape as one written as a dozen edges, so a fixed count
    /// per edge samples the two very differently — which matters wherever two
    /// loops are compared against each other rather than used on their own.
    fn boundary_polyline_at(&self, bound: &Bound, per_edge: usize) -> Vec<Vec3> {
        let mut out: Vec<Vec3> = Vec::new();
        for h in &bound.halves {
            let e = self.solid.edge(h.edge);
            let c = self.solid.curve(e.curve);
            let n = per_edge;
            let pts = (0..=n).map(|k| {
                let t = k as f64 / n as f64;
                c.point_at(e.range.at(if h.forward { t } else { 1.0 - t }))
            });
            for p in pts {
                if out.last().is_some_and(|l| (*l - p).length_squared() < 1e-24) {
                    continue;
                }
                out.push(p);
            }
        }
        while out.len() > 1
            && out
                .first()
                .zip(out.last())
                .is_some_and(|(f, l)| (*f - *l).length_squared() < 1e-24)
        {
            out.pop();
        }
        out
    }

    /// LOOP [2] points into the fin cycle; fins link via their forward
    /// pointer and each carries the vertex the loop enters its edge at.
    fn lower_loop(&mut self, le: &RawEntity) -> Result<Option<Bound>, String> {
        let first = ptr(self.entities, le, 2);
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
            let a = usize::from(self.entities.fields(fe).len() < 10);
            // A fin names the loop it belongs to. Following the forward
            // pointer without checking it walks straight out of this loop and
            // through the rest of the face — one plane in the Solid Edge
            // assembly collected 163 half-edges that way, and its holes and
            // outer profile fused into a single polygon whose triangulation
            // agreed with nothing around it.
            if ptr(self.entities, fe, 1 - a) != le.index {
                break;
            }
            cycle.push(fe);
            fin = ptr(self.entities, fe, 2 - a);
        }
        if cycle.is_empty() {
            return Ok(None);
        }

        // The vertex each fin starts at, in cycle order.
        let starts: Vec<usize> = cycle
            .iter()
            .map(|fe| {
                let a = usize::from(self.entities.fields(fe).len() < 10);
                self.vertex_position_handle(ptr(self.entities, fe, 4 - a))
            })
            .collect();

        let mut halves = Vec::with_capacity(cycle.len());
        for (i, fe) in cycle.iter().enumerate() {
            let a = usize::from(self.entities.fields(fe).len() < 10);
            let edge_ptr = ptr(self.entities, fe, 6 - a);
            let sense = chr(self.entities, fe, 9 - a);
            let pcurve_ptr = ptr(self.entities, fe, 7 - a);
            if edge_ptr == 0 {
                self.skip(
                    ptr(self.entities, fe, 0),
                    "fin names no edge; the loop it belongs to is left open here",
                );
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
                        .and_then(|pe| geom::pcurve_of(self.entities, pe, self.index));
                    halves.push(HalfEdge {
                        edge,
                        forward: forward ^ built_reversed,
                        pcurve,
                    });
                }
                Err(reason) => self.skip(edge_ptr, reason),
            }
        }
        if std::env::var_os("XT_LOOP_TRACE").is_some() {
            let senses: String = cycle
                .iter()
                .map(|fe| chr(self.entities, fe, 9 - usize::from(self.entities.fields(fe).len() < 10)))
                .collect();
            let dirs: String = halves
                .iter()
                .map(|h| if h.forward { '+' } else { '-' })
                .collect();
            let where_it_starts = halves
                .first()
                .map(|h| {
                    let e = &self.solid.edges[h.edge.index()];
                    let c = &self.solid.curves[e.curve.index()];
                    // A closed edge's two ends are the same point, so the
                    // direction only shows a quarter of the way along.
                    let (t0, t1) = if h.forward {
                        (e.range.lo, e.range.lo + 0.25 * e.range.span())
                    } else {
                        (e.range.hi, e.range.hi - 0.25 * e.range.span())
                    };
                    let (a, b) = (c.point_at(t0), c.point_at(t1));
                    let axis = match c {
                        Curve::Circle { frame, radius } => format!(
                            " circle r={radius:.5} axis=[{:.3},{:.3},{:.3}] ref=[{:.3},{:.3},{:.3}]",
                            frame.axis.x, frame.axis.y, frame.axis.z,
                            frame.ref_dir.x, frame.ref_dir.y, frame.ref_dir.z
                        ),
                        _ => String::new(),
                    };
                    format!(
                        "[{:.4},{:.4},{:.4}] -> [{:.4},{:.4},{:.4}]{axis}",
                        a.x, a.y, a.z, b.x, b.y, b.z
                    )
                })
                .unwrap_or_default();
            let edges: String = cycle
                .iter()
                .map(|fe| {
                    let a = usize::from(self.entities.fields(fe).len() < 10);
                    let ep = ptr(self.entities, fe, 6 - a);
                    let cp = self.index.get(&ep).map(|ee| ptr(self.entities, ee, 6)).unwrap_or(0);
                    let raw = self
                        .index
                        .get(&cp)
                        .map(|ce| {
                            let f: Vec<String> =
                                self.entities.fields(ce).iter().map(|v| format!("{v:?}")).collect();
                            format!("type={} {}", ce.type_id, f.join(" "))
                        })
                        .unwrap_or_default();
                    format!("edge={ep} curve={cp} [{raw}]")
                })
                .collect::<Vec<_>>()
                .join("; ");
            eprintln!(
                "[loop] loop={} fins={} senses=[{senses}] half-edges=[{dirs}] first half-edge walks {where_it_starts} :: {edges}",
                le.index,
                cycle.len()
            );
        }
        self.close_loop(&mut halves);
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
        let pe = self.index.get(&ptr(self.entities, ve, 5))?;
        let a = self.entities.fields(pe).get(5).map(|f| f.as_vec3())?;
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
        let curve_ptr = ptr(self.entities, ee, 6);
        let tolerant_ok = std::env::var_os("XT_NO_TOLERANT").is_none();
        let mut route = if curve_ptr != 0 { "own-curve" } else { "stand-in" };
        let mut curve_id = if curve_ptr != 0 {
            match self.intern_curve(curve_ptr) {
                Ok(id) => id,
                // The edge's own curve is an SP_CURVE living in the parameter
                // space of a surface this crate cannot lower — a blend. The
                // same edge is written a second time as each fin's parameter
                // curve, and the fin on the *other* side of it sits on
                // ordinary geometry, so the edge is read from there instead.
                // Same edge, read from the side that can be evaluated; losing
                // it entirely left every loop that used it broken.
                Err(reason) if fin_pcurve != 0 && tolerant_ok => {
                    route = "tolerant-sp-curve";
                    self.intern_tolerant_curve(edge_ptr, fin_pcurve)
                        .map_err(|second| format!("{reason}; {second}"))?
                }
                Err(reason) => match self.blend_section(edge_ptr, start, end) {
                    Some(id) => {
                        route = "blend-section";
                        id
                    }
                    None => return Err(reason),
                },
            }
        } else if tolerant_ok && !self.pcurves_of(edge_ptr, fin_pcurve).is_empty() {
            // A tolerant edge has no 3D curve at all — its geometry lives in
            // each fin's SP_CURVE. This fin's own is tried first, but when
            // this fin sits on a blend the pcurve is in a parameter space
            // that cannot be evaluated, and the edge would be lost. Its
            // partner fin describes the same edge from the face on the other
            // side, which is usually ordinary geometry, so every fin on the
            // edge is offered and the first that evaluates wins.
            let mut last = String::new();
            let mut found = None;
            for cand in self.pcurves_of(edge_ptr, fin_pcurve) {
                match self.intern_tolerant_curve(edge_ptr, cand) {
                    Ok(id) => {
                        found = Some(id);
                        break;
                    }
                    Err(reason) => last = reason,
                }
            }
            match found.or_else(|| self.blend_section(edge_ptr, start, end)) {
                Some(id) => id,
                None => return Err(last),
            }
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

        if std::env::var_os("XT_CLOSED_TRACE").is_some() && closed_no_vertex {
            eprintln!(
                "[closed] edge {edge_ptr}: fins gave no vertices (start={fwd_start} end={fwd_end}) \
                 -> full range on curve {:?}",
                std::mem::discriminant(curve)
            );
        }
        let tol = f64_at(self.entities, ee, 2);
        let tolerance = if tol.is_finite() && tol > 0.0 {
            tol
        } else {
            self.solid.tolerance
        };

        // A trimmed curve carries an interval — but that interval belongs to
        // the curve, and it is only this edge's range if walking it starts and
        // finishes at this edge's own two vertices. Taken on trust it is
        // wrong for 220 of the pilot assembly's 26,531 edges, every one of
        // them a chart written as a polyline: the file's interval is in the
        // chart's own parameter while ours is the segment index, so a range of
        // [0.000028, 0.002837] addresses the first three thousandths of the
        // first segment — a sliver of a micron standing in for an edge nearly
        // three millimetres long, and the two faces sharing it then disagree
        // about where their boundary runs. The vertices are the statement that
        // can be checked, so they are what decides.
        let mut range = match curve {
            Curve::Trimmed { range, .. } if reaches_ends(curve, *range, p0, p1, tolerance) => {
                *range
            }
            _ if closed_no_vertex => curve.natural_range(),
            // Which of the two arcs between the end points the edge is comes
            // from the curve's own sense character, not from the loop: '-'
            // means the curve runs against the edge, so the edge is the arc
            // walked in decreasing parameter. Passing a constant `true` here
            // took the long way round for half the arcs — on one body that
            // quadrupled the total edge length and folded its faces over
            // themselves.
            _ => {
                let along = self
                    .index
                    .get(&curve_ptr)
                    .map(|ce| geom::geom_sense(self.entities, ce) != '-')
                    .unwrap_or(true);
                recover_edge_range(curve, p0, p1, along, tolerance)
            }
        };

        // Recovery can genuinely fail on an INTERSECTION edge: its chart is
        // the modeller's sparse evaluation of the curve and may not reach the
        // edge's ends at all. The fin's own parameter curve covers exactly
        // this edge, so sample it through the face's surface and use that —
        // range, geometry and end points all come from the samples.
        // The same question as before, asked of the recovered range: a chart
        // that does not reach this edge's ends cannot be trimmed into it, and
        // a range that misses them by twenty millimetres is not an answer.
        // Checking only that the span was non-zero let those through.
        // Where the edge's curve is a chart and the chart runs coarse, the
        // curve it stands for can be computed instead of approximated: the
        // file names the two surfaces it lies on, and the intersection of two
        // surfaces is a walk, not a guess. Only the coarse ones are walked —
        // a chart whose samples already sit inside the tolerance is the same
        // curve and cheaper.
        let walked = self.computed_intersection(curve_ptr, curve, range, p0, p1, tolerance);
        if let Some(fine) = walked {
            // Not cached against the curve entity: the walk runs between
            // *this* edge's ends, and several edges share one intersection.
            // Handing the second of them the first one's walk is how a shared
            // boundary ends up in two places.
            let cid = CurveId(self.solid.curves.len() as u32);
            self.solid.curves.push(fine);
            curve_id = cid;
            range = self.solid.curves[cid.index()].natural_range();
        }
        // When the chart's range cannot be made to reach this edge's ends, the
        // edge may still be describable another way: a blend states its own
        // curves, across it and along it, and both can be built from the ends
        // the edge names. That path was only reached when the curve failed to
        // lower at all, which is a stricter condition than it needs — a curve
        // that lowered to something whose range is unrecoverable is in the
        // same position.
        let curve = &self.solid.curves[curve_id.index()];
        let unrecoverable = !reaches_ends(curve, range, p0, p1, tolerance);
        if unrecoverable && let Some(cid) = self.blend_section(edge_ptr, start, end) {
            curve_id = cid;
            range = self.solid.curves[cid.index()].natural_range();
        }
        let curve = &self.solid.curves[curve_id.index()];
        let usable = reaches_ends(curve, range, p0, p1, tolerance);
        let original_range = range;
        let (curve_id, range, p0, p1, rebuilt) =
            if usable {
                (curve_id, range, p0, p1, false)
            } else if fin_pcurve != 0 {
                let cid = self.intern_tolerant_curve(edge_ptr, fin_pcurve)?;
                let c = &self.solid.curves[cid.index()];
                let natural = c.natural_range();
                // The fin's parameter curve covers this edge, but not only
                // this edge: unless the file wrapped it in a trimmed curve it
                // spans the whole chart, which on a swept surface runs the
                // length of the profile. Taking all of it makes a face out of
                // a sliver and draws it three hundred millimetres across a
                // body two hundred wide. The edge's own end points say where
                // on the chart it starts and stops, so ask them.
                let trimmed = recover_edge_range(c, p0, p1, true, tolerance);
                let range = if trimmed.span().is_finite() && trimmed.span() > 0.0 {
                    trimmed
                } else {
                    natural
                };
                let (a, b) = (c.point_at(range.lo), c.point_at(range.hi));
                // The fin's curve is offered, not imposed. It covers this edge
                // when the chart could not, but on a chart that merely runs
                // coarse it is the worse of the two, and swapping the coarse
                // answer for a wrong one traded eighty-four mis-ranged edges
                // for two hundred open ones. Whichever actually reaches the
                // edge's ends wins; if neither does, the original stands,
                // because it is at least the curve the file named.
                if reaches_ends(c, range, p0, p1, tolerance) {
                    (cid, range, a, b, true)
                } else {
                    (curve_id, original_range, p0, p1, false)
                }
            } else if range.span().is_finite() && range.span() > 0.0 {
                (curve_id, range, p0, p1, false)
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
        //
        // A closed edge is the other case where the stored curve can run
        // against the edge. It names no vertices, so nothing about the loop
        // says which way round it goes — the statement is the curve's own
        // sense character, the same one a surface carries and the same one
        // that already decides which of two arcs an open edge is. It was read
        // in that branch and dropped in this one.
        //
        // What that cost: the flange round of `102.A1525` is a torus band
        // between two circles, and the file gives its two fins `-` and `-`
        // while giving the circles themselves `+` and `-`. Composed, the band
        // walks one ring each way, which is what a face boundary does. Dropped,
        // both rings ran the same way, the tessellator's "which side is
        // material" rule had nothing to work with, and it took the 270° band
        // instead of the 90° one: a 2 mm annular notch the whole way round the
        // flange, three quarters of a torus buried in the solid, and 3060
        // triangles where 1080 were needed. Our STEP reader and OpenCASCADE
        // both had it right, which is how it was found.
        let curve_reversed = closed_no_vertex
            && self
                .index
                .get(&curve_ptr)
                .map(|ce| geom::geom_sense(self.entities, ce) == '-')
                .unwrap_or(false);
        if curve_reversed && std::env::var_os("XT_CLOSED_TRACE").is_some() {
            eprintln!("[closed] edge {edge_ptr}: its curve's own sense is '-', so the edge runs against it");
        }
        let built_reversed = (rebuilt && !forward) ^ curve_reversed;

        // Vertex identity is what makes the mesh watertight: the tessellator
        // pins every edge chain's ends to its vertices, so two edges meeting
        // at a corner produce bit-identical points only if they name the SAME
        // vertex. A rebuilt edge's own end points are sample points a micron
        // or so off the model's vertex, so the model's vertex handle wins
        // wherever the fins supplied one — the sample merely fills the gap
        // when they did not.
        let (sv, ev) = {
            let sv = if fwd_start != 0 {
                let anchor = self.vertex_point(fwd_start).unwrap_or(p0);
                self.intern_vertex(fwd_start, anchor)
            } else {
                self.intern_vertex(0, p0)
            };
            let ev = if closed_no_vertex || (fwd_end != 0 && fwd_end == fwd_start) {
                sv
            } else if fwd_end != 0 {
                let anchor = self.vertex_point(fwd_end).unwrap_or(p1);
                self.intern_vertex(fwd_end, anchor)
            } else if (p1 - p0).length_squared() < 1e-24 {
                sv
            } else {
                self.intern_vertex(0, p1)
            };
            (sv, ev)
        };

        let id = EdgeId(self.solid.edges.len() as u32);
        if std::env::var_os("XT_EDGE_TRACE").is_some() {
            let ce = self.index.get(&curve_ptr);
            eprintln!(
                "[edge] index={} site=A curve={curve_ptr} type={} base_type={}",
                self.solid.edges.len(),
                ce.map(|e| e.type_id).unwrap_or(0),
                ce.filter(|e| e.type_id == xt::TRIMMED_CURVE).and_then(|e| self.index.get(&ptr(self.entities, e, 5))).map(|b| b.type_id).unwrap_or(0),
            );
        }
        if std::env::var_os("XT_EDGE_TRACE").is_some() {
            let c = self.solid.curve(curve_id);
            let (kind, pts) = match c {
                Curve::Polyline { points } => ("polyline", points.len()),
                Curve::Circle { .. } => ("circle", 0),
                Curve::Line { .. } => ("line", 0),
                Curve::Trimmed { .. } => ("trimmed", 0),
                _ => ("other", 0),
            };
            let span = match c {
                Curve::Polyline { points } if points.len() >= 2 => (points[points.len() - 1] - points[0]).length(),
                _ => f64::NAN,
            };
            let (p0, p1) = (self.solid.vertex(sv), self.solid.vertex(ev));
            let ends = match c {
                Curve::Polyline { points } if points.len() >= 2 => format!("{:.5}/{:.5}", (points[0] - p0).length().min((points[0] - p1).length()), (points[points.len()-1] - p1).length().min((points[points.len()-1] - p0).length())),
                _ => String::from("-"),
            };
            let ctype = self.index.get(&curve_ptr).map(|e| e.type_id).unwrap_or(0);
            let raw_ptr = self.index.get(&edge_ptr).map(|ee| ptr(self.entities, ee, 6)).unwrap_or(usize::MAX);
            let (q0, q1) = match c { Curve::Polyline { points } if points.len() >= 2 => (points[0], points[points.len()-1]), _ => (Vec3::ZERO, Vec3::ZERO) };
            eprintln!("[edge] body#{} index={} site=B curve_id={} {kind} {pts} pts span={span:.5} route={route} entity={curve_ptr} type={ctype} raw_edge_curve={raw_ptr} fin_pcurve={fin_pcurve} ends-miss={ends} poly=[{:.4},{:.4},{:.4}]..[{:.4},{:.4},{:.4}] verts=[{:.4},{:.4},{:.4}]..[{:.4},{:.4},{:.4}]", BODY_NO.with(|b| b.get()), self.solid.edges.len(), curve_id.0, q0.x,q0.y,q0.z,q1.x,q1.y,q1.z, p0.x,p0.y,p0.z,p1.x,p1.y,p1.z);
        }
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
    /// Every parameter curve that describes `edge_ptr`, this fin's first.
    ///
    /// An edge is written once per fin, in each adjoining face's own
    /// parameter space. They are the same curve in space, so when one face's
    /// surface cannot be evaluated the edge is still fully described by the
    /// others — reading it from there is the same edge, not an approximation
    /// of it.
    fn pcurves_of(&self, edge_ptr: usize, fin_pcurve: usize) -> Vec<usize> {
        let mut out = Vec::new();
        if fin_pcurve != 0 {
            out.push(fin_pcurve);
        }
        let Some(ee) = self.index.get(&edge_ptr) else {
            return out;
        };
        // EDGE [3] = one of its fins; FIN [5 - a] = the partner across it.
        let mut fin = ptr(self.entities, ee, 3);
        let mut seen = rustc_hash::FxHashSet::default();
        while fin != 0 && seen.insert(fin) {
            let Some(fe) = self.index.get(&fin).filter(|e| e.type_id == xt::FIN) else {
                break;
            };
            let a = usize::from(self.entities.fields(fe).len() < 10);
            let pc = ptr(self.entities, fe, 7 - a);
            if pc != 0 && !out.contains(&pc) {
                out.push(pc);
            }
            fin = ptr(self.entities, fe, 5 - a);
        }
        out
    }

    /// A surface to compute a curve on, built once per entity.
    fn curve_surface(&self, e: &RawEntity) -> Option<cad_ir::brep::Surface> {
        if let Some(hit) = self.blend_surfaces.borrow().get(&e.index) {
            return hit.clone();
        }
        let built = geom::surface_for_curve(self.entities, e, self.index).ok();
        self.blend_surfaces.borrow_mut().insert(e.index, built.clone());
        built
    }

    /// The intersection curve, walked, when the chart standing in for it is
    /// coarser than the body's own tolerance.
    ///
    /// Returns `None` — and the chart stands — when the entity is not an
    /// intersection, when either surface cannot be evaluated, when the chart
    /// is already fine enough to be the curve, or when the walk does not
    /// arrive. Nothing here is a guess: it either computes the curve the file
    /// defines or leaves the file's own approximation alone.
    fn computed_intersection(
        &self,
        curve_ptr: usize,
        curve: &Curve,
        range: Interval,
        p0: Vec3,
        p1: Vec3,
        tolerance: f64,
    ) -> Option<Curve> {
        let ce = self.index.get(&curve_ptr)?;
        let ie = if ce.type_id == xt::TRIMMED_CURVE {
            self.index.get(&ptr(self.entities, ce, 7))?
        } else {
            ce
        };
        if ie.type_id != xt::INTERSECTION {
            { if std::env::var_os("XT_WALK_TRACE").is_some() { eprintln!("[walk] body#{} curve {curve_ptr} refused at guard 1", BODY_NO.with(|b| b.get())); } return None; }
        }
        let probe = std::env::var_os("XT_WALK_PROBE").is_some();
        macro_rules! give_up {
            ($why:expr) => {{
                if probe {
                    eprintln!("[walk] body#{} {}: {}", BODY_NO.with(|b| b.get()), curve_ptr, $why);
                }
                { if std::env::var_os("XT_WALK_TRACE").is_some() { eprintln!("[walk] body#{} curve {curve_ptr} refused at guard 3", BODY_NO.with(|b| b.get())); } return None; }
            }};
        }
        let Some(first) = self.index.get(&ptr(self.entities, ie, 7)) else { give_up!("no first surface") };
        let Some(second) = self.index.get(&self.entities.extra(ie).first().map(|f| f.as_ptr()).unwrap_or(0)) else {
            give_up!("no second surface")
        };
        // A blend boundary among the two is not a transversal meeting at all:
        // the blend touches its mating surface there rather than cutting it,
        // so the normals are parallel and the walk has no direction to follow.
        // Walking the ball's track instead was built and measured — it arrives
        // nowhere on this file and costs eighty per cent of the running time,
        // so the chart stands for these and the walk is not attempted.
        // Building a blend surface is a marching solve, and most of the charts
        // that are merely coarse still reach their edge's ends — the walk would
        // improve them, but the chart is usable and the blend is not worth
        // sixty milliseconds. Where the chart cannot even reach the ends there
        // is no other reading, and then it is worth any price.
        let needs_blend = [first, second]
            .iter()
            .any(|e| e.type_id == xt::BLENDED_EDGE || e.type_id == xt::BLEND_BOUND);
        if needs_blend && reaches_ends(curve, range, p0, p1, tolerance) {
            give_up!("the chart already reaches this edge's ends")
        }
        let (Some(a), Some(b)) = (self.curve_surface(first), self.curve_surface(second)) else {
            give_up!(format!(
                "surfaces {} and {} do not both lower",
                first.type_id, second.type_id
            ))
        };

        // Coarse means: the chart's own steps across this edge are further
        // apart than the tolerance can excuse.
        //
        // This measures our own sampling rather than the file's — on a
        // polyline `point_at` interpolates between the chart's vertices, so
        // sixteen samples are always close together however few vertices there
        // are. Replacing it with the honest question — do the chart's chords
        // lie on the two surfaces the curve is the meeting of, which is
        // arithmetic on an analytic surface — was built and measured: points
        // over 0.2 mm against OpenCASCADE 1789 to 1814, over 0.05 mm 13201 to
        // 13432, and non-manifold edges 11 to 8. Better topology, worse
        // shape, and it did not reach the face it was written for, whose two
        // surfaces include a blend and which is refused below whatever this
        // says. Reverted, and recorded so the next attempt starts here.
        const PROBES: usize = 16;
        let mut widest = 0.0f64;
        let mut previous = curve.point_at(range.lo);
        for i in 1..=PROBES {
            let q = curve.point_at(range.at(i as f64 / PROBES as f64));
            widest = widest.max((q - previous).length());
            previous = q;
        }
        if widest <= tolerance * 20.0 {
            { if std::env::var_os("XT_WALK_TRACE").is_some() { eprintln!("[walk] body#{} curve {curve_ptr} refused at guard 2", BODY_NO.with(|b| b.get())); } return None; }
        }
        let Some(walked) = geom::intersection_polyline(&a, &b, p0, p1, tolerance) else {
            give_up!("the walk did not arrive")
        };

        // Two surfaces can meet along more than one curve, and a walk that
        // sets off along the wrong one still arrives somewhere. The chart is
        // coarse but it is the file's statement of *which* curve this is, and
        // it states how coarse: a walk that strays further from it than that
        // is on the other branch and is refused.
        let slack = {
            let chart = self.index.get(&ptr(self.entities, ie, 8)).filter(|c| c.type_id == xt::CHART);
            let stated = chart.map(|c| f64_at(self.entities, c, 3).abs()).unwrap_or(0.0);
            stated.max(widest) + tolerance
        };
        let near_chart = |q: Vec3| {
            (0..=PROBES)
                .map(|i| (curve.point_at(range.at(i as f64 / PROBES as f64)) - q).length())
                .fold(f64::INFINITY, f64::min)
                <= slack
        };
        let Curve::Polyline { points } = &walked else {
            { if std::env::var_os("XT_WALK_TRACE").is_some() { eprintln!("[walk] refused at guard 4"); } return None; }
        };
        let on_branch = points.iter().all(|q| near_chart(*q));
        if probe && !on_branch {
            eprintln!("[walk] {curve_ptr}: the walk left the chart's branch");
        }
        on_branch.then_some(walked)
    }

    /// Close a loop that lost an edge, so the faces sharing it still meet.
    ///
    /// A loop is a closed walk: every half-edge has to end at the vertex the
    /// next one starts from. When a fin names no edge, or its edge cannot be
    /// read, the walk is left with a hole — and nothing downstream notices,
    /// because every *surviving* edge is still used by exactly two faces. The
    /// tessellator then draws a chord across the hole on one face and the real
    /// boundary on the other, and the two do not meet.
    ///
    /// What can honestly be done is to state the gap and make both sides agree
    /// about it. A straight edge between the two loose vertices is not the
    /// curve the file lost, and it is recorded as a skip so it is never
    /// mistaken for one — but it is interned like any other edge, keyed by the
    /// pair of vertices it joins, so the face on either side is handed the
    /// same `EdgeId`, sampled once, and the two boundaries are then identical
    /// to the bit rather than merely close.
    fn close_loop(&mut self, halves: &mut Vec<HalfEdge>) {
        if halves.len() < 2 {
            return;
        }
        let ends = |solid: &Solid, h: &HalfEdge| {
            let e = solid.edge(h.edge);
            if h.forward {
                (e.start, e.end)
            } else {
                (e.end, e.start)
            }
        };
        // However broken a loop is, it cannot need more bridges than it has
        // half-edges; anything beyond that is this walk failing to converge,
        // and it stops rather than filling memory.
        let limit = halves.len();
        let mut made = 0usize;
        let mut w = 0;
        while w < halves.len() && made < limit {
            let next = (w + 1) % halves.len();
            let (_, from) = ends(&self.solid, &halves[w]);
            let (to, _) = ends(&self.solid, &halves[next]);
            if from == to {
                w += 1;
                continue;
            }
            let (a, b) = (self.solid.vertex(from), self.solid.vertex(to));
            if (a - b).length() <= self.solid.tolerance * 10.0 {
                w += 1;
                continue;
            }
            let Some((edge, forward)) = self.bridge_edge(from, to, a, b) else {
                w += 1;
                continue;
            };
            let bridge = HalfEdge {
                edge,
                forward,
                pcurve: None,
            };
            made += 1;
            if next == 0 {
                // The join that wraps: the bridge belongs after the last
                // half-edge, which is the end of the list, not the front of
                // it. Inserting at the front would shift every index this
                // walk still has to visit.
                halves.push(bridge);
                break;
            }
            halves.insert(next, bridge);
            // Step onto the bridge, not past it: a loop that lost two edges
            // has two gaps, and skipping the join the bridge just made skips
            // the second one with it.
            w += 1;
        }
    }

    /// The straight edge between two vertices, made once and shared.
    fn bridge_edge(
        &mut self,
        from: VertexId,
        to: VertexId,
        a: Vec3,
        b: Vec3,
    ) -> Option<(EdgeId, bool)> {
        // Keyed by the unordered pair, because the two faces sharing the gap
        // meet it from opposite ends and have to be given the same edge — but
        // then one of them traverses it backwards, and saying so is the whole
        // point of a half-edge.
        let key = (from.0.min(to.0) as usize, from.0.max(to.0) as usize);
        if let Some(&(edge, _)) = self.bridges.get(&key) {
            let forward = self.solid.edge(edge).start == from;
            return Some((edge, forward));
        }
        let direction = b - a;
        if direction.length_squared() <= 0.0 {
            return None;
        }
        let curve = CurveId(self.solid.curves.len() as u32);
        self.solid.curves.push(Curve::Line {
            origin: a,
            direction,
        });
        let edge = EdgeId(self.solid.edges.len() as u32);
        self.solid.edges.push(Edge {
            curve,
            start: from,
            end: to,
            range: Interval::new(0.0, 1.0),
            tolerance: self.solid.tolerance,
            same_sense: true,
        });
        self.bridges.insert(key, (edge, true));
        self.skip(
            0,
            format!(
                "a loop was left open by a missing edge; bridged the {:.4} between its ends",
                (b - a).length()
            ),
        );
        Some((edge, true))
    }

    /// Rebuild a rolling-ball blend's cross-section from the edge's own ends.
    ///
    /// An edge across a blend is written only as a parameter curve in the
    /// blend's own space, and where every face meeting it is itself a blend
    /// there is no other description to fall back on — nineteen edges of the
    /// pilot assembly are in exactly that position, and losing them takes a
    /// stretch out of the boundary of every face that used them.
    ///
    /// Nothing has to be reverse-engineered to get them back. The curve is a
    /// cross-section — one whole sweep of the ball, from its contact on the
    /// first mating surface to its contact on the second — so its two ends are
    /// the edge's own two vertices, which the file states outright. The ball
    /// touching a surface puts its centre exactly the blend radius along that
    /// surface's normal, and the section is then the arc of that radius from
    /// one vertex to the other. Which vertex sits on which surface, and which
    /// way the centre lies off it, are not stated, so all four combinations
    /// are tried and one is accepted only if the ball really does reach the
    /// other end.
    fn blend_section(&mut self, edge_ptr: usize, start: usize, end: usize) -> Option<CurveId> {
        let probe = std::env::var_os("XT_SECTION_PROBE").is_some();
        macro_rules! bail {
            ($why:expr) => {{
                if probe {
                    eprintln!("[section] edge {edge_ptr}: {}", $why);
                }
                return None;
            }};
        }
        let ee = self.index.get(&edge_ptr)?;
        let Some((be, across, far_side)) = std::iter::once(ptr(self.entities, ee, 6))
            .chain(self.pcurves_of(edge_ptr, 0))
            .filter(|p| *p != 0)
            .find_map(|p| geom::blend_parameter_curve(self.entities, self.index.get(&p)?, self.index))
        else {
            bail!("no description is a blend parameter curve")
        };
        if chr(self.entities, be, 7) != 'R' {
            bail!(format!("blend_type {:?}", chr(self.entities, be, 7)))
        }
        let radius = f64_at(self.entities, be, 11).abs();
        if !(radius.is_finite() && radius > 0.0) {
            return None;
        }
        let mate = |i: usize| -> Option<cad_ir::brep::Surface> {
            let e = self.index.get(&ptr(self.entities, be, i))?;
            match geom::surface(self.entities, e, self.index) {
                Ok(s) => Some(s),
                Err(_) => {
                    if probe {
                        eprintln!("[section] edge {edge_ptr}: mate type {}", e.type_id);
                    }
                    None
                }
            }
        };
        let (a, b) = (mate(8)?, mate(9)?);
        let (Some(p0), Some(p1)) = (self.vertex_point(start), self.vertex_point(end)) else {
            bail!("the edge has no vertices")
        };
        // A hundredth of the radius is tighter than any writer's rounding and
        // still refuses a pairing that is simply wrong — but not below what
        // the file itself claims to be accurate to. On a one-millimetre blend
        // that floor is the difference between reading a section and refusing
        // it over thirteen microns, on a body whose stated tolerance is ten.
        let tolerance = (radius * 0.01).max(self.solid.tolerance);

        // A curve running *along* the blend is the ball's contact track on one
        // of the mating surfaces, not a section of it, so there is no arc to
        // build: it has to be walked. Which surface is stated by which end of
        // the blend the curve sits at.
        if !across {
            // Which end of the blend the curve sits at says which surface the
            // ball touches, but the two are only distinguishable when the
            // parameterisation is read the same way round as the file wrote
            // it — so it is a preference, not a fact, and the other surface is
            // tried after it. So is the direction the centre lies off the
            // surface, which the file does not state at all.
            let first = if far_side { (&b, &a) } else { (&a, &b) };
            let second = if far_side { (&a, &b) } else { (&b, &a) };
            for (near, far) in [first, second] {
                for sign in [1.0f64, -1.0] {
                    // Where a blend runs out, its track stops short of the
                    // vertex the edge names — by up to a radius and a half on
                    // this file. The chain's ends are pinned to the vertices
                    // downstream whatever happens here, so the walk is allowed
                    // to begin at the nearest point of the track; what it may
                    // not do is begin somewhere else entirely, which is what
                    // the bound is for.
                    if let Some(track) = geom::blend_rail_from(
                        near,
                        far,
                        radius,
                        sign,
                        p0,
                        p1,
                        tolerance,
                        radius * 2.0,
                    ) {
                        let id = CurveId(self.solid.curves.len() as u32);
                        self.solid.curves.push(track);
                        return Some(id);
                    }
                }
            }
            bail!("the ball's track could not be walked on either surface")
        }

        match blend_arc(&a, &b, p0, p1, radius, tolerance) {
            Some((frame, radius)) => {
                // Stored as the circle it is, so everything downstream — the
                // sag criterion, the range recovery, the inversion — treats it
                // like any other arc.
                let id = CurveId(self.solid.curves.len() as u32);
                self.solid.curves.push(Curve::Circle { frame, radius });
                Some(id)
            }
            None => {
                if probe {
                    eprintln!("[section] edge {edge_ptr}: no ball of r={radius:.5} reaches both ends");
                }
                None
            }
        }
    }

    fn intern_tolerant_curve(
        &mut self,
        edge_ptr: usize,
        fin_pcurve: usize,
    ) -> Result<CurveId, String> {
        let Some(pe) = self.index.get(&fin_pcurve) else {
            return Err(format!("edge {edge_ptr}: fin pcurve {fin_pcurve} does not exist"));
        };
        if std::env::var_os("XT_EDGE_TRACE").is_some() {
            eprintln!("[tolerant] edge {edge_ptr} pcurve {fin_pcurve} type {}", pe.type_id);
        }
        let curve = geom::sp_curve_polyline(self.entities, pe, self.index)?;
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
        let surface = geom::surface(self.entities, se, self.index)?;
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
        let mut curve = geom::curve(self.entities, ce, self.index)?;
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

/// Pick the four indices of a closed polyline that read as a quad's corners.
///
/// A blend band is a quadrilateral however many edges the file split its sides
/// into, so its corners are where the boundary turns sharpest — not where one
/// edge record ends and the next begins. Corners are chosen greedily by
/// turning angle with a minimum separation, so four points on one tight fillet
/// end cannot all win; whatever the angles fail to supply is filled in evenly,
/// which is the right answer for a boundary with no corners at all (a closed
/// band) and keeps the result defined for every input.
fn quad_corners(ring: &[Vec3]) -> [usize; 4] {
    let n = ring.len();
    let min_gap = (n / 8).max(1);

    let mut scored: Vec<(f64, usize)> = (0..n)
        .map(|i| {
            let prev = ring[(i + n - 1) % n];
            let here = ring[i];
            let next = ring[(i + 1) % n];
            let a = (here - prev).try_normalized();
            let b = (next - here).try_normalized();
            let turn = match (a, b) {
                (Some(a), Some(b)) => a.dot(b).clamp(-1.0, 1.0).acos(),
                _ => 0.0,
            };
            (turn, i)
        })
        .collect();
    scored.sort_by(|x, y| y.0.partial_cmp(&x.0).unwrap_or(std::cmp::Ordering::Equal));

    let mut chosen: Vec<usize> = Vec::with_capacity(4);
    for (turn, i) in scored {
        if chosen.len() == 4 {
            break;
        }
        // A corner worth the name turns by more than a few degrees; anything
        // smoother is just the discretisation of a curved side.
        if turn < 0.15 {
            break;
        }
        let far_enough = chosen.iter().all(|&c| {
            let d = if c > i { c - i } else { i - c };
            d.min(n - d) >= min_gap
        });
        if far_enough {
            chosen.push(i);
        }
    }

    // Fill any shortfall with evenly spaced indices, keeping the separation.
    let mut probe = 0usize;
    while chosen.len() < 4 && probe < n {
        let candidate = probe;
        let far_enough = chosen.iter().all(|&c| {
            let d = if c > candidate { c - candidate } else { candidate - c };
            d.min(n - d) >= min_gap
        });
        if far_enough {
            chosen.push(candidate);
        }
        probe += (n / 4).max(1);
    }
    while chosen.len() < 4 {
        chosen.push(chosen.len() * n / 4);
    }

    chosen.sort_unstable();
    [chosen[0], chosen[1], chosen[2], chosen[3]]
}

/// Resample a closed polyline to `n + 1` points evenly by arc length, the last
/// repeating the first so the band closes on itself.
fn resample_closed(ring: &[Vec3], n: usize) -> Vec<Vec3> {
    let mut closed: Vec<Vec3> = ring.to_vec();
    if closed
        .first()
        .zip(closed.last())
        .is_some_and(|(f, l)| (*f - *l).length_squared() > 1e-24)
    {
        closed.push(closed[0]);
    }
    resample(&closed, n)
}

/// The diagonal of a polyline's bounding box.
fn polyline_extent(points: &[Vec3]) -> f64 {
    let mut b = cad_ir::math::Aabb::EMPTY;
    for p in points {
        b.add_point(*p);
    }
    b.diagonal()
}

/// How far `p` lies from a polyline, measured to its segments.
fn distance_to_polyline(p: Vec3, points: &[Vec3]) -> f64 {
    let mut best = f64::INFINITY;
    for w in points.windows(2) {
        let d = w[1] - w[0];
        let len2 = d.length_squared();
        let t = if len2 > 0.0 {
            ((p - w[0]).dot(d) / len2).clamp(0.0, 1.0)
        } else {
            0.0
        };
        best = best.min((p - (w[0] + d * t)).length());
    }
    best
}

/// Does walking `range` on `curve` start and finish at the edge's own ends?
///
/// The only claim a parameter range makes that can be checked against
/// something else is where it begins and ends. An edge states that separately,
/// as its two vertices, so the two can be compared — and a range that does not
/// reach them is not this edge's range, whatever the file attached it to.
fn reaches_ends(curve: &Curve, range: Interval, p0: Vec3, p1: Vec3, tolerance: f64) -> bool {
    if !range.span().is_finite() || range.span() == 0.0 {
        return false;
    }
    let (a, b) = (curve.point_at(range.lo), curve.point_at(range.hi));
    let reach = (tolerance * 10.0).max(1e-9);
    let meets = |x: Vec3, y: Vec3| (x - y).length() <= reach;
    (meets(a, p0) && meets(b, p1)) || (meets(a, p1) && meets(b, p0))
}

/// The circle a rolling ball's cross-section runs on, from `p0` to `p1`.
///
/// The ball touching a surface puts its centre exactly `radius` along that
/// surface's normal, so one contact point fixes the centre — up to which of
/// the two mating surfaces the point belongs to and which way the normal
/// points. All four are tried and one is accepted only if the ball reaches the
/// other contact as well, which is what makes this a reading of the file's own
/// description rather than a guess at it.
///
/// Returns the frame of the circle: centred on the ball, its reference
/// direction toward `p0` and its axis such that the sweep to `p1` is positive.
fn blend_arc(
    a: &cad_ir::brep::Surface,
    b: &cad_ir::brep::Surface,
    p0: Vec3,
    p1: Vec3,
    radius: f64,
    tolerance: f64,
) -> Option<(cad_ir::math::Frame, f64)> {
    for (near, far) in [(a, b), (b, a)] {
        for (from, to) in [(p0, p1), (p1, p0)] {
            let Some(uv) = near.invert(from, None) else {
                continue;
            };
            if (near.point_at(uv) - from).length() > tolerance {
                continue;
            }
            if far
                .invert(to, None)
                .is_none_or(|w| (far.point_at(w) - to).length() > tolerance)
            {
                continue;
            }
            for sign in [1.0f64, -1.0] {
                let normal = near.normal_at(uv) * sign;
                // The ball is not free: it touches `near` at `from`, so its
                // centre is on that normal, and it must also pass through
                // `to`. Those two facts fix the radius outright —
                // |d + n·r| = r with d = from − to gives r = −|d|²/(2 d·n) —
                // so rather than test the stated radius and refuse when it
                // misses by a hair, solve for the ball that actually fits and
                // check the file's number against it. On one edge here the
                // two differ by thirteen microns on a millimetre fillet, which
                // is the file's own rounding and not a different ball.
                let d = from - to;
                let denominator = 2.0 * d.dot(normal);
                let fitted = (denominator.abs() > 0.0)
                    .then(|| -d.length_squared() / denominator)
                    .filter(|r| r.is_finite() && *r > 0.0);
                // The file's own radius first — it is what the blend is, and
                // where it works there is nothing to solve for. Only where it
                // misses does the geometry get to say what ball it actually
                // is, and then only if the two agree to within a twentieth.
                let radius = if ((to - (from + normal * radius)).length() - radius).abs()
                    <= tolerance
                {
                    radius
                } else {
                    match fitted.filter(|r| (*r - radius).abs() <= radius * 0.05) {
                        Some(r) => r,
                        None => continue,
                    }
                };
                let centre = from + normal * radius;
                if ((to - centre).length() - radius).abs() > tolerance {
                    continue;
                }
                let (u, w) = (from - centre, to - centre);
                let Some(eu) = u.try_normalized() else { continue };
                let Some(perp) = (w - eu * w.dot(eu)).try_normalized() else {
                    continue;
                };
                if !w.dot(perp).atan2(w.dot(eu)).is_finite() {
                    continue;
                }
                // The arc has to start at `p0` whichever end was used to find
                // the centre, so the frame is always built from `p0`.
                let e0 = (p0 - centre).try_normalized()?;
                let e1 = (p1 - centre).try_normalized()?;
                let axis = e0.cross(e1).try_normalized()?;
                return Some((cad_ir::math::Frame::new(centre, axis, e0), radius));
            }
        }
    }
    None
}

/// Swap a grid's two directions, so a patch built across can be stored along.
fn transpose(g: Vec<Vec<Vec3>>) -> Vec<Vec<Vec3>> {
    let (rows, cols) = (g.len(), g.first().map(|r| r.len()).unwrap_or(0));
    (0..cols)
        .map(|j| (0..rows).map(|i| g[i][j]).collect())
        .collect()
}

/// Cut a closed polyline into the four chains between its corners.
fn split_at_corners(ring: &[Vec3], corners: [usize; 4]) -> [Vec<Vec3>; 4] {
    let n = ring.len();
    let mut sides: [Vec<Vec3>; 4] = Default::default();
    for k in 0..4 {
        let from = corners[k];
        let to = corners[(k + 1) % 4];
        let mut i = from;
        loop {
            sides[k].push(ring[i]);
            if i == to {
                break;
            }
            i = (i + 1) % n;
        }
        // A side between coincident corners collapses; give it two points so
        // the resampler has a segment to walk.
        if sides[k].len() < 2 {
            sides[k].push(ring[to]);
        }
    }
    sides
}

/// Resample a polyline to `n + 1` points spaced evenly by arc length.
///
/// Even spacing is what keeps the Coons grid from bunching where the file
/// happened to place its samples, which would show as a band of stretched
/// triangles down the middle of every fillet.
fn resample(points: &[Vec3], n: usize) -> Vec<Vec3> {
    if points.len() < 2 {
        return vec![points.first().copied().unwrap_or(Vec3::ZERO); n + 1];
    }
    let mut cumulative = Vec::with_capacity(points.len());
    let mut total = 0.0;
    cumulative.push(0.0);
    for w in points.windows(2) {
        total += (w[1] - w[0]).length();
        cumulative.push(total);
    }
    if total <= 0.0 {
        return vec![points[0]; n + 1];
    }

    let mut out = Vec::with_capacity(n + 1);
    let mut seg = 0usize;
    for k in 0..=n {
        let target = total * k as f64 / n as f64;
        while seg + 2 < points.len() && cumulative[seg + 1] < target {
            seg += 1;
        }
        let span = cumulative[seg + 1] - cumulative[seg];
        let t = if span > 0.0 {
            ((target - cumulative[seg]) / span).clamp(0.0, 1.0)
        } else {
            0.0
        };
        out.push(points[seg].lerp(points[seg + 1], t));
    }
    out
}

/// The default modelling tolerance when a body does not state one.
pub const DEFAULT_TOLERANCE: f64 = 1e-5;

/// A rough interval sanity bound, re-exported for tests.
pub fn finite(i: Interval) -> bool {
    i.span().is_finite() && i.span() > 0.0
}

#[cfg(test)]
mod tests {

    /// The blend surface built from the ball that makes it, on a fillet whose
    /// answer is known: a quarter-round of radius 2 in the crease between two
    /// planes is a quarter cylinder of radius 2, its axis along the crease.
    #[test]
    fn a_rolling_ball_blend_is_the_quarter_cylinder_it_should_be() {
        let floor = Surface::Plane {
            frame: Frame::new(Vec3::ZERO, Vec3::Z, Vec3::X),
        };
        let wall = Surface::Plane {
            frame: Frame::new(Vec3::ZERO, Vec3::X, Vec3::Y),
        };
        let radius = 2.0;
        // The ball's centre runs along (2, y, 2); its track on the floor is
        // x = 2, which is what the spine is lifted from.
        let (from, to) = (Vec3::new(2.0, -6.0, 0.0), Vec3::new(2.0, 6.0, 0.0));
        let track = geom::blend_rail_polyline(&floor, &wall, radius, 1.0, from, to, 1e-6)
            .expect("the ball leaves a track");
        let Curve::Polyline { points } = &track else {
            panic!("expected a walked polyline");
        };

        // Lift it, and check every ball centre is a radius from both planes —
        // which is the whole definition of the spine.
        for p in points {
            let centre = Vec3::new(p.x, p.y, p.z + radius);
            assert!((centre.z - radius).abs() < 1e-6, "{centre:?} is off the floor");
            assert!((centre.x - radius).abs() < 1e-6, "{centre:?} is off the wall");
        }
    }

    /// A rolling ball in the crease between two planes leaves a straight track
    /// on each of them. Walking that track has to reproduce it — and, unlike a
    /// cross-section, it cannot be built from the two ends because the curve
    /// between them is whatever the surfaces make it.
    #[test]
    fn a_contact_track_is_walked_along_the_surface_it_touches() {
        let floor = Surface::Plane {
            frame: Frame::new(Vec3::ZERO, Vec3::Z, Vec3::X),
        };
        let wall = Surface::Plane {
            frame: Frame::new(Vec3::ZERO, Vec3::X, Vec3::Y),
        };
        let radius = 2.0;
        // The ball of radius 2 in the crease touches the floor along x = 2.
        let (from, to) = (Vec3::new(2.0, -8.0, 0.0), Vec3::new(2.0, 8.0, 0.0));

        let track = geom::blend_rail_polyline(&floor, &wall, radius, 1.0, from, to, 1e-6)
            .expect("the ball leaves a track");
        let Curve::Polyline { points } = &track else {
            panic!("expected a walked polyline");
        };
        assert!(points.len() > 4, "only {} points", points.len());
        for q in points {
            assert!((q.x - 2.0).abs() < 1e-3, "{q:?} left the contact line");
            assert!(q.z.abs() < 1e-3, "{q:?} left the floor");
        }
        assert!((points[0] - from).length() < 1e-9);
        assert!((points[points.len() - 1] - to).length() < 1e-9);
    }

    /// And a radius no ball in that crease has leaves no track to walk.
    #[test]
    fn a_track_that_does_not_exist_is_refused() {
        let floor = Surface::Plane {
            frame: Frame::new(Vec3::ZERO, Vec3::Z, Vec3::X),
        };
        let wall = Surface::Plane {
            frame: Frame::new(Vec3::new(100.0, 0.0, 0.0), Vec3::X, Vec3::Y),
        };
        // A ball of radius 2 touching the floor at x = 2 is 98 from that wall.
        let (from, to) = (Vec3::new(2.0, -8.0, 0.0), Vec3::new(2.0, 8.0, 0.0));
        assert!(geom::blend_rail_polyline(&floor, &wall, 2.0, 1.0, from, to, 1e-6).is_none());
    }
    use super::*;
    use cad_ir::brep::{Curve, Surface};
    use cad_ir::math::Frame;

    /// A cylinder cut by a plane meets it in a circle. Walking that
    /// intersection has to produce the circle — not the chord between its
    /// ends, which is what the file's own chart would give across a span this
    /// wide.
    #[test]
    fn an_intersection_is_walked_not_guessed() {
        let cylinder = Surface::Cylinder {
            frame: Frame::new(Vec3::ZERO, Vec3::Z, Vec3::X),
            radius: 10.0,
        };
        let lid = Surface::Plane {
            frame: Frame::new(Vec3::new(0.0, 0.0, 3.0), Vec3::Z, Vec3::X),
        };
        let (from, to) = (Vec3::new(10.0, 0.0, 3.0), Vec3::new(-10.0, 0.0, 3.0));

        let walked = geom::intersection_polyline(&cylinder, &lid, from, to, 1e-4)
            .expect("the two surfaces meet");
        let Curve::Polyline { points } = &walked else {
            panic!("expected a walked polyline");
        };

        assert!(points.len() > 8, "only {} points for half a circle", points.len());
        assert!((points[0] - from).length() < 1e-9);
        assert!((points[points.len() - 1] - to).length() < 1e-9);
        for q in points {
            assert!(
                ((q.x * q.x + q.y * q.y).sqrt() - 10.0).abs() < 1e-3,
                "{q:?} left the cylinder"
            );
            assert!((q.z - 3.0).abs() < 1e-3, "{q:?} left the plane");
        }
        // And it is a half turn, not a chord: the middle has to bulge out to
        // the cylinder rather than cut across it.
        let mid = points[points.len() / 2];
        assert!(
            mid.y.abs() > 5.0,
            "the walk cut across instead of going round: {mid:?}"
        );
    }

    /// A quarter-round fillet of radius 2 in the crease between the plane
    /// z = 0 and the plane x = 0: the ball sits at (2, y, 2) and touches at
    /// (2, y, 0) and (0, y, 2). Given those two contacts and the radius, the
    /// arc between them has to come back exactly.
    #[test]
    fn a_cross_section_is_rebuilt_from_its_two_contacts() {
        let floor = Surface::Plane {
            frame: Frame::new(Vec3::ZERO, Vec3::Z, Vec3::X),
        };
        let wall = Surface::Plane {
            frame: Frame::new(Vec3::ZERO, Vec3::X, Vec3::Y),
        };
        let radius = 2.0;
        let (p0, p1) = (Vec3::new(2.0, 5.0, 0.0), Vec3::new(0.0, 5.0, 2.0));

        let (frame, fitted) = blend_arc(&floor, &wall, p0, p1, radius, 1e-9).expect("an arc");
        assert!((fitted - radius).abs() < 1e-9, "the radius came out {fitted}");
        assert!(
            (frame.origin - Vec3::new(2.0, 5.0, 2.0)).length() < 1e-9,
            "centre came out at {:?}",
            frame.origin
        );

        // Walking the circle from p0 must reach p1, and stay on the fillet.
        let circle = cad_ir::brep::Curve::Circle { frame, radius };
        assert!((circle.point_at(0.0) - p0).length() < 1e-9);
        let quarter = std::f64::consts::FRAC_PI_2;
        assert!(
            (circle.point_at(quarter) - p1).length() < 1e-9,
            "the quarter turn landed at {:?}",
            circle.point_at(quarter)
        );
        let mid = circle.point_at(quarter * 0.5);
        assert!(
            (mid - frame.origin).length() > radius - 1e-9,
            "the middle of the arc left the ball"
        );
        assert!(mid.x < 2.0 && mid.z < 2.0, "the arc bulged the wrong way: {mid:?}");
    }

    /// Two points that no ball of the stated radius can touch at once are not
    /// a cross-section, and must be refused rather than bent into one.
    #[test]
    fn a_pair_no_ball_reaches_is_refused() {
        let floor = Surface::Plane {
            frame: Frame::new(Vec3::ZERO, Vec3::Z, Vec3::X),
        };
        let wall = Surface::Plane {
            frame: Frame::new(Vec3::ZERO, Vec3::X, Vec3::Y),
        };
        // The contacts a radius-2 fillet would have, asked for with radius 5.
        let (p0, p1) = (Vec3::new(2.0, 5.0, 0.0), Vec3::new(0.0, 5.0, 2.0));
        assert!(blend_arc(&floor, &wall, p0, p1, 5.0, 1e-6).is_none());
    }
}

thread_local! {
    /// The worst miss along the rail of the attempt in progress, as a multiple
    /// of the tolerance it had to meet — every point has to pass, so this is
    /// what decides the attempt.
    static ATTEMPT_WORST: std::cell::Cell<f64> = const { std::cell::Cell::new(0.0) };
    /// The best any attempt managed for the face being lowered. Probes only.
    static BEST_MISS: std::cell::Cell<f64> = const { std::cell::Cell::new(f64::INFINITY) };
    /// Whether some ball did sweep a grid for this face and it was refused for
    /// not carrying the face's own boundary.
    static REFUSED_HERE: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
    /// Whether the face being lowered took its surface from arc sections.
    static ARCED_HERE: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
    /// Which body is being lowered, for probes that print before the body has
    /// a name.
    static BODY_NO: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
    /// One tight-track refusal is taken apart, not hundreds.
    static DUMPED_TIGHT: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
    /// One face is dumped in full, not eleven thousand.
    static DUMPED_ONE: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
}

/// Does every point of a face's boundary sit on this grid?
///
/// The grid is a mesh of flat cells — that is what a degree-one surface is —
/// so the question is answered exactly by the distance to the nearest of its
/// triangles. A rolling ball can satisfy its two contacts along a rail and
/// still produce a sheet the face's other edges are nowhere near; this is the
/// test that says so.
fn grid_holds(grid: &[Vec<Vec3>], ring: &[Vec3], tolerance: f64) -> bool {
    if grid.len() < 2 || grid[0].len() < 2 {
        return false;
    }
    let limit = tolerance * tolerance;
    // A box round the sheet first. The test is asked inside the search now —
    // sixteen times a face rather than once — and most of what it refuses is a
    // sheet somewhere else entirely, which the box answers in a few
    // comparisons instead of walking every cell.
    let mut box_of = cad_ir::math::Aabb::EMPTY;
    for row in grid {
        for q in row {
            box_of.add_point(*q);
        }
    }
    let outside_by = |p: Vec3| {
        let (lo, hi) = (box_of.min, box_of.max);
        let d = |x: f64, a: f64, b: f64| (a - x).max(x - b).max(0.0);
        Vec3::new(d(p.x, lo.x, hi.x), d(p.y, lo.y, hi.y), d(p.z, lo.z, hi.z)).length_squared()
    };
    ring.iter().all(|p| {
        if outside_by(*p) > limit {
            return false;
        }
        let mut best = f64::INFINITY;
        for w in grid.windows(2) {
            for j in 0..w[0].len().min(w[1].len()) - 1 {
                for tri in [
                    [w[0][j], w[0][j + 1], w[1][j]],
                    [w[1][j], w[0][j + 1], w[1][j + 1]],
                ] {
                    best = best.min(distance_to_triangle(*p, tri));
                    if best <= limit {
                        return true;
                    }
                }
            }
        }
        best <= limit
    })
}


/// Distance from a point to the grid, for reporting how a refusal missed.
fn point_to_grid(grid: &[Vec<Vec3>], p: Vec3) -> f64 {
    let mut best = f64::INFINITY;
    for w in grid.windows(2) {
        for j in 0..w[0].len().min(w[1].len()) - 1 {
            for tri in [
                [w[0][j], w[0][j + 1], w[1][j]],
                [w[1][j], w[0][j + 1], w[1][j + 1]],
            ] {
                best = best.min(distance_to_triangle(p, tri));
            }
        }
    }
    best.sqrt()
}

/// Squared distance from a point to a triangle, by projecting and clamping.
fn distance_to_triangle(p: Vec3, t: [Vec3; 3]) -> f64 {
    let (e0, e1, d) = (t[1] - t[0], t[2] - t[0], t[0] - p);
    let (a, b, c) = (e0.dot(e0), e0.dot(e1), e1.dot(e1));
    let (dd, e) = (e0.dot(d), e1.dot(d));
    let det = (a * c - b * b).max(1e-300);
    let mut u = (b * e - c * dd) / det;
    let mut v = (b * dd - a * e) / det;
    if u + v > 1.0 {
        let over = (u + v - 1.0) * 0.5;
        u -= over;
        v -= over;
    }
    u = u.clamp(0.0, 1.0);
    v = v.clamp(0.0, 1.0 - u);
    (t[0] + e0 * u + e1 * v - p).length_squared()
}

/// Say where a refused grid missed the face, by the patch's own four sides.
///
/// A rail adrift and an end adrift are different faults and want different
/// fixes, and one number for the whole ring cannot tell them apart.
fn report_refusal(grid: &[Vec<Vec3>], ring: &[Vec3], corners: [usize; 4], limit: f64) {
    let d: Vec<f64> = ring.iter().map(|p| point_to_grid(grid, *p)).collect();
    let off = d.iter().filter(|x| **x > limit).count();
    let worst = d.iter().fold(0.0f64, |m, x| m.max(*x));
    let mut per_side = [(0usize, 0usize); 4];
    for (i, dist) in d.iter().enumerate() {
        let side = (0..4)
            .find(|k| {
                let (a, b) = (corners[*k], corners[(*k + 1) % 4]);
                if a <= b { i >= a && i < b } else { i >= a || i < b }
            })
            .unwrap_or(0);
        per_side[side].1 += 1;
        if *dist > limit {
            per_side[side].0 += 1;
        }
    }
    let shape: String = per_side
        .iter()
        .map(|(bad, all)| {
            if *all == 0 {
                '-'
            } else if *bad * 4 >= *all * 3 {
                '#'
            } else if *bad * 4 >= *all {
                '+'
            } else if *bad > 0 {
                '.'
            } else {
                ' '
            }
        })
        .collect();
    eprintln!(
        "[blend] a rolled grid missed the boundary at {off} of {} points, worst {worst:.6}, sides [{shape}]",
        ring.len()
    );
}

/// A one-word name for a surface, for the probes.
fn surface_name(s: &cad_ir::brep::Surface) -> &'static str {
    use cad_ir::brep::Surface::*;
    match s {
        Plane { .. } => "plane",
        Cylinder { .. } => "cylinder",
        Cone { .. } => "cone",
        Sphere { .. } => "sphere",
        Torus { .. } => "torus",
        Nurbs(_) => "spline",
        LinearExtrusion { .. } => "extrusion",
        Revolution { .. } => "revolution",
        Offset { .. } => "offset",
        RectangularTrimmed { .. } => "trimmed",
    }
}
