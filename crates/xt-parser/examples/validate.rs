//! Validation: parse .x_t files and print topology stats for comparison.
//! Usage: validate <directory>

use std::env;
use std::path::Path;

fn main() {
    let dir = env::args().nth(1).unwrap_or_else(|| {
        eprintln!("Usage: validate <directory-of-xt-files>");
        std::process::exit(1);
    });

    let mut files: Vec<_> = std::fs::read_dir(&dir)
        .unwrap()
        .filter_map(|e| e.ok())
        .filter(|e| e.path().extension().map_or(false, |ext| ext == "x_t"))
        .map(|e| e.path())
        .collect();
    files.sort();

    let mut total = 0;
    let mut ok = 0;
    let mut err = 0;
    let mut stats = Stats::default();

    for path in files.iter().take(100) {
        total += 1;
        let fname = path.file_name().unwrap().to_string_lossy();

        match std::panic::catch_unwind(|| xt_parser::parse_xt_file(path)) {
            Ok(Ok(file)) => {
                ok += 1;
                for body in &file.bodies {
                    stats.bodies += 1;
                    for shell in &body.shells {
                        stats.shells += 1;
                        for face in &shell.faces {
                            stats.faces += 1;
                            for lp in &face.loops {
                                stats.loops += 1;
                                stats.fins += lp.fins.len();
                            }
                        }
                    }
                    stats.surfaces += body.surfaces.len();
                    stats.curves += body.curves.len();
                    stats.edges += body.edges.len();
                    stats.vertices += body.vertices.len();
                    stats.points += body.points.len();
                }
            }
            Ok(Err(e)) => {
                err += 1;
            }
            Err(_) => {
                err += 1;
            }
        }
    }

    println!("=== xt-winnow validation: {} files ===", total);
    println!("Parse OK:   {} / {} ({:.0}%)", ok, total, ok as f64 / total as f64 * 100.0);
    println!("Parse ERR:  {}", err);
    println!();
    println!("Bodies:     {}", stats.bodies);
    println!("Shells:     {}", stats.shells);
    println!("Faces:      {}", stats.faces);
    println!("Loops:      {}", stats.loops);
    println!("Fins:       {}", stats.fins);
    println!("Surfaces:   {}", stats.surfaces);
    println!("Curves:     {}", stats.curves);
    println!("Edges:      {}", stats.edges);
    println!("Vertices:   {}", stats.vertices);
    println!("Points:     {}", stats.points);
    println!();
    if stats.bodies > 0 {
        println!("Avg faces/body:    {:.1}", stats.faces as f64 / stats.bodies as f64);
        println!("Avg edges/body:    {:.1}", stats.edges as f64 / stats.bodies as f64);
        println!("Avg vertices/body: {:.1}", stats.vertices as f64 / stats.bodies as f64);
    }
    // Euler characteristic check: V - E + F = 2 for closed manifold solids
    let euler = stats.vertices as i64 - stats.edges as i64 + stats.faces as i64;
    println!("V - E + F = {} (expect ~2 × bodies = {} for closed solids)", euler, 2 * stats.bodies);
}

#[derive(Default)]
struct Stats {
    bodies: usize,
    shells: usize,
    faces: usize,
    loops: usize,
    fins: usize,
    surfaces: usize,
    curves: usize,
    edges: usize,
    vertices: usize,
    points: usize,
}
