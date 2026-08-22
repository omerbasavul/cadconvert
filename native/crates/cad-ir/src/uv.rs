//! Texture coordinates for a solid that has none.
//!
//! A B-Rep carries no texture coordinates and there is no reason it should:
//! nothing about a cylinder says where a pattern starts on it. The appearance
//! library does say something, though, and it is the thing that decides how
//! this is done — every textured appearance states `initTextureWidth` and
//! `initTextureHeight` in **metres**. Powder coat is 6.35 mm. A physical size
//! is only meaningful against a projection at world scale, which is what
//! SolidWorks itself applies to an appearance by default, so that is what is
//! reproduced here rather than anything derived from surface parameters.
//!
//! Surface parameters would be the other candidate and they are worse for this.
//! A cylinder's `u` is an angle, a cone's varies with height, and a spline's is
//! whatever the modeller's knot vector happened to be; getting a uniform
//! physical scale out of them means arc-length reparameterising every face, and
//! the 1 046 blend faces this project rebuilds as Coons patches have no
//! original parameterisation left to reparameterise. A projection is defined
//! everywhere, needs nothing from the reader, and is what the source
//! application meant.
//!
//! # What a projection costs
//!
//! Each vertex takes its coordinates from the plane its own normal faces most
//! directly. Where a surface turns past 45° the plane changes, and the
//! triangles spanning that turn have their texture stretched. That is inherent
//! to box mapping and it is why it suits a fine grain and not a picture: at
//! 6.35 mm a powder coat's grain is smaller than the triangles involved. A
//! decal would need a real parameterisation, and this is not that.
//!
//! Coordinates come out in **millimetres of surface**, the same unit as the
//! positions. Turning them into repeats is the material's job — it knows its
//! own tile size — and it keeps one set of coordinates usable by every material
//! on a mesh rather than baking one material's tiling into shared vertices.

use crate::mesh::Mesh;

/// Which plane a normal faces most directly, and the two axes that span it.
///
/// The sign flips are what stop opposite sides of a part carrying mirrored
/// texture. They cost nothing and they are wrong to leave out.
fn plane(normal: [f32; 3]) -> (usize, usize, f32, f32) {
    let [x, y, z] = [normal[0].abs(), normal[1].abs(), normal[2].abs()];
    if x >= y && x >= z {
        // Facing ±X: the Z–Y plane.
        if normal[0] >= 0.0 { (2, 1, -1.0, 1.0) } else { (2, 1, 1.0, 1.0) }
    } else if y >= z {
        // Facing ±Y: the X–Z plane.
        if normal[1] >= 0.0 { (0, 2, 1.0, -1.0) } else { (0, 2, 1.0, 1.0) }
    } else {
        // Facing ±Z: the X–Y plane.
        if normal[2] >= 0.0 { (0, 1, 1.0, 1.0) } else { (0, 1, -1.0, 1.0) }
    }
}

/// Fill `mesh.uvs` by projecting every vertex onto the plane its normal faces.
///
/// Does nothing to a mesh with no normals: without one there is no plane to
/// choose, and a guess here would be a texture applied at random.
pub fn project(mesh: &mut Mesh) {
    if mesh.normals.len() != mesh.positions.len() {
        return;
    }
    mesh.uvs.clear();
    mesh.uvs.reserve(mesh.positions.len());

    for (position, normal) in mesh.positions.iter().zip(&mesh.normals) {
        let (u_axis, v_axis, u_sign, v_sign) = plane(*normal);
        // glTF and USD both put the texture origin at the top left with v
        // running down, so the vertical axis is negated once, here, rather
        // than in each writer.
        mesh.uvs.push([
            position[u_axis] * u_sign,
            -(position[v_axis] * v_sign),
        ]);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn mesh_facing(normal: [f32; 3], positions: &[[f32; 3]]) -> Mesh {
        Mesh {
            positions: positions.to_vec(),
            normals: vec![normal; positions.len()],
            indices: (0..positions.len() as u32).collect(),
            ..Default::default()
        }
    }

    #[test]
    fn a_face_takes_its_coordinates_from_the_plane_it_faces() {
        // Facing +Z, so the X–Y plane: u follows x and v follows y.
        let mut m = mesh_facing([0.0, 0.0, 1.0], &[[0.0; 3], [10.0, 0.0, 5.0], [0.0, 4.0, 5.0]]);
        project(&mut m);
        assert_eq!(m.uvs[0], [0.0, 0.0]);
        assert_eq!(m.uvs[1], [10.0, 0.0]);
        assert_eq!(m.uvs[2], [0.0, -4.0]);

        // Facing +X, so the Z–Y plane: the position's z now drives u, and the
        // face's own x — its distance along the projection axis — drops out.
        let mut m = mesh_facing([1.0, 0.0, 0.0], &[[7.0, 0.0, 0.0], [7.0, 0.0, 3.0]]);
        project(&mut m);
        assert_eq!(m.uvs[0], [0.0, 0.0]);
        assert_eq!(m.uvs[1], [-3.0, 0.0]);
    }

    #[test]
    fn the_scale_is_the_models_own_millimetres() {
        // Two points 6.35 mm apart span exactly one powder-coat tile. Nothing
        // in here knows that number; it stays the material's business.
        let mut m = mesh_facing([0.0, 0.0, 1.0], &[[0.0; 3], [6.35, 0.0, 0.0]]);
        project(&mut m);
        assert!((m.uvs[1][0] - m.uvs[0][0] - 6.35).abs() < 1e-5);
    }

    #[test]
    fn opposite_sides_are_not_mirror_images_of_each_other() {
        // The same slab seen from +Z and from -Z. Without the sign flip both
        // sides run u the same way, and a part looks like its own reflection
        // where the two meet.
        let points = [[0.0; 3], [10.0, 0.0, 0.0]];
        let mut front = mesh_facing([0.0, 0.0, 1.0], &points);
        let mut back = mesh_facing([0.0, 0.0, -1.0], &points);
        project(&mut front);
        project(&mut back);
        assert_eq!(front.uvs[1][0], 10.0);
        assert_eq!(back.uvs[1][0], -10.0);
    }

    #[test]
    fn a_mesh_without_normals_is_left_alone() {
        // No normal means no plane, and a guess would put the texture on at
        // random. Better to emit nothing and let the writer omit the material's
        // textures than to emit something meaningless.
        let mut m = Mesh {
            positions: vec![[0.0; 3]; 3],
            indices: vec![0, 1, 2],
            ..Default::default()
        };
        project(&mut m);
        assert!(m.uvs.is_empty());
    }

    #[test]
    fn every_vertex_gets_exactly_one_coordinate() {
        let mut m = mesh_facing([0.3, 0.9, 0.2], &[[1.0, 2.0, 3.0]; 64]);
        project(&mut m);
        assert_eq!(m.uvs.len(), m.positions.len());
        assert!(m.uvs.iter().all(|uv| uv.iter().all(|c| c.is_finite())));
    }
}
