//! Resolving STEP presentation styles down to a colour per styled item.
//!
//! AP214 buries a face's colour eight indirections deep:
//!
//! ```text
//! STYLED_ITEM → PRESENTATION_STYLE_ASSIGNMENT → SURFACE_STYLE_USAGE
//!   → SURFACE_SIDE_STYLE → SURFACE_STYLE_FILL_AREA → FILL_AREA_STYLE
//!   → FILL_AREA_STYLE_COLOUR → COLOUR_RGB
//! ```
//!
//! Every link is a SELECT type, so exporters differ in which optional layers
//! they write and in whether they carry colour through the fill-area chain, the
//! rendering chain, or both. The resolver walks the shapes it knows and falls
//! back to a bounded search for a colour entity when a vendor writes something
//! outside them, because a face with a slightly-wrongly-derived colour is a far
//! better outcome than a face with no colour at all.
//!
//! A styled item's target may be a face, a solid, or a whole shape
//! representation, so callers must look up a face's colour by walking outward
//! from the face to its solid and its representation — [`Styles::lookup`] does
//! exactly that when given the chain.

use crate::error::Result;
use crate::kind::Kind;
use crate::{Entity, StepFile};
use rustc_hash::{FxHashMap, FxHashSet};

/// A resolved surface appearance.
///
/// ISO 10303-46 says only that a colour component is "the intensity" in 0..1
/// and never names a transfer function, so the meaning has to be read off real
/// files. In `910 2001 007.stp` the thirteen colours include
/// `(0, 0.2, 0.7333…)`, `(0.1333…, 0.7333…, 0.5333…)` and `(0.8, 0, 0)` —
/// which are exactly 0x00/0x33/0xBB, 0x22/0xBB/0x88 and 0xCC/0x00/0x00 over
/// 255. Authoring tools write the colour the user picked in the swatch, so
/// these components are **display-referred sRGB**, not linear.
///
/// That distinction is not cosmetic: glTF `baseColorFactor` and USD
/// `diffuseColor` are both linear, so passing these numbers through unchanged
/// makes every surface visibly too bright. [`Appearance::linear_rgb`] is the
/// conversion, and it is the value the exporters must use.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Appearance {
    /// Display-referred sRGB in 0..1, exactly as the file stores it.
    pub rgb: [f32; 3],
    /// Opacity in 0..1. 1.0 is fully opaque.
    pub alpha: f32,
}

impl Appearance {
    /// The 8-bit sRGB hex form, for logs and material naming.
    pub fn srgb_hex(&self) -> String {
        let enc = |v: f32| (v.clamp(0.0, 1.0) * 255.0).round() as u8;
        format!(
            "{:02X}{:02X}{:02X}",
            enc(self.rgb[0]),
            enc(self.rgb[1]),
            enc(self.rgb[2])
        )
    }

    /// The colour as linear RGB, for glTF `baseColorFactor` and USD
    /// `diffuseColor`.
    pub fn linear_rgb(&self) -> [f32; 3] {
        let dec = |v: f32| {
            let v = v.clamp(0.0, 1.0);
            if v <= 0.040_45 {
                v / 12.92
            } else {
                ((v + 0.055) / 1.055).powf(2.4)
            }
        };
        [dec(self.rgb[0]), dec(self.rgb[1]), dec(self.rgb[2])]
    }
}

/// Every appearance the file assigns, keyed by the entity it was assigned to.
#[derive(Debug, Default, Clone)]
pub struct Styles {
    by_item: FxHashMap<u32, Appearance>,
    /// Styled items whose style chain yielded no colour at all.
    pub unresolved: Vec<u32>,
}

impl Styles {
    /// The appearance assigned directly to `id`.
    pub fn get(&self, id: u32) -> Option<Appearance> {
        self.by_item.get(&id).copied()
    }

    /// The first assigned appearance along a most-specific-first chain.
    ///
    /// Pass the face, then its solid, then its shape representation: STEP lets
    /// an exporter colour any of them, and the innermost assignment wins.
    pub fn lookup(&self, chain: impl IntoIterator<Item = u32>) -> Option<Appearance> {
        chain.into_iter().find_map(|id| self.get(id))
    }

    /// How many items carry an appearance.
    pub fn len(&self) -> usize {
        self.by_item.len()
    }

    pub fn is_empty(&self) -> bool {
        self.by_item.is_empty()
    }

    /// The distinct appearances present, most-used first.
    pub fn palette(&self) -> Vec<(Appearance, usize)> {
        let mut counts: FxHashMap<[u32; 4], (Appearance, usize)> = FxHashMap::default();
        for a in self.by_item.values() {
            let key = [
                a.rgb[0].to_bits(),
                a.rgb[1].to_bits(),
                a.rgb[2].to_bits(),
                a.alpha.to_bits(),
            ];
            let slot = counts.entry(key).or_insert((*a, 0));
            slot.1 += 1;
        }
        let mut v: Vec<_> = counts.into_values().collect();
        v.sort_by(|a, b| b.1.cmp(&a.1));
        v
    }
}

/// Resolve every `STYLED_ITEM` in the file.
pub fn resolve(file: &StepFile) -> Result<Styles> {
    let mut styles = Styles::default();

    // Plain styled items first, then overrides, so an override always lands on
    // top of the style it replaces regardless of file order.
    for e in file.by_kind(Kind::StyledItem) {
        apply_styled_item(file, e, false, &mut styles)?;
    }
    for e in file.by_kind(Kind::OverRidingStyledItem) {
        apply_styled_item(file, e, true, &mut styles)?;
    }

    Ok(styles)
}

/// `STYLED_ITEM(name, styles, item)`; the override subtype appends
/// `over_ridden_style`, which we do not need because we key on the item.
fn apply_styled_item(
    file: &StepFile,
    e: &Entity,
    is_override: bool,
    out: &mut Styles,
) -> Result<()> {
    let mut a = file.args_of(e);
    a.skip()?; // name
    let mut style_refs = Vec::new();
    a.next_ref_list(&mut style_refs)?;
    let Ok(item) = a.next_ref() else {
        return Ok(());
    };

    let mut found: Option<Appearance> = None;
    for &s in &style_refs {
        if let Some(app) = appearance_from_style(file, s)? {
            found = Some(app);
            break;
        }
    }

    match found {
        Some(app) => {
            if is_override || !out.by_item.contains_key(&item) {
                out.by_item.insert(item, app);
            }
        }
        None => out.unresolved.push(e.id),
    }
    Ok(())
}

/// Resolve one `PRESENTATION_STYLE_ASSIGNMENT` (or a style it selects).
fn appearance_from_style(file: &StepFile, id: u32) -> Result<Option<Appearance>> {
    let Some(e) = file.get(id) else {
        return Ok(None);
    };

    match e.kind {
        Kind::PresentationStyleAssignment | Kind::PresentationStyleByContext => {
            let mut a = file.args_of(e);
            let mut refs = Vec::new();
            a.next_ref_list(&mut refs)?;
            for &r in &refs {
                if let Some(app) = appearance_from_style(file, r)? {
                    return Ok(Some(app));
                }
            }
            Ok(None)
        }

        // `SURFACE_STYLE_USAGE(side, style)`
        Kind::SurfaceStyleUsage => {
            let mut a = file.args_of(e);
            a.skip()?; // .BOTH. / .POSITIVE. / .NEGATIVE.
            match a.next_ref() {
                Ok(r) => appearance_from_style(file, r),
                Err(_) => Ok(None),
            }
        }

        // `SURFACE_SIDE_STYLE(name, styles)` — a set mixing fill-area,
        // rendering and transparency entries, so gather across all of them.
        Kind::SurfaceSideStyle => {
            let mut a = file.args_of(e);
            a.skip()?; // name
            let mut refs = Vec::new();
            a.next_ref_list(&mut refs)?;
            let mut rgb: Option<[f32; 3]> = None;
            let mut alpha = 1.0f32;
            for &r in &refs {
                match file.kind_of(r) {
                    Kind::SurfaceStyleTransparent => {
                        if let Ok(mut t) = file.args(r)
                            && let Ok(v) = t.next_measure_f64()
                        {
                            // The attribute is transparency, not opacity.
                            alpha = 1.0 - (v as f32).clamp(0.0, 1.0);
                        }
                    }
                    _ => {
                        if rgb.is_none()
                            && let Some(app) = appearance_from_style(file, r)?
                        {
                            rgb = Some(app.rgb);
                            // A rendering entry may itself carry transparency.
                            if app.alpha < 1.0 {
                                alpha = app.alpha;
                            }
                        }
                    }
                }
            }
            Ok(rgb.map(|rgb| Appearance { rgb, alpha }))
        }

        // `SURFACE_STYLE_FILL_AREA(fill_area)`
        Kind::SurfaceStyleFillArea => {
            let mut a = file.args_of(e);
            match a.next_ref() {
                Ok(r) => appearance_from_style(file, r),
                Err(_) => Ok(None),
            }
        }

        // `FILL_AREA_STYLE(name, styles)`
        Kind::FillAreaStyle => {
            let mut a = file.args_of(e);
            a.skip()?; // name
            let mut refs = Vec::new();
            a.next_ref_list(&mut refs)?;
            for &r in &refs {
                if let Some(app) = appearance_from_style(file, r)? {
                    return Ok(Some(app));
                }
            }
            Ok(None)
        }

        // `FILL_AREA_STYLE_COLOUR(name, colour)`
        Kind::FillAreaStyleColour => {
            let mut a = file.args_of(e);
            a.skip()?; // name
            match a.next_ref() {
                Ok(r) => colour_of(file, r),
                Err(_) => Ok(None),
            }
        }

        // `SURFACE_STYLE_RENDERING(method, colour)` and the
        // `…_WITH_PROPERTIES(method, colour, properties)` subtype.
        Kind::SurfaceStyleRendering | Kind::SurfaceStyleRenderingWithProperties => {
            let mut a = file.args_of(e);
            a.skip()?; // rendering method
            match a.next_ref() {
                Ok(r) => colour_of(file, r),
                Err(_) => Ok(None),
            }
        }

        Kind::ColourRgb | Kind::DraughtingPreDefinedColour => colour_of(file, id),

        // A vendor shape we do not model. Search its references for a colour
        // rather than dropping the style entirely.
        _ => {
            let mut seen = FxHashSet::default();
            Ok(search_for_colour(file, id, 4, &mut seen)?)
        }
    }
}

/// Decode a colour entity.
fn colour_of(file: &StepFile, id: u32) -> Result<Option<Appearance>> {
    let Some(e) = file.get(id) else {
        return Ok(None);
    };
    match e.kind {
        Kind::ColourRgb => {
            let mut a = file.args_of(e);
            a.skip()?; // name
            let (Ok(r), Ok(g), Ok(b)) = (a.next_f64(), a.next_f64(), a.next_f64()) else {
                return Ok(None);
            };
            Ok(Some(Appearance {
                rgb: [r as f32, g as f32, b as f32],
                alpha: 1.0,
            }))
        }
        Kind::DraughtingPreDefinedColour => {
            let mut a = file.args_of(e);
            let name = a.next_str()?;
            Ok(predefined_colour(&name).map(|rgb| Appearance { rgb, alpha: 1.0 }))
        }
        _ => Ok(None),
    }
}

/// The ISO 10303-46 pre-defined colour names.
fn predefined_colour(name: &str) -> Option<[f32; 3]> {
    Some(match name.to_ascii_lowercase().as_str() {
        "black" => [0.0, 0.0, 0.0],
        "white" => [1.0, 1.0, 1.0],
        "red" => [1.0, 0.0, 0.0],
        "green" => [0.0, 1.0, 0.0],
        "blue" => [0.0, 0.0, 1.0],
        "yellow" => [1.0, 1.0, 0.0],
        "magenta" => [1.0, 0.0, 1.0],
        "cyan" => [0.0, 1.0, 1.0],
        _ => return None,
    })
}

/// Bounded breadth-first hunt for a colour entity reachable from `id`.
///
/// Used only when an exporter writes a style shape outside the modelled chain.
fn search_for_colour(
    file: &StepFile,
    id: u32,
    depth: u32,
    seen: &mut FxHashSet<u32>,
) -> Result<Option<Appearance>> {
    if depth == 0 || !seen.insert(id) {
        return Ok(None);
    }
    let Some(e) = file.get(id) else {
        return Ok(None);
    };
    if matches!(e.kind, Kind::ColourRgb | Kind::DraughtingPreDefinedColour) {
        return colour_of(file, id);
    }
    let mut a = file.args_of(e);
    let values = a.rest().unwrap_or_default();
    for r in refs_in(&values) {
        if let Some(app) = search_for_colour(file, r, depth - 1, seen)? {
            return Ok(Some(app));
        }
    }
    Ok(None)
}

/// Every entity reference appearing anywhere in a decoded value tree.
fn refs_in(values: &[crate::Value<'_>]) -> Vec<u32> {
    let mut out = Vec::new();
    fn walk(v: &crate::Value<'_>, out: &mut Vec<u32>) {
        match v {
            crate::Value::Ref(id) => out.push(*id),
            crate::Value::List(items) | crate::Value::Typed(_, items) => {
                for i in items {
                    walk(i, out);
                }
            }
            _ => {}
        }
    }
    for v in values {
        walk(v, &mut out);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse(data: &str) -> StepFile {
        let src =
            format!("ISO-10303-21;\nHEADER;\nENDSEC;\nDATA;\n{data}\nENDSEC;\nEND-ISO-10303-21;\n");
        StepFile::from_bytes(src.into_bytes()).unwrap()
    }

    /// The full AP214 fill-area chain as Solid Edge writes it.
    const FILL_CHAIN: &str = "\
#1=ADVANCED_FACE('',(),#99,.T.);
#10=COLOUR_RGB('',0.8,0.2,0.1);
#11=FILL_AREA_STYLE_COLOUR('',#10);
#12=FILL_AREA_STYLE('',(#11));
#13=SURFACE_STYLE_FILL_AREA(#12);
#14=SURFACE_SIDE_STYLE('',(#13));
#15=SURFACE_STYLE_USAGE(.BOTH.,#14);
#16=PRESENTATION_STYLE_ASSIGNMENT((#15));
#17=STYLED_ITEM('',(#16),#1);";

    #[test]
    fn resolves_the_full_fill_area_chain() {
        let f = parse(FILL_CHAIN);
        let s = resolve(&f).unwrap();
        assert!(s.unresolved.is_empty());
        let app = s.get(1).expect("face 1 should be styled");
        assert_eq!(app.rgb, [0.8, 0.2, 0.1]);
        assert_eq!(app.alpha, 1.0);
    }

    #[test]
    fn resolves_the_rendering_chain_with_transparency() {
        let f = parse(
            "#1=MANIFOLD_SOLID_BREP('',#98);
             #10=COLOUR_RGB('',0.1,0.4,0.9);
             #11=SURFACE_STYLE_RENDERING(.NORMAL_SHADING.,#10);
             #12=SURFACE_STYLE_TRANSPARENT(0.25);
             #13=SURFACE_SIDE_STYLE('',(#11,#12));
             #14=SURFACE_STYLE_USAGE(.BOTH.,#13);
             #15=PRESENTATION_STYLE_ASSIGNMENT((#14));
             #16=STYLED_ITEM('',(#15),#1);",
        );
        let s = resolve(&f).unwrap();
        let app = s.get(1).unwrap();
        assert_eq!(app.rgb, [0.1, 0.4, 0.9]);
        assert!((app.alpha - 0.75).abs() < 1e-6);
    }

    #[test]
    fn an_override_replaces_the_base_style() {
        let mut src = FILL_CHAIN.to_string();
        src.push_str(
            "
#20=COLOUR_RGB('',0.,1.,0.);
#21=FILL_AREA_STYLE_COLOUR('',#20);
#22=FILL_AREA_STYLE('',(#21));
#23=SURFACE_STYLE_FILL_AREA(#22);
#24=SURFACE_SIDE_STYLE('',(#23));
#25=SURFACE_STYLE_USAGE(.BOTH.,#24);
#26=PRESENTATION_STYLE_ASSIGNMENT((#25));
#27=OVER_RIDING_STYLED_ITEM('',(#26),#1,#17);",
        );
        let f = parse(&src);
        let s = resolve(&f).unwrap();
        assert_eq!(s.get(1).unwrap().rgb, [0.0, 1.0, 0.0]);
    }

    #[test]
    fn a_predefined_colour_name_resolves() {
        let f = parse(
            "#1=ADVANCED_FACE('',(),#99,.T.);
             #10=DRAUGHTING_PRE_DEFINED_COLOUR('red');
             #11=FILL_AREA_STYLE_COLOUR('',#10);
             #12=FILL_AREA_STYLE('',(#11));
             #13=SURFACE_STYLE_FILL_AREA(#12);
             #14=SURFACE_SIDE_STYLE('',(#13));
             #15=SURFACE_STYLE_USAGE(.BOTH.,#14);
             #16=PRESENTATION_STYLE_ASSIGNMENT((#15));
             #17=STYLED_ITEM('',(#16),#1);",
        );
        assert_eq!(resolve(&f).unwrap().get(1).unwrap().rgb, [1.0, 0.0, 0.0]);
    }

    #[test]
    fn a_style_with_no_colour_is_reported_not_silently_dropped() {
        let f = parse(
            "#1=ADVANCED_FACE('',(),#99,.T.);
             #15=CURVE_STYLE('',#98,POSITIVE_LENGTH_MEASURE(0.1),#97);
             #16=PRESENTATION_STYLE_ASSIGNMENT((#15));
             #17=STYLED_ITEM('',(#16),#1);",
        );
        let s = resolve(&f).unwrap();
        assert!(s.get(1).is_none());
        assert_eq!(s.unresolved, vec![17]);
    }

    #[test]
    fn lookup_prefers_the_most_specific_assignment() {
        let mut src = FILL_CHAIN.to_string();
        src.push_str(
            "
#30=MANIFOLD_SOLID_BREP('',#98);
#31=COLOUR_RGB('',0.,0.,1.);
#32=FILL_AREA_STYLE_COLOUR('',#31);
#33=FILL_AREA_STYLE('',(#32));
#34=SURFACE_STYLE_FILL_AREA(#33);
#35=SURFACE_SIDE_STYLE('',(#34));
#36=SURFACE_STYLE_USAGE(.BOTH.,#35);
#37=PRESENTATION_STYLE_ASSIGNMENT((#36));
#38=STYLED_ITEM('',(#37),#30);",
        );
        let f = parse(&src);
        let s = resolve(&f).unwrap();
        // Face 1 is styled directly, so its own colour wins over the solid's.
        assert_eq!(s.lookup([1, 30]).unwrap().rgb, [0.8, 0.2, 0.1]);
        // A face with no style of its own inherits the solid's.
        assert_eq!(s.lookup([2, 30]).unwrap().rgb, [0.0, 0.0, 1.0]);
        assert!(s.lookup([2, 31337]).is_none());
    }

    #[test]
    fn srgb_hex_is_a_direct_8_bit_encoding() {
        // The file stores display sRGB, so 0.2 is 0x33 — the exact byte the
        // authoring tool's colour picker produced.
        let a = Appearance {
            rgb: [0.0, 0.2, 0.733_333_3],
            alpha: 1.0,
        };
        assert_eq!(a.srgb_hex(), "0033BB");
    }

    #[test]
    fn linear_conversion_darkens_mid_greys() {
        let a = Appearance {
            rgb: [0.5, 0.5, 0.5],
            alpha: 1.0,
        };
        // sRGB 0.5 is linear 0.2140, the standard EOTF value.
        assert!((a.linear_rgb()[0] - 0.214_041_14).abs() < 1e-6);
        // The transfer function is exact at both ends.
        let black = Appearance {
            rgb: [0.0; 3],
            alpha: 1.0,
        };
        let white = Appearance {
            rgb: [1.0; 3],
            alpha: 1.0,
        };
        assert_eq!(black.linear_rgb(), [0.0; 3]);
        assert!((white.linear_rgb()[0] - 1.0).abs() < 1e-6);
    }

    #[test]
    fn palette_counts_distinct_appearances() {
        let f = parse(FILL_CHAIN);
        let s = resolve(&f).unwrap();
        let p = s.palette();
        assert_eq!(p.len(), 1);
        assert_eq!(p[0].1, 1);
    }
}
