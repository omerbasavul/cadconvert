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

    let t1 = Instant::now();
    let (mut scene, _) = lower::asm::to_scene(&file)?;
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
            let rough = s.rough_bounds().diagonal();
            let mesh = m.bounds().diagonal();
            (rough > 0.0 && mesh > rough * 1.5).then(|| (g.name.clone(), rough, mesh))
        })
        .collect();
    suspect.sort_by(|a, b| (b.2 / b.1).partial_cmp(&(a.2 / a.1)).unwrap());
    if !suspect.is_empty() {
        println!("\n-- meshes larger than their own vertices --");
        for (name, rough, mesh) in suspect.iter().take(8) {
            println!(
                "  {name:<24} brep {rough:>10.1} mm   mesh {mesh:>12.1} mm   x{:.0}",
                mesh / rough
            );
        }
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
