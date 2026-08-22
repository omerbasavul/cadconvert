//! Extract attribute data — colours, names, reflectivity — from an XT file.
//!
//! `cargo run --release -p xt-parser --example xt_attrs -- file.x_t`
//!
//! Walks the raw entity graph directly: ATT_DEF_ID (79) carries a definition's
//! name as raw chars, ATTRIB_DEF (80) points to it, ATTRIBUTE (81) points to
//! its definition and its owner, and the value entities (82–89) hang off the
//! attribute's pointer array.

use std::collections::HashMap;
use xt_parser::entity::RawEntity;

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let Some(path) = std::env::args().nth(1) else {
        eprintln!("usage: xt_attrs <file.x_t>");
        std::process::exit(2);
    };

    let bytes = std::fs::read(&path)?;
    let text = String::from_utf8_lossy(&bytes);
    let (header_text, body_text) = xt_parser::header::split_header(&text)?;
    let _ = xt_parser::header::parse_header(header_text)?;
    let tline = xt_parser::schema::parse_tline(body_text)?;
    let mut input = tline.body.as_str();
    let partition_count = if tline.has_base_schema {
        xt_parser::schema::parse_schema_preamble(&mut input)
            .map_err(|e| format!("schema preamble: {e}"))?
            .partition_count
    } else {
        0
    };
    let (entities, truncated) = xt_parser::entity::parse_entities_opt(
        &mut input,
        partition_count,
        tline.has_base_schema,
        tline.key_major,
    )?;
    println!("{} entities", entities.len());
    if let Some(t) = &truncated {
        println!("TRUNCATED: {t}");
    }

    // Index by entity handle.
    let by_index: HashMap<usize, &RawEntity> =
        entities.iter().map(|e| (e.index, e)).collect();
    let type_of = |idx: usize| by_index.get(&idx).map(|e| e.type_id);

    // Definition names: ATT_DEF_ID (79) raw chars, referenced by
    // ATTRIB_DEF.identifier (field 1).
    let mut def_names: HashMap<usize, String> = HashMap::new();
    for e in entities.iter().filter(|e| e.type_id == 80) {
        let ident = e.fields.get(1).map(|f| f.as_ptr()).unwrap_or(0);
        if let Some(id_e) = by_index.get(&ident) {
            let name: String = id_e.var_char().iter().collect();
            def_names.insert(e.index, name);
        }
    }
    println!("\n-- attribute definitions --");
    let mut names: Vec<_> = def_names.iter().collect();
    names.sort();
    for (idx, name) in &names {
        println!("  def #{idx}: {name}");
    }

    // Attributes: definition (field 1), owner (field 2), values (var_ptr).
    println!("\n-- attributes by definition --");
    let mut per_def: HashMap<&str, usize> = HashMap::new();
    let mut colours: Vec<(usize, u16, [f64; 3])> = Vec::new(); // owner, owner_type, rgb
    let mut name_attrs: Vec<(usize, u16, String)> = Vec::new();
    let mut reflect: Vec<(usize, f64)> = Vec::new();

    for e in entities.iter().filter(|e| e.type_id == 81) {
        let def = e.fields.get(1).map(|f| f.as_ptr()).unwrap_or(0);
        let owner = e.fields.get(2).map(|f| f.as_ptr()).unwrap_or(0);
        let Some(def_name) = def_names.get(&def) else {
            continue;
        };
        *per_def.entry(def_name.as_str()).or_default() += 1;
        let owner_type = type_of(owner).unwrap_or(0);

        // Value entities hang off the attribute's pointer array.
        let mut floats: Vec<f64> = Vec::new();
        let mut chars: String = String::new();
        for &v in e.var_ptr() {
            if let Some(ve) = by_index.get(&(v as usize)) {
                floats.extend_from_slice(ve.var_f64());
                chars.extend(ve.var_char().iter());
            }
        }
        if def_name.contains("COLOUR") && floats.len() >= 3 {
            colours.push((owner, owner_type, [floats[0], floats[1], floats[2]]));
        } else if def_name.contains("NAME") && !chars.is_empty() {
            name_attrs.push((owner, owner_type, chars));
        } else if def_name.contains("REFLECT") && !floats.is_empty() {
            reflect.push((owner, floats[0]));
        }
    }
    let mut per: Vec<_> = per_def.iter().collect();
    per.sort_by_key(|(_, n)| std::cmp::Reverse(**n));
    for (name, n) in per {
        println!("  {name}: {n}");
    }

    println!("\n-- colours ({}) --", colours.len());
    let mut palette: HashMap<String, (usize, HashMap<u16, usize>)> = HashMap::new();
    for (_, owner_type, rgb) in &colours {
        let hex = format!(
            "{:02X}{:02X}{:02X}",
            (rgb[0].clamp(0.0, 1.0) * 255.0).round() as u8,
            (rgb[1].clamp(0.0, 1.0) * 255.0).round() as u8,
            (rgb[2].clamp(0.0, 1.0) * 255.0).round() as u8
        );
        let slot = palette.entry(hex).or_default();
        slot.0 += 1;
        *slot.1.entry(*owner_type).or_default() += 1;
    }
    let mut pal: Vec<_> = palette.iter().collect();
    pal.sort_by_key(|(_, (n, _))| std::cmp::Reverse(*n));
    for (hex, (n, owners)) in pal {
        let mut o: Vec<_> = owners.iter().collect();
        o.sort();
        let odesc: Vec<String> = o.iter().map(|(t, c)| format!("type{t}x{c}")).collect();
        println!("  #{hex}: {n}  (owners: {})", odesc.join(", "));
    }

    println!("\n-- name attributes ({}) --", name_attrs.len());
    for (owner, owner_type, name) in name_attrs.iter().take(30) {
        println!("  owner #{owner} (type {owner_type}): {name:?}");
    }

    // The join that decides how each colour should be shaded: does the
    // designer's reflectivity flag correlate with colour?
    let refl_of: HashMap<usize, f64> = reflect.iter().copied().collect();
    let mut cross: HashMap<(String, String), usize> = HashMap::new();
    for (owner, _, rgb) in &colours {
        let hex = format!(
            "{:02X}{:02X}{:02X}",
            (rgb[0].clamp(0.0, 1.0) * 255.0).round() as u8,
            (rgb[1].clamp(0.0, 1.0) * 255.0).round() as u8,
            (rgb[2].clamp(0.0, 1.0) * 255.0).round() as u8
        );
        let r = refl_of
            .get(owner)
            .map(|v| format!("{v:.2}"))
            .unwrap_or_else(|| "none".into());
        *cross.entry((hex, r)).or_default() += 1;
    }
    let mut cv: Vec<_> = cross.iter().collect();
    cv.sort_by_key(|((h, _), n)| (h.clone(), std::cmp::Reverse(**n)));
    println!("\n-- colour x reflectivity --");
    for ((hex, r), n) in cv {
        println!("  #{hex}  refl={r}: {n}");
    }

    println!("\n-- reflectivity ({}) --", reflect.len());
    let mut vals: HashMap<String, usize> = HashMap::new();
    for (_, v) in &reflect {
        *vals.entry(format!("{v:.3}")).or_default() += 1;
    }
    let mut vv: Vec<_> = vals.iter().collect();
    vv.sort();
    for (v, n) in vv {
        println!("  {v}: {n}");
    }
    Ok(())
}
