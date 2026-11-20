//! One body's faces, each meshed on its own and timed.
//!
//! `cargo run --release -p cad-xt --example face_probe -- file.x_t [face,face,...]`
//!
//! `xt_mesh` reports a model; this reports its faces, which is what is wanted
//! when a model takes two minutes and the question is which face takes them.
//! With a list of face numbers only those are meshed, so a probe that prints
//! per face — `CAD_TESS_FACE`, `CAD_TESS_TRACE` — says something about one
//! face in a second rather than about three hundred in a hundred.

use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Instant;

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let mut args = std::env::args().skip(1);
    let Some(path) = args.next() else {
        eprintln!("usage: face_probe <file.x_t> [face,face,...]");
        std::process::exit(2);
    };
    let only: Option<Vec<u32>> = args
        .next()
        .map(|list| list.split(',').filter_map(|v| v.trim().parse().ok()).collect());

    let t0 = Instant::now();
    let (scene, report) = cad_xt::scene_from_file(&path, &cad_xt::LowerOptions::default())?;
    println!("lower {:.0} ms", t0.elapsed().as_secs_f64() * 1e3);
    if let Some(t) = &report.truncated {
        println!("TRUNCATED: {t}");
    }
    for s in &report.skipped {
        println!("skip #{}: {}", s.entity, s.reason);
    }

    let scale = scene.vertex_bounds().diagonal();
    // SAG_MM and ANGLE_DEG are the command line's --sag and --angle.
    let mut options = cad_tess::Options::default();
    if let Some(mm) = std::env::var("SAG_MM").ok().and_then(|v| v.parse::<f64>().ok()) {
        options.linear_deflection = mm;
        options.relative = false;
    }
    if let Some(deg) = std::env::var("ANGLE_DEG").ok().and_then(|v| v.parse::<f64>().ok()) {
        options.angular_deflection = deg.to_radians();
    }
    let resolved = options.resolve(scale);
    println!("scale {scale:.3}  sag {:.5}", resolved.sag);

    let body_only = std::env::var("BODY_ONLY").ok();
    for g in &scene.geometry {
        let Some(solid) = &g.brep else { continue };
        if body_only.as_ref().is_some_and(|b| !g.name.contains(b.as_str())) {
            continue;
        }
        let t1 = Instant::now();
        // One edge at a time and named before it is sampled, for the edge
        // that never comes back.
        if std::env::var_os("EDGE_PROBE").is_some() {
            use std::io::Write as _;
            for (i, e) in solid.edges.iter().enumerate() {
                let c = solid.curve(e.curve);
                let nat = c.natural_range();
                println!(
                    "edge {i}: {} range [{:.6},{:.6}] natural [{:.4},{:.4}] period {:?} tolerance {:.3e}",
                    cad_tess::curve_kind(c),
                    e.range.lo,
                    e.range.hi,
                    nat.lo,
                    nat.hi,
                    c.period(),
                    e.tolerance
                );
                std::io::stdout().flush().ok();
                let (a, b) = (solid.vertex(e.start), solid.vertex(e.end));
                let bad = match c {
                    cad_ir::brep::Curve::Polyline { points } => points.iter().filter(|q| !q.is_finite()).count(),
                    _ => 0,
                };
                if bad > 0 || !a.is_finite() || !b.is_finite() {
                    println!("   NOT FINITE: {bad} polyline points; vertices [{:.4},{:.4},{:.4}] [{:.4},{:.4},{:.4}]", a.x, a.y, a.z, b.x, b.y, b.z);
                    if let cad_ir::brep::Curve::Polyline { points } = c {
                        for (k, q) in points.iter().enumerate() {
                            if !q.is_finite() || k < 2 {
                                println!("      point {k}: [{:.5},{:.5},{:.5}]", q.x, q.y, q.z);
                            }
                        }
                    }
                    let faces: Vec<u32> = solid
                        .faces
                        .iter()
                        .enumerate()
                        .filter(|(_, f)| f.bounds.iter().any(|bd| bd.halves.iter().any(|h| h.edge.0 as usize == i)))
                        .map(|(fi, _)| fi as u32)
                        .collect();
                    println!("      used by faces {faces:?}: {:?}", faces.iter().map(|f| cad_tess::surface_kind(solid.surface(solid.face(cad_ir::brep::FaceId(*f)).surface))).collect::<Vec<_>>());
                    continue;
                }
                let dump: Vec<usize> = std::env::var("EDGE_DUMP")
                    .ok()
                    .map(|v| v.split(',').filter_map(|x| x.trim().parse().ok()).collect())
                    .unwrap_or_default();
                if let Some(only) = std::env::var("EDGE_ONLY").ok().and_then(|v| v.parse::<usize>().ok()) {
                    if i != only { continue; }
                    let local = resolved.for_extent(20.0);
                    let chain = cad_tess::edge::discretise(solid, e, &local);
                    println!("   -> {} points (local sag {:.5})", chain.points.len(), local.sag);
                    continue;
                }
                if dump.contains(&i) {
                    println!("   vertices [{:.6},{:.6},{:.6}] [{:.6},{:.6},{:.6}] max_depth {} sag {:.6} angle {:.4}", a.x, a.y, a.z, b.x, b.y, b.z, resolved.max_depth, resolved.sag, resolved.angle);
                    if let cad_ir::brep::Curve::Polyline { points } = c {
                        for (k, q) in points.iter().enumerate() {
                            println!("      point {k}: [{:.6},{:.6},{:.6}]", q.x, q.y, q.z);
                        }
                    }
                    for t in [0.0, 0.5, 1.0, 48.5, 96.5, 97.0] {
                        let q = c.point_at(t);
                        let d = c.derivative_at(t);
                        println!("      at {t}: [{:.6},{:.6},{:.6}] derivative [{:.6},{:.6},{:.6}]", q.x, q.y, q.z, d.x, d.y, d.z);
                    }
                    let faces: Vec<u32> = solid
                        .faces
                        .iter()
                        .enumerate()
                        .filter(|(_, f)| f.bounds.iter().any(|bd| bd.halves.iter().any(|h| h.edge.0 as usize == i)))
                        .map(|(fi, _)| fi as u32)
                        .collect();
                    println!("      used by faces {faces:?}: {:?}", faces.iter().map(|f| cad_tess::surface_kind(solid.surface(solid.face(cad_ir::brep::FaceId(*f)).surface))).collect::<Vec<_>>());
                    continue;
                }
                let chain = cad_tess::edge::discretise(solid, e, &resolved);
                println!("   -> {} points", chain.points.len());
            }
        }
        let edges = cad_tess::edge::discretise_all(solid, &resolved);
        println!(
            "{}: {} faces, {} edges discretised in {:.0} ms",
            g.name,
            solid.faces.len(),
            edges.len(),
            t1.elapsed().as_secs_f64() * 1e3
        );

        let faces: Vec<cad_ir::brep::FaceId> = solid
            .shell_faces()
            .map(|(_, f)| f)
            .filter(|f| only.as_ref().is_none_or(|o| o.contains(&f.0)))
            .collect();
        let next = AtomicUsize::new(0);
        let threads = if only.is_some() {
            1
        } else {
            std::thread::available_parallelism().map_or(4, |n| n.get())
        };
        // NEAR=x,y,z,r marks the faces with a vertex within r of a point.
        let near: Option<([f64; 3], f64)> = std::env::var("NEAR").ok().and_then(|v| {
            let n: Vec<f64> = v.split(',').filter_map(|t| t.trim().parse().ok()).collect();
            (n.len() == 4).then(|| ([n[0], n[1], n[2]], n[3]))
        });
        let mut rows: Vec<(u32, f64, Result<(usize, bool, bool, usize), String>)> = std::thread::scope(|scope| {
            let workers: Vec<_> = (0..threads)
                .map(|_| {
                    scope.spawn(|| {
                        let mut mine = Vec::new();
                        loop {
                            let i = next.fetch_add(1, Ordering::Relaxed);
                            let Some(fid) = faces.get(i) else { break };
                            let t = Instant::now();
                            let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                                cad_tess::face::tessellate(solid, *fid, &edges, &resolved)
                            }))
                            .unwrap_or_else(|_| Err("panicked".to_string()))
                            .map(|p| {
                                let hit = near.is_some_and(|(c, r)| {
                                    p.positions.iter().any(|q| {
                                        let d = [q[0] as f64 - c[0], q[1] as f64 - c[1], q[2] as f64 - c[2]];
                                        (d[0] * d[0] + d[1] * d[1] + d[2] * d[2]).sqrt() <= r
                                    })
                                });
                                if std::env::var_os("DUMP_PATCH").is_some() {
                                    for (k, q) in p.positions.iter().enumerate() {
                                        println!("[patch] vertex {k}: [{:.6},{:.6},{:.6}]", q[0], q[1], q[2]);
                                    }
                                    for t in p.indices.chunks_exact(3) {
                                        println!("[patch] triangle {} {} {}", t[0], t[1], t[2]);
                                    }
                                }
                                // OFF_SURFACE: how far the patch's own vertices sit from
                                // the surface the face lies on — what a face meshed from
                                // its boundary costs in shape.
                                if std::env::var_os("OFF_SURFACE").is_some() {
                                    let surface = solid.surface(solid.face(*fid).surface);
                                    let mut worst = 0.0f64;
                                    let mut at = None;
                                    let d = surface.domain();
                                    for q in &p.positions {
                                        let x = cad_ir::math::Vec3::new(q[0] as f64, q[1] as f64, q[2] as f64);
                                        if let Some(mut uv) = surface.nearest(x, None) {
                                            let mut off = (surface.point_at(uv) - x).length();
                                            // The search can stop on a pole or an edge of the
                                            // surface, where no slope leads away; before a vertex
                                            // is called off, it is looked for from all over.
                                            if off > 1e-3 && d.u.span().abs() < 1e6 && d.v.span().abs() < 1e6 {
                                                for i in 0..24 {
                                                    for j in 0..24 {
                                                        let seed = cad_ir::math::Vec2::new(
                                                            d.u.lo + d.u.span() * (i as f64 + 0.5) / 24.0,
                                                            d.v.lo + d.v.span() * (j as f64 + 0.5) / 24.0,
                                                        );
                                                        if let Some(q) = surface.nearest_from(x, seed) {
                                                            let o = (surface.point_at(q) - x).length();
                                                            if o < off {
                                                                (off, uv) = (o, q);
                                                            }
                                                        }
                                                    }
                                                }
                                            }
                                            if off > worst {
                                                (worst, at) = (off, Some((x, uv)));
                                            }
                                        }
                                    }
                                    println!("[off] face {} rebuilt={} worst {:.4} from its surface over {} vertices", fid.0, p.rebuilt, worst, p.positions.len());
                                    if let Some((x, uv)) = at.filter(|_| worst > 1e-3) {
                                        // The search for the nearest point is itself a solve and
                                        // can stop short; say where, so it can be told from a
                                        // vertex that really is off.
                                        println!("[off]   at [{:.4}, {:.4}, {:.4}], nearest found at u {:.5} v {:.5}", x.x, x.y, x.z, uv.u, uv.v);
                                    }
                                }
                                // Triangles with two corners in one place.
                                let flat = p
                                    .indices
                                    .chunks_exact(3)
                                    .filter(|t| {
                                        let q = |i: u32| p.positions[i as usize];
                                        q(t[0]) == q(t[1]) || q(t[1]) == q(t[2]) || q(t[0]) == q(t[2])
                                    })
                                    .count();
                                (p.indices.len() / 3, p.rebuilt, hit, flat)
                            });
                            mine.push((fid.0, t.elapsed().as_secs_f64() * 1e3, result));
                        }
                        mine
                    })
                })
                .collect();
            workers.into_iter().flat_map(|w| w.join().unwrap()).collect()
        });
        rows.sort_by(|a, b| b.1.total_cmp(&a.1));

        let total: f64 = rows.iter().map(|r| r.1).sum();
        println!("face time summed over threads: {:.0} ms", total);
        for (fid, ms, result) in &rows {
            let face = solid.face(cad_ir::brep::FaceId(*fid));
            let surface = solid.surface(face.surface);
            let kind = match surface {
                cad_ir::Surface::Nurbs(n) => format!(
                    "nurbs {}x{} deg {}x{}{}",
                    n.control_points.len(),
                    n.control_points.first().map_or(0, |r| r.len()),
                    n.u_degree,
                    n.v_degree,
                    if n.u_closed || n.v_closed { " closed" } else { "" }
                ),
                other => cad_tess::surface_kind(other).to_string(),
            };
            let halves: usize = face.bounds.iter().map(|b| b.halves.len()).sum();
            let outcome = match result {
                Ok((tris, rebuilt, hit, flat)) => {
                    format!(
                        "{tris} triangles{}{}{}",
                        if *rebuilt { " (rebuilt)" } else { "" },
                        if *hit { " NEAR" } else { "" },
                        if *flat > 0 { format!(" FLAT {flat}") } else { String::new() }
                    )
                }
                Err(why) => format!("FAILED: {why}"),
            };
            println!(
                "  face {fid:>4}  {ms:>9.1} ms  {kind:<28} loops {} halves {halves:<3} {outcome}",
                face.bounds.len()
            );
        }
    }
    Ok(())
}
