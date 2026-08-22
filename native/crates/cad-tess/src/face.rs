//! Triangulating one trimmed face.
//!
//! The face's trim loops are mapped into the surface's parameter space, the
//! region they enclose is filled by a constrained Delaunay triangulation, and
//! every resulting parameter point is lifted back to 3D. Three things make this
//! harder than it sounds, and all three occur in the pilot file:
//!
//! **No pcurves.** The exporter writes only 3D curves, so every boundary point
//! has to be inverted onto the surface. Done point-by-point that is ambiguous
//! on a periodic surface — a point at u = 0 is equally at u = 2π — so the chain
//! is unwrapped as it is built, each point placed in the branch nearest its
//! predecessor.
//!
//! **No `FACE_OUTER_BOUND`.** Every loop arrives as a plain `FACE_BOUND`, so
//! which one is the outer boundary and which are holes has to be decided
//! geometrically, by parameter-space area.
//!
//! **No seam edges.** A cylinder's lateral face is bounded by its two circles
//! and nothing else, so its loops do not close in parameter space — they run
//! from `u₀` to `u₀ + 2π`. Such a face is filled as the strip between its
//! wrapping loops, with the seam supplied here rather than by the file.

use crate::knots::{self, Axis};
use crate::edge::Chain;
use crate::options::Resolved;
use cad_ir::brep::{Bound, FaceId, Solid, Surface};
use cad_ir::eval::surface::Domain;
use cad_ir::math::{Vec2, Vec3};
use spade::{ConstrainedDelaunayTriangulation, Point2, Triangulation};

/// A triangulated face, in its own vertex numbering.
#[derive(Debug, Default, Clone)]
pub struct Patch {
    pub positions: Vec<[f32; 3]>,
    pub normals: Vec<[f32; 3]>,
    pub indices: Vec<u32>,
    /// Boundary segments the triangulation could not enforce because they
    /// crossed one already in it. Non-zero means this face's parameter image
    /// folds over itself, and the region fill can leak through the gap.
    pub crossings: usize,
    /// How much of the file's own boundary this patch failed to draw, and how
    /// much it tore open inside itself — the number the face's own readings
    /// are compared on.
    pub undrawn: usize,
    /// True when this patch was rebuilt from the face's boundary rather than
    /// evaluated from its surface. The surface is then not what the patch
    /// stands for, and measuring the patch against it answers a question
    /// nobody asked: the rebuild is offered precisely because the surface path
    /// could not draw the face.
    pub rebuilt: bool,
    /// How each unenforced segment came to be so: two boundary points landing
    /// on one triangulation vertex, a segment crossing another, or a split
    /// that had to invent a point. They want different fixes, so they are
    /// counted apart.
    pub merged: usize,
    pub crossed: usize,
    pub invented: usize,
    /// How close the boundary comes to itself, in model units, measured
    /// between parts of it far enough apart not to be neighbours.
    ///
    /// This is the face's own width where it is narrow — a fillet run out to
    /// nothing, a knife edge between two nearly tangent surfaces — and it is
    /// the length its boundary has to be sampled at to stay a simple curve.
    /// Zero when the boundary never approaches itself.
    pub narrowest: f64,
}

/// A trim loop expressed in parameter space, alongside its exact 3D points.
#[derive(Debug, Clone)]
struct UvLoop {
    uv: Vec<Vec2>,
    /// The cached 3D point for each parameter point. Boundary vertices keep
    /// these rather than being re-evaluated, which is what keeps shared edges
    /// crack-free.
    xyz: Vec<Vec3>,
    /// Net parameter travel in u, in periods. Zero for a loop that closes.
    wrap: i32,
    /// Which way the file had this loop travelling in u, kept from before the
    /// strip builder turned every ring the same way. On a surface periodic in
    /// v the two rings bounding a band are the only statement of which side of
    /// the band is material, and normalising their direction erases it.
    travel: i32,
    /// Signed area in parameter space; positive is counter-clockwise.
    area: f64,
}

/// Triangulate one face.
pub fn tessellate(
    solid: &Solid,
    fid: FaceId,
    edges: &[Chain],
    options: &Resolved,
) -> Result<Patch, String> {
    CURRENT_FACE.with(|c| c.set(fid.0));
    if std::env::var("CAD_TESS_FACE").ok().and_then(|v| v.parse::<u32>().ok()) == Some(fid.0) {
        let face = solid.face(fid);
        let surface = solid.surface(face.surface);
        println!("[face-dump] face={} {} same_sense={}", fid.0, surface_kind(surface), face.same_sense);
        println!("            surface = {surface:?}");
        for (i, b) in face.bounds.iter().enumerate() {
            let mut box_of = cad_ir::math::Aabb::EMPTY;
            let mut n = 0usize;
            for h in &b.halves {
                let e = solid.edge(h.edge);
                let c = solid.curve(e.curve);
                for k in 0..=8 {
                    box_of.add_point(c.point_at(e.range.lo + e.range.span() * k as f64 / 8.0));
                }
                n += 1;
            }
            for h in &b.halves {
                let e = solid.edge(h.edge);
                let c = solid.curve(e.curve);
                let kind = match c {
                    cad_ir::brep::Curve::Line { .. } => "line",
                    cad_ir::brep::Curve::Circle { .. } => "circle",
                    cad_ir::brep::Curve::Ellipse { .. } => "ellipse",
                    cad_ir::brep::Curve::Polyline { points } => {
                        println!("              edge {:?} polyline of {} points, range [{:.5},{:.5}], forward={}",
                            h.edge, points.len(), e.range.lo, e.range.hi, h.forward);
                        for (k, q) in points.iter().enumerate() {
                            let off = surface
                                .invert(*q, None)
                                .map(|uv| (surface.point_at(uv) - *q).length())
                                .unwrap_or(f64::NAN);
                            println!(
                                "                  vertex {k} [{:.4},{:.4},{:.4}]  off this face's surface by {off:.4}",
                                q.x, q.y, q.z
                            );
                        }
                        continue;
                    }
                    cad_ir::brep::Curve::Nurbs(_) => "nurbs",
                    cad_ir::brep::Curve::Trimmed { .. } => "trimmed",
                    _ => "other",
                };
                println!(
                    "              edge {:?} {kind}, range [{:.5},{:.5}], forward={}",
                    h.edge, e.range.lo, e.range.hi, h.forward
                );
            }
            println!(
                "            bound {i} outer={} {n} edges, box {:.3}x{:.3}x{:.3} at [{:.3},{:.3},{:.3}]",
                b.outer,
                box_of.max.x - box_of.min.x,
                box_of.max.y - box_of.min.y,
                box_of.max.z - box_of.min.z,
                box_of.min.x, box_of.min.y, box_of.min.z
            );
        }
    }
    let mut chosen = choose_reading(solid, fid, edges, options);
    // Whatever built it, a patch does not lay the same facet twice. It happens
    // on a boundary rebuild where the ring closes onto a point: the fan there
    // narrows to nothing and the last triangles come out from both sides, the
    // second copy wound the other way. The two together cover exactly what one
    // covers, and every edge they share reads as used four times — a
    // non-manifold edge in the finished body, and no surface at all.
    if let Ok(patch) = &mut chosen {
        lay_each_facet_once(patch);
    }
    // What the finished face actually costs in shape: how far the midpoint of
    // each triangle edge falls from the surface the edge stands for. This is
    // the faceting a viewer sees, measured on the patch that was kept rather
    // than predicted from the tolerance that asked for it — and several
    // patches are built per face, so measuring where they are built says
    // nothing about the one that survives.
    if std::env::var_os("CAD_TESS_FACE_SAG").is_some() {
        if let Ok(p) = &chosen {
            let surface = solid.surface(solid.face(fid).surface);
            let local = options.for_extent(face_extent(solid, fid));
            let at = |i: u32| {
                let q = p.positions[i as usize];
                Vec3::new(q[0] as f64, q[1] as f64, q[2] as f64)
            };
            let mut worst = 0.0f64;
            let mut worst_edge = (Vec3::ZERO, Vec3::ZERO);
            for tri in p.indices.chunks_exact(3) {
                for k in 0..3 {
                    let (a, b) = (at(tri[k]), at(tri[(k + 1) % 3]));
                    let mid = (a + b) * 0.5;
                    // Distance to the surface, found by inverting the chord's
                    // midpoint — seeded from one of its own endpoints. A
                    // surface that passes close to itself, which a helical
                    // sweep does once per turn, otherwise inverts the midpoint
                    // onto the *neighbouring* coil and reports the gap between
                    // coils as faceting: on this pilot it read the spring, a
                    // face measured elsewhere as 0.25 mm from OpenCASCADE's,
                    // as 4.2 mm from its own surface. The endpoint is on the
                    // surface, so its own inversion needs no hint and gives a
                    // sound one.
                    //
                    // It is still not sound on a surface closed in u. Face 122
                    // of `201 201 003-51` is a groove swept all the way round;
                    // a 1.07 mm edge across its seam has ends at u = 0.0476 and
                    // u = 1.0000, the midpoint inverts to the far side of the
                    // loop, and the face reads **9.28 mm** — six times worse
                    // than anything else in the model, on a mesh whose largest
                    // departure from OpenCASCADE anywhere is 2.38 mm.
                    //
                    // Averaging the endpoints' own parameters instead, with the
                    // seam taken into account, was tried and measured worse:
                    // the parameter midpoint is not the chord's midpoint unless
                    // the parameterisation is uniform, and on this body it took
                    // faces that read 0.85 mm to 2.71. Reverted. The reading
                    // stands as it is, and the `[facesag-edge]` line beneath
                    // prints both ends' parameters — when they sit a period
                    // apart, the number above is the seam and not the mesh.
                    let hint = surface.invert(a, None);
                    if let Some(uv) = surface.invert(mid, hint) {
                        let d = (surface.point_at(uv) - mid).length();
                        if d > worst {
                            worst = d;
                            worst_edge = (a, b);
                        }
                    }
                }
            }
            println!(
                "[facesag] {worst:.6} {} {} {:.6} {} face={} body={}",
                p.indices.len() / 3,
                surface_kind(surface),
                local.sag,
                if p.rebuilt { "rebuilt" } else { "surface" },
                fid.0,
                solid.name,
            );
            if worst > local.sag * 20.0 {
                let (a, b) = worst_edge;
                let uva = surface.invert(a, None).unwrap_or_default();
                let uvb = surface.invert(b, None).unwrap_or_default();
                println!(
                    "[facesag-edge] len {:.4} uv [{:.4},{:.4}]..[{:.4},{:.4}] tris {} {}",
                    (b - a).length(),
                    uva.u, uva.v, uvb.u, uvb.v,
                    p.indices.len() / 3,
                    surface_kind(surface),
                );
            }
        }
    }
    chosen
}

/// Of the readings of this face that were tried, the one that drew most of it.
fn choose_reading(
    solid: &Solid,
    fid: FaceId,
    edges: &[Chain],
    options: &Resolved,
) -> Result<Patch, String> {
    let plain = tessellate_reading(solid, fid, edges, options, false);
    // Only worth asking the other way where the first left something open,
    // and only on a surface that has a seam to cross.
    let seamed = solid.surface(solid.face(fid).surface).domain().u_period.is_some();
    let left_open = match &plain {
        Ok(p) => p.undrawn,
        Err(_) => usize::MAX,
    };
    if !seamed || left_open == 0 {
        return plain;
    }
    match tessellate_reading(solid, fid, edges, options, true) {
        Ok(other) if other.undrawn < left_open => Ok(other),
        _ => plain,
    }
}

fn tessellate_reading(
    solid: &Solid,
    fid: FaceId,
    edges: &[Chain],
    options: &Resolved,
    carry_seam: bool,
) -> Result<Patch, String> {
    // The face is held to a fraction of its own extent, as its edges were:
    // how finely a face's interior is sampled has to follow the face, not the
    // assembly it belongs to.
    let options = &options.for_extent(face_extent(solid, fid));
    let face = solid.face(fid);
    let surface = solid.surface(face.surface);
    let domain = surface.domain();

    // Parameter space is anisotropic — on a cylinder u is radians and v is
    // millimetres — and a Delaunay triangulation of raw parameters produces
    // slivers. Scaling both axes to comparable 3D lengths fixes that, and the
    // scale is divided back out before anything is evaluated.
    let scale = parameter_scale(surface, &domain);

    let mut loops = Vec::new();
    // A single-vertex bound is the file stating that this face runs to a point
    // — a cone's apex, a sphere's pole. It carries no edges, so it contributes
    // no boundary, but its existence is the only trustworthy licence to close a
    // face onto a pole at all.
    let mut apexes: Vec<Vec3> = Vec::new();
    for bound in &face.bounds {
        if let Some(v) = bound.vertex
            && bound.halves.is_empty()
        {
            apexes.push(solid.vertex(v));
            continue;
        }
        // The same statement written the other way: a loop whose every edge
        // collapses onto one point. Parasolid has no vertex-loop concept, so
        // a cone's apex arrives as a bound carrying a zero-length edge, and
        // reading it as an ordinary loop threw it away — which left the face
        // with one wrapping ring, nothing to close it onto, and no way to
        // mesh at all. The point is stated either way; both ways are read.
        let collapsed = !bound.halves.is_empty()
            && bound.halves.iter().all(|h| {
                edges.get(h.edge.index()).is_some_and(|c| {
                    c.points
                        .iter()
                        .all(|q| (*q - c.points[0]).length_squared() < 1e-24)
                })
            });
        if collapsed
            && let Some(first) = bound
                .halves
                .first()
                .and_then(|h| edges.get(h.edge.index()))
                .and_then(|c| c.points.first())
        {
            // What the file gave for a bound that collapsed to a point: the
            // curve each of its edges names, and how long that edge's chain
            // came out. A face whose every bound collapses has no boundary to
            // trim against, and the fault is in whatever produced the curve,
            // not in the trimming.
            if std::env::var_os("CAD_TESS_DROPPED").is_some() {
                for h in &bound.halves {
                    let e = &solid.edges[h.edge.index()];
                    let curve = &solid.curves[e.curve.index()];
                    let chain = edges.get(h.edge.index());
                    println!(
                        "[collapsed] edge {} curve {:?} range [{:.6},{:.6}] points {} span {:.6} \
                         vertices {:.4}",
                        h.edge.index(),
                        std::mem::discriminant(curve),
                        e.range.lo,
                        e.range.hi,
                        chain.map_or(0, |c| c.points.len()),
                        chain.map_or(0.0, |c| {
                            let mut b = cad_ir::math::Aabb::EMPTY;
                            for q in &c.points {
                                b.add_point(*q);
                            }
                            b.diagonal()
                        }),
                        (solid.vertices[e.end.index()] - solid.vertices[e.start.index()]).length(),
                    );
                    // The curve's own reach, before anything the tessellator
                    // did to it: this separates a reader that produced nonsense
                    // from a repair that threw away something usable.
                    let inner = match curve {
                        cad_ir::brep::Curve::Trimmed { base, range } => format!(
                            "base {:?} trim [{:.6},{:.6}] base-natural [{:.4},{:.4}]",
                            std::mem::discriminant(&**base),
                            range.lo,
                            range.hi,
                            base.natural_range().lo,
                            base.natural_range().hi
                        ),
                        _ => String::new(),
                    };
                    println!("[dropped]     {inner}");
                    let nat = curve.natural_range();
                    let mut raw = cad_ir::math::Aabb::EMPTY;
                    for i in 0..=32 {
                        raw.add_point(curve.point_at(nat.at(i as f64 / 32.0)));
                    }
                    println!(
                        "[collapsed]   curve itself: natural [{:.4},{:.4}] spans {:.4} \
                         from [{:.3},{:.3},{:.3}]",
                        nat.lo,
                        nat.hi,
                        raw.diagonal(),
                        curve.point_at(nat.lo).x,
                        curve.point_at(nat.lo).y,
                        curve.point_at(nat.lo).z,
                    );
                }
            }
            drop_note("read as a degenerate point bound", bound.halves.len());
            apexes.push(*first);
            continue;
        }
        match build_loop(bound, surface, &domain, edges, solid.tolerance, carry_seam)? {
            Some(l) => loops.push(l),
            // A dropped loop is boundary the face will not draw, and every
            // segment of it is one its neighbour draws alone.
            None if std::env::var_os("CAD_TESS_DROPPED").is_some() => {
                let segs: usize = bound
                    .halves
                    .iter()
                    .filter_map(|h| edges.get(h.edge.index()))
                    .map(|c| c.points.len().saturating_sub(1))
                    .sum();
                let mut bb = cad_ir::math::Aabb::EMPTY;
                for h in &bound.halves {
                    if let Some(c) = edges.get(h.edge.index()) {
                        for q in &c.points {
                            bb.add_point(*q);
                        }
                    }
                }
                println!(
                    "[dropped] {} {segs} halves={} extent={:.6} tol={:.6}",
                    surface_kind(surface),
                    bound.halves.len(),
                    bb.diagonal(),
                    solid.tolerance
                );
                for h in &bound.halves {
                    let e = &solid.edges[h.edge.index()];
                    let c = edges.get(h.edge.index());
                    let curve = &solid.curves[e.curve.index()];
                    let mut b = cad_ir::math::Aabb::EMPTY;
                    if let Some(c) = c {
                        for q in &c.points {
                            b.add_point(*q);
                        }
                    }
                    println!(
                        "[dropped]   edge {} curve {:?} range [{:.4},{:.4}] chain {} pts \
                         spanning {:.4}; first [{:.3},{:.3},{:.3}] last [{:.3},{:.3},{:.3}]",
                        h.edge.index(),
                        std::mem::discriminant(curve),
                        e.range.lo,
                        e.range.hi,
                        c.map_or(0, |c| c.points.len()),
                        b.diagonal(),
                        c.and_then(|c| c.points.first()).map_or(0.0, |p| p.x),
                        c.and_then(|c| c.points.first()).map_or(0.0, |p| p.y),
                        c.and_then(|c| c.points.first()).map_or(0.0, |p| p.z),
                        c.and_then(|c| c.points.last()).map_or(0.0, |p| p.x),
                        c.and_then(|c| c.points.last()).map_or(0.0, |p| p.y),
                        c.and_then(|c| c.points.last()).map_or(0.0, |p| p.z),
                    );
                    // How far the curve itself bows away from the straight
                    // line between its ends. If that is under the tolerance
                    // the face is held to, the rail *is* straight as far as
                    // this mesh can tell, and two rails that are both straight
                    // between the same two points bound nothing.
                    let r = e.range;
                    let (a, b) = (curve.point_at(r.lo), curve.point_at(r.hi));
                    let axis = b - a;
                    let len2 = axis.length_squared();
                    let mut bow = 0.0f64;
                    for i in 1..64 {
                        let q = curve.point_at(r.at(i as f64 / 64.0));
                        let d = if len2 > 0.0 {
                            let t = ((q - a).dot(axis) / len2).clamp(0.0, 1.0);
                            (q - (a + axis * t)).length()
                        } else {
                            (q - a).length()
                        };
                        bow = bow.max(d);
                    }
                    let inner = match curve {
                        cad_ir::brep::Curve::Trimmed { base, range } => format!(
                            "base {:?} trim [{:.6},{:.6}] base-natural [{:.4},{:.4}]",
                            std::mem::discriminant(&**base),
                            range.lo,
                            range.hi,
                            base.natural_range().lo,
                            base.natural_range().hi
                        ),
                        _ => String::new(),
                    };
                    println!("[dropped]     {inner}");
                    let nat = curve.natural_range();
                    println!(
                        "[dropped]     bows {bow:.6} from its chord; natural range \
                         [{:.4},{:.4}] span {:.4}; period {:?}",
                        nat.lo,
                        nat.hi,
                        nat.span(),
                        curve.period(),
                    );
                }
            }
            None => {}
        }
    }

    // Wrapping loops come in two kinds and they need opposite treatment.
    //
    // A *bare ring* is a circle at constant v: it travels a whole period and
    // encloses nothing, so on its own it bounds no region and has to be paired
    // with something — another ring, a wavy boundary, or a declared apex.
    //
    // A *band boundary* also travels a whole period but encloses real area,
    // because the file put a seam edge in the loop. Its implicit closing edge
    // already runs along that seam, so it is a complete polygon in the unrolled
    // strip and needs nothing added.
    //
    // Telling them apart by area alone fails: a wavy boundary and a ring can
    // both sit next to a much larger loop. Comparing each loop's area against
    // the band its own parameter extent would sweep is local and decides both.
    let wrapping: Vec<usize> = loops
        .iter()
        .enumerate()
        .filter(|(_, l)| l.wrap != 0)
        .map(|(i, _)| i)
        .collect();
    let is_bare_ring = |l: &UvLoop| {
        let (u_lo, u_hi) = span(l.uv.iter().map(|p| p.u));
        let (v_lo, v_hi) = span(l.uv.iter().map(|p| p.v));
        l.area.abs() <= (u_hi - u_lo) * (v_hi - v_lo) * 0.05 + 1e-12
    };
    // One wrapping loop that is not a bare ring closes itself; sending it to
    // the seam-building path would have it look for a partner it does not need.
    //
    // Unless its two ends are the same point in space. Then the loop is a ring
    // however much area it sweeps on its way round: its first and last
    // parameters are a whole period apart, so the polygon's own closing edge
    // is a chord straight across the domain, and nothing downstream can draw
    // a boundary that crosses everything else in the face.
    //
    // Only where the surface has a point to close onto. A sphere's poles and a
    // cone's apex are places the parameter domain genuinely collapses, so a
    // ring there bounds a cap and the seam path has something to draw to. A
    // spline's domain edge is an ordinary curve, and closing a ring onto it
    // would draw a lid the model does not have. No spline face in the pilot
    // assembly reaches this test, so the restriction costs nothing there; it
    // is here because the path it guards is only meaningful where a point
    // exists.
    let closes_to_a_point = matches!(
        surface,
        Surface::Sphere { .. } | Surface::Cone { .. } | Surface::Revolution { .. }
    );
    // And only where the ring does not already reach that point. A ring whose
    // own v touches the pole carries the apex already; closing it onto one
    // puts a second copy of a point the boundary has, the strip pinches to
    // nothing there, and the polygon crosses itself. That is the 0.5 mm ball
    // of the pilot assembly, whose ring runs from the equator to v = π/2
    // exactly.
    //
    // Measured in space, against the sag. On the ball in question the ring
    // stops 5.5e-7 of a radian short of the pole, which on a 0.5 mm sphere is
    // three ten-thousandths of a micron, and no threshold in `v` reads that as
    // touching without also catching rings that plainly are not.
    //
    // Three wider tests were measured and every one is worse. The ring's own
    // median step, and the seam's own first step, both catch the 11.35 mm
    // sphere of the section above — whose ring keeps 1.52 mm clear and closes
    // perfectly well — and its 4.3 mm lid comes back with it: points over 1 mm
    // against OpenCASCADE 20 → 109 and 20 → 113. Asking exactly whether the
    // ring rises above the closing chord, which is the real failure, spares
    // that sphere but still costs 20 → 24, and fixes nothing in the Parasolid
    // reading it was built for.
    let d = surface.domain();
    let poles = [d.v.lo, d.v.hi].map(|v| surface.point_at(Vec2::new(d.u.lo, v)));
    let reaches_the_point = |l: &UvLoop| {
        l.xyz
            .iter()
            .any(|q| poles.iter().any(|p| p.is_finite() && (*q - *p).length() <= options.sag))
    };
    if std::env::var_os("CAD_TESS_LOOPS").is_some() && closes_to_a_point {
        for l in loops.iter().filter(|l| l.wrap != 0) {
            let (lo, hi) = l
                .uv
                .iter()
                .fold((f64::MAX, f64::MIN), |(a, b), q| (a.min(q.v), b.max(q.v)));
            println!(
                "[loop]   a wrapping ring on a {}: v {:.9}..{:.9}, nearest approach to a pole {:.3e} rad, {:.6} mm, against a sag of {:.6}",
                surface_kind(surface),
                lo,
                hi,
                (lo - d.v.lo).abs().min((hi - d.v.hi).abs()),
                l.xyz
                    .iter()
                    .map(|q| {
                        poles
                            .iter()
                            .filter(|p| p.is_finite())
                            .map(|p| (*q - *p).length())
                            .fold(f64::INFINITY, f64::min)
                    })
                    .fold(f64::INFINITY, f64::min),
                options.sag,
            );
        }
    }
    let ends_meet = |l: &UvLoop| {
        closes_to_a_point
            && l.xyz.len() >= 2
            && (l.xyz[l.xyz.len() - 1] - l.xyz[0]).length_squared() < 1e-18
            && !reaches_the_point(l)
    };
    let wrapping: Vec<usize> = if wrapping.len() == 1
        && !is_bare_ring(&loops[wrapping[0]])
        && !ends_meet(&loops[wrapping[0]])
    {
        Vec::new()
    } else {
        wrapping
    };

    if std::env::var_os("CAD_TESS_TRACE_VWRAP").is_some()
        && let Some(pv) = domain.v_period
    {
        for l in &loops {
            let travel = l.uv[l.uv.len() - 1].v - l.uv[0].v;
            let periods = travel / pv;
            if (periods.abs() - 1.0).abs() < 0.15 {
                eprintln!(
                    "[vwrap] face {} {} bounds={} loop n={} v-travel={:.3} periods u-span={:.3} area={:.3e}",
                    fid.0,
                    surface_kind(surface),
                    face.bounds.len(),
                    l.uv.len(),
                    periods,
                    span(l.uv.iter().map(|p| p.u)).1 - span(l.uv.iter().map(|p| p.u)).0,
                    l.area
                );
            }
        }
    }

    if std::env::var_os("CAD_TESS_TRACE").is_some() {
        eprintln!(
            "[face {}] surface={} bounds={} domain u=[{:.3},{:.3}] v=[{:.4e},{:.4e}] \
             u_period={:?} halves=[{}] loops={:?}",
            fid.0,
            surface_kind(surface),
            face.bounds.len(),
            domain.u.lo,
            domain.u.hi,
            domain.v.lo,
            domain.v.hi,
            domain.u_period,
            face.bounds
                .iter()
                .map(|b| {
                    b.halves
                        .iter()
                        .map(|h| {
                            let e = solid.edge(h.edge);
                            format!(
                                "{}{}",
                                if h.forward { '+' } else { '-' },
                                curve_kind(solid.curve(e.curve))
                            )
                        })
                        .collect::<Vec<_>>()
                        .join(",")
                })
                .collect::<Vec<_>>()
                .join(" | "),
            loops
                .iter()
                .map(|l| format!(
                    "{{n={} wrap={} area={:.3e} u=[{:.3},{:.3}] v=[{:.4},{:.4}]}}",
                    l.uv.len(),
                    l.wrap,
                    l.area,
                    span(l.uv.iter().map(|p| p.u)).0,
                    span(l.uv.iter().map(|p| p.u)).1,
                    span(l.uv.iter().map(|p| p.v)).0,
                    span(l.uv.iter().map(|p| p.v)).1,
                ))
                .collect::<Vec<_>>()
        );
    }

    if std::env::var_os("CAD_TESS_TRACE_UV").is_some()
        && loops.iter().filter(|l| l.wrap != 0).count() == 1
    {
        for (i, l) in loops.iter().enumerate() {
            eprintln!(
                "  [{}] loop {i}: wrap={} n={} area={:.4} halves={}",
                surface_kind(surface),
                l.wrap,
                l.uv.len(),
                l.area,
                face.bounds
                    .iter()
                    .map(|b| b
                        .halves
                        .iter()
                        .map(|h| format!(
                            "{}{}",
                            if h.forward { '+' } else { '-' },
                            curve_kind(solid.curve(solid.edge(h.edge).curve))
                        ))
                        .collect::<Vec<_>>()
                        .join(","))
                    .collect::<Vec<_>>()
                    .join(" | ")
            );
            for (uv, xyz) in l.uv.iter().zip(&l.xyz) {
                eprintln!(
                    "    u={:9.5} v={:9.5}   xyz=({:9.4},{:9.4},{:9.4})",
                    uv.u, uv.v, xyz.x, xyz.y, xyz.z
                );
            }
        }
    }

    if std::env::var_os("CAD_TESS_ON_SURFACE").is_some() {
        for l in loops.iter() {
            let r = l
                .uv
                .iter()
                .zip(&l.xyz)
                .map(|(uv, p)| (surface.point_at(*uv) - *p).length())
                .fold(0.0f64, f64::max);
            let kind = match surface {
                Surface::Nurbs(n) if n.u_degree == 1 && n.v_degree == 1 => "grid".to_string(),
                Surface::Nurbs(_) => "nurbs".to_string(),
                other => format!("{:?}", std::mem::discriminant(other)),
            };
            if r > 0.01 {
                let curves: Vec<String> = face
                    .bounds
                    .iter()
                    .flat_map(|b| b.halves.iter())
                    .map(|h| {
                        format!("{:?}", std::mem::discriminant(solid.curve(solid.edge(h.edge).curve)))
                    })
                    .collect();
                println!("[resid] {r:.6} {kind} halves={} curves={} face={}", curves.len(), curves.join(","), fid.0);
            } else {
                let extent = {
                    let mut b = cad_ir::math::Aabb::EMPTY;
                    for l in loops.iter() {
                        for q in &l.xyz {
                            b.add_point(*q);
                        }
                    }
                    b.diagonal()
                };
                println!("[resid] {r:.6} {kind} rel {:.6} face={}", r / extent.max(1e-12), fid.0);
            }
        }
    }

    // The boundary the *file* gave, kept before the region step merges the
    // loops into one walk. Every later check has to measure against this: a
    // merged region is the tessellator's own reconstruction, and scoring a
    // patch against it lets a bad reconstruction validate itself — the patch
    // draws the merged ring perfectly and the file's segments stay open.
    let file_rings: Vec<Vec<Vec3>> = loops.iter().map(|l| l.xyz.clone()).collect();

    if std::env::var_os("CAD_TESS_BOUNDS").is_some() {
        // What the file says this face's boundary is made of: the curve behind
        // every half-edge and how many points it contributed. A chord where an
        // arc should be shows up here as a line among circles.
        let parts: Vec<String> = face
            .bounds
            .iter()
            .map(|b| {
                b.halves
                    .iter()
                    .map(|h| {
                        let n = edges.get(h.edge.index()).map(|c| c.points.len()).unwrap_or(0);
                        {
                            let e = solid.edge(h.edge);
                            let (a, b) = (solid.vertex(e.start), solid.vertex(e.end));
                            format!(
                                "{}({})x{n}[{:.2}]#{}c{}",
                                curve_kind(solid.curve(e.curve)),
                                {
                                    let mut c = solid.curve(e.curve);
                                    while let cad_ir::brep::Curve::Trimmed { base, .. } = c {
                                        c = base;
                                    }
                                    curve_kind(c)
                                },
                                (b - a).length(),
                                h.edge.0,
                                e.curve.0
                            )
                        }
                    })
                    .collect::<Vec<_>>()
                    .join(" ")
            })
            .collect();
        println!(
            "[bounds] face {} {} sizes={:?} | {}",
            fid.0,
            surface_kind(surface),
            file_rings.iter().map(|r| r.len()).collect::<Vec<_>>(),
            parts.join("  ||  ")
        );
    }

    // A boundary rebuild that did not quite cover its boundary, kept so the
    // surface path can be measured against it rather than replacing it.
    let mut rebuilt: Option<(usize, Patch)> = None;

    // A surface that is degree one in both directions is a grid of points
    // with flat cells between them — a mesh, not a designed surface, and the
    // only thing that produces one here is a face rebuilt from its own
    // boundary. Such a face is meshed from that boundary directly, because
    // asking the grid where a boundary point sits is the one question it
    // cannot answer accurately, and a wrong answer there slits the face.
    if let Surface::Nurbs(n) = surface
        && n.u_degree == 1
        && n.v_degree == 1
        && loops.len() == 1
        && loops[0].wrap == 0
        && apexes.is_empty()
    {
        // Which four points of the ring are the patch's corners is a
        // judgement, and getting it wrong leaves whole stretches of the
        // boundary undrawn — every one of which is a crack against the
        // neighbour that did draw it. So both readings are built and the one
        // that actually covers the boundary is kept, exactly as the surface
        // path below chooses between its own candidates. Taking the first
        // reading on trust left this, the path every rebuilt blend face
        // takes, as the only unmeasured step in the tessellator.
        let rings: Vec<&[Vec3]> = file_rings.iter().map(|r| r.as_slice()).collect();
        // What the reader measured about this face's interior, where it did.
        let measured = match surface {
            Surface::Nurbs(n) if solid.is_measured(face.surface) => Some(n),
            _ => None,
        };
        let mut best: Option<(usize, Patch)> = None;
        let mut why = String::from("boundary mesh produced no patch");
        for candidate in [
            blend_patch(&loops[0].xyz, &[], false, options, face.same_sense, measured),
            blend_patch(&loops[0].xyz, &[], true, options, face.same_sense, measured),
        ] {
            match candidate {
                Ok(patch) => {
                    let gaps = boundary_gaps(&patch, &rings) + interior_holes(&patch, &rings);
                    if best.as_ref().is_none_or(|(b, _)| gaps < *b) {
                        best = Some((gaps, patch));
                    }
                }
                Err(reason) => why = reason,
            }
        }
        // How far a rebuild's own interior leaves the surface it stands for.
        // Covering the boundary says nothing about the middle: a Coons patch
        // is built from the edges and is free to bulge anywhere between them.
        let departure = |patch: &Patch| {
            let mut worst = 0.0f64;
            for q in &patch.positions {
                let w = Vec3::new(q[0] as f64, q[1] as f64, q[2] as f64);
                if let Some(uv) = surface.invert(w, None) {
                    let near = (surface.point_at(uv) - w).length();
                    if near.is_finite() && near > worst {
                        worst = near;
                    }
                }
            }
            worst
        };
        if let Some((gaps, patch)) = &best
            && std::env::var_os("CAD_TESS_REBUILD_OFF").is_some()
        {
            println!(
                "[rebuild] face {} {} gaps={gaps} worst departure from its surface {:.6} mm, sag {:.6}",
                fid.0,
                surface_kind(surface),
                departure(patch),
                options.sag
            );
        }
        match best {
            // A rebuild that covers the whole boundary is the answer: it is
            // built from that boundary and cannot stray from it.
            //
            // Requiring it also to stay within the sag of the surface it
            // stands for was measured and is much worse — 332 open edges, 178
            // non-manifold, five faces unmeshable and the triangle count up by
            // half. Inverting a point onto a degree-one grid is the one thing
            // that grid cannot answer, so the departure is often a lie about
            // the solver rather than a fact about the patch, and it pushes
            // sound rebuilds into a surface path that then fails. The probe is
            // kept because the measurement itself is real where the inversion
            // holds: face 158 of `205 211 013-51-oa2` does sit 0.032 mm from
            // its grid against a sag of 0.0247.
            Some((0, patch)) => return Ok(patch),
            // One that does not is still probably the answer, but not on
            // trust — it is carried down to be weighed against the ordinary
            // surface path, which for these faces is usually worse and
            // occasionally very much better.
            Some(other) => rebuilt = Some(other),
            None => return Err(why),
        }
    }

    // The extent of the boundary the *file* supplied, captured before any seam
    // or pole ring is synthesised. Measuring the finished patch against a
    // boundary the tessellator invented would let a bad invention validate
    // itself.
    let mut boundary = cad_ir::math::Aabb::EMPTY;
    for l in loops.iter() {
        for p in &l.xyz {
            boundary.add_point(*p);
        }
    }

    // What a face's boundary actually came out as, for the handful that still
    // fail. A loop is only as good as the chains it was built from, and where
    // two of them trace the same path the region between them is empty however
    // it is triangulated.
    if std::env::var_os("CAD_TESS_TRACE_FACE").is_some_and(|v| {
        v.to_string_lossy() == surface_kind(surface)
    }) {
        if let Surface::Torus { major_radius, minor_radius, .. } = surface {
            println!("[face] torus major {major_radius:.4} minor {minor_radius:.4}");
        }
        println!(
            "[face] face={} {} bounds {} loops {} area {:?}",
            CURRENT_FACE.with(|c| c.get()),
            surface_kind(surface),
            face.bounds.iter().map(|b| b.halves.len()).sum::<usize>(),
            loops.len(),
            loops.iter().map(|l| l.area).collect::<Vec<_>>(),
        );
        for (i, l) in loops.iter().enumerate() {
            println!("[face]  loop {i}: {} points, wrap {}", l.uv.len(), l.wrap);
            for (k, (uv, q)) in l.uv.iter().zip(&l.xyz).enumerate() {
                if k < 6 || k + 6 >= l.uv.len() || std::env::var_os("CAD_TESS_TRACE_ALL").is_some() {
                    println!(
                        "[face]    {k:3} uv ({:.6},{:.6}) xyz [{:.4},{:.4},{:.4}]",
                        uv.u, uv.v, q.x, q.y, q.z
                    );
                }
            }
        }
    }

    let (outer, holes) = if loops.is_empty() {
        // A whole sphere or torus is written with no bounds at all: the surface
        // is closed in both directions, so there is nothing to trim. But a
        // face that *did* declare bounds and lost them is a different thing
        // entirely, and handing it the surface's whole domain draws a face
        // the file never described — on one offset face here that invented a
        // boundary of 512 segments, every one of them a crack, because no
        // neighbour goes anywhere near it. Say so instead.
        if face.bounds.iter().any(|b| !b.halves.is_empty()) {
            return Err(format!(
                "face declares {} boundary edges but none of them could be built",
                face.bounds.iter().map(|b| b.halves.len()).sum::<usize>()
            ));
        }
        (full_domain_loop(surface, &domain, options)?, Vec::new())
    } else {
        let (outer, mut holes) = if wrapping.is_empty() {
            closed_region(&mut loops)?
        } else {
            wrapped_region(
                &mut loops,
                &wrapping,
                &apexes,
                surface,
                &domain,
                options,
                face.same_sense,
            )?
        };
        if std::env::var_os("CAD_TESS_WRAP").is_some() {
            println!(
                "[wrap] face {} {} loops {} wrapping {} outer {} pts, u span {:.4}, period {:?}",
                fid.0,
                surface_kind(surface),
                loops.len() + wrapping.len(),
                wrapping.len(),
                outer.uv.len(),
                {
                    let (lo, hi) = span(outer.uv.iter().map(|p| p.u));
                    hi - lo
                },
                domain.u_period,
            );
        }
        rephase_holes(&outer, &mut holes, &domain);
        // What boundary this face ended up with, against what the file gave
        // it. Two faces meeting at an edge have to draw the same points there;
        // where one of them rebuilds its boundary instead, the mesh opens
        // along that edge and nothing inside the face can tell.
        if std::env::var_os("CAD_TESS_RINGS").is_some() {
            let mean = |r: &[Vec3]| {
                r.iter().fold(Vec3::ZERO, |a, p| a + *p) * (1.0 / r.len().max(1) as f64)
            };
            let from_file: usize = face.bounds.iter().map(|b| b.halves.len()).sum();
            let m = mean(&outer.xyz);
            println!(
                "[rings] face {} {} bounds {from_file} edges, outer {} pts at                  [{:.3},{:.3},{:.3}], {} holes",
                fid.0,
                surface_kind(surface),
                outer.xyz.len(),
                m.x,
                m.y,
                m.z,
                holes.len()
            );
            for h in &holes {
                let m = mean(&h.xyz);
                println!(
                    "[rings]   hole {} pts at [{:.3},{:.3},{:.3}]",
                    h.xyz.len(),
                    m.x,
                    m.y,
                    m.z
                );
            }
            // A fingerprint of the boundary as circles about x: how many
            // points sit on each. Two faces meeting at a circular edge must
            // report the same count, and where they do not the mesh is open
            // along it.
            let mut circles: std::collections::BTreeMap<(i64, i64), usize> = Default::default();
            for q in outer.xyz.iter().chain(holes.iter().flat_map(|h| h.xyz.iter())) {
                let r = q.y.hypot(q.z);
                *circles
                    .entry(((q.x * 1e4).round() as i64, (r * 1e4).round() as i64))
                    .or_default() += 1;
            }
            // The points themselves, for one nominated circle, so the two
            // faces that meet there can be compared point for point.
            if let Ok(want) = std::env::var("CAD_TESS_RING_AT") {
                let mut parts = want.split(',').filter_map(|v| v.trim().parse::<f64>().ok());
                if let (Some(wx), Some(wr)) = (parts.next(), parts.next()) {
                    let mut on: Vec<f64> = outer
                        .xyz
                        .iter()
                        .chain(holes.iter().flat_map(|h| h.xyz.iter()))
                        .filter(|q| {
                            (q.x - wx).abs() < 1e-3 && (q.y.hypot(q.z) - wr).abs() < 1e-3
                        })
                        .map(|q| q.z.atan2(q.y))
                        .collect();
                    if !on.is_empty() {
                        on.sort_by(|a, b| a.partial_cmp(b).unwrap());
                        println!("[at] face {} has {} points, angles:", fid.0, on.len());
                        for a in &on {
                            println!("[at]   {:.6}", a);
                        }
                    }
                }
            }
            for ((x, r), n) in circles.iter().filter(|(_, n)| **n > 2) {
                println!(
                    "[circle] face {} x {:.4} r {:.4} pts {n}",
                    fid.0,
                    *x as f64 / 1e4,
                    *r as f64 / 1e4
                );
            }
        }
        (outer, holes)
    };

    if outer.uv.len() < 3 {
        return Err(format!(
            "outer boundary has only {} parameter points",
            outer.uv.len()
        ));
    }

    if boundary.is_empty() {
        for p in outer.xyz.iter().chain(holes.iter().flat_map(|h| h.xyz.iter())) {
            boundary.add_point(*p);
        }
    }

    let rings: Vec<&[Vec3]> = file_rings.iter().map(|r| r.as_slice()).collect();
    let patch = match triangulate(
        surface,
        &domain,
        outer,
        holes,
        scale,
        options,
        face.same_sense,
        &file_rings,
        carry_seam,
    ) {
        Ok(p) => match rebuilt.take() {
            // Whichever leaves fewer cracks behind.
            Some((gaps, r)) if gaps < boundary_gaps(&p, &rings) + interior_holes(&p, &rings) => r,
            _ => p,
        },
        Err(e) => match rebuilt.take() {
            Some((_, r)) => r,
            None => return Err(e),
        },
    };

    // A patch may legitimately bulge past its boundary — a spherical cap
    // reaches a radius above the circle that bounds it — but not by much more
    // than the boundary's own size. Anything further means the parameter-space
    // region was wrong, and emitting it would drag the whole model's bounding
    // box out with it and coarsen every relative tolerance downstream.
    if !boundary.is_empty() {
        let allowance = boundary.diagonal().max(options.sag * 16.0);
        let centre = boundary.centre();
        let limit = boundary.diagonal() * 0.5 + allowance;
        let mut worst = 0.0f64;
        for p in &patch.positions {
            let d = (cad_ir::math::Vec3::new(p[0] as f64, p[1] as f64, p[2] as f64) - centre)
                .length();
            worst = worst.max(d);
        }
        if worst > limit {
            return Err(format!(
                "patch reaches {worst:.1} from its boundary centre, but the boundary is only \
                 {:.1} across",
                boundary.diagonal()
            ));
        }
    }

    Ok(patch)
}

/// Say why a loop was dropped, when asked.
fn drop_note(why: &str, n: usize) {
    if std::env::var_os("CAD_TESS_DROPPED").is_some() {
        println!("[drop-why] {why} ({n})");
    }
}

/// How big a face is, from the boundary the file gave it.
fn face_extent(solid: &Solid, fid: FaceId) -> f64 {
    let mut b = cad_ir::math::Aabb::EMPTY;
    for bound in &solid.face(fid).bounds {
        for h in &bound.halves {
            let e = solid.edge(h.edge);
            b.add_point(solid.vertex(e.start));
            b.add_point(solid.vertex(e.end));
            let c = solid.curve(e.curve);
            for i in 0..=4 {
                let p = c.point_at(e.range.at(i as f64 / 4.0));
                if p.x.is_finite() && p.y.is_finite() && p.z.is_finite() {
                    b.add_point(p);
                }
            }
        }
    }
    let size = b.size();
    size.x.max(size.y).max(size.z)
}

/// Map one bound into parameter space.
fn build_loop(
    bound: &Bound,
    surface: &Surface,
    domain: &Domain,
    edges: &[Chain],
    tolerance: f64,
    carry_seam: bool,
) -> Result<Option<UvLoop>, String> {
    // A single-vertex loop marks a degenerate point — a cone apex, a sphere
    // pole. It bounds no area and contributes no constraint.
    if bound.vertex.is_some() && bound.halves.is_empty() {
        return Ok(None);
    }

    let mut xyz: Vec<Vec3> = Vec::new();
    for half in &bound.halves {
        let chain = edges
            .get(half.edge.index())
            .ok_or_else(|| format!("edge {} has no discretisation", half.edge.0))?;
        if chain.points.len() < 2 {
            continue;
        }
        let pts = chain.oriented(half.forward);
        // Drop the joining point, which the previous half-edge already added.
        let skip = usize::from(!xyz.is_empty());
        xyz.extend(pts.into_iter().skip(skip).map(|(p, _)| p));
    }
    if xyz.len() < 3 {
        drop_note("fewer than three boundary points", xyz.len());
        return Ok(None);
    }

    // A loop can run out along an edge and straight back down the same edge,
    // which the file is entitled to do — it is how a slit through a solid is
    // written — but the spur it makes encloses nothing and has no surface. Left
    // in, its two coincident sides are one line in parameter space, the
    // triangulation cannot enforce a boundary that lies on top of itself, and
    // the region fill leaks out through the gap that leaves. Taking the spur
    // out costs the mesh nothing it was ever going to draw.
    // Spur removal is allowed to take away what encloses nothing, not to take
    // away the loop. Where it would, the reading that called it a spur was
    // wrong — two rails of a blend that run together at their ends look the
    // same locally — and the boundary is kept whole instead. One offset face
    // of the Parasolid pilot, 2.2 mm across, was lost this way.
    let unspurred = xyz.clone();
    loop {
        let n = xyz.len();
        if n < 3 {
            break;
        }
        let Some(tip) = (0..n).find(|i| {
            let (before, after) = (xyz[(i + n - 1) % n], xyz[(i + 1) % n]);
            (before - after).length_squared() < 1e-24
        }) else {
            break;
        };
        // Remove the tip and one of the two coincident points either side.
        let other = (tip + 1) % n;
        let (first, second) = (tip.min(other), tip.max(other));
        xyz.remove(second);
        xyz.remove(first);
    }
    if xyz.len() < 3 {
        drop_note("spur removal left fewer than three points", xyz.len());
        if std::env::var_os("CAD_TESS_DROPPED").is_some() {
            let mut b = cad_ir::math::Aabb::EMPTY;
            for q in &unspurred {
                b.add_point(*q);
            }
            println!(
                "[spur] the loop had {} points spanning {:.4}; restoring it",
                unspurred.len(),
                b.diagonal()
            );
        }
        xyz = unspurred;
        if xyz.len() < 3 {
            return Ok(None);
        }
    }

    // The closing point is kept through the inversion. It coincides with the
    // first point in 3D, but on a periodic surface it lands a whole period away
    // in parameter space — and there it is a genuine, distinct boundary vertex.
    // Dropping it before unwrapping is what turns a full cylindrical band into
    // an open path with its last edge missing.
    let mut uv = unwrap_chain_with(surface, domain, &xyz, tolerance, carry_seam);
    let closes_in_3d = (xyz[xyz.len() - 1] - xyz[0]).length_squared() < 1e-24;

    let wrap = net_wrap(&uv, domain);
    if std::env::var_os("CAD_TESS_LOOPS").is_some() {
        let (lo, hi) = uv
            .iter()
            .fold((f64::INFINITY, f64::NEG_INFINITY), |(a, b), p| (a.min(p.u), b.max(p.u)));
        println!(
            "[loop] {} {} pts  u first {:.4} last {:.4} (travel {:.4}), spanning {:.4} of a period {:?}; closes in 3d {}; counted wrap {}",
            surface_kind(surface),
            uv.len(),
            uv[0].u,
            uv[uv.len() - 1].u,
            uv[uv.len() - 1].u - uv[0].u,
            hi - lo,
            domain.u_period,
            closes_in_3d,
            wrap,
        );
        // The strip builder reads a wrapping ring as a monotone walk in u. A
        // ring that turns back crosses itself once the seam is added, and the
        // triangulation then cannot enforce the boundary at all.
        let back: Vec<(usize, f64)> = uv
            .windows(2)
            .enumerate()
            .filter(|(_, w)| w[1].u < w[0].u)
            .map(|(i, w)| (i, w[1].u - w[0].u))
            .collect();
        if wrap != 0 && !back.is_empty() {
            let worst = back.iter().map(|(_, d)| d.abs()).fold(0.0f64, f64::max);
            println!(
                "[loop]   it turns back in u at {} of {} steps, the largest by {:.4}",
                back.len(),
                uv.len() - 1,
                worst
            );
        }
    }
    if closes_in_3d && wrap == 0 {
        // An ordinary closed loop: the repeat is redundant and would give the
        // polygon a zero-length edge.
        xyz.pop();
        uv.pop();
    }
    if uv.len() < 3 {
        drop_note("fewer than three parameter points", uv.len());
        return Ok(None);
    }

    let area = signed_area(&uv);
    Ok(Some(UvLoop {
        uv,
        xyz,
        travel: wrap.signum(),
        wrap,
        area,
    }))
}

/// Invert a 3D chain onto the surface, keeping the parameter path continuous.
///
/// Each point is placed in the branch nearest its predecessor, so a loop that
/// crosses the seam produces `… 6.1, 6.2, 6.4 …` rather than `… 6.1, 6.2,
/// 0.1 …`. Without this every periodic face is torn in half.
/// The same walk, told whether to read a step that turns back as a crossing of
/// the seam instead.
///
/// Where a boundary crosses the seam the parameter appears to jump most of a
/// period backwards, and where it genuinely doubles back it looks the same. A
/// coarsely sampled edge offers no evidence between the two — two points and a
/// jump — so neither reading can be shown right locally. Both are built and
/// the face keeps whichever leaves it less open.
fn unwrap_chain_with(
    surface: &Surface,
    domain: &Domain,
    xyz: &[Vec3],
    tolerance: f64,
    carry_seam: bool,
) -> Vec<Vec2> {
    let mut out = Vec::with_capacity(xyz.len());
    let mut previous: Option<(Vec2, Vec3)> = None;
    // A parameter is only this boundary point's parameter if the surface
    // evaluated there lands back on it. Anything looser and two points can be
    // handed the same answer — which the triangulation reads as one point, so
    // the boundary loses a corner and the face draws a chord where its
    // neighbour draws two edges. Every such swap is a slit in the mesh.
    let reach = (tolerance * 10.0).max(1e-9);
    for &p in xyz {
        let hint = previous.map(|(uv, _)| uv);
        let residual = |uv: Vec2| (surface.point_at(uv) - p).length();
        // Two questions have to come out the same: does this parameter reach
        // the point, and does it continue the boundary from the last one. A
        // global solve answers the first and can fail the second — on a patch
        // that folds or nearly repeats it returns an equally close parameter
        // somewhere else, and the boundary jumps the domain and crosses
        // itself. A solve started from the neighbour answers the second and
        // can fail the first. So take both answers, keep the ones that reach
        // the point, and among those keep the one nearest the neighbour;
        // only when neither reaches it does accuracy alone decide.
        let mut uv = match hint {
            Some(prev) => {
                let (du, dv) = surface.derivatives_at(prev);
                // Parameter distance measured as the 3D length it stands for,
                // so u and v compare on equal terms.
                let travel = |uv: Vec2| {
                    ((uv.u - prev.u) * du.length()).hypot((uv.v - prev.v) * dv.length())
                };
                let mut best: Option<(Vec2, f64, f64)> = None;
                for c in [surface.invert_near(p, prev, reach), surface.invert(p, Some(prev))]
                    .into_iter()
                    .flatten()
                {
                    let (r, t) = (residual(c), travel(c));
                    let better = match best {
                        None => true,
                        Some((_, br, bt)) => match (br <= reach, r <= reach) {
                            (false, true) => true,
                            (true, false) => false,
                            (true, true) => t < bt,
                            (false, false) => r < br,
                        },
                    };
                    if better {
                        best = Some((c, r, t));
                    }
                }
                let landed = best.map(|(c, _, _)| c).unwrap_or(prev);
                if landed.u != prev.u || landed.v != prev.v {
                    landed
                } else {
                    // The solve could not tell this point from the last one,
                    // and two boundary points at one parameter are one point
                    // to a triangulation — the face loses a corner and slits
                    // against its neighbour. Step by what the surface says the
                    // move from there to here is worth: it separates them, and
                    // because it follows the boundary's own displacement it
                    // does not fold the boundary back across itself the way an
                    // unconstrained solve does.
                    let gap = p - surface.point_at(prev);
                    let (a, b, c) = (du.dot(du), du.dot(dv), dv.dot(dv));
                    let (e, f) = (gap.dot(du), gap.dot(dv));
                    let det = a * c - b * b;
                    if det.abs() > 1e-300 {
                        Vec2::new(prev.u + (e * c - f * b) / det, prev.v + (a * f - b * e) / det)
                    } else {
                        prev
                    }
                }
            }
            None => surface.invert(p, None).unwrap_or_default(),
        };
        if let Some((prev_uv, prev_p)) = previous {
            // At a pole or an apex the surface stops depending on one of its
            // parameters: every u names the same point on a sphere's pole, so
            // the u that comes back is whatever the arithmetic happened to
            // produce from a radial vector of length zero. Taking the
            // neighbour's instead is not an approximation — it is the one
            // choice that keeps the boundary a continuous path, and without it
            // consecutive points jump across the domain and the boundary
            // crosses itself, which costs the whole face its trim.
            let (du, dv) = surface.derivatives_at(uv);
            let (pdu, pdv) = surface.derivatives_at(prev_uv);
            if du.length() <= pdu.length() * 1e-6 {
                uv.u = prev_uv.u;
            }
            if dv.length() <= pdv.length() * 1e-6 {
                uv.v = prev_uv.v;
            }
            // A boundary that runs along the edge of the domain is solved
            // there to whatever precision the surface allows, and where the
            // surface changes little across that edge the answer wanders. On
            // one spline face of the pilot's Parasolid reading the v of six
            // consecutive boundary points came back as 0.9994, 0.9998, 0.9983,
            // 0.9987, 0.9999 — a wobble four times the width of the whole
            // region in u, so the boundary crossed itself and the face lay on
            // itself along 48 edges. Two points on that same edge landed on
            // 1.0 exactly, which is what the rest of them mean.
            //
            // Snapping such a point to the bound was built and measured. Gated
            // by proximity (a hundredth of the domain's span) and by cost in
            // space (within the geometry's own tolerance) it does exactly what
            // it should: that face's six points all come back at v = 1.000000,
            // u monotone, and the Parasolid reading's self-overlapping edges
            // fall from 87 to 11 with its distances to OpenCASCADE unchanged.
            //
            // It is still not kept. The count that ships did not move — those
            // eleven are coincidences between faces, not folds within one —
            // and the STEP reading went from no non-manifold edge to one. A
            // measure that improves what cannot be seen and worsens what can
            // is not an improvement. Looser and tighter gates were tried too:
            // no proximity gate with ten times the tolerance gives 215 open
            // half-edges, and a tenth of the tolerance leaves STEP at one and
            // takes Parasolid to twelve.
            // Branch first, so `limit_step` judges the step in the same
            // winding the boundary is already travelling in.
            if let Some(period) = domain.u_period {
                uv.u = nearest_branch(uv.u, prev_uv.u, period);
                if carry_seam && uv.u < prev_uv.u {
                    uv.u += period;
                }
            }
            if let Some(period) = domain.v_period {
                uv.v = nearest_branch(uv.v, prev_uv.v, period);
            }
            uv = limit_step(surface, prev_uv, uv, prev_p, p, tolerance);
            // And branch again, because that step may have solved afresh. A
            // solve answers with a point, not with a winding, and returns it
            // on whichever branch its own arithmetic ended on — usually the
            // principal one. Left there, a single boundary point sits a whole
            // period from its neighbours: the triangulation then joins it
            // straight across the domain, which on a cylinder is a chord
            // through the axis. Measured on the pilot assembly, 398 faces
            // carried such a chord, reaching 45 mm on a part 90 mm across,
            // while every one of them reported its boundary fully drawn.
            if let Some(period) = domain.u_period {
                uv.u = nearest_branch(uv.u, prev_uv.u, period);
                if carry_seam && uv.u < prev_uv.u {
                    uv.u += period;
                }
            }
            if let Some(period) = domain.v_period {
                uv.v = nearest_branch(uv.v, prev_uv.v, period);
            }
        }
        previous = Some((uv, p));
        out.push(uv);
    }

    snap_runs_to_the_domain_edge(surface, domain, xyz, &mut out, tolerance);
    out
}

/// Put a stretch of boundary that runs along the edge of the domain onto it.
///
/// Where a surface barely changes across the edge of its own parameter domain,
/// the parameter across that edge is ill-conditioned and the solve returns
/// noise at that scale. On one spline face of the pilot's Parasolid reading, a
/// region 0.0009 wide in `u` had its far side solved along `v = 1` as 0.9994,
/// 0.9998, 0.9983, 0.9987, 0.9999 — a wobble four times the region's whole
/// width — so the boundary crossed itself and the face lay on itself along 48
/// edges. Two of those six points came back at 1.0 exactly, which is what all
/// six mean.
///
/// The question is asked of a *run*, never of a point. A boundary lies along
/// the edge only if several points in a row do; an isolated point that happens
/// to pass the tests belongs where the solve put it, and snapping it was
/// measured to put a non-manifold edge into the STEP reading, which had none.
/// Three conditions, all of them necessary:
///
/// * the point sits within a hundredth of the domain's own span of the bound,
///   so nothing interior is ever dragged out;
/// * moving it there costs nothing in space, within the tolerance the geometry
///   is guaranteed to — the bound simply names the same point;
/// * and at least three consecutive points qualify together.
fn snap_runs_to_the_domain_edge(
    surface: &Surface,
    domain: &Domain,
    xyz: &[Vec3],
    uv: &mut [Vec2],
    tolerance: f64,
) {
    // Three, measured. Two is enough to put a non-manifold edge into the STEP
    // reading, which has none: a pair of points either side of a bin can pass
    // the tests by accident, three in a row do not.
    const RUN: usize = 3;
    let reach = (tolerance * 10.0).max(1e-9);
    let bounds: [(bool, f64, f64); 4] = [
        (true, domain.u.lo, domain.u.span()),
        (true, domain.u.hi, domain.u.span()),
        (false, domain.v.lo, domain.v.span()),
        (false, domain.v.hi, domain.v.span()),
    ];
    for (in_u, bound, span) in bounds {
        if !bound.is_finite() || bound.abs() > 1e11 || !(span > 0.0) {
            continue;
        }
        let moved_to = |q: Vec2| {
            if in_u {
                Vec2::new(bound, q.v)
            } else {
                Vec2::new(q.u, bound)
            }
        };
        let qualifies: Vec<bool> = uv
            .iter()
            .zip(xyz)
            .map(|(q, p)| {
                let here = if in_u { q.u } else { q.v };
                (here - bound).abs() <= span * 0.01
                    && (surface.point_at(moved_to(*q)) - surface.point_at(*q)).length() <= tolerance
                    && (surface.point_at(moved_to(*q)) - *p).length() <= reach
            })
            .collect();
        let mut i = 0;
        while i < qualifies.len() {
            if !qualifies[i] {
                i += 1;
                continue;
            }
            let start = i;
            while i < qualifies.len() && qualifies[i] {
                i += 1;
            }
            if i - start >= RUN {
                for q in &mut uv[start..i] {
                    *q = moved_to(*q);
                }
            }
        }
    }
}

fn limit_step(
    surface: &Surface,
    prev_uv: Vec2,
    uv: Vec2,
    prev_p: Vec3,
    p: Vec3,
    tolerance: f64,
) -> Vec2 {
    let moved = (p - prev_p).length();
    if moved <= tolerance * 10.0 {
        return uv;
    }
    let (du, dv) = surface.derivatives_at(prev_uv);
    let implied_u = du.length() * (uv.u - prev_uv.u).abs();
    let implied_v = dv.length() * (uv.v - prev_uv.v).abs();
    // A chord is at most a few percent shorter than the arc it subtends for any
    // step worth taking, so four times the distance is a wide margin around
    // anything legitimate.
    let budget = moved * 4.0 + tolerance;
    if implied_u <= budget && implied_v <= budget {
        return uv;
    }
    // The step is not credible, but the previous point's parameter is not this
    // point's answer either — handing it over merges two boundary points into
    // one and slits the face. Solve again from there instead, and keep the
    // implausible answer only if the local solve cannot reach the point at
    // all, since a parameter that at least names the right place beats one
    // that names a different point entirely.
    let reach = (tolerance * 10.0).max(1e-9);
    surface.invert_near(p, prev_uv, reach).unwrap_or(uv)
}

/// Shift `value` by whole periods to sit as close to `reference` as possible.
fn nearest_branch(value: f64, reference: f64, period: f64) -> f64 {
    if period <= 0.0 {
        return value;
    }
    value - period * ((value - reference) / period).round()
}

/// How many whole periods the loop travels in u, rounded.
fn net_wrap(uv: &[Vec2], domain: &Domain) -> i32 {
    let Some(period) = domain.u_period else {
        return 0;
    };
    if uv.len() < 2 {
        return 0;
    }
    // The chain still carries its closing point, so the travel is simply the
    // distance from the first parameter to the last.
    let travel = uv[uv.len() - 1].u - uv[0].u;
    let periods = travel / period;
    let whole = periods.round();
    // Only a near-exact whole number of turns counts. A sliver face whose
    // boundary inverts erratically can accumulate 0.9 of a period without
    // going anywhere, and treating that as a wrap sends it down the
    // seam-closing path, which then invents a boundary it does not have.
    if whole.abs() >= 1.0 && (periods - whole).abs() < 0.15 {
        whole as i32
    } else {
        0
    }
}

/// Twice the signed area of a parameter-space polygon; positive is CCW.
fn signed_area(uv: &[Vec2]) -> f64 {
    let mut a = 0.0;
    for i in 0..uv.len() {
        let p = uv[i];
        let q = uv[(i + 1) % uv.len()];
        a += p.u * q.v - q.u * p.v;
    }
    a * 0.5
}

/// Slide each hole onto the same period as the loop that encloses it.
///
/// Every loop is unwrapped on its own, so on a periodic surface nothing ties
/// them to a common branch: a bore through a cylinder can come back with its
/// rim at u ≈ 5.9 and the opening it cuts at u ≈ -0.4, a full turn apart and
/// describing the same place. The region test then reads the hole as lying
/// outside the face it belongs to and leaves the opening uncut — which is what
/// a bore, a threaded entry, or a lettered pocket looks like when it comes out
/// unmeshed. Shifting by whole periods is exact: it moves the parameter and
/// not the point it names.
fn rephase_holes(outer: &UvLoop, holes: &mut [UvLoop], domain: &Domain) {
    let mid = |lo: f64, hi: f64| (lo + hi) * 0.5;
    let span = |vals: &mut dyn Iterator<Item = f64>| {
        vals.fold((f64::INFINITY, f64::NEG_INFINITY), |(a, b), x| {
            (a.min(x), b.max(x))
        })
    };
    let (ou_lo, ou_hi) = span(&mut outer.uv.iter().map(|p| p.u));
    let (ov_lo, ov_hi) = span(&mut outer.uv.iter().map(|p| p.v));
    let (ou, ov) = (mid(ou_lo, ou_hi), mid(ov_lo, ov_hi));

    for hole in holes.iter_mut() {
        let mut shift = Vec2::default();
        if let Some(period) = domain.u_period.filter(|p| *p > 0.0) {
            let (lo, hi) = span(&mut hole.uv.iter().map(|p| p.u));
            shift.u = ((ou - mid(lo, hi)) / period).round() * period;
        }
        if let Some(period) = domain.v_period.filter(|p| *p > 0.0) {
            let (lo, hi) = span(&mut hole.uv.iter().map(|p| p.v));
            shift.v = ((ov - mid(lo, hi)) / period).round() * period;
        }
        if shift.u == 0.0 && shift.v == 0.0 {
            continue;
        }
        for p in &mut hole.uv {
            p.u += shift.u;
            p.v += shift.v;
        }
        hole.area = signed_area(&hole.uv);
    }
}

/// Move a wrapping strip's cut to somewhere no hole lies across it.
///
/// The rings run monotonically in `u` over one period; the cut is wherever
/// they begin. A hole is first brought onto the strip by whole periods, which
/// is all `rephase_holes` can do; if one still lies across an end, the strip
/// is re-cut in the largest gap between the holes and the holes brought over
/// again. Nothing moves in space — this only chooses where the seam is.
fn reseam(rings: &mut [UvLoop], holes: &mut [UvLoop], period: f64) {
    if rings.is_empty() || holes.is_empty() || !(period > 0.0) {
        return;
    }
    let span = |l: &UvLoop| {
        l.uv.iter().fold((f64::INFINITY, f64::NEG_INFINITY), |(a, b), p| {
            (a.min(p.u), b.max(p.u))
        })
    };
    let (u0, u1) = rings.iter().map(span).fold(
        (f64::INFINITY, f64::NEG_INFINITY),
        |(a, b), (c, d)| (a.min(c), b.max(d)),
    );
    if !(u0.is_finite() && u1 > u0) {
        return;
    }
    // Bring every hole onto the strip, by whole periods.
    let onto = |holes: &mut [UvLoop], lo: f64| {
        for h in holes.iter_mut() {
            let (a, _) = h.uv.iter().fold((f64::INFINITY, 0.0), |(m, _), p| (m.min(p.u), 0.0));
            let shift = ((a - lo) / period).floor() * period;
            if shift != 0.0 {
                for p in &mut h.uv {
                    p.u -= shift;
                }
            }
        }
    };
    onto(holes, u0);
    let straddles = |holes: &[UvLoop], lo: f64| {
        holes.iter().any(|h| {
            let (a, b) = span(h);
            b > lo + period + 1e-12 || a < lo - 1e-12
        })
    };
    if !straddles(holes, u0) {
        return;
    }

    // The largest stretch of the strip no hole covers, and its middle.
    let mut covered: Vec<(f64, f64)> = holes.iter().map(span).collect();
    covered.sort_by(|a, b| a.0.partial_cmp(&b.0).unwrap_or(std::cmp::Ordering::Equal));
    let (mut best_gap, mut cut) = (0.0f64, u0);
    let mut reach = u0;
    for (a, b) in covered.iter().copied().chain(std::iter::once((u0 + period, u0 + period))) {
        if a - reach > best_gap {
            best_gap = a - reach;
            cut = (reach + a) * 0.5;
        }
        reach = reach.max(b);
    }
    if best_gap <= 0.0 || cut <= u0 || cut >= u0 + period {
        return;
    }

    for r in rings.iter_mut() {
        let n = r.uv.len();
        let Some(k) = (0..n).find(|&i| r.uv[i].u >= cut) else {
            continue;
        };
        if k == 0 || r.xyz.len() != n {
            continue;
        }
        r.uv.rotate_left(k);
        r.xyz.rotate_left(k);
        for p in &mut r.uv[n - k..] {
            p.u += period;
        }
        r.area = signed_area(&r.uv);
    }
    onto(holes, cut);
}

/// Pick the outer loop by parameter-space area and return the rest as holes.
fn closed_region(loops: &mut Vec<UvLoop>) -> Result<(UvLoop, Vec<UvLoop>), String> {
    if loops.is_empty() {
        return Err("face has no usable trim loops".into());
    }
    // The outer loop is the one the others sit inside. Signed area says that
    // for loops that are simple closed curves, and stops saying it as soon as
    // one is not: a boundary whose parameter image doubles back has its own
    // area cancel, and the loop that encloses everything then scores lower
    // than the one it encloses. Containment is what is actually meant, so it
    // is asked first and the area is the tie-break.
    let boxes: Vec<(f64, f64, f64, f64)> = loops
        .iter()
        .map(|l| {
            l.uv.iter().fold(
                (f64::INFINITY, f64::NEG_INFINITY, f64::INFINITY, f64::NEG_INFINITY),
                |(a, b, c, d), p| (a.min(p.u), b.max(p.u), c.min(p.v), d.max(p.v)),
            )
        })
        .collect();
    let slack = boxes
        .iter()
        .map(|b| (b.1 - b.0).max(b.3 - b.2))
        .fold(0.0f64, f64::max)
        * 1e-9;
    let contains_all = |i: usize| {
        boxes.iter().enumerate().all(|(j, o)| {
            i == j
                || (boxes[i].0 <= o.0 + slack
                    && boxes[i].1 >= o.1 - slack
                    && boxes[i].2 <= o.2 + slack
                    && boxes[i].3 >= o.3 - slack)
        })
    };
    let enclosing: Vec<usize> = (0..loops.len()).filter(|i| contains_all(*i)).collect();
    let by_area = || {
        loops
            .iter()
            .enumerate()
            .max_by(|a, b| {
                a.1.area
                    .abs()
                    .partial_cmp(&b.1.area.abs())
                    .unwrap_or(std::cmp::Ordering::Equal)
            })
            .map(|(i, _)| i)
            .unwrap_or(0)
    };
    let outer_index = match enclosing.as_slice() {
        [only] => *only,
        _ => by_area(),
    };
    let outer = loops.remove(outer_index);
    Ok((outer, std::mem::take(loops)))
}

/// Build the region for a face whose loops wrap the periodic direction.
///
/// The file supplies no seam, so one is added here: the region is the strip
/// between the wrapping loops, closed on the left and right by segments of the
/// parameter lines `u = u₀` and `u = u₀ + period`.
fn wrapped_region(
    loops: &mut Vec<UvLoop>,
    wrapping: &[usize],
    apexes: &[Vec3],
    surface: &Surface,
    domain: &Domain,
    options: &Resolved,
    same_sense: bool,
) -> Result<(UvLoop, Vec<UvLoop>), String> {
    let Some(period) = domain.u_period else {
        return Err("a loop wraps but the surface is not periodic in u".into());
    };
    if wrapping.len() > 2 {
        return Err(format!(
            "{} loops wrap the seam; only one or two can be closed automatically",
            wrapping.len()
        ));
    }

    // Take the wrapping loops out, leaving the rest as holes.
    let mut rings: Vec<UvLoop> = Vec::new();
    for &i in wrapping.iter().rev() {
        rings.push(loops.remove(i));
    }
    let mut holes = std::mem::take(loops);

    if std::env::var_os("CAD_TESS_RAWRING").is_some() {
        if let Surface::Torus { frame, major_radius: major, minor_radius: minor } = surface {
            println!(
                "[torus] face={} major={major:.5} minor={minor:.5} origin=[{:.4},{:.4},{:.4}] axis=[{:.3},{:.3},{:.3}] ref=[{:.3},{:.3},{:.3}] same_sense={same_sense}",
                CURRENT_FACE.with(|c| c.get()),
                frame.origin.x, frame.origin.y, frame.origin.z,
                frame.axis.x, frame.axis.y, frame.axis.z,
                frame.ref_dir.x, frame.ref_dir.y, frame.ref_dir.z
            );
        }
        for (i, r) in rings.iter().enumerate() {
            println!(
                "[rawring] face={} ring {i}: {} pts wrap={} u {:.5}->{:.5} v {:.5}->{:.5}, walks [{:.4},{:.4},{:.4}] -> [{:.4},{:.4},{:.4}]",
                CURRENT_FACE.with(|c| c.get()),
                r.uv.len(), r.wrap,
                r.uv[0].u, r.uv[r.uv.len() - 1].u,
                r.uv[0].v, r.uv[r.uv.len() - 1].v,
                r.xyz[0].x, r.xyz[0].y, r.xyz[0].z,
                r.xyz[1].x, r.xyz[1].y, r.xyz[1].z
            );
        }
    }
    // Put every ring on the same branch and running the same way in u.
    let u0 = rings
        .iter()
        .flat_map(|r| r.uv.iter())
        .map(|p| p.u)
        .fold(f64::INFINITY, f64::min);
    for r in &mut rings {
        r.travel = r.wrap.signum();
        if r.wrap < 0 {
            r.uv.reverse();
            r.xyz.reverse();
            r.wrap = -r.wrap;
        }
        let shift = period * ((r.uv[0].u - u0) / period).round();
        for p in &mut r.uv {
            p.u -= shift;
        }
    }
    // Where the strip is cut is a free choice — the surface closes on itself
    // there — but not a harmless one: a hole lying across the cut is split in
    // two by it, and the halves then sit at opposite ends of the strip where
    // no triangulation can join them. So the cut is moved to somewhere no hole
    // reaches. On the pilot assembly one cylindrical face carries a hole that
    // straddles the cut in both readers, and it is the single face STEP still
    // fails to close.
    reseam(&mut rings, &mut holes, period);
    // Both rings have to be cut at the same u, or the strip is closed across
    // whatever angle separates their own starting vertices.
    // The cut is put at the first ring's first point, and the seam column
    // that closes the strip stands there. If that ring's own first step runs
    // *backwards* in u, the column is drawn through its first segment and the
    // boundary crosses itself — cone face 10 of `200 201 003-51` steps from
    // (3.9500, 3.5) to (3.9411, 2.0), 0.0089 back in u, exactly the amount
    // its strip overran the period by, and the crossing cost the face its
    // surface reading in both readers. Starting the ring one point on puts
    // the cut where the ring is already moving forward. Nothing moves in
    // space; the ring is the same ring begun at its second vertex.
    // Where the cut goes is a free choice, and two things make a choice wrong.
    //
    // The first is a ring whose own first step runs *backwards* in u: the seam
    // column is drawn through that first segment and the boundary crosses
    // itself. Cone face 10 of `200 201 003-51` steps from (3.9500, 3.5) to
    // (3.9411, 2.0), 0.0089 back in u, exactly the amount its strip overran
    // the period by, and the crossing cost the face its surface reading in
    // both readers.
    //
    // The second is a ring with a segment of its own standing at the cut —
    // two consecutive points at the same u, a step straight up or down in v.
    // The seam column stands there too, and the ring's segment is never drawn:
    // the two bolt holes of `219 204 008` are bores whose top rim is
    // castellated, three arcs at one height and three at another with six
    // 0.5 mm steps between them, and the cut landed on the last of those
    // steps. One boundary segment undrawn, three cracks, and the face lost to
    // a rebuild that spanned the bore instead of lining it.
    //
    // So the cut is tried at each of the first ring's own points in turn and
    // the first one that offends neither way is taken. Nothing is invented —
    // every cut is a point the ring already has, which is what keeps the
    // boundary shared with the neighbouring face — and a cut that is already
    // good is found at the first try, so nothing moves on a face that was
    // right.
    let slide_to_cut = |rings: &mut Vec<UvLoop>| {
        if let Some(cut) = rings.first().and_then(|r| r.uv.first()).map(|p| p.u) {
            for r in rings.iter_mut().skip(1) {
                let Some(start) = r.uv.first().map(|p| p.u) else { continue };
                let slide = cut - start;
                if slide != 0.0 {
                    for q in &mut r.uv {
                        q.u += slide;
                    }
                    r.area = signed_area(&r.uv);
                }
            }
        }
    };
    let stands_at_the_cut = |rings: &[UvLoop]| -> bool {
        let Some(cut) = rings.first().and_then(|r| r.uv.first()).map(|p| p.u) else {
            return false;
        };
        let hi = cut + period;
        let at_seam = |u: f64| (u - cut).abs() < 1e-6 || (u - hi).abs() < 1e-6;
        rings.iter().any(|r| {
            r.uv
                .windows(2)
                .any(|w| (w[1].u - w[0].u).abs() < 1e-9 && at_seam(w[0].u))
        })
    };
    // Begin a wrapping ring at its own point `k` instead of its first. The
    // points that come round to the back belong a period on, and the ring's
    // repeated closing point is rebuilt from the new start.
    fn begin_ring_at(r: &mut UvLoop, k: usize, period: f64) {
        let open = r.uv.len().saturating_sub(1);
        if k == 0 || open < 3 {
            return;
        }
        let k = k % open;
        r.uv.truncate(open);
        r.xyz.truncate(open);
        r.uv.rotate_left(k);
        r.xyz.rotate_left(k);
        for q in r.uv.iter_mut().skip(open - k) {
            q.u += period;
        }
        let closing = Vec2::new(r.uv[0].u + period, r.uv[0].v);
        r.uv.push(closing);
        r.xyz.push(r.xyz[0]);
        r.area = signed_area(&r.uv);
    }
    {
        let tries = rings.first().map(|r| r.uv.len().saturating_sub(1)).unwrap_or(0);
        let mut taken: Option<(usize, Vec<UvLoop>)> = None;
        for k in 0..tries {
            let mut trial = rings.clone();
            begin_ring_at(&mut trial[0], k, period);
            if trial[0].uv.len() > 3 && trial[0].uv[1].u < trial[0].uv[0].u {
                continue;
            }
            align_cuts(&mut trial, period);
            slide_to_cut(&mut trial);
            if !stands_at_the_cut(&trial) {
                taken = Some((k, trial));
                break;
            }
        }
        match taken {
            Some((k, trial)) => {
                if k > 0 && std::env::var_os("CAD_TESS_LOOPS").is_some() {
                    eprintln!("[strip] cut moved {k} point(s) on to clear the seam");
                }
                rings = trial;
            }
            None => {
                // No cut clears both; keep the one the ring came with, minus
                // the backward first step, which is the older rule alone.
                if let Some(r) = rings.first_mut()
                    && r.uv.len() > 3
                    && r.uv[1].u < r.uv[0].u
                {
                    begin_ring_at(r, 1, period);
                    if std::env::var_os("CAD_TESS_LOOPS").is_some() {
                        eprintln!("[strip] first ring began with a backward step; cut moved one point on");
                    }
                }
                align_cuts(&mut rings, period);
                slide_to_cut(&mut rings);
            }
        }
    }
    // Two ways of taking a ring's own seam off it were built and measured,
    // for cone face 10 of `200 201 003-51`, whose ring starts with a step
    // straight down the v-bound and ends with a run along it, and whose strip
    // then adds a seam column through the same place. Dropping the trailing
    // run changed nothing. Dropping the leading step fired on three rings,
    // left the cone exactly where it was, and took the STEP reading's points
    // over 1 mm against OpenCASCADE from 1 to 69 and the Parasolid reading's
    // from 14 to 54. The seam column is not what that face is losing to.
    if std::env::var_os("CAD_TESS_LOOPS").is_some() {
        for (i, r) in rings.iter().enumerate() {
            let (lo, hi) = r.uv.iter().fold((f64::MAX, f64::MIN), |(a, b), q| (a.min(q.u), b.max(q.u)));
            eprintln!(
                "[strip] ring {i}: {} pts, u first {:.4} last {:.4}, spans {:.4}, overruns the period by {:.4}",
                r.uv.len(), r.uv[0].u, r.uv[r.uv.len() - 1].u, hi - lo, (hi - lo) - period
            );
        }
    }
    rings.sort_by(|a, b| {
        mean_v(&a.uv)
            .partial_cmp(&mean_v(&b.uv))
            .unwrap_or(std::cmp::Ordering::Equal)
    });

    if std::env::var_os("CAD_TESS_LOOPS").is_some() {
        println!(
            "[loop]   wrapped_region on a {} with {} ring(s) and {} hole(s)",
            surface_kind(surface),
            rings.len(),
            holes.len()
        );
    }
    let (lower, upper) = match rings.len() {
        2 => {
            let mut upper = rings.pop().expect("checked length");
            let mut lower = rings.pop().expect("checked length");
            // On a surface periodic in v — a torus — two rings cut it into two
            // bands, and "between the sorted v values" picks one arbitrarily.
            // The rings themselves say which side is material: walking a
            // boundary with the surface normal up keeps the face on the left,
            // so a ring travelling +u has material above it in v and a ring
            // travelling −u has material below. The +u ring is therefore the
            // band's floor; when it sits at the higher v, the band crosses the
            // v seam and the ceiling belongs one period up.
            if let Some(pv) = domain.v_period {
                // "Material on the left" is stated against the face's own
                // normal, not the surface's. Where the two point opposite
                // ways the file's loops run the other way round, and reading
                // them as if they did not turns every such band inside out.
                let sense = if same_sense { 1 } else { -1 };
                let (mut lo_travel, mut up_travel) =
                    (lower.travel * sense, upper.travel * sense);
                if lo_travel < 0 && up_travel > 0 {
                    std::mem::swap(&mut lower, &mut upper);
                    std::mem::swap(&mut lo_travel, &mut up_travel);
                }
                let lifted = lo_travel > 0 && up_travel < 0 && mean_v(&lower.uv) > mean_v(&upper.uv);
                if lifted {
                    for p in &mut upper.uv {
                        p.v += pv;
                    }
                }
                if std::env::var_os("CAD_TESS_BAND").is_some() {
                    println!(
                        "[band] face={} same_sense={same_sense} travel lower {} upper {} (as the face sees them {lo_travel} / {up_travel}), mean v {:.5} / {:.5}, period {pv:.5}, ceiling lifted={lifted}, band spans {:.5} ({:.1} % of the period); lower walks [{:.4},{:.4},{:.4}]->[{:.4},{:.4},{:.4}], upper walks [{:.4},{:.4},{:.4}]->[{:.4},{:.4},{:.4}]",
                        CURRENT_FACE.with(|c| c.get()),
                        lower.travel, upper.travel,
                        mean_v(&lower.uv), mean_v(&upper.uv),
                        (mean_v(&upper.uv) - mean_v(&lower.uv)).abs(),
                        100.0 * (mean_v(&upper.uv) - mean_v(&lower.uv)).abs() / pv,
                        lower.xyz[0].x, lower.xyz[0].y, lower.xyz[0].z,
                        lower.xyz[1].x, lower.xyz[1].y, lower.xyz[1].z,
                        upper.xyz[0].x, upper.xyz[0].y, upper.xyz[0].z,
                        upper.xyz[1].x, upper.xyz[1].y, upper.xyz[1].z
                    );
                }
            }
            (lower, upper)
        }
        1 => {
            // One ring, so the face runs from it to a point. Close it only
            // where the file says so: a single-vertex bound is that statement.
            // In the pilot assembly just 18 of 1423 conical and spherical faces
            // carry one while 114 reach this branch, and inferring the rest
            // from geometry alone fabricated a fan of long thin triangles
            // reaching to an apex the part does not have.
            let ring = rings.pop().expect("checked length");
            let apex = nearest_apex(apexes, &ring).or_else(|| {
                // No declared apex — Parasolid has no vertex-loop concept, so
                // its cone-to-a-point faces arrive as a single ring. Closing
                // onto the surface's own degenerate point is legitimate when
                // that point is FINITE and within reach of the ring: the
                // domain edge where a cone's radius hits zero is its apex.
                // The reach bound is what stops a shallow cone running to an
                // apex far outside the part (the spike bug of the STEP pilot).
                let v_mid = mean_v(&ring.uv);
                let toward_high = (domain.v.hi - v_mid).abs() < (v_mid - domain.v.lo).abs();
                let v_edge = if toward_high { domain.v.hi } else { domain.v.lo };
                if !v_edge.is_finite() || v_edge.abs() > 1e11 {
                    return None;
                }
                let candidate = surface.point_at(Vec2::new(ring.uv[0].u, v_edge));
                let centre = ring.xyz.iter().fold(Vec3::ZERO, |a, p| a + *p)
                    * (1.0 / ring.xyz.len().max(1) as f64);
                let girth = ring
                    .xyz
                    .iter()
                    .map(|p| (*p - centre).length())
                    .fold(0.0f64, f64::max);
                ((candidate - centre).length() <= girth * 8.0 + options.sag * 16.0)
                    .then_some(candidate)
            });
            let Some(apex) = apex else {
                return Err(format!(
                    "one wrapping loop, no declared apex, and no finite domain \
                     edge within reach ({} degenerate bounds declared)",
                    apexes.len()
                ));
            };
            let Some(apex_uv) = surface.invert(apex, ring.uv.first().copied()) else {
                return Err("the apex does not invert onto the surface".into());
            };
            if std::env::var_os("CAD_TESS_APEX").is_some() {
                let centre = ring.xyz.iter().fold(Vec3::ZERO, |a, p| a + *p)
                    * (1.0 / ring.xyz.len().max(1) as f64);
                let girth = ring
                    .xyz
                    .iter()
                    .map(|p| (*p - centre).length())
                    .fold(0.0f64, f64::max);
                println!(
                    "[apex] {} ring {} pts girth {:.3} apex [{:.3},{:.3},{:.3}] reach {:.3}                      declared {}",
                    surface_kind(surface),
                    ring.xyz.len(),
                    girth,
                    apex.x,
                    apex.y,
                    apex.z,
                    (apex - centre).length(),
                    apexes.len(),
                );
            }
            let edge_ring = pole_ring(&ring, apex, apex_uv.v);
            if apex_uv.v > mean_v(&ring.uv) {
                (ring, edge_ring)
            } else {
                (edge_ring, ring)
            }
        }
        _ => return Err("no wrapping loop after filtering".into()),
    };

    let strip = close_strip(lower, upper, period, surface, options);
    Ok((strip, holes))
}

/// The boundary of a face that trims nothing: the whole parameter rectangle.
///
/// Only possible when both directions are bounded, which in practice means a
/// closed surface — a sphere or a torus. The `u = u_hi` column is evaluated at
/// `u_lo`, so the seam it introduces is closed to the bit.
fn full_domain_loop(
    surface: &Surface,
    domain: &Domain,
    options: &Resolved,
) -> Result<UvLoop, String> {
    let bounded = |i: cad_ir::math::Interval| {
        i.span().is_finite() && i.span().abs() < 1e9 && i.span() > 0.0
    };
    if !bounded(domain.u) || !bounded(domain.v) {
        return Err("face has no trim loops and its surface is unbounded".into());
    }

    let (u_lo, u_hi) = (domain.u.lo, domain.u.hi);
    let (v_lo, v_hi) = (domain.v.lo, domain.v.hi);
    let nu = direction_steps(surface, domain, Axis::U, u_lo, u_hi, 0.5 * (v_lo + v_hi), options)
        .clamp(3, 256);
    let nv = direction_steps(surface, domain, Axis::V, v_lo, v_hi, 0.5 * (u_lo + u_hi), options)
        .clamp(3, 256);

    // The same seeding every other line on a surface gets: an even count
    // cannot see shape it steps over, and this one is clamped at 256 besides.
    // A face with no trim loops is usually a whole sphere or torus, where a
    // spline's breaks do not arise — but where they do, the boundary of the
    // whole domain is as entitled to them as any other line.
    let u_line = |t: f64| surface.point_at(Vec2::new(t, 0.5 * (v_lo + v_hi)));
    let v_line = |t: f64| surface.point_at(Vec2::new(0.5 * (u_lo + u_hi), t));
    let us = knots::merge_even(
        &knots::thin_breaks(
            &knots::surface_breaks(surface, Axis::U, u_lo, u_hi),
            u_lo,
            u_hi,
            options.sag,
            &u_line,
        ),
        u_lo,
        u_hi,
        nu,
    );
    let vs = knots::merge_even(
        &knots::thin_breaks(
            &knots::surface_breaks(surface, Axis::V, v_lo, v_hi),
            v_lo,
            v_hi,
            options.sag,
            &v_line,
        ),
        v_lo,
        v_hi,
        nv,
    );

    let mut uv = Vec::with_capacity(2 * (us.len() + vs.len()));
    let mut xyz = Vec::with_capacity(uv.capacity());
    // The u period, when there is one, is exactly the domain width here.
    let wrap_u = |u: f64| if domain.u_period.is_some() && u >= u_hi { u_lo } else { u };
    let wrap_v = |v: f64| if domain.v_period.is_some() && v >= v_hi { v_lo } else { v };

    let push = |u: f64, v: f64, uv: &mut Vec<Vec2>, xyz: &mut Vec<Vec3>| {
        uv.push(Vec2::new(u, v));
        xyz.push(surface.point_at(Vec2::new(wrap_u(u), wrap_v(v))));
    };

    for &u in us.iter().take(us.len() - 1) {
        push(u, v_lo, &mut uv, &mut xyz);
    }
    for &v in vs.iter().take(vs.len() - 1) {
        push(u_hi, v, &mut uv, &mut xyz);
    }
    for &u in us.iter().skip(1).rev() {
        push(u, v_hi, &mut uv, &mut xyz);
    }
    for &v in vs.iter().skip(1).rev() {
        push(u_lo, v, &mut uv, &mut xyz);
    }

    Ok(UvLoop {
        area: signed_area(&uv),
        wrap: 0,
        travel: 0,
        uv,
        xyz,
    })
}

fn mean_v(uv: &[Vec2]) -> f64 {
    if uv.is_empty() {
        return 0.0;
    }
    uv.iter().map(|p| p.v).sum::<f64>() / uv.len() as f64
}

/// The declared apex nearest a ring, if one is close enough to be its pole.
///
/// A part may declare several degenerate points; the one belonging to this ring
/// is the one within reach of it. A cone's apex sits within a few radii of its
/// base, so anything further away belongs to a different feature.
fn nearest_apex(apexes: &[Vec3], ring: &UvLoop) -> Option<Vec3> {
    if ring.xyz.is_empty() {
        return None;
    }
    let centre = ring.xyz.iter().fold(Vec3::ZERO, |a, p| a + *p) * (1.0 / ring.xyz.len() as f64);
    let radius = ring
        .xyz
        .iter()
        .map(|p| (*p - centre).length())
        .fold(0.0f64, f64::max);
    apexes
        .iter()
        .copied()
        .map(|a| ((a - centre).length(), a))
        .filter(|(d, _)| *d <= radius * 16.0 + 1e-6)
        .min_by(|a, b| a.0.partial_cmp(&b.0).unwrap_or(std::cmp::Ordering::Equal))
        .map(|(_, a)| a)
}

/// The pole as a parameter-space line at constant `v`, every point of which is
/// the same 3D apex.
///
/// The position comes from the file's own vertex rather than from evaluating
/// the surface at a guessed parameter, so the fan converges exactly where the
/// model says it does.
fn pole_ring(like: &UvLoop, apex: Vec3, v: f64) -> UvLoop {
    let uv: Vec<Vec2> = like.uv.iter().map(|p| Vec2::new(p.u, v)).collect();
    let xyz: Vec<Vec3> = vec![apex; uv.len()];
    UvLoop {
        area: signed_area(&uv),
        wrap: like.wrap,
        travel: like.travel,
        uv,
        xyz,
    }
}

/// Join two rings into one closed boundary with seam segments at both ends.
/// Cut every ring of a strip at the same place in u.
///
/// A ring bounding a periodic face is a closed curve, and where the file began
/// it is wherever its own vertex happens to sit. Two rings can therefore start
/// a quarter turn apart, and the strip built from them is closed by a boundary
/// segment running that quarter turn straight across — on a cylinder, a chord
/// through the part. Measured on the pilot assembly, 398 faces carried such a
/// chord, reaching 45 mm on a part 90 mm across, and every one of them
/// reported its boundary fully drawn: nothing else in the tessellator can see
/// this, because the boundary really is complete. It is simply cut wrong.
///
/// Each ring is rotated to whichever of its own points lies nearest the shared
/// cut. No point is invented — a boundary vertex is shared with the face
/// across the edge, and one inserted on this side alone is a crack — so the
/// rings still begin up to one sampling step apart, which is the same
/// magnitude as the sampling itself.
fn align_cuts(rings: &mut [UvLoop], period: f64) {
    if !(period > 0.0) {
        return;
    }
    let Some(target) = rings.first().and_then(|r| r.uv.first()).map(|p| p.u) else {
        return;
    };
    // How far a parameter sits from the cut, ignoring whole periods.
    let offset = |u: f64| {
        let x = (u - target) / period;
        (x - x.round()).abs()
    };
    for r in rings.iter_mut().skip(1) {
        let n = r.uv.len();
        if n < 3 || r.xyz.len() != n {
            continue;
        }
        // The ring carries its closing point: the last entry repeats the first
        // a period on, and rotating has to leave it that way.
        let open = n - 1;
        let Some(start) = (0..open).min_by(|&a, &b| {
            offset(r.uv[a].u)
                .partial_cmp(&offset(r.uv[b].u))
                .unwrap_or(std::cmp::Ordering::Equal)
        }) else {
            continue;
        };
        r.uv.truncate(open);
        r.xyz.truncate(open);
        r.uv.rotate_left(start);
        r.xyz.rotate_left(start);
        // What wrapped round to the back belongs a period on from where it was.
        for p in &mut r.uv[open - start..] {
            p.u += period;
        }
        // Put the whole ring on the branch the cut is on, then close it.
        let shift = period * ((r.uv[0].u - target) / period).round();
        for p in &mut r.uv {
            p.u -= shift;
        }
        let closing = Vec2::new(r.uv[0].u + period, r.uv[0].v);
        r.uv.push(closing);
        r.xyz.push(r.xyz[0]);
        r.area = signed_area(&r.uv);
    }
}

fn close_strip(
    lower: UvLoop,
    upper: UvLoop,
    period: f64,
    surface: &Surface,
    options: &Resolved,
) -> UvLoop {
    let u_lo = lower.uv[0].u;
    let u_hi = u_lo + period;
    if std::env::var_os("CAD_TESS_SEAM").is_some() {
        let flat = |r: &UvLoop| {
            let n = r.uv.len();
            (0..n)
                .filter(|&i| (r.uv[(i + 1) % n].u - r.uv[i].u).abs() < 1e-9)
                .map(|i| format!("{i}@u={:.5}", r.uv[i].u))
                .collect::<Vec<_>>()
                .join(" ")
        };
        println!(
            "[seam] face={} cut u_lo={u_lo:.5} u_hi={u_hi:.5}; lower {} pts first u {:.5} last {:.5}, constant-u steps [{}]; upper {} pts first u {:.5} last {:.5}, constant-u steps [{}]",
            CURRENT_FACE.with(|c| c.get()),
            lower.uv.len(), lower.uv[0].u, lower.uv[lower.uv.len() - 1].u, flat(&lower),
            upper.uv.len(), upper.uv[0].u, upper.uv[upper.uv.len() - 1].u, flat(&upper),
        );
    }

    let mut uv = Vec::with_capacity(lower.uv.len() + upper.uv.len() + 8);
    let mut xyz = Vec::with_capacity(uv.capacity());

    // Along the lower ring, left to right.
    for (p, q) in lower.uv.iter().zip(&lower.xyz) {
        uv.push(*p);
        xyz.push(*q);
    }
    // Close the lower ring onto the right-hand seam.
    let lower_end = Vec2::new(u_hi, lower.uv[0].v);
    uv.push(lower_end);
    xyz.push(lower.xyz[0]);

    // Up the right-hand seam, evaluated on the left so the two columns agree.
    seam_segment(
        &mut uv,
        &mut xyz,
        surface,
        u_hi,
        u_lo,
        lower_end.v,
        upper.uv[0].v,
        options,
    );

    // Back along the upper ring, right to left.
    let upper_start = Vec2::new(u_hi, upper.uv[0].v);
    uv.push(upper_start);
    xyz.push(upper.xyz[0]);
    for (p, q) in upper.uv.iter().zip(&upper.xyz).rev() {
        if uv.last().is_some_and(|l| (l.u - p.u).abs() < 1e-15 && (l.v - p.v).abs() < 1e-15) {
            continue;
        }
        uv.push(*p);
        xyz.push(*q);
    }

    // Down the left-hand seam, the same points in the opposite order.
    seam_segment(
        &mut uv,
        &mut xyz,
        surface,
        u_lo,
        u_lo,
        upper.uv[0].v,
        lower.uv[0].v,
        options,
    );

    // The rings arrive carrying their own closing point — the repeat of the
    // first, a period on — and the seam columns add their ends on top of it,
    // so the strip comes out with the same 3D point twice in a row at each
    // corner. A repeated boundary point is a zero-length segment, and the
    // triangle built on it has no area: it is dropped where triangles are
    // emitted, and dropping it leaves the two edges it carried without a
    // partner. Nothing inside the face can see that — the boundary really was
    // drawn — but the finished mesh is open along it, and on the pilot
    // assembly this was most of 1,820 open half-edges against OpenCASCADE's
    // 113. The polygon closes last-to-first implicitly, so a trailing repeat
    // of the first point is redundant in the same way.
    let mut cut = 0;
    let mut i = 1;
    while i < xyz.len() {
        if (xyz[i] - xyz[i - 1 - cut]).length_squared() <= 0.0 {
            cut += 1;
        } else if cut > 0 {
            xyz[i - cut] = xyz[i];
            uv[i - cut] = uv[i];
        }
        i += 1;
    }
    xyz.truncate(xyz.len() - cut);
    uv.truncate(uv.len() - cut);
    while xyz.len() > 3 && (xyz[xyz.len() - 1] - xyz[0]).length_squared() <= 0.0 {
        xyz.pop();
        uv.pop();
    }

    UvLoop {
        area: signed_area(&uv),
        wrap: 0,
        travel: 0,
        uv,
        xyz,
    }
}

/// Interior points along a seam line, so the strip's ends are not one long edge.
///
/// `u` places the points in parameter space; `u_eval` is where they are
/// evaluated. The two differ by exactly one period on the right-hand seam, and
/// keeping the evaluation canonical is what makes the two columns of the strip
/// bit-identical in 3D — without it the seam is a hairline crack.
fn seam_segment(
    uv: &mut Vec<Vec2>,
    xyz: &mut Vec<Vec3>,
    surface: &Surface,
    u: f64,
    u_eval: f64,
    v_from: f64,
    v_to: f64,
    options: &Resolved,
) {
    let steps = v_steps(surface, u_eval, v_from, v_to, options);
    // A strip is closed by walking its seam up one side and back down the
    // other, and the two walks have to land on the same points to the bit or
    // the mesh does not close along the seam. Interpolating from whichever end
    // the walk starts at does not: `a + (b − a)·t` and `b + (a − b)·(1 − t)`
    // agree in exact arithmetic and differ in the last place in f64. So the
    // parameters are always built from the lower end and simply read backwards
    // for the descending walk.
    let ascending = v_from <= v_to;
    let (lo, hi) = if ascending { (v_from, v_to) } else { (v_to, v_from) };

    // The seam is a line on the surface like any other, and it needs the same
    // seeding: an even count cannot see shape it steps over, and `v_steps`
    // stops doubling at 256. The pilot's spring runs 594 knot spans along its
    // helix, so its seam column had fewer than half the points its interior
    // grid had — and the triangulation bridged the gaps with a fan from the
    // seam's corner reaching a twentieth of the way along the spring. Looked
    // at down the axis, that fan is a cone spanning the inside of the coil.
    let along = |t: f64| surface.point_at(Vec2::new(u_eval, t));
    let breaks = crate::knots::thin_breaks(
        &crate::knots::surface_breaks(surface, crate::knots::Axis::V, lo, hi),
        lo,
        hi,
        options.sag,
        &along,
    );
    let ladder = crate::knots::merge_even(&breaks, lo, hi, steps);

    // Both walks read the same ladder, forwards or backwards, so the two
    // columns stay bit-identical.
    let inner = ladder.len().saturating_sub(2);
    for i in 0..inner {
        let v = if ascending {
            ladder[i + 1]
        } else {
            ladder[inner - i]
        };
        uv.push(Vec2::new(u, v));
        xyz.push(surface.point_at(Vec2::new(u_eval, v)));
    }
}

/// How many segments the seam needs, from the surface's curvature along v.
fn v_steps(surface: &Surface, u: f64, v_from: f64, v_to: f64, options: &Resolved) -> usize {
    match surface {
        // Straight in v; one segment is exact.
        Surface::Cylinder { .. } | Surface::Cone { .. } | Surface::Plane { .. } => {
            options.min_edge_segments.max(1)
        }
        Surface::Sphere { radius, .. } => options.segments_for_arc(*radius, v_to - v_from),
        Surface::Torus { minor_radius, .. } => {
            options.segments_for_arc(*minor_radius, v_to - v_from)
        }
        _ => {
            // Sample the isoparametric line and count what the sag needs.
            let probe = |t: f64| surface.point_at(Vec2::new(u, v_from + (v_to - v_from) * t));
            adaptive_steps(&probe, options)
        }
    }
}

/// Segments an arbitrary parameterised path needs to stay within the sag.
fn adaptive_steps(probe: &dyn Fn(f64) -> Vec3, options: &Resolved) -> usize {
    let mut n = options.min_edge_segments.max(1);
    while n < 256 {
        let mut worst: f64 = 0.0;
        let mut turn: f64 = 0.0;
        let mut previous: Option<Vec3> = None;
        for i in 0..n {
            let a = probe(i as f64 / n as f64);
            let b = probe((i + 1) as f64 / n as f64);
            let m = probe((i as f64 + 0.5) / n as f64);
            worst = worst.max((m - (a + b) * 0.5).length());
            // How far the surface turns from one step to the next. A chord
            // criterion alone cannot see a small feature inside a large face:
            // a spring's wire is 1.2 mm thick on a helix 43 mm long, and the
            // face is held to a fraction of *its* size, so seven segments
            // satisfy the chord and the wire comes out a heptagon. This is the
            // same limit `segments_for_arc` puts on a cylinder or a cone; it
            // was simply never applied to a spline.
            if let Some(before) = previous
                && let (Some(x), Some(y)) = (before.try_normalized(), (b - a).try_normalized())
            {
                turn = turn.max(x.dot(y).clamp(-1.0, 1.0).acos());
            }
            previous = Some(b - a);
        }
        if worst <= options.sag && turn <= options.angle {
            break;
        }
        n *= 2;
    }
    n
}

/// The 3D length a unit step in each parameter direction covers, at the middle
/// of the domain.
fn parameter_scale(surface: &Surface, domain: &Domain) -> Vec2 {
    let mid = Vec2::new(
        clamp_finite(domain.u.at(0.5)),
        clamp_finite(domain.v.at(0.5)),
    );
    let (du, dv) = surface.derivatives_at(mid);
    let su = du.length();
    let sv = dv.length();
    // A degenerate direction — a sphere's pole — would scale to zero and
    // collapse the triangulation onto a line.
    Vec2::new(
        if su.is_finite() && su > 1e-12 { su } else { 1.0 },
        if sv.is_finite() && sv > 1e-12 { sv } else { 1.0 },
    )
}

fn clamp_finite(v: f64) -> f64 {
    if v.is_finite() { v.clamp(-1e9, 1e9) } else { 0.0 }
}

/// Fill the region and lift it to 3D.
#[allow(clippy::too_many_arguments)]
fn triangulate(
    surface: &Surface,
    domain: &Domain,
    outer: UvLoop,
    holes: Vec<UvLoop>,
    scale: Vec2,
    options: &Resolved,
    same_sense: bool,
    file_rings: &[Vec<Vec3>],
    carry_seam: bool,
) -> Result<Patch, String> {
    let rings: Vec<&[Vec3]> = if file_rings.is_empty() {
        std::iter::once(outer.xyz.as_slice())
            .chain(holes.iter().map(|h| h.xyz.as_slice()))
            .collect()
    } else {
        file_rings.iter().map(|r| r.as_slice()).collect()
    };
    let cracks = |p: &Patch| boundary_gaps(p, &rings) + interior_holes(p, &rings);

    let build = |split: bool| {
        triangulate_region(
            surface,
            domain,
            outer.clone(),
            holes.clone(),
            scale,
            options,
            same_sense,
            split,
        )
    };
    // How far a patch reaches outside the box its own boundary spans. A face
    // is drawn between its edges; a reading that wanders far outside them is
    // drawing something else.
    let bounds = {
        let mut b = cad_ir::math::Aabb::EMPTY;
        for p in outer.xyz.iter().chain(holes.iter().flat_map(|h| h.xyz.iter())) {
            b.add_point(*p);
        }
        b
    };
    let stray = |p: &Patch| {
        let g = |v: Vec3, i: usize| [v.x, v.y, v.z][i];
        p.positions.iter().fold(0.0f64, |m, q| {
            let w = Vec3::new(q[0] as f64, q[1] as f64, q[2] as f64);
            (0..3).fold(m, |m, k| {
                m.max(g(bounds.min, k) - g(w, k)).max(g(w, k) - g(bounds.max, k))
            })
        })
    };

    if std::env::var("CAD_TESS_FACE").ok().and_then(|v| v.parse::<u32>().ok())
        == Some(CURRENT_FACE.with(|c| c.get()))
    {
        println!(
            "[uv-dump] face={} outer {} pts wrap={} area={:.6} domain u [{:.4},{:.4}] period {:?} v [{:.4},{:.4}]",
            CURRENT_FACE.with(|c| c.get()),
            outer.uv.len(),
            outer.wrap,
            outer.area,
            domain.u.lo, domain.u.hi, domain.u_period, domain.v.lo, domain.v.hi
        );
        for (i, (q, x)) in outer.uv.iter().zip(&outer.xyz).enumerate() {
            let back = surface.point_at(*q);
            println!(
                "            {i:3} u {:.5} v {:.5}   boundary [{:.4},{:.4},{:.4}]  surface says [{:.4},{:.4},{:.4}]  off by {:.4}",
                q.u, q.v, x.x, x.y, x.z, back.x, back.y, back.z, (back - *x).length()
            );
        }
    }
    let refused = build(false);
    // The other reading is worth building where the first left something open
    // — which is exactly where a crossing was refused — and also where the
    // first stays closed but wanders.
    //
    // A crack-free reading is not therefore the right one. Face 1341 of
    // `200 201 003-51` is a strip of a 2 mm surface of revolution, a quarter
    // of the way round it, and its boundary straddles the seam. Refusing the
    // crossing gives a reading that closes its boundary perfectly and then
    // draws the *complement*: the whole turn, a 4 mm tube 16.6 mm long
    // standing 3.9 mm proud of the wall — 525 triangles of a blade the part
    // does not have, and 324 mm³ of material neither the STEP reading nor
    // OpenCASCADE has. It was taken because it had no cracks and the other
    // reading was never built.
    //
    // A tenth of the boundary's own diagonal is the trigger. A legitimate
    // patch bulges outside its boundary by its sagitta, which is a tolerance;
    // a hemisphere, the roundest thing a boundary can bound, stands one radius
    // proud — half its rim's diagonal. A tenth leaves the fast path to
    // everything ordinary and asks the question where it is worth asking.
    let wanders = refused
        .as_ref()
        .ok()
        .is_some_and(|p| stray(p) > bounds.diagonal() * 0.1);
    let patch = match &refused {
        Ok(p) if cracks(p) == 0 && !wanders => refused,
        _ => match (&refused, build(true)) {
            (Ok(a), Ok(b)) if cracks(&b) < cracks(a) => Ok(b),
            // Equally closed: the one that stays nearest its own boundary.
            (Ok(a), Ok(b)) if cracks(&b) == cracks(a) && stray(&b) < stray(a) => Ok(b),
            (Err(_), Ok(b)) => Ok(b),
            _ => refused,
        },
    }?;

    triangulate_compare(
        patch,
        surface,
        &outer,
        &holes,
        options,
        same_sense,
        &rings,
        carry_seam,
    )
}

/// Triangulate the face's parameter region.
///
/// `split_crossings` decides what happens where the boundary's image crosses
/// itself. Splitting there invents a point the neighbouring face has never
/// heard of, which is a seam; refusing leaves the segment unenforced, which
/// lets the region fill walk out through it and tear the middle out of the
/// face. Neither is right in general — on the pilot assembly refusing costs
/// one face two undrawn segments and nine torn ones, splitting costs it two
/// seams — so both are built and the caller keeps whichever leaves less open.
/// Set once the first bad face has been dumped, so the trace is one face and
/// not eleven thousand.
static DUMPED: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

/// Split the triangles that left the surface, and only those.
///
/// A parameter grid can only be told to be finer *everywhere*, and on a face
/// whose parameterisation is even for its whole length and loses it in the last
/// hundredth — a sweep running out, a spline narrowing to a cusp — that is a
/// demand no face can pay: the pilot's spring would need sixteen hundred lines
/// where ninety-six serve almost all of it. Six ways of sizing the grid for it
/// were built and measured, and every one changed nothing or made the assembly
/// worse.
///
/// A finished triangulation can be asked a better question: which of *its own*
/// edges left the surface. Only those are split, each gaining a vertex at the
/// surface point of its parameter midpoint, and each triangle is rebuilt from
/// however many of its three edges were split — one gives two triangles, two
/// give three, three give four. That is red-green refinement, and it is what
/// OpenCASCADE's mesher does under `ControlSurfaceDeflection`.
///
/// A boundary edge is never split. Its points are shared with the face across
/// it, which has no way of knowing this side added one, and a vertex in the
/// middle of the neighbour's edge is a crack.
fn refine_patch(
    patch: &mut Patch,
    parameters: &mut Vec<Vec2>,
    on_boundary: &[bool],
    surface: &Surface,
    options: &Resolved,
    same_sense: bool,
) {
    if !options.interior_points || matches!(surface, Surface::Plane { .. }) {
        return;
    }
    if patch.indices.is_empty() || parameters.len() != patch.positions.len() {
        return;
    }
    let boundary = |i: u32| on_boundary.get(i as usize).copied().unwrap_or(true);
    let ceiling = patch.indices.len() * 4 + 4096;

    for _ in 0..3 {
        let mut split: rustc_hash::FxHashMap<(u32, u32), u32> = Default::default();
        let mut skipped = 0usize;
        for tri in patch.indices.chunks_exact(3) {
            for k in 0..3 {
                let (a, b) = (tri[k], tri[(k + 1) % 3]);
                if boundary(a) && boundary(b) {
                    skipped += 1;
                    continue;
                }
                let key = if a < b { (a, b) } else { (b, a) };
                if split.contains_key(&key) {
                    continue;
                }
                let (ua, ub) = (parameters[a as usize], parameters[b as usize]);
                let at = |i: u32| {
                    let q = patch.positions[i as usize];
                    Vec3::new(q[0] as f64, q[1] as f64, q[2] as f64)
                };
                let mid = Vec2::new(0.5 * (ua.u + ub.u), 0.5 * (ua.v + ub.v));
                let chord = (at(a) + at(b)) * 0.5;
                let on_surface = surface.point_at(mid);
                if (on_surface - chord).length() <= options.sag {
                    continue;
                }
                let mut n = surface.normal_at(mid);
                if !same_sense {
                    n = -n;
                }
                let index = patch.positions.len() as u32;
                patch.positions.push([
                    on_surface.x as f32,
                    on_surface.y as f32,
                    on_surface.z as f32,
                ]);
                patch.normals.push([n.x as f32, n.y as f32, n.z as f32]);
                parameters.push(mid);
                split.insert(key, index);
            }
        }
        if std::env::var_os("CAD_TESS_REFINE").is_some() {
            println!(
                "[refine] {} split {} edges of {} triangles, {skipped} boundary edges passed over",
                surface_kind(surface),
                split.len(),
                patch.indices.len() / 3
            );
        }
        if split.is_empty() {
            return;
        }

        let mut out: Vec<u32> = Vec::with_capacity(patch.indices.len() * 2);
        for tri in patch.indices.chunks_exact(3) {
            let cut = |k: usize| -> Option<u32> {
                let (a, b) = (tri[k], tri[(k + 1) % 3]);
                let key = if a < b { (a, b) } else { (b, a) };
                split.get(&key).copied()
            };
            let m = [cut(0), cut(1), cut(2)];
            let (v0, v1, v2) = (tri[0], tri[1], tri[2]);
            match (m[0], m[1], m[2]) {
                (None, None, None) => out.extend_from_slice(&[v0, v1, v2]),
                // One split: a triangle either side of the new vertex.
                (Some(p), None, None) => {
                    out.extend_from_slice(&[v0, p, v2]);
                    out.extend_from_slice(&[p, v1, v2]);
                }
                (None, Some(p), None) => {
                    out.extend_from_slice(&[v1, p, v0]);
                    out.extend_from_slice(&[p, v2, v0]);
                }
                (None, None, Some(p)) => {
                    out.extend_from_slice(&[v2, p, v1]);
                    out.extend_from_slice(&[p, v0, v1]);
                }
                // Two split: the corner between them, then the quad the other
                // two corners and the two new vertices make.
                (Some(p), Some(q), None) => {
                    out.extend_from_slice(&[v1, q, p]);
                    out.extend_from_slice(&[v0, p, q]);
                    out.extend_from_slice(&[v0, q, v2]);
                }
                (None, Some(q), Some(r)) => {
                    out.extend_from_slice(&[v2, r, q]);
                    out.extend_from_slice(&[v1, q, r]);
                    out.extend_from_slice(&[v1, r, v0]);
                }
                (Some(p), None, Some(r)) => {
                    out.extend_from_slice(&[v0, p, r]);
                    out.extend_from_slice(&[v2, r, p]);
                    out.extend_from_slice(&[v2, p, v1]);
                }
                // All three: the middle triangle and one at each corner.
                (Some(p), Some(q), Some(r)) => {
                    out.extend_from_slice(&[v0, p, r]);
                    out.extend_from_slice(&[p, v1, q]);
                    out.extend_from_slice(&[r, q, v2]);
                    out.extend_from_slice(&[p, q, r]);
                }
            }
        }
        patch.indices = out;
        if patch.indices.len() > ceiling {
            return;
        }
    }

    // A triangle whose three edges all hug the surface can still have its
    // middle far from it: the surface inside it can bow or twist without
    // moving its sides. Splitting such a triangle at its own centre fixes that
    // and touches no edge at all, so it cannot crack anything — not the
    // neighbouring triangle, which keeps its edges, and certainly not the
    // neighbouring face. Measured on the pilot, the spring's ends deviate this
    // way and no amount of edge refinement reaches them.
    for _ in 0..3 {
        let mut out: Vec<u32> = Vec::with_capacity(patch.indices.len());
        let mut added = 0usize;
        for tri in patch.indices.chunks_exact(3) {
            let at = |i: u32| {
                let q = patch.positions[i as usize];
                Vec3::new(q[0] as f64, q[1] as f64, q[2] as f64)
            };
            let uv = |i: u32| parameters[i as usize];
            let middle = Vec2::new(
                (uv(tri[0]).u + uv(tri[1]).u + uv(tri[2]).u) / 3.0,
                (uv(tri[0]).v + uv(tri[1]).v + uv(tri[2]).v) / 3.0,
            );
            let flat = (at(tri[0]) + at(tri[1]) + at(tri[2])) * (1.0 / 3.0);
            let on_surface = surface.point_at(middle);
            if (on_surface - flat).length() <= options.sag || patch.indices.len() + added * 6 > ceiling
            {
                out.extend_from_slice(tri);
                continue;
            }
            let mut n = surface.normal_at(middle);
            if !same_sense {
                n = -n;
            }
            let c = patch.positions.len() as u32;
            patch.positions.push([
                on_surface.x as f32,
                on_surface.y as f32,
                on_surface.z as f32,
            ]);
            patch.normals.push([n.x as f32, n.y as f32, n.z as f32]);
            parameters.push(middle);
            out.extend_from_slice(&[tri[0], tri[1], c]);
            out.extend_from_slice(&[tri[1], tri[2], c]);
            out.extend_from_slice(&[tri[2], tri[0], c]);
            added += 1;
        }
        patch.indices = out;
        if added == 0 {
            return;
        }
    }
}

fn triangulate_region(
    surface: &Surface,
    domain: &Domain,
    outer: UvLoop,
    holes: Vec<UvLoop>,
    scale: Vec2,
    options: &Resolved,
    same_sense: bool,
    split_crossings: bool,
) -> Result<Patch, String> {
    let mut cdt: ConstrainedDelaunayTriangulation<Point2<f64>> =
        ConstrainedDelaunayTriangulation::new();

    // Parameter points, their exact 3D positions where one is known, and the
    // handle spade gave them.
    let mut uv_of: Vec<Vec2> = Vec::new();
    let mut xyz_of: Vec<Option<Vec3>> = Vec::new();

    let insert = |cdt: &mut ConstrainedDelaunayTriangulation<Point2<f64>>,
                      uv_of: &mut Vec<Vec2>,
                      xyz_of: &mut Vec<Option<Vec3>>,
                      uv: Vec2,
                      xyz: Option<Vec3>|
     -> Option<usize> {
        let p = Point2::new(uv.u * scale.u, uv.v * scale.v);
        if !p.x.is_finite() || !p.y.is_finite() {
            return None;
        }
        let handle = cdt.insert(p).ok()?;
        let index = handle.index();
        if index >= uv_of.len() {
            uv_of.resize(index + 1, Vec2::default());
            xyz_of.resize(index + 1, None);
        }
        uv_of[index] = uv;
        // A boundary point's cached position wins; a later interior insertion
        // that lands on the same handle must not overwrite it.
        if xyz_of[index].is_none() {
            xyz_of[index] = xyz;
        }
        Some(index)
    };

    let skipped = std::cell::Cell::new(0usize);

    let merged = std::cell::Cell::new(0usize);

    let crossed_count = std::cell::Cell::new(0usize);

    let invented = std::cell::Cell::new(0usize);
    let constrain = |cdt: &mut ConstrainedDelaunayTriangulation<Point2<f64>>,
                     uv_of: &mut Vec<Vec2>,
                     xyz_of: &mut Vec<Option<Vec3>>,
                     handles: &[usize]| {
        for w in 0..handles.len() {
            let a = handles[w];
            let b = handles[(w + 1) % handles.len()];
            if a == b {
                // Two boundary points on one vertex: the segment between them
                // cannot be drawn, and the boundary has a gap there whether or
                // not the triangulation objected. It counts as unenforced for
                // the same reason a crossing does — the region fill can walk
                // through it.
                skipped.set(skipped.get() + 1);
                merged.set(merged.get() + 1);
                continue;
            }
            let (ha, hb) = (
                spade::handles::FixedVertexHandle::from_index(a),
                spade::handles::FixedVertexHandle::from_index(b),
            );
            let (pa_uv, pb_uv) = (uv_of[a], uv_of[b]);
            if cdt.can_add_constraint(ha, hb) {
                cdt.add_constraint(ha, hb);
                continue;
            }
            // Two reasons a segment is refused, and they want opposite
            // treatment. If it crosses another boundary segment, the boundary
            // folds over itself and splitting at the crossing invents a point
            // the neighbouring face has never heard of — one seam per split.
            // If nothing crosses it, it was refused for passing through a
            // vertex that is already there, and splitting at that vertex
            // invents nothing: it is the same boundary, written as two
            // segments instead of one.
            let crosses = {
                let side = |p: Vec2, q: Vec2, r: Vec2| {
                    let t = (q.u - p.u) * (r.v - p.v) - (q.v - p.v) * (r.u - p.u);
                    if t > 0.0 {
                        1i32
                    } else if t < 0.0 {
                        -1
                    } else {
                        0
                    }
                };
                (0..handles.len())
                    .filter(|i| {
                        *i != w
                            && (*i + 1) % handles.len() != w
                            && (w + 1) % handles.len() != *i
                    })
                    .any(|i| {
                        let (c, d) =
                            (uv_of[handles[i]], uv_of[handles[(i + 1) % handles.len()]]);
                        side(pa_uv, pb_uv, c) * side(pa_uv, pb_uv, d) < 0
                            && side(c, d, pa_uv) * side(c, d, pb_uv) < 0
                    })
            };
            if !crosses || split_crossings {
                let before = cdt.num_vertices();
                cdt.add_constraint_and_split(ha, hb, |p| p);
                if cdt.num_vertices() == before {
                    // Split at vertices that were already there: the same
                    // boundary, written as two segments instead of one.
                    continue;
                }
                // It had to invent points after all. Give them parameters off
                // the triangulation and leave their positions to the surface,
                // and count the segment as unenforced so the region fill knows
                // not to trust itself here.
                for v in cdt.vertices() {
                    let i = v.index();
                    if i >= uv_of.len() {
                        uv_of.resize(i + 1, Vec2::default());
                        xyz_of.resize(i + 1, None);
                        uv_of[i] =
                            Vec2::new(v.position().x / scale.u, v.position().y / scale.v);
                    }
                }
            }
            skipped.set(skipped.get() + 1);
            if crosses {
                crossed_count.set(crossed_count.get() + 1);
                if std::env::var_os("CAD_TESS_CROSSING").is_some() {
                    let (qa, qb) = (xyz_of[a], xyz_of[b]);
                    println!(
                        "[crossing] uv ({:.6},{:.6})-({:.6},{:.6}) xyz {:?} {:?}",
                        pa_uv.u, pa_uv.v, pb_uv.u, pb_uv.v, qa, qb
                    );
                }
            } else {
                invented.set(invented.get() + 1);
            }
        }
    };

    // Two boundary points landing on one triangulation vertex costs the face
    // a boundary point: the polygon then runs straight from the one before to
    // the one after, and the neighbouring face — which kept all three — has a
    // segment this one never draws. The position is what has to be exact, and
    // it is cached; the parameter is only how the triangulation orders things.
    // So when a point would merge, move its parameter a hair along the way the
    // boundary is already going and insert it as its own vertex.
    let mut boundary_handles = Vec::new();
    let mut taken: rustc_hash::FxHashSet<usize> = Default::default();
    let mut previous_uv: Option<Vec2> = None;
    for (uv, xyz) in outer.uv.iter().zip(&outer.xyz) {
        let mut candidate = *uv;
        for attempt in 0..4 {
            let Some(h) = insert(&mut cdt, &mut uv_of, &mut xyz_of, candidate, Some(*xyz)) else {
                break;
            };
            if taken.insert(h) {
                boundary_handles.push(h);
                previous_uv = Some(candidate);
                break;
            }
            // Step off along the boundary's own direction, growing the nudge
            // in case the first one lands on another vertex too.
            let step = previous_uv
                .map(|p| Vec2::new(uv.u - p.u, uv.v - p.v))
                .filter(|d| d.u != 0.0 || d.v != 0.0)
                .unwrap_or(Vec2::new(1.0, 1.0));
            let scale = f64::EPSILON * 64.0 * (1 << attempt) as f64;
            candidate = Vec2::new(
                candidate.u + step.u * scale + scale * uv.u.abs(),
                candidate.v + step.v * scale + scale * uv.v.abs(),
            );
        }
    }
    constrain(&mut cdt, &mut uv_of, &mut xyz_of, &boundary_handles);

    // A hole's points need the same protection as the outer loop's: two of
    // them landing on one triangulation vertex loses the second one's position
    // to the first, and the face then draws a different path around the hole
    // than the face on the other side of it does.
    for hole in &holes {
        let mut hs = Vec::new();
        let mut previous_uv: Option<Vec2> = None;
        for (uv, xyz) in hole.uv.iter().zip(&hole.xyz) {
            let mut candidate = *uv;
            for attempt in 0..4 {
                let Some(h) = insert(&mut cdt, &mut uv_of, &mut xyz_of, candidate, Some(*xyz))
                else {
                    break;
                };
                if taken.insert(h) {
                    hs.push(h);
                    previous_uv = Some(candidate);
                    break;
                }
                let step = previous_uv
                    .map(|p| Vec2::new(uv.u - p.u, uv.v - p.v))
                    .filter(|d| d.u != 0.0 || d.v != 0.0)
                    .unwrap_or(Vec2::new(1.0, 1.0));
                let scale = f64::EPSILON * 64.0 * (1 << attempt) as f64;
                candidate = Vec2::new(
                    candidate.u + step.u * scale + scale * uv.u.abs(),
                    candidate.v + step.v * scale + scale * uv.v.abs(),
                );
            }
        }
        if hs.len() >= 3 {
            constrain(&mut cdt, &mut uv_of, &mut xyz_of, &hs);
        }
    }

    if options.interior_points {
        for uv in interior_samples(surface, domain, &outer, &holes, options) {
            insert(&mut cdt, &mut uv_of, &mut xyz_of, uv, None);
        }
    }

    // Which triangles are inside the trimmed region.
    //
    // The obvious test — is this triangle's centroid inside the boundary
    // polygon — asks each triangle in isolation, and near a boundary that is
    // curved between its samples the answer flips for triangles that plainly
    // belong: measured over this assembly, it discarded thousands of them,
    // each one leaving its edges exposed as a slit.
    //
    // Walking the triangulation instead asks the question once. Start outside
    // the convex hull, where the answer is known, and cross into neighbouring
    // triangles: an ordinary edge leaves the answer alone, and a constraint —
    // every boundary and hole segment is one — flips it. That is the even-odd
    // rule again, but evaluated along a path rather than per triangle, so
    // neighbours can never disagree and no triangle is left out by a
    // borderline test.
    let mut inside = vec![false; cdt.num_all_faces()];
    let mut visited = vec![false; cdt.num_all_faces()];
    let mut stack: Vec<(spade::handles::FixedFaceHandle<spade::handles::InnerTag>, bool)> =
        Vec::new();
    for hull in cdt.convex_hull() {
        // The hull edge's other side is the outer face; this side is the first
        // triangle in, and it is inside the region only if the hull edge is
        // itself a boundary.
        if let Some(f) = hull.rev().face().as_inner() {
            let state = hull.is_constraint_edge();
            if !visited[f.index()] {
                visited[f.index()] = true;
                inside[f.index()] = state;
                stack.push((f.fix(), state));
            }
        }
    }
    while let Some((f, state)) = stack.pop() {
        for e in cdt.face(f).adjacent_edges() {
            let Some(next) = e.rev().face().as_inner() else {
                continue;
            };
            if visited[next.index()] {
                continue;
            }
            let carried = state ^ e.is_constraint_edge();
            visited[next.index()] = true;
            inside[next.index()] = carried;
            stack.push((next.fix(), carried));
        }
    }

    // The walk assigns each triangle the first answer that reaches it, and
    // where the boundary has a gap two paths can reach the same place with
    // different answers. The disagreement always shows up on an ordinary edge
    // — a constraint is allowed to separate two answers, nothing else is — and
    // an ordinary edge with a kept triangle on one side and a dropped one on
    // the other is a slit in the mesh. Settling those by majority leaves the
    // regions the constraints really do bound untouched, and closes the slit:
    // a triangle surrounded by agreement joins it.
    for _ in 0..SETTLE_ROUNDS {
        let mut flip: Vec<spade::handles::FixedFaceHandle<spade::handles::InnerTag>> = Vec::new();
        for tri in cdt.inner_faces() {
            let mut agree = 0i32;
            let mut differ = 0i32;
            for e in tri.adjacent_edges() {
                if e.is_constraint_edge() {
                    continue;
                }
                let Some(other) = e.rev().face().as_inner() else {
                    continue;
                };
                if inside[other.index()] == inside[tri.index()] {
                    agree += 1;
                } else {
                    differ += 1;
                }
            }
            if differ > agree {
                flip.push(tri.fix());
            }
        }
        if flip.is_empty() {
            break;
        }
        for f in flip {
            inside[f.index()] = !inside[f.index()];
        }
    }

    let emit = |region: &[bool]| {
        let mut patch = Patch::default();
        let mut remap = vec![u32::MAX; uv_of.len()];
        let mut kept = 0usize;
        let mut parameters: Vec<Vec2> = Vec::new();
        let mut on_boundary: Vec<bool> = Vec::new();

    for tri in cdt.inner_faces() {
        let vs = tri.vertices();
        let idx = [vs[0].index(), vs[1].index(), vs[2].index()];
        if !region[tri.index()] {
            continue;
        }

        // A triangle two of whose corners are the same point in 3D has no
        // area. It draws nothing, and its two coincident edges are counted
        // twice over — which is most of what a mesh audit reports as
        // non-manifold. This happens wherever the surface collapses: the last
        // row of quads against a cone's apex or a sphere's pole is a row of
        // these.
        {
            let q = |i: usize| xyz_of[i].unwrap_or_else(|| surface.point_at(uv_of[i]));
            let (a, b, c) = (q(idx[0]), q(idx[1]), q(idx[2]));
            if (b - a).cross(c - a).length_squared() <= 0.0 {
                continue;
            }
        }

        let mut corner = [0u32; 3];
        for (k, &i) in idx.iter().enumerate() {
            if remap[i] == u32::MAX {
                let uv = uv_of[i];
                let p = xyz_of[i].unwrap_or_else(|| surface.point_at(uv));
                let mut n = surface.normal_at(uv);
                if !same_sense {
                    n = -n;
                }
                remap[i] = patch.positions.len() as u32;
                patch.positions.push([p.x as f32, p.y as f32, p.z as f32]);
                patch.normals.push([n.x as f32, n.y as f32, n.z as f32]);
                // Kept alongside so the patch can be refined afterwards
                // without inverting anything: a vertex whose position came
                // from the shared edge cache is a boundary vertex, and the
                // face across that edge has the same points, so it must never
                // be split from this side.
                parameters.push(uv);
                on_boundary.push(xyz_of[i].is_some());
            }
            corner[k] = remap[i];
        }

        // spade emits every inner face counter-clockwise in its own
        // coordinates, which are the scaled parameters. A CCW parameter
        // triangle has its 3D normal along ∂u × ∂v, i.e. the surface normal;
        // a face that reverses the surface must reverse the winding with it.
        if same_sense {
            patch.indices.extend_from_slice(&corner);
        } else {
            patch.indices.extend_from_slice(&[corner[0], corner[2], corner[1]]);
        }
        kept += 1;
    }

        let _ = kept;

        // A triangle that reaches across a quarter of the face's own parameter
        // range is not faceting, it is a chord through the part. Dump the
        // first face that produces one, whole, so the cause can be read off
        // rather than guessed at.
        if std::env::var_os("CAD_TESS_DUMP_BAD").is_some()
            && std::env::var("CAD_TESS_DUMP_KIND")
                .map_or(true, |k| k == surface_kind(surface))
        {
            let (lo, hi) = span(outer.uv.iter().map(|p| p.u));
            let width = hi - lo;
            let mut offender = None;
            for tri in cdt.inner_faces() {
                if !region[tri.index()] {
                    continue;
                }
                let i = tri.vertices().map(|v| v.index());
                let (a, b, c) = (uv_of[i[0]], uv_of[i[1]], uv_of[i[2]]);
                // What matters is not how far the triangle reaches in
                // parameter but how far it leaves the surface: on a plane a
                // triangle spanning the whole face is exact.
                // How far the edge leaves the surface, found by inverting its
                // 3D midpoint. Comparing against the *parameter* midpoint
                // instead reports a whole cone as wrong: an apex has no
                // meaningful u, so the parameter halfway along a generator
                // lands on the far side of the part while the generator itself
                // lies on the surface exactly.
                let reach = [(a, b), (b, c), (c, a)]
                    .iter()
                    .map(|(x, y)| {
                        let mid = (surface.point_at(*x) + surface.point_at(*y)) * 0.5;
                        match surface.invert(mid, Some(*x)) {
                            Some(uv) => (surface.point_at(uv) - mid).length(),
                            None => 0.0,
                        }
                    })
                    .fold(0.0f64, f64::max);
                // The bound is in millimetres so a single face can be
                // singled out; without one the first face over the limit is
                // dumped and the worst is never reached.
                let limit = std::env::var("CAD_TESS_DUMP_BAD")
                    .ok()
                    .and_then(|v| v.parse::<f64>().ok())
                    .unwrap_or(options.sag * 20.0);
                if reach > limit {
                    offender = Some((a, b, c, reach));
                    break;
                }
            }
            if let Some((a, b, c, reach)) = offender
                && !DUMPED.swap(true, std::sync::atomic::Ordering::SeqCst)
            {
                println!(
                    "[bad] {} u range [{lo:.4},{hi:.4}] width {width:.4} reach {reach:.4} {}",
                    surface_kind(surface),
                    match surface {
                        Surface::Sphere { radius, frame } => format!(
                            "radius {radius:.4} centre [{:.2},{:.2},{:.2}]",
                            frame.origin.x, frame.origin.y, frame.origin.z
                        ),
                        Surface::Torus { major_radius, minor_radius, .. } =>
                            format!("major {major_radius:.4} minor {minor_radius:.4}"),
                        _ => String::new(),
                    }
                );
                println!(
                    "[bad] triangle uv ({:.15},{:.15}) ({:.15},{:.15}) ({:.15},{:.15})",
                    a.u, a.v, b.u, b.v, c.u, c.v
                );
                let w = |q: Vec2| surface.point_at(q);
                println!(
                    "[bad] triangle xyz [{:.3},{:.3},{:.3}] [{:.3},{:.3},{:.3}] \
                     [{:.3},{:.3},{:.3}]",
                    w(a).x, w(a).y, w(a).z,
                    w(b).x, w(b).y, w(b).z,
                    w(c).x, w(c).y, w(c).z,
                );
                println!("[bad] outer loop, {} points, wrap {}:", outer.uv.len(), outer.wrap);
                for q in &outer.uv {
                    println!("[bad]   {:.5} {:.5}", q.u, q.v);
                }
                for (h, hole) in holes.iter().enumerate() {
                    println!("[bad] hole {h}, {} points, wrap {}:", hole.uv.len(), hole.wrap);
                    for q in &hole.uv {
                        println!("[bad]   {:.5} {:.5}", q.u, q.v);
                    }
                }
            }
        }
        // Refine the triangles that missed, rather than the cells that might.
        // A grid can only be told to be finer everywhere; a finished
        // triangulation can be asked which of *its own* edges left the surface,
        // and only those are split.
        refine_patch(
            &mut patch,
            &mut parameters,
            &on_boundary,
            surface,
            options,
            same_sense,
        );
        patch
    };

    // An empty region is not a reason to give up on the face: the boundary is
    // still there, and rebuilding from it needs nothing else. Treat it as a
    // patch that drew none of its boundary and let the comparison below find
    // something better — it always will, if anything can.
    // How much parameter space the loops say this face covers — the outer's
    // area less each hole's — against how much the region actually took in.
    // A face can draw every boundary segment, tear nothing, and still cover
    // the wrong thing: a region fill that swallows a hole is exactly that, and
    // neither the gap count nor the interior-hole count can see it. On
    // embossed lettering it is the difference between an O and a blob.
    if std::env::var_os("CAD_TESS_COVERAGE").is_some() {
        let want = signed_area(&outer.uv).abs()
            - holes.iter().map(|h| signed_area(&h.uv).abs()).sum::<f64>();
        let got: f64 = cdt
            .inner_faces()
            .filter(|t| inside[t.index()])
            .map(|t| {
                let i = t.vertices().map(|v| v.index());
                let (a, b, c) = (uv_of[i[0]], uv_of[i[1]], uv_of[i[2]]);
                ((b.u - a.u) * (c.v - a.v) - (b.v - a.v) * (c.u - a.u)).abs() * 0.5
            })
            .sum();
        if want > 0.0 {
            println!(
                "[coverage] {} {:.4} holes={} extent={:.4}",
                surface_kind(surface),
                got / want,
                holes.len(),
                {
                    let mut b = cad_ir::math::Aabb::EMPTY;
                    for p in &outer.xyz {
                        b.add_point(*p);
                    }
                    b.diagonal()
                }
            );
        }
    }
    let mut patch = emit(&inside);

    patch.crossings = skipped.get();
    patch.merged = merged.get();
    patch.crossed = crossed_count.get();
    patch.invented = invented.get();
    if patch.crossings > 0 {
        patch.narrowest = narrowest_approach(&outer.xyz);
    }

    Ok(patch)
}

/// Weigh the region's triangulation against the ways of meshing this face that
/// ask nothing of its parameterisation, and keep whichever leaves less open.
/// How many edges of a patch are used by more than two of its own triangles.
///
/// Reported under `CAD_TESS_WIND`; **not** used to choose between candidates.
/// Adding it to the crack count was measured: it changes the choice on a
/// handful of faces, costs about 1,800 triangles, and leaves the non-manifold
/// count exactly where it was, because the alternatives on offer fold in the
/// same place.
///
/// Two is what a surface does: every interior edge has a triangle either side.
/// More than two is the patch lying on itself — its parameter region folded,
/// and the fill covered the same piece of surface twice. The file is not
/// describing a slit there (checked: the faces this catches have no repeated
/// edge in their bounds), so it is the reading that folded, and a candidate
/// that folds should lose to one that does not.
///
/// Positions, not indices: a fold puts two *different* parameter points on the
/// same place in space, which is exactly what indices cannot see. A periodic
/// face's seam does the same thing legitimately, but its two columns are
/// traversed once each, so they come to two uses and not more.
/// How many of a patch's triangles occupy three positions another already does.
///
/// Two triangles on the same three points are the same facet: the mesh gains
/// nothing by carrying both and every edge they share reads as used four
/// times, which is a non-manifold edge in the finished body.
/// Drop any triangle whose three positions another triangle already occupies.
///
/// Keeps the first, so the facet is still drawn; the copy carries no surface
/// the first does not and costs the mesh its manifoldness — every edge the two
/// share reads as used four times.
pub(crate) fn lay_each_facet_once(patch: &mut Patch) {
    // Two vertices standing at one point have to be recognised as one before
    // their triangles can be compared, and rounding alone will not do it:
    // the two are computed by separate routes and agree to far better than a
    // micron, but a pair either side of a bin edge rounds apart. So each
    // position takes the identity of the first one found in its own bin or
    // any bin touching it.
    let bin = |v: f32| (v as f64 * 1e6).round() as i64;
    let mut ids: rustc_hash::FxHashMap<[i64; 3], u32> = Default::default();
    let mut id_of: Vec<u32> = Vec::with_capacity(patch.positions.len());
    for (i, p) in patch.positions.iter().enumerate() {
        let b = [bin(p[0]), bin(p[1]), bin(p[2])];
        let mut found = None;
        'search: for dx in -1..=1 {
            for dy in -1..=1 {
                for dz in -1..=1 {
                    if let Some(&id) = ids.get(&[b[0] + dx, b[1] + dy, b[2] + dz]) {
                        found = Some(id);
                        break 'search;
                    }
                }
            }
        }
        let id = found.unwrap_or(i as u32);
        ids.insert(b, id);
        id_of.push(id);
    }

    let mut seen: rustc_hash::FxHashSet<[u32; 3]> = Default::default();
    let mut kept: Vec<u32> = Vec::with_capacity(patch.indices.len());
    for tri in patch.indices.chunks_exact(3) {
        let mut set = [id_of[tri[0] as usize], id_of[tri[1] as usize], id_of[tri[2] as usize]];
        set.sort_unstable();
        // The identities decide what counts as the same facet; the triangle
        // keeps its own vertices. Rewriting it to the merged ones was measured
        // and tears the mesh: a face's seam carries the same point twice on
        // purpose, and the neighbour across it matches both.
        if seen.insert(set) {
            kept.extend_from_slice(tri);
        }
    }
    patch.indices = kept;
}

pub(crate) fn repeated_facets(patch: &Patch) -> usize {
    let mut seen: rustc_hash::FxHashSet<[[i64; 3]; 3]> = Default::default();
    let mut repeats = 0;
    for tri in patch.indices.chunks_exact(3) {
        let mut set = [tri[0], tri[1], tri[2]].map(|i| patch.positions[i as usize]);
        set.sort_by(|a, b| {
            a[0].total_cmp(&b[0]).then(a[1].total_cmp(&b[1])).then(a[2].total_cmp(&b[2]))
        });
        if !seen.insert(set.map(|p| p.map(|v| (v as f64 * 1e6).round() as i64))) {
            repeats += 1;
        }
    }
    repeats
}

pub fn self_overlaps(patch: &Patch) -> usize {
    let key = |p: &[f32; 3]| {
        let q = |v: f32| (v as f64 * 1e6).round() as i64;
        let mut h = 1469598103934665603u64;
        for v in [q(p[0]), q(p[1]), q(p[2])] {
            for b in v.to_le_bytes() {
                h ^= b as u64;
                h = h.wrapping_mul(1099511628211);
            }
        }
        h
    };
    let mut uses: rustc_hash::FxHashMap<(u64, u64), usize> = Default::default();
    for tri in patch.indices.chunks_exact(3) {
        let k: Vec<u64> = tri.iter().map(|&i| key(&patch.positions[i as usize])).collect();
        for e in 0..3 {
            let (a, b) = (k[e], k[(e + 1) % 3]);
            if a == b {
                continue;
            }
            let id = if a < b { (a, b) } else { (b, a) };
            *uses.entry(id).or_default() += 1;
        }
    }
    if std::env::var_os("CAD_TESS_FOLD").is_some() {
        // Is the same triangle laid down twice? Keyed on the three corner
        // positions as a set, so a copy with a different winding still counts.
        let mut seen: rustc_hash::FxHashMap<[u64; 3], usize> = Default::default();
        for tri in patch.indices.chunks_exact(3) {
            let mut k: Vec<u64> = tri.iter().map(|&i| key(&patch.positions[i as usize])).collect();
            k.sort_unstable();
            *seen.entry([k[0], k[1], k[2]]).or_default() += 1;
        }
        let repeats: usize = seen.values().filter(|n| **n > 1).map(|n| n - 1).sum();
        if repeats > 0 {
            println!(
                "[fold]   {repeats} of {} triangles are a second copy of one already laid",
                patch.indices.len() / 3
            );
            let mut shown = 0;
            for (t, tri) in patch.indices.chunks_exact(3).enumerate() {
                let mut k: Vec<u64> =
                    tri.iter().map(|&i| key(&patch.positions[i as usize])).collect();
                k.sort_unstable();
                if seen[&[k[0], k[1], k[2]]] > 1 && shown < 6 {
                    shown += 1;
                    let c: Vec<String> = tri
                        .iter()
                        .map(|&i| {
                            let p = patch.positions[i as usize];
                            format!("{i}@[{:.4},{:.4},{:.4}]", p[0], p[1], p[2])
                        })
                        .collect();
                    println!("[fold]     triangle {t}: {}", c.join("  "));
                }
            }
        }
    }
    if std::env::var_os("CAD_TESS_FOLD").is_some() {
        // Which of the two stories the over-use tells: four triangles that
        // really lie on one another, or four that are apart in the mesh and
        // only meet after the weld pulls their corners together.
        let mut fans: rustc_hash::FxHashMap<(u64, u64), Vec<usize>> = Default::default();
        for (t, tri) in patch.indices.chunks_exact(3).enumerate() {
            let k: Vec<u64> = tri.iter().map(|&i| key(&patch.positions[i as usize])).collect();
            for e in 0..3 {
                let (a, b) = (k[e], k[(e + 1) % 3]);
                if a == b {
                    continue;
                }
                fans.entry(if a < b { (a, b) } else { (b, a) }).or_default().push(t);
            }
        }
        for (edge, tris) in fans.iter().filter(|(_, t)| t.len() > 2) {
            // The edge's own two ends, and every corner that is not one of
            // them. If the far corners sit within a triangle's reach of the
            // edge, the triangles really are stacked there; if they are
            // scattered, distant parts of the face have been sewn to one line.
            let mut ends: Vec<[f32; 3]> = Vec::new();
            let mut far: Vec<[f32; 3]> = Vec::new();
            for &t in tris {
                for &i in &patch.indices[t * 3..t * 3 + 3] {
                    let p = patch.positions[i as usize];
                    let k = key(&p);
                    if k == edge.0 || k == edge.1 {
                        if !ends.iter().any(|q| key(q) == k) {
                            ends.push(p);
                        }
                    } else {
                        far.push(p);
                    }
                }
            }
            let dist = |a: &[f32; 3], b: &[f32; 3]| {
                ((a[0] - b[0]).powi(2) + (a[1] - b[1]).powi(2) + (a[2] - b[2]).powi(2)).sqrt()
            };
            let length = if ends.len() == 2 { dist(&ends[0], &ends[1]) } else { f32::NAN };
            let mid = if ends.len() == 2 {
                [
                    (ends[0][0] + ends[1][0]) / 2.0,
                    (ends[0][1] + ends[1][1]) / 2.0,
                    (ends[0][2] + ends[1][2]) / 2.0,
                ]
            } else {
                [f32::NAN; 3]
            };
            let reach = far.iter().map(|p| dist(p, &mid)).fold(0.0f32, f32::max);
            let near = far.iter().map(|p| dist(p, &mid)).fold(f32::INFINITY, f32::min);
            println!(
                "[fold]   {} triangles on one edge of {:.4} mm; far corners {:.4}..{:.4} mm from its middle",
                tris.len(),
                length,
                near,
                reach
            );
        }
    }
    uses.values().filter(|n| **n > 2).count()
}

fn triangulate_compare(
    patch: Patch,
    surface: &Surface,
    outer: &UvLoop,
    holes: &[UvLoop],
    options: &Resolved,
    same_sense: bool,
    rings: &[&[Vec3]],
    carry_seam: bool,
) -> Result<Patch, String> {
    // Two ways of meshing this face are available and neither is always right.
    // Triangulating the parameter region is exact wherever the region can be
    // read, and there are two ways it cannot: the boundary's image folds, so a
    // segment of it crosses another and cannot be enforced; or it pinches,
    // touching itself at a vertex, so that going round one way and the other
    // disagree about which side is in. Rebuilding from the boundary asks the
    // parameterisation nothing at all, but blends the interior rather than
    // evaluating it.
    //
    // Rather than guess from a symptom which to trust, mesh the face both ways
    // wherever the first leaves any of its boundary undrawn, and keep whichever
    // draws more of it — because a boundary segment this face omits is a hole
    // in the finished mesh, and everything else between the two is interior.
    // How far a patch strays beyond the box its own boundary spans. A rebuild
    // cannot stray at all — it is built from the boundary — so this only ever
    // separates candidates when the region was read wrongly, and then it is
    // the whole difference between a face and a spike.
    let mut bounds = cad_ir::math::Aabb::EMPTY;
    for ring in rings {
        for q in ring.iter() {
            bounds.add_point(*q);
        }
    }
    let stray = |p: &Patch| {
        if bounds.is_empty() {
            return 0.0;
        }
        let g = |v: Vec3, i: usize| [v.x, v.y, v.z][i];
        p.positions.iter().fold(0.0f64, |m, q| {
            let w = Vec3::new(q[0] as f64, q[1] as f64, q[2] as f64);
            (0..3).fold(m, |m, k| {
                m.max(g(bounds.min, k) - g(w, k)).max(g(w, k) - g(bounds.max, k))
            })
        })
    };

    // What a candidate leaves open, all of it: segments of the file's own
    // boundary it never drew, and edges it tore inside itself. Counting only
    // the first ranks a patch that covers the boundary and holes its middle
    // above one that does neither, and both are cracks in the finished mesh.
    let cracks = |p: &Patch| boundary_gaps(p, &rings) + interior_holes(p, &rings);
    if std::env::var_os("CAD_TESS_RINGS").is_some() {
        for (k, r) in rings.iter().enumerate() {
            let mut b = cad_ir::math::Aabb::EMPTY;
            for q in r.iter() {
                b.add_point(*q);
            }
            println!(
                "[ring] face={} #{k} of {} points, ends {:.5} mm apart, box {:.3}x{:.3}x{:.3} at [{:.3}, {:.3}, {:.3}]",
                CURRENT_FACE.with(|c| c.get()),
                r.len(),
                r.first().zip(r.last()).map(|(a, z)| (*a - *z).length()).unwrap_or(f64::NAN),
                b.max.x - b.min.x, b.max.y - b.min.y, b.max.z - b.min.z,
                b.min.x, b.min.y, b.min.z
            );
        }
    }
    let mut best = (cracks(&patch), stray(&patch), patch);
    if std::env::var_os("CAD_TESS_CANDIDATES").is_some() {
        println!(
            "[cand] {} surface-path cracks={} stray={:.4} ring={}  merged={} crossed={} invented={} narrowest={:.6} face={}",
            surface_kind(surface),
            best.0,
            best.1,
            rings.iter().map(|r| r.len()).sum::<usize>(),
            best.2.merged,
            best.2.crossed,
            best.2.invented,
            best.2.narrowest,
            CURRENT_FACE.with(|c| c.get()),
        );
    }
    // A hemisphere stands a radius proud of the circle bounding it, which is
    // about half that circle's diagonal; past that a patch is not bulging, it
    // is somewhere else.
    let hole_rings: Vec<Vec<Vec3>> = holes.iter().map(|h| h.xyz.clone()).collect();
    // The file's own loops, for the planar reading below: the region step
    // bridges them into one walk, and a bridged ring projected onto a plane
    // crosses itself, so every triangle in it reads as outside.
    let (planar_outer, planar_holes) = {
        let mut loops: Vec<Vec<Vec3>> = rings.iter().map(|r| r.to_vec()).collect();
        let biggest = loops
            .iter()
            .enumerate()
            .max_by(|a, b| {
                ring_extent(a.1)
                    .partial_cmp(&ring_extent(b.1))
                    .unwrap_or(std::cmp::Ordering::Equal)
            })
            .map(|(i, _)| i)
            .unwrap_or(0);
        let head = loops.remove(biggest);
        (head, loops)
    };

    if best.0 > 0 || best.1 > bounds.diagonal() * 0.5 {
        // Without the holes the rebuild covers the outer boundary and leaves
        // theirs undrawn; with them it cuts them out but has to locate them in
        // a parameterisation of its own. Both are offered and neither is
        // assumed.
        for candidate in [
            blend_patch(&outer.xyz, &[], false, options, same_sense, None),
            blend_patch(&outer.xyz, &hole_rings, false, options, same_sense, None),
            blend_patch(&outer.xyz, &[], true, options, same_sense, None),
            blend_patch(&outer.xyz, &hole_rings, true, options, same_sense, None),
        ]
        .into_iter()
        .flatten()
        {
            let gaps = cracks(&candidate);
            let out = stray(&candidate);
            if std::env::var_os("CAD_TESS_CANDIDATES").is_some() {
                println!(
                    "[cand] {} ring={} cracks={gaps} stray={out:.4} diag={:.4} face={}",
                    surface_kind(surface),
                    rings.iter().map(|r| r.len()).sum::<usize>(),
                    bounds.diagonal(),
                    CURRENT_FACE.with(|c| c.get()),
                );
            }
            // A rebuild that reaches more than half the boundary's own
            // diagonal past it is not this face, whatever its crack count. A
            // hemisphere stands one radius proud of its rim — half the rim's
            // diagonal — and nothing legitimate stands further. Cone face 10
            // of `200 201 003-51` had four such candidates, each with no
            // cracks and a stray of 1.2–1.4 diagonals, and "fewer cracks
            // wins" took one over a surface reading with seven: a 1,172
            // triangle patch 41 mm off its cone, identically in both readers.
            if out > bounds.diagonal() * 0.5 {
                continue;
            }
            // A rebuild of an analytic face has to be on that face.
            //
            // Where the surface is a plane, cylinder, cone, sphere or torus,
            // inverting a point is closed form, so "is this patch the face?"
            // is arithmetic rather than a judgement. The two bolt holes of
            // `219 204 008` are what this is for: the bore wall is a cylinder
            // of radius 3.25 mm whose top rim steps down 0.5 mm where a slot
            // crosses it, the strip does not lay that one vertical boundary
            // edge, and so the surface reading arrives with three cracks. A
            // rebuild over the same two rings has none — because it spans the
            // bore instead of lining it, a domed sheet through the axis, 3.22
            // mm from a surface 3.25 mm in radius. Fewer cracks won, and both
            // readers capped both holes with a membrane you can see without
            // magnification.
            //
            // An earlier form of this rule was removed as protecting nothing:
            // it was written for cone face 10 of `200 201 003-51`, whose real
            // fault turned out to be upstream in `wrapped_region`, and broader
            // versions of it opened 30 half-edges in the STEP reading. This is
            // the narrow form — a tenth of the boundary's own diagonal, only
            // against analytic surfaces, only where a surface reading exists
            // to fall back on — and it is kept because it now has a case.
            let departure = |p: &Patch| -> f64 {
                if !matches!(
                    surface,
                    Surface::Plane { .. }
                        | Surface::Cylinder { .. }
                        | Surface::Cone { .. }
                        | Surface::Sphere { .. }
                        | Surface::Torus { .. }
                ) {
                    return 0.0;
                }
                p.positions.iter().fold(0.0f64, |m, q| {
                    let w = Vec3::new(q[0] as f64, q[1] as f64, q[2] as f64);
                    let d = surface
                        .invert(w, None)
                        .map(|uv| (surface.point_at(uv) - w).length())
                        .unwrap_or(0.0);
                    m.max(d)
                })
            };
            let off = departure(&candidate);
            if off > bounds.diagonal() * 0.1 {
                if std::env::var_os("CAD_TESS_CANDIDATES").is_some() {
                    println!(
                        "[cand] refused: the rebuild stands {off:.4} mm off its own {} (a tenth of {:.4} allowed) face={}",
                        surface_kind(surface),
                        bounds.diagonal(),
                        CURRENT_FACE.with(|c| c.get()),
                    );
                }
                continue;
            }
            // Fewer gaps wins; among equals, the one that stays nearest its
            // own boundary.
            if (gaps, out) < (best.0, best.1) {
                best = (gaps, out, candidate);
            }
        }
    }

    // Laying the boundary flat is the last thing to try, and only where every
    // other reading still leaves the mesh open. It asks nothing of the
    // surface, which is its strength and also why it must never be preferred
    // to a reading that already works: on a cone closed at an apex it draws
    // the whole boundary too, and would win on any measure but this one while
    // flattening the cone into the disc its boundary spans. Offered last, and
    // only against cracks it can actually remove.
    if best.0 > 0
        && let Ok(flat) = planar_patch(&planar_outer, &planar_holes, surface, options, same_sense)
    {
        let left = cracks(&flat);
        if std::env::var_os("CAD_TESS_CANDIDATES").is_some() {
            println!(
                "[cand] {} ring={} planar cracks={left}",
                surface_kind(surface),
                rings.iter().map(|r| r.len()).sum::<usize>()
            );
        }
        if left < best.0 {
            best = (left, stray(&flat), flat);
        }
    }

    best.2.undrawn = best.0;
    if best.0 > 0 && std::env::var_os("CAD_TESS_LEFTOVER").is_some() {
        // Faces the comparison could not close. Every one is a crack in the
        // finished mesh, so this is the list that says what is left to do:
        // what it left undrawn, what it tore open inside itself, and how the
        // region's boundary came to be unenforced where it was.
        println!(
            "[leftover] {} seam={} cracks={} gaps={} holes={} merged={} crossed={} invented={} sizes={:?}",
            surface_kind(surface),
            usize::from(carry_seam),
            best.0,
            boundary_gaps(&best.2, rings),
            interior_holes(&best.2, rings),
            best.2.merged,
            best.2.crossed,
            best.2.invented,
            rings.iter().map(|r| r.len()).collect::<Vec<_>>()
        );
        // Is every hole actually inside the outer loop? If a hole crosses it,
        // the region is not "outer minus holes" at all and no in/out rule can
        // read it — that is a defect in the boundary, not in the fill.
        for (k, h) in holes.iter().enumerate() {
            let inside = h.uv.iter().filter(|p| contains(&outer.uv, **p)).count();
            println!(
                "[hole] {k}: {inside} of {} points inside the outer loop, areas {:.1} vs {:.1}",
                h.uv.len(),
                outer.area,
                h.area
            );
        }
        if std::env::var_os("CAD_TESS_DUMP_UV").is_some() {
            for (k, l) in std::iter::once(outer).chain(holes.iter()).enumerate() {
                let pts: Vec<String> = l
                    .uv
                    .iter()
                    .map(|q| format!("({:.3},{:.3})", q.u, q.v))
                    .collect();
                println!("[uv] loop {k} area={:.3} wrap={} {}", l.area, l.wrap, pts.join(" "));
            }
        }
    }
    if std::env::var_os("CAD_TESS_GAPS").is_some() {
        let segs: usize = rings.iter().map(|r| r.len()).sum();
        println!(
            "[gaps] {} {} {segs} {}",
            surface_kind(surface),
            boundary_gaps(&best.2, &rings),
            interior_holes(&best.2, &rings)
        );
    }
    Ok(best.2)
}

/// How close a closed polyline comes to itself, ignoring near neighbours.
///
/// "Near" is a quarter of the way round, which is far enough that an ordinary
/// convex boundary reports its own diameter and only a boundary that folds
/// back on itself reports something small.
fn narrowest_approach(ring: &[Vec3]) -> f64 {
    let n = ring.len();
    if n < 8 {
        return 0.0;
    }
    let far = n / 4;
    let mut narrowest = f64::INFINITY;
    for i in 0..n {
        for j in (i + far)..n {
            if n - (j - i) < far {
                continue;
            }
            narrowest = narrowest.min((ring[j] - ring[i]).length());
        }
    }
    if narrowest.is_finite() { narrowest } else { 0.0 }
}

/// How many rounds the region answer is settled for.
///
/// Each round fixes the triangles that disagree with most of their neighbours,
/// and the count falls off fast: measured on this assembly, four rounds close
/// a quarter of the remaining slits, eight close a little more, and sixteen
/// buys two percent on top of that.
const SETTLE_ROUNDS: usize = 8;

/// Even-odd point-in-polygon test in parameter space.
fn contains(poly: &[Vec2], p: Vec2) -> bool {
    let mut inside = false;
    let n = poly.len();
    for i in 0..n {
        let a = poly[i];
        let b = poly[(i + 1) % n];
        if (a.v > p.v) != (b.v > p.v) {
            let t = (p.v - a.v) / (b.v - a.v);
            if p.u < a.u + t * (b.u - a.u) {
                inside = !inside;
            }
        }
    }
    inside
}

/// Interior parameter samples, spaced by what the surface's curvature needs.
///
/// A plane needs none — its boundary triangulation is already exact — which is
/// most faces in a mechanical model and most of the saving.
/// How far an interior sample must stay from the boundary, as a fraction of
/// the boundary's own median step.
///
/// The margin exists so a sample cannot land on a boundary constraint and split
/// it, because the neighbouring face — whose samples are its own — would not
/// split its copy, and the two meshes would then disagree along a shared edge.
/// It was half the median step, which is generous, and generosity has a price:
/// a region that is thin compared with its boundary's spacing gets no interior
/// points at all and is triangulated by chords running its whole length. That
/// is what happens in the sliver beside a corner where a spline's
/// parameterisation collapses, and it left 86 faces of 11,214 carrying an edge
/// more than twenty times their own tolerance from their surface.
///
/// Measured on the pilot assembly, sweeping the fraction down:
///
/// | fraction | faces with such an edge | open half-edges | triangles |
/// |---|---|---|---|
/// | 0.5 | 86 | 0 | 1,798,962 |
/// | 0.125 | 78 | 0 | 1,880,192 |
/// | 0.0625 | 65 | 0 | 1,929,606 |
/// | 0.03 | 19 | 0 | 1,975,156 |
/// | **0.015** | **6** | **0** | 2,017,614 |
/// | 0.005 | 6 | 0 | 2,078,678 |
///
/// The mesh never opens — the fear the margin was guarding against does not
/// materialise, because a boundary point's *position* is cached and shared
/// whatever the triangulation does with the parameters around it — and below
/// 0.015 nothing further is bought. The margin is kept rather than removed
/// because a sample exactly on a constraint is still worth avoiding.
const CLEARANCE: f64 = 0.015;

fn interior_samples(
    surface: &Surface,
    domain: &Domain,
    outer: &UvLoop,
    holes: &[UvLoop],
    options: &Resolved,
) -> Vec<Vec2> {
    if matches!(surface, Surface::Plane { .. }) {
        return Vec::new();
    }

    let (u_lo, u_hi) = span(outer.uv.iter().map(|p| p.u));
    let (v_lo, v_hi) = span(outer.uv.iter().map(|p| p.v));
    if !(u_hi > u_lo && v_hi > v_lo) {
        return Vec::new();
    }

    let u_mid = 0.5 * (u_lo + u_hi);
    let v_mid = 0.5 * (v_lo + v_hi);
    let nu = direction_steps(surface, domain, Axis::U, u_lo, u_hi, v_mid, options);
    let nv = direction_steps(surface, domain, Axis::V, v_lo, v_hi, u_mid, options);
    // A grid finer than this buys nothing a viewer can see and costs file size
    // on every part of an assembly at once. It bounds the *even* divisions
    // only: a surface written in pieces gets a line at each piece as well,
    // because an even grid, however fine, cannot see shape it steps over.
    let nu = nu.min(96);
    let nv = nv.min(96);

    // Where the surface stops being one polynomial. Kept only where the
    // surface actually bends across the break, so a sheet written as a hundred
    // redundant spans costs nothing and a helical sweep keeps every span it
    // needs.
    let along_u = |t: f64| surface.point_at(Vec2::new(t, v_mid));
    let along_v = |t: f64| surface.point_at(Vec2::new(u_mid, t));
    let no_knots = std::env::var_os("CAD_TESS_NO_KNOTS").is_some();
    let u_breaks = if no_knots { Vec::new() } else { knots::thin_breaks(
        &knots::surface_breaks(surface, Axis::U, u_lo, u_hi),
        u_lo,
        u_hi,
        options.sag,
        &along_u,
    )};
    let v_breaks = if no_knots { Vec::new() } else { knots::thin_breaks(
        &knots::surface_breaks(surface, Axis::V, v_lo, v_hi),
        v_lo,
        v_hi,
        options.sag,
        &along_v,
    )};

    if nu <= 1 && nv <= 1 && u_breaks.is_empty() && v_breaks.is_empty() {
        return Vec::new();
    }
    // Interior points sit at the crossings of the grid lines, so a direction
    // divided into one cell contributes no line and the crossings vanish with
    // it — the face then gets nothing at all, however finely the other
    // direction was divided. Measured over this assembly, that left 78% of
    // faces triangulated from their boundary alone, spanned by chords the
    // length of the face. One interior line is the least that lets the other
    // direction's divisions meet anything.
    // A ruled direction needs no interior line: a cylinder, a cone and an
    // extrusion are straight along their sweep, so triangles spanning it lie
    // on the surface exactly. A direction that curves is different — with no
    // line crossing it the face is triangulated by chords running its whole
    // length, and the chord's error is the face's own sagitta. So force a
    // line only where the surface actually bends both ways.
    let doubly_curved = matches!(
        surface,
        Surface::Sphere { .. } | Surface::Torus { .. } | Surface::Nurbs(_) | Surface::Revolution { .. }
    );
    // A ruled direction earns its exemption only over a rectangular region.
    // Where the region is trimmed to some other shape — a cylinder cut by a
    // slot, a cone with a pocket in it — the triangulation has to reach across
    // the interior to fill it, and with no line in the ruled direction to meet,
    // it reaches across the curved one instead. Measured on the pilot
    // assembly, that is a chord spanning 27° of a cylinder, 1.07 mm from a
    // surface held to 0.05 mm. Interior points are the crossings of the two
    // families of lines, so one direction divided into a single cell leaves
    // the face no interior points at all, however finely the other is divided.
    let simple_region = holes.is_empty() && {
        let eu = (u_hi - u_lo) * 1e-6;
        let ev = (v_hi - v_lo) * 1e-6;
        outer.uv.iter().all(|p| {
            (p.u - u_lo).abs() <= eu
                || (u_hi - p.u).abs() <= eu
                || (p.v - v_lo).abs() <= ev
                || (v_hi - p.v).abs() <= ev
        })
    };
    let (mut nu, mut nv) = if doubly_curved || !simple_region {
        (nu.max(2), nv.max(2))
    } else {
        (nu, nv)
    };

    // Over a region that is not a rectangle the grid also has to be roughly
    // square in 3D. A cylinder 72 mm long and 14 mm across gets 45 divisions
    // round and, by the ruling rule, one along — cells thirty times longer
    // than they are wide. A Delaunay triangulation of points laid out like
    // that answers with needles, and a needle's long edge runs wherever it
    // likes: on the pilot assembly one such face carried a sliver spanning
    // 277° of its own circumference. Matching the two spacings costs a few
    // hundred triangles a face and takes the choice away from the mesher.
    if !simple_region {
        let (du3, dv3) = surface.derivatives_at(Vec2::new(u_mid, v_mid));
        let across = (u_hi - u_lo) * du3.length();
        let along = (v_hi - v_lo) * dv3.length();
        if across.is_finite() && along.is_finite() && across > 0.0 && along > 0.0 {
            let u_step = across / nu.max(1) as f64;
            let v_step = along / nv.max(1) as f64;
            // Square is not required, only not grotesque. Cells up to four
            // times longer than they are wide triangulate cleanly; insisting
            // on square doubles the whole assembly's triangle count for
            // nothing a viewer can see.
            const ASPECT: f64 = 4.0;
            if u_step > 0.0 {
                nv = nv.max((along / (u_step * ASPECT)).ceil() as usize).min(96);
            }
            if v_step > 0.0 {
                nu = nu.max((across / (v_step * ASPECT)).ceil() as usize).min(96);
            }
        }
    }
    let (nu, nv) = (nu, nv);

    // The lines this face is gridded on: the even divisions merged with the
    // surface's own breaks.
    let us = knots::merge_even(&u_breaks, u_lo, u_hi, nu);
    let vs = knots::merge_even(&v_breaks, v_lo, v_hi, nv);

    // An interior point that lands on a boundary constraint splits it — and
    // the neighbouring face, whose interior samples are its own, does not
    // split its copy of that same edge. The two meshes then disagree along a
    // shared boundary and the model opens up. Keeping samples clear of the
    // boundary by a fraction of the local spacing is what makes independent
    // per-face triangulation safe.
    // The clearance a sample has to keep from the boundary follows the local
    // spacing, and with breaks merged in the spacing is no longer uniform, so
    // take the narrowest cell rather than an average that a dense run of knots
    // would make meaningless.
    let narrowest = |xs: &[f64], fallback: f64| {
        xs.windows(2)
            .map(|w| w[1] - w[0])
            .filter(|d| *d > 0.0)
            .fold(f64::INFINITY, f64::min)
            .min(fallback)
    };
    let du = narrowest(&us, (u_hi - u_lo) / nu.max(1) as f64);
    let dv = narrowest(&vs, (v_hi - v_lo) / nv.max(1) as f64);
    // What a sample has to stay clear of is a boundary segment, so the length
    // that matters is the boundary's own, not the interior grid's. Measuring
    // it by the grid instead makes the exclusion zone as wide as the face
    // whenever the face is thin, and a thin face — a fillet band, a narrow
    // collar — then gets no interior points at all and is triangulated by
    // chords running its whole length.
    let mut steps: Vec<f64> = std::iter::once(&outer.uv)
        .chain(holes.iter().map(|h| &h.uv))
        .flat_map(|r| {
            r.windows(2)
                .map(|w| (w[1].u - w[0].u).hypot(w[1].v - w[0].v))
        })
        .filter(|d| *d > 0.0)
        .collect();
    let clearance = if steps.is_empty() {
        0.25 * du.hypot(dv)
    } else {
        steps.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
        (CLEARANCE * steps[steps.len() / 2]).min(0.25 * du.hypot(dv))
    };

    // A grid uniform in parameter cannot follow a surface that is not. The
    // pilot's spring runs its wire as a clean circle for its whole length and
    // then loses it in the final hundredth, where one grid cell covers 2.0 mm
    // of surface against 0.06 mm in the middle; dividing *every* row finely
    // enough for that would ask sixteen hundred lines of a face that needs
    // ninety-six almost everywhere. So the cells that fail are subdivided and
    // the rest are left alone — which is what OpenCASCADE's own mesher does,
    // and why its spring has no triangle further than 0.0005 mm from this
    // surface while ours had some at 4.9 mm.
    //
    // A cell fails when the surface at its middle leaves the quad its four
    // corners span by more than the sag. The bound on depth and on the number
    // added keeps a pathological face from taking the whole budget.
    let mut extra: Vec<Vec2> = Vec::new();
    if options.interior_points && !matches!(surface, Surface::Plane { .. }) {
        let budget = 8192usize;
        let refine = |u0: f64, u1: f64, v0: f64, v1: f64, out: &mut Vec<Vec2>| {
            let mut stack = vec![(u0, u1, v0, v1, 4u32)];
            while let Some((a, b, c, d, depth)) = stack.pop() {
                if out.len() >= budget {
                    return;
                }
                let corner = |u: f64, v: f64| surface.point_at(Vec2::new(u, v));
                let middle = Vec2::new(0.5 * (a + b), 0.5 * (c + d));


                // The surface at the cell's middle against the quad its four
                // corners span. Three richer criteria were measured against
                // this one and all were worse on the assembly — see below.
                let quad = (corner(a, c) + corner(b, c) + corner(b, d) + corner(a, d)) * 0.25;
                if (surface.point_at(middle) - quad).length() <= options.sag {
                    continue;
                }
                out.push(middle);
                if depth > 0 {
                    let (um, vm) = (middle.u, middle.v);
                    stack.push((a, um, c, vm, depth - 1));
                    stack.push((um, b, c, vm, depth - 1));
                    stack.push((um, b, vm, d, depth - 1));
                    stack.push((a, um, vm, d, depth - 1));
                }
            }
        };
        for w in us.windows(2) {
            for h in vs.windows(2) {
                refine(w[0], w[1], h[0], h[1], &mut extra);
            }
        }
    }

    let mut out = Vec::with_capacity((us.len() * vs.len()) / 2 + extra.len());
    for &u in us.iter().skip(1).take(us.len().saturating_sub(2)) {
        for &v in vs.iter().skip(1).take(vs.len().saturating_sub(2)) {
            let uv = Vec2::new(u, v);
            if !contains(&outer.uv, uv) {
                continue;
            }
            if holes.iter().any(|h| contains(&h.uv, uv)) {
                continue;
            }
            if near_boundary(&outer.uv, uv, clearance)
                || holes.iter().any(|h| near_boundary(&h.uv, uv, clearance))
            {
                continue;
            }
            out.push(uv);
        }
    }

    // The refinement points have to be inside the region like the grid's, but
    // they keep a much smaller margin from the boundary: a cell that fails is
    // failing *because* the surface moves fast across it, and the place it
    // moves fastest is often against the boundary — the end of a sweep, the
    // corner of a trim. Holding them off by the grid's margin throws away
    // exactly the ones asked for.
    let tight = clearance * 0.05;
    for uv in extra {
        if !contains(&outer.uv, uv) {
            continue;
        }
        if holes.iter().any(|h| contains(&h.uv, uv)) {
            continue;
        }
        if near_boundary(&outer.uv, uv, tight)
            || holes.iter().any(|h| near_boundary(&h.uv, uv, tight))
        {
            continue;
        }
        out.push(uv);
    }
    out
}

/// Distance from `p` to the polygon's nearest edge, tested against `limit`.
fn near_boundary(poly: &[Vec2], p: Vec2, limit: f64) -> bool {
    let limit2 = limit * limit;
    for i in 0..poly.len() {
        let a = poly[i];
        let b = poly[(i + 1) % poly.len()];
        let (ex, ey) = (b.u - a.u, b.v - a.v);
        let len2 = ex * ex + ey * ey;
        let t = if len2 > 0.0 {
            (((p.u - a.u) * ex + (p.v - a.v) * ey) / len2).clamp(0.0, 1.0)
        } else {
            0.0
        };
        let (dx, dy) = (p.u - (a.u + ex * t), p.v - (a.v + ey * t));
        if dx * dx + dy * dy < limit2 {
            return true;
        }
    }
    false
}


/// Segments one parameter direction needs across `[lo, hi]`.
fn direction_steps(
    surface: &Surface,
    _domain: &Domain,
    axis: Axis,
    lo: f64,
    hi: f64,
    other: f64,
    options: &Resolved,
) -> usize {
    let span = hi - lo;
    match (surface, &axis) {
        // Ruled in v: straight lines need no interior samples along it.
        (Surface::Cylinder { .. } | Surface::Cone { .. }, Axis::V) => 1,
        (Surface::Cylinder { radius, .. }, Axis::U) => options.segments_for_arc(*radius, span),
        (Surface::Cone {
            radius, half_angle, ..
        }, Axis::U) => {
            // The radius varies along the cone; the widest end sets the step.
            let r = (radius + other * half_angle.tan()).abs().max(radius.abs());
            options.segments_for_arc(r, span)
        }
        (Surface::Sphere { radius, .. }, Axis::U) => {
            // A parallel's radius shrinks toward the poles.
            options.segments_for_arc(radius * other.cos().abs().max(1e-3), span)
        }
        (Surface::Sphere { radius, .. }, Axis::V) => options.segments_for_arc(*radius, span),
        (Surface::Torus {
            major_radius,
            minor_radius,
            ..
        }, Axis::U) => options.segments_for_arc(major_radius + minor_radius.abs(), span),
        (Surface::Torus { minor_radius, .. }, Axis::V) => {
            options.segments_for_arc(*minor_radius, span)
        }
        _ => {
            let probe = |t: f64| {
                let p = match axis {
                    Axis::U => Vec2::new(lo + span * t, other),
                    Axis::V => Vec2::new(other, lo + span * t),
                };
                surface.point_at(p)
            };
            adaptive_steps(&probe, options)
        }
    }
}

fn span(values: impl Iterator<Item = f64>) -> (f64, f64) {
    let mut lo = f64::INFINITY;
    let mut hi = f64::NEG_INFINITY;
    for v in values {
        lo = lo.min(v);
        hi = hi.max(v);
    }
    (lo, hi)
}

#[cfg(test)]
mod tests {

    fn ring(uv: Vec<Vec2>) -> UvLoop {
        let xyz = uv.iter().map(|p| Vec3::new(p.u, p.v, 0.0)).collect();
        let area = signed_area(&uv);
        UvLoop {
            uv,
            xyz,
            wrap: 0,
            travel: 0,
            area,
        }
    }

    #[test]
    fn the_outer_loop_is_the_one_that_contains_the_rest() {
        // The enclosing loop written so its own signed area cancels — a
        // boundary whose parameter image doubles back does exactly this — and
        // a small loop inside it with an honest area. By area the small one
        // wins and the face comes out inside out; by containment it does not.
        let big = ring(vec![
            Vec2::new(-10.0, -10.0),
            Vec2::new(10.0, -10.0),
            Vec2::new(10.0, 10.0),
            Vec2::new(-10.0, 10.0),
            Vec2::new(-10.0, -10.0),
            Vec2::new(-10.0, 10.0),
            Vec2::new(10.0, 10.0),
            Vec2::new(10.0, -10.0),
        ]);
        let small = ring(vec![
            Vec2::new(-2.0, -2.0),
            Vec2::new(2.0, -2.0),
            Vec2::new(2.0, 2.0),
            Vec2::new(-2.0, 2.0),
        ]);
        assert!(big.area.abs() < small.area.abs(), "the fixture is not the case");

        let mut loops = vec![big, small];
        let (outer, holes) = closed_region(&mut loops).expect("a region");
        assert_eq!(holes.len(), 1);
        let span = |l: &UvLoop| {
            l.uv.iter().fold(0.0f64, |m, p| m.max(p.u.abs()))
        };
        assert!(span(&outer) > span(&holes[0]), "the wrong loop was called outer");
    }

    #[test]
    fn the_seam_moves_off_a_hole_that_lies_across_it() {
        let period = std::f64::consts::TAU;
        // A strip cut at -pi, and a hole sitting right on the cut.
        // A wrapping loop is a path across one period, not a closed ring: its
        // last point is a period on from its first, not a repeat of it.
        let mut rings = vec![ring(
            (0..12)
                .map(|i| Vec2::new(-std::f64::consts::PI + period * i as f64 / 12.0, 0.0))
                .collect(),
        )];
        let mut holes = vec![ring(vec![
            Vec2::new(-3.3, 0.4),
            Vec2::new(-2.9, 0.4),
            Vec2::new(-2.9, 0.6),
            Vec2::new(-3.3, 0.6),
        ])];
        reseam(&mut rings, &mut holes, period);

        let lo = rings[0].uv.iter().fold(f64::INFINITY, |m, p| m.min(p.u));
        let hi = rings[0].uv.iter().fold(f64::NEG_INFINITY, |m, p| m.max(p.u));
        let (hlo, hhi) = holes[0].uv.iter().fold(
            (f64::INFINITY, f64::NEG_INFINITY),
            |(a, b), p| (a.min(p.u), b.max(p.u)),
        );
        assert!(
            hlo >= lo - 1e-9 && hhi <= hi + 1e-9,
            "hole u[{hlo},{hhi}] still runs off the strip u[{lo},{hi}]"
        );
        assert!(
            (hi - lo - period * 11.0 / 12.0).abs() < 1e-9,
            "the strip's own span changed: u[{lo},{hi}]"
        );
    }
    use super::*;

    #[test]
    fn signed_area_is_positive_for_a_counter_clockwise_square() {
        let square = [
            Vec2::new(0.0, 0.0),
            Vec2::new(1.0, 0.0),
            Vec2::new(1.0, 1.0),
            Vec2::new(0.0, 1.0),
        ];
        assert!((signed_area(&square) - 1.0).abs() < 1e-15);
        let mut reversed = square;
        reversed.reverse();
        assert!((signed_area(&reversed) + 1.0).abs() < 1e-15);
    }

    #[test]
    fn containment_handles_a_square_with_a_hole_shape() {
        let square = [
            Vec2::new(0.0, 0.0),
            Vec2::new(4.0, 0.0),
            Vec2::new(4.0, 4.0),
            Vec2::new(0.0, 4.0),
        ];
        assert!(contains(&square, Vec2::new(2.0, 2.0)));
        assert!(!contains(&square, Vec2::new(5.0, 2.0)));
        assert!(!contains(&square, Vec2::new(2.0, -1.0)));
    }

    #[test]
    fn nearest_branch_picks_the_closest_period() {
        // 0.1 next to 6.2 on a 2*pi period belongs one turn up.
        let tau = std::f64::consts::TAU;
        let got = nearest_branch(0.1, 6.2, tau);
        assert!((got - (0.1 + tau)).abs() < 1e-12, "got {got}");
        assert!((nearest_branch(3.0, 3.1, tau) - 3.0).abs() < 1e-12);
    }

    #[test]
    fn net_wrap_counts_a_full_turn() {
        let tau = std::f64::consts::TAU;
        let domain = Domain {
            u: cad_ir::math::Interval::new(0.0, tau),
            v: cad_ir::math::Interval::new(0.0, 1.0),
            u_period: Some(tau),
            v_period: None,
        };
        let ring: Vec<Vec2> = (0..12)
            .map(|i| Vec2::new(tau * i as f64 / 12.0, 0.0))
            .collect();
        assert_eq!(net_wrap(&ring, &domain), 1);

        let closed = vec![
            Vec2::new(0.0, 0.0),
            Vec2::new(1.0, 0.0),
            Vec2::new(1.0, 1.0),
            Vec2::new(0.0, 1.0),
        ];
        assert_eq!(net_wrap(&closed, &domain), 0);
    }

    #[test]
    fn parameter_scale_never_collapses_a_direction() {
        use cad_ir::math::Frame;
        let s = Surface::Sphere {
            frame: Frame::IDENTITY,
            radius: 5.0,
        };
        let scale = parameter_scale(&s, &s.domain());
        assert!(scale.u > 0.0 && scale.v > 0.0);
        assert!(scale.u.is_finite() && scale.v.is_finite());
    }
}

/// A short name for a surface variant, for diagnostics.
pub fn surface_kind(s: &Surface) -> &'static str {
    match s {
        Surface::Plane { .. } => "plane",
        Surface::Cylinder { .. } => "cylinder",
        Surface::Cone { .. } => "cone",
        Surface::Sphere { .. } => "sphere",
        Surface::Torus { .. } => "torus",
        // A patch that is degree one both ways is a grid of points, which is
        // what a rebuilt boundary produces — worth separating in a report.
        Surface::Nurbs(n) if n.u_degree == 1 && n.v_degree == 1 => "grid",
        Surface::Nurbs(_) => "nurbs",
        Surface::LinearExtrusion { .. } => "extrusion",
        Surface::Revolution { .. } => "revolution",
        Surface::Offset { .. } => "offset",
        Surface::RectangularTrimmed { .. } => "trimmed",
    }
}

/// A short name for a curve variant, for diagnostics.
pub fn curve_kind(c: &cad_ir::brep::Curve) -> &'static str {
    use cad_ir::brep::Curve::*;
    match c {
        Line { .. } => "line",
        Circle { .. } => "circ",
        Ellipse { .. } => "elli",
        Parabola { .. } => "para",
        Hyperbola { .. } => "hyp",
        Polyline { .. } => "poly",
        Nurbs(_) => "nurb",
        Trimmed { .. } => "trim",
        Composite { .. } => "comp",
        OnSurface { .. } => "onsurf",
    }
}

/// Mesh a face whose surface is a sampled grid straight from its own boundary.
///
/// A face rebuilt from its boundary — which is what the Parasolid reader does
/// with the blend family, having no closed form for a rolling-ball fillet —
/// carries a surface that is a grid of points with flat cells between them.
/// Putting that through the ordinary path asks it to do the one thing it is
/// bad at: given a 3D point, say which parameter names it. The patch only
/// approximates the boundary it was built from, so the answer is approximate,
/// and two neighbouring boundary points can come back with the same parameter
/// — at which point the triangulation has lost a boundary vertex and the face
/// is slit against everything it touches.
///
/// None of that question needs asking. The boundary is a closed chain of known
/// points, and the patch's parameterisation is the boundary's own: cut the
/// chain at its four corners and each side runs along one edge of the unit
/// square, so every boundary point's parameter is its arc-length fraction
/// along its side. Nothing is inverted, nothing is approximated, and the mesh
/// uses the neighbouring faces' exact points because they are the same points.
fn blend_patch(
    ring_in: &[Vec3],
    hole_rings: &[Vec<Vec3>],
    even_corners: bool,
    options: &Resolved,
    same_sense: bool,
    measured: Option<&cad_ir::brep::NurbsSurface>,
) -> Result<Patch, String> {
    // A point that repeats its neighbour carries no boundary and would take a
    // second copy of one parameter, which is exactly the collapse this path
    // exists to avoid.
    let mut ring: Vec<Vec3> = Vec::with_capacity(ring_in.len());
    for p in ring_in {
        if ring.last().is_some_and(|l| (*l - *p).length_squared() < 1e-24) {
            continue;
        }
        ring.push(*p);
    }
    while ring.len() > 1
        && ring
            .first()
            .zip(ring.last())
            .is_some_and(|(f, l)| (*f - *l).length_squared() < 1e-24)
    {
        ring.pop();
    }
    // Four corners and a side between each — but a boundary of exactly three
    // is a triangle, and this already handles one: the fourth side comes out
    // degenerate, which is what a triangular patch's fourth side is. Only
    // fewer than three is not a patch. Refusing three contradicted the very
    // thing the corner search was written to do and cost one face outright.
    let mut ring = ring;
    if ring.len() == 3 {
        ring.push(ring[2]);
    }
    if ring.len() < 4 {
        return Err(format!(
            "blend rebuild needs at least three boundary points, found {}",
            ring.len()
        ));
    }
    let ring = &ring[..];
    // Where the corners go decides how the boundary is spread over the square,
    // and the sharpest turns are only a guess at them: a boundary that curves
    // smoothly all the way round has no corners to find, and one with five
    // sharp ones has too many. Spacing them evenly is the other reasonable
    // reading, and the caller compares both against the boundary they draw.
    // Where the corners go decides how the boundary is spread over the square.
    //
    // Where the reader measured this face's interior, its grid was built on
    // *its* four corners, and cutting here at ours puts the two out of step:
    // measured, the mismatch is a quarter of a side at the median and a whole
    // side at the ninetieth percentile, so only 195 of 1034 grids could be
    // used at all. The grid carries its own corners — they are the four ends
    // of its control net — so the ring is cut at the points nearest them and
    // the two agree by construction rather than by luck.
    let corners = match measured.filter(|n| {
        n.u_degree == 1 && n.v_degree == 1 && n.control_points.len() >= 2
    }) {
        Some(n) if !even_corners => {
            let g = &n.control_points;
            let (rows, cols) = (g.len() - 1, g[0].len() - 1);
            let want = [g[0][0], g[rows][0], g[rows][cols], g[0][cols]];
            let nearest = |q: Vec3| {
                ring.iter()
                    .enumerate()
                    .min_by(|(_, a), (_, b)| {
                        (**a - q)
                            .length_squared()
                            .partial_cmp(&(**b - q).length_squared())
                            .unwrap_or(std::cmp::Ordering::Equal)
                    })
                    .map(|(i, _)| i)
                    .unwrap_or(0)
            };
            let found = [nearest(want[0]), nearest(want[1]), nearest(want[2]), nearest(want[3])];
            // Four distinct points, in the order they come round the ring.
            // Anything else means the grid is not this ring's, and the
            // boundary's own corners are the better answer.
            let mut sorted = found;
            sorted.sort_unstable();
            let distinct = sorted.windows(2).all(|w| w[0] < w[1]);
            let cyclic = {
                let mut rotated = found;
                while rotated[0] != sorted[0] {
                    rotated.rotate_left(1);
                }
                rotated == sorted
            };
            if distinct && cyclic { found } else { quad_corners(ring) }
        }
        _ if even_corners => {
            let n = ring.len();
            [0, n / 4, n / 2, (3 * n) / 4]
        }
        _ => quad_corners(ring),
    };
    let sides = split_at_corners(ring, corners);

    // Sides 2 and 3 run backwards around the loop; index every side from the
    // patch's own origin so the four agree about which way is which.
    let along_v0 = &sides[0];
    let along_u1 = &sides[1];
    let along_v1: Vec<Vec3> = sides[2].iter().rev().copied().collect();
    let along_u0: Vec<Vec3> = sides[3].iter().rev().copied().collect();

    let p00 = along_v0[0];
    let p10 = along_v0[along_v0.len() - 1];
    let p11 = along_v1[along_v1.len() - 1];
    let p01 = along_v1[0];

    // Where the reader measured this face's interior, use it.
    //
    // Offering *every* stored grid this way was tried first and measured
    // worse — points over 1 mm against OpenCASCADE 14 to 65, over 0.2 mm 2253
    // to 2683 — because most grids are not measurements. An arc-sectioned grid
    // is a construction from the record's stated radius, and a Coons patch is
    // the boundary restated; taking either as evidence about the interior
    // replaces a good interpolation with a worse one. `Solid::measured` is the
    // reader saying which grids the ball actually rolled: solved from it
    // touching both mating surfaces, and gated against this very boundary.
    //
    // On those, ruling between the boundary's opposite sides is the chord of
    // the fillet's arc — 0.7 to 1.3 mm inside the surface on the 1.0–1.5 mm
    // fillets here — and the grid is the arc. The boundary is untouched either
    // way; only interior samples move.
    //
    // It applies only when the grid is this patch: same four corners, same way
    // round. `quad_corners` is a judgement made twice, once by the reader and
    // once here, and the caller also offers an evenly-cornered reading; when
    // the two disagree the grid's parameters mean something else.
    // A degree-one grid is bilinear inside a cell, which is what its own
    // evaluator would say and cheaper to say directly.
    let grid_at = |n: &cad_ir::brep::NurbsSurface, s: f64, t: f64| {
        let g = &n.control_points;
        let (rows, cols) = (g.len() - 1, g[0].len() - 1);
        let (a, b) = (
            (s.clamp(0.0, 1.0)) * rows as f64,
            (t.clamp(0.0, 1.0)) * cols as f64,
        );
        let (i, j) = ((a.floor() as usize).min(rows - 1), (b.floor() as usize).min(cols - 1));
        let (fu, fv) = (a - i as f64, b - j as f64);
        g[i][j] * ((1.0 - fu) * (1.0 - fv))
            + g[i + 1][j] * (fu * (1.0 - fv))
            + g[i][j + 1] * ((1.0 - fu) * fv)
            + g[i + 1][j + 1] * (fu * fv)
    };

    // Which way round the grid sits.
    //
    // The grid's corners are where the *reader's* `quad_corners` cut the same
    // boundary, and this patch's are where ours did. The two agree about the
    // shape and rarely about which corner is first: offered 1020 measured
    // grids, an identity match took **14**. So all eight ways a square maps
    // onto a square are tried — four turns and their mirrors — and the one
    // whose four corners land on this patch's is used. Nothing is stretched;
    // it is the same grid read from a different corner.
    const ORIENTATIONS: [fn(f64, f64) -> (f64, f64); 8] = [
        |s, t| (s, t),
        |s, t| (t, 1.0 - s),
        |s, t| (1.0 - s, 1.0 - t),
        |s, t| (1.0 - t, s),
        |s, t| (t, s),
        |s, t| (s, 1.0 - t),
        |s, t| (1.0 - t, 1.0 - s),
        |s, t| (1.0 - s, t),
    ];
    let usable = |n: &cad_ir::brep::NurbsSurface| {
        n.u_degree == 1
            && n.v_degree == 1
            && n.weights.is_empty()
            && n.control_points.len() >= 2
            && n.control_points.first().map(|r| r.len()).unwrap_or(0) >= 2
            && {
                let cols = n.control_points[0].len();
                n.control_points.iter().all(|r| r.len() == cols)
            }
    };
    let shaped: Option<(&cad_ir::brep::NurbsSurface, fn(f64, f64) -> (f64, f64))> = measured
        .filter(|n| usable(n))
        .and_then(|n| {
            let tol = 1e-6 * (p11 - p00).length().max((p01 - p10).length()).max(1e-9);
            let corners = [(0.0, 0.0, p00), (1.0, 0.0, p10), (1.0, 1.0, p11), (0.0, 1.0, p01)];
            let best = ORIENTATIONS
                .iter()
                .map(|turn| {
                    let worst = corners.iter().fold(0.0f64, |m, (s, t, want)| {
                        let (u, v) = turn(*s, *t);
                        m.max((grid_at(n, u, v) - *want).length())
                    });
                    (worst, turn)
                })
                .min_by(|a, b| a.0.partial_cmp(&b.0).unwrap_or(std::cmp::Ordering::Equal));
            if std::env::var_os("CAD_TESS_MEASURED").is_some()
                && let Some((worst, _)) = best
            {
                let side = (p10 - p00).length().max((p01 - p00).length());
                println!(
                    "[corners] face={} best orientation misses by {worst:.5} mm, side {side:.4} mm, that is {:.2}% of it",
                    CURRENT_FACE.with(|c| c.get()),
                    100.0 * worst / side.max(1e-9)
                );
            }
            best.filter(|(worst, _)| *worst <= tol).map(|(_, turn)| (n, *turn))
        });

    if std::env::var_os("CAD_TESS_MEASURED").is_some() {
        println!(
            "[measured] face={} offered={} taken={}",
            CURRENT_FACE.with(|c| c.get()),
            measured.is_some(),
            shaped.is_some()
        );
    }
    // Bilinearly blended transfinite interpolation, evaluated by arc length so
    // the interior follows the boundary's own pace rather than the sampling's.
    let surface_at = |s: f64, t: f64| {
        if let Some((n, turn)) = shaped {
            let (u, v) = turn(s, t);
            return grid_at(n, u, v);
        }
        let c0 = at_fraction(along_v0, s);
        let c1 = at_fraction(&along_v1, s);
        let d0 = at_fraction(&along_u0, t);
        let d1 = at_fraction(along_u1, t);
        let ruled_t = c0 * (1.0 - t) + c1 * t;
        let ruled_s = d0 * (1.0 - s) + d1 * s;
        let bilinear = p00 * ((1.0 - s) * (1.0 - t))
            + p10 * (s * (1.0 - t))
            + p01 * ((1.0 - s) * t)
            + p11 * (s * t);
        ruled_t + ruled_s - bilinear
    };

    // The boundary's parameters, taken from where each point sits along its
    // own side. This is the whole point: exact, ordered, and never two the
    // same.
    let mut uv: Vec<Vec2> = Vec::with_capacity(ring.len());
    let mut xyz: Vec<Vec3> = Vec::with_capacity(ring.len());
    for (k, side) in sides.iter().enumerate() {
        let fractions = arc_fractions(side);
        // The last point of each side is the first of the next; leave it to
        // the side that starts there.
        for (i, p) in side.iter().enumerate().take(side.len() - 1) {
            let f = fractions[i];
            uv.push(match k {
                0 => Vec2::new(f, 0.0),
                1 => Vec2::new(1.0, f),
                2 => Vec2::new(1.0 - f, 1.0),
                _ => Vec2::new(0.0, 1.0 - f),
            });
            xyz.push(*p);
        }
    }
    if uv.len() < 3 {
        return Err("blend rebuild collapsed to fewer than three boundary points".into());
    }

    // How finely to sample the interior: enough that the flat cells stay
    // within the same sag the rest of the mesh is held to.
    let extent = {
        let mut b = cad_ir::math::Aabb::EMPTY;
        for p in ring {
            b.add_point(*p);
        }
        b.diagonal()
    };
    let steps = ((extent / options.sag.max(1e-9)).sqrt().ceil() as usize).clamp(2, 24);

    let mut cdt: ConstrainedDelaunayTriangulation<Point2<f64>> =
        ConstrainedDelaunayTriangulation::new();
    let mut handles = Vec::with_capacity(uv.len());
    let mut point_of: Vec<Vec3> = Vec::new();
    let mut param_of: Vec<Vec2> = Vec::new();
    // A boundary vertex's position is the neighbouring face's position, to the
    // bit. Nothing may overwrite it — least of all an interior sample that
    // happened to land on the same parameter.
    let mut is_boundary: Vec<bool> = Vec::new();
    for (p, q) in uv.iter().zip(&xyz) {
        let Ok(h) = cdt.insert(Point2::new(p.u, p.v)) else {
            continue;
        };
        let i = h.index();
        if i >= point_of.len() {
            point_of.resize(i + 1, Vec3::ZERO);
            param_of.resize(i + 1, Vec2::default());
            is_boundary.resize(i + 1, false);
        }
        point_of[i] = *q;
        param_of[i] = *p;
        is_boundary[i] = true;
        handles.push(i);
    }
    if handles.len() < 3 {
        return Err("blend rebuild lost its boundary to duplicate parameters".into());
    }
    for w in 0..handles.len() {
        let (a, b) = (handles[w], handles[(w + 1) % handles.len()]);
        if a == b {
            continue;
        }
        let (ha, hb) = (
            spade::handles::FixedVertexHandle::from_index(a),
            spade::handles::FixedVertexHandle::from_index(b),
        );
        if cdt.can_add_constraint(ha, hb) {
            cdt.add_constraint(ha, hb);
        }
    }

    // A hole is a boundary too, and its points belong in the same square. The
    // transfinite map has no closed-form inverse, but unlike the surface's own
    // parameterisation it is smooth and single-valued over the square by
    // construction, so a sweep and a few Newton steps land on the right place
    // every time.
    let locate = |p: Vec3| {
        const M: usize = 12;
        let mut best = Vec2::new(0.5, 0.5);
        let mut best_d2 = (surface_at(0.5, 0.5) - p).length_squared();
        for i in 0..=M {
            for j in 0..=M {
                let (s, t) = (i as f64 / M as f64, j as f64 / M as f64);
                let d = (surface_at(s, t) - p).length_squared();
                if d < best_d2 {
                    best_d2 = d;
                    best = Vec2::new(s, t);
                }
            }
        }
        let h = 1e-4;
        for _ in 0..24 {
            let here = surface_at(best.u, best.v);
            let du = (surface_at((best.u + h).min(1.0), best.v)
                - surface_at((best.u - h).max(0.0), best.v))
                * (0.5 / h);
            let dv = (surface_at(best.u, (best.v + h).min(1.0))
                - surface_at(best.u, (best.v - h).max(0.0)))
                * (0.5 / h);
            let gap = p - here;
            let (a, b, c) = (du.dot(du), du.dot(dv), dv.dot(dv));
            let (e, f) = (gap.dot(du), gap.dot(dv));
            let det = a * c - b * b;
            if det.abs() < 1e-300 {
                break;
            }
            let step = Vec2::new((e * c - f * b) / det, (a * f - b * e) / det);
            let next = Vec2::new(
                (best.u + step.u).clamp(0.0, 1.0),
                (best.v + step.v).clamp(0.0, 1.0),
            );
            let moved = (next.u - best.u).abs().max((next.v - best.v).abs());
            best = next;
            if moved < 1e-12 {
                break;
            }
        }
        best
    };
    for hole in hole_rings {
        let mut hs = Vec::new();
        for p in hole {
            let uv = locate(*p);
            let Ok(h) = cdt.insert(Point2::new(uv.u, uv.v)) else {
                continue;
            };
            let k = h.index();
            if k >= point_of.len() {
                point_of.resize(k + 1, Vec3::ZERO);
                param_of.resize(k + 1, Vec2::default());
                is_boundary.resize(k + 1, false);
            }
            if !is_boundary[k] {
                point_of[k] = *p;
                param_of[k] = uv;
                is_boundary[k] = true;
                hs.push(k);
            }
        }
        for w in 0..hs.len() {
            let (a, b) = (hs[w], hs[(w + 1) % hs.len()]);
            if a == b {
                continue;
            }
            let (ha, hb) = (
                spade::handles::FixedVertexHandle::from_index(a),
                spade::handles::FixedVertexHandle::from_index(b),
            );
            if cdt.can_add_constraint(ha, hb) {
                cdt.add_constraint(ha, hb);
            }
        }
    }

    // Interior samples, kept clear of the boundary so they cannot split a
    // constraint the neighbouring face has no matching split for.
    let margin = 0.5 / steps as f64;
    for i in 1..steps {
        for j in 1..steps {
            let (s, t) = (i as f64 / steps as f64, j as f64 / steps as f64);
            if s < margin || s > 1.0 - margin || t < margin || t > 1.0 - margin {
                continue;
            }
            let Ok(h) = cdt.insert(Point2::new(s, t)) else {
                continue;
            };
            let k = h.index();
            if k >= point_of.len() {
                point_of.resize(k + 1, Vec3::ZERO);
                param_of.resize(k + 1, Vec2::default());
                is_boundary.resize(k + 1, false);
            }
            if !is_boundary[k] {
                point_of[k] = surface_at(s, t);
                param_of[k] = Vec2::new(s, t);
            }
        }
    }

    // The holes, in the square's own coordinates, for the region test below.
    let hole_uv: Vec<Vec<Vec2>> = hole_rings
        .iter()
        .map(|h| h.iter().map(|p| locate(*p)).collect())
        .collect();

    // The boundary is the unit square's perimeter, so everything inside it is
    // the face; the only region test left is which of it the holes take back.
    let mut patch = Patch::default();
    let mut remap = vec![u32::MAX; point_of.len()];
    for tri in cdt.inner_faces() {
        let idx = tri.vertices().map(|v| v.index());
        let centroid = Vec2::new(
            (param_of[idx[0]].u + param_of[idx[1]].u + param_of[idx[2]].u) / 3.0,
            (param_of[idx[0]].v + param_of[idx[1]].v + param_of[idx[2]].v) / 3.0,
        );
        if centroid.u < 0.0 || centroid.u > 1.0 || centroid.v < 0.0 || centroid.v > 1.0 {
            continue;
        }
        if !hole_uv.is_empty() && hole_uv.iter().any(|h| contains(h, centroid)) {
            continue;
        }
        let (a, b, c) = (point_of[idx[0]], point_of[idx[1]], point_of[idx[2]]);
        if (b - a).cross(c - a).length_squared() <= 0.0 {
            continue;
        }

        let mut corner = [0u32; 3];
        for (k, &i) in idx.iter().enumerate() {
            if remap[i] == u32::MAX {
                let p = point_of[i];
                let uv = param_of[i];
                let eps = 1e-4;
                let du = surface_at((uv.u + eps).min(1.0), uv.v)
                    - surface_at((uv.u - eps).max(0.0), uv.v);
                let dv = surface_at(uv.u, (uv.v + eps).min(1.0))
                    - surface_at(uv.u, (uv.v - eps).max(0.0));
                let mut n = du.cross(dv).normalized_or(Vec3::new(0.0, 0.0, 1.0));
                if !same_sense {
                    n = -n;
                }
                remap[i] = patch.positions.len() as u32;
                patch.positions.push([p.x as f32, p.y as f32, p.z as f32]);
                patch.normals.push([n.x as f32, n.y as f32, n.z as f32]);
            }
            corner[k] = remap[i];
        }
        if same_sense {
            patch.indices.extend_from_slice(&corner);
        } else {
            patch.indices.extend_from_slice(&[corner[0], corner[2], corner[1]]);
        }
    }
    if patch.indices.is_empty() {
        return Err("blend rebuild produced no triangles".into());
    }
    patch.rebuilt = true;
    Ok(patch)
}

/// Mesh a face by laying its boundary flat, when nothing else draws it.
///
/// The two parameterisations already available both make an assumption about
/// the face's shape: the surface's own can fold or pinch, and the boundary
/// rebuild reads the ring as a quadrilateral with four corners in it. A face
/// that is neither — a blend band running round a closed feature, its outer
/// boundary written as fourteen edges and its inner as one — has nowhere
/// sensible to land in either, and on the pilot assembly one such face left a
/// quarter of its own boundary undrawn, which with its neighbours' side of the
/// same segments was two thirds of every crack left in the model.
///
/// A boundary encloses a region in its own best-fit plane whatever the surface
/// under it does, and projecting onto that plane needs no corners, no solve
/// and no assumption about how many loops there are. What it cannot do is
/// invent the interior, so the layout is planar and every position is the real
/// thing: boundary points are the neighbouring face's own points, to the bit,
/// and interior points are the surface evaluated where the plane says to look.
/// It is offered as one more candidate and kept only where it draws more of
/// the boundary than the others, so a face that does fold over its own plane
/// simply loses.
fn planar_patch(
    ring_in: &[Vec3],
    hole_rings: &[Vec<Vec3>],
    surface: &Surface,
    options: &Resolved,
    same_sense: bool,
) -> Result<Patch, String> {
    let ring: Vec<Vec3> = dedup_ring(ring_in);
    if ring.len() < 3 {
        return Err(format!(
            "planar rebuild needs at least three boundary points, found {}",
            ring.len()
        ));
    }
    let holes: Vec<Vec<Vec3>> = hole_rings.iter().map(|h| dedup_ring(h)).collect();

    // Newell's normal: the area-weighted normal of a closed polygon, which is
    // stable however the boundary is sampled and does not care that it is not
    // flat.
    let centre = ring.iter().fold(Vec3::ZERO, |a, p| a + *p) * (1.0 / ring.len() as f64);
    let mut normal = Vec3::ZERO;
    for w in 0..ring.len() {
        let (a, b) = (ring[w] - centre, ring[(w + 1) % ring.len()] - centre);
        normal = normal + a.cross(b);
    }
    let Some(n) = normal.try_normalized() else {
        return Err("planar rebuild found no plane in the boundary".into());
    };
    // Any two directions across it will do; take the longest boundary edge so
    // the projection is not degenerate for a boundary that is nearly a line.
    let along = (0..ring.len())
        .map(|w| ring[(w + 1) % ring.len()] - ring[w])
        .max_by(|a, b| {
            a.length_squared()
                .partial_cmp(&b.length_squared())
                .unwrap_or(std::cmp::Ordering::Equal)
        })
        .and_then(|d| (d - n * d.dot(n)).try_normalized())
        .ok_or("planar rebuild found no direction across its plane")?;
    let across = n.cross(along);
    let flat = |p: Vec3| {
        let d = p - centre;
        Vec2::new(d.dot(along), d.dot(across))
    };

    let outer_uv: Vec<Vec2> = ring.iter().map(|p| flat(*p)).collect();
    let hole_uv: Vec<Vec<Vec2>> = holes
        .iter()
        .map(|h| h.iter().map(|p| flat(*p)).collect())
        .collect();

    let mut cdt: ConstrainedDelaunayTriangulation<Point2<f64>> =
        ConstrainedDelaunayTriangulation::new();
    let mut point_of: Vec<Vec3> = Vec::new();
    let mut param_of: Vec<Vec2> = Vec::new();
    let mut is_boundary: Vec<bool> = Vec::new();
    let grow = |point_of: &mut Vec<Vec3>,
                    param_of: &mut Vec<Vec2>,
                    is_boundary: &mut Vec<bool>,
                    i: usize| {
        if i >= point_of.len() {
            point_of.resize(i + 1, Vec3::ZERO);
            param_of.resize(i + 1, Vec2::default());
            is_boundary.resize(i + 1, false);
        }
    };

    let constrain = |cdt: &mut ConstrainedDelaunayTriangulation<Point2<f64>>,
                         point_of: &mut Vec<Vec3>,
                         param_of: &mut Vec<Vec2>,
                         is_boundary: &mut Vec<bool>,
                         xyz: &[Vec3],
                         uv: &[Vec2]| {
        let mut handles = Vec::with_capacity(uv.len());
        for (p, q) in uv.iter().zip(xyz) {
            let Ok(h) = cdt.insert(Point2::new(p.u, p.v)) else {
                continue;
            };
            let i = h.index();
            grow(point_of, param_of, is_boundary, i);
            if !is_boundary[i] {
                point_of[i] = *q;
                param_of[i] = *p;
                is_boundary[i] = true;
            }
            handles.push(i);
        }
        for w in 0..handles.len() {
            let (a, b) = (handles[w], handles[(w + 1) % handles.len()]);
            if a == b {
                continue;
            }
            let (ha, hb) = (
                spade::handles::FixedVertexHandle::from_index(a),
                spade::handles::FixedVertexHandle::from_index(b),
            );
            if cdt.can_add_constraint(ha, hb) {
                cdt.add_constraint(ha, hb);
            }
        }
    };

    constrain(
        &mut cdt,
        &mut point_of,
        &mut param_of,
        &mut is_boundary,
        &ring,
        &outer_uv,
    );
    for (h, uv) in holes.iter().zip(&hole_uv) {
        constrain(
            &mut cdt,
            &mut point_of,
            &mut param_of,
            &mut is_boundary,
            h,
            uv,
        );
    }

    // Interior samples on the plane's own grid, spaced to the same sag the
    // rest of the mesh is held to, and lifted onto the surface rather than
    // onto the plane. One that will not invert is simply not placed.
    let mut box_min = outer_uv[0];
    let mut box_max = outer_uv[0];
    for p in &outer_uv {
        box_min = Vec2::new(box_min.u.min(p.u), box_min.v.min(p.v));
        box_max = Vec2::new(box_max.u.max(p.u), box_max.v.max(p.v));
    }
    let span = Vec2::new(box_max.u - box_min.u, box_max.v - box_min.v);
    let extent = span.u.max(span.v);
    let steps = ((extent / options.sag.max(1e-9)).sqrt().ceil() as usize).clamp(2, 48);
    let step = Vec2::new(span.u / steps as f64, span.v / steps as f64);
    for i in 1..steps {
        for j in 1..steps {
            let uv = Vec2::new(box_min.u + step.u * i as f64, box_min.v + step.v * j as f64);
            if !contains(&outer_uv, uv) || hole_uv.iter().any(|h| contains(h, uv)) {
                continue;
            }
            // Keep clear of the boundary: an interior point that lands on a
            // constraint splits it, and the neighbour has no matching split.
            let clearance = step.u.min(step.v) * 0.5;
            if distance_to_ring(&outer_uv, uv) < clearance
                || hole_uv.iter().any(|h| distance_to_ring(h, uv) < clearance)
            {
                continue;
            }
            let world = centre + along * uv.u + across * uv.v;
            let Some(st) = surface.invert(world, None) else {
                continue;
            };
            let Ok(h) = cdt.insert(Point2::new(uv.u, uv.v)) else {
                continue;
            };
            let k = h.index();
            grow(&mut point_of, &mut param_of, &mut is_boundary, k);
            if !is_boundary[k] {
                point_of[k] = surface.point_at(st);
                param_of[k] = uv;
            }
        }
    }

    let mut patch = Patch::default();
    let mut remap = vec![u32::MAX; point_of.len()];
    for tri in cdt.inner_faces() {
        let idx = tri.vertices().map(|v| v.index());
        let centroid = Vec2::new(
            (param_of[idx[0]].u + param_of[idx[1]].u + param_of[idx[2]].u) / 3.0,
            (param_of[idx[0]].v + param_of[idx[1]].v + param_of[idx[2]].v) / 3.0,
        );
        if !contains(&outer_uv, centroid) || hole_uv.iter().any(|h| contains(h, centroid)) {
            continue;
        }
        let (a, b, c) = (point_of[idx[0]], point_of[idx[1]], point_of[idx[2]]);
        let face_normal = (b - a).cross(c - a);
        if face_normal.length_squared() <= 0.0 {
            continue;
        }
        let mut corner = [0u32; 3];
        for (k, &i) in idx.iter().enumerate() {
            if remap[i] == u32::MAX {
                let p = point_of[i];
                let mut vn = surface
                    .invert(p, None)
                    .map(|st| surface.normal_at(st))
                    .unwrap_or(n);
                if !same_sense {
                    vn = -vn;
                }
                remap[i] = patch.positions.len() as u32;
                patch.positions.push([p.x as f32, p.y as f32, p.z as f32]);
                patch.normals.push([vn.x as f32, vn.y as f32, vn.z as f32]);
            }
            corner[k] = remap[i];
        }
        // The plane's winding follows the boundary's, which is the face's.
        if same_sense {
            patch.indices.extend_from_slice(&corner);
        } else {
            patch.indices.extend_from_slice(&[corner[0], corner[2], corner[1]]);
        }
    }
    if patch.indices.is_empty() {
        return Err("planar rebuild produced no triangles".into());
    }
    patch.rebuilt = true;
    Ok(patch)
}

/// The diagonal of a ring's bounding box, for telling an outer loop from what
/// it has cut out of it.
fn ring_extent(ring: &[Vec3]) -> f64 {
    let mut b = cad_ir::math::Aabb::EMPTY;
    for p in ring {
        b.add_point(*p);
    }
    b.diagonal()
}

/// A closed polyline with its repeated and coincident points taken out.
fn dedup_ring(ring_in: &[Vec3]) -> Vec<Vec3> {
    let mut ring: Vec<Vec3> = Vec::with_capacity(ring_in.len());
    for p in ring_in {
        if ring.last().is_some_and(|l| (*l - *p).length_squared() < 1e-24) {
            continue;
        }
        ring.push(*p);
    }
    while ring.len() > 1
        && ring
            .first()
            .zip(ring.last())
            .is_some_and(|(f, l)| (*f - *l).length_squared() < 1e-24)
    {
        ring.pop();
    }
    ring
}

/// How far a parameter point lies from a closed ring's segments.
fn distance_to_ring(ring: &[Vec2], p: Vec2) -> f64 {
    let mut best = f64::INFINITY;
    for w in 0..ring.len() {
        let (a, b) = (ring[w], ring[(w + 1) % ring.len()]);
        let d = Vec2::new(b.u - a.u, b.v - a.v);
        let len2 = d.u * d.u + d.v * d.v;
        let t = if len2 > 0.0 {
            (((p.u - a.u) * d.u + (p.v - a.v) * d.v) / len2).clamp(0.0, 1.0)
        } else {
            0.0
        };
        let q = Vec2::new(a.u + d.u * t, a.v + d.v * t);
        best = best.min(((p.u - q.u).powi(2) + (p.v - q.v).powi(2)).sqrt());
    }
    best
}

/// Each point's fraction of the way along a polyline, by arc length.
fn arc_fractions(points: &[Vec3]) -> Vec<f64> {
    let mut run = Vec::with_capacity(points.len());
    let mut total = 0.0;
    run.push(0.0);
    for w in points.windows(2) {
        total += (w[1] - w[0]).length();
        run.push(total);
    }
    if total <= 0.0 {
        let n = points.len().max(2) - 1;
        return (0..points.len()).map(|i| i as f64 / n as f64).collect();
    }
    run.iter().map(|r| r / total).collect()
}

/// The point a given fraction of the way along a polyline, by arc length.
fn at_fraction(points: &[Vec3], f: f64) -> Vec3 {
    if points.len() < 2 {
        return points.first().copied().unwrap_or(Vec3::ZERO);
    }
    let fractions = arc_fractions(points);
    let f = f.clamp(0.0, 1.0);
    for i in 0..points.len() - 1 {
        if f <= fractions[i + 1] {
            let span = fractions[i + 1] - fractions[i];
            let t = if span > 0.0 { (f - fractions[i]) / span } else { 0.0 };
            return points[i].lerp(points[i + 1], t);
        }
    }
    points[points.len() - 1]
}

/// Edges the patch uses only once that are not part of its boundary.
///
/// A patch is meant to be a disc: every interior edge shared by two triangles,
/// every boundary edge by one. An interior edge used once is a hole torn
/// inside the face, which no neighbour can close because no neighbour goes
/// there. It is a different defect from a boundary segment left undrawn, and
/// counting the two apart is what tells a region-fill bug from a corner bug.
fn interior_holes(patch: &Patch, rings: &[&[Vec3]]) -> usize {
    let mut ids: rustc_hash::FxHashMap<[u32; 3], u32> = Default::default();
    let mut key = |q: [u32; 3]| {
        let next = ids.len() as u32;
        *ids.entry(q).or_insert(next)
    };
    let welded: Vec<u32> = patch
        .positions
        .iter()
        .map(|q| key([q[0].to_bits(), q[1].to_bits(), q[2].to_bits()]))
        .collect();
    let mut uses: rustc_hash::FxHashMap<(u32, u32), usize> = Default::default();
    for tri in patch.indices.chunks_exact(3) {
        for k in 0..3 {
            let (a, b) = (welded[tri[k] as usize], welded[tri[(k + 1) % 3] as usize]);
            if a != b {
                *uses.entry((a.min(b), a.max(b))).or_default() += 1;
            }
        }
    }
    let mut boundary: rustc_hash::FxHashSet<(u32, u32)> = Default::default();
    for ring in rings {
        for w in 0..ring.len() {
            let (p, q) = (ring[w], ring[(w + 1) % ring.len()]);
            let a = key([(p.x as f32).to_bits(), (p.y as f32).to_bits(), (p.z as f32).to_bits()]);
            let b = key([(q.x as f32).to_bits(), (q.y as f32).to_bits(), (q.z as f32).to_bits()]);
            boundary.insert((a.min(b), a.max(b)));
        }
    }
    uses.iter()
        .filter(|(e, c)| **c == 1 && !boundary.contains(e))
        .count()
}

/// How many of a ring's segments a patch failed to draw.
///
/// A face's boundary segments are its neighbours' too, so every one the patch
/// omits is a hole in the finished mesh. Counting them is how two ways of
/// meshing the same face can be compared on the only thing that matters here.
fn boundary_gaps(patch: &Patch, rings: &[&[Vec3]]) -> usize {
    let mut ids: rustc_hash::FxHashMap<[u32; 3], u32> = Default::default();
    let mut have: rustc_hash::FxHashSet<(u32, u32)> = Default::default();
    let mut key = |q: [u32; 3]| {
        let next = ids.len() as u32;
        *ids.entry(q).or_insert(next)
    };
    let welded: Vec<u32> = patch
        .positions
        .iter()
        .map(|q| key([q[0].to_bits(), q[1].to_bits(), q[2].to_bits()]))
        .collect();
    for tri in patch.indices.chunks_exact(3) {
        for k in 0..3 {
            let (a, b) = (welded[tri[k] as usize], welded[tri[(k + 1) % 3] as usize]);
            if a != b {
                have.insert((a.min(b), a.max(b)));
            }
        }
    }
    let mut gaps = 0usize;
    for ring in rings {
        for w in 0..ring.len() {
            let p = ring[w];
            let q = ring[(w + 1) % ring.len()];
            if (p - q).length_squared() < 1e-24 {
                continue;
            }
            let a = key([(p.x as f32).to_bits(), (p.y as f32).to_bits(), (p.z as f32).to_bits()]);
            let b = key([(q.x as f32).to_bits(), (q.y as f32).to_bits(), (q.z as f32).to_bits()]);
            if !have.contains(&(a.min(b), a.max(b))) {
                if std::env::var_os("CAD_TESS_GAP_WHERE").is_some() {
                    println!(
                        "[gap] face={} ring of {} at {w}: [{:.4}, {:.4}, {:.4}] -> [{:.4}, {:.4}, {:.4}]  ({:.5} mm long)",
                        CURRENT_FACE.with(|c| c.get()),
                        ring.len(),
                        p.x, p.y, p.z, q.x, q.y, q.z, (q - p).length()
                    );
                }
                gaps += 1;
            }
        }
    }
    gaps
}

/// The four indices of a closed polyline that read as a quad's corners.
///
/// A blend band is a quadrilateral however many edges the file split its sides
/// into, so its corners are where the boundary turns sharpest rather than
/// where one edge record ends. Whatever the turning angles fail to supply is
/// filled in evenly, which is the right answer for a boundary with no corners
/// at all and keeps the result defined for every input.
fn quad_corners(ring: &[Vec3]) -> [usize; 4] {
    let n = ring.len();
    let min_gap = (n / 8).max(1);
    let mut scored: Vec<(f64, usize)> = (0..n)
        .map(|i| {
            let prev = ring[(i + n - 1) % n];
            let here = ring[i];
            let next = ring[(i + 1) % n];
            let turn = match ((here - prev).try_normalized(), (next - here).try_normalized()) {
                (Some(a), Some(b)) => a.dot(b).clamp(-1.0, 1.0).acos(),
                _ => 0.0,
            };
            (turn, i)
        })
        .collect();
    scored.sort_by(|a, b| b.0.partial_cmp(&a.0).unwrap_or(std::cmp::Ordering::Equal));

    let far_enough = |chosen: &[usize], i: usize| {
        chosen.iter().all(|&c| {
            let d = if c > i { c - i } else { i - c };
            d.min(n - d) >= min_gap
        })
    };
    let mut chosen: Vec<usize> = Vec::with_capacity(4);
    for (turn, i) in scored {
        if chosen.len() == 4 || turn < 0.15 {
            break;
        }
        if far_enough(&chosen, i) {
            chosen.push(i);
        }
    }
    let mut probe = 0usize;
    while chosen.len() < 4 && probe < n {
        if far_enough(&chosen, probe) {
            chosen.push(probe);
        }
        probe += (n / 4).max(1);
    }
    while chosen.len() < 4 {
        chosen.push(chosen.len() * n / 4);
    }
    chosen.sort_unstable();
    [chosen[0], chosen[1], chosen[2], chosen[3]]
}

/// Cut a closed polyline into the four chains between its corners.
fn split_at_corners(ring: &[Vec3], corners: [usize; 4]) -> [Vec<Vec3>; 4] {
    let n = ring.len();
    let mut sides: [Vec<Vec3>; 4] = Default::default();
    for k in 0..4 {
        let (from, to) = (corners[k], corners[(k + 1) % 4]);
        let mut i = from;
        loop {
            sides[k].push(ring[i]);
            if i == to {
                break;
            }
            i = (i + 1) % n;
        }
        if sides[k].len() < 2 {
            sides[k].push(ring[to]);
        }
    }
    sides
}

thread_local! {
    /// The face being tessellated, for the probes that print from deep inside
    /// the region code where no face id is in scope.
    static CURRENT_FACE: std::cell::Cell<u32> = const { std::cell::Cell::new(u32::MAX) };
}
