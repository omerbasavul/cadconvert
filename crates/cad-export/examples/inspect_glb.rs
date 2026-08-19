//! Independently verify a GLB with the `gltf` crate's reader.
//!
//! `cargo run --release -p cad-export --example inspect_glb -- file.glb`
//!
//! Deliberately uses a reader this project did not write: a writer that
//! validates its own output only proves it is self-consistent.

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let Some(path) = std::env::args().nth(1) else {
        eprintln!("usage: inspect_glb <file.glb>");
        std::process::exit(2);
    };

    let bytes = std::fs::read(&path)?;
    println!("{path}  {:.2} MB", bytes.len() as f64 / 1e6);

    let (document, buffers, _images) = gltf::import_slice(&bytes)?;
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
            let reader = p.reader(|b| Some(&buffers[b.index()]));
            let positions: Vec<[f32; 3]> =
                reader.read_positions().map(|i| i.collect()).unwrap_or_default();
            vertices += positions.len();
            if let Some(n) = reader.read_normals() {
                for v in n {
                    let len = (v[0] * v[0] + v[1] * v[1] + v[2] * v[2]).sqrt();
                    if (len - 1.0).abs() > 1e-2 {
                        bad_normals += 1;
                    }
                }
            }
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
            let reader = p.reader(|b| Some(&buffers[b.index()]));
            if let Some(positions) = reader.read_positions() {
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
