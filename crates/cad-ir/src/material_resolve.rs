//! Deciding what a surface is made of, from the evidence available.
//!
//! The exchange formats this pipeline reads carry no engineering material —
//! measured on the samples: neither the `.x_t` files nor the 32 MB STEP
//! assembly names one anywhere. What survives is a colour per face, a part
//! number per product, and whatever the user can tell us. So material
//! resolution is a fallback chain, each rung optional:
//!
//! 1. **A material name** — from a sidecar table entry (and later from SLDPRT
//!    metadata), classified by [`MaterialClass::from_name`].
//! 2. **The user's table** — part-number patterns and colour rules in a plain
//!    text file, see [`MaterialTable`].
//! 3. **Colour inference** — [`MaterialClass::infer_from_srgb`]. In a machine
//!    assembly a neutral grey face *is* metal to any human reader, and shading
//!    it as matte plastic is the wrong picture; saturated colours are paint.
//! 4. **A neutral default.**
//!
//! Inference is a heuristic and is therefore both overridable per colour in
//! the table and disableable wholesale.

use crate::material::{Material, MaterialClass};

impl MaterialClass {
    /// Classify an engineering material name.
    ///
    /// Handles English and Turkish trade names plus the alloy designations
    /// that actually appear in CAD part data (AISI grades, EN aluminium
    /// numbers, DIN steel names, polymer abbreviations). Substring matching is
    /// restricted to tokens long enough not to fire by accident; short codes
    /// like `PA6` or `POM` must stand as their own word.
    pub fn from_name(raw: &str) -> Option<MaterialClass> {
        let s = raw.to_lowercase();
        let has = |needle: &str| s.contains(needle);
        let word = |needle: &str| {
            s.split(|c: char| !c.is_ascii_alphanumeric())
                .any(|w| w == needle)
        };

        // Order matters: the specific beats the generic ("stainless" before
        // "steel", "anodised" before "aluminium").
        if has("stainless") || has("paslanmaz") || has("inox")
            || word("304") || word("316") || word("321") || has("x5crni")
        {
            return Some(MaterialClass::StainlessSteel);
        }
        if has("cast iron") || has("dökme demir") || has("en-gjl") || has("en-gjs")
            || word("gg25") || word("ggg40")
        {
            return Some(MaterialClass::CastIron);
        }
        if has("anodis") || has("anodiz") || has("eloksal") {
            return Some(MaterialClass::AnodisedAluminium);
        }
        if has("alumin") || has("alümin") || word("6061") || word("6082")
            || word("5754") || word("7075") || word("1060") || word("5083")
        {
            return Some(MaterialClass::Aluminium);
        }
        if has("steel") || has("çelik") || has("celik") || has("aisi")
            || word("c45") || word("s235") || word("s355") || word("st37")
            || word("st52") || has("42crmo") || has("16mncr")
        {
            return Some(MaterialClass::Steel);
        }
        if has("copper") || has("bakır") || has("bakir") || has("cu-etp") {
            return Some(MaterialClass::Copper);
        }
        if has("brass") || has("pirinç") || has("pirinc") || has("cuzn") || word("ms58") {
            return Some(MaterialClass::Brass);
        }
        if has("bronze") || has("bronz") || has("cusn") {
            return Some(MaterialClass::Bronze);
        }
        if has("titan") {
            return Some(MaterialClass::Titanium);
        }
        if has("chrome") || has("chromium") || has("krom") {
            return Some(MaterialClass::Chrome);
        }
        if has("zinc") || has("çinko") || has("cinko") || has("galvaniz") || has("galvanis") {
            return Some(MaterialClass::Zinc);
        }
        if has("gold") || has("altın") || has("altin") {
            return Some(MaterialClass::Gold);
        }
        if has("silver") || has("gümüş") || has("gumus") {
            return Some(MaterialClass::Silver);
        }
        if has("rubber") || has("kauçuk") || has("kaucuk") || has("lastik")
            || word("epdm") || word("nbr") || word("fkm") || has("viton")
            || has("silicone") || has("silikon")
        {
            return Some(MaterialClass::Rubber);
        }
        if has("glass") || word("cam") {
            return Some(MaterialClass::Glass);
        }
        if has("wood") || has("ahşap") || has("ahsap") {
            return Some(MaterialClass::Wood);
        }
        if has("ceramic") || has("seramik") {
            return Some(MaterialClass::Ceramic);
        }
        if has("concrete") || has("beton") {
            return Some(MaterialClass::Concrete);
        }
        if has("paint") || has("boya") {
            return Some(MaterialClass::Paint);
        }
        if has("fabric") || has("kumaş") || has("kumas") || has("felt") || has("keçe") {
            return Some(MaterialClass::Fabric);
        }
        if has("foam") || has("köpük") || has("kopuk") || has("sünger") || has("sunger") {
            return Some(MaterialClass::Foam);
        }
        if has("carbon") || has("karbon") || word("cfrp") || has("composite") || has("kompozit") {
            return Some(MaterialClass::Composite);
        }
        if has("plastic") || has("plastik") || word("abs") || word("pom")
            || has("delrin") || has("nylon") || has("naylon") || word("pa6") || word("pa66")
            || word("pp") || word("pe") || word("pvc") || word("ptfe") || has("teflon")
            || word("pc") || has("polycarb") || has("polikarbon") || word("pmma")
            || has("acrylic") || has("akrilik") || word("peek") || word("hdpe") || word("pom-c")
        {
            return Some(MaterialClass::Plastic);
        }
        None
    }

    /// Infer a material family from a display-sRGB colour alone.
    ///
    /// The reasoning a human applies to a CAD viewport, made explicit: in a
    /// mechanical assembly a neutral grey surface is machined metal — bright
    /// grey reads aluminium, mid grey steel, dark grey cast or oxidised metal,
    /// near-black is rubber — and anything with real saturation is a painted
    /// or polymer surface, because bare metal is never blue or green. Tiers
    /// verified against the pilot assembly's fourteen colours.
    pub fn infer_from_srgb(srgb: [f32; 3]) -> MaterialClass {
        let max = srgb[0].max(srgb[1]).max(srgb[2]);
        let min = srgb[0].min(srgb[1]).min(srgb[2]);
        let value = max;
        let saturation = if max > 0.0 { (max - min) / max } else { 0.0 };

        if saturation >= 0.15 {
            return MaterialClass::Paint;
        }
        if value >= 0.70 {
            MaterialClass::Aluminium
        } else if value >= 0.28 {
            MaterialClass::Steel
        } else if value >= 0.13 {
            MaterialClass::CastIron
        } else {
            MaterialClass::Rubber
        }
    }
}

/// The colour evidence for one styled surface.
#[derive(Debug, Clone, Copy)]
pub struct ColourEvidence {
    /// Display-referred sRGB in 0..1, as the file stores it.
    pub srgb: [f32; 3],
    /// The same colour converted to linear, ready for PBR.
    pub linear: [f32; 3],
    pub alpha: f32,
}

/// One rule from a user-supplied material table.
#[derive(Debug, Clone, PartialEq)]
enum Rule {
    /// `part <pattern> = <material>` — glob over the part name.
    Part { pattern: String, material: String },
    /// `color RRGGBB = <material>` — exact colour match.
    Colour { hex: String, material: String },
    /// `default = <material>` — for surfaces with no other evidence.
    Default { material: String },
}

/// A user-supplied mapping from part numbers and colours to material names.
///
/// Plain text, hand-editable, no dependencies:
///
/// ```text
/// # ERP export, 2026-08
/// part 219 203 *   = AISI 304
/// part 1102.*      = EPDM
/// color 0033BB     = boya
/// default          = steel
/// ```
///
/// Patterns support `*` anywhere. Material names go through
/// [`MaterialClass::from_name`], so both `AISI 304` and `paslanmaz çelik`
/// work. First matching rule wins, so order the specific above the general.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct MaterialTable {
    rules: Vec<Rule>,
}

impl MaterialTable {
    /// Parse a table. Unparseable lines are returned as errors alongside the
    /// table rather than aborting it: a typo in line 40 should not silently
    /// discard the 39 rules above it, and should not be silently skipped
    /// either.
    pub fn parse(text: &str) -> (MaterialTable, Vec<String>) {
        let mut rules = Vec::new();
        let mut errors = Vec::new();
        for (i, line) in text.lines().enumerate() {
            let line = line.trim();
            if line.is_empty() || line.starts_with('#') {
                continue;
            }
            let Some((lhs, rhs)) = line.split_once('=') else {
                errors.push(format!("line {}: no `=` in {line:?}", i + 1));
                continue;
            };
            let (lhs, material) = (lhs.trim(), rhs.trim().to_string());
            if material.is_empty() {
                errors.push(format!("line {}: empty material name", i + 1));
                continue;
            }
            if lhs == "default" {
                rules.push(Rule::Default { material });
            } else if let Some(hex) = lhs.strip_prefix("color ").or(lhs.strip_prefix("colour ")) {
                let hex = hex.trim().trim_start_matches('#').to_uppercase();
                if hex.len() == 6 && hex.chars().all(|c| c.is_ascii_hexdigit()) {
                    rules.push(Rule::Colour { hex, material });
                } else {
                    errors.push(format!("line {}: {hex:?} is not RRGGBB", i + 1));
                }
            } else if let Some(pattern) = lhs.strip_prefix("part ") {
                rules.push(Rule::Part {
                    pattern: pattern.trim().to_string(),
                    material,
                });
            } else {
                errors.push(format!(
                    "line {}: expected `part …`, `color …` or `default`, got {lhs:?}",
                    i + 1
                ));
            }
        }
        (MaterialTable { rules }, errors)
    }

    pub fn is_empty(&self) -> bool {
        self.rules.is_empty()
    }

    fn part_match(&self, part: &str) -> Option<&str> {
        self.rules.iter().find_map(|r| match r {
            Rule::Part { pattern, material } if glob_match(pattern, part) => {
                Some(material.as_str())
            }
            _ => None,
        })
    }

    fn colour_match(&self, hex: &str) -> Option<&str> {
        self.rules.iter().find_map(|r| match r {
            Rule::Colour { hex: h, material } if h == hex => Some(material.as_str()),
            _ => None,
        })
    }

    fn default(&self) -> Option<&str> {
        self.rules.iter().find_map(|r| match r {
            Rule::Default { material } => Some(material.as_str()),
            _ => None,
        })
    }
}

/// `*`-wildcard match, case-insensitive.
fn glob_match(pattern: &str, text: &str) -> bool {
    let (p, t) = (pattern.to_lowercase(), text.to_lowercase());
    let parts: Vec<&str> = p.split('*').collect();
    if parts.len() == 1 {
        return p == t;
    }
    let mut pos = 0usize;
    for (i, piece) in parts.iter().enumerate() {
        if piece.is_empty() {
            continue;
        }
        match t[pos..].find(piece) {
            Some(off) => {
                // The first piece is anchored at the start, the last at the end.
                if i == 0 && off != 0 {
                    return false;
                }
                pos += off + piece.len();
            }
            None => return false,
        }
    }
    if !parts.last().unwrap_or(&"").is_empty() && !t.ends_with(parts.last().unwrap()) {
        return false;
    }
    true
}

/// Resolves a part-plus-colour into a [`Material`], applying the whole chain.
#[derive(Debug, Clone, Default)]
pub struct MaterialResolver {
    pub table: MaterialTable,
    /// Turn a colour with no other evidence into a material family. On by
    /// default; off, colours become the neutral dielectric they were before.
    pub no_inference: bool,
}

impl MaterialResolver {
    /// Resolve one surface.
    pub fn resolve(&self, part: &str, colour: Option<ColourEvidence>) -> Material {
        let hex = colour.map(|c| {
            format!(
                "{:02X}{:02X}{:02X}",
                (c.srgb[0].clamp(0.0, 1.0) * 255.0).round() as u8,
                (c.srgb[1].clamp(0.0, 1.0) * 255.0).round() as u8,
                (c.srgb[2].clamp(0.0, 1.0) * 255.0).round() as u8,
            )
        });

        // Rung 1+2: the user's table, part rule first, then colour rule.
        let named = self
            .table
            .part_match(part)
            .or_else(|| hex.as_deref().and_then(|h| self.table.colour_match(h)));
        if let Some(name) = named {
            return self.build_named(name, colour, hex.as_deref());
        }

        // Rung 3: infer from the colour.
        if let Some(c) = colour {
            if self.no_inference {
                let mut m =
                    Material::from_colour(format!("colour-{}", hex.as_deref().unwrap_or("?")), c.linear, c.alpha);
                m.name = format!("colour-{}", hex.as_deref().unwrap_or("?"));
                return m;
            }
            let class = MaterialClass::infer_from_srgb(c.srgb);
            return tinted(
                class,
                format!("{}-{}", class_slug(class), hex.as_deref().unwrap_or("?")),
                Some(c),
            );
        }

        // Rung 4: no evidence at all.
        match self.table.default() {
            Some(name) => self.build_named(name, None, None),
            None => Material::unknown(),
        }
    }

    fn build_named(&self, name: &str, colour: Option<ColourEvidence>, hex: Option<&str>) -> Material {
        match MaterialClass::from_name(name) {
            Some(class) => tinted(class, name.to_string(), colour),
            None => {
                // The user named something we cannot classify. Keep their name
                // — it is the most specific fact we have — and take the shading
                // from the colour tier so the surface still looks like *some*
                // family rather than defaulting to grey plastic.
                let class = colour
                    .map(|c| MaterialClass::infer_from_srgb(c.srgb))
                    .unwrap_or(MaterialClass::Plastic);
                let mut m = tinted(class, name.to_string(), colour);
                let _ = hex;
                m.name = name.to_string();
                m
            }
        }
    }
}

/// A class preset carrying the file's own colour.
///
/// Dielectrics take the colour as-is — paint is exactly its pigment. Metals
/// blend it half-and-half with the class's measured reflectance: a metal's
/// base colour *is* its reflectance, and CAD viewport greys sit well below the
/// range real metals reflect in, so using them raw produces sooty, dull metal.
/// The blend keeps the designer's light/dark intent while restoring enough
/// reflectance to read as metal under an environment map.
fn tinted(class: MaterialClass, name: String, colour: Option<ColourEvidence>) -> Material {
    let preset = Material::from_class(class, name.clone());
    match colour {
        None => preset,
        Some(c) => {
            let base = if class.is_metal() {
                [
                    0.5 * preset.base_color[0] + 0.5 * c.linear[0],
                    0.5 * preset.base_color[1] + 0.5 * c.linear[1],
                    0.5 * preset.base_color[2] + 0.5 * c.linear[2],
                ]
            } else {
                c.linear
            };
            Material {
                base_color: base,
                alpha: c.alpha,
                ..preset
            }
        }
    }
}

fn class_slug(class: MaterialClass) -> &'static str {
    use MaterialClass::*;
    match class {
        Steel => "steel",
        StainlessSteel => "stainless",
        CastIron => "cast-iron",
        Aluminium => "aluminium",
        AnodisedAluminium => "anodised-al",
        Copper => "copper",
        Brass => "brass",
        Bronze => "bronze",
        Titanium => "titanium",
        Chrome => "chrome",
        Zinc => "zinc",
        Gold => "gold",
        Silver => "silver",
        Plastic => "plastic",
        Rubber => "rubber",
        Glass => "glass",
        Wood => "wood",
        Ceramic => "ceramic",
        Concrete => "concrete",
        Paint => "paint",
        Fabric => "fabric",
        Foam => "foam",
        Composite => "composite",
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::material::MaterialSource;

    fn ev(hex: &str) -> ColourEvidence {
        let v = |i: usize| u8::from_str_radix(&hex[i..i + 2], 16).unwrap() as f32 / 255.0;
        let srgb = [v(0), v(2), v(4)];
        let lin = |x: f32| {
            if x <= 0.04045 { x / 12.92 } else { ((x + 0.055) / 1.055).powf(2.4) }
        };
        ColourEvidence {
            srgb,
            linear: [lin(srgb[0]), lin(srgb[1]), lin(srgb[2])],
            alpha: 1.0,
        }
    }

    #[test]
    fn names_classify_across_languages_and_designations() {
        use MaterialClass::*;
        for (name, want) in [
            ("AISI 304", StainlessSteel),
            ("Paslanmaz Çelik", StainlessSteel),
            ("AISI 1018 Steel", Steel),
            ("Çelik S235JR", Steel),
            ("Alüminyum 6061-T6", Aluminium),
            ("Eloksallı Alüminyum", AnodisedAluminium),
            ("EPDM 70 Shore", Rubber),
            ("Kauçuk", Rubber),
            ("ABS", Plastic),
            ("PA6-GF30 Naylon", Plastic),
            ("POM", Plastic),
            ("Pirinç MS58", Brass),
            ("Galvanizli Sac", Zinc),
            ("Dökme Demir GG25", CastIron),
            ("Cam", Glass),
            ("Toz Boya RAL 5010", Paint),
        ] {
            assert_eq!(MaterialClass::from_name(name), Some(want), "{name}");
        }
        assert_eq!(MaterialClass::from_name("Unobtainium X99"), None);
    }

    #[test]
    fn short_codes_only_match_as_whole_words() {
        // "pc" must not fire inside longer words.
        assert_eq!(MaterialClass::from_name("PCB assembly"), None);
        assert_eq!(MaterialClass::from_name("PC"), Some(MaterialClass::Plastic));
        // "cam" (TR glass) must not fire inside "camshaft".
        assert_eq!(MaterialClass::from_name("camshaft"), None);
        assert_eq!(MaterialClass::from_name("cam"), Some(MaterialClass::Glass));
    }

    /// Every one of the pilot assembly's fourteen colours lands in the tier a
    /// person looking at the rendered assembly would put it in.
    #[test]
    fn the_pilot_palette_infers_sensibly() {
        use MaterialClass::*;
        for (hex, want) in [
            ("808080", Steel),
            ("555555", Steel),
            ("555759", Steel),
            ("81888C", Steel),
            ("777788", Steel),
            ("778888", Steel),
            ("D1D1D1", Aluminium),
            ("333333", CastIron),
            ("1F1F1F", Rubber),
            ("000000", Rubber),
            ("0033BB", Paint),
            ("CC0000", Paint),
            ("30BF94", Paint),
            ("22BB88", Paint),
        ] {
            assert_eq!(
                MaterialClass::infer_from_srgb(ev(hex).srgb),
                want,
                "#{hex}"
            );
        }
    }

    #[test]
    fn inference_produces_metallic_greys_and_painted_colours() {
        let r = MaterialResolver::default();
        let steel = r.resolve("214 201 007", Some(ev("808080")));
        assert_eq!(steel.metallic, 1.0);
        assert!(steel.name.starts_with("steel-"));
        // The blend lifts the viewport grey toward real metal reflectance.
        assert!(steel.base_color[0] > ev("808080").linear[0]);

        let paint = r.resolve("214 201 007", Some(ev("0033BB")));
        assert_eq!(paint.metallic, 0.0);
        // Paint keeps the designer's colour exactly.
        assert!((paint.base_color[2] - ev("0033BB").linear[2]).abs() < 1e-6);
    }

    #[test]
    fn disabling_inference_restores_plain_colours() {
        let r = MaterialResolver {
            no_inference: true,
            ..Default::default()
        };
        let m = r.resolve("x", Some(ev("808080")));
        assert_eq!(m.metallic, 0.0);
        assert_eq!(m.source, MaterialSource::Colour);
    }

    #[test]
    fn table_rules_beat_inference_and_specific_beats_general() {
        let (table, errors) = MaterialTable::parse(
            "# test\n\
             part 219 203 * = AISI 304\n\
             part 1102.*    = EPDM\n\
             color 0033BB   = boya\n\
             default        = çelik\n",
        );
        assert!(errors.is_empty(), "{errors:?}");
        let r = MaterialResolver { table, no_inference: false };

        // Part rule wins over what the colour would have inferred.
        let m = r.resolve("219 203 008", Some(ev("808080")));
        assert_eq!(m.name, "AISI 304");
        assert_eq!(m.metallic, 1.0);

        let m = r.resolve("1102.A110", Some(ev("D1D1D1")));
        assert_eq!(m.name, "EPDM");
        assert_eq!(m.metallic, 0.0);

        // Colour rule catches parts no part rule matched.
        let m = r.resolve("999", Some(ev("0033BB")));
        assert_eq!(m.name, "boya");

        // Default covers the evidence-free case.
        let m = r.resolve("999", None);
        assert_eq!(m.name, "çelik");
        assert_eq!(m.metallic, 1.0);
    }

    #[test]
    fn an_unclassifiable_table_name_keeps_the_name_and_shades_by_colour() {
        let (table, _) = MaterialTable::parse("part * = Unobtainium\n");
        let r = MaterialResolver { table, no_inference: false };
        let m = r.resolve("anything", Some(ev("808080")));
        assert_eq!(m.name, "Unobtainium");
        // Grey tier → shaded as metal even though the name meant nothing.
        assert_eq!(m.metallic, 1.0);
    }

    #[test]
    fn bad_table_lines_are_reported_not_swallowed() {
        let (table, errors) = MaterialTable::parse(
            "part A = steel\n\
             nonsense line\n\
             color XYZ = paint\n\
             part B = \n",
        );
        assert_eq!(errors.len(), 3);
        assert!(!table.is_empty(), "the good rule must survive");
    }

    #[test]
    fn glob_matching_is_anchored_and_case_insensitive() {
        assert!(glob_match("219 203 *", "219 203 008"));
        assert!(!glob_match("219 203 *", "x 219 203 008"));
        assert!(glob_match("*-51", "204 201 013-51"));
        assert!(!glob_match("*-51", "204 201 013-51x"));
        assert!(glob_match("1102.*", "1102.A110"));
        assert!(glob_match("aisi*", "AISI 304"));
        assert!(glob_match("exact", "EXACT"));
        assert!(!glob_match("exact", "exactly"));
    }

    #[test]
    fn transparency_survives_resolution() {
        let mut c = ev("30BF94");
        c.alpha = 0.4;
        let m = MaterialResolver::default().resolve("x", Some(c));
        assert!((m.alpha - 0.4).abs() < 1e-6);
        assert!(m.is_transparent());
    }
}
