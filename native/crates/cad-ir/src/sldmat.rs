//! Reading a SolidWorks material library and turning it into PBR materials.
//!
//! A `.sldmat` file is the designer's own statement of what a part is made of:
//! a swatch colour, a set of optical coefficients, the name of the shader
//! SolidWorks would render it with, and the physical properties. Everything a
//! viewer needs to make steel look like steel is in there, and reading it is
//! the difference between a model coloured by guesswork and one coloured the
//! way it was designed.
//!
//! The mapping to glTF's metallic-roughness model is documented on each step,
//! marked with where it comes from: the format's own documentation, the glTF
//! specification, or — where neither says — a decision recorded as such.

use crate::material::{Material, MaterialClass};
use std::collections::HashMap;

/// One material as the library states it.
#[derive(Debug, Clone)]
pub struct SldMaterial {
    pub name: String,
    /// The classification it was filed under, e.g. `Steel`.
    pub classification: String,
    /// Swatch colour, as the 8-bit display values the file writes.
    pub swatch: [u8; 3],
    pub optical: Optical,
    /// Shader names the file asks for, lower-cased and concatenated: the
    /// legacy `pwshader`/`cgshader` pair, the 2008+ `pwshader2`/`cgshader2`,
    /// and the `swtexture` path, which names what the library thinks the
    /// surface is made of.
    pub shaders: String,
    /// The same, without the texture path.
    ///
    /// The texture is a shared image — every plastic in the library, rubber
    /// included, points at `plastic\polished\pplastic2.jpg` — so its path
    /// says what a surface is made of but nothing about how it was finished.
    /// A judgement about finish has to be made on the shader names alone.
    pub shader_names: String,
    /// Density in kg/m³, when the file gives one.
    pub density: Option<f64>,
}

/// The six optical coefficients, each documented as running 0 to 1.
#[derive(Debug, Clone, Copy, Default)]
pub struct Optical {
    pub ambient: f64,
    pub transparency: f64,
    pub diffuse: f64,
    pub specularity: f64,
    pub shininess: f64,
    pub emission: f64,
}

/// The largest `Shininess` any of these libraries uses.
///
/// Taken literally, roughness is `1 − Shininess`: SolidWorks defines
/// `Shininess = 1 − specular spread` and calls that spread roughness, and glTF
/// roughness is the same perceptual quantity, so no exponent conversion is
/// involved. But no material in any surveyed library sets Shininess above 0.4,
/// so the literal reading squeezes every material into the top two fifths of
/// the range and polished steel comes out looking sandblasted. Dividing
/// through by the range actually used restores the contrast the library was
/// drawn with.
const SHININESS_RANGE: f64 = 0.4;

/// Roughness never reaches zero: a perfect mirror has no highlight at all
/// under a punctual light, which reads as a black surface rather than a shiny
/// one.
const MIN_ROUGHNESS: f64 = 0.05;

/// The roughest the library's own polished entries are, and so the roughest a
/// material whose shader name says "polished" is allowed to come out.
const POLISHED_CEILING: f32 = 0.25;

impl SldMaterial {
    /// The material as a renderer wants it.
    /// How rough the surface is, from `Shininess` — and from the shader name
    /// where the two disagree.
    ///
    /// `Shininess` is not maintained across this library. Its own `Copper`
    /// and `Brass` ask for the `CopperPolished` and `BrassPolished` shaders
    /// and set Shininess to 0.025, which taken literally is a surface as rough
    /// as unfinished concrete; `Nickel`, which stands in for chrome plating
    /// here, asks for `NickelPolished` and comes out at 0.75. Six entries are
    /// like this. Where a shader name states a polish, that is a statement
    /// about how the material *looks*, which is the thing roughness encodes,
    /// and it is taken as a ceiling — not as the value, so an entry whose
    /// optics already agree keeps them. The ceiling is the library's own: the
    /// polished entries it does fill in sensibly, `6061 Alloy` and
    /// `AISI 1020`, sit at 0.225 and 0.05, so 0.25 is the top of the range it
    /// uses for a polished finish rather than a number invented here.
    ///
    /// The other direction is left alone. Half the steels name a polished
    /// shader while pointing at a cast texture, and there is no reading of
    /// that which is obviously right.
    pub fn roughness(&self) -> f32 {
        let stated =
            (1.0 - self.optical.shininess / SHININESS_RANGE).clamp(MIN_ROUGHNESS, 1.0) as f32;
        const POLISHED: [&str; 5] = ["polish", "chrome", "verchrom", "mirror", "plate"];
        if POLISHED.iter().any(|t| self.shader_names.contains(t)) {
            stated.min(POLISHED_CEILING)
        } else {
            stated
        }
    }

    pub fn to_material(&self) -> Material {
        let metallic = if self.is_metal() { 1.0 } else { 0.0 };
        let linear = srgb_to_linear(self.swatch);
        Material {
            name: self.name.clone(),
            // For a metal glTF's base colour *is* the Fresnel reflectance at
            // normal incidence, not an albedo, so a measured value for the
            // metal beats the swatch — which is a UI colour chosen to be
            // recognisable in a list. Where no measurement is on file the
            // swatch is still the best statement available.
            base_color: if metallic > 0.0 {
                measured_f0(&self.shaders)
                    .or_else(|| measured_f0(&self.name))
                    .unwrap_or(linear)
            } else {
                linear
            },
            alpha: (1.0 - self.optical.transparency).clamp(0.0, 1.0) as f32,
            metallic,
            roughness: self.roughness(),
            ior: 1.5,
            transmission: 0.0,
            emissive: [0.0; 3],
            double_sided: false,
            // The library named the material, so say so: the appearance came
            // from a stated engineering material, not from a colour guess.
            source: crate::material::MaterialSource::Named {
                raw: self.name.clone(),
                preset: crate::material::MaterialClass::from_name(&self.name)
                    .unwrap_or(fallback_class(metallic > 0.0)),
            },
        }
    }

    /// Whether this is a metal, which in this model is a yes or a no.
    ///
    /// Classification comes first because it is always present and states what
    /// the material *is*; the shader name states how it *looks* and the two
    /// can honestly disagree — the library's own AISI 1020, a plain carbon
    /// steel, asks to be drawn with the stainless shader. The dielectric
    /// tokens are checked before either, because a carbon-fibre part shades as
    /// dark resin and reads as foil if called a metal.
    pub fn is_metal(&self) -> bool {
        if DIELECTRIC_TOKENS
            .iter()
            .any(|t| self.shaders.contains(t) || lower(&self.name).contains(t))
        {
            return false;
        }
        if METAL_CLASSES
            .iter()
            .any(|c| lower(&self.classification).contains(c))
        {
            return true;
        }
        METAL_TOKENS
            .iter()
            .any(|t| self.shaders.contains(t) || lower(&self.name).contains(t))
    }
}

/// Classifications whose members are metals. Matched as substrings so a
/// library that writes "Aluminum Alloys" and one that writes "Aluminium" both
/// land.
const METAL_CLASSES: [&str; 7] = [
    "steel", "iron", "alumin", "copper", "other alloys", "other metals", "titanium",
];

/// Shader-name and material-name tokens that settle the question on their own.
/// Checked before the classification, so a composite is never a metal.
const DIELECTRIC_TOKENS: [&str; 18] = [
    "plastic",
    "rubber",
    "glass",
    "fibre",
    "fiber",
    "wood",
    "ceramic",
    "porcelain",
    "stone",
    "paint",
    "powdercoat",
    "leather",
    "cloth",
    "acrylic",
    "nylon",
    "ptfe",
    "pvc",
    "polycarbonate",
];

const METAL_TOKENS: [&str; 22] = [
    "steel",
    "stainless",
    "iron",
    "alumin",
    "copper",
    "brass",
    "bronze",
    "nickel",
    "chrom",
    "titan",
    "tungsten",
    "molybdenum",
    "vanadium",
    "zirconium",
    "magnesium",
    "zinc",
    "silver",
    "gold",
    "platinum",
    "cobalt",
    "tin",
    "lead",
];

/// Measured normal-incidence reflectance for the metals that have one on
/// record, in linear RGB.
///
/// These are the values the real-time rendering literature settled on and
/// which Unreal and the glTF specification's own examples use. Where two
/// sources disagree the row says so.
fn measured_f0(token_source: &str) -> Option<[f32; 3]> {
    let s = lower(token_source);
    // Longest tokens first: "stainless" must beat "steel", which it contains
    // neither way round, but "gold" must not be found inside "goldenrod".
    const TABLE: [(&str, [f32; 3]); 11] = [
        ("stainless", [0.669, 0.639, 0.598]),
        ("alumin", [0.913, 0.921, 0.925]),
        ("copper", [0.955, 0.637, 0.538]),
        ("nickel", [0.660, 0.609, 0.526]),
        ("platinum", [0.672, 0.637, 0.585]),
        ("cobalt", [0.662, 0.655, 0.634]),
        ("silver", [0.972, 0.960, 0.915]),
        ("gold", [1.000, 0.766, 0.336]),
        // Two published sets disagree by a fifth on these three; these are the
        // Lagarde/Unreal figures, chosen for consistency with the rest.
        ("chrom", [0.550, 0.556, 0.554]),
        ("titan", [0.542, 0.497, 0.449]),
        ("brass", [0.910, 0.778, 0.423]),
    ];
    if let Some((_, f0)) = TABLE.iter().find(|(t, _)| s.contains(t)) {
        return Some(*f0);
    }
    // Plain carbon steel and iron share a value.
    (s.contains("steel") || s.contains("iron")).then_some([0.560, 0.570, 0.580])
}

/// One sRGB display channel to the linear value glTF stores.
pub fn channel_to_linear(s: f32) -> f32 {
    if s <= 0.04045 {
        s / 12.92
    } else {
        ((s + 0.055) / 1.055).powf(2.4)
    }
}

/// An sRGB display colour to the linear one glTF stores.
pub fn srgb_to_linear_rgb(srgb: [f32; 3]) -> [f32; 3] {
    [
        channel_to_linear(srgb[0]),
        channel_to_linear(srgb[1]),
        channel_to_linear(srgb[2]),
    ]
}

/// sRGB display bytes to the linear values glTF stores.
fn srgb_to_linear(c: [u8; 3]) -> [f32; 3] {
    srgb_to_linear_rgb([
        c[0] as f32 / 255.0,
        c[1] as f32 / 255.0,
        c[2] as f32 / 255.0,
    ])
}

fn lower(s: &str) -> String {
    s.to_ascii_lowercase()
}

/// Every material in a library, keyed by lower-cased name.
#[derive(Debug, Clone, Default)]
pub struct SldLibrary {
    pub materials: HashMap<String, SldMaterial>,
}

/// The library entry that stands for a material family.
///
/// A part rarely names its material — neither Parasolid nor STEP carries one
/// here — so the family is inferred from the colour, and the shading then came
/// from a preset written into this crate. The library has the real thing: the
/// same families, with SolidWorks' own optical coefficients and the shader it
/// draws them with. Naming one entry per family lets an inferred material be
/// shaded by the library rather than by a guess, and naming it — rather than
/// averaging the family — keeps the choice auditable: this steel is `AISI
/// 1020`, and you can go and look at it.
///
/// `None` for the families the library does not carry — paint, concrete,
/// fabric, foam are finishes and fillers, not engineering materials, and
/// SolidWorks does not list them.
pub fn representative(class: MaterialClass) -> Option<&'static str> {
    use MaterialClass::*;
    Some(match class {
        Steel => "AISI 1020",
        StainlessSteel => "AISI 304",
        CastIron => "Gray Cast Iron",
        Aluminium | AnodisedAluminium => "6061 Alloy",
        Copper => "Copper",
        Brass => "Brass",
        Bronze => "Tin Bearing Bronze",
        Titanium => "Titanium",
        // The library carries no chromium; nickel is the bright plated metal
        // it does carry, and chrome plating is what it stands in for.
        Chrome => "Nickel",
        Gold => "Pure Gold",
        Silver => "Pure Silver",
        Plastic => "ABS",
        Rubber => "Rubber",
        Glass => "Glass",
        Wood => "Oak",
        Ceramic => "Ceramic Porcelain",
        Composite => "Zoltek Panex 33",
        Zinc | Paint | Concrete | Fabric | Foam => return None,
    })
}

/// The SolidWorks default material library, carried with the crate.
///
/// Shipping it rather than reading it off a SolidWorks installation means a
/// part that names `AISI 1020` renders as that steel on a machine that has
/// never had SolidWorks on it, which is the whole point of a converter.
///
/// Its provenance, and the check that it is byte-for-byte the library it
/// claims to be, are in `assets/PROVENANCE.md`.
const BUNDLED: &[u8] = include_bytes!("../assets/solidworks-materials.sldmat");

impl SldLibrary {
    /// The library that ships with this crate: 115 materials in 12
    /// classifications, covering the steels, aluminium alloys, coppers,
    /// plastics, rubbers and woods a mechanical assembly is made of.
    pub fn bundled() -> SldLibrary {
        SldLibrary::parse(BUNDLED)
    }

    /// Read a library from raw file bytes.
    ///
    /// The encoding is taken from the byte-order mark and not from the XML
    /// declaration: most of these files are UTF-16 little-endian with a mark,
    /// but some are UTF-8 bytes still declaring UTF-16, and some declare
    /// nothing at all. Believing the declaration fails on roughly one file in
    /// seven.
    pub fn parse(bytes: &[u8]) -> SldLibrary {
        let text = decode(bytes);
        let mut materials = HashMap::new();
        let mut classification = String::new();

        for (i, _) in text.match_indices('<') {
            let rest = &text[i..];
            // The tag has to be the one starting here; searching ahead would
            // pick up whatever classification comes next in the file and file
            // every material under it.
            if rest.starts_with("<classification ")
                && let Some(name) = attribute(rest, "classification", "name")
            {
                classification = name;
            }
            if !rest.starts_with("<material ") {
                continue;
            }
            let Some(name) = attribute(rest, "material", "name") else {
                continue;
            };
            // The material's own span ends at its closing tag, or at the next
            // material if the file is malformed.
            let end = rest.find("</material>").unwrap_or(rest.len());
            let block = &rest[..end];
            let swatch = attribute(block, "swatchcolor", "RGB")
                .and_then(|h| parse_hex(&h))
                .unwrap_or([200, 200, 200]);
            let mut shaders = String::new();
            let mut shader_names = String::new();
            // `swtexture` names an image inside the SolidWorks installation —
            // `images\\textures\\metal\\cast\\cast_fine.jpg` — and the path itself
            // states what the library thinks the surface is made of and how it
            // was finished. The image cannot be shipped, but the statement is
            // free and it is evidence of exactly the kind `is_metal` weighs.
            for tag in ["pwshader", "cgshader", "pwshader2", "cgshader2", "swtexture"] {
                for key in ["name", "path"] {
                    if let Some(v) = attribute(block, tag, key) {
                        shaders.push_str(&lower(&v));
                        shaders.push(' ');
                        if tag != "swtexture" {
                            shader_names.push_str(&lower(&v));
                            shader_names.push(' ');
                        }
                    }
                }
            }
            let key = lower(&name);
            materials.insert(
                key,
                SldMaterial {
                    name,
                    classification: classification.clone(),
                    swatch,
                    optical: optical(block),
                    shaders,
                    shader_names,
                    density: attribute(block, "DENS", "value").and_then(|v| v.parse().ok()),
                },
            );
        }
        SldLibrary { materials }
    }

    pub fn get(&self, name: &str) -> Option<&SldMaterial> {
        self.materials.get(&lower(name))
    }

    pub fn is_empty(&self) -> bool {
        self.materials.is_empty()
    }
}

/// Bytes to text, deciding the encoding from the byte-order mark alone.
fn decode(bytes: &[u8]) -> String {
    match bytes {
        [0xFF, 0xFE, rest @ ..] => utf16(rest, u16::from_le_bytes),
        [0xFE, 0xFF, rest @ ..] => utf16(rest, u16::from_be_bytes),
        [0xEF, 0xBB, 0xBF, rest @ ..] => String::from_utf8_lossy(rest).into_owned(),
        _ => String::from_utf8_lossy(bytes).into_owned(),
    }
}

fn utf16(bytes: &[u8], order: fn([u8; 2]) -> u16) -> String {
    let units: Vec<u16> = bytes
        .chunks_exact(2)
        .map(|c| order([c[0], c[1]]))
        .collect();
    String::from_utf16_lossy(&units)
}

/// The six optical coefficients, read by name.
///
/// The file writes them in one order and the SolidWorks API reports them in
/// another, so position is not a safe way to read them. Both quote styles
/// appear, often in the same file.
fn optical(block: &str) -> Optical {
    let read = |key: &str| {
        attribute_anywhere(block, key)
            .and_then(|v| v.parse::<f64>().ok())
            .unwrap_or(0.0)
            .clamp(0.0, 1.0)
    };
    Optical {
        ambient: read("Ambient"),
        transparency: read("Transparency"),
        diffuse: read("Diffuse"),
        specularity: read("Specularity"),
        shininess: read("Shininess"),
        emission: read("Emission"),
    }
}

/// The value of `key` on the first `<tag ...>` in `text`.
fn attribute(text: &str, tag: &str, key: &str) -> Option<String> {
    let open = text.find(&format!("<{tag} "))?;
    let rest = &text[open..];
    let close = rest.find('>')?;
    attribute_anywhere(&rest[..close], key)
}

/// The value of `key=` anywhere in `text`, in either quote style.
fn attribute_anywhere(text: &str, key: &str) -> Option<String> {
    let at = text.find(&format!("{key}="))?;
    let rest = &text[at + key.len() + 1..];
    let quote = rest.chars().next()?;
    if quote != '"' && quote != '\'' {
        return None;
    }
    let body = &rest[quote.len_utf8()..];
    let end = body.find(quote)?;
    Some(body[..end].to_string())
}

fn parse_hex(h: &str) -> Option<[u8; 3]> {
    let h = h.trim();
    (h.len() == 6).then(|| {
        [
            u8::from_str_radix(&h[0..2], 16).ok(),
            u8::from_str_radix(&h[2..4], 16).ok(),
            u8::from_str_radix(&h[4..6], 16).ok(),
        ]
    })?;
    Some([
        u8::from_str_radix(&h[0..2], 16).ok()?,
        u8::from_str_radix(&h[2..4], 16).ok()?,
        u8::from_str_radix(&h[4..6], 16).ok()?,
    ])
}

/// The family to record when the name is not one the presets recognise.
fn fallback_class(metal: bool) -> crate::material::MaterialClass {
    use crate::material::MaterialClass;
    if metal { MaterialClass::Steel } else { MaterialClass::Plastic }
}

#[cfg(test)]
mod tests {

    #[test]
    fn a_polished_shader_caps_a_roughness_its_optics_contradict() {
        let lib = SldLibrary::bundled();
        // The library asks for `CopperPolished` and then sets Shininess to
        // 0.025, which read literally is concrete. Six entries do this, and
        // `Nickel` is one — the material chrome plating is drawn with here.
        for name in ["Copper", "Brass", "Nickel", "Pure Silver", "Plain Carbon Steel"] {
            let m = lib.get(name).expect(name);
            assert!(
                m.shader_names.contains("polish") || m.shader_names.contains("plate"),
                "{name} was chosen for this test because its shader says polished"
            );
            assert!(
                m.roughness() <= POLISHED_CEILING,
                "{name} came out at {}",
                m.roughness()
            );
        }
    }

    #[test]
    fn optics_that_agree_with_the_shader_are_left_alone() {
        let lib = SldLibrary::bundled();
        // Both name a polished shader and both already say so in their optics.
        assert!((lib.get("6061 Alloy").unwrap().roughness() - 0.225).abs() < 1e-3);
        assert!((lib.get("AISI 1020").unwrap().roughness() - 0.05).abs() < 1e-3);
        // And nothing that does not claim a polish moves.
        assert!((lib.get("Rubber").unwrap().roughness() - 1.0).abs() < 1e-6);
        assert!((lib.get("Gray Cast Iron").unwrap().roughness() - 0.225).abs() < 1e-3);
    }

    #[test]
    fn the_texture_path_is_kept_out_of_the_finish_judgement() {
        let lib = SldLibrary::bundled();
        // Every plastic in the library, rubber included, points at
        // `plastic\polished\pplastic2.jpg`. If that counted, rubber would be
        // polished.
        let rubber = lib.get("Rubber").unwrap();
        assert!(rubber.shaders.contains("polished"), "the texture path says polished");
        assert!(!rubber.shader_names.contains("polish"), "its shaders do not");
        assert_eq!(rubber.roughness(), 1.0);
    }
    use super::*;

    const SAMPLE: &str = r#"<?xml version="1.0"?>
<mstns:materials>
 <classification name="Steel">
  <material name="AISI 1020" matid="2">
   <shaders>
    <pwshader name="stainless steel"/>
    <cgshader name="SteelAISI1020"/>
   </shaders>
   <swatchcolor RGB="C6C6D1">
    <sldcolorswatch:Optical Ambient='0.520000' Transparency='0.000000' Diffuse='0.800000' Specularity='1.000000' Shininess='0.400000' Emission='0.000000'/>
   </swatchcolor>
   <physicalproperties>
    <DENS displayname="Density" value="0.79E+04"/>
   </physicalproperties>
  </material>
 </classification>
 <classification name="Plastics">
  <material name="Nylon 6/10">
   <shaders><cgshader name="PlasticNylon"/></shaders>
   <swatchcolor RGB="E5E5CC">
    <sldcolorswatch:Optical Ambient="1.0" Transparency="0.250000" Diffuse="1.0" Specularity="0.3" Shininess="0.100000" Emission="0.0"/>
   </swatchcolor>
  </material>
 </classification>
</mstns:materials>"#;

    /// The library that ships with the crate has to actually parse, and its
    /// metals have to come out as metals.
    #[test]
    fn the_bundled_library_reads() {
        let lib = SldLibrary::bundled();
        assert_eq!(lib.materials.len(), 115);
        let steel = lib.get("AISI 1020").expect("the default steel is present");
        assert_eq!(steel.classification, "Steel");
        assert!(steel.is_metal());
        let nylon = lib.get("Nylon 6/10").expect("a plastic is present");
        assert!(!nylon.is_metal());
        let metals = lib.materials.values().filter(|m| m.is_metal()).count();
        assert_eq!(metals, 77, "the library's six metal classifications hold 77 materials");
    }

    #[test]
    fn reads_a_library_and_keeps_each_material_under_its_classification() {
        let lib = SldLibrary::parse(SAMPLE.as_bytes());
        assert_eq!(lib.materials.len(), 2);
        let steel = lib.get("aisi 1020").expect("the steel is there");
        assert_eq!(steel.classification, "Steel");
        assert_eq!(steel.swatch, [0xC6, 0xC6, 0xD1]);
        assert_eq!(steel.density, Some(0.79e4));
        let nylon = lib.get("Nylon 6/10").expect("the nylon is there");
        assert_eq!(nylon.classification, "Plastics");
    }

    /// The file writes these in one order and the API reports them in another,
    /// and both quote styles occur, so neither position nor quoting can be
    /// assumed.
    #[test]
    fn optical_values_are_read_by_name_in_either_quote_style() {
        let lib = SldLibrary::parse(SAMPLE.as_bytes());
        let steel = lib.get("aisi 1020").unwrap();
        assert_eq!(steel.optical.ambient, 0.52);
        assert_eq!(steel.optical.shininess, 0.4);
        assert_eq!(steel.optical.transparency, 0.0);
        let nylon = lib.get("nylon 6/10").unwrap();
        assert_eq!(nylon.optical.transparency, 0.25);
        assert_eq!(nylon.optical.shininess, 0.1);
    }

    /// A UTF-16 library with a byte-order mark is the common case, and the
    /// declaration inside it is not to be trusted.
    #[test]
    fn utf16_with_a_byte_order_mark_reads_the_same_as_utf8() {
        let mut bytes = vec![0xFF, 0xFE];
        for unit in SAMPLE.encode_utf16() {
            bytes.extend_from_slice(&unit.to_le_bytes());
        }
        let lib = SldLibrary::parse(&bytes);
        assert_eq!(lib.materials.len(), 2);
        assert_eq!(lib.get("aisi 1020").unwrap().swatch, [0xC6, 0xC6, 0xD1]);
    }

    #[test]
    fn a_steel_is_a_metal_and_a_nylon_is_not() {
        let lib = SldLibrary::parse(SAMPLE.as_bytes());
        assert!(lib.get("aisi 1020").unwrap().is_metal());
        assert!(!lib.get("nylon 6/10").unwrap().is_metal());
    }

    /// The whole point of the shininess rescale: taken literally every
    /// material in the library lands between 0.6 and 1.0 and nothing looks
    /// polished.
    #[test]
    fn the_shiniest_material_in_the_library_comes_out_polished() {
        let lib = SldLibrary::parse(SAMPLE.as_bytes());
        let steel = lib.get("aisi 1020").unwrap().to_material();
        assert!(
            steel.roughness <= 0.06,
            "shininess 0.4 is the library's maximum and should read as polished, got {}",
            steel.roughness
        );
        let nylon = lib.get("nylon 6/10").unwrap().to_material();
        assert!(
            (nylon.roughness - 0.75).abs() < 1e-6,
            "shininess 0.1 should land three quarters of the way to matte, got {}",
            nylon.roughness
        );
    }

    /// A metal's base colour is its Fresnel reflectance, and a measured one
    /// beats the swatch the library picked to be recognisable in a list.
    #[test]
    fn a_metal_takes_its_measured_reflectance_and_a_plastic_its_swatch() {
        let lib = SldLibrary::parse(SAMPLE.as_bytes());
        let steel = lib.get("aisi 1020").unwrap().to_material();
        // The shader asks for stainless even though the steel is plain carbon,
        // and the shader is what states the appearance.
        assert!((steel.base_color[0] - 0.669).abs() < 1e-3, "{:?}", steel.base_color);
        assert_eq!(steel.metallic, 1.0);

        let nylon = lib.get("nylon 6/10").unwrap().to_material();
        assert_eq!(nylon.metallic, 0.0);
        // E5 = 229 → sRGB 0.898 → linear 0.784
        assert!((nylon.base_color[0] - 0.784).abs() < 2e-3, "{:?}", nylon.base_color);
        assert!((nylon.alpha - 0.75).abs() < 1e-6);
    }
}
