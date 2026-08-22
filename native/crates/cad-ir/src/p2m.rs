//! Reading SolidWorks appearance files, and what they say that a material
//! library cannot.
//!
//! A `.sldmat` states what a part is *made of* — steel, aluminium, ABS — and
//! that is the wrong question for a renderer twice over. It has no entry for
//! paint at all, because paint is not a material in SolidWorks but an
//! appearance; and where it does have an entry, its `Shininess` is a single
//! number that has plainly not been kept up to date (see
//! [`crate::sldmat::SldMaterial::roughness`]). The appearance library beside it
//! answers the question directly: 619 files, each naming a finish and stating
//! how it reflects.
//!
//! The format is a flat list of quoted keys and values:
//!
//! ```text
//! "blurryReflections" off
//! "reflectivity" 0
//! "roughness" 0.92
//! "sw_shader" ""
//! ```
//!
//! # Reading `roughness`
//!
//! It is not glTF roughness, and taking it for that gets clear glass wrong.
//! The file describes a PhotoView surface, where the reflection is either
//! sharp or blurred and `roughness` only controls the blur:
//!
//! * **`reflectivity` is zero** — there is no reflection at all, so nothing to
//!   sharpen or blur, and the number describes the surface itself. Taken as
//!   stated. This is the case for powder coat.
//! * **`blurryReflections off`** and reflectivity above zero — the reflection
//!   is a mirror however rough the number looks. Glass at 0.70, high-gloss
//!   plastic at 0.60 and chromium plate at 0.70 are all this, and all of them
//!   are smooth. Read as polished.
//! * **`blurryReflections on`** — the number is the blur, which is exactly
//!   what glTF roughness is. Taken as stated: brushed chromium 0.25, burnished
//!   chrome 0.50, cast chromium plate 0.80, matte rubber 0.85.
//!
//! Every calibration point above is a file in the bundled tree; the rule was
//! chosen to fit them rather than the other way round.

use crate::material::{Material, MaterialClass, MaterialSource};
use std::collections::HashMap;

include!(concat!(env!("OUT_DIR"), "/appearances.rs"));

/// A surface finish as the appearance library states it.
#[derive(Debug, Clone, Default)]
pub struct Appearance {
    /// Path within the library, lower-cased and without the extension, e.g.
    /// `painted/powder coat/dark powdercoat`.
    pub path: String,
    /// The renderer's own shader name, where the file names one.
    pub shader: String,
    /// `col1`, the primary colour, as the file's own 0..1 triple.
    pub colour: Option<[f32; 3]>,
    pub roughness: f64,
    pub reflectivity: f64,
    pub specular_factor: f64,
    pub diffuse_factor: f64,
    pub transparency: f64,
    /// Whether the reflection is blurred at all. Absent means it is not.
    pub blurry: bool,
    /// The file carries `metallic_color` or `metallic_ior`, which only the
    /// true metals do — 41 of the 619.
    pub metallic: bool,
    pub ior: Option<f64>,
}

/// A surface that reflects sharply, in glTF's terms.
///
/// Not zero: a perfect mirror has no highlight at all under a punctual light,
/// which reads as a black surface rather than a shiny one — the same floor
/// [`crate::sldmat`] holds to.
const POLISHED: f64 = 0.05;

impl Appearance {
    /// The finish as glTF roughness. See the module note for the reading.
    pub fn roughness(&self) -> f32 {
        let r = if self.reflectivity <= 1e-6 {
            self.roughness
        } else if !self.blurry {
            POLISHED
        } else {
            self.roughness
        };
        r.clamp(POLISHED, 1.0) as f32
    }

    /// The finish as a material, carrying no colour: the colour of a part is
    /// the file's own, and an appearance only says how it is finished.
    pub fn to_material(&self, name: impl Into<String>) -> Material {
        let name = name.into();
        let metallic = if self.metallic { 1.0 } else { 0.0 };
        Material {
            name: name.clone(),
            base_color: self.colour.unwrap_or([0.5, 0.5, 0.5]),
            alpha: (1.0 - self.transparency).clamp(0.0, 1.0) as f32,
            metallic,
            roughness: self.roughness(),
            ior: self.ior.unwrap_or(1.5) as f32,
            transmission: 0.0,
            emissive: [0.0; 3],
            double_sided: false,
            source: MaterialSource::Named {
                raw: self.path.clone(),
                preset: MaterialClass::Paint,
            },
        }
    }
}

/// The appearance library carried with the crate.
#[derive(Debug, Clone, Default)]
pub struct AppearanceLibrary {
    pub appearances: HashMap<String, Appearance>,
}

impl AppearanceLibrary {
    /// The library that ships with this crate: every `.p2m` under
    /// `assets/Materials`, keyed by its path without the extension.
    ///
    /// Parsed once for the process. It is 619 files and 332 KB of text, and it
    /// is asked for once per material resolved — which is once per face on a
    /// painted assembly. Building it each time cost the pilot around half a
    /// gigabyte of allocator churn, against a finished scene of 19 MB.
    pub fn bundled() -> &'static AppearanceLibrary {
        static BUNDLED_LIBRARY: std::sync::OnceLock<AppearanceLibrary> = std::sync::OnceLock::new();
        BUNDLED_LIBRARY.get_or_init(|| AppearanceLibrary {
            appearances: BUNDLED_APPEARANCES
                .iter()
                .map(|(path, text)| ((*path).to_string(), parse(path, text)))
                .collect(),
        })
    }

    pub fn get(&self, path: &str) -> Option<&Appearance> {
        self.appearances.get(&path.to_lowercase())
    }
}

/// Which appearance stands for a class the material library cannot describe.
///
/// Only the finishes a machine assembly is actually delivered in. Paint is the
/// one that matters: it is the majority of a painted assembly's surface and a
/// `.sldmat` has no entry for it, so before this its gloss was the one number
/// in the pipeline with nothing behind it.
pub fn representative(class: MaterialClass, matte: bool) -> Option<&'static str> {
    match class {
        // A machine casting is powder-coated or sprayed, not car-painted. The
        // designer's own per-face reflectivity says which of the two readings
        // applies; where it says nothing, a delivered machine is matte more
        // often than it is glossy.
        MaterialClass::Paint if matte => Some("painted/powder coat/dark powdercoat"),
        MaterialClass::Paint => Some("painted/car/gloss blue"),
        _ => None,
    }
}

fn parse(path: &str, text: &str) -> Appearance {
    let mut a = Appearance {
        path: path.to_string(),
        ..Default::default()
    };
    for line in text.lines() {
        let line = line.trim();
        let Some(rest) = line.strip_prefix('"') else { continue };
        let Some(end) = rest.find('"') else { continue };
        let (key, value) = (&rest[..end], rest[end + 1..].trim());
        let number = || value.trim_matches('"').parse::<f64>().ok();
        match key {
            "roughness" => a.roughness = number().unwrap_or(0.0),
            "reflectivity" => a.reflectivity = number().unwrap_or(0.0),
            "specular_factor" => a.specular_factor = number().unwrap_or(0.0),
            "diffuse_factor" => a.diffuse_factor = number().unwrap_or(1.0),
            "transparency" => a.transparency = number().unwrap_or(0.0),
            "mtl_ior" => a.ior = number(),
            "blurryReflections" => a.blurry = value.starts_with("on"),
            "sw_shader" => a.shader = value.trim_matches('"').to_lowercase(),
            "metallic_color" | "metallic_ior" => a.metallic = true,
            "col1" => {
                let v: Vec<f32> = value.split_whitespace().filter_map(|t| t.parse().ok()).collect();
                if let [r, g, b] = v[..] {
                    a.colour = Some([r, g, b]);
                }
            }
            _ => {}
        }
    }
    a
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_library_is_there_and_parses() {
        let lib = AppearanceLibrary::bundled();
        assert!(lib.appearances.len() > 600, "got {}", lib.appearances.len());
        assert!(lib.appearances.values().all(|a| a.roughness >= 0.0));
    }

    #[test]
    fn powder_coat_is_the_matte_paint_the_sldmat_has_no_entry_for() {
        let lib = AppearanceLibrary::bundled();
        let p = lib
            .get(representative(MaterialClass::Paint, true).unwrap())
            .expect("powder coat");
        // Stated in the file: no reflection at all, and a rough surface.
        assert_eq!(p.reflectivity, 0.0);
        assert!(!p.blurry);
        assert!((p.roughness() - 0.92).abs() < 1e-6, "got {}", p.roughness());
    }

    #[test]
    fn a_sharp_reflection_reads_as_polished_whatever_the_number_says() {
        let lib = AppearanceLibrary::bundled();
        // Clear glass states roughness 0.70 and blurry reflections off. Taken
        // literally it would be sandblasted; it is a mirror.
        let glass = lib.get("glass/gloss/clear glass").expect("clear glass");
        assert!(glass.roughness > 0.5, "the file really does say {}", glass.roughness);
        assert!(!glass.blurry);
        assert_eq!(glass.roughness(), POLISHED as f32);
        // A blurred reflection is read as stated.
        let brushed = lib.get("metal/chrome/brushed chromium").expect("brushed chromium");
        assert!(brushed.blurry);
        assert!((brushed.roughness() - 0.25).abs() < 1e-6);
    }

    #[test]
    fn only_the_true_metals_carry_a_metallic_colour() {
        let lib = AppearanceLibrary::bundled();
        let metals = lib.appearances.values().filter(|a| a.metallic).count();
        assert_eq!(metals, 41, "the bundled tree has 41 such files");
        assert!(!lib.get("painted/powder coat/dark powdercoat").unwrap().metallic);
    }
}
