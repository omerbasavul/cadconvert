//! Report one part's B-Rep as we read it: faces, surface kinds, and bounds.
//!
//! `cargo run --release -p cad-step --example part_probe -- file.stp "221 201 001"`
//!
//! When a reference mesher produces six times our triangles for the same part,
//! the question is whether we read fewer faces or meshed them worse. This
//! answers the first half.

use cad_step::{lower, StepFile};
use std::collections::BTreeMap;

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let mut args = std::env::args().skip(1);
    let (Some(path), Some(want)) = (args.next(), args.next()) else {
        eprintln!("usage: part_probe <file.stp> <part-name-substring>");
        std::process::exit(2);
    };

    let file = StepFile::open(&path)?;
    let (scene, _) = lower::asm::to_scene(&file)?;

    for g in scene.geometry.iter().filter(|g| g.name.contains(&want)) {
        println!("part  {}", g.name);
        let Some(brep) = &g.brep else {
            println!("  no B-Rep");
            continue;
        };
        let mut kinds: BTreeMap<&'static str, usize> = BTreeMap::new();
        for (i, _) in brep.faces.iter().enumerate() {
            let f = cad_ir::brep::FaceId(i as u32);
            *kinds.entry(surface_kind(brep, f)).or_default() += 1;
        }
        println!("  faces   {}", brep.faces.len());
        println!("  edges   {}", brep.edges.len());
        for (k, n) in &kinds {
            println!("    {n:5}  {k}");
        }
        if let Ok(want) = std::env::var("CAD_PROBE_EDGE")
            && let Ok(i) = want.parse::<usize>()
            && let Some(e) = brep.edges.get(i)
        {
            let c = brep.curve(e.curve);
            let (p0, p1) = (brep.vertex(e.start), brep.vertex(e.end));
            println!("  edge {i}: range {:?} verts [{:.4},{:.4},{:.4}]..[{:.4},{:.4},{:.4}]", e.range, p0.x,p0.y,p0.z, p1.x,p1.y,p1.z);
            match c {
                cad_ir::brep::Curve::Nurbs(n) => println!("    nurbs degree {} control {} knots {:?} weights {}", n.degree, n.control_points.len(), n.knots, n.weights.len()),
                cad_ir::brep::Curve::Trimmed { range, base } => println!("    trimmed {:?} of {:?}", range, std::mem::discriminant(base.as_ref())),
                other => println!("    {:?}", std::mem::discriminant(other)),
            }
            for t in 0..=4 {
                let u = e.range.at(t as f64 / 4.0);
                let q = c.point_at(u);
                println!("    t={:.2} u={:.6} -> [{:.4},{:.4},{:.4}]", t as f64 / 4.0, u, q.x, q.y, q.z);
            }
        }
        let b = brep.geometric_bounds();
        println!("  bounds  {:?} .. {:?}", b.min, b.max);
        // A body's first shell is its outside; the rest are cavities. A
        // cavity that never arrives leaves the body closed, every face
        // meshed, and the part solid where the model is hollow — which no
        // structural check can see and only a volume can.
        println!(
            "  shells  {}  ({} faces outside{})",
            brep.shells.len(),
            brep.shells.first().map_or(0, |s| s.faces.len()),
            brep.shells
                .iter()
                .skip(1)
                .map(|s| format!(", {} in a cavity", s.faces.len()))
                .collect::<String>()
        );

        // What the tessellator actually did with each face, and the knot span
        // it had to work with — a helical sweep is one face carrying all the
        // curvature, so a per-face count is what shows under-sampling.
        for (i, _) in brep.faces.iter().enumerate() {
            let f = cad_ir::brep::FaceId(i as u32);
            let s = brep.surface(brep.face(f).surface);
            let kind = surface_kind(brep, f);
            if let cad_ir::brep::Surface::Nurbs(n) = s {
                println!(
                    "    face {i}: {kind}  degree {}x{}  control {}x{}  \
                     u_knots {:.4}..{:.4} ({} spans)  v_knots {:.4}..{:.4} ({} spans)  \
                     closed u={} v={}",
                    n.u_degree,
                    n.v_degree,
                    n.control_points.len(),
                    n.control_points.first().map_or(0, |r| r.len()),
                    n.u_knots.first().copied().unwrap_or(0.0),
                    n.u_knots.last().copied().unwrap_or(0.0),
                    n.u_knots.len().saturating_sub(n.u_degree * 2 + 1) + 1,
                    n.v_knots.first().copied().unwrap_or(0.0),
                    n.v_knots.last().copied().unwrap_or(0.0),
                    n.v_knots.len().saturating_sub(n.v_degree * 2 + 1) + 1,
                    n.u_closed,
                    n.v_closed,
                );
                // The weights of the first column across v: a rational arc
                // carries cos(θ/2) on its middle control point.
                if !n.weights.is_empty() {
                    let col: Vec<String> = n.weights[0].iter().map(|w| format!("{w:.4}")).collect();
                    println!("      weights across v (first column): {}", col.join(" "));
                }
            } else {
                // The analytic surfaces carry their whole shape in a handful of
                // numbers, and those numbers are what a disagreement with
                // another reader is settled on. Printing them turns "our cone
                // differs from theirs" into a value that can be read straight
                // out of the source file.
                use cad_ir::brep::Surface::*;
                let axis = |fr: &cad_ir::Frame| {
                    format!(
                        "at [{:.4}, {:.4}, {:.4}] along [{:.4}, {:.4}, {:.4}]",
                        fr.origin.x, fr.origin.y, fr.origin.z, fr.axis.x, fr.axis.y, fr.axis.z
                    )
                };
                let detail = match s {
                    Plane { frame } => axis(frame),
                    Cylinder { frame, radius } => format!("r {radius:.4}  {}", axis(frame)),
                    Cone { frame, radius, half_angle } => format!(
                        "r {radius:.4}  half angle {:.4} deg  {}",
                        half_angle.to_degrees(),
                        axis(frame)
                    ),
                    Sphere { frame, radius } => format!("r {radius:.4}  {}", axis(frame)),
                    Torus { frame, major_radius, minor_radius } => format!(
                        "major {major_radius:.4} minor {minor_radius:.4}  {}",
                        axis(frame)
                    ),
                    _ => String::new(),
                };
                println!("    face {i}: {kind}  {detail}");
            }
        }
    }
    Ok(())
}

fn surface_kind(brep: &cad_ir::brep::Solid, f: cad_ir::brep::FaceId) -> &'static str {
    use cad_ir::brep::Surface::*;
    match brep.surface(brep.face(f).surface) {
        Plane { .. } => "plane",
        Cylinder { .. } => "cylinder",
        Cone { .. } => "cone",
        Sphere { .. } => "sphere",
        Torus { .. } => "torus",
        Nurbs(_) => "nurbs",
        LinearExtrusion { .. } => "linear extrusion",
        Revolution { .. } => "revolution",
        Offset { .. } => "offset",
        RectangularTrimmed { .. } => "rectangular trimmed",
    }
}
