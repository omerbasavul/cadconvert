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
        for (r, n) in by_kind.iter().take(12) {
            println!("  {n:>6}  {r}");
        }
    }

    println!("\n-- materials --");
    for (i, m) in scene.materials.iter().enumerate().take(20) {
        println!(
            "  [{i:>2}] {:<22} linear({:.3},{:.3},{:.3})  metal {:.1}  rough {:.2}",
            m.name, m.base_color[0], m.base_color[1], m.base_color[2], m.metallic, m.roughness
        );
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
    println!("\n-- glb --");
    println!("  {}  {:.2} MB", out.display(), bytes as f64 / 1e6);

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
