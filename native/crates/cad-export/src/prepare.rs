//! Shared work every writer needs before it can emit anything.

use cad_ir::material::{Material, MaterialSource};
use cad_ir::math::Transform;
use cad_ir::scene::{GeometryId, MaterialId, Scene};
use std::collections::HashMap;

/// One geometry placed once in the world.
#[derive(Debug, Clone, Copy)]
pub struct DrawItem {
    pub geometry: GeometryId,
    pub transform: Transform,
    pub material: Option<MaterialId>,
}

/// Every instance in the scene, with its world transform.
///
/// For a writer that cannot express a hierarchy. Writers that can should walk
/// the tree instead and keep the sharing.
pub fn flatten(scene: &Scene, root: Transform) -> Vec<DrawItem> {
    scene
        .instances()
        .into_iter()
        .filter(|i| {
            scene
                .geometry_of(i.geometry)
                .mesh
                .as_ref()
                .is_some_and(|m| !m.is_empty())
        })
        .map(|i| DrawItem {
            geometry: i.geometry,
            transform: i.transform.then(&root),
            material: i.material,
        })
        .collect()
}

/// Make names unique and safe for formats that key on them.
///
/// USD prim paths and OBJ group names both break on a duplicate or on
/// punctuation, and CAD part numbers are full of spaces and dots.
#[derive(Debug, Default)]
pub struct Names {
    used: HashMap<String, usize>,
}

impl Names {
    /// A unique identifier derived from `raw`.
    ///
    /// Keeps ASCII letters, digits and underscores; everything else becomes an
    /// underscore. A name that would start with a digit gets a leading
    /// underscore, because USD prim names may not.
    pub fn unique(&mut self, raw: &str) -> String {
        let mut base: String = raw
            .chars()
            .map(|c| if c.is_ascii_alphanumeric() { c } else { '_' })
            .collect();
        while base.ends_with('_') {
            base.pop();
        }
        if base.is_empty() {
            base.push_str("unnamed");
        }
        if base.starts_with(|c: char| c.is_ascii_digit()) {
            base.insert(0, '_');
        }

        let count = self.used.entry(base.clone()).or_insert(0);
        *count += 1;
        if *count == 1 {
            base
        } else {
            format!("{base}_{}", *count - 1)
        }
    }
}

/// A short, human-meaningful label for a material.
pub fn material_label(m: &Material) -> String {
    match &m.source {
        MaterialSource::Named { raw, preset } => format!("{raw} ({preset:?})"),
        MaterialSource::Colour => m.name.clone(),
        MaterialSource::Default => "default".into(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn names_are_sanitised_and_deduplicated() {
        let mut n = Names::default();
        assert_eq!(n.unique("214 201 007"), "_214_201_007");
        assert_eq!(n.unique("214 201 007"), "_214_201_007_1");
        // A part number starting with a digit is not a legal USD prim name.
        assert_eq!(n.unique("105.A2080"), "_105_A2080");
    }

    #[test]
    fn a_leading_digit_gets_a_prefix() {
        let mut n = Names::default();
        assert!(n.unique("7bolt").starts_with('_'));
        assert_eq!(n.unique("bolt"), "bolt");
    }

    #[test]
    fn an_empty_or_symbol_only_name_still_produces_something() {
        let mut n = Names::default();
        assert_eq!(n.unique(""), "unnamed");
        assert_eq!(n.unique("---"), "unnamed_1");
    }

    #[test]
    fn material_labels_name_their_provenance() {
        use cad_ir::material::MaterialClass;
        let named = Material::from_class(MaterialClass::Steel, "AISI 1018");
        assert!(material_label(&named).contains("AISI 1018"));
        assert!(material_label(&named).contains("Steel"));
        assert_eq!(material_label(&Material::unknown()), "default");
    }
}
