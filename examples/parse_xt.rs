use std::env;
use xt_parser::parse_xt_file;

fn main() {
    let path = env::args().nth(1).expect("usage: parse_xt <file.x_t>");
    match parse_xt_file(&path) {
        Ok(file) => {
            println!("Version: {}", file.header.version);
            println!("Application: {}", file.header.application);
            println!("Schema: {}", file.header.schema_key);
            println!("Bodies: {}", file.bodies.len());
            for (i, body) in file.bodies.iter().enumerate() {
                println!(
                    "  body[{}]: type={:?}, shells={}, surfaces={}, curves={}, points={}, edges={}, vertices={}",
                    i,
                    body.body_type,
                    body.shells.len(),
                    body.surfaces.len(),
                    body.curves.len(),
                    body.points.len(),
                    body.edges.len(),
                    body.vertices.len(),
                );
                for (j, shell) in body.shells.iter().enumerate() {
                    println!("    shell[{}]: {} faces", j, shell.faces.len());
                    for (k, face) in shell.faces.iter().enumerate() {
                        let surf_type = body
                            .surfaces
                            .get(&face.surface_key)
                            .map(|s| match s {
                                xt_parser::XtSurface::Plane(_) => "plane",
                                xt_parser::XtSurface::Cylinder(_) => "cylinder",
                                xt_parser::XtSurface::Cone(_) => "cone",
                                xt_parser::XtSurface::Sphere(_) => "sphere",
                                xt_parser::XtSurface::Torus(_) => "torus",
                                xt_parser::XtSurface::BSpline(_) => "bspline",
                                xt_parser::XtSurface::Offset(_) => "offset",
                                xt_parser::XtSurface::Swept(_) => "swept",
                                xt_parser::XtSurface::Spun(_) => "spun",
                                xt_parser::XtSurface::Blend(_) => "blend",
                            })
                            .unwrap_or("???");
                        println!(
                            "      face[{}]: surf={}({}), sense={:?}, loops={}",
                            k,
                            surf_type,
                            face.surface_key,
                            face.sense,
                            face.loops.len()
                        );
                    }
                }
            }
        }
        Err(e) => {
            eprintln!("Error: {}", e);
            std::process::exit(1);
        }
    }
}
