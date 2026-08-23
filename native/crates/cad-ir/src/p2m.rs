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

use crate::material::{Material, MaterialClass, MaterialSource, Textures};
use std::collections::HashMap;

include!(concat!(env!("OUT_DIR"), "/appearances.rs"));
include!(concat!(env!("OUT_DIR"), "/textures.rs"));

/// The bytes of a texture carried in the binary, by the normalised path an
/// [`Appearance`] names it with.
///
/// Only the finishes [`representative`] can select bring their images; the
/// library names 349 images and 255 MB of them, which is not something to put
/// in a converter. Everything else returns `None` and converts without a
/// texture, exactly as it did before there were any.
pub fn bundled_texture(path: &str) -> Option<&'static [u8]> {
    BUNDLED_TEXTURES
        .iter()
        .find(|(key, _, _)| *key == path)
        .map(|(_, bytes, _)| *bytes)
}

/// What the shipped image is called, which is not always what the appearance
/// calls it: a `.dds` ships as a `.png` because that is what it was decoded
/// into, and a colour image ships as the grain that replaced it. Naming the
/// original in the output would put a `.dds` extension on PNG bytes.
pub fn bundled_texture_name(path: &str) -> &str {
    let file = path.rsplit('/').next().unwrap_or(path);
    BUNDLED_SHIPPED
        .iter()
        .find(|(key, _)| *key == file)
        .map(|(_, name)| *name)
        .unwrap_or(file)
}

/// What a bundled colour image averages, so a caller can put the level back.
///
/// One for an image carried as it was. Less for a grain whose level was
/// divided out and then clipped — 0.905 for the powder coat, because a grain
/// runs above its own mean as often as below and there is nowhere above one
/// for it to go.
pub fn bundled_texture_level(path: &str) -> f32 {
    BUNDLED_TEXTURES
        .iter()
        .find(|(key, _, _)| *key == path)
        .map(|(_, _, level)| *level)
        .unwrap_or(1.0)
}

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
    /// `color_texname`: the image the finish is coloured by, as a path
    /// relative to the SolidWorks data root, normalised — lower case, forward
    /// slashes, no `..`. 229 of the 619 name one.
    pub colour_texture: Option<String>,
    /// A tangent-space normal map. `bumpTexture` with `bumpIsNormalMap on`,
    /// which is 156 files and always an `_n.dds` under `shaders/surfacefinish`.
    pub normal_texture: Option<String>,
    /// A height map, where that is all the file offers: `bump_file_texture`
    /// pointing at a grey `*bump.jpg`. Not a normal map, and not
    /// interchangeable with one.
    pub height_texture: Option<String>,
    /// `bumpStrength`, in metres of relief. Almost always 0.001.
    pub bump_strength: f64,
    /// `initTextureWidth` and `initTextureHeight`: the physical size of one
    /// tile, in metres. Powder coat is 6.35 mm. This is what makes a texture
    /// the right size on a part rather than stretched across it, and it is the
    /// reason texture coordinates have to be generated at world scale.
    pub tile_metres: Option<[f64; 2]>,
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
            // The images this appearance names are attached by whoever can
            // load them; an Appearance has no Scene to put them in.
            textures: Textures::default(),
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

/// A texture path as the file writes it, made into something that can be
/// looked up: lower case, forward slashes, and `..` resolved. The files mix
/// separators within a single path — `images\\shaders\\../textures/...` is
/// theirs, not a typo — and mix case between `Images` and `images`.
fn texture_path(raw: &str) -> Option<String> {
    let raw = raw.trim().trim_matches('"');
    if raw.is_empty() {
        return None;
    }
    let lowered = raw.replace('\\', "/").to_lowercase();
    let mut parts: Vec<&str> = Vec::new();
    for part in lowered.split('/') {
        match part {
            "" | "." => {}
            ".." => {
                parts.pop();
            }
            _ => parts.push(part),
        }
    }
    (!parts.is_empty()).then(|| parts.join("/"))
}

fn parse(path: &str, text: &str) -> Appearance {
    let mut a = Appearance {
        path: path.to_string(),
        diffuse_factor: 1.0,
        ..Default::default()
    };
    // `bumpTexture` names a normal map only when the file says so; otherwise
    // the same key is a height map. Both are read, and the decision is made
    // once at the end, because the two lines can arrive in either order.
    let mut bump_texture = None;
    let mut bump_is_normal_map = false;
    let mut bump_file_texture = None;
    let mut tile = [None, None];

    for line in text.lines() {
        let line = line.trim();
        // `color texture "color_texname" "Images\\textures\\..."` — the one
        // line in the format that does not begin with a quoted key, which is
        // why every texture in the library went unread.
        if let Some(rest) = line.strip_prefix("color texture ") {
            let mut quoted = rest.split('"').skip(1).step_by(2);
            if let (Some(key), Some(value)) = (quoted.next(), quoted.next()) {
                match key {
                    "color_texname" => a.colour_texture = texture_path(value),
                    "bump_file_texture" => bump_file_texture = texture_path(value),
                    _ => {}
                }
            }
            continue;
        }
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
            // An index of refraction of exactly 1 is vacuum, and 533 of the
            // 619 files state it — for powder coat, for rubber, for concrete.
            // It means "not stated", not "does not refract": taken literally
            // it reaches glTF as KHR_materials_ior 1.0, which switches the
            // Fresnel response off entirely and leaves paint with no sheen at
            // any angle. The 47 files that state a real one — glass at 1.52,
            // reflective glass at 1.591 — are the ones worth carrying.
            "mtl_ior" => a.ior = number().filter(|&v| v > 1.0 + 1e-9),
            "blurryReflections" => a.blurry = value.starts_with("on"),
            "sw_shader" => a.shader = value.trim_matches('"').to_lowercase(),
            "metallic_color" | "metallic_ior" => a.metallic = true,
            "bumpTexture" => bump_texture = texture_path(value),
            "bumpIsNormalMap" => bump_is_normal_map = value.starts_with("on"),
            "bumpStrength" => a.bump_strength = number().unwrap_or(0.0),
            "initTextureWidth" => tile[0] = number(),
            "initTextureHeight" => tile[1] = number(),
            "col1" => {
                let v: Vec<f32> = value.split_whitespace().filter_map(|t| t.parse().ok()).collect();
                if let [r, g, b] = v[..] {
                    a.colour = Some([r, g, b]);
                }
            }
            _ => {}
        }
    }

    // A normal map is only a normal map when the file says it is. Where both
    // keys name a file they usually name the same one (113 of 135); where they
    // differ, `bumpTexture` is the normal map under `shaders/surfacefinish`
    // and `bump_file_texture` is a grey height map beside the colour image.
    if bump_is_normal_map {
        a.normal_texture = bump_texture.clone();
    }
    if a.normal_texture.is_none() {
        a.height_texture = bump_file_texture.or(bump_texture);
    } else {
        a.height_texture = bump_file_texture.filter(|f| Some(f) != a.normal_texture.as_ref());
    }
    if let [Some(w), Some(h)] = tile {
        if w > 0.0 && h > 0.0 {
            a.tile_metres = Some([w, h]);
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
    fn the_library_names_the_textures_it_was_measured_to_name() {
        let lib = AppearanceLibrary::bundled();
        let count = |f: fn(&Appearance) -> bool| lib.appearances.values().filter(|a| f(a)).count();

        // Surveyed over the bundled tree before any of this was written. They
        // are here so that a change to the parsing has to explain itself
        // against the corpus rather than against one file.
        assert_eq!(count(|a| a.colour_texture.is_some()), 229);
        // 139, not the 156 that carry `bumpIsNormalMap on`. The other 17 set
        // the flag and leave `bumpTexture` empty, so it describes a field that
        // is not there; what they do name is a `*bump.jpg` beside the colour
        // image. Two of those were measured to be pure greyscale — chroma 0.0
        // — which is a height map. A normal map is blue: around 128/128/255.
        assert_eq!(count(|a| a.normal_texture.is_some()), 139);
        assert_eq!(count(|a| a.tile_metres.is_some()), 483);

        // Every path a file names resolves to somewhere inside the library's
        // own image tree, with one exception that names a root this install
        // does not have.
        let outside = lib
            .appearances
            .values()
            .flat_map(|a| [&a.colour_texture, &a.normal_texture, &a.height_texture])
            .flatten()
            .filter(|p| !p.starts_with("images/"))
            .count();
        assert_eq!(outside, 1, "only `SystemTexture/...` sits elsewhere");
    }

    #[test]
    fn powder_coat_brings_its_own_grain() {
        // The finish most of a painted assembly is delivered in, and the one
        // that makes this worth doing: a colour image and a real tangent-space
        // normal map, tiled every 6.35 mm.
        let lib = AppearanceLibrary::bundled();
        let p = lib.get("painted/powder coat/dark powdercoat").expect("powder coat");

        assert_eq!(
            p.colour_texture.as_deref(),
            Some("images/textures/painted/powdercoat_dark.jpg")
        );
        assert_eq!(
            p.normal_texture.as_deref(),
            Some("images/shaders/surfacefinish/powdercoat_n.dds")
        );
        // The same file is also listed as bump_file_texture. It is one map, not
        // two, and it must not be counted twice.
        assert_eq!(p.height_texture, None);

        let [w, h] = p.tile_metres.expect("a tile size");
        assert!((w - 0.00635).abs() < 1e-9 && (h - 0.00635).abs() < 1e-9);
    }

    #[test]
    fn a_path_the_file_writes_is_made_into_one_that_can_be_looked_up() {
        // Mixed separators inside a single path, a parent step in the middle,
        // and inconsistent case — all three occur in the bundled tree.
        assert_eq!(
            texture_path(r"Images\shaders\../textures/organic/wood/oak/UNFINISHED oak.jpg"),
            Some("images/textures/organic/wood/oak/unfinished oak.jpg".into())
        );
        assert_eq!(texture_path(""), None);
        assert_eq!(texture_path("\"\""), None);
    }

    #[test]
    fn a_height_map_is_not_promoted_to_a_normal_map() {
        // 22 files name a normal map and a height map that are different
        // images. The normal map is the one under shaders/surfacefinish.
        let lib = AppearanceLibrary::bundled();
        let carpet = lib.get("fabric/carpet/carpet color1 2d").expect("carpet");
        assert_eq!(
            carpet.normal_texture.as_deref(),
            Some("images/shaders/surfacefinish/carpet1_n.dds")
        );
        assert_eq!(
            carpet.height_texture.as_deref(),
            Some("images/textures/fabric/carpet/carpet color1 bump.jpg")
        );

        // And a file that offers only a height map keeps it as one.
        let held_as_height = lib
            .appearances
            .values()
            .filter(|a| a.normal_texture.is_none() && a.height_texture.is_some())
            .count();
        assert!(held_as_height > 0);
    }

    #[test]
    fn every_finish_that_can_be_chosen_carries_its_images() {
        // build.rs embeds the textures of a fixed list of finishes, and
        // `representative` decides which finishes can be reached. Two lists,
        // and nothing but this test to stop them drifting apart: a finish
        // added to one and not the other converts silently without its
        // textures, which is the kind of quiet loss this project keeps finding.
        if BUNDLED_TEXTURES.is_empty() {
            // No image tree on this machine — a fresh clone, or CI. build.rs
            // has already said so. There is nothing to be inconsistent with.
            return;
        }
        let lib = AppearanceLibrary::bundled();
        let mut checked = 0;
        for &class in MaterialClass::ALL {
            for matte in [true, false] {
                let Some(path) = representative(class, matte) else {
                    continue;
                };
                let a = lib.get(path).expect("a representative that exists");
                for texture in [&a.colour_texture, &a.normal_texture].into_iter().flatten() {
                    assert!(
                        bundled_texture(texture).is_some(),
                        "{path} names {texture}, which build.rs did not embed"
                    );
                    checked += 1;
                }
            }
        }
        assert!(checked >= 2, "the powder coat alone names two images");
    }

    #[test]
    fn an_index_of_refraction_of_one_is_not_an_index_of_refraction() {
        let lib = AppearanceLibrary::bundled();

        // Glass states its own and keeps it.
        assert_eq!(lib.get("glass/gloss/clear glass").unwrap().ior, Some(1.52));
        let reflective = lib.get("glass/gloss/reflective clear glass").unwrap();
        assert!((reflective.ior.unwrap() - 1.591).abs() < 1e-9);

        // Powder coat says 1, which is vacuum, which it is not. Nothing is
        // carried and the writers use glTF's own 1.5.
        let powder = lib.get("painted/powder coat/dark powdercoat").unwrap();
        assert_eq!(powder.ior, None);
        assert!((powder.to_material("x").ior - 1.5).abs() < 1e-6);

        let stated = lib.appearances.values().filter(|a| a.ior.is_some()).count();
        assert_eq!(stated, 47, "the rest of the library says 1");
    }

    #[test]
    fn only_the_true_metals_carry_a_metallic_colour() {
        let lib = AppearanceLibrary::bundled();
        let metals = lib.appearances.values().filter(|a| a.metallic).count();
        assert_eq!(metals, 41, "the bundled tree has 41 such files");
        assert!(!lib.get("painted/powder coat/dark powdercoat").unwrap().metallic);
    }
}
