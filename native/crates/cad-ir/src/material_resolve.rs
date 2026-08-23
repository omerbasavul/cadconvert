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

use crate::material::{Material, MaterialClass, MaterialSource};
use crate::scene::Scene;
use std::collections::HashMap;

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

/// What a rule says a surface is: a material, and optionally the colour to
/// paint it.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct Named {
    pub material: String,
    /// `#RRGGBB` written after the material name.
    ///
    /// The one thing in the chain that outranks the file. Everything else here
    /// changes how a surface is *finished* and leaves its colour alone, because
    /// the colour is the file's own and was checked body by body against
    /// another kernel. This is for when the product is not delivered in the
    /// colour the model was drawn in — a casting modelled grey and shipped
    /// black — which no amount of reading the file will reveal.
    pub colour: Option<[u8; 3]>,
}

/// One rule from a user-supplied material table.
#[derive(Debug, Clone, PartialEq)]
enum Rule {
    /// `part <pattern> = <material>` — glob over the part name.
    Part { pattern: String, named: Named },
    /// `color RRGGBB = <material>` — exact colour match.
    Colour { hex: String, named: Named },
    /// `default = <material>` — for surfaces with no other evidence.
    Default { named: Named },
}

/// Split a trailing `#RRGGBB` off a material name.
///
/// Only with the hash, and only at the end: a material really can be called
/// `AISI 304` or `1.4301`, and eating six characters off the end of one
/// because they happen to be hexadecimal would be a fine way to lose a name.
fn split_colour(rhs: &str) -> (String, Option<[u8; 3]>, Option<String>) {
    let Some((name, hex)) = rhs.rsplit_once('#') else {
        return (rhs.trim().to_string(), None, None);
    };
    let hex = hex.trim();
    if hex.len() != 6 || !hex.chars().all(|c| c.is_ascii_hexdigit()) {
        return (rhs.trim().to_string(), None, Some(format!("{hex:?} is not RRGGBB")));
    }
    let byte = |i: usize| u8::from_str_radix(&hex[i..i + 2], 16).unwrap_or(0);
    let name = name.trim();
    let name = if name.is_empty() { hex.to_uppercase() } else { name.to_string() };
    (name, Some([byte(0), byte(2), byte(4)]), None)
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
            let lhs = lhs.trim();
            let (material, colour, bad_colour) = split_colour(rhs);
            if let Some(why) = bad_colour {
                errors.push(format!("line {}: {why}", i + 1));
                continue;
            }
            if material.is_empty() {
                errors.push(format!("line {}: empty material name", i + 1));
                continue;
            }
            let named = Named { material, colour };
            if lhs == "default" {
                rules.push(Rule::Default { named });
            } else if let Some(hex) = lhs.strip_prefix("color ").or(lhs.strip_prefix("colour ")) {
                let hex = hex.trim().trim_start_matches('#').to_uppercase();
                if hex.len() == 6 && hex.chars().all(|c| c.is_ascii_hexdigit()) {
                    rules.push(Rule::Colour { hex, named });
                } else {
                    errors.push(format!("line {}: {hex:?} is not RRGGBB", i + 1));
                }
            } else if let Some(pattern) = lhs.strip_prefix("part ") {
                rules.push(Rule::Part {
                    pattern: pattern.trim().to_string(),
                    named,
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

    fn part_match(&self, part: &str) -> Option<&Named> {
        self.rules.iter().find_map(|r| match r {
            Rule::Part { pattern, named } if glob_match(pattern, part) => Some(named),
            _ => None,
        })
    }

    fn colour_match(&self, hex: &str) -> Option<&Named> {
        self.rules.iter().find_map(|r| match r {
            Rule::Colour { hex: h, named } if h == hex => Some(named),
            _ => None,
        })
    }

    fn default(&self) -> Option<&Named> {
        self.rules.iter().find_map(|r| match r {
            Rule::Default { named } => Some(named),
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
#[derive(Debug, Clone)]
pub struct MaterialResolver {
    pub table: MaterialTable,
    /// Turn a colour with no other evidence into a material family. On by
    /// default; off, colours become the neutral dielectric they were before.
    pub no_inference: bool,
    /// Designer-assigned reflectivity per colour (uppercase `RRGGBB`), 0..1.
    ///
    /// Recovered from a Parasolid twin's `SDL/TYSA_REFLECTIVITY` attributes,
    /// which the STEP export drops. Where present this outranks colour
    /// inference: the guess "mid grey is probably machined steel" is replaced
    /// by the authoring tool's own statement of which faces are metal and
    /// which are matte — in the pilot the two machined-metal colours carry 1.0
    /// on every face and the painted castings 0.0, exactly the split a person
    /// reads off the product photo.
    pub reflectivity_by_colour: HashMap<String, f32>,
    /// The SolidWorks material library, when one has been loaded.
    ///
    /// This outranks every preset below it. A `.sldmat` entry is the designer's
    /// own statement of the material — its swatch, its optical coefficients and
    /// the shader SolidWorks would draw it with — where everything else in this
    /// chain is the converter guessing from a name or a colour.
    pub library: crate::sldmat::SldLibrary,
}

impl Default for MaterialResolver {
    fn default() -> MaterialResolver {
        MaterialResolver {
            table: <MaterialTable as Default>::default(),
            no_inference: false,
            reflectivity_by_colour: HashMap::default(),
            library: crate::sldmat::SldLibrary::bundled(),
        }
    }
}

impl MaterialResolver {
    /// Resolve one surface whose reflectivity is known per face.
    ///
    /// The Parasolid path attaches `SDL/TYSA_REFLECTIVITY` to the face itself,
    /// so no colour-keyed join is needed; the STEP path, which has no such
    /// attribute, goes through [`MaterialResolver::resolve`] and the per-colour
    /// map instead.
    pub fn resolve_with_reflectivity(
        &self,
        part: &str,
        colour: Option<ColourEvidence>,
        reflectivity: Option<f32>,
    ) -> Material {
        if let (Some(c), Some(refl)) = (colour, reflectivity)
            && !self.no_inference
        {
            let hex = hex_of(c.srgb);
            // An explicit table rule still wins over the designer flag.
            let named = self
                .table
                .part_match(part)
                .or_else(|| self.table.colour_match(&hex));
            if let Some(name) = named {
                return self.build_named(name, colour, Some(&hex));
            }
            return material_from_reflectivity(c, refl, &hex, &self.library);
        }
        self.resolve(part, colour)
    }

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
        if let Some(named) = named {
            return self.build_named(named, colour, hex.as_deref());
        }

        // Rung 2½: designer-assigned reflectivity, when a Parasolid twin
        // supplied it. Metal or matte is then a stated fact, not a guess; only
        // WHICH metal or WHICH matte family remains inferred from the colour.
        if let (Some(c), Some(hex)) = (colour, hex.as_deref())
            && !self.no_inference
            && let Some(&refl) = self.reflectivity_by_colour.get(hex)
        {
            return material_from_reflectivity(c, refl, hex, &self.library);
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
                &self.library,
                Finish::Unstated,
            );
        }

        // Rung 4: no evidence at all.
        match self.table.default() {
            Some(name) => self.build_named(name, None, None),
            None => Material::unknown(),
        }
    }

    fn build_named(&self, named: &Named, colour: Option<ColourEvidence>, _hex: Option<&str>) -> Material {
        let name = named.material.as_str();
        // A colour written in the table replaces the file's, and it replaces
        // it everywhere below — including on a metal, where the file's own
        // colour is normally set aside in favour of a measured reflectance.
        // Someone who writes `#1A1A1A` has said what the surface is; there is
        // nothing left to infer.
        let colour = match named.colour {
            Some(rgb) => {
                let srgb = [
                    rgb[0] as f32 / 255.0,
                    rgb[1] as f32 / 255.0,
                    rgb[2] as f32 / 255.0,
                ];
                Some(ColourEvidence {
                    srgb,
                    linear: crate::sldmat::srgb_to_linear_rgb(srgb),
                    // Opacity stays whatever the file said: a table that names
                    // a colour has said nothing about transparency.
                    alpha: colour.map_or(1.0, |c| c.alpha),
                })
            }
            None => colour,
        };
        // The library states this material; nothing below can improve on it.
        // The part's own colour still applies to a dielectric, because a
        // painted casting is the library's plastic in the product's colour,
        // but a metal keeps the reflectance measured for it.
        let mut built = self.build_named_inner(name, colour);
        // Applied here rather than in each branch below, so that whatever the
        // material turned out to be — a library entry, a classified name, or
        // a name nothing recognises — a colour the table stated is the colour
        // that ships.
        if let Some(rgb) = named.colour {
            built.base_color = crate::sldmat::srgb_to_linear_rgb([
                rgb[0] as f32 / 255.0,
                rgb[1] as f32 / 255.0,
                rgb[2] as f32 / 255.0,
            ]);
        }
        built
    }

    fn build_named_inner(&self, name: &str, colour: Option<ColourEvidence>) -> Material {
        if let Some(entry) = self.library.get(name) {
            let mut m = entry.to_material();
            if let Some(c) = colour
                && m.metallic == 0.0
            {
                m.base_color = crate::sldmat::srgb_to_linear_rgb(c.srgb);
            }
            return m;
        }
        match MaterialClass::from_name(name) {
            Some(class) => tinted(class, name.to_string(), colour, &self.library, Finish::from_name(name)),
            None => {
                // The user named something we cannot classify. Keep their name
                // — it is the most specific fact we have — and take the shading
                // from the colour tier so the surface still looks like *some*
                // family rather than defaulting to grey plastic.
                let class = colour
                    .map(|c| MaterialClass::infer_from_srgb(c.srgb))
                    .unwrap_or(MaterialClass::Plastic);
                let mut m = tinted(class, name.to_string(), colour, &self.library, Finish::from_name(name));
                m.name = name.to_string();
                m
            }
        }
    }
}

/// Uppercase `RRGGBB` of a display-sRGB colour.
fn hex_of(srgb: [f32; 3]) -> String {
    format!(
        "{:02X}{:02X}{:02X}",
        (srgb[0].clamp(0.0, 1.0) * 255.0).round() as u8,
        (srgb[1].clamp(0.0, 1.0) * 255.0).round() as u8,
        (srgb[2].clamp(0.0, 1.0) * 255.0).round() as u8,
    )
}

/// The designer said how shiny this face is; only the family within
/// metal-or-matte remains inferred from the colour.
fn material_from_reflectivity(
    c: ColourEvidence,
    refl: f32,
    hex: &str,
    library: &crate::sldmat::SldLibrary,
) -> Material {
    let value = c.srgb[0].max(c.srgb[1]).max(c.srgb[2]);
    let class = if refl >= 0.5 {
        if value >= 0.70 {
            MaterialClass::Aluminium
        } else if value >= 0.28 {
            MaterialClass::Steel
        } else {
            MaterialClass::CastIron
        }
    } else if value < 0.13 {
        // Matte near-black reads as rubber in a machine assembly.
        MaterialClass::Rubber
    } else {
        MaterialClass::Paint
    };
    // The designer's own reflectivity is a statement about the finish, which
    // is exactly what chooses between the appearance library's powder coat and
    // its gloss.
    let finish = if refl < 0.5 { Finish::Matte } else { Finish::Reflective };
    tinted(
        class,
        format!("{}-{hex}", class_slug(class)),
        Some(c),
        library,
        finish,
    )
}

/// What the evidence says about how a surface was finished, as opposed to what
/// it is made of.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Finish {
    /// The designer marked this colour matte.
    Matte,
    /// The designer marked it reflective.
    Reflective,
    /// Nobody said.
    Unstated,
}

impl Finish {
    /// The finish a material's own name states, where it states one.
    ///
    /// A name is the most specific thing a user can tell us, and a trade name
    /// usually carries the finish inside it: "toz boya" *is* powder coat,
    /// "matt lack" is matt lacquer. Reading it is what lets a table of colour
    /// rules say everything a Parasolid twin's per-face reflectivity says —
    /// on the pilot, that a mid-grey is matte paint and not machined steel,
    /// and that the paint is powder coat at 0.92 rather than car gloss at
    /// 0.65. Without it the twin has to be parsed in full, every time, for
    /// fourteen flags.
    fn from_name(raw: &str) -> Finish {
        let s = raw.to_lowercase();
        let has = |t: &str| s.contains(t);
        if has("toz boya") || has("powder") || has("matte") || has("matt ") || has("mat ")
            || has("satin") || has("saten") || has("eloksal") || has("anodis") || has("anodiz")
        {
            return Finish::Matte;
        }
        if has("parlak") || has("gloss") || has("polish") || has("polished") || has("krom")
            || has("chrome") || has("mirror") || has("ayna")
        {
            return Finish::Reflective;
        }
        Finish::Unstated
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
fn tinted(
    class: MaterialClass,
    name: String,
    colour: Option<ColourEvidence>,
    library: &crate::sldmat::SldLibrary,
    finish: Finish,
) -> Material {
    // The family was inferred, but how a family reflects light is not a thing
    // to invent: the library states it, for these same families, in the
    // designer's own numbers. Take the optics from there and keep the preset
    // only for the finishes the library does not carry.
    let preset = crate::sldmat::representative(class)
        .and_then(|entry| library.get(entry))
        .map(|entry| Material {
            name: name.clone(),
            ..entry.to_material()
        })
        // The material library has no entry for some finishes — paint above
        // all, which is not a material in SolidWorks but an appearance, and is
        // most of a painted assembly's surface. The appearance library beside
        // it does: powder coat states no reflection and a roughness of 0.92,
        // which is what a delivered machine casting is. Before this, paint's
        // gloss was the one number in the pipeline with nothing behind it.
        .or_else(|| {
            let appearances = crate::p2m::AppearanceLibrary::bundled();
            let path = crate::p2m::representative(class, finish == Finish::Matte)?;
            let a = appearances.get(path)?;
            Some(Material {
                ..a.to_material(name.clone())
            })
        })
        .unwrap_or_else(|| Material::from_class(class, name.clone()));
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

/// Give every material that names an appearance the images that appearance
/// names, loading each one once into the scene.
///
/// Run after the materials are settled and before the writers see them. It is
/// separate from resolving because resolving produces a [`Material`] and this
/// needs a [`Scene`] to put images in; threading one through the other only to
/// reach here would have every caller carrying a scene it does not use.
///
/// Returns whatever could not be loaded, by name. A finish whose image is
/// missing or unreadable converts without it — the colour and the roughness
/// are still the appearance's own — so these are warnings, not failures.
pub fn attach_appearance_textures(scene: &mut Scene) -> Vec<String> {
    let library = crate::p2m::AppearanceLibrary::bundled();
    let mut warnings = Vec::new();

    for index in 0..scene.materials.len() {
        let MaterialSource::Named { raw, .. } = &scene.materials[index].source else {
            continue;
        };
        let Some(appearance) = library.get(raw) else {
            continue;
        };
        // `raw` is an appearance path only when the appearance library
        // answered; a .sldmat name reaches `get` and finds nothing, which is
        // the check rather than an extra flag on the material.
        let (colour, normal, tile, strength) = (
            appearance.colour_texture.clone(),
            appearance.normal_texture.clone(),
            appearance.tile_metres,
            appearance.bump_strength,
        );
        let name = raw.clone();

        let mut textures = crate::material::Textures::default();
        let mut colour_path = None;
        for (path, slot) in [(colour, true), (normal, false)] {
            let Some(path) = path else { continue };
            let Some(bytes) = crate::p2m::bundled_texture(&path) else {
                // Not embedded. Expected for most of the library; only the
                // finishes the resolver can reach carry their images.
                continue;
            };
            match crate::image::load(crate::p2m::bundled_texture_name(&path), bytes) {
                Ok(image) => {
                    let id = scene.add_image(image);
                    if slot {
                        textures.base_colour = Some(id);
                        colour_path = Some(path.clone());
                    } else {
                        textures.normal = Some(id);
                    }
                }
                Err(e) => warnings.push(format!("{name}: {path}: {e}")),
            }
        }

        // `initTextureWidth` is metres and the models are millimetres.
        textures.set_tile_mm(tile.map(|[w, h]| [(w * 1000.0) as f32, (h * 1000.0) as f32]));
        // `bumpStrength` is a relief depth in metres, not glTF's normal scale,
        // and the library states 0.001 for almost everything. Treated as
        // full strength unless the file says less: a normal map that has been
        // authored is meant to be seen.
        textures.set_normal_scale(if strength > 0.0 { 1.0 } else { 0.0 });

        // A colour image carries the appearance's own colour baked into it —
        // powdercoat_dark.jpg has a linear mean of 0.1830 and `dark
        // powdercoat` states col1 0.1843, the same number — so multiplying a
        // part's colour by it applies that level twice.
        //
        // Dividing the *colour* by that level was the first answer and it was
        // wrong, because glTF's base colour factor stops at one. The pilot's
        // dominant paint needed 1.12 and its blue needed 2.44; both clamped,
        // and 45% of the model came out white. The render hid it, since the
        // image multiplied the level straight back — but the material said
        // white, and any reader that ignores textures showed white.
        //
        // So the level comes out of the *image*, once and offline, by
        // tools/make_grain.py. What is shipped is the grain alone, and what is
        // left here is putting back the little it lost to clipping: 0.905 for
        // the powder coat, a tenth of a stop. Every colour the pilot carries
        // is well under that.
        if let Some(path) = colour_path.as_deref() {
            let level = crate::p2m::bundled_texture_level(path);
            if level > 1e-4 {
                let base = &mut scene.materials[index].base_color;
                for c in base.iter_mut() {
                    *c = (*c / level).min(1.0);
                }
            }
        }

        scene.materials[index].textures = textures;
    }
    warnings
}

#[cfg(test)]
mod tests {

    /// A part that names no material still has to be shaded by the library,
    /// not by a preset written into this crate. Neither Parasolid nor STEP
    /// carries a material name on the pilot assembly, so this is the path
    /// every surface in it actually takes.
    #[test]
    fn an_inferred_family_is_shaded_by_the_library() {
        let resolver = MaterialResolver::default();
        let library = crate::sldmat::SldLibrary::bundled();

        // Bright neutral grey reads as aluminium.
        let grey = ColourEvidence {
            srgb: [0.82, 0.82, 0.82],
            linear: [0.64, 0.64, 0.64],
            alpha: 1.0,
        };
        let m = resolver.resolve("no name in the file", Some(grey));
        let entry = library
            .get(crate::sldmat::representative(MaterialClass::Aluminium).unwrap())
            .expect("the library carries the aluminium it names");
        let want = entry.to_material();
        assert!(m.metallic > 0.5, "aluminium came out dielectric");
        assert!(
            (m.roughness - want.roughness).abs() < 1e-6,
            "roughness {} is not the library's {}",
            m.roughness,
            want.roughness
        );
        // And it is not the crate's own preset, which is what this replaced.
        let preset = Material::from_class(MaterialClass::Aluminium, String::new());
        assert!(
            (want.roughness - preset.roughness).abs() > 1e-6,
            "the fixture cannot tell the two apart"
        );
    }

    /// A family the library does not carry keeps the preset — paint is a
    /// finish, not an engineering material, and SolidWorks does not list it.
    #[test]
    fn a_family_the_library_lacks_keeps_its_preset() {
        assert!(crate::sldmat::representative(MaterialClass::Paint).is_none());
        let resolver = MaterialResolver::default();
        let red = ColourEvidence {
            srgb: [0.8, 0.0, 0.0],
            linear: [0.6, 0.0, 0.0],
            alpha: 1.0,
        };
        let m = resolver.resolve("part", Some(red));
        assert_eq!(m.metallic, 0.0);
        assert!(m.base_color[0] > m.base_color[1], "the part's own colour was lost");
    }
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
        let r = MaterialResolver { table, ..Default::default() };

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
        let r = MaterialResolver { table, ..Default::default() };
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
    fn designer_reflectivity_outranks_colour_inference() {
        let mut r = MaterialResolver::default();
        // The pilot's measured split: bright and dark metal, matte mid-grey.
        r.reflectivity_by_colour.insert("D1D1D1".into(), 1.0);
        r.reflectivity_by_colour.insert("555759".into(), 1.0);
        r.reflectivity_by_colour.insert("808080".into(), 0.0);
        r.reflectivity_by_colour.insert("000000".into(), 0.0);

        let m = r.resolve("x", Some(ev("D1D1D1")));
        assert_eq!(m.metallic, 1.0);
        assert!(m.name.starts_with("aluminium-"));

        let m = r.resolve("x", Some(ev("555759")));
        assert_eq!(m.metallic, 1.0, "dark but reflective is dark metal");

        // Mid grey WOULD infer steel; the designer said matte, so it is paint.
        let m = r.resolve("x", Some(ev("808080")));
        assert_eq!(m.metallic, 0.0);
        assert!(m.name.starts_with("paint-"));
        // And matte means the appearance library's powder coat, which states
        // no reflection at all and a roughness of 0.92 — not a number chosen
        // here. See `crate::p2m`.
        assert!((m.roughness - 0.92).abs() < 1e-6, "matte, not semi-gloss");

        let m = r.resolve("x", Some(ev("000000")));
        assert_eq!(m.name, "rubber-000000");

        // A colour the twin never saw still goes through inference.
        let m = r.resolve("x", Some(ev("555555")));
        assert_eq!(m.metallic, 1.0, "unlisted grey still infers steel");
    }

    #[test]
    fn a_table_rule_still_beats_reflectivity() {
        let (table, _) = MaterialTable::parse("color 808080 = AISI 304\n");
        let mut r = MaterialResolver { table, ..Default::default() };
        r.reflectivity_by_colour.insert("808080".into(), 0.0);
        let m = r.resolve("x", Some(ev("808080")));
        assert_eq!(m.name, "AISI 304", "the user's explicit word wins");
        assert_eq!(m.metallic, 1.0);
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

#[cfg(test)]
mod colour_override_tests {
    use super::*;

    fn table(text: &str) -> MaterialResolver {
        let (table, errors) = MaterialTable::parse(text);
        assert!(errors.is_empty(), "{errors:?}");
        MaterialResolver { table, ..MaterialResolver::default() }
    }

    fn grey() -> ColourEvidence {
        let srgb = [0.5, 0.5, 0.5];
        ColourEvidence { srgb, linear: crate::sldmat::srgb_to_linear_rgb(srgb), alpha: 1.0 }
    }

    fn blue() -> ColourEvidence {
        let srgb = [0.0, 0.2, 0.733]; // #0033BB, a painted casting
        ColourEvidence { srgb, linear: crate::sldmat::srgb_to_linear_rgb(srgb), alpha: 1.0 }
    }

    #[test]
    fn a_dielectrics_colour_is_the_files_and_stays_that_way() {
        // Nothing in the chain may touch it. The colour of a painted surface
        // is what the file says, and on this project's pilot every one of them
        // was checked body by body against another kernel.
        for resolver in [
            MaterialResolver::default(),
            table("part 200 201 003* = Toz Boya"),
            table("color 0033BB = Toz Boya"),
        ] {
            let m = resolver.resolve("200 201 003-51", Some(blue()));
            assert_eq!(m.metallic, 0.0);
            assert_eq!(m.base_color, blue().linear, "a paint changed colour");
        }
    }

    #[test]
    fn a_metals_base_colour_is_a_reflectance_and_not_the_files_colour() {
        // The one place a colour does change, and it is a translation rather
        // than an invention: in glTF a metal's base colour is its Fresnel
        // reflectance at normal incidence, not an albedo. Feeding it the
        // file's swatch — a colour chosen to be recognisable in a list —
        // gives a metal that reflects the wrong amount of light. Steel
        // measures near 0.56 and the pilot's swatch for it is #555759.
        //
        // Which is why the pilot ships steel as #A6A3A0 and aluminium as
        // #E4E4E5 rather than #555759 and #D1D1D1. Anyone auditing the output
        // against the file will find those two and should find this comment.
        let m = MaterialResolver::default().resolve("gear", Some(grey()));
        assert_eq!(m.metallic, 1.0, "mid grey with no other evidence reads as metal");
        assert_ne!(m.base_color, grey().linear);
        assert!(
            m.base_color.iter().all(|&c| c > 0.3),
            "a metal reflects far more than a mid grey swatch suggests: {:?}",
            m.base_color
        );

        // And a table that states a colour still wins.
        let m = table("part gear* = Aluminium 6061 #B87333").resolve("gear", Some(grey()));
        let expect = crate::sldmat::srgb_to_linear_rgb([0xB8 as f32 / 255.0, 0x73 as f32 / 255.0, 0x33 as f32 / 255.0]);
        for k in 0..3 {
            assert!((m.base_color[k] - expect[k]).abs() < 1e-6, "{:?}", m.base_color);
        }
    }

    #[test]
    fn a_colour_written_in_the_table_replaces_the_files() {
        // For a casting modelled grey and delivered black — which no amount of
        // reading the file will reveal, because the file says grey.
        let m = table("part 200 201 003* = Toz Boya #1A1A1A")
            .resolve("200 201 003-51", Some(grey()));
        let expect = crate::sldmat::srgb_to_linear_rgb([26.0 / 255.0; 3]);
        for k in 0..3 {
            assert!((m.base_color[k] - expect[k]).abs() < 1e-6, "{:?}", m.base_color);
        }
    }

    #[test]
    fn it_replaces_a_metals_colour_too() {
        // A dielectric normally takes the file's colour and a metal keeps the
        // reflectance measured for it. Someone who writes a colour has said
        // what the surface is, and there is nothing left to infer.
        let m = table("part gear* = Aluminium 6061 #B87333").resolve("gear 1", Some(grey()));
        assert_eq!(m.metallic, 1.0, "still a metal");
        let expect = crate::sldmat::srgb_to_linear_rgb([0xB8 as f32 / 255.0, 0x73 as f32 / 255.0, 0x33 as f32 / 255.0]);
        for k in 0..3 {
            assert!((m.base_color[k] - expect[k]).abs() < 1e-6, "{:?}", m.base_color);
        }
    }

    #[test]
    fn a_colour_rule_can_carry_one_as_well_as_a_part_rule() {
        let m = table("color 808080 = Toz Boya #000000").resolve("anything", Some(grey()));
        assert_eq!(m.base_color, [0.0, 0.0, 0.0]);
    }

    #[test]
    fn a_material_whose_name_ends_in_hex_keeps_its_name() {
        // `AISI 304` and `1.4301` are real names and a rule that ate six
        // characters off the end of one would be a fine way to lose it. Only a
        // hash counts.
        let (t, errors) = MaterialTable::parse("part a* = AISI 304\npart b* = 1.4301\n");
        assert!(errors.is_empty(), "{errors:?}");
        let r = MaterialResolver { table: t, ..MaterialResolver::default() };
        // Neither name lost its tail to a colour that was never written.
        for part in ["a1", "b1"] {
            let m = r.resolve(part, Some(blue()));
            assert!(
                m.name.contains("304") || m.name.contains("4301")
                    || m.name.to_lowercase().contains("steel")
                    || m.name.to_lowercase().contains("çelik"),
                "{part} became {:?}",
                m.name
            );
        }
    }

    #[test]
    fn a_colour_that_is_not_six_hex_digits_is_reported_rather_than_ignored() {
        let (_, errors) = MaterialTable::parse("part a* = Toz Boya #12345\n");
        assert_eq!(errors.len(), 1, "{errors:?}");
        assert!(errors[0].contains("RRGGBB"), "{}", errors[0]);
    }

    #[test]
    fn a_colour_on_its_own_is_a_material_named_by_it() {
        // `default = #1A1A1A` — no material, just a colour. It keeps the
        // shading the colour implies and takes the colour as written.
        let m = table("default = #1A1A1A").resolve("unknown", None);
        assert_eq!(m.base_color, crate::sldmat::srgb_to_linear_rgb([26.0 / 255.0; 3]));
    }
}

#[cfg(test)]
mod texture_tests {
    use super::*;
    use crate::scene::Scene;

    /// A material as the resolver leaves it: the appearance's finish, the
    /// file's own colour.
    fn painted(base: [f32; 3]) -> Material {
        let lib = crate::p2m::AppearanceLibrary::bundled();
        let appearance = lib.get("painted/powder coat/dark powdercoat").unwrap();
        Material {
            base_color: base,
            ..appearance.to_material("paint")
        }
    }

    #[test]
    fn what_the_image_lost_to_clipping_is_put_back_and_no_more() {
        // The shipped image is the grain alone — its level was divided out
        // offline by tools/make_grain.py — so the colour needs only the little
        // the grain lost when it was clipped at one. 0.905 for the powder
        // coat, a tenth of a stop.
        //
        // Dividing by the appearance's whole level was the first answer and it
        // was wrong: glTF's base colour factor stops at one, the pilot's
        // dominant paint needed 1.12 and its blue needed 2.44, and 45% of the
        // model came out white.
        let mut scene = Scene::default();
        let colour = [0.216, 0.216, 0.216]; // #808080, the pilot's dominant paint
        scene.add_material(painted(colour));
        attach_appearance_textures(&mut scene);

        let m = &scene.materials[0];
        if m.textures.base_colour.is_none() {
            return; // no image tree on this machine; build.rs said so
        }
        // By the path the appearance names, not by what the image ended up
        // called: the shipped file is the grain that replaced it.
        let path = crate::p2m::AppearanceLibrary::bundled()
            .get("painted/powder coat/dark powdercoat")
            .unwrap()
            .colour_texture
            .clone()
            .unwrap();
        let level = crate::p2m::bundled_texture_level(&path);
        assert!(level > 0.8 && level < 1.0, "the grain kept most of itself: {level}");

        for k in 0..3 {
            let expected = colour[k] / level;
            assert!(
                (m.base_color[k] - expected).abs() < 1e-6,
                "channel {k}: {} vs {expected}",
                m.base_color[k]
            );
            // And nothing was clamped away.
            assert!(m.base_color[k] < 1.0, "channel {k} saturated");
        }
    }

    #[test]
    fn the_colours_the_pilot_carries_all_survive() {
        // Every painted colour in the pilot assembly, including the one that
        // used to clamp to white and the blue that used to lose its hue.
        let colours: [[f32; 3]; 4] = [
            [0.216, 0.216, 0.216],       // #808080
            [0.220, 0.246, 0.262],       // #81888C
            [0.091, 0.091, 0.091],       // #555555
            [0.000, 0.033, 0.497],       // #0033BB
        ];
        for colour in colours {
            let mut scene = Scene::default();
            scene.add_material(painted(colour));
            attach_appearance_textures(&mut scene);
            let m = &scene.materials[0];
            if m.textures.base_colour.is_none() {
                return;
            }
            for k in 0..3 {
                assert!(
                    m.base_color[k] < 1.0,
                    "{colour:?} channel {k} saturated at {}",
                    m.base_color[k]
                );
                // Within a tenth of a stop of what the file said. A channel
                // the file set to zero stays zero: the blue paint has no red
                // in it and dividing by the grain does not invent any.
                assert!(
                    m.base_color[k] >= colour[k]
                        && m.base_color[k] <= colour[k] * 1.15 + 1e-6,
                    "{colour:?} channel {k} became {}",
                    m.base_color[k]
                );
            }
        }
    }

    #[test]
    fn a_material_with_no_appearance_is_left_exactly_as_it_was() {
        let mut scene = Scene::default();
        let steel = Material::from_class(MaterialClass::Steel, "steel");
        scene.add_material(steel.clone());
        let warnings = attach_appearance_textures(&mut scene);
        assert!(warnings.is_empty());
        assert_eq!(scene.materials[0].base_color, steel.base_color);
        assert!(scene.materials[0].textures.is_empty());
        assert!(scene.images.is_empty());
    }

    #[test]
    fn the_tile_size_arrives_in_millimetres() {
        let mut scene = Scene::default();
        scene.add_material(painted([0.1; 3]));
        attach_appearance_textures(&mut scene);
        if let Some([w, h]) = scene.materials[0].textures.tile_mm() {
            // 0.00635 m as stated by the file.
            assert!((w - 6.35).abs() < 1e-3 && (h - 6.35).abs() < 1e-3, "{w} x {h}");
        }
    }
}
