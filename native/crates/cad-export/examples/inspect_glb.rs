//! Independently verify a GLB with the `gltf` crate's reader.
//!
//! `cargo run --release -p cad-export --example inspect_glb -- file.glb`
//!
//! Deliberately uses a reader this project did not write: a writer that
//! validates its own output only proves it is self-consistent.

// Shared with the other tools here; each uses the part of it it needs.
#[allow(dead_code)]
#[path = "common/glb_read.rs"]
mod glb_read;

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let Some(path) = std::env::args().nth(1) else {
        eprintln!("usage: inspect_glb <file.glb>");
        std::process::exit(2);
    };

    let bytes = std::fs::read(&path)?;
    println!("{path}  {:.2} MB", bytes.len() as f64 / 1e6);

    let (document, buffers) = glb_read::open(&bytes)?;
    println!("  scenes      {}", document.scenes().count());
    println!("  nodes       {}", document.nodes().count());
    println!("  meshes      {}", document.meshes().count());
    println!("  materials   {}", document.materials().count());
    println!("  accessors   {}", document.accessors().count());

    let mut triangles = 0usize;
    let mut vertices = 0usize;
    let mut primitives = 0usize;
    let mut degenerate = 0usize;
    let mut bad_normals = 0usize;

    for mesh in document.meshes() {
        for p in mesh.primitives() {
            primitives += 1;
            let positions: Vec<[f32; 3]> =
                glb_read::positions(&p, &buffers);
            vertices += positions.len();
            {
                let n = glb_read::normals(&p, &buffers);
                for v in n {
                    let len = (v[0] * v[0] + v[1] * v[1] + v[2] * v[2]).sqrt();
                    if (len - 1.0).abs() > 1e-2 {
                        bad_normals += 1;
                    }
                }
            }
            let reader = p.reader(|b| Some(&buffers[b.index()]));
            if let Some(idx) = reader.read_indices() {
                let idx: Vec<u32> = idx.into_u32().collect();
                triangles += idx.len() / 3;
                for t in idx.chunks_exact(3) {
                    if t[0] == t[1] || t[1] == t[2] || t[0] == t[2] {
                        degenerate += 1;
                    }
                }
            }
        }
    }

    println!("  primitives  {primitives}");
    println!("  vertices    {vertices}");
    println!("  triangles   {triangles}");
    println!("  degenerate  {degenerate}");
    println!("  bad normals {bad_normals}");

    // Placed extent, following the node hierarchy the way a viewer does. This
    // is what catches a wrong root transform, which no per-mesh check can see.
    let identity = [
        [1.0, 0.0, 0.0, 0.0],
        [0.0, 1.0, 0.0, 0.0],
        [0.0, 0.0, 1.0, 0.0],
        [0.0, 0.0, 0.0, 1.0],
    ];
    let mut min = [f32::INFINITY; 3];
    let mut max = [f32::NEG_INFINITY; 3];
    for scene in document.scenes() {
        for node in scene.nodes() {
            walk(&node, identity, &buffers, &mut min, &mut max);
        }
    }
    if min[0].is_finite() {
        println!(
            "  world bbox  {:.4} x {:.4} x {:.4} m   centred at ({:.3}, {:.3}, {:.3})",
            max[0] - min[0],
            max[1] - min[1],
            max[2] - min[2],
            (min[0] + max[0]) / 2.0,
            (min[1] + max[1]) / 2.0,
            (min[2] + max[2]) / 2.0,
        );
    }

    // What the file has at one place, in world millimetres. Two instruments
    // disagreeing about a distance is a question only the file can settle, and
    // this asks it without a transform of anyone's guessing: the same walk the
    // bounding box uses, and the answer in the same frame the box is printed
    // in.
    if let Ok(at) = std::env::var("INSPECT_AT") {
        let c: Vec<f64> = at.split(',').filter_map(|v| v.trim().parse().ok()).collect();
        if c.len() == 3 {
            let q = [c[0] as f32 / 1000.0, c[1] as f32 / 1000.0, c[2] as f32 / 1000.0];
            let mut best_vertex = f64::INFINITY;
            let mut best_tri = f64::INFINITY;
            for scene in document.scenes() {
                for node in scene.nodes() {
                    near(&node, identity, &buffers, q, &mut best_vertex, &mut best_tri);
                }
            }
            println!(
                "\n  at [{:.3}, {:.3}, {:.3}] mm: nearest vertex {:.4} mm, nearest triangle {:.4} mm",
                c[0], c[1], c[2],
                best_vertex * 1000.0,
                best_tri * 1000.0
            );
        }
    }

    println!("\n  materials:");
    for m in document.materials().take(24) {
        let pbr = m.pbr_metallic_roughness();
        let c = pbr.base_color_factor();
        println!(
            "    {:<22} rgba({:.3},{:.3},{:.3},{:.2})  metal {:.2}  rough {:.2}  {:?}",
            m.name().unwrap_or("(unnamed)"),
            c[0],
            c[1],
            c[2],
            c[3],
            pbr.metallic_factor(),
            pbr.roughness_factor(),
            m.alpha_mode()
        );
    }

    Ok(())
}

/// Column-major 4x4 multiply, matching glTF's matrix layout.
fn mul(a: [[f32; 4]; 4], b: [[f32; 4]; 4]) -> [[f32; 4]; 4] {
    let mut m = [[0.0f32; 4]; 4];
    for (c, col) in m.iter_mut().enumerate() {
        for (r, cell) in col.iter_mut().enumerate() {
            for k in 0..4 {
                *cell += a[k][r] * b[c][k];
            }
        }
    }
    m
}

fn walk(
    node: &gltf::Node,
    parent: [[f32; 4]; 4],
    buffers: &[gltf::buffer::Data],
    min: &mut [f32; 3],
    max: &mut [f32; 3],
) {
    let world = mul(parent, node.transform().matrix());
    if let Some(mesh) = node.mesh() {
        for p in mesh.primitives() {
            {
                let positions = glb_read::positions(&p, &buffers);
                for v in positions {
                    for (i, slot) in min.iter_mut().enumerate() {
                        let x = world[0][i] * v[0]
                            + world[1][i] * v[1]
                            + world[2][i] * v[2]
                            + world[3][i];
                        *slot = slot.min(x);
                        max[i] = max[i].max(x);
                    }
                }
            }
        }
    }
    for child in node.children() {
        walk(&child, world, buffers, min, max);
    }
}

/// Nearest vertex and nearest triangle to a world point, walking the tree the
/// same way the bounding box does.
fn near(
    node: &gltf::Node,
    parent: [[f32; 4]; 4],
    buffers: &[gltf::buffer::Data],
    q: [f32; 3],
    best_vertex: &mut f64,
    best_tri: &mut f64,
) {
    let world = mul(parent, node.transform().matrix());
    let place = |v: [f32; 3]| -> [f64; 3] {
        let mut o = [0.0f64; 3];
        for (i, slot) in o.iter_mut().enumerate() {
            *slot = (world[0][i] * v[0] + world[1][i] * v[1] + world[2][i] * v[2] + world[3][i])
                as f64;
        }
        o
    };
    if let Some(mesh) = node.mesh() {
        for p in mesh.primitives() {
            let positions = glb_read::positions(&p, &buffers);
            let pts: Vec<[f64; 3]> = positions.into_iter().map(place).collect();
            let target = [q[0] as f64, q[1] as f64, q[2] as f64];
            for v in &pts {
                let d = ((v[0] - target[0]).powi(2)
                    + (v[1] - target[1]).powi(2)
                    + (v[2] - target[2]).powi(2))
                .sqrt();
                *best_vertex = best_vertex.min(d);
            }
            let reader = p.reader(|b| Some(&buffers[b.index()]));
            if let Some(idx) = reader.read_indices() {
                let idx: Vec<u32> = idx.into_u32().collect();
                for t in idx.chunks_exact(3) {
                    let tri = [pts[t[0] as usize], pts[t[1] as usize], pts[t[2] as usize]];
                    *best_tri = best_tri.min(point_to_triangle(target, tri));
                }
            }
        }
    }
    for child in node.children() {
        near(&child, world, buffers, q, best_vertex, best_tri);
    }
}

/// Distance from a point to a triangle, by projecting and clamping.
fn point_to_triangle(p: [f64; 3], t: [[f64; 3]; 3]) -> f64 {
    let sub = |a: [f64; 3], b: [f64; 3]| [a[0] - b[0], a[1] - b[1], a[2] - b[2]];
    let dot = |a: [f64; 3], b: [f64; 3]| a[0] * b[0] + a[1] * b[1] + a[2] * b[2];
    let (e0, e1, d) = (sub(t[1], t[0]), sub(t[2], t[0]), sub(t[0], p));
    let (a, b, c) = (dot(e0, e0), dot(e0, e1), dot(e1, e1));
    let (dd, e) = (dot(e0, d), dot(e1, d));
    let det = (a * c - b * b).max(1e-300);
    let mut s = (b * e - c * dd) / det;
    let mut u = (b * dd - a * e) / det;
    if s + u > 1.0 {
        let over = s + u - 1.0;
        s -= over * 0.5;
        u -= over * 0.5;
    }
    s = s.clamp(0.0, 1.0);
    u = u.clamp(0.0, 1.0 - s);
    let q = [
        t[0][0] + e0[0] * s + e1[0] * u,
        t[0][1] + e0[1] * s + e1[1] * u,
        t[0][2] + e0[2] * s + e1[2] * u,
    ];
    ((q[0] - p[0]).powi(2) + (q[1] - p[1]).powi(2) + (q[2] - p[2]).powi(2)).sqrt()
}
