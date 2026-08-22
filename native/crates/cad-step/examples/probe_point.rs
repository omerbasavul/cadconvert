//! Ask what a body has at one place in the assembly.
//!
//! A distance report says a reference mesher has surface somewhere we do not.
//! That leaves two very different faults: the geometry is not in our reading at
//! all, or it is there and the face was trimmed away or drawn somewhere else.
//! This separates them, by asking each body both questions at the same point —
//! how near its *untrimmed* surfaces come, and how near its triangles do.
//!
//! `cargo run --release -p cad-step --example probe_point -- file.stp X,Y,Z [part]`
//! where X,Y,Z are world millimetres, as `mesh_diff` prints them.

use cad_ir::Vec3;
use cad_step::{StepFile, lower};
use cad_tess::{Options, surface_kind};

/// Invert an affine 3x4 by inverting its linear part and moving the origin.
fn invert(m: &[[f64; 4]; 3]) -> Option<[[f64; 4]; 3]> {
    let a = [
        [m[0][0], m[0][1], m[0][2]],
        [m[1][0], m[1][1], m[1][2]],
        [m[2][0], m[2][1], m[2][2]],
    ];
    let det = a[0][0] * (a[1][1] * a[2][2] - a[1][2] * a[2][1])
        - a[0][1] * (a[1][0] * a[2][2] - a[1][2] * a[2][0])
        + a[0][2] * (a[1][0] * a[2][1] - a[1][1] * a[2][0]);
    if det.abs() < 1e-300 {
        return None;
    }
    let c = |r: usize, k: usize| {
        let (r1, r2) = ((r + 1) % 3, (r + 2) % 3);
        let (k1, k2) = ((k + 1) % 3, (k + 2) % 3);
        (a[r1][k1] * a[r2][k2] - a[r1][k2] * a[r2][k1]) / det
    };
    // Transposed cofactors give the inverse of the linear part.
    let inv = [[c(0, 0), c(1, 0), c(2, 0)], [c(0, 1), c(1, 1), c(2, 1)], [
        c(0, 2),
        c(1, 2),
        c(2, 2),
    ]];
    let t = [m[0][3], m[1][3], m[2][3]];
    let mut out = [[0.0; 4]; 3];
    for r in 0..3 {
        for k in 0..3 {
            out[r][k] = inv[r][k];
        }
        out[r][3] = -(inv[r][0] * t[0] + inv[r][1] * t[1] + inv[r][2] * t[2]);
    }
    Some(out)
}

fn apply(m: &[[f64; 4]; 3], p: Vec3) -> Vec3 {
    Vec3::new(
        m[0][0] * p.x + m[0][1] * p.y + m[0][2] * p.z + m[0][3],
        m[1][0] * p.x + m[1][1] * p.y + m[1][2] * p.z + m[1][3],
        m[2][0] * p.x + m[2][1] * p.y + m[2][2] * p.z + m[2][3],
    )
}

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let mut args = std::env::args().skip(1);
    let (Some(path), Some(at)) = (args.next(), args.next()) else {
        eprintln!("usage: probe_point <file.stp> <X,Y,Z in world mm> [part-substring]");
        std::process::exit(2);
    };
    let want = args.next();
    let c: Vec<f64> = at.split(',').filter_map(|v| v.trim().parse().ok()).collect();
    if c.len() != 3 {
        eprintln!("the point must be three numbers separated by commas");
        std::process::exit(2);
    }
    let world = Vec3::new(c[0], c[1], c[2]);

    // Either reader, so the same question can be put to both: a place one of
    // them covers and the other does not is exactly what this is for.
    let mut scene = if path.to_lowercase().ends_with(".x_t") {
        cad_xt::scene_from_file(&path, &cad_xt::LowerOptions::default())
            .map_err(|e| e.to_string())?
            .0
    } else {
        let file = StepFile::open(&path)?;
        lower::asm::to_scene(&file)?.0
    };
    cad_tess::tessellate_scene(&mut scene, &Options::default());

    println!("at [{:.4}, {:.4}, {:.4}] in world millimetres:", world.x, world.y, world.z);
    let mut any = false;
    for inst in scene.instances() {
        let g = &scene.geometry[inst.geometry.0 as usize];
        if let Some(w) = &want
            && !g.name.contains(w.as_str())
        {
            continue;
        }
        let Some(solid) = &g.brep else { continue };
        let Some(back) = invert(&inst.transform.m) else { continue };
        let local = apply(&back, world);

        let b = solid.geometric_bounds();
        let outside = (0..3).any(|k| {
            let (p, lo, hi) = match k {
                0 => (local.x, b.min.x, b.max.x),
                1 => (local.y, b.min.y, b.max.y),
                _ => (local.z, b.min.z, b.max.z),
            };
            p < lo - 10.0 || p > hi + 10.0
        });
        if outside {
            continue;
        }
        any = true;

        // How near the body's surfaces come, ignoring every trim.
        let mut near: Vec<(f64, usize, &'static str)> = Vec::new();
        for (i, s) in solid.surfaces.iter().enumerate() {
            if let Some(uv) = s.invert(local, None) {
                near.push(((s.point_at(uv) - local).length(), i, surface_kind(s)));
            }
        }
        near.sort_by(|a, b| a.0.total_cmp(&b.0));

        // And how near the triangles we actually drew come.
        let mut drawn = f64::INFINITY;
        if let Some(mesh) = &g.mesh {
            for q in &mesh.positions {
                let p = Vec3::new(q[0] as f64, q[1] as f64, q[2] as f64);
                drawn = drawn.min((p - local).length());
            }
        }

        println!(
            "  {:<26} local [{:.4}, {:.4}, {:.4}]   nearest drawn vertex {:.4} mm",
            g.name, local.x, local.y, local.z, drawn
        );
        for (d, i, kind) in near.iter().take(4) {
            // Which faces stand on that surface, and which way they face. A
            // surface reached by no face is geometry the file never used; a
            // surface with a face on it that the mesh does not reach is a face
            // drawn somewhere it does not belong.
            let on: Vec<String> = solid
                .faces
                .iter()
                .enumerate()
                .filter(|(_, f)| f.surface.0 as usize == *i)
                .map(|(fi, f)| {
                    format!("{fi}{}", if f.same_sense { "" } else { " (reversed)" })
                })
                .collect();
            println!(
                "      surface {i:>5} {kind:<10} {d:.4} mm away, untrimmed   faces on it: {}",
                if on.is_empty() { "none".into() } else { on.join(", ") }
            );
        }
    }
    if !any {
        println!("  no body's bounds reach this point");
    }
    Ok(())
}
