//! USD's binary encoding, so a USDZ is the size of the mesh it carries.
//!
//! The text form this project writes elsewhere is correct and every reader
//! takes it, but USD spells every coordinate out and a USDZ may not compress
//! anything: the pilot assembly comes to 172 MB against 40 MB as glTF. Handing
//! that same text to `usdcat` gives 43 MB, so the container is the whole
//! difference and nothing is wrong with the scene.
//!
//! The crate format was learned by taking apart files USD produced rather than
//! from a specification, and `tools/usdc_decode.py` is what did the taking
//! apart. It is kept in the repository because it is also how a file written
//! here is checked against one written by USD: run it over both and the
//! sections line up or they do not.
//!
//! # The shape of a file
//!
//! ```text
//! [88-byte header: "PXR-USDC", version, offset of the table of contents]
//! [value data — arrays and anything too big to inline]
//! [TOKENS] [STRINGS] [FIELDS] [FIELDSETS] [PATHS] [SPECS]
//! [table of contents]
//! ```
//!
//! Nothing in it is a tree. A prim is a *spec*: a path, a set of fields, and a
//! kind. The tree is in `PATHS`, as a pre-order walk with one jump per entry —
//! `-1` for a prim with children and no sibling, `0` for one with a sibling
//! and no children, `-2` for the last of a run, and a positive number for how
//! far ahead the next sibling sits.

mod coding;
mod value;
mod scene;
mod write;

pub use value::Value;
pub use write::{write, Spec, SpecKind};

/// A scene as a crate file.
///
/// `image_paths` are the names the images will have inside the package, in the
/// same order as `scene.images`.
pub fn write_scene(
    scene: &cad_ir::Scene,
    options: &crate::Options,
    image_paths: &[String],
) -> Vec<u8> {
    write(&scene::specs(scene, options, image_paths))
}
