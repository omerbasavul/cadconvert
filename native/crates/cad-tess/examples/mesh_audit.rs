//! Find triangles that do not belong: slivers, spikes, and anything reaching
//! far outside the body it was built from.
//!
//! A face can pass every structural check — indices in range, normals unit
//! length, edges shared by two triangles — and still contain a triangle whose
//! third vertex sits half a metre away. That is what this looks for.
//!
//! `cargo run --release -p cad-tess --example mesh_audit -- file.stp`

use cad_ir::brep::FaceId;
use cad_ir::math::Vec3;
use cad_step::{lower, StepFile};
use cad_tess::Options;

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let Some(path) = std::env::args().nth(1) else {
        eprintln!("usage: mesh_audit <file.stp>");
        std::process::exit(2);
    };

    let file = StepFile::open(&path)?;
    let (mut scene, _) = lower::asm::to_scene(&file)?;
    let report = cad_tess::tessellate_scene(&mut scene, &Options::default());
    println!(
        "{} geometries, {} triangles, {}/{} faces meshed",
        scene.geometry.len(),
        scene.stored_triangle_count(),
        report.faces_ok,
        report.faces_ok + report.failed.len()
    );

    // A triangle whose longest edge exceeds this fraction of the body's own
    // extent is not describing a surface — a real facet is a fraction of a
    // feature, and a feature is a fraction of the part.
    const SPIKE_FRACTION: f64 = 0.25;

    let mut rows: Vec<(String, usize, usize, f64, f64)> = Vec::new();
    let mut total_spikes = 0usize;
    let mut total_slivers = 0usize;

    for g in &scene.geometry {
        let (Some(mesh), Some(solid)) = (&g.mesh, &g.brep) else {
            continue;
        };
        let extent = solid.geometric_bounds().diagonal();
        if extent <= 0.0 {
            continue;
        }
        let spike_limit = extent * SPIKE_FRACTION;

        let mut spikes = 0usize;
        let mut slivers = 0usize;
        let mut worst = 0.0f64;
        for tri in mesh.indices.chunks_exact(3) {
            let p: Vec<Vec3> = tri
                .iter()
                .map(|&i| {
                    let v = mesh.positions[i as usize];
                    Vec3::new(v[0] as f64, v[1] as f64, v[2] as f64)
                })
                .collect();
            let e = [
                (p[1] - p[0]).length(),
                (p[2] - p[1]).length(),
                (p[0] - p[2]).length(),
            ];
            let longest = e.iter().copied().fold(0.0f64, f64::max);
            let shortest = e.iter().copied().fold(f64::INFINITY, f64::min);
            if longest > spike_limit {
                spikes += 1;
                worst = worst.max(longest);
            }
            // A needle: long and thin, which is what a wrong third vertex makes
            // and what a well-formed facet never is.
            if shortest > 0.0 && longest / shortest > 200.0 {
                slivers += 1;
            }
        }
        total_spikes += spikes;
        total_slivers += slivers;
        if spikes > 0 || slivers > 0 {
            rows.push((g.name.clone(), spikes, slivers, worst, extent));
        }
    }

    println!("\n-- oversized triangles --");
    println!(
        "  {total_spikes} exceed a quarter of their body's extent, {total_slivers} are needles"
    );
    rows.sort_by(|a, b| (b.3 / b.4).partial_cmp(&(a.3 / a.4)).unwrap());
    println!(
        "\n  {:<24} {:>7} {:>8} {:>11} {:>10} {:>7}",
        "geometry", "spikes", "needles", "longest mm", "extent mm", "ratio"
    );
    for (name, spikes, slivers, worst, extent) in rows.iter().take(14) {
        println!(
            "  {name:<24} {spikes:>7} {slivers:>8} {worst:>11.1} {extent:>10.1} {:>6.1}x",
            worst / extent
        );
    }

    // Which face produced the worst one, so the cause can be looked at rather
    // than guessed at.
    println!("\n-- worst offending faces --");
    let mut offenders: Vec<(String, FaceId, f64, f64, String)> = Vec::new();
    for g in &scene.geometry {
        let (Some(solid), Some(_)) = (&g.brep, &g.mesh) else {
            continue;
        };
        let extent = solid.geometric_bounds().diagonal();
        if extent <= 0.0 {
            continue;
        }
        let edges = cad_tess::edge::discretise_all(solid, &Options::default().resolve(extent));
        for (_, fid) in solid.shell_faces() {
            let Ok(patch) = cad_tess::face::tessellate(solid, fid, &edges, &Options::default().resolve(extent))
            else {
                continue;
            };
            let mut longest = 0.0f64;
            for tri in patch.indices.chunks_exact(3) {
                let p: Vec<Vec3> = tri
                    .iter()
                    .map(|&i| {
                        let v = patch.positions[i as usize];
                        Vec3::new(v[0] as f64, v[1] as f64, v[2] as f64)
                    })
                    .collect();
                longest = longest
                    .max((p[1] - p[0]).length())
                    .max((p[2] - p[1]).length())
                    .max((p[0] - p[2]).length());
            }
            if longest > extent * SPIKE_FRACTION {
                let f = solid.face(fid);
                offenders.push((
                    g.name.clone(),
                    fid,
                    longest,
                    extent,
                    format!(
                        "{} bounds={} halves={:?}",
                        surface_kind(solid.surface(f.surface)),
                        f.bounds.len(),
                        f.bounds.iter().map(|b| b.halves.len()).collect::<Vec<_>>()
                    ),
                ));
            }
        }
    }
    offenders.sort_by(|a, b| (b.2 / b.3).partial_cmp(&(a.2 / a.3)).unwrap());
    for (name, fid, longest, extent, desc) in offenders.iter().take(12) {
        println!(
            "  {name:<22} face {:<5} longest {longest:>9.1} of {extent:>7.1} mm   {desc}",
            fid.0
        );
    }
    println!("  {} faces produce an oversized triangle", offenders.len());

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
