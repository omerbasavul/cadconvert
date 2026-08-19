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
pub mod face;
pub mod options;

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

    let materials: Vec<(Option<MaterialId>, Vec<Option<MaterialId>>)> = scene
        .geometry
        .iter()
        .map(|g| (g.material, g.face_materials.clone()))
        .collect();

    let results: Vec<(Mesh, Report)> = scene
        .geometry
        .par_iter()
        .zip(materials.par_iter())
        .map(|(g, (material, face_materials))| match &g.brep {
            Some(solid) => tessellate_solid(&g.name, solid, *material, face_materials, &resolved),
            None => (Mesh::default(), Report::default()),
        })
        .collect();

    let mut report = Report::default();
    for (g, (mesh, r)) in scene.geometry.iter_mut().zip(results) {
        if !mesh.is_empty() {
            g.mesh = Some(mesh);
        }
        report.merge(r);
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
fn escapes_body(patch: &face::Patch, reference: &cad_ir::math::Aabb) -> bool {
    if reference.is_empty() {
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
            let result = face::tessellate(solid, fid, &edges, options);
            (shell_index, fid, result)
        })
        .collect();

    let mut mesh = Mesh::default();
    let mut report = Report::default();
    let default_material = material.map(|m| m.0).unwrap_or(0);

    // The solid's own vertices under-describe a body whose faces are all
    // periodic — a plain shaft has no vertices at all — but where there are
    // plenty they are a sound reference, and a patch far outside them points at
    // a face whose parameter region was reconstructed wrongly.
    let reference = solid.geometric_bounds();
    let trace_strays = std::env::var_os("CAD_TESS_TRACE_STRAY")
        .is_some_and(|v| v.to_string_lossy() == "1" || name.contains(&*v.to_string_lossy()))
        && !reference.is_empty();

    for (_shell, fid, result) in faces {
        match result {
            Ok(patch) if !patch.indices.is_empty() && !escapes_body(&patch, &reference) => {
                if trace_strays {
                    let centre = reference.centre();
                    let limit = reference.diagonal().max(1.0) * 2.0;
                    let worst = patch
                        .positions
                        .iter()
                        .map(|p| {
                            (cad_ir::math::Vec3::new(p[0] as f64, p[1] as f64, p[2] as f64)
                                - centre)
                                .length()
                        })
                        .fold(0.0f64, f64::max);
                    if worst > limit {
                        let f = solid.face(fid);
                        let edges_desc: Vec<String> = f
                            .bounds
                            .iter()
                            .flat_map(|b| b.halves.iter())
                            .map(|h| {
                                let e = solid.edge(h.edge);
                                let c = solid.curve(e.curve);
                                let a = solid.vertex(e.start);
                                let z = solid.vertex(e.end);
                                let mid = c.point_at(e.range.at(0.5));
                                format!(
                                    "{:?}/range[{:.4},{:.4}]/mid{:.0}",
                                    std::mem::discriminant(c),
                                    e.range.lo,
                                    e.range.hi,
                                    (mid - (a + z) * 0.5).length()
                                )
                            })
                            .collect();
                        eprintln!(
                            "[stray] {name} face {} reaches {worst:.1} (body spans {limit:.1}) \
                             surface={:?} edges={:?}",
                            fid.0,
                            std::mem::discriminant(solid.surface(f.surface)),
                            edges_desc
                        );
                    }
                }
                let material = face_materials
                    .get(fid.index())
                    .copied()
                    .flatten()
                    .map(|m| m.0)
                    .unwrap_or(default_material);
                let base = mesh.positions.len() as u32;
                let start = mesh.indices.len() as u32;
                mesh.positions.extend_from_slice(&patch.positions);
                mesh.normals.extend_from_slice(&patch.normals);
                mesh.indices.extend(patch.indices.iter().map(|i| i + base));
                mesh.parts.push(MeshPart {
                    material,
                    start,
                    count: mesh.indices.len() as u32 - start,
                });
                report.faces_ok += 1;
            }
            Ok(patch) if patch.indices.is_empty() => report.failed.push(FaceFailure {
                geometry: name.to_string(),
                face: fid,
                reason: "triangulation produced no triangles".into(),
            }),
            // The patch left the body it belongs to. Its boundary passed the
            // per-face check, so the boundary itself is where the error is —
            // an edge range recovered as the wrong arc, or a parameter that
            // jumped near a degenerate point. Dropping the face leaves a hole
            // that is reported; keeping it puts a spike through the model.
            Ok(patch) => report.failed.push(FaceFailure {
                geometry: name.to_string(),
                face: fid,
                reason: format!(
                    "patch reaches {:.1} mm from the body centre, but the body spans {:.1} mm",
                    farthest(&patch, &reference),
                    reference.diagonal()
                ),
            }),
            Err(e) => report.failed.push(FaceFailure {
                geometry: name.to_string(),
                face: fid,
                reason: e,
            }),
        }
    }

    mesh.coalesce_parts();
    report.triangles = mesh.triangle_count();
    report.vertices = mesh.vertex_count();
    (mesh, report)
}

#[cfg(test)]
mod tests {
    use super::*;
    use cad_ir::brep::*;
    use cad_ir::math::{Frame, Interval, Vec3, TAU};

    /// A closed cylinder: two circular caps and one lateral face with a seam.
    ///
    /// Built by hand rather than read from a file so the test depends on
    /// nothing but the IR — and so a regression here points at the tessellator
    /// rather than at a reader.
    pub(crate) fn cylinder_solid(radius: f64, height: f64) -> Solid {
        let frame = Frame::new(Vec3::ZERO, Vec3::Z, Vec3::X);
        let top_frame = Frame::new(Vec3::new(0.0, 0.0, height), Vec3::Z, Vec3::X);

        let mut s = Solid {
            name: "cylinder".into(),
            body_type: BodyType::Solid,
            tolerance: 1e-6,
            ..Default::default()
        };

        // Two vertices on the seam, one per cap.
        s.vertices.push(Vec3::new(radius, 0.0, 0.0));
        s.vertices.push(Vec3::new(radius, 0.0, height));

        // Curves: bottom circle, top circle, the seam line.
        s.curves.push(Curve::Circle { frame, radius });
        s.curves.push(Curve::Circle {
            frame: top_frame,
            radius,
        });
        s.curves.push(Curve::Line {
            origin: Vec3::new(radius, 0.0, 0.0),
            direction: Vec3::new(0.0, 0.0, height),
        });

        s.surfaces.push(Surface::Cylinder { frame, radius });
        s.surfaces.push(Surface::Plane { frame });
        s.surfaces.push(Surface::Plane { frame: top_frame });

        // Edges: both circles are closed edges; the seam runs between them.
        s.edges.push(Edge {
            start: VertexId(0),
            end: VertexId(0),
            curve: CurveId(0),
            same_sense: true,
            range: Interval::new(0.0, TAU),
            tolerance: 1e-6,
        });
        s.edges.push(Edge {
            start: VertexId(1),
            end: VertexId(1),
            curve: CurveId(1),
            same_sense: true,
            range: Interval::new(0.0, TAU),
            tolerance: 1e-6,
        });
        s.edges.push(Edge {
            start: VertexId(0),
            end: VertexId(1),
            curve: CurveId(2),
            same_sense: true,
            range: Interval::new(0.0, 1.0),
            tolerance: 1e-6,
        });

        let he = |edge: u32, forward: bool| HalfEdge {
            edge: EdgeId(edge),
            forward,
            pcurve: None,
        };

        // Lateral face: seam up, top circle, seam down, bottom circle.
        s.faces.push(Face {
            surface: SurfaceId(0),
            same_sense: true,
            bounds: vec![Bound {
                outer: true,
                halves: vec![he(2, true), he(1, true), he(2, false), he(0, false)],
                vertex: None,
            }],
        });
        // Bottom cap, its normal pointing down means same_sense is false.
        s.faces.push(Face {
            surface: SurfaceId(1),
            same_sense: false,
            bounds: vec![Bound {
                outer: true,
                halves: vec![he(0, true)],
                vertex: None,
            }],
        });
        // Top cap.
        s.faces.push(Face {
            surface: SurfaceId(2),
            same_sense: true,
            bounds: vec![Bound {
                outer: true,
                halves: vec![he(1, true)],
                vertex: None,
            }],
        });

        s.shells.push(Shell {
            faces: vec![FaceId(0), FaceId(1), FaceId(2)],
            closed: true,
            is_void: false,
        });
        s
    }

    #[test]
    fn a_cylinder_tessellates_into_a_closed_mesh() {
        let solid = cylinder_solid(10.0, 25.0);
        let opts = Options::default().resolve(30.0);
        let (mesh, report) = tessellate_solid("c", &solid, None, &[], &opts);
        assert!(report.failed.is_empty(), "failures: {:?}", report.failed);
        assert_eq!(report.faces_ok, 3);
        assert!(mesh.triangle_count() > 20, "only {} triangles", mesh.triangle_count());

        // Every position must lie on the cylinder or one of its caps.
        for p in &mesh.positions {
            let r = ((p[0] as f64).powi(2) + (p[1] as f64).powi(2)).sqrt();
            let z = p[2] as f64;
            let on_wall = (r - 10.0).abs() < 0.2;
            let on_cap = (z.abs() < 1e-6 || (z - 25.0).abs() < 1e-6) && r <= 10.0 + 1e-6;
            assert!(on_wall || on_cap, "stray vertex {p:?} r={r} z={z}");
        }
    }

    #[test]
    fn the_mesh_is_watertight_by_shared_edge_positions() {
        let solid = cylinder_solid(4.0, 9.0);
        let opts = Options::default().resolve(10.0);
        let (mesh, _) = tessellate_solid("c", &solid, None, &[], &opts);

        // Weld by exact bit pattern — no tolerance. If the edge cache did its
        // job, coincident vertices from different faces are bit-identical, and
        // every welded edge is used by exactly two triangles.
        let mut ids = rustc_hash::FxHashMap::default();
        let mut welded = Vec::with_capacity(mesh.positions.len());
        for p in &mesh.positions {
            let key = [p[0].to_bits(), p[1].to_bits(), p[2].to_bits()];
            let next = ids.len() as u32;
            welded.push(*ids.entry(key).or_insert(next));
        }

        let mut edge_uses: rustc_hash::FxHashMap<(u32, u32), i32> = Default::default();
        for tri in mesh.indices.chunks_exact(3) {
            for k in 0..3 {
                let a = welded[tri[k] as usize];
                let b = welded[tri[(k + 1) % 3] as usize];
                if a == b {
                    continue; // a degenerate sliver, counted nowhere
                }
                let key = (a.min(b), a.max(b));
                *edge_uses.entry(key).or_default() += 1;
            }
        }
        let open: Vec<_> = edge_uses.iter().filter(|&(_, &n)| n != 2).collect();
        assert!(
            open.is_empty(),
            "{} edges are not shared by exactly two triangles",
            open.len()
        );
    }

    #[test]
    fn triangles_wind_outward() {
        let solid = cylinder_solid(6.0, 12.0);
        let opts = Options::default().resolve(15.0);
        let (mesh, _) = tessellate_solid("c", &solid, None, &[], &opts);

        // The signed volume of a correctly wound closed mesh is positive.
        let mut volume = 0.0f64;
        for tri in mesh.indices.chunks_exact(3) {
            let p: Vec<Vec3> = tri
                .iter()
                .map(|&i| {
                    let v = mesh.positions[i as usize];
                    Vec3::new(v[0] as f64, v[1] as f64, v[2] as f64)
                })
                .collect();
            volume += p[0].dot(p[1].cross(p[2])) / 6.0;
        }
        let exact = std::f64::consts::PI * 36.0 * 12.0;
        assert!(volume > 0.0, "mesh is inside out, volume {volume}");
        // A faceted cylinder always under-fills the true one, never over.
        assert!(
            volume < exact && volume > exact * 0.97,
            "volume {volume} vs exact {exact}"
        );
    }

    #[test]
    fn normals_point_outward_and_are_not_averaged_across_the_rim() {
        let solid = cylinder_solid(5.0, 10.0);
        let opts = Options::default().resolve(12.0);
        let (mesh, _) = tessellate_solid("c", &solid, None, &[], &opts);
        assert_eq!(mesh.normals.len(), mesh.positions.len());

        for (p, n) in mesh.positions.iter().zip(&mesh.normals) {
            let n = Vec3::new(n[0] as f64, n[1] as f64, n[2] as f64);
            assert!((n.length() - 1.0).abs() < 1e-4, "unnormalised {n:?}");
            let z = p[2] as f64;
            if z > 1e-6 && z < 10.0 - 1e-6 {
                // On the wall: the normal is radial, with no z component.
                assert!(n.z.abs() < 1e-4, "wall normal has z: {n:?}");
            }
        }

        // A rim vertex belongs to both the wall and a cap, and must carry a
        // different normal in each — one radial, one axial.
        let rim: Vec<_> = mesh
            .positions
            .iter()
            .zip(&mesh.normals)
            .filter(|(p, _)| (p[2] as f64).abs() < 1e-9)
            .filter(|(p, _)| {
                (((p[0] as f64).powi(2) + (p[1] as f64).powi(2)).sqrt() - 5.0).abs() < 1e-6
            })
            .collect();
        assert!(rim.len() > 4, "expected duplicated rim vertices");
        assert!(
            rim.iter().any(|(_, n)| n[2].abs() > 0.9),
            "no axial normal on the rim"
        );
        assert!(
            rim.iter().any(|(_, n)| n[2].abs() < 0.1),
            "no radial normal on the rim"
        );
    }

    #[test]
    fn a_tighter_tolerance_produces_more_triangles() {
        let solid = cylinder_solid(20.0, 5.0);
        let (a, _) = tessellate_solid("c", &solid, None, &[], &Options::draft().resolve(25.0));
        let (b, _) = tessellate_solid("c", &solid, None, &[], &Options::fine().resolve(25.0));
        assert!(
            b.triangle_count() > a.triangle_count() * 2,
            "draft {} vs fine {}",
            a.triangle_count(),
            b.triangle_count()
        );
    }

    /// Loosening only the sag must not change anything while the angular limit
    /// is the binding constraint — that is the limit doing its job, and a
    /// regression here would mean small holes silently becoming polygons.
    #[test]
    fn the_angular_limit_holds_the_floor_when_sag_is_generous() {
        let solid = cylinder_solid(20.0, 5.0);
        let count = |sag: f64| {
            let o = Options {
                linear_deflection: sag,
                relative: false,
                angular_deflection: 20f64.to_radians(),
                ..Options::default()
            }
            .resolve(25.0);
            tessellate_solid("c", &solid, None, &[], &o).0.triangle_count()
        };
        assert_eq!(count(5.0), count(50.0));

        // Removing the angular limit lets the generous sag through.
        let unlimited = Options {
            linear_deflection: 5.0,
            relative: false,
            angular_deflection: std::f64::consts::PI,
            ..Options::default()
        }
        .resolve(25.0);
        let loose = tessellate_solid("c", &solid, None, &[], &unlimited).0.triangle_count();
        assert!(loose < count(5.0), "angular limit had no effect");
    }
}
