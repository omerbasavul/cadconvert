//! Read a STEP file, tessellate it, and report the mesh.
//!
//! `cargo run --release -p cad-step --example step_mesh -- file.stp [quality]`
//! where quality is `draft`, `normal` or `fine`.

use cad_step::{lower, StepFile};
use cad_tess::Options;
use std::time::Instant;

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let mut args = std::env::args().skip(1);
    let Some(path) = args.next() else {
        eprintln!("usage: step_mesh <file.stp> [draft|normal|fine]");
        std::process::exit(2);
    };
    let options = match args.next().as_deref() {
        Some("draft") => Options::draft(),
        Some("fine") => Options::fine(),
        _ => Options::default(),
    };

    let t0 = Instant::now();
    let file = StepFile::open(&path)?;
    let read_ms = t0.elapsed().as_secs_f64() * 1e3;

    // Material resolution: a table via CAD_MATERIALS=path, inference off via
    // CAD_NO_INFER=1.
    let mut opts = lower::asm::LowerOptions::default();
    if let Ok(path) = std::env::var("CAD_MATERIALS") {
        let text = std::fs::read_to_string(&path)?;
        let (table, errors) = cad_ir::MaterialTable::parse(&text);
        for e in &errors {
            eprintln!("[materials] {path}: {e}");
        }
        opts.materials.table = table;
    }
    opts.materials.no_inference = std::env::var_os("CAD_NO_INFER").is_some();

    let t1 = Instant::now();
    let (mut scene, _) = lower::asm::to_scene_with(&file, &opts)?;
    let lower_ms = t1.elapsed().as_secs_f64() * 1e3;

    let scale = scene.vertex_bounds().diagonal();
    let t2 = Instant::now();
    let report = cad_tess::tessellate_scene(&mut scene, &options);
    let tess_ms = t2.elapsed().as_secs_f64() * 1e3;

    println!("{path}");
    println!(
        "  read {read_ms:.0} ms   lower {lower_ms:.0} ms   tessellate {tess_ms:.0} ms   \
         total {:.0} ms",
        read_ms + lower_ms + tess_ms
    );
    println!(
        "  quality     sag {:.4} mm (relative {}, model {scale:.1} mm), angle {:.1} deg",
        options.resolve(scene.vertex_bounds().diagonal()).sag,
        options.relative,
        options.angular_deflection.to_degrees()
    );

    let total_faces = report.faces_ok + report.failed.len();
    println!("\n-- tessellation --");
    println!(
        "  faces       {}/{} ok  ({:.2}%)",
        report.faces_ok,
        total_faces,
        report.success_rate() * 100.0
    );
    println!("  triangles   {} stored, {} placed", report.triangles, scene.triangle_count());
    println!("  vertices    {}", report.vertices);
    let bytes: usize = scene
        .geometry
        .iter()
        .filter_map(|g| g.mesh.as_ref())
        .map(|m| m.byte_size())
        .sum();
    println!("  mesh bytes  {:.2} MB raw", bytes as f64 / 1e6);

    if !report.failed.is_empty() {
        println!("\n-- failures --");
        let mut by_reason: std::collections::BTreeMap<String, usize> = Default::default();
        for f in &report.failed {
            // Collapse the varying counts so the shapes of failure are visible.
            let key = f
                .reason
                .split(|c: char| c.is_ascii_digit())
                .collect::<String>();
            *by_reason.entry(key).or_default() += 1;
        }
        for (reason, n) in &by_reason {
            println!("  {n:>6}  {reason}");
        }
        println!("\n  examples:");
        for f in report.failed.iter().take(5) {
            println!("    {} face {}: {}", f.geometry, f.face.0, f.reason);
        }
    }

    // Compare each geometry's mesh against its own B-Rep vertices: a mesh that
    // reaches well past the points it was built from has stray vertices, and
    // that is far easier to see per part than in the whole-scene bounds.
    let mut suspect: Vec<(String, f64, f64)> = scene
        .geometry
        .iter()
        .filter_map(|g| {
            let (Some(m), Some(s)) = (&g.mesh, &g.brep) else {
                return None;
            };
            let rough = s.geometric_bounds().diagonal();
            let mesh = m.bounds().diagonal();
            (rough > 0.0 && mesh > rough * 1.5).then(|| (g.name.clone(), rough, mesh))
        })
        .collect();
    suspect.sort_by(|a, b| (b.2 / b.1).partial_cmp(&(a.2 / a.1)).unwrap());
    if !suspect.is_empty() {
        println!("\n-- meshes larger than their own geometry --");
        for (name, rough, mesh) in suspect.iter().take(8) {
            println!(
                "  {name:<24} brep {rough:>10.1} mm   mesh {mesh:>12.1} mm   x{:.1}",
                mesh / rough
            );
        }
    }

    // Write the result out, which is the only check that matters in the end.
    // Never write next to the user's source files.
    let out_dir = std::env::var("CAD_OUT").unwrap_or_else(|_| "out".into());
    std::fs::create_dir_all(&out_dir)?;
    let stem = std::path::Path::new(&path)
        .file_stem()
        .unwrap_or_default()
        .to_string_lossy()
        .into_owned();
    let out = std::path::Path::new(&out_dir).join(format!("{stem}.glb"));
    let t3 = Instant::now();
    let plain = cad_export::glb::write_file(&scene, &cad_export::Options::default(), &out)?;
    let write_ms = t3.elapsed().as_secs_f64() * 1e3;

    let compact_path = std::path::Path::new(&out_dir).join(format!("{stem}.compact.glb"));
    let compact =
        cad_export::glb::write_file(&scene, &cad_export::Options::compact(), &compact_path)?;

    println!("\n-- glb --");
    println!(
        "  {}  {:.2} MB   written in {write_ms:.0} ms",
        out.display(),
        plain as f64 / 1e6
    );
    println!(
        "  {}  {:.2} MB   ({:.0}% of plain)",
        compact_path.display(),
        compact as f64 / 1e6,
        compact as f64 / plain as f64 * 100.0
    );

    // Which placement reaches furthest, so an outlier can be named rather than
    // just widening the scene's box anonymously.
    let mut placed: Vec<(String, f64, f64)> = scene
        .instances()
        .iter()
        .filter_map(|i| {
            let g = scene.geometry_of(i.geometry);
            let m = g.mesh.as_ref()?;
            let bb = m.bounds().transformed(&i.transform);
            (!bb.is_empty()).then(|| {
                (
                    g.name.clone(),
                    bb.centre().length(),
                    bb.diagonal(),
                )
            })
        })
        .collect();
    placed.sort_by(|a, b| b.1.partial_cmp(&a.1).unwrap());
    println!("\n-- furthest placements (mm from origin) --");
    for (name, dist, size) in placed.iter().take(5) {
        println!("  {name:<24} centre {dist:>9.1}   size {size:>8.1}");
    }

    println!("\n-- materials --");
    for (i, m) in scene.materials.iter().enumerate() {
        println!(
            "  [{i:>2}] {:<22} linear({:.3},{:.3},{:.3})  metal {:.1}  rough {:.2}  alpha {:.2}",
            m.name,
            m.base_color[0],
            m.base_color[1],
            m.base_color[2],
            m.metallic,
            m.roughness,
            m.alpha
        );
    }

    let b = scene.bounds();
    println!("\n-- extent (mm) --");
    println!(
        "  {:.1} x {:.1} x {:.1}",
        b.size().x,
        b.size().y,
        b.size().z
    );

    Ok(())
}
