//! Lower a STEP file into the neutral scene IR and report what came out.
//!
//! `cargo run --release -p cad-step --example step_scene -- file.stp`

use cad_step::{lower, StepFile};
use std::time::Instant;

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let Some(path) = std::env::args().nth(1) else {
        eprintln!("usage: step_scene <file.stp>");
        std::process::exit(2);
    };

    let bytes = std::fs::metadata(&path).map(|m| m.len()).unwrap_or(0);
    let t0 = Instant::now();
    let file = StepFile::open(&path)?;
    let scan_ms = t0.elapsed().as_secs_f64() * 1e3;

    let t1 = Instant::now();
    let (scene, report) = lower::asm::to_scene(&file)?;
    let lower_ms = t1.elapsed().as_secs_f64() * 1e3;

    println!("{path}");
    println!(
        "  {:.1} MB   scan {scan_ms:.0} ms   lower {lower_ms:.0} ms   total {:.0} ms",
        bytes as f64 / 1e6,
        scan_ms + lower_ms
    );
    println!(
        "  source      {} via {}",
        scene.meta.source, scene.meta.authoring_tool
    );
    println!(
        "  units       {:?}, tolerance {} mm",
        scene.meta.unit, scene.meta.tolerance
    );

    let instances = scene.instances();
    println!("\n-- scene --");
    println!("  nodes       {}", scene.nodes.len());
    println!("  roots       {}", scene.roots.len());
    println!("  geometry    {} distinct", scene.geometry.len());
    println!("  instances   {}", instances.len());
    println!("  materials   {}", scene.materials.len());

    let mut faces = 0usize;
    let mut edges = 0usize;
    let mut vertices = 0usize;
    let mut surf_kinds: std::collections::BTreeMap<&str, usize> = Default::default();
    let mut curve_kinds: std::collections::BTreeMap<&str, usize> = Default::default();
    for g in &scene.geometry {
        let Some(s) = &g.brep else { continue };
        faces += s.faces.len();
        edges += s.edges.len();
        vertices += s.vertices.len();
        for x in &s.surfaces {
            *surf_kinds.entry(surface_name(x)).or_default() += 1;
        }
        for c in &s.curves {
            *curve_kinds.entry(curve_name(c)).or_default() += 1;
        }
    }
    // How many splines are rational, checked against the file's own count of
    // RATIONAL_B_SPLINE_* records: a mismatch means weights were dropped, and a
    // spline evaluated without its weights still interpolates its end points,
    // so nothing downstream would notice until the middle bulges away.
    let mut rational_curves = 0usize;
    let mut rational_surfaces = 0usize;
    let mut nurbs_curves = 0usize;
    let mut nurbs_surfaces = 0usize;
    for g in &scene.geometry {
        let Some(s) = &g.brep else { continue };
        for c in &s.curves {
            if let cad_ir::Curve::Nurbs(n) = c {
                nurbs_curves += 1;
                if !n.weights.is_empty() {
                    rational_curves += 1;
                }
            }
        }
        for x in &s.surfaces {
            if let cad_ir::Surface::Nurbs(n) = x {
                nurbs_surfaces += 1;
                if !n.weights.is_empty() {
                    rational_surfaces += 1;
                }
            }
        }
    }
    println!("\n-- splines --");
    println!("  curves      {nurbs_curves} of which {rational_curves} rational");
    println!("  surfaces    {nurbs_surfaces} of which {rational_surfaces} rational");

    println!("\n-- b-rep --");
    println!("  faces       {faces}");
    println!("  edges       {edges}");
    println!("  vertices    {vertices}");
    println!("  surfaces:");
    for (k, n) in &surf_kinds {
        println!("    {n:>7}  {k}");
    }
    println!("  curves:");
    for (k, n) in &curve_kinds {
        println!("    {n:>7}  {k}");
    }

    let bounds = scene.bounds();
    if !bounds.is_empty() {
        println!("\n-- extent (mm) --");
        println!(
            "  {:.1} x {:.1} x {:.1},  diagonal {:.1}",
            bounds.size().x,
            bounds.size().y,
            bounds.size().z,
            bounds.diagonal()
        );
    }

    println!("\n-- materials --");
    for (i, m) in scene.materials.iter().enumerate() {
        println!(
            "  [{i:>2}] {:<20} linear({:.3},{:.3},{:.3}) alpha {:.2}  {:?}",
            m.name, m.base_color[0], m.base_color[1], m.base_color[2], m.alpha, m.source
        );
    }

    println!("\n-- report --");
    println!("  skipped sub-entities  {}", report.skipped.len());
    for s in report.skipped.iter().take(8) {
        println!("    #{}: {}", s.entity, s.reason);
    }
    if report.skipped.len() > 8 {
        println!("    … {} more", report.skipped.len() - 8);
    }
    println!("  unresolved styles     {}", report.unresolved_styles);
    println!("  products with no geometry  {}", report.empty_products.len());
    for p in report.empty_products.iter().take(5) {
        println!("    {p}");
    }
    println!("  geometries with complaints {}", report.diagnostics.len());
    for (name, c) in report.diagnostics.iter().take(6) {
        println!("    {name}: {}", c.join("; "));
    }

    println!("\n-- assembly tree (first 3 levels) --");
    print_tree(&scene, &scene.roots.clone(), 0, 3);

    Ok(())
}

fn print_tree(scene: &cad_ir::Scene, nodes: &[cad_ir::NodeId], depth: usize, max: usize) {
    if depth >= max {
        return;
    }
    for (i, &n) in nodes.iter().enumerate() {
        if i >= 6 {
            println!("{:indent$}… {} more", "", nodes.len() - 6, indent = depth * 2 + 2);
            break;
        }
        let node = scene.node(n);
        let tris = node
            .geometry
            .map(|g| {
                scene.geometry_of(g)
                    .brep
                    .as_ref()
                    .map(|s| s.faces.len())
                    .unwrap_or(0)
            })
            .unwrap_or(0);
        println!(
            "{:indent$}{} [{} children, {} faces]",
            "",
            node.name,
            node.children.len(),
            tris,
            indent = depth * 2 + 2
        );
        print_tree(scene, &node.children, depth + 1, max);
    }
}

fn surface_name(s: &cad_ir::Surface) -> &'static str {
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

fn curve_name(c: &cad_ir::Curve) -> &'static str {
    use cad_ir::Curve::*;
    match c {
        Line { .. } => "line",
        Circle { .. } => "circle",
        Ellipse { .. } => "ellipse",
        Parabola { .. } => "parabola",
        Hyperbola { .. } => "hyperbola",
        Polyline { .. } => "polyline",
        Nurbs(_) => "nurbs",
        Trimmed { .. } => "trimmed",
        Composite { .. } => "composite",
        OnSurface { .. } => "on-surface",
    }
}
