//! Write a small textured scene as USDZ, so `usdchecker` has something to read.
//!
//! `cargo run -p cad-export --example usdz_probe -- out.usdz`

use cad_ir::material::{Material, MaterialClass, Textures};
use cad_ir::mesh::{Mesh, MeshPart};
use cad_ir::scene::{Geometry, Node, Scene};

fn main() {
    let out = std::env::args().nth(1).unwrap_or_else(|| "probe.usdz".into());

    let mut scene = Scene::default();
    scene.meta.source = "probe.x_t".into();

    let colour = scene.add_image(cad_ir::image::Image {
        name: "grain".into(),
        mime: cad_ir::image::Mime::Png,
        width: 4,
        height: 4,
        bytes: cad_ir::image::encode_png(4, 4, &[190; 64]),
    });
    let normal = scene.add_image(cad_ir::image::Image {
        name: "relief".into(),
        mime: cad_ir::image::Mime::Png,
        width: 4,
        height: 4,
        bytes: cad_ir::image::encode_png(4, 4, &[128, 128, 255, 255].repeat(16)),
    });

    let mut textures = Textures::default();
    textures.base_colour = Some(colour);
    textures.normal = Some(normal);
    textures.set_tile_mm(Some([6.35, 6.35]));
    let mut painted = Material::from_class(MaterialClass::Paint, "powder coat");
    painted.textures = textures;
    scene.add_material(painted);
    scene.add_material(Material::from_class(MaterialClass::Steel, "steel"));

    // A box, so there is something with faces pointing several ways, split
    // across two materials so the GeomSubset path is exercised too.
    let mut mesh = Mesh::default();
    let s = 25.0f32;
    let corners = [
        [-s, -s, -s], [s, -s, -s], [s, s, -s], [-s, s, -s],
        [-s, -s, s], [s, -s, s], [s, s, s], [-s, s, s],
    ];
    let faces: [[usize; 4]; 6] = [
        [0, 3, 2, 1], [4, 5, 6, 7], [0, 1, 5, 4],
        [2, 3, 7, 6], [1, 2, 6, 5], [0, 4, 7, 3],
    ];
    let normals = [
        [0.0, 0.0, -1.0], [0.0, 0.0, 1.0], [0.0, -1.0, 0.0],
        [0.0, 1.0, 0.0], [1.0, 0.0, 0.0], [-1.0, 0.0, 0.0],
    ];
    for (f, quad) in faces.iter().enumerate() {
        let base = mesh.positions.len() as u32;
        for &c in quad {
            mesh.positions.push(corners[c]);
            mesh.normals.push(normals[f]);
        }
        mesh.indices.extend_from_slice(&[base, base + 1, base + 2, base, base + 2, base + 3]);
    }
    cad_ir::uv::project(&mut mesh);
    mesh.parts = vec![
        MeshPart { material: 0, start: 0, count: 24 },
        MeshPart { material: 1, start: 24, count: 12 },
    ];

    let g = scene.add_geometry(Geometry {
        name: "910 2001 007".into(),
        brep: None,
        mesh: Some(mesh),
        material: None,
        face_materials: vec![],
    });
    for i in 0..2 {
        let mut node = Node {
            name: format!("body {i}"),
            geometry: Some(g),
            ..Default::default()
        };
        node.transform = cad_ir::math::Transform::from_translation(cad_ir::math::Vec3::new(
            i as f64 * 60.0,
            0.0,
            0.0,
        ));
        let id = scene.add_node(node);
        scene.roots.push(id);
    }

    let bytes = cad_export::usd::write_file(&scene, &cad_export::Options::default(), &out)
        .expect("write the usdz");
    println!("{out}  {bytes} bytes");
}
