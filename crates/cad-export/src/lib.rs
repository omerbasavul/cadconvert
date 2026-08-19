//! Writers for the three delivery formats.
//!
//! Everything here reads a [`cad_ir::Scene`] and nothing else, so a new input
//! format costs nothing on this side.
//!
//! # Conventions the writers must reconcile
//!
//! The scene is **millimetres, Z up, right-handed** — mechanical CAD's
//! convention and what both source formats already use. The targets differ:
//!
//! | Format | Units | Up axis | How it is handled |
//! |--------|-------|---------|-------------------|
//! | glTF/GLB | metres | Y | a root node transform, so instancing survives |
//! | USDZ | declared | declared | `metersPerUnit` and `upAxis` metadata, no geometry change |
//! | OBJ | none | none | baked into the coordinates, since OBJ can say nothing |
//!
//! Doing it with a root transform rather than by rewriting every vertex is not
//! only cheaper — it keeps a shared geometry shared, which is the single
//! largest lever on output size for an assembly.

#![forbid(unsafe_code)]

pub mod error;
pub mod glb;
pub mod prepare;

pub use error::{ExportError, Result};

use cad_ir::math::Transform;

/// Which axis points up in the output.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum UpAxis {
    /// glTF's convention, and what most web viewers assume.
    #[default]
    Y,
    /// The scene's own convention; USD can declare it.
    Z,
}

/// How aggressively to shrink the output.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Compression {
    /// Plain float positions and normals.
    #[default]
    None,
    /// Quantise positions to 16-bit and normals to 8-bit octahedral, declaring
    /// `KHR_mesh_quantization`.
    ///
    /// Roughly halves the vertex data and every glTF 2.0 viewer that supports
    /// the extension reads it directly — no decoder to ship.
    Quantized,
}

/// Options common to every writer.
#[derive(Debug, Clone, PartialEq)]
pub struct Options {
    pub up_axis: UpAxis,
    /// Multiplier from scene units to the output's units.
    pub unit_scale: f64,
    /// Emit per-vertex normals.
    pub normals: bool,
    pub compression: Compression,
    /// Collapse the assembly tree, writing each instance's geometry separately.
    ///
    /// Costs size — a bolt used eighty times is stored eighty times — and is
    /// only worth it for a consumer that cannot follow a node hierarchy.
    pub flatten: bool,
    /// Name written into the output's asset metadata.
    pub generator: String,
}

impl Default for Options {
    fn default() -> Self {
        Options {
            up_axis: UpAxis::Y,
            unit_scale: 1e-3,
            normals: true,
            compression: Compression::None,
            flatten: false,
            generator: format!("cadmesh {}", env!("CARGO_PKG_VERSION")),
        }
    }
}

impl Options {
    /// The transform taking scene coordinates into the output's frame.
    ///
    /// Millimetres to the target unit, and Z-up to the target's up axis. The
    /// Z-up→Y-up step is a −90° rotation about X, which preserves handedness —
    /// negating an axis instead would mirror the model and invert every normal.
    pub fn root_transform(&self) -> Transform {
        let s = self.unit_scale;
        match self.up_axis {
            UpAxis::Z => Transform::from_scale(s),
            UpAxis::Y => Transform {
                m: [
                    [s, 0.0, 0.0, 0.0],
                    [0.0, 0.0, s, 0.0],
                    [0.0, -s, 0.0, 0.0],
                ],
            },
        }
    }

    /// Options producing the smallest standards-compliant GLB.
    pub fn compact() -> Options {
        Options {
            compression: Compression::Quantized,
            ..Options::default()
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use cad_ir::math::Vec3;

    #[test]
    fn the_root_transform_converts_millimetres_to_metres() {
        let t = Options::default().root_transform();
        let p = t.point(Vec3::new(1000.0, 0.0, 0.0));
        assert!((p.x - 1.0).abs() < 1e-12);
    }

    #[test]
    fn z_up_becomes_y_up_without_mirroring() {
        let t = Options::default().root_transform();
        // The scene's +Z must land on the output's +Y.
        let up = t.direction(Vec3::Z);
        assert!((up.y - 1e-3).abs() < 1e-15, "got {up:?}");
        assert!(up.x.abs() < 1e-15 && up.z.abs() < 1e-15);
        // The scene's +Y must land on the output's −Z, keeping the frame right
        // handed. A determinant of the wrong sign would flip every triangle.
        let fwd = t.direction(Vec3::Y);
        assert!((fwd.z + 1e-3).abs() < 1e-15, "got {fwd:?}");
        assert!(t.determinant() > 0.0, "the conversion mirrors the model");
    }

    #[test]
    fn z_up_output_only_scales() {
        let t = Options {
            up_axis: UpAxis::Z,
            ..Options::default()
        }
        .root_transform();
        assert!((t.direction(Vec3::Z).z - 1e-3).abs() < 1e-15);
        assert!(t.determinant() > 0.0);
    }
}
