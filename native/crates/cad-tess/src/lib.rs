//! Watertight triangulation of trimmed B-Rep solids.
//!
//! # How watertightness is achieved
//!
//! Faces are triangulated independently — that is what makes the work
//! parallelisable, and what lets each face use its own parameter space. The
//! risk is cracks along shared edges, and the fix is structural rather than a
//! post-process weld:
//!
//! 1. **Every edge is discretised exactly once**, before any face is touched,
//!    and the resulting 3D points are cached by [`EdgeId`]. Both faces meeting
//!    at that edge receive the *same* `f64` points.
//! 2. **Boundary vertices keep their cached 3D position**, never the result of
//!    projecting into UV and evaluating back. `invert` then `point_at` is not
//!    an identity — it is a Newton solve followed by an evaluation — and a
//!    micron of round-trip error along a shared edge is a visible crack.
//! 3. **Vertex positions are shared, normals are not.** Two faces meeting at an
//!    edge emit bit-identical positions but their own normals, because a CAD
//!    model's edges are hard. Averaging normals across them is exactly what
//!    makes a converted part look melted.
//!
//! # Tolerance
//!
//! [`Options::linear_deflection`] is the maximum distance between the mesh and
//! the true surface, in scene units. [`Options::angular_deflection`] caps the
//! angle between adjacent facet normals, which is what keeps a small hole from
//! becoming a hexagon regardless of how tight the linear tolerance is.
//! Requesting a deflection below the source model's own tolerance is refused:
//! the extra triangles would resolve noise, not geometry.

#![forbid(unsafe_code)]

pub mod edge;
pub mod knots;
pub mod face;
pub mod options;

pub use face::{curve_kind, surface_kind};
pub use options::Options;

use cad_ir::brep::{FaceId, Solid};
use cad_ir::mesh::{Mesh, MeshPart};
use cad_ir::scene::{MaterialId, Scene};
use rayon::prelude::*;

/// What tessellation produced, beyond the meshes themselves.
#[derive(Debug, Default, Clone)]
pub struct Report {
    /// Faces that produced no triangles, with why.
    pub failed: Vec<FaceFailure>,
    pub triangles: usize,
    pub vertices: usize,
    /// Faces whose triangulation succeeded.
    pub faces_ok: usize,
}

/// A face that could not be triangulated.
#[derive(Debug, Clone)]
pub struct FaceFailure {
    pub geometry: String,
    pub face: FaceId,
    pub reason: String,
}

impl Report {
    fn merge(&mut self, other: Report) {
        self.failed.extend(other.failed);
        self.triangles += other.triangles;
        self.vertices += other.vertices;
        self.faces_ok += other.faces_ok;
    }

    /// Fraction of faces that produced triangles.
    pub fn success_rate(&self) -> f64 {
        let total = self.faces_ok + self.failed.len();
        if total == 0 {
            1.0
        } else {
            self.faces_ok as f64 / total as f64
        }
    }
}

/// Tessellate every geometry in the scene, filling each one's mesh.
pub fn tessellate_scene(scene: &mut Scene, options: &Options) -> Report {
    // Resolve a relative tolerance against the whole scene, so every part of an
    // assembly is tessellated to the same absolute accuracy — otherwise a small
    // bracket gets a hundred times the triangle density of the frame it bolts
    // to, for no visible benefit and a large share of the file.
    let scale = scene.vertex_bounds().diagonal();
    let resolved = options.resolve(scale);

    // A debug run wants one part's faces, not an assembly's; every probe in
    // this crate prints per face, and fifty parts' worth of that buries the
    // one being chased.
    let only = std::env::var("CAD_TESS_ONLY").ok();

    // One body at a time, and its faces in parallel.
    //
    // Both loops used to run in parallel, and the peak was the product: every
    // body in flight holds all of its face patches at once, and rayon will
    // start as many bodies as it has threads.
    //
    // What it costs is measured rather than assumed, and it is not free. Put
    // back — `par_iter_mut` here, everything else unchanged, byte-identical
    // output — three interleaved runs each:
    //
    //     Parasolid   276 MB, 25.5 s  ->  385 MB, 24.8 s
    //     STEP        294 MB, 10.2 s  ->  408 MB,  7.5 s
    //
    // On the Parasolid it buys nothing: its faces already saturate the
    // machine, the largest being 513 700 triangles. On the STEP, whose fifty
    // parts are small enough that one body's faces do not, it is worth 27% of
    // the clock — and 114 MB, which is the whole of what several rounds of
    // work have taken off this program. Bounding it to two bodies would halve
    // both halves of that and still spend fifty megabytes to save a second.
    //
    // Sequential also makes the exchange below possible: a body's mesh is
    // finished before the next one starts, so the boundary representation it
    // was built from can go back to the allocator right there. Collecting the
    // meshes first and assigning them afterwards — which this did — holds
    // every brep and every mesh at once for no reason but the borrow checker.
    let mut report = Report::default();
    for g in scene.geometry.iter_mut() {
        let wanted = match &g.brep {
            Some(solid) => only
                .as_ref()
                .map(|w| g.name.contains(w) || solid.name.contains(w))
                .unwrap_or(true),
            None => false,
        };
        if !wanted {
            continue;
        }

        // Taken where the caller allows it, borrowed where it does not. See
        // [`Options::release_brep`]: on the pilot this is 41.9 MB that has no
        // reader left, standing through the write.
        let taken = if options.release_brep {
            g.brep.take()
        } else {
            None
        };
        let Some(solid) = taken.as_ref().or(g.brep.as_ref()) else {
            continue;
        };
        let (mesh, r) = tessellate_solid(
            &g.name,
            solid,
            g.material,
            &g.face_materials,
            &resolved,
        );
        report.merge(r);
        // Before the mesh is stored, so the two are never both charged to this
        // body at once.
        drop(taken);
        if !mesh.is_empty() {
            g.mesh = Some(mesh);
        }
    }
    report
}

/// How far the patch's farthest point sits from the body's centre.
fn farthest(patch: &face::Patch, reference: &cad_ir::math::Aabb) -> f64 {
    if reference.is_empty() {
        return 0.0;
    }
    let centre = reference.centre();
    patch
        .positions
        .iter()
        .map(|p| {
            (cad_ir::math::Vec3::new(p[0] as f64, p[1] as f64, p[2] as f64) - centre).length()
        })
        .fold(0.0f64, f64::max)
}

/// True when a patch reaches outside the body it was built from.
///
/// A face can bulge past its own boundary — a spherical cap reaches a radius
/// above the circle bounding it — but never past the body. The allowance is a
/// full body diagonal beyond the centre, which no correct patch approaches and
/// every mis-recovered one exceeds.
/// Make every face of a shell agree about which way round it is.
///
/// Two triangles meeting at an edge must traverse it in opposite directions.
/// Where they do not, one of the two faces is inside out: the mesh is closed,
/// but a renderer culls the wrong side of it, the normals point into the part,
/// and anything that works from winding — a boolean, a slicer — is misled.
///
/// The per-face sense flag the file carries is right for the great majority of
/// faces and wrong for a few, and no single face can tell which it is. The
/// topology can: the shared edges constrain the whole shell up to one global
/// flip, so the faces are two-coloured across those constraints and the colour
/// that moves fewer triangles is the one taken. That keeps the file's own
/// statement wherever it is self-consistent — including its distinction
/// between an outer shell and a void, which a global rule would destroy — and
/// overrules it only where it contradicts itself.
///
/// A shell whose constraints cannot be satisfied at all (an odd cycle, which a
/// Möbius-like reading produces) keeps the first colouring reached; there is no
/// consistent answer to find, and the count is reported under `CAD_TESS_WIND`.
/// Six times the volume a patch encloses about the origin, `flip` reversing it.
///
/// Summed over a closed shell this is the volume itself, and its sign says
/// which way the shell faces. Over anything not closed it is meaningless, and
/// the caller checks for that.
fn patch_volume(patch: &face::Patch, flip: bool) -> f64 {
    let mut total = 0.0;
    for t in patch.indices.chunks_exact(3) {
        let p = |i: u32| {
            let q = patch.positions[i as usize];
            [q[0] as f64, q[1] as f64, q[2] as f64]
        };
        let (a, b, c) = if flip {
            (p(t[0]), p(t[2]), p(t[1]))
        } else {
            (p(t[0]), p(t[1]), p(t[2]))
        };
        total += a[0] * (b[1] * c[2] - b[2] * c[1]) - a[1] * (b[0] * c[2] - b[2] * c[0])
            + a[2] * (b[0] * c[1] - b[1] * c[0]);
    }
    total / 6.0
}

fn orient_shell(faces: &mut [(usize, FaceId, Result<face::Patch, String>)]) -> (usize, usize) {
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

    // Group by shell: two shells of one solid share no edge, and their global
    // senses are independent — an outer skin and the wall of a void inside it
    // are wound opposite on purpose.
    let mut shells: rustc_hash::FxHashMap<usize, Vec<usize>> = Default::default();
    for (i, (shell, _, result)) in faces.iter().enumerate() {
        if result.as_ref().is_ok_and(|p| !p.indices.is_empty()) {
            shells.entry(*shell).or_default().push(i);
        }
    }

    let mut flipped = 0usize;
    let mut unsatisfiable = 0usize;


    for members in shells.values() {
        let slot: rustc_hash::FxHashMap<usize, usize> =
            members.iter().enumerate().map(|(n, i)| (*i, n)).collect();
        // For each shared edge, whether the two faces already agree.
        let mut agree: Vec<Vec<(usize, bool)>> = vec![Vec::new(); members.len()];
        // Grown into, not reserved. Sizing this from the triangle count was
        // meant to save the final doubling — twenty megabytes by the audit's
        // arithmetic — and cost a hundred and twenty: a hash map rounds a
        // reservation up to a power of two and takes it in one contiguous
        // piece, where growing reuses what the reader has already freed. The
        // tessellation stage went 279 MB to 354 and back.
        let mut seen: rustc_hash::FxHashMap<(u64, u64), (bool, usize)> = Default::default();
        for &i in members {
            let Ok(patch) = &faces[i].2 else { continue };
            for t in patch.indices.chunks_exact(3) {
                let k: [u64; 3] = [
                    key(&patch.positions[t[0] as usize]),
                    key(&patch.positions[t[1] as usize]),
                    key(&patch.positions[t[2] as usize]),
                ];
                for e in 0..3 {
                    let (a, b) = (k[e], k[(e + 1) % 3]);
                    if a == b {
                        continue;
                    }
                    let (lo, hi, forward) = if a < b { (a, b, true) } else { (b, a, false) };
                    match seen.entry((lo, hi)) {
                        std::collections::hash_map::Entry::Occupied(o) => {
                            let (other_forward, other) = *o.get();
                            if other != i {
                                // Opposite directions is agreement.
                                let ok = other_forward != forward;
                                agree[slot[&i]].push((slot[&other], ok));
                                agree[slot[&other]].push((slot[&i], ok));
                            }
                        }
                        std::collections::hash_map::Entry::Vacant(v) => {
                            v.insert((forward, i));
                        }
                    }
                }
            }
        }

        // Two-colour each connected group, then keep whichever colour moves
        // fewer triangles — the file is right far more often than not.
        let mut colour: Vec<Option<bool>> = vec![None; members.len()];
        for start in 0..members.len() {
            if colour[start].is_some() {
                continue;
            }
            colour[start] = Some(false);
            let mut queue = std::collections::VecDeque::from([start]);
            let mut group = vec![start];
            while let Some(n) = queue.pop_front() {
                let here = colour[n].unwrap_or(false);
                // By index rather than by clone. This is the inner loop of a
                // breadth-first walk over every face of a shell, and cloning
                // the adjacency list on each visit allocates once per face for
                // nothing.
                for k in 0..agree[n].len() {
                    let (m, ok) = agree[n][k];
                    let want = here != !ok;
                    match colour[m] {
                        None => {
                            colour[m] = Some(want);
                            group.push(m);
                            queue.push_back(m);
                        }
                        Some(had) if had != want => unsatisfiable += 1,
                        Some(_) => {}
                    }
                }
            }
            // The constraints fix the group up to one global flip, and for a
            // closed group the volume it encloses decides that flip outright:
            // wound outward it is positive. This is not a preference between
            // two readings — one of them turns the part inside out.
            let volume = group
                .iter()
                .filter_map(|&n| {
                    let patch = faces[members[n]].2.as_ref().ok()?;
                    let flip = colour[n] == Some(true);
                    Some(patch_volume(patch, flip))
                })
                .sum::<f64>();
            let decided = if volume.abs() > 0.0 {
                volume < 0.0
            } else {
                // Nothing enclosed to measure — a sheet, or a group of one
                // face. Keep the file's reading for the greater part of it.
                let weigh = |on: bool| -> usize {
                    group
                        .iter()
                        .filter(|&&n| colour[n] == Some(on))
                        .filter_map(|&n| faces[members[n]].2.as_ref().ok())
                        .map(|p| p.indices.len())
                        .sum()
                };
                weigh(true) > weigh(false)
            };
            if decided {
                for &n in &group {
                    colour[n] = colour[n].map(|c| !c);
                }
            }
        }

        for (n, &i) in members.iter().enumerate() {
            if colour[n] != Some(true) {
                continue;
            }
            let Ok(patch) = &mut faces[i].2 else { continue };
            for t in patch.indices.chunks_exact_mut(3) {
                t.swap(1, 2);
            }
            for v in &mut patch.normals {
                v[0] = -v[0];
                v[1] = -v[1];
                v[2] = -v[2];
            }
            flipped += 1;
        }
    }
    (flipped, unsatisfiable)
}


/// Close the cracks left where two faces discretised the same edge differently.
///
/// Two faces that share an edge in the topology also share its points, and
/// cannot crack. Two faces that share an edge only in *geometry* — a model
/// carrying a duplicated or collapsed edge, which both of these readers see
/// identically because both files describe it — each discretise it for
/// themselves, and one may put a point where the other draws a chord. The
/// point then sits in the middle of the other's edge: the mesh is open along
/// it, and no face can tell, because each drew its own boundary in full.
///
/// The topology cannot say those two edges are one; the geometry can. A vertex
/// that lies on an open edge, within the tolerance that edge was drawn to,
/// splits it. Nothing is moved and nothing is invented — the split point is a
/// vertex that is already in the mesh — so the surface is unchanged and only
/// the crack closes.
fn stitch_t_junctions(
    faces: &mut [(usize, FaceId, Result<face::Patch, String>)],
    sag: f64,
) -> usize {
    if !(sag > 0.0) {
        return 0;
    }
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
    let at = |p: &[f32; 3]| [p[0] as f64, p[1] as f64, p[2] as f64];
    let key_of = |p: &[f64; 3]| {
        [p[0], p[1], p[2]].map(|v| (v * 1e6).round() as i64)
    };

    // Every undirected edge, how often it is used, and one place it is used.
    // Grown into. See the note beside `seen` in `orient_shell`.
    let mut uses: rustc_hash::FxHashMap<(u64, u64), (usize, usize, usize)> = Default::default();
    for (i, (_, _, result)) in faces.iter().enumerate() {
        let Ok(patch) = result else { continue };
        for (t, tri) in patch.indices.chunks_exact(3).enumerate() {
            let k = [
                key(&patch.positions[tri[0] as usize]),
                key(&patch.positions[tri[1] as usize]),
                key(&patch.positions[tri[2] as usize]),
            ];
            for e in 0..3 {
                let (a, b) = (k[e], k[(e + 1) % 3]);
                if a == b {
                    continue;
                }
                let id = if a < b { (a, b) } else { (b, a) };
                let slot = uses.entry(id).or_insert((0, i, t * 3 + e));
                slot.0 += 1;
            }
        }
    }

    // The vertices that bound a crack, which are the only candidates to split
    // one: a point in the middle of a face's interior cannot be a T-junction.
    let open: Vec<(u64, u64, usize, usize)> = uses
        .iter()
        .filter(|(_, (n, _, _))| *n == 1)
        .map(|(k, (_, i, e))| (k.0, k.1, *i, *e))
        .collect();
    if open.is_empty() {
        return 0;
    }
    let mut loose: rustc_hash::FxHashMap<u64, [f64; 3]> = Default::default();
    for (_, _, i, e) in &open {
        let Ok(patch) = &faces[*i].2 else { continue };
        let tri = &patch.indices[(e / 3) * 3..(e / 3) * 3 + 3];
        for c in 0..3 {
            let v = &patch.positions[tri[c] as usize];
            loose.insert(key(v), at(v));
        }
    }

    // Where each open edge has to be split, by the face and corner that owns it.
    let mut splits: rustc_hash::FxHashMap<(usize, usize), Vec<(f64, [f64; 3])>> =
        Default::default();
    let mut found = 0usize;
    for (lo, hi, face, corner) in &open {
        let Ok(patch) = &faces[*face].2 else { continue };
        let tri = &patch.indices[(corner / 3) * 3..(corner / 3) * 3 + 3];
        let e = corner % 3;
        let p = at(&patch.positions[tri[e] as usize]);
        let q = at(&patch.positions[tri[(e + 1) % 3] as usize]);
        let d = [q[0] - p[0], q[1] - p[1], q[2] - p[2]];
        let len2 = d[0] * d[0] + d[1] * d[1] + d[2] * d[2];
        if len2 <= 0.0 {
            continue;
        }
        for (k, r) in &loose {
            if *k == *lo || *k == *hi {
                continue;
            }
            let w = [r[0] - p[0], r[1] - p[1], r[2] - p[2]];
            let t = (w[0] * d[0] + w[1] * d[1] + w[2] * d[2]) / len2;
            if !(t > 1e-6 && t < 1.0 - 1e-6) {
                continue;
            }
            let off = [w[0] - d[0] * t, w[1] - d[1] * t, w[2] - d[2] * t];
            // The chord is allowed to leave the true surface by the sag, so a
            // point on that surface can stand that far off it and no further.
            if (off[0] * off[0] + off[1] * off[1] + off[2] * off[2]).sqrt() > sag {
                continue;
            }
            // One triangle can be reached from more than one of its own open
            // edges, and each pass offers the same loose point again. Placing
            // a point twice on one edge gives the fan a repeated corner, and
            // the triangle built across it is the same facet a second time —
            // which reads as a non-manifold edge in the finished body.
            let here = splits.entry((*face, corner / 3)).or_default();
            if here.iter().any(|(_, s)| key_of(s) == key_of(r)) {
                continue;
            }
            here.push((t, *r));
            found += 1;
        }
    }
    if splits.is_empty() {
        return 0;
    }

    // Rewrite each affected triangle as the polygon it has become. A triangle
    // with points on its edges is convex, so a fan from its first corner is a
    // valid triangulation of it.
    for ((face, tri), _) in splits.iter() {
        let _ = (face, tri);
    }
    let mut by_face: rustc_hash::FxHashMap<usize, rustc_hash::FxHashMap<usize, Vec<(usize, f64, [f64; 3])>>> =
        Default::default();
    for ((face, tri), points) in splits {
        // Which of the triangle's three edges each split belongs to is
        // recovered from the corner the edge was found on.
        let entry = by_face.entry(face).or_default().entry(tri).or_default();
        for (t, r) in points {
            entry.push((usize::MAX, t, r));
        }
    }

    for (face, tris) in by_face {
        let Ok(patch) = &mut faces[face].2 else { continue };
        let mut out: Vec<u32> = Vec::with_capacity(patch.indices.len() + tris.len() * 3);
        for (t, tri) in patch.indices.chunks_exact(3).enumerate() {
            let Some(points) = tris.get(&t) else {
                out.extend_from_slice(tri);
                continue;
            };
            // Place each split on whichever of the three edges it lies along.
            let corner_of = |r: [f64; 3]| -> Option<(usize, f64)> {
                let mut best: Option<(usize, f64, f64)> = None;
                for e in 0..3 {
                    let p = at(&patch.positions[tri[e] as usize]);
                    let q = at(&patch.positions[tri[(e + 1) % 3] as usize]);
                    let d = [q[0] - p[0], q[1] - p[1], q[2] - p[2]];
                    let len2 = d[0] * d[0] + d[1] * d[1] + d[2] * d[2];
                    if len2 <= 0.0 {
                        continue;
                    }
                    let w = [r[0] - p[0], r[1] - p[1], r[2] - p[2]];
                    let s = (w[0] * d[0] + w[1] * d[1] + w[2] * d[2]) / len2;
                    if !(s > 1e-6 && s < 1.0 - 1e-6) {
                        continue;
                    }
                    let off = [w[0] - d[0] * s, w[1] - d[1] * s, w[2] - d[2] * s];
                    let dist = (off[0] * off[0] + off[1] * off[1] + off[2] * off[2]).sqrt();
                    if best.is_none_or(|(_, _, b)| dist < b) {
                        best = Some((e, s, dist));
                    }
                }
                best.map(|(e, s, _)| (e, s))
            };

            let mut on_edge: [Vec<(f64, [f64; 3])>; 3] = [Vec::new(), Vec::new(), Vec::new()];
            for (_, _, r) in points {
                if let Some((e, s)) = corner_of(*r) {
                    on_edge[e].push((s, *r));
                }
            }
            for v in &mut on_edge {
                v.sort_by(|a, b| a.0.partial_cmp(&b.0).unwrap_or(std::cmp::Ordering::Equal));
                v.dedup_by(|a, b| (a.0 - b.0).abs() < 1e-9);
            }

            // Walk the triangle's boundary, inserting the splits as they come.
            let mut ring: Vec<u32> = Vec::with_capacity(3 + points.len());
            for e in 0..3 {
                ring.push(tri[e]);
                for (s, r) in &on_edge[e] {
                    let na = patch.normals[tri[e] as usize];
                    let nb = patch.normals[tri[(e + 1) % 3] as usize];
                    let mix = |i: usize| na[i] as f64 * (1.0 - s) + nb[i] as f64 * s;
                    let (x, y, z) = (mix(0), mix(1), mix(2));
                    let l = (x * x + y * y + z * z).sqrt().max(1e-30);
                    patch.positions.push([r[0] as f32, r[1] as f32, r[2] as f32]);
                    patch.normals.push([(x / l) as f32, (y / l) as f32, (z / l) as f32]);
                    ring.push(patch.positions.len() as u32 - 1);
                }
            }
            for k in 1..ring.len().saturating_sub(1) {
                out.extend_from_slice(&[ring[0], ring[k], ring[k + 1]]);
            }
        }
        patch.indices = out;
    }
    found
}

fn escapes_body(patch: &face::Patch, reference: &cad_ir::math::Aabb) -> bool {
    // A reference that spans nothing cannot judge anything. An O-ring is a
    // single toroidal face whose only bound is one vertex — no curves, no
    // second point — so the body's extent comes out as that one point, its
    // diagonal is zero, and every patch "reaches further than the body". Both
    // parts of the pilot assembly that this rejected were correct: a torus of
    // 10.2 mm rejected for reaching 10.2 mm from a body 0.0 mm across.
    if reference.is_empty() || !(reference.diagonal() > 0.0) {
        return false;
    }
    farthest(patch, reference) > reference.diagonal()
}

/// Tessellate one solid into a single mesh with per-material index runs.
pub fn tessellate_solid(
    name: &str,
    solid: &Solid,
    material: Option<MaterialId>,
    face_materials: &[Option<MaterialId>],
    options: &options::Resolved,
) -> (Mesh, Report) {
    let edges = edge::discretise_all(solid, options);

    // Per-face work is independent once the edge chains exist, which is where
    // the parallelism belongs: a face is a coherent unit of a few hundred
    // triangles, while a whole solid can be one face or ten thousand.
    let faces: Vec<_> = solid
        .shell_faces()
        .collect::<Vec<_>>()
        .into_par_iter()
        .map(|(shell_index, fid)| {
            // One face must not be able to lose the model. The triangulator
            // reaches a third-party constrained-Delaunay library, and a
            // boundary it cannot cope with aborts the thread — which, before
            // this, took the whole conversion with it and printed nothing at
            // all. A face that panics is a face that failed, no more, and it
            // is reported as one.
            let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                face::tessellate(solid, fid, &edges, options)
            }))
            .unwrap_or_else(|_| {
                Err("the triangulation aborted on this face's boundary".to_string())
            });
            (shell_index, fid, result)
        })
        .collect();

    let reference = solid.geometric_bounds();

    // How big the mesh will be, before a single vertex goes in.
    //
    // A Vec that grows by doubling keeps as much again as it needs, and the
    // scene keeps that for as long as it keeps the mesh — on the pilot, 82.5 MB
    // taken to hold 53.7 MB, and it is held for the life of the conversion.
    // Every patch that will be accepted is already in hand here, so the totals
    // are a sum rather than a guess.
    //
    // Two lengths per patch, and nothing else. Running the acceptance test here
    // as well would be exact, but `escapes_body` calls `farthest` — a square
    // root per vertex over 1.2 million of them — and doing it in a pass of its
    // own touches every patch twice, once cold. That cost 25% of the
    // conversion's wall clock to save a reservation that was already almost
    // exact: on the pilot every one of 11 214 faces is accepted, so the
    // over-reservation is the rejected faces, and there are none.
    let (mut want_vertices, mut want_indices) = (0usize, 0usize);
    for (_, _, result) in faces.iter() {
        if let Ok(patch) = result {
            want_vertices += patch.positions.len();
            want_indices += patch.indices.len();
        }
    }

    let mut mesh = Mesh::default();
    mesh.positions.reserve_exact(want_vertices);
    // Normals are pushed beside positions at every site that pushes either, and
    // `Mesh` documents them as empty or exactly as long as positions.
    mesh.normals.reserve_exact(want_vertices);
    mesh.indices.reserve_exact(want_indices);
    let mut face_ranges: Vec<(FaceId, u32, usize, usize)> = Vec::new();
    let mut crossed: rustc_hash::FxHashSet<u32> = Default::default();
    let mut report = Report::default();
    let default_material = material.map(|m| m.0).unwrap_or(0);


    // The solid's own vertices under-describe a body whose faces are all
    // periodic — a plain shaft has no vertices at all — but where there are
    // plenty they are a sound reference, and a patch far outside them points at
    // a face whose parameter region was reconstructed wrongly.
    let trace_strays = std::env::var_os("CAD_TESS_TRACE_STRAY")
        .is_some_and(|v| v.to_string_lossy() == "1" || name.contains(&*v.to_string_lossy()))
        && !reference.is_empty();

    // Triangles are grouped by material as they arrive, so the finished mesh
    // has one contiguous index run per material rather than a run per face.
    let mut runs: std::collections::BTreeMap<u32, Vec<u32>> = Default::default();

    // The file's per-face sense flags contradict each other on a minority of
    // faces; the shared edges say which minority.
    let mut faces = faces;
    let (flipped, unsatisfiable) = orient_shell(&mut faces);
    let doubled = |faces: &[(usize, FaceId, Result<face::Patch, String>)]| -> usize {
        faces.iter().filter_map(|(_, _, r)| r.as_ref().ok()).map(face::repeated_facets).sum()
    };
    let watching = std::env::var_os("CAD_TESS_FOLD").is_some();
    let before_stitch = if watching { doubled(&faces) } else { 0 };
    // Which face's triangles come nearest a world point, for chasing a hole
    // that the per-body probes cannot attribute. Scene millimetres, the
    // solid's own frame.
    if let Ok(at) = std::env::var("CAD_TESS_NEAREST_FACE") {
        let c: Vec<f64> = at.split(',').filter_map(|v| v.trim().parse().ok()).collect();
        if c.len() == 3 {
            let q = cad_ir::Vec3::new(c[0], c[1], c[2]);
            let mut best: Option<(f64, u32, usize, bool)> = None;
            for (_, fid, r) in faces.iter() {
                let Ok(patch) = r else { continue };
                for v in &patch.positions {
                    let d = (cad_ir::Vec3::new(v[0] as f64, v[1] as f64, v[2] as f64) - q).length();
                    if best.is_none_or(|(b, _, _, _)| d < b) {
                        best = Some((d, fid.0, patch.indices.len() / 3, patch.rebuilt));
                    }
                }
            }
            if let Some((d, f, tris, rebuilt)) = best
                && d < 5.0
            {
                println!(
                    "[nearest-face] {name}: face {f} ({}, {tris} triangles, {}) has a vertex {d:.4} mm from the point",
                    face::surface_kind(solid.surface(solid.face(FaceId(f)).surface)),
                    if rebuilt { "rebuilt" } else { "surface" }
                );
            }
        }
    }
    let stitched = stitch_t_junctions(&mut faces, options.sag);
    if watching {
        println!(
            "[fold] {name}: facets laid twice — {before_stitch} before stitching, {} after",
            doubled(&faces)
        );
        for (_, fid, r) in &faces {
            let Ok(patch) = r else { continue };
            let n = face::repeated_facets(patch);
            if n > 0 {
                println!(
                    "[fold]   face {} {} {} lays {n} facet(s) twice, of {}",
                    fid.0,
                    face::surface_kind(solid.surface(solid.face(*fid).surface)),
                    if patch.rebuilt { "rebuilt from its boundary" } else { "meshed from its surface" },
                    patch.indices.len() / 3
                );
            }
        }
    }
    if std::env::var_os("CAD_TESS_WIND").is_some() {
        println!(
            "[wind] {name}: flipped {flipped} faces, {unsatisfiable} constraints \
             unsatisfiable, {stitched} T-junctions stitched"
        );
    }
    let faces = faces;

    // Which faces are wound against their neighbours. Two triangles meeting
    // at an edge must traverse it in opposite directions, or one of them is
    // inside out — the mesh is closed, but a renderer culls the wrong side of
    // it and any tool that works from winding is misled. Nothing inside a
    // single face can see this: it takes the whole shell.
    if std::env::var_os("CAD_TESS_WIND").is_some() {
        let mut used: rustc_hash::FxHashMap<(u64, u64), (usize, u32)> = Default::default();
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
        let mut against: rustc_hash::FxHashMap<u32, usize> = Default::default();
        for (_, fid, result) in &faces {
            let Ok(patch) = result else { continue };
            for t in patch.indices.chunks_exact(3) {
                let k: Vec<u64> = t.iter().map(|&i| key(&patch.positions[i as usize])).collect();
                for e in 0..3 {
                    let (a, b) = (k[e], k[(e + 1) % 3]);
                    if a == b {
                        continue;
                    }
                    let (lo, hi, dir) = if a < b { (a, b, 1usize) } else { (b, a, 0usize) };
                    match used.entry((lo, hi)) {
                        std::collections::hash_map::Entry::Occupied(o) => {
                            let (other_dir, other_face) = *o.get();
                            if other_dir == dir {
                                *against.entry(fid.0).or_default() += 1;
                                *against.entry(other_face).or_default() += 1;
                            }
                        }
                        std::collections::hash_map::Entry::Vacant(v) => {
                            v.insert((dir, fid.0));
                        }
                    }
                }
            }
        }
        // Which faces share an edge with more than one other: an edge used by
        // four triangles is two sheets meeting, and naming the faces says
        // whether four of them meet there or two each use it twice.
        {
            let mut uses: rustc_hash::FxHashMap<(u64, u64), Vec<u32>> = Default::default();
            for (_, fid, result) in &faces {
                let Ok(patch) = result else { continue };
                for t in patch.indices.chunks_exact(3) {
                    let k: Vec<u64> =
                        t.iter().map(|&i| key(&patch.positions[i as usize])).collect();
                    for e in 0..3 {
                        let (a, b) = (k[e], k[(e + 1) % 3]);
                        if a == b {
                            continue;
                        }
                        let id = if a < b { (a, b) } else { (b, a) };
                        uses.entry(id).or_default().push(fid.0);
                    }
                }
            }
            // Does the file itself give that face the same edge twice? A
            // loop that runs out along an edge and back is a slit, and a slit
            // is non-manifold in any triangle mesh — the two sides are the
            // same surface. If the file says so, the mesh is right to say so.
            let repeated = |f: u32| -> usize {
                let face = solid.face(FaceId(f));
                let mut ids: Vec<usize> = face
                    .bounds
                    .iter()
                    .flat_map(|b| b.halves.iter().map(|h| h.edge.index()))
                    .collect();
                ids.sort_unstable();
                let total = ids.len();
                ids.dedup();
                total - ids.len()
            };
            let mut shown = 0;
            for (_, who) in uses.iter().filter(|(_, v)| v.len() > 2) {
                if shown >= 6 {
                    break;
                }
                let mut faces_here = who.clone();
                faces_here.sort_unstable();
                let distinct = {
                    let mut d = faces_here.clone();
                    d.dedup();
                    d
                };
                println!(
                    "[nonmanifold] {name}: used {} times by {} distinct faces {:?}; \
                     the file gives them {} repeated edges",
                    who.len(),
                    distinct.len(),
                    distinct,
                    distinct.iter().map(|f| repeated(*f)).sum::<usize>(),
                );
                for f in &distinct {
                    let folds = faces
                        .iter()
                        .find(|(_, id, _)| id.0 == *f)
                        .and_then(|(_, _, r)| r.as_ref().ok())
                        .map(face::self_overlaps)
                        .unwrap_or(0);
                    let _ = folds;
                    let fid = FaceId(*f);
                    let rebuilt = faces
                        .iter()
                        .find(|(_, id, _)| id.0 == *f)
                        .and_then(|(_, _, r)| r.as_ref().ok())
                        .map(|p| p.rebuilt)
                        .unwrap_or(false);
                    println!(
                        "[nonmanifold]   face {f} {} {}",
                        face::surface_kind(solid.surface(solid.face(fid).surface)),
                        if rebuilt { "rebuilt from its boundary" } else { "meshed from its surface" }
                    );

                    if folds > 0 {
                        println!("[nonmanifold]     it lies on itself along {folds} edges");
                    }
                }
                shown += 1;
            }
        }

        let mut ranked: Vec<_> = against.into_iter().collect();
        ranked.sort_by(|a, b| b.1.cmp(&a.1));
        for (f, n) in ranked.iter().take(12) {
            let fid = FaceId(*f);
            println!(
                "[wind] {name} face {f} {} same_sense={} conflicts {n}  {}",
                face::surface_kind(solid.surface(solid.face(fid).surface)),
                solid.face(fid).same_sense,
                match solid.surface(solid.face(fid).surface) {
                    cad_ir::brep::Surface::Torus {
                        major_radius,
                        minor_radius,
                        frame,
                    } => format!(
                        "major {major_radius:.4} minor {minor_radius:.4} axis \
                         [{:.3},{:.3},{:.3}]",
                        frame.axis.x, frame.axis.y, frame.axis.z
                    ),
                    cad_ir::brep::Surface::Cylinder { radius, frame } => format!(
                        "radius {radius:.4} axis [{:.3},{:.3},{:.3}]",
                        frame.axis.x, frame.axis.y, frame.axis.z
                    ),
                    cad_ir::brep::Surface::Cone {
                        radius,
                        half_angle,
                        frame,
                    } => format!(
                        "radius {radius:.4} half_angle {:.4} axis [{:.3},{:.3},{:.3}]",
                        half_angle.to_degrees(),
                        frame.axis.x,
                        frame.axis.y,
                        frame.axis.z
                    ),
                    _ => String::new(),
                }
            );
        }
        println!("[wind] {name}: {} faces wound against a neighbour", ranked.len());
    }

    for (_shell, fid, result) in faces {
        match result {
            Ok(patch) if !patch.indices.is_empty() && !escapes_body(&patch, &reference) => {
                if patch.crossings > 0 {
                    crossed.insert(fid.0);
                }
                if std::env::var_os("CAD_TESS_TRACE_HUGE").is_some() {
                    let far = farthest(&patch, &reference);
                    if far > 1.0e4 || far > reference.diagonal().max(1.0) * 0.9 {
                        eprintln!(
                            "[huge] {name} face {} reaches {far:.0} (ref diag {:.0}) surface={}",
                            fid.0,
                            reference.diagonal(),
                            face::surface_kind(solid.surface(solid.face(fid).surface))
                        );
                    }
                }
                if trace_strays {
                    let worst = farthest(&patch, &reference);
                    if worst > reference.diagonal().max(1.0) * 2.0 {
                        eprintln!(
                            "[stray] {name} face {} reaches {worst:.1} (body spans {:.1}) surface={}",
                            fid.0,
                            reference.diagonal(),
                            face::surface_kind(solid.surface(solid.face(fid).surface))
                        );
                    }
                }

                let base = mesh.positions.len() as u32;
                mesh.positions.extend_from_slice(&patch.positions);
                mesh.normals.extend_from_slice(&patch.normals);
                let material = face_materials
                    .get(fid.index())
                    .copied()
                    .flatten()
                    .map(|m| m.0)
                    .unwrap_or(default_material);
                let run = runs.entry(material).or_default();
                let run_start = run.len();
                run.extend(patch.indices.iter().map(|i| i + base));
                let _ = run_start;
                report.triangles += patch.indices.len() / 3;
                report.faces_ok += 1;
                face_ranges.push((fid, material, run_start, run.len()));
            }
            Ok(patch) => report.failed.push(FaceFailure {
                geometry: name.to_string(),
                face: fid,
                reason: if patch.indices.is_empty() {
                    "the triangulation produced no triangles".to_string()
                } else {
                    format!(
                        "patch reaches {:.1} mm from the body centre, but the body spans {:.1} mm",
                        farthest(&patch, &reference),
                        reference.diagonal()
                    )
                },
            }),
            Err(e) => report.failed.push(FaceFailure {
                geometry: name.to_string(),
                face: fid,
                reason: e,
            }),
        }
    }


    // Lay the runs down one after another and record where each begins, which
    // is what a writer needs to emit one primitive per material.
    let mut order: Vec<(u32, Vec<u32>)> = runs.into_iter().collect();
    order.sort_by_key(|(m, _)| *m);
    let mut base_of: std::collections::BTreeMap<u32, usize> = Default::default();
    for (material, indices) in order {
        if indices.is_empty() {
            continue;
        }
        base_of.insert(material, mesh.indices.len());
        mesh.parts.push(MeshPart {
            material,
            start: mesh.indices.len() as u32,
            count: indices.len() as u32,
        });
        mesh.indices.extend_from_slice(&indices);
    }
    // The per-face ranges were recorded against each material's own run; shift
    // them onto the finished index buffer now that the runs are laid down.
    let ranges: Vec<(FaceId, usize, usize)> = face_ranges
        .iter()
        .map(|(fid, material, lo, hi)| {
            let base = base_of.get(material).copied().unwrap_or(0);
            (*fid, base + lo, base + hi)
        })
        .collect();
    let face_ranges = ranges;
    report.vertices += mesh.positions.len();

    // Per-face crack attribution: weld the finished mesh by exact bit pattern
    // and blame each unshared edge on the face that contributed it. A face
    // whose boundary does not meet its neighbours' shows up here by name and
    // surface kind, which is the only way to tell a lowering bug from a
    // tessellation one.
    if std::env::var_os("CAD_TESS_CRACKS").is_some() {
        let mut ids: rustc_hash::FxHashMap<[u32; 3], u32> = Default::default();
        let mut weld = Vec::with_capacity(mesh.positions.len());
        for p in &mesh.positions {
            let key = [p[0].to_bits(), p[1].to_bits(), p[2].to_bits()];
            let next = ids.len() as u32;
            weld.push(*ids.entry(key).or_insert(next));
        }
        let mut uses: rustc_hash::FxHashMap<(u32, u32), u32> = Default::default();
        for tri in mesh.indices.chunks_exact(3) {
            for k in 0..3 {
                let (a, b) = (weld[tri[k] as usize], weld[tri[(k + 1) % 3] as usize]);
                if a != b {
                    *uses.entry((a.min(b), a.max(b))).or_default() += 1;
                }
            }
        }
        if std::env::var_os("CAD_TESS_TRACE_OPEN").is_some() {
            // Take each open edge back to the B-rep edge it came from, and
            // report how many faces name that edge against how many actually
            // drew it. That separates "the neighbour was never asked" from
            // "the neighbour was asked and drew something else".
            let mut seg_of: std::collections::HashMap<(u32, u32), usize> = Default::default();
            let mut pos_id: std::collections::HashMap<[u32; 3], u32> = Default::default();
            for (i, q) in mesh.positions.iter().enumerate() {
                pos_id
                    .entry([q[0].to_bits(), q[1].to_bits(), q[2].to_bits()])
                    .or_insert(weld[i]);
            }
            let idx_of = |q: [f32; 3]| -> Option<u32> {
                pos_id
                    .get(&[q[0].to_bits(), q[1].to_bits(), q[2].to_bits()])
                    .copied()
            };
            for (ei, chain) in edges.iter().enumerate() {
                for w in chain.points.windows(2) {
                    let a = idx_of([w[0].x as f32, w[0].y as f32, w[0].z as f32]);
                    let b = idx_of([w[1].x as f32, w[1].y as f32, w[1].z as f32]);
                    if let (Some(a), Some(b)) = (a, b) {
                        seg_of.insert((a.min(b), a.max(b)), ei);
                    }
                }
            }
            let mut users: std::collections::HashMap<usize, Vec<u32>> = Default::default();
            for (fi, face) in solid.faces.iter().enumerate() {
                for bd in &face.bounds {
                    for h in &bd.halves {
                        users.entry(h.edge.index()).or_default().push(fi as u32);
                    }
                }
            }
            let meshed: std::collections::BTreeSet<u32> =
                face_ranges.iter().map(|(f, _, _)| f.0).collect();
            for (fid, lo, hi) in &face_ranges {
                let kind = face::surface_kind(solid.surface(solid.face(*fid).surface));
                let mut own: std::collections::HashMap<(u32, u32), usize> = Default::default();
                let mut raw: std::collections::HashMap<(u32, u32), usize> = Default::default();
                for tri in mesh.indices[*lo..*hi].chunks_exact(3) {
                    for k in 0..3 {
                        let (i, j) = (tri[k] as usize, tri[(k + 1) % 3] as usize);
                        let (a, b) = (weld[i], weld[j]);
                        if a != b {
                            *own.entry((a.min(b), a.max(b))).or_default() += 1;
                        }
                        let (a, b) = (i.min(j) as u32, i.max(j) as u32);
                        *raw.entry((a, b)).or_default() += 1;
                    }
                }
                let on_chain = own
                    .iter()
                    .filter(|(_, c)| **c == 1)
                    .filter(|(e, _)| seg_of.contains_key(e))
                    .count();
                let ring: usize = solid
                    .face(*fid)
                    .bounds
                    .iter()
                    .flat_map(|b| b.halves.iter())
                    .map(|h| edges[h.edge.index()].points.len().saturating_sub(1))
                    .sum();
                if on_chain != ring {
                    let sizes: Vec<usize> = solid
                        .face(*fid)
                        .bounds
                        .iter()
                        .map(|b| {
                            b.halves
                                .iter()
                                .map(|h| edges[h.edge.index()].points.len().saturating_sub(1))
                                .sum()
                        })
                        .collect();
                    eprintln!("  [face-mismatch] {kind} on-chain={on_chain} ring={ring} loop-sizes={sizes:?}");
                }
            }
            let mut owner: std::collections::HashMap<(u32, u32), &str> = Default::default();
            for (fid, lo, hi) in &face_ranges {
                let kind = face::surface_kind(solid.surface(solid.face(*fid).surface));
                for tri in mesh.indices[*lo..*hi].chunks_exact(3) {
                    for k in 0..3 {
                        let (a, b) = (weld[tri[k] as usize], weld[tri[(k + 1) % 3] as usize]);
                        if a != b {
                            owner.insert((a.min(b), a.max(b)), kind);
                        }
                    }
                }
            }
            let mut tally: std::collections::BTreeMap<String, usize> = Default::default();
            for (k, _) in uses.iter().filter(|(_, c)| **c == 1) {
                let who = owner.get(k).copied().unwrap_or("?");
                let key = match seg_of.get(k) {
                    None => format!("{who}: not on any edge chain"),
                    Some(ei) => {
                        let u = users.get(ei).map(|v| v.as_slice()).unwrap_or(&[]);
                        let drawn = u.iter().filter(|f| meshed.contains(f)).count();
                        format!("{who}: edge named by {} faces, {drawn} meshed", u.len())
                    }
                };
                *tally.entry(key).or_default() += 1;
            }
            for (k, v) in &tally {
                eprintln!("  [open-trace] {v:>5}  {k}");
            }
        }
        let open = uses.values().filter(|c| **c == 1).count();
        let over = uses.values().filter(|c| **c > 2).count();
        let mut by_kind: std::collections::BTreeMap<&str, (usize, usize)> = Default::default();
        for (fid, lo, hi) in &face_ranges {
            let mut o = 0usize;
            let mut m = 0usize;
            for tri in mesh.indices[*lo..*hi].chunks_exact(3) {
                for k in 0..3 {
                    let (a, b) = (weld[tri[k] as usize], weld[tri[(k + 1) % 3] as usize]);
                    match uses.get(&(a.min(b), a.max(b))) {
                        Some(&1) if a != b => o += 1,
                        Some(&n) if a != b && n > 2 => m += 1,
                        _ => {}
                    }
                }
            }
            let slot = by_kind
                .entry(face::surface_kind(solid.surface(solid.face(*fid).surface)))
                .or_default();
            slot.0 += o;
            slot.1 += m;
        }
        if std::env::var_os("CAD_TESS_NM").is_some() {
            // For every edge used more than twice: is the extra user the same
            // face folding back on itself, or a different face overlapping it?
            let mut owner: std::collections::HashMap<(u32, u32), Vec<u32>> = Default::default();
            for (fid, lo, hi) in &face_ranges {
                for tri in mesh.indices[*lo..*hi].chunks_exact(3) {
                    for k in 0..3 {
                        let (a, b) = (weld[tri[k] as usize], weld[tri[(k + 1) % 3] as usize]);
                        if a != b {
                            owner.entry((a.min(b), a.max(b))).or_default().push(fid.0);
                        }
                    }
                }
            }
            let (mut fold, mut overlap) = (0usize, 0usize);
            let mut faces_involved: std::collections::BTreeSet<u32> = Default::default();
            for (_, fs) in owner.iter().filter(|(_, v)| v.len() > 2) {
                let mut count: std::collections::BTreeMap<u32, usize> = Default::default();
                for f in fs {
                    *count.entry(*f).or_default() += 1;
                }
                // A fold is one face laying three or more triangles on its own
                // edge. Anything else is two faces reaching over the same line.
                if count.values().any(|c| *c > 2) {
                    fold += 1;
                } else {
                    overlap += 1;
                }
                faces_involved.extend(count.keys().copied());
                let mut shape: Vec<usize> = count.values().copied().collect();
                shape.sort_unstable_by(|a, b| b.cmp(a));
                let kinds: Vec<&str> = count
                    .keys()
                    .map(|f| {
                        face::surface_kind(
                            solid.surface(solid.face(cad_ir::brep::FaceId(*f)).surface),
                        )
                    })
                    .collect();
                eprintln!("  [nm-shape] {shape:?} {kinds:?}");
            }
            eprintln!(
                "  [nm] folds={fold} overlaps={overlap} across {} faces",
                faces_involved.len()
            );
        }
        let mut count: std::collections::BTreeMap<&str, usize> = Default::default();
        for (fid, _, _) in &face_ranges {
            *count
                .entry(face::surface_kind(solid.surface(solid.face(*fid).surface)))
                .or_default() += 1;
        }
        for (kind, (o, m)) in &by_kind {
            eprintln!(
                "  [kind] {kind:<12} faces={:<6} open={o} nonmanifold={m}",
                count.get(kind).copied().unwrap_or(0)
            );
        }
        eprintln!(
            "[cracks] {name}: {open} open, {over} non-manifold, of {} edges, {} faces with a folded boundary",
            uses.len(),
            crossed.len()
        );
    }

    // The 10.1 MB of capacity left over here has been tried and left alone.
    //
    // The reservation above sums the patches; the mesh that comes out is
    // smaller, because stitching merges the vertices two faces share and the
    // seam pass drops facets a pole would draw twice — 1 442 102 patch
    // vertices become 1 216 308, 7 309 428 patch indices become 6 131 796.
    // `shrink_to_fit` on the four buffers does remove every byte of it, and
    // measured against this same code without it, four interleaved runs each:
    // the Parasolid went 260 -> 257 MB, inside the spread, and the STEP's
    // readings got no better and possibly worse. The peak is reached during
    // tessellation, where a shrink is a second copy of the buffer being
    // shrunk, and it pays for the slack it removes.
    //
    // Reserving the final size instead is not available: it is not known
    // until the stitching has run, and a pass to find it cost a quarter of
    // the wall clock.

    (mesh, report)
}

#[cfg(test)]
mod tests {

    /// A face whose boundary crosses the seam of a periodic surface, sampled
    /// so coarsely that the crossing looks like a step backwards. Neither
    /// reading can be shown right from the two points alone, so both have to
    /// be built and the one that closes the face kept.
    #[test]
    fn a_seam_crossing_read_two_ways_keeps_the_reading_that_closes() {
        use cad_ir::brep::{Bound, Curve, Edge, Face, HalfEdge, Shell, Surface};
        use cad_ir::math::{Frame, Interval};

        // A cylindrical band, its two ends closed by full circles.
        let radius = 10.0;
        let mut solid = Solid {
            vertices: vec![Vec3::new(radius, 0.0, 0.0), Vec3::new(radius, 0.0, 8.0)],
            curves: vec![
                Curve::Circle {
                    frame: Frame::new(Vec3::ZERO, Vec3::Z, Vec3::X),
                    radius,
                },
                Curve::Circle {
                    frame: Frame::new(Vec3::new(0.0, 0.0, 8.0), Vec3::Z, Vec3::X),
                    radius,
                },
            ],
            surfaces: vec![Surface::Cylinder {
                frame: Frame::new(Vec3::ZERO, Vec3::Z, Vec3::X),
                radius,
            }],
            ..Default::default()
        };
        let full = Interval::new(0.0, cad_ir::math::TAU);
        solid.edges = (0..2)
            .map(|i| Edge {
                curve: cad_ir::brep::CurveId(i),
                start: cad_ir::brep::VertexId(i),
                end: cad_ir::brep::VertexId(i),
                range: full,
                tolerance: 1e-6,
                same_sense: i == 0,
            })
            .collect();
        solid.faces = vec![Face {
            surface: cad_ir::brep::SurfaceId(0),
            same_sense: true,
            bounds: (0..2)
                .map(|i| Bound {
                    outer: i == 0,
                    halves: vec![HalfEdge {
                        edge: cad_ir::brep::EdgeId(i),
                        forward: i == 0,
                        pcurve: None,
                    }],
                    vertex: None,
                })
                .collect(),
        }];
        solid.shells = vec![Shell {
            faces: vec![cad_ir::brep::FaceId(0)],
            is_void: false,
            closed: true,
        }];

        let opts = Options::default().resolve(20.0);
        let (mesh, report) = tessellate_solid("band", &solid, None, &[], &opts);
        assert_eq!(report.failed.len(), 0, "{:?}", report.failed);

        // The band's whole area, not a sliver of it.
        let area: f64 = mesh
            .indices
            .chunks_exact(3)
            .map(|t| {
                let p = |i: u32| {
                    let q = mesh.positions[i as usize];
                    Vec3::new(q[0] as f64, q[1] as f64, q[2] as f64)
                };
                let (a, b, c) = (p(t[0]), p(t[1]), p(t[2]));
                (b - a).cross(c - a).length() * 0.5
            })
            .sum();
        let want = cad_ir::math::TAU * radius * 8.0;
        assert!(
            (area - want).abs() < want * 0.05,
            "the band came out {area:.1} of its {want:.1}"
        );
    }
    use super::*;
    use cad_ir::math::Vec3;

    /// A cone whose apex arrives as a bound carrying a zero-length edge —
    /// which is how Parasolid states it, having no vertex-loop concept — must
    /// mesh, and mesh closed. Read as an ordinary loop that bound collapses to
    /// nothing and is discarded, which leaves the face with one wrapping ring
    /// and, when the surface's own domain cannot supply a point within reach,
    /// nothing at all to close onto.
    #[test]
    fn a_cone_apex_written_as_a_collapsed_edge_still_meshes() {
        use cad_ir::brep::{Bound, Curve, Edge, Face, HalfEdge, Shell, Surface};
        use cad_ir::math::{Frame, Interval};

        let apex = Vec3::new(0.0, 0.0, 8.0);
        let mut solid = Solid {
            vertices: vec![Vec3::new(5.0, 0.0, 0.0), apex],
            curves: vec![
                Curve::Circle {
                    frame: Frame::new(Vec3::ZERO, Vec3::Z, Vec3::X),
                    radius: 5.0,
                },
                // The apex, written as a circle of no radius at all.
                Curve::Circle {
                    frame: Frame::new(apex, Vec3::Z, Vec3::X),
                    radius: 0.0,
                },
            ],
            surfaces: vec![Surface::Cone {
                frame: Frame::new(Vec3::ZERO, Vec3::Z, Vec3::X),
                radius: 5.0,
                half_angle: -(5.0f64 / 8.0).atan(),
            }],
            ..Default::default()
        };
        let full = Interval::new(0.0, cad_ir::math::TAU);
        solid.edges = vec![
            Edge {
                curve: cad_ir::brep::CurveId(0),
                start: cad_ir::brep::VertexId(0),
                end: cad_ir::brep::VertexId(0),
                range: full,
                tolerance: 1e-6,
                same_sense: true,
            },
            Edge {
                curve: cad_ir::brep::CurveId(1),
                start: cad_ir::brep::VertexId(1),
                end: cad_ir::brep::VertexId(1),
                range: full,
                tolerance: 1e-6,
                same_sense: true,
            },
        ];
        let ring = |edge: u32, outer: bool| Bound {
            outer,
            halves: vec![HalfEdge {
                edge: cad_ir::brep::EdgeId(edge),
                forward: true,
                pcurve: None,
            }],
            vertex: None,
        };
        solid.faces = vec![Face {
            surface: cad_ir::brep::SurfaceId(0),
            same_sense: true,
            bounds: vec![ring(0, true), ring(1, false)],
        }];
        solid.shells = vec![Shell {
            faces: vec![cad_ir::brep::FaceId(0)],
            is_void: false,
            closed: true,
        }];

        let opts = Options::default().resolve(10.0);
        let (mesh, report) = tessellate_solid("cone", &solid, None, &[], &opts);
        assert_eq!(report.failed.len(), 0, "{:?}", report.failed);
        assert!(report.triangles > 0);
        assert!(!mesh.is_empty());

        // The collapsed bound must not be drawn as boundary: every point of it
        // is the same point, so any triangle edge along it would be a crack.
        let at_apex = mesh
            .positions
            .iter()
            .filter(|q| {
                (Vec3::new(q[0] as f64, q[1] as f64, q[2] as f64) - apex).length() < 1e-3
            })
            .count();
        assert!(at_apex > 0, "the apex is missing from the mesh");
    }

    /// A washer: two concentric circles with a rim far narrower than the
    /// spacing of the points describing them. This is the shape three faces of
    /// the pilot assembly have, and the one the region fill tears the rim out
    /// of.
    /// The same washer, but with the inner boundary written the way the file
    /// writes it on three faces of the pilot assembly: not one circle but a
    /// ring of arcs with two straight flats across it — the shape a milled
    /// slot leaves. Those three faces are the ones the tessellator still
    /// cannot close, so the fixture has to have the flats in it.
    #[test]
    fn a_thin_annulus_with_flats_keeps_its_whole_rim() {
        use cad_ir::brep::{Bound, Curve, Edge, Face, HalfEdge, Shell, Surface};
        use cad_ir::math::{Frame, Interval};

        let (outer_r, inner_r) = (37.0, 35.5);
        // The flats cut the inner ring at ±33 degrees about the x axis.
        let cut = 33f64.to_radians();
        let flat_a = Vec3::new(inner_r * cut.cos(), inner_r * cut.sin(), 0.0);
        let flat_b = Vec3::new(inner_r * cut.cos(), -inner_r * cut.sin(), 0.0);
        let far_a = Vec3::new(-inner_r * cut.cos(), inner_r * cut.sin(), 0.0);
        let far_b = Vec3::new(-inner_r * cut.cos(), -inner_r * cut.sin(), 0.0);

        let mut solid = Solid {
            vertices: vec![
                Vec3::new(outer_r, 0.0, 0.0),
                flat_a,
                far_a,
                far_b,
                flat_b,
            ],
            curves: vec![
                Curve::Circle {
                    frame: Frame::new(Vec3::ZERO, Vec3::Z, Vec3::X),
                    radius: outer_r,
                },
                Curve::Circle {
                    frame: Frame::new(Vec3::ZERO, Vec3::Z, Vec3::X),
                    radius: inner_r,
                },
                Curve::Line {
                    origin: flat_a,
                    direction: far_a - flat_a,
                },
                Curve::Line {
                    origin: far_b,
                    direction: flat_b - far_b,
                },
            ],
            surfaces: vec![Surface::Plane {
                frame: Frame::new(Vec3::ZERO, Vec3::Z, Vec3::X),
            }],
            ..Default::default()
        };
        let v = |i: u32| cad_ir::brep::VertexId(i);
        solid.edges = vec![
            // The outer circle, closed on its own seam.
            Edge {
                curve: cad_ir::brep::CurveId(0),
                start: v(0),
                end: v(0),
                range: Interval::new(0.0, cad_ir::math::TAU),
                tolerance: 1e-6,
                same_sense: true,
            },
            // The inner ring: arc, flat, arc, flat.
            Edge {
                curve: cad_ir::brep::CurveId(1),
                start: v(4),
                end: v(1),
                range: Interval::new(-cut, cut),
                tolerance: 1e-6,
                same_sense: true,
            },
            Edge {
                curve: cad_ir::brep::CurveId(2),
                start: v(1),
                end: v(2),
                range: Interval::new(0.0, 1.0),
                tolerance: 1e-6,
                same_sense: true,
            },
            Edge {
                curve: cad_ir::brep::CurveId(1),
                start: v(2),
                end: v(3),
                range: Interval::new(std::f64::consts::PI - cut, std::f64::consts::PI + cut),
                tolerance: 1e-6,
                same_sense: true,
            },
            Edge {
                curve: cad_ir::brep::CurveId(3),
                start: v(3),
                end: v(4),
                range: Interval::new(0.0, 1.0),
                tolerance: 1e-6,
                same_sense: true,
            },
        ];
        let half = |e: u32, forward: bool| HalfEdge {
            edge: cad_ir::brep::EdgeId(e),
            forward,
            pcurve: None,
        };
        solid.faces = vec![Face {
            surface: cad_ir::brep::SurfaceId(0),
            same_sense: true,
            bounds: vec![
                Bound {
                    outer: true,
                    halves: vec![half(0, true)],
                    vertex: None,
                },
                Bound {
                    outer: false,
                    halves: vec![half(1, false), half(4, false), half(3, false), half(2, false)],
                    vertex: None,
                },
            ],
        }];
        solid.shells = vec![Shell {
            faces: vec![cad_ir::brep::FaceId(0)],
            is_void: false,
            closed: true,
        }];

        let opts = Options::default().resolve(74.0);
        let (mesh, report) = tessellate_solid("slotted washer", &solid, None, &[], &opts);
        assert_eq!(report.failed.len(), 0, "{:?}", report.failed);

        // The area between the circle and the flattened ring, exactly. The
        // flats are the chords from +cut to pi-cut and back, so each cuts off
        // a segment subtending pi - 2*cut.
        let ring_area = {
            let alpha = std::f64::consts::PI - 2.0 * cut;
            let segment = 0.5 * inner_r * inner_r * (alpha - alpha.sin());
            std::f64::consts::PI * inner_r * inner_r - 2.0 * segment
        };
        let want = std::f64::consts::PI * outer_r * outer_r - ring_area;
        let area: f64 = mesh
            .indices
            .chunks_exact(3)
            .map(|t| {
                let p = |i: u32| {
                    let q = mesh.positions[i as usize];
                    Vec3::new(q[0] as f64, q[1] as f64, q[2] as f64)
                };
                let (a, b, c) = (p(t[0]), p(t[1]), p(t[2]));
                (b - a).cross(c - a).length() * 0.5
            })
            .sum();
        assert!(
            (area - want).abs() < want * 0.05,
            "the mesh covers {area:.1} of the face's {want:.1}"
        );
    }

    #[test]
    fn a_thin_annulus_keeps_its_whole_rim() {
        use cad_ir::brep::{Bound, Curve, Edge, Face, HalfEdge, Shell, Surface};
        use cad_ir::math::{Frame, Interval};

        let (outer_r, inner_r) = (37.0, 35.5);
        let mut solid = Solid {
            vertices: vec![
                Vec3::new(outer_r, 0.0, 0.0),
                Vec3::new(inner_r, 0.0, 0.0),
            ],
            curves: vec![
                Curve::Circle {
                    frame: Frame::new(Vec3::ZERO, Vec3::Z, Vec3::X),
                    radius: outer_r,
                },
                Curve::Circle {
                    frame: Frame::new(Vec3::ZERO, Vec3::Z, Vec3::X),
                    radius: inner_r,
                },
            ],
            surfaces: vec![Surface::Plane {
                frame: Frame::new(Vec3::ZERO, Vec3::Z, Vec3::X),
            }],
            ..Default::default()
        };
        let full = Interval::new(0.0, cad_ir::math::TAU);
        solid.edges = (0..2)
            .map(|i| Edge {
                curve: cad_ir::brep::CurveId(i),
                start: cad_ir::brep::VertexId(i),
                end: cad_ir::brep::VertexId(i),
                range: full,
                tolerance: 1e-6,
                same_sense: i == 0,
            })
            .collect();
        solid.faces = vec![Face {
            surface: cad_ir::brep::SurfaceId(0),
            same_sense: true,
            bounds: (0..2)
                .map(|i| Bound {
                    outer: i == 0,
                    halves: vec![HalfEdge {
                        edge: cad_ir::brep::EdgeId(i),
                        forward: i == 0,
                        pcurve: None,
                    }],
                    vertex: None,
                })
                .collect(),
        }];
        solid.shells = vec![Shell {
            faces: vec![cad_ir::brep::FaceId(0)],
            is_void: false,
            closed: true,
        }];

        let opts = Options::default().resolve(74.0);
        let (mesh, report) = tessellate_solid("washer", &solid, None, &[], &opts);
        assert_eq!(report.failed.len(), 0, "{:?}", report.failed);

        // The rim is the whole face: every triangle has to lie in it, and the
        // area they cover has to be the annulus's own.
        let area: f64 = mesh
            .indices
            .chunks_exact(3)
            .map(|t| {
                let p = |i: u32| {
                    let q = mesh.positions[i as usize];
                    Vec3::new(q[0] as f64, q[1] as f64, q[2] as f64)
                };
                let (a, b, c) = (p(t[0]), p(t[1]), p(t[2]));
                (b - a).cross(c - a).length() * 0.5
            })
            .sum();
        let want = std::f64::consts::PI * (outer_r * outer_r - inner_r * inner_r);
        assert!(
            (area - want).abs() < want * 0.05,
            "the mesh covers {area:.1} of the annulus's {want:.1}"
        );
    }

    #[test]
    fn an_empty_solid_produces_an_empty_mesh() {
        let solid = Solid::default();
        let opts = Options::default().resolve(10.0);
        let (mesh, report) = tessellate_solid("empty", &solid, None, &[], &opts);
        assert!(mesh.is_empty());
        assert_eq!(report.triangles, 0);
    }

    /// A patch that reaches further from the body's centre than the body's own
    /// diagonal did not come from this body.
    #[test]
    fn a_patch_outside_the_body_is_refused() {
        let mut reference = cad_ir::math::Aabb::EMPTY;
        reference.add_point(Vec3::new(-1.0, -1.0, -1.0));
        reference.add_point(Vec3::new(1.0, 1.0, 1.0));
        let near = face::Patch {
            positions: vec![[0.5, 0.0, 0.0], [0.0, 0.5, 0.0], [0.0, 0.0, 0.5]],
            ..Default::default()
        };
        let far = face::Patch {
            positions: vec![[100.0, 0.0, 0.0], [0.0, 0.5, 0.0], [0.0, 0.0, 0.5]],
            ..Default::default()
        };
        assert!(!escapes_body(&near, &reference));
        assert!(escapes_body(&far, &reference));
    }

    /// An empty reference cannot judge anything, and must not refuse.
    #[test]
    fn nothing_escapes_a_body_with_no_reference() {
        let patch = face::Patch {
            positions: vec![[1e9, 0.0, 0.0]],
            ..Default::default()
        };
        assert!(!escapes_body(&patch, &cad_ir::math::Aabb::EMPTY));
    }

    #[test]
    fn the_success_rate_counts_faces_not_triangles() {
        let mut report = Report {
            faces_ok: 3,
            ..Default::default()
        };
        assert_eq!(report.success_rate(), 1.0);
        report.failed.push(FaceFailure {
            geometry: "x".into(),
            face: FaceId(0),
            reason: "no".into(),
        });
        assert!((report.success_rate() - 0.75).abs() < 1e-12);
    }
}
