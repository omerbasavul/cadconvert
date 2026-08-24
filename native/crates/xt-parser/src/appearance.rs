//! Appearance hints from an XT file's attribute graph.
//!
//! Parasolid files carry designer-assigned appearance the neutral exchange
//! formats drop. Measured on the Solid Edge twin of the pilot assembly, whose
//! STEP export has colours only:
//!
//! * `SDL/TYSA_COLOUR` — per-face RGB, one per face, matching the STEP palette.
//! * `SDL/TYSA_REFLECTIVITY` — per-face 0..1. In the pilot every face of the
//!   two machined-metal colours carries 1.0 and every painted, rubber and
//!   plastic face carries 0.0 — the metal/matte split a person reads off the
//!   catalogue render, stated explicitly by the authoring tool.
//! * `SDL/TYSA_NAME` — per-body part numbers.
//!
//! The natural join key toward a STEP-built scene is the colour itself: face
//! numbering differs between the two exports, but the pilot's correlation is
//! exact per colour, so per-colour majority reflectivity transfers cleanly.

use crate::entity::{Entities, RawEntity};
use crate::{Result, XtError};
use std::collections::HashMap;

/// Appearance data recovered from one XT file.
#[derive(Debug, Clone, Default)]
pub struct AppearanceHints {
    /// Per colour (uppercase `RRGGBB`): faces seen, and how many of them are
    /// reflective (reflectivity ≥ 0.5).
    pub colours: HashMap<String, ColourStat>,
    /// Body entity handle → its `SDL/TYSA_NAME`, typically the part number.
    pub body_names: Vec<(usize, String)>,
}

/// Reflectivity statistics for one colour.
#[derive(Debug, Clone, Copy, Default)]
pub struct ColourStat {
    pub faces: usize,
    pub reflective: usize,
}

impl AppearanceHints {
    /// Majority reflectivity per colour, for feeding a material resolver.
    ///
    /// Colours whose faces disagree still get their majority — the pilot has
    /// one colour with 3 of 20 faces at 0.05, which is sheen noise, not a
    /// second material.
    pub fn reflectivity_by_colour(&self) -> HashMap<String, f32> {
        self.colours
            .iter()
            .filter(|(_, s)| s.faces > 0)
            .map(|(hex, s)| {
                (
                    hex.clone(),
                    if s.reflective * 2 >= s.faces { 1.0 } else { 0.0 },
                )
            })
            .collect()
    }
}

/// Extract appearance hints from XT file text.
pub fn appearance_hints(text: &str) -> Result<AppearanceHints> {
    let (header_text, body_text) = crate::header::split_header(text)?;
    let _ = crate::header::parse_header(header_text)?;
    let tline = crate::schema::parse_tline(body_text)?;
    let mut input = tline.body.as_str();
    let partition_count = if tline.has_base_schema {
        crate::schema::parse_schema_preamble(&mut input)
            .map_err(|e| XtError::Parse {
                offset: 0,
                detail: format!("schema preamble: {e}"),
            })?
            .partition_count
    } else {
        0
    };
    // The four kinds of entity the walk below touches, and the value carriers
    // an attribute's payload can sit in. Everything else is read and dropped:
    // this recovers fourteen colour-to-finish lines out of 476 877 entities,
    // and the STEP file that asked for them is still resident while it runs.
    //
    // Generous on the carriers on purpose. The walk takes whatever floats and
    // characters it finds behind an attribute's pointers, so a carrier left
    // out of this list would not fail — it would quietly return a different
    // colour.
    const KEEP: &[u16] = &[
        crate::schema::BODY,
        crate::schema::ATT_DEF_ID,
        crate::schema::ATTRIB_DEF,
        crate::schema::ATTRIBUTE,
        crate::schema::INT_VALUES,
        crate::schema::REAL_VALUES,
        crate::schema::CHAR_VALUES,
        crate::schema::POINT_VALUES,
        crate::schema::VECTOR_VALUES,
        crate::schema::AXIS_VALUES,
        crate::schema::TAG_VALUES,
        crate::schema::DIRECTION_VALUES,
    ];

    // A truncated stream still yields whatever attributes were read before the
    // stop; hints are best-effort by nature.
    let (entities, _truncated) = crate::entity::parse_entities_keeping(
        &mut input,
        partition_count,
        tline.has_base_schema,
        tline.key_major,
        Some(KEEP),
    )?;
    Ok(hints_from_entities(&entities))
}

/// The graph walk, separated for testing.
pub fn hints_from_entities(entities: &Entities) -> AppearanceHints {
    let by_index: HashMap<usize, &RawEntity> = entities.iter().map(|e| (e.index, e)).collect();

    // ATTRIB_DEF (80) → its name, via the ATT_DEF_ID (79) it points at.
    let mut def_names: HashMap<usize, String> = HashMap::new();
    for e in entities.iter().filter(|e| e.type_id == 80) {
        let ident = entities.fields(e).get(1).map(|f| f.as_ptr()).unwrap_or(0);
        if let Some(id_e) = by_index.get(&ident) {
            def_names.insert(e.index, id_e.var_char().iter().collect());
        }
    }

    let mut colour_of_face: HashMap<usize, String> = HashMap::new();
    let mut reflectivity_of_face: HashMap<usize, f64> = HashMap::new();
    let mut body_names = Vec::new();

    for e in entities.iter().filter(|e| e.type_id == 81) {
        let def = entities.fields(e).get(1).map(|f| f.as_ptr()).unwrap_or(0);
        let owner = entities.fields(e).get(2).map(|f| f.as_ptr()).unwrap_or(0);
        let Some(def_name) = def_names.get(&def) else {
            continue;
        };

        // Gather this attribute's payload from its value entities.
        let mut floats: Vec<f64> = Vec::new();
        let mut chars = String::new();
        for &v in e.var_ptr() {
            if let Some(ve) = by_index.get(&v) {
                floats.extend_from_slice(ve.var_f64());
                chars.extend(ve.var_char().iter());
            }
        }

        match def_name.as_str() {
            "SDL/TYSA_COLOUR" if floats.len() >= 3 => {
                let hex = format!(
                    "{:02X}{:02X}{:02X}",
                    (floats[0].clamp(0.0, 1.0) * 255.0).round() as u8,
                    (floats[1].clamp(0.0, 1.0) * 255.0).round() as u8,
                    (floats[2].clamp(0.0, 1.0) * 255.0).round() as u8
                );
                colour_of_face.insert(owner, hex);
            }
            "SDL/TYSA_REFLECTIVITY" if !floats.is_empty() => {
                reflectivity_of_face.insert(owner, floats[0]);
            }
            "SDL/TYSA_NAME" if !chars.is_empty() => {
                if by_index.get(&owner).is_some_and(|o| o.type_id == 12) {
                    body_names.push((owner, chars));
                }
            }
            _ => {}
        }
    }

    let mut colours: HashMap<String, ColourStat> = HashMap::new();
    for (face, hex) in &colour_of_face {
        let stat = colours.entry(hex.clone()).or_default();
        stat.faces += 1;
        if reflectivity_of_face.get(face).copied().unwrap_or(0.0) >= 0.5 {
            stat.reflective += 1;
        }
    }

    AppearanceHints {
        colours,
        body_names,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The parts these read are not in the repository — they are real exports,
    /// too large and not ours to publish. Point `XT_SAMPLES` at a directory of
    /// them to run these; without it they skip.
    fn load(name: &str) -> Option<AppearanceHints> {
        let Ok(dir) = std::env::var("XT_SAMPLES") else {
            eprintln!("XT_SAMPLES unset; skipping {name}");
            return None;
        };
        let path = std::path::Path::new(&dir).join(name);
        if !path.exists() {
            eprintln!("sample {name} absent; skipping");
            return None;
        }
        let bytes = std::fs::read(path).unwrap();
        Some(appearance_hints(&String::from_utf8_lossy(&bytes)).unwrap())
    }

    /// The measured ground truth of the pilot twin: two machined-metal colours
    /// fully reflective, everything else matte, and the body part numbers
    /// present. A regression here means the attribute graph walk broke.
    #[test]
    fn the_solid_edge_twin_yields_the_known_reflectivity_split() {
        let Some(h) = load("910 2001 007.x_t") else {
            return;
        };
        let refl = h.reflectivity_by_colour();
        assert_eq!(refl.get("D1D1D1"), Some(&1.0), "the bright metal colour");
        assert_eq!(refl.get("555759"), Some(&1.0), "the dark metal colour");
        for matte in ["808080", "555555", "0033BB", "000000", "30BF94"] {
            assert_eq!(refl.get(matte), Some(&0.0), "#{matte} must be matte");
        }
        assert!(
            h.body_names.iter().any(|(_, n)| n == "218 201 005"),
            "part numbers missing: {:?}",
            h.body_names.iter().take(3).collect::<Vec<_>>()
        );
        let total: usize = h.colours.values().map(|s| s.faces).sum();
        assert_eq!(total, 11214, "one colour per face");
    }

    /// A SolidWorks export has colours but no reflectivity attributes at all —
    /// every colour must then read matte rather than inventing shine.
    #[test]
    fn a_file_without_reflectivity_reads_matte() {
        let Some(h) = load("500.076.x_t") else {
            return;
        };
        let refl = h.reflectivity_by_colour();
        assert_eq!(refl.get("808080"), Some(&0.0));
    }
}
