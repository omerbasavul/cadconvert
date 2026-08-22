//! Triangle meshes: what the tessellator produces and every writer consumes.
//!
//! Positions are `f32` because that is what glTF, USD crate binary and every
//! GPU want, and carrying `f64` all the way to the writer only to narrow it
//! there doubles the memory of the largest structure in the pipeline. The
//! tessellator works in `f64` and narrows once, here.
//!
//! A mesh is split into [`MeshPart`]s by material rather than into one mesh per
//! material. Keeping a single shared vertex buffer is what lets an assembly of
//! fifty parts and fourteen colours become fourteen draw calls over one buffer
//! instead of fifty buffers, which is most of the file-size story for glTF.

use crate::math::Aabb;

/// A triangle mesh with per-material index ranges.
#[derive(Debug, Clone, Default)]
pub struct Mesh {
    pub positions: Vec<[f32; 3]>,
    /// Empty, or exactly as long as `positions`.
    pub normals: Vec<[f32; 3]>,
    /// Empty, or exactly as long as `positions`.
    pub uvs: Vec<[f32; 2]>,
    /// Triangle corners, three per triangle, counter-clockwise when seen from
    /// outside.
    pub indices: Vec<u32>,
    /// Contiguous index ranges, one per material used.
    pub parts: Vec<MeshPart>,
}

/// A run of triangles sharing one material.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct MeshPart {
    /// Index into [`crate::Scene::materials`].
    pub material: u32,
    /// First entry in [`Mesh::indices`].
    pub start: u32,
    /// Number of entries, always a multiple of three.
    pub count: u32,
}

impl Mesh {
    pub fn vertex_count(&self) -> usize {
        self.positions.len()
    }

    pub fn triangle_count(&self) -> usize {
        self.indices.len() / 3
    }

    pub fn is_empty(&self) -> bool {
        self.indices.is_empty()
    }

    pub fn bounds(&self) -> Aabb {
        let mut b = Aabb::EMPTY;
        for p in &self.positions {
            b.add_point(crate::math::Vec3::new(
                p[0] as f64,
                p[1] as f64,
                p[2] as f64,
            ));
        }
        b
    }

    /// Whether `u16` indices suffice.
    ///
    /// glTF permits both, and halving the index buffer is free size for the
    /// many small parts an assembly is made of.
    pub fn fits_u16_indices(&self) -> bool {
        self.positions.len() <= u16::MAX as usize
    }

    /// Append `other`, offsetting its indices and material parts.
    ///
    /// Used to merge the meshes of parts that share a material, which turns a
    /// draw call per part into a draw call per material.
    pub fn append(&mut self, other: &Mesh) {
        let base = self.positions.len() as u32;
        let index_base = self.indices.len() as u32;

        // The vertex attribute sets must agree, or a merged mesh would have
        // normals for some vertices and not others — which no writer can
        // express. Fill the gap rather than corrupting the buffer.
        let want_normals = !self.normals.is_empty() || !other.normals.is_empty();
        let want_uvs = !self.uvs.is_empty() || !other.uvs.is_empty();
        if want_normals && self.normals.len() < self.positions.len() {
            self.normals.resize(self.positions.len(), [0.0, 0.0, 1.0]);
        }
        if want_uvs && self.uvs.len() < self.positions.len() {
            self.uvs.resize(self.positions.len(), [0.0, 0.0]);
        }

        self.positions.extend_from_slice(&other.positions);
        if want_normals {
            if other.normals.is_empty() {
                self.normals.resize(self.positions.len(), [0.0, 0.0, 1.0]);
            } else {
                self.normals.extend_from_slice(&other.normals);
            }
        }
        if want_uvs {
            if other.uvs.is_empty() {
                self.uvs.resize(self.positions.len(), [0.0, 0.0]);
            } else {
                self.uvs.extend_from_slice(&other.uvs);
            }
        }
        self.indices.extend(other.indices.iter().map(|i| i + base));
        self.parts.extend(other.parts.iter().map(|p| MeshPart {
            material: p.material,
            start: p.start + index_base,
            count: p.count,
        }));
    }

    /// Merge parts that use the same material into one run each.
    ///
    /// Reorders `indices`; vertex data is untouched.
    pub fn coalesce_parts(&mut self) {
        if self.parts.len() < 2 {
            return;
        }
        let mut order: Vec<usize> = (0..self.parts.len()).collect();
        order.sort_by_key(|&i| self.parts[i].material);

        let mut indices = Vec::with_capacity(self.indices.len());
        let mut parts: Vec<MeshPart> = Vec::new();
        for &i in &order {
            let p = self.parts[i];
            let range = p.start as usize..(p.start + p.count) as usize;
            let start = indices.len() as u32;
            indices.extend_from_slice(&self.indices[range]);
            match parts.last_mut() {
                Some(last) if last.material == p.material => last.count += p.count,
                _ => parts.push(MeshPart {
                    material: p.material,
                    start,
                    count: p.count,
                }),
            }
        }
        self.indices = indices;
        self.parts = parts;
    }

    /// Recompute normals by area-weighted face averaging, welding nothing.
    ///
    /// Only for meshes that arrived without normals. The tessellator produces
    /// exact analytic normals, which are strictly better — averaging across a
    /// CAD model's hard edges is precisely what makes a converted part look
    /// melted.
    pub fn recompute_normals(&mut self) {
        self.normals = vec![[0.0f32; 3]; self.positions.len()];
        for tri in self.indices.chunks_exact(3) {
            let (a, b, c) = (tri[0] as usize, tri[1] as usize, tri[2] as usize);
            let (pa, pb, pc) = (self.positions[a], self.positions[b], self.positions[c]);
            let u = [pb[0] - pa[0], pb[1] - pa[1], pb[2] - pa[2]];
            let v = [pc[0] - pa[0], pc[1] - pa[1], pc[2] - pa[2]];
            // Unnormalised, so the cross product's magnitude area-weights it.
            let n = [
                u[1] * v[2] - u[2] * v[1],
                u[2] * v[0] - u[0] * v[2],
                u[0] * v[1] - u[1] * v[0],
            ];
            for &i in &[a, b, c] {
                for k in 0..3 {
                    self.normals[i][k] += n[k];
                }
            }
        }
        for n in &mut self.normals {
            let len = (n[0] * n[0] + n[1] * n[1] + n[2] * n[2]).sqrt();
            if len > f32::EPSILON {
                for c in n.iter_mut() {
                    *c /= len;
                }
            } else {
                *n = [0.0, 0.0, 1.0];
            }
        }
    }

    /// Drop vertices no triangle references, compacting the buffers.
    ///
    /// Face-by-face tessellation naturally leaves some behind — a trim loop
    /// sample that every triangle avoided — and they are pure file size.
    pub fn drop_unused_vertices(&mut self) {
        let mut used = vec![false; self.positions.len()];
        for &i in &self.indices {
            if let Some(slot) = used.get_mut(i as usize) {
                *slot = true;
            }
        }
        if used.iter().all(|&u| u) {
            return;
        }

        let mut remap = vec![u32::MAX; self.positions.len()];
        let mut next = 0u32;
        for (i, &u) in used.iter().enumerate() {
            if u {
                remap[i] = next;
                next += 1;
            }
        }

        let keep = |v: &mut Vec<[f32; 3]>| {
            if v.len() == used.len() {
                let mut out = Vec::with_capacity(next as usize);
                for (i, &u) in used.iter().enumerate() {
                    if u {
                        out.push(v[i]);
                    }
                }
                *v = out;
            }
        };
        keep(&mut self.positions);
        keep(&mut self.normals);
        if self.uvs.len() == used.len() {
            let mut out = Vec::with_capacity(next as usize);
            for (i, &u) in used.iter().enumerate() {
                if u {
                    out.push(self.uvs[i]);
                }
            }
            self.uvs = out;
        }
        for i in &mut self.indices {
            *i = remap[*i as usize];
        }
    }

    /// Approximate size in bytes of the vertex and index data.
    pub fn byte_size(&self) -> usize {
        self.positions.len() * 12
            + self.normals.len() * 12
            + self.uvs.len() * 8
            + self.indices.len() * if self.fits_u16_indices() { 2 } else { 4 }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tri_mesh(material: u32) -> Mesh {
        Mesh {
            positions: vec![[0.0, 0.0, 0.0], [1.0, 0.0, 0.0], [0.0, 1.0, 0.0]],
            normals: vec![[0.0, 0.0, 1.0]; 3],
            uvs: vec![],
            indices: vec![0, 1, 2],
            parts: vec![MeshPart {
                material,
                start: 0,
                count: 3,
            }],
        }
    }

    #[test]
    fn append_offsets_indices_and_parts() {
        let mut a = tri_mesh(0);
        a.append(&tri_mesh(1));
        assert_eq!(a.vertex_count(), 6);
        assert_eq!(a.triangle_count(), 2);
        assert_eq!(a.indices, vec![0, 1, 2, 3, 4, 5]);
        assert_eq!(a.parts[1], MeshPart { material: 1, start: 3, count: 3 });
        assert_eq!(a.normals.len(), 6);
    }

    #[test]
    fn append_fills_missing_attributes_rather_than_desynchronising() {
        let mut a = tri_mesh(0);
        let mut b = tri_mesh(1);
        b.normals.clear();
        a.append(&b);
        assert_eq!(a.normals.len(), a.positions.len());

        let mut c = tri_mesh(0);
        c.normals.clear();
        c.append(&tri_mesh(1));
        assert_eq!(c.normals.len(), c.positions.len());
    }

    #[test]
    fn coalesce_merges_parts_sharing_a_material() {
        let mut m = tri_mesh(1);
        m.append(&tri_mesh(0));
        m.append(&tri_mesh(1));
        assert_eq!(m.parts.len(), 3);
        m.coalesce_parts();
        assert_eq!(m.parts.len(), 2);
        assert_eq!(m.parts[0].material, 0);
        assert_eq!(m.parts[1].material, 1);
        assert_eq!(m.parts[1].count, 6);
        // Every original triangle survives, just reordered.
        assert_eq!(m.indices.len(), 9);
        let total: u32 = m.parts.iter().map(|p| p.count).sum();
        assert_eq!(total as usize, m.indices.len());
    }

    #[test]
    fn coalesced_ranges_stay_contiguous_and_in_order() {
        let mut m = tri_mesh(2);
        m.append(&tri_mesh(0));
        m.append(&tri_mesh(2));
        m.append(&tri_mesh(0));
        m.coalesce_parts();
        let mut cursor = 0;
        for p in &m.parts {
            assert_eq!(p.start, cursor);
            cursor += p.count;
        }
        assert_eq!(cursor as usize, m.indices.len());
    }

    #[test]
    fn unused_vertices_are_dropped_and_indices_remapped() {
        let mut m = tri_mesh(0);
        m.positions.push([9.0, 9.0, 9.0]);
        m.normals.push([0.0, 0.0, 1.0]);
        assert_eq!(m.vertex_count(), 4);
        m.drop_unused_vertices();
        assert_eq!(m.vertex_count(), 3);
        assert_eq!(m.normals.len(), 3);
        assert_eq!(m.indices, vec![0, 1, 2]);
    }

    #[test]
    fn dropping_is_a_no_op_when_every_vertex_is_used() {
        let mut m = tri_mesh(0);
        let before = m.positions.clone();
        m.drop_unused_vertices();
        assert_eq!(m.positions, before);
    }

    #[test]
    fn recomputed_normals_face_the_winding() {
        let mut m = tri_mesh(0);
        m.normals.clear();
        m.recompute_normals();
        // Counter-clockwise in the XY plane means +Z.
        for n in &m.normals {
            assert!((n[2] - 1.0).abs() < 1e-6, "got {n:?}");
        }
    }

    #[test]
    fn index_width_follows_the_vertex_count() {
        let m = tri_mesh(0);
        assert!(m.fits_u16_indices());
        let mut big = m.clone();
        big.positions.resize(70_000, [0.0; 3]);
        assert!(!big.fits_u16_indices());
    }
}
