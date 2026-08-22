//! Parasolid → GLB, standalone: no STEP anywhere in the path.
//!
//! `cargo run --release -p cad-xt --example xt_mesh -- file.x_t [draft|fine]`
//!
//! Takes CAD_MATERIALS=<table> and CAD_NO_INFER=1 like the STEP pipeline.

use cad_tess::Options;
use std::time::Instant;

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let mut args = std::env::args().skip(1);
    let Some(path) = args.next() else {
        eprintln!("usage: xt_mesh <file.x_t> [draft|fine]");
        std::process::exit(2);
    };
    let quality = match args.next().as_deref() {
        Some("draft") => Options::draft(),
        Some("fine") => Options::fine(),
        _ => Options::default(),
    };

    let mut opts = cad_xt::LowerOptions::default();
    if let Ok(table_path) = std::env::var("CAD_MATERIALS") {
        let text = std::fs::read_to_string(&table_path)?;
        let (table, errors) = cad_ir::MaterialTable::parse(&text);
        for e in &errors {
            eprintln!("[materials] {table_path}: {e}");
        }
        opts.materials.table = table;
    }
    opts.materials.no_inference = std::env::var_os("CAD_NO_INFER").is_some();

    let t0 = Instant::now();
    let (mut scene, report) = cad_xt::scene_from_file(&path, &opts)?;
    let lower_ms = t0.elapsed().as_secs_f64() * 1e3;

    // Narrow to one body by name, so per-face traces are unambiguous.
    if let Ok(only) = std::env::var("XT_ONLY") {
        let keep: Vec<bool> = scene
            .geometry
            .iter()
            .map(|g| g.name.contains(&only))
            .collect();
        for (g, k) in scene.geometry.iter_mut().zip(&keep) {
            if !k {
                g.brep = None;
            }
        }
    }

    let t1 = Instant::now();
    let tess = cad_tess::tessellate_scene(&mut scene, &quality);
    let tess_ms = t1.elapsed().as_secs_f64() * 1e3;

    println!("{path}");
    println!("  lower {lower_ms:.0} ms   tessellate {tess_ms:.0} ms");
    if let Some(t) = &report.truncated {
        println!("  TRUNCATED: {t}");
    }

    let mut faces = 0usize;
    let mut edges = 0usize;
    for g in &scene.geometry {
        if let Some(s) = &g.brep {
            faces += s.faces.len();
            edges += s.edges.len();
        }
    }
    if std::env::var_os("XT_BODY_BOUNDS").is_some() {
        println!("\n-- per-body vertex bounds (mm) --");
        for g in &scene.geometry {
            if let Some(sd) = &g.brep {
                let b = sd.vertex_bounds();
                if !b.is_empty() {
                    println!(
                        "  {:<22} centre ({:>10.1},{:>10.1},{:>10.1})  diag {:>9.1}",
                        g.name,
                        b.centre().x,
                        b.centre().y,
                        b.centre().z,
                        b.diagonal()
                    );
                }
            }
        }
    }

    if std::env::var_os("XT_STRAY_SCAN").is_some() {
        for g in &scene.geometry {
            let Some(sd) = &g.brep else { continue };
            for (vi, v) in sd.vertices.iter().enumerate() {
                if v.length() > 1.0e5 {
                    // which edges reference it, and what curve they ride
                    for (ei, e) in sd.edges.iter().enumerate() {
                        if e.start.index() == vi || e.end.index() == vi {
                            let kind = match sd.curve(e.curve) {
                                cad_ir::Curve::Line { .. } => "line",
                                cad_ir::Curve::Circle { .. } => "circle",
                                cad_ir::Curve::Polyline { .. } => "polyline",
                                cad_ir::Curve::Nurbs(_) => "nurbs",
                                cad_ir::Curve::Trimmed { .. } => "trimmed",
                                _ => "other",
                            };
                            println!(
                                "[stray-v] {} vertex {vi} at ({:.0},{:.0},{:.0}) edge {ei} {kind} range [{:.3},{:.3}]",
                                g.name, v.x, v.y, v.z, e.range.lo, e.range.hi
                            );
                        }
                    }
                }
            }
        }
    }

    println!("\n-- scene --");
    println!("  bodies      {}", scene.geometry.len());
    println!("  faces       {faces}   edges {edges}");
    println!(
        "  meshed      {}/{} faces ({:.2}%)",
        tess.faces_ok,
        tess.faces_ok + tess.failed.len(),
        tess.success_rate() * 100.0
    );
    println!("  triangles   {}", tess.triangles);
    // A closed solid uses every edge from exactly two faces. One use means a
    // face that should have been on the other side is missing, and that is a
    // reading gap rather than a tessellation one — worth separating, because
    // the two are fixed in different places.
    {
        let (mut once, mut total) = (0usize, 0usize);
        for g in &scene.geometry {
            let Some(solid) = &g.brep else { continue };
            let mut uses: std::collections::HashMap<u32, usize> = Default::default();
            for face in &solid.faces {
                for b in &face.bounds {
                    for h in &b.halves {
                        *uses.entry(h.edge.0).or_default() += 1;
                    }
                }
            }
            total += uses.len();
            once += uses.values().filter(|c| **c < 2).count();
        }
        println!("  edge use    {once} of {total} edges reach only one face");
        let mut bare: std::collections::BTreeMap<&str, usize> = Default::default();
        for g in &scene.geometry {
            let Some(solid) = &g.brep else { continue };
            for f in &solid.faces {
                if f.bounds.iter().all(|b| b.halves.is_empty()) {
                    *bare.entry(cad_tess::surface_kind(solid.surface(f.surface))).or_default() += 1;
                }
            }
        }
        let mut dup = 0usize;
        let mut edges_seen = 0usize;
        for g in &scene.geometry {
            let Some(solid) = &g.brep else { continue };
            let mut by_ends: std::collections::HashMap<[u64; 6], usize> = Default::default();
            for e in &solid.edges {
                let k = |p: cad_ir::math::Vec3| {
                    [
                        (p.x * 1e6).round() as i64 as u64,
                        (p.y * 1e6).round() as i64 as u64,
                        (p.z * 1e6).round() as i64 as u64,
                    ]
                };
                let (a, b) = (k(solid.vertex(e.start)), k(solid.vertex(e.end)));
                let key = if a <= b {
                    [a[0], a[1], a[2], b[0], b[1], b[2]]
                } else {
                    [b[0], b[1], b[2], a[0], a[1], a[2]]
                };
                *by_ends.entry(key).or_default() += 1;
                edges_seen += 1;
            }
            dup += by_ends.values().filter(|c| **c > 1).map(|c| *c - 1).sum::<usize>();
        }
        println!("  duplicate   {dup} of {edges_seen} edges share another edge's two ends");
        // Does each edge's curve, walked over its range, actually start and
        // finish at that edge's own vertices? It is the one claim a parameter
        // range makes that can be checked against something else the file
        // says, and an edge that fails it puts its two faces' boundaries in
        // different places.
        let (mut bad, mut worst) = (0usize, 0.0f64);
        let mut by_kind: std::collections::BTreeMap<&str, (usize, f64)> = Default::default();
        for g in &scene.geometry {
            let Some(solid) = &g.brep else { continue };
            for e in &solid.edges {
                let c = solid.curve(e.curve);
                let (a, b) = (c.point_at(e.range.lo), c.point_at(e.range.hi));
                let (p, q) = (solid.vertex(e.start), solid.vertex(e.end));
                let miss = ((a - p).length().max((b - q).length()))
                    .min((a - q).length().max((b - p).length()));
                if miss > e.tolerance.max(solid.tolerance) * 10.0 {
                    bad += 1;
                    worst = worst.max(miss);
                    *by_kind
                        .entry(cad_tess::curve_kind(c))
                        .or_insert((0usize, 0.0f64)) = {
                        let slot = by_kind.get(cad_tess::curve_kind(c)).copied().unwrap_or((0, 0.0));
                        (slot.0 + 1, slot.1.max(miss))
                    };
                }
            }
        }
        println!("  range       {bad} of {edges_seen} edges miss their own vertices, worst {worst:.4}");
        for (k, (n, w)) in &by_kind {
            println!("              {k} {n} worst {w:.4}");
        }
        let n: usize = bare.values().sum();
        println!("  no bounds   {n} faces trim against nothing: {bare:?}");
        // A loop is a closed walk: each half-edge has to end where the next
        // one starts. Counting how often every edge is used cannot see a loop
        // that lost one — the survivors are still used twice each — so the
        // junctions are checked directly.
        let (mut broken, mut faces_broken, mut widest) = (0usize, 0usize, 0.0f64);
        for g in &scene.geometry {
            let Some(solid) = &g.brep else { continue };
            for f in &solid.faces {
                let mut hurt = false;
                for b in &f.bounds {
                    if b.halves.len() < 2 {
                        continue;
                    }
                    for w in 0..b.halves.len() {
                        let (h, next) = (&b.halves[w], &b.halves[(w + 1) % b.halves.len()]);
                        let end = |h: &cad_ir::brep::HalfEdge| {
                            let e = solid.edge(h.edge);
                            solid.vertex(if h.forward { e.end } else { e.start })
                        };
                        let start = |h: &cad_ir::brep::HalfEdge| {
                            let e = solid.edge(h.edge);
                            solid.vertex(if h.forward { e.start } else { e.end })
                        };
                        let gap = (end(h) - start(next)).length();
                        if gap > solid.tolerance * 10.0 {
                            broken += 1;
                            hurt = true;
                            widest = widest.max(gap);
                        }
                    }
                }
                if hurt {
                    faces_broken += 1;
                }
            }
        }
        println!(
            "  junctions   {broken} loop joins do not meet, across {faces_broken} faces, widest {widest:.4}"
        );
        // A chart is Parasolid's sparse evaluation of an intersection curve,
        // not the curve. Where its samples sit further apart than the sag the
        // mesh is held to, drawing straight between them puts a chord where an
        // arc belongs — and on a face whose boundary that chord cuts across,
        // the region stops being readable at all.
        let (mut coarse, mut widest_step) = (0usize, 0.0f64);
        for g in &scene.geometry {
            let Some(solid) = &g.brep else { continue };
            for e in &solid.edges {
                let mut c = solid.curve(e.curve);
                let charted = loop {
                    match c {
                        cad_ir::brep::Curve::Polyline { .. } => break true,
                        cad_ir::brep::Curve::Trimmed { base, .. } => c = base,
                        _ => break false,
                    }
                };
                if !charted {
                    continue;
                }
                let span = (solid.vertex(e.end) - solid.vertex(e.start)).length();
                if span > 1.0 {
                    coarse += 1;
                    widest_step = widest_step.max(span);
                }
            }
        }
        println!("  charts      {coarse} chart edges span more than a millimetre, widest {widest_step:.2}");
    }
    println!("  materials   {}", scene.materials.len());

    if !report.skipped.is_empty() {
        let mut by_reason: std::collections::BTreeMap<String, usize> = Default::default();
        for s in &report.skipped {
            let key: String = s.reason.split(|c: char| c.is_ascii_digit()).collect();
            *by_reason.entry(key).or_default() += 1;
        }
        println!("\n-- lowering skips ({}) --", report.skipped.len());
        for (r, n) in by_reason.iter().take(10) {
            println!("  {n:>6}  {r}");
        }
        if std::env::var_os("XT_SKIP_SAMPLES").is_some() {
            let mut seen: std::collections::BTreeMap<String, usize> = Default::default();
            for sk in &report.skipped {
                let key: String = sk.reason.split(|c: char| c.is_ascii_digit()).collect();
                let n = seen.entry(key).or_default();
                if *n < 2 {
                    println!("    e.g. #{}: {}", sk.entity, sk.reason);
                }
                *n += 1;
            }
        }
    }
    if !tess.failed.is_empty() {
        println!("\n-- tessellation failures ({}) --", tess.failed.len());
        let mut by_kind: std::collections::BTreeMap<String, usize> = Default::default();
        for f in &tess.failed {
            let kind = scene
                .geometry
                .iter()
                .find(|g| g.name == f.geometry)
                .and_then(|g| g.brep.as_ref())
                .map(|s| surface_kind(s.surface(s.face(f.face).surface)))
                .unwrap_or("?");
            let reason: String = f.reason.split(|c: char| c.is_ascii_digit()).collect();
            *by_kind.entry(format!("{kind}: {reason}")).or_default() += 1;
        }
        if std::env::var_os("XT_FAIL_SAMPLES").is_some() {
            for f in &tess.failed {
                // How much of the model goes missing with it: the extent of
                // the boundary the file gave the face.
                let extent = scene
                    .geometry
                    .iter()
                    .find(|g| g.name == f.geometry)
                    .and_then(|g| g.brep.as_ref())
                    .map(|s| {
                        let mut b = cad_ir::math::Aabb::EMPTY;
                        for bd in &s.face(f.face).bounds {
                            for h in &bd.halves {
                                let e = s.edge(h.edge);
                                b.add_point(s.vertex(e.start));
                                b.add_point(s.vertex(e.end));
                            }
                        }
                        b.diagonal()
                    })
                    .unwrap_or(0.0);
                println!("    #{:?} in {} spans {extent:.3}: {}", f.face, f.geometry, f.reason);
            }
        }
        for (r, n) in by_kind.iter().take(12) {
            println!("  {n:>6}  {r}");
        }
    }

    if !report.diagnostics.is_empty() {
        println!("\n-- topology diagnostics (first 8 bodies) --");
        for (name, c) in report.diagnostics.iter().take(8) {
            println!("  {name:<22} {}", c.join("; "));
        }
    }

    println!("\n-- materials --");
    for (i, m) in scene.materials.iter().enumerate().take(20) {
        println!(
            "  [{i:>2}] {:<22} linear({:.3},{:.3},{:.3})  metal {:.1}  rough {:.2}",
            m.name, m.base_color[0], m.base_color[1], m.base_color[2], m.metallic, m.roughness
        );
    }

    if std::env::var_os("FACECOUNT").is_some() {
        for g in &scene.geometry {
            if let Some(sd) = &g.brep {
                let loops: usize = sd.faces.iter().map(|f| f.bounds.len()).sum();
                let halves: usize = sd
                    .faces
                    .iter()
                    .flat_map(|f| f.bounds.iter())
                    .map(|b| b.halves.len())
                    .sum();
                let closed = sd.edges.iter().filter(|e| e.start == e.end).count();
                let arc_len: f64 = sd
                    .edges
                    .iter()
                    .map(|e| {
                        let c = sd.curve(e.curve);
                        let n = 8;
                        (0..n)
                            .map(|k| {
                                let a = c.point_at(e.range.at(k as f64 / n as f64));
                                let b = c.point_at(e.range.at((k + 1) as f64 / n as f64));
                                (b - a).length()
                            })
                            .sum::<f64>()
                    })
                    .sum();
                println!(
                    "[fc] {:<24} faces {:>5} loops {:>5} halves {:>6} edges {:>6} closed {:>5} arclen {:>12.1}",
                    g.name, sd.faces.len(), loops, halves, sd.edges.len(), closed, arc_len
                );
            }
        }
    }

    let b = scene.bounds();
    if !b.is_empty() {
        println!("\n-- extent (mm) --");
        println!("  {:.1} x {:.1} x {:.1}", b.size().x, b.size().y, b.size().z);
    }

    let out_dir = std::env::var("CAD_OUT").unwrap_or_else(|_| "out".into());
    std::fs::create_dir_all(&out_dir)?;
    let stem = std::path::Path::new(&path)
        .file_stem()
        .unwrap_or_default()
        .to_string_lossy()
        .into_owned();
    let out = std::path::Path::new(&out_dir).join(format!("{stem}.xt.glb"));
    let bytes = cad_export::glb::write_file(&scene, &cad_export::Options::default(), &out)?;
    let lean_path = std::path::Path::new(&out_dir).join(format!("{stem}.xt.lean.glb"));
    let lean = cad_export::glb::write_file(&scene, &cad_export::Options::lean(), &lean_path)?;
    let compact_path = std::path::Path::new(&out_dir).join(format!("{stem}.xt.compact.glb"));
    let compact =
        cad_export::glb::write_file(&scene, &cad_export::Options::compact(), &compact_path)?;
    println!("\n-- glb --");
    println!("  {}  {:.2} MB", out.display(), bytes as f64 / 1e6);
    println!(
        "  {}  {:.2} MB   ({:.0}% of plain, every vertex exactly where it was)",
        lean_path.display(),
        lean as f64 / 1e6,
        lean as f64 / bytes as f64 * 100.0
    );
    println!(
        "  {}  {:.2} MB   ({:.0}% of plain)",
        compact_path.display(),
        compact as f64 / 1e6,
        compact as f64 / bytes as f64 * 100.0
    );

    Ok(())
}

fn surface_kind(s: &cad_ir::Surface) -> &'static str {
    use cad_ir::Surface::*;
    match s {
        Plane { .. } => "plane",
        Cylinder { .. } => "cylinder",
        Cone { .. } => "cone",
        Sphere { .. } => "sphere",
        Torus { .. } => "torus",
        Nurbs(_) => "nurbs",
        LinearExtrusion { .. } => "extrusion",
        Revolution { .. } => "revolution",
        Offset { .. } => "offset",
        RectangularTrimmed { .. } => "trimmed",
    }
}
