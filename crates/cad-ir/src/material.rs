//! Appearance, expressed once in the terms every target format shares.
//!
//! glTF, USD `UsdPreviewSurface` and OBJ's MTL all describe a surface
//! differently, but glTF's metallic-roughness model is the common denominator
//! the other two can be derived from without guessing. So the IR stores
//! metallic-roughness, and each writer lowers from it.
//!
//! A [`Material`] also keeps the *reason* it looks the way it does, in
//! [`Material::source`]. Whether a grey face is grey because the file named its
//! material "AISI 304" or because nothing was known and grey was the fallback
//! changes what a user should be told, and a pipeline that throws that away
//! cannot tell them.

/// A physically-based surface appearance.
#[derive(Debug, Clone, PartialEq)]
pub struct Material {
    /// Display name, unique within a [`crate::Scene`].
    pub name: String,
    /// Linear RGB base colour. Albedo for a dielectric, reflectance tint for a
    /// metal.
    pub base_color: [f32; 3],
    /// Opacity. Below 1.0 the writers emit a blended material.
    pub alpha: f32,
    /// 0 for a dielectric, 1 for a metal. Values between are for transitions
    /// only; no real surface is half a metal.
    pub metallic: f32,
    /// Perceptual roughness: 0 is a mirror, 1 is fully diffuse.
    pub roughness: f32,
    /// Index of refraction, used by the dielectric specular response. 1.5 is
    /// the glTF default and right for most plastics.
    pub ior: f32,
    /// Fraction of light transmitted through the surface, for glass.
    pub transmission: f32,
    /// Linear RGB emission.
    pub emissive: [f32; 3],
    /// Render both faces. Necessary for sheet bodies, which have no inside.
    pub double_sided: bool,
    /// Where this material's values came from.
    pub source: MaterialSource,
}

/// Provenance of a material's appearance.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum MaterialSource {
    /// A named engineering material was found and mapped to a PBR preset.
    Named {
        /// The name as written in the source file or sidecar, verbatim.
        raw: String,
        /// The preset it was recognised as.
        preset: MaterialClass,
    },
    /// Only a colour was available; the shading parameters are a default.
    Colour,
    /// Nothing was known.
    Default,
}

/// The families a named engineering material is recognised into.
///
/// Deliberately coarse. A converter cannot tell 304 stainless from 316 by
/// looking, and pretending otherwise produces a table nobody can maintain; what
/// matters visually is metal-vs-dielectric, how rough, and roughly what colour.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum MaterialClass {
    Steel,
    StainlessSteel,
    CastIron,
    Aluminium,
    AnodisedAluminium,
    Copper,
    Brass,
    Bronze,
    Titanium,
    Chrome,
    Zinc,
    Gold,
    Silver,
    Plastic,
    Rubber,
    Glass,
    Wood,
    Ceramic,
    Concrete,
    Paint,
    Fabric,
    Foam,
    Composite,
}

impl Material {
    /// A neutral material for geometry with no appearance information.
    pub fn unknown() -> Material {
        Material {
            name: "default".into(),
            // Linear 0.25 is sRGB ~0.54, a mid grey that reads as unpainted
            // metal without implying a specific one.
            base_color: [0.25, 0.25, 0.26],
            alpha: 1.0,
            metallic: 0.0,
            roughness: 0.55,
            ior: 1.5,
            transmission: 0.0,
            emissive: [0.0; 3],
            double_sided: false,
            source: MaterialSource::Default,
        }
    }

    /// A material carrying only a colour, shaded as a smooth dielectric.
    ///
    /// Not metallic: guessing "metal" from a grey colour would make every
    /// painted grey bracket a mirror, and a wrong metal reads far worse than a
    /// slightly-too-plastic dielectric.
    pub fn from_colour(name: impl Into<String>, linear_rgb: [f32; 3], alpha: f32) -> Material {
        Material {
            name: name.into(),
            base_color: linear_rgb,
            alpha,
            metallic: 0.0,
            roughness: 0.5,
            ior: 1.5,
            transmission: 0.0,
            emissive: [0.0; 3],
            double_sided: false,
            source: MaterialSource::Colour,
        }
    }

    /// The preset appearance of a material class.
    ///
    /// Base colours are linear-RGB reflectance values. The metals use measured
    /// F0 reflectance, which is why aluminium is not pure white and copper and
    /// gold are strongly tinted.
    pub fn from_class(class: MaterialClass, raw: impl Into<String>) -> Material {
        let raw = raw.into();
        let (base_color, metallic, roughness, ior, transmission) = match class {
            MaterialClass::Steel => ([0.560, 0.570, 0.580], 1.0, 0.38, 1.5, 0.0),
            MaterialClass::StainlessSteel => ([0.660, 0.660, 0.660], 1.0, 0.22, 1.5, 0.0),
            MaterialClass::CastIron => ([0.280, 0.280, 0.290], 1.0, 0.62, 1.5, 0.0),
            MaterialClass::Aluminium => ([0.913, 0.921, 0.925], 1.0, 0.32, 1.5, 0.0),
            MaterialClass::AnodisedAluminium => ([0.560, 0.570, 0.580], 1.0, 0.45, 1.5, 0.0),
            MaterialClass::Copper => ([0.955, 0.638, 0.538], 1.0, 0.30, 1.5, 0.0),
            MaterialClass::Brass => ([0.887, 0.789, 0.434], 1.0, 0.28, 1.5, 0.0),
            MaterialClass::Bronze => ([0.714, 0.428, 0.181], 1.0, 0.35, 1.5, 0.0),
            MaterialClass::Titanium => ([0.542, 0.497, 0.449], 1.0, 0.35, 1.5, 0.0),
            MaterialClass::Chrome => ([0.550, 0.556, 0.554], 1.0, 0.06, 1.5, 0.0),
            MaterialClass::Zinc => ([0.664, 0.824, 0.850], 1.0, 0.40, 1.5, 0.0),
            MaterialClass::Gold => ([1.000, 0.766, 0.336], 1.0, 0.20, 1.5, 0.0),
            MaterialClass::Silver => ([0.972, 0.960, 0.915], 1.0, 0.15, 1.5, 0.0),
            MaterialClass::Plastic => ([0.220, 0.220, 0.230], 0.0, 0.35, 1.46, 0.0),
            MaterialClass::Rubber => ([0.020, 0.020, 0.020], 0.0, 0.88, 1.52, 0.0),
            MaterialClass::Glass => ([0.900, 0.930, 0.920], 0.0, 0.05, 1.52, 0.9),
            MaterialClass::Wood => ([0.230, 0.140, 0.070], 0.0, 0.72, 1.53, 0.0),
            MaterialClass::Ceramic => ([0.800, 0.790, 0.760], 0.0, 0.18, 1.60, 0.0),
            MaterialClass::Concrete => ([0.330, 0.325, 0.310], 0.0, 0.90, 1.50, 0.0),
            MaterialClass::Paint => ([0.250, 0.250, 0.250], 0.0, 0.30, 1.50, 0.0),
            MaterialClass::Fabric => ([0.180, 0.180, 0.190], 0.0, 0.95, 1.46, 0.0),
            MaterialClass::Foam => ([0.400, 0.400, 0.390], 0.0, 0.98, 1.45, 0.0),
            MaterialClass::Composite => ([0.045, 0.045, 0.048], 0.0, 0.42, 1.55, 0.0),
        };
        Material {
            name: raw.clone(),
            base_color,
            alpha: if transmission > 0.0 { 1.0 } else { 1.0 },
            metallic,
            roughness,
            ior,
            transmission,
            emissive: [0.0; 3],
            double_sided: false,
            source: MaterialSource::Named { raw, preset: class },
        }
    }

    /// The class's preset shading with the colour the file actually assigned.
    ///
    /// This is the common case in practice: a file names a material *and*
    /// carries a per-face colour. The colour is what the user chose to see, and
    /// the class supplies the metalness and roughness the colour cannot.
    pub fn from_class_tinted(
        class: MaterialClass,
        raw: impl Into<String>,
        linear_rgb: [f32; 3],
        alpha: f32,
    ) -> Material {
        Material {
            base_color: linear_rgb,
            alpha,
            ..Material::from_class(class, raw)
        }
    }

    /// True when the writers must emit a blended, not opaque, material.
    pub fn is_transparent(&self) -> bool {
        self.alpha < 0.999 || self.transmission > 0.001
    }

    /// A stable key for deduplicating materials.
    ///
    /// Bit patterns rather than rounded values: two materials that differ by
    /// one ulp are visually identical, but merging them would need a tolerance
    /// and a tolerance here silently loses distinctions the source file drew.
    pub fn dedup_key(&self) -> MaterialKey {
        MaterialKey {
            name: self.name.clone(),
            bits: [
                self.base_color[0].to_bits(),
                self.base_color[1].to_bits(),
                self.base_color[2].to_bits(),
                self.alpha.to_bits(),
                self.metallic.to_bits(),
                self.roughness.to_bits(),
                self.ior.to_bits(),
                self.transmission.to_bits(),
                self.emissive[0].to_bits(),
                self.emissive[1].to_bits(),
                self.emissive[2].to_bits(),
            ],
            double_sided: self.double_sided,
        }
    }
}

/// Deduplication key for [`Material`].
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct MaterialKey {
    name: String,
    bits: [u32; 11],
    double_sided: bool,
}

impl MaterialClass {
    /// Every class, for exhaustive tests and table dumps.
    pub const ALL: &'static [MaterialClass] = &[
        MaterialClass::Steel,
        MaterialClass::StainlessSteel,
        MaterialClass::CastIron,
        MaterialClass::Aluminium,
        MaterialClass::AnodisedAluminium,
        MaterialClass::Copper,
        MaterialClass::Brass,
        MaterialClass::Bronze,
        MaterialClass::Titanium,
        MaterialClass::Chrome,
        MaterialClass::Zinc,
        MaterialClass::Gold,
        MaterialClass::Silver,
        MaterialClass::Plastic,
        MaterialClass::Rubber,
        MaterialClass::Glass,
        MaterialClass::Wood,
        MaterialClass::Ceramic,
        MaterialClass::Concrete,
        MaterialClass::Paint,
        MaterialClass::Fabric,
        MaterialClass::Foam,
        MaterialClass::Composite,
    ];

    /// True for classes shaded as metals.
    pub fn is_metal(self) -> bool {
        matches!(
            self,
            MaterialClass::Steel
                | MaterialClass::StainlessSteel
                | MaterialClass::CastIron
                | MaterialClass::Aluminium
                | MaterialClass::AnodisedAluminium
                | MaterialClass::Copper
                | MaterialClass::Brass
                | MaterialClass::Bronze
                | MaterialClass::Titanium
                | MaterialClass::Chrome
                | MaterialClass::Zinc
                | MaterialClass::Gold
                | MaterialClass::Silver
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_class_has_a_plausible_preset() {
        for &c in MaterialClass::ALL {
            let m = Material::from_class(c, "x");
            assert_eq!(
                m.metallic,
                if c.is_metal() { 1.0 } else { 0.0 },
                "{c:?} metallic disagrees with is_metal"
            );
            assert!(
                (0.0..=1.0).contains(&m.roughness),
                "{c:?} roughness out of range"
            );
            assert!(m.ior >= 1.0, "{c:?} ior below vacuum");
            assert!(
                m.base_color.iter().all(|v| (0.0..=1.0).contains(v)),
                "{c:?} base colour out of range"
            );
        }
    }

    #[test]
    fn a_named_material_records_both_the_raw_name_and_the_preset() {
        let m = Material::from_class(MaterialClass::StainlessSteel, "AISI 304");
        assert_eq!(m.name, "AISI 304");
        assert_eq!(
            m.source,
            MaterialSource::Named {
                raw: "AISI 304".into(),
                preset: MaterialClass::StainlessSteel
            }
        );
    }

    #[test]
    fn a_colour_only_material_is_never_metallic() {
        // A grey painted bracket must not become a mirror.
        let m = Material::from_colour("grey", [0.21, 0.21, 0.21], 1.0);
        assert_eq!(m.metallic, 0.0);
        assert_eq!(m.source, MaterialSource::Colour);
    }

    #[test]
    fn tinting_keeps_the_class_shading_and_takes_the_file_colour() {
        let m = Material::from_class_tinted(
            MaterialClass::Aluminium,
            "6061-T6",
            [0.0, 0.2, 0.5],
            1.0,
        );
        assert_eq!(m.base_color, [0.0, 0.2, 0.5]);
        assert_eq!(m.metallic, 1.0);
        assert_eq!(m.roughness, Material::from_class(MaterialClass::Aluminium, "x").roughness);
    }

    #[test]
    fn transparency_is_detected_from_alpha_or_transmission() {
        assert!(!Material::unknown().is_transparent());
        assert!(Material::from_class(MaterialClass::Glass, "glass").is_transparent());
        let mut m = Material::unknown();
        m.alpha = 0.5;
        assert!(m.is_transparent());
    }

    #[test]
    fn dedup_keys_separate_materials_that_differ_in_any_field() {
        let a = Material::from_class(MaterialClass::Steel, "steel");
        let mut b = a.clone();
        assert_eq!(a.dedup_key(), b.dedup_key());
        b.roughness += 0.01;
        assert_ne!(a.dedup_key(), b.dedup_key());
        let mut c = a.clone();
        c.double_sided = true;
        assert_ne!(a.dedup_key(), c.dedup_key());
    }

    #[test]
    fn metals_are_brighter_than_the_dielectrics() {
        // A sanity check on the F0 values: a metal's reflectance should not be
        // as dark as rubber's albedo, or the presets have been transposed.
        let rubber = Material::from_class(MaterialClass::Rubber, "r").base_color[0];
        for &c in MaterialClass::ALL.iter().filter(|c| c.is_metal()) {
            assert!(
                Material::from_class(c, "m").base_color[0] > rubber,
                "{c:?} is darker than rubber"
            );
        }
    }
}
