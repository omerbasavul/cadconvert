//! Where a lowered scene's memory goes.
//!
//! Peak resident size for the pilot is around 550 MB after reading and before
//! meshing, from a 33 MB file — and the parsers account for only 36 MB of that
//! for Parasolid and 58 MB for STEP. The rest is the scene. This walks it and
//! puts a number against each kind of thing in it, so the answer is a
//! measurement rather than a guess about which vector is the large one.
//!
//! Heap bytes only: a `Vec`'s capacity times its element size, plus the
//! structs the scene stores inline. It does not see allocator overhead or the
//! slack a `Vec` holds beyond its length, so the total is a floor, not the
//! resident size.

use cad_ir::brep::{Curve, Curve2, Surface};

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let Some(path) = std::env::args().nth(1) else {
        eprintln!("usage: scene_bytes <file.x_t|file.stp>");
        std::process::exit(2);
    };
    let scene = cad_convert::read(std::path::Path::new(&path), &cad_convert::Options::default())?;

    let mut total = Total::default();
    for geometry in &scene.geometry {
        let Some(solid) = &geometry.brep else { continue };
        total.bodies += 1;
        total.faces += solid.faces.len();
        total.edges += solid.edges.len();
        total.vertices += solid.vertices.len();
        total.curves += solid.curves.len();
        total.surfaces += solid.surfaces.len();

        total.inline += solid.faces.capacity() * size_of::<cad_ir::brep::Face>();
        total.inline += solid.edges.capacity() * size_of::<cad_ir::brep::Edge>();
        total.inline += solid.vertices.capacity() * size_of::<cad_ir::Vec3>();
        total.inline += solid.curves.capacity() * size_of::<Curve>();
        total.inline += solid.surfaces.capacity() * size_of::<Surface>();

        for face in &solid.faces {
            for bound in &face.bounds {
                total.half_edges += bound.halves.len();
                total.bound_bytes += bound.halves.capacity() * size_of::<cad_ir::brep::HalfEdge>();
                for half in &bound.halves {
                    if let Some(pcurve) = &half.pcurve {
                        total.pcurve_bytes += curve2_bytes(pcurve);
                        total.pcurves += 1;
                    }
                }
            }
        }
        for curve in &solid.curves {
            let (bytes, kind) = curve_bytes(curve);
            match kind {
                Kind::Polyline => total.polyline_bytes += bytes,
                Kind::Nurbs => total.curve_nurbs_bytes += bytes,
                Kind::Other => total.curve_other_bytes += bytes,
            }
        }
        for surface in &solid.surfaces {
            total.surface_bytes += surface_bytes(surface);
            if let Surface::Nurbs(n) = surface {
                total.nurbs_surfaces += 1;
                total.control_points += n.control_points.iter().map(|r| r.len()).sum::<usize>();
            }
        }
    }

    println!("{path}");
    println!(
        "  {} bodies, {} faces, {} edges, {} half-edges, {} vertices",
        total.bodies, total.faces, total.edges, total.half_edges, total.vertices
    );
    println!(
        "  {} curves, {} surfaces ({} of them NURBS, {} control points between them)",
        total.curves, total.surfaces, total.nurbs_surfaces, total.control_points
    );
    println!("\n  heap bytes, by what holds them:");
    let rows = [
        ("surfaces (control points, knots, weights)", total.surface_bytes),
        ("pcurves on half-edges", total.pcurve_bytes),
        ("curves: chart polylines", total.polyline_bytes),
        ("curves: NURBS", total.curve_nurbs_bytes),
        ("curves: everything else", total.curve_other_bytes),
        ("half-edge lists on bounds", total.bound_bytes),
        ("the arrays themselves", total.inline),
    ];
    let sum: usize = rows.iter().map(|(_, b)| b).sum();
    let mut sorted = rows;
    sorted.sort_by_key(|(_, b)| std::cmp::Reverse(*b));
    for (what, bytes) in sorted {
        println!(
            "    {:<42} {:>8.1} MB  {:>5.1}%",
            what,
            bytes as f64 / 1e6,
            100.0 * bytes as f64 / sum as f64
        );
    }
    println!("    {:<42} {:>8.1} MB", "counted", sum as f64 / 1e6);
    if total.pcurves > 0 {
        println!(
            "\n  a pcurve costs {:.0} bytes on average, and there are {} of them",
            total.pcurve_bytes as f64 / total.pcurves as f64,
            total.pcurves
        );
    }
    Ok(())
}

#[derive(Default)]
struct Total {
    bodies: usize,
    faces: usize,
    edges: usize,
    half_edges: usize,
    vertices: usize,
    curves: usize,
    surfaces: usize,
    nurbs_surfaces: usize,
    control_points: usize,
    pcurves: usize,
    inline: usize,
    surface_bytes: usize,
    pcurve_bytes: usize,
    polyline_bytes: usize,
    curve_nurbs_bytes: usize,
    curve_other_bytes: usize,
    bound_bytes: usize,
}

enum Kind {
    Polyline,
    Nurbs,
    Other,
}

fn curve_bytes(curve: &Curve) -> (usize, Kind) {
    match curve {
        Curve::Polyline { points } => (points.capacity() * size_of::<cad_ir::Vec3>(), Kind::Polyline),
        Curve::Nurbs(n) => (
            n.control_points.capacity() * size_of::<cad_ir::Vec3>()
                + n.knots.capacity() * 8
                + n.weights.capacity() * 8,
            Kind::Nurbs,
        ),
        Curve::Trimmed { base, .. } => {
            let (bytes, kind) = curve_bytes(base);
            (bytes + size_of::<Curve>(), kind)
        }
        Curve::Composite { segments } => (
            segments.capacity() * size_of::<cad_ir::brep::CompositeSegment>()
                + segments.iter().map(|s| curve_bytes(&s.curve).0).sum::<usize>(),
            Kind::Other,
        ),
        _ => (0, Kind::Other),
    }
}

fn curve2_bytes(curve: &Curve2) -> usize {
    match curve {
        Curve2::Polyline { points } => points.capacity() * size_of::<cad_ir::Vec2>(),
        Curve2::Nurbs(n) => {
            n.control_points.capacity() * size_of::<cad_ir::Vec2>()
                + n.knots.capacity() * 8
                + n.weights.capacity() * 8
        }
        _ => 0,
    }
}

fn surface_bytes(surface: &Surface) -> usize {
    match surface {
        Surface::Nurbs(n) => {
            n.control_points.capacity() * size_of::<Vec<cad_ir::Vec3>>()
                + n.control_points
                    .iter()
                    .map(|r| r.capacity() * size_of::<cad_ir::Vec3>())
                    .sum::<usize>()
                + n.weights.capacity() * size_of::<Vec<f64>>()
                + n.weights.iter().map(|r| r.capacity() * 8).sum::<usize>()
                + n.u_knots.capacity() * 8
                + n.v_knots.capacity() * 8
        }
        Surface::Revolution { profile, .. } | Surface::LinearExtrusion { profile, .. } => {
            curve_bytes(profile).0 + size_of::<Curve>()
        }
        Surface::Offset { base, .. } | Surface::RectangularTrimmed { base, .. } => {
            surface_bytes(base) + size_of::<Surface>()
        }
        _ => 0,
    }
}
