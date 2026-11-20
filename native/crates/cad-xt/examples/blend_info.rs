//! What a file's rolling-ball blends are made of.
//!
//! `cargo run --release -p cad-xt --example blend_info -- file.x_t`
//!
//! One line per BLENDED_EDGE: its type and two ranges, the surfaces it runs
//! between, and what its spine is — the curve the ball's centre follows, which
//! everything else about the blend is derived from.

use rustc_hash::FxHashMap;
use std::collections::BTreeMap;
use xt_parser::entity::RawEntity;

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let path = std::env::args().nth(1).expect("usage: blend_info <file.x_t>");
    let file = xt_parser::parse_raw_owned(xt_parser::decode(std::fs::read(&path)?))?;
    let ents = &file.entities;
    let index: FxHashMap<usize, &RawEntity> = ents.iter().map(|e| (e.index, e)).collect();
    let type_of = |h: usize| index.get(&h).map(|e| e.type_id).unwrap_or(0);
    let p = |e: &RawEntity, i: usize| ents.fields(e).get(i).map(|f| f.as_ptr()).unwrap_or(0);
    let f = |e: &RawEntity, i: usize| ents.fields(e).get(i).map(|f| f.as_f64()).unwrap_or(f64::NAN);
    let c = |e: &RawEntity, i: usize| ents.fields(e).get(i).map(|f| f.as_char()).unwrap_or('?');

    // How many faces sit on each surface entity.
    let mut faces_on: FxHashMap<usize, usize> = FxHashMap::default();
    let mut face_surface_types: BTreeMap<u16, usize> = BTreeMap::new();
    for e in ents.iter().filter(|e| e.type_id == 14) {
        *faces_on.entry(p(e, 7)).or_default() += 1;
        *face_surface_types.entry(type_of(p(e, 7))).or_default() += 1;
    }
    println!("faces by surface type: {face_surface_types:?}");

    let mut census: BTreeMap<String, usize> = BTreeMap::new();
    for be in ents.iter().filter(|e| e.type_id == 56) {
        let (sa, sb) = (p(be, 8), p(be, 9));
        let spine = p(be, 10);
        let spine_e = index.get(&spine);
        let (spine_surfs, chart) = match spine_e {
            Some(se) if se.type_id == 38 => {
                let s0 = p(se, 7);
                let s1 = ents.extra(se).first().map(|f| f.as_ptr()).unwrap_or(0);
                let off = |h: usize| match index.get(&h) {
                    Some(o) if o.type_id == 60 => {
                        format!("60(base {} #{} d {:.5})", type_of(p(o, 9)), p(o, 9), f(o, 10))
                    }
                    Some(o) => format!("{}", o.type_id),
                    None => "-".into(),
                };
                let ch = index.get(&p(se, 8)).map(|ch| {
                    format!(
                        "chart n={} chord {:.6} ang {:.4}",
                        ents.fields(ch).get(2).map(|f| f.as_i64()).unwrap_or(0),
                        f(ch, 3),
                        f(ch, 4)
                    )
                });
                (format!("{} x {}", off(s0), off(s1)), ch.unwrap_or_default())
            }
            Some(se) => (format!("spine type {}", se.type_id), String::new()),
            None => ("no spine".into(), String::new()),
        };
        let key = format!("{} x {}", type_of(sa), type_of(sb));
        *census.entry(key).or_default() += 1;
        println!(
            "blend #{:<6} {} faces {}  range [{:+.5},{:+.5}] thumb [{:.2},{:.2}]  mates {}#{} {}#{}  spine #{} {}  {}",
            be.index,
            c(be, 7),
            faces_on.get(&be.index).copied().unwrap_or(0),
            f(be, 11),
            f(be, 12),
            f(be, 13),
            f(be, 14),
            type_of(sa),
            sa,
            type_of(sb),
            sb,
            spine,
            spine_surfs,
            chart
        );
    }
    println!("mate pairs: {census:?}");
    topology(ents);
    Ok(())
}

// Appended probe: topology census. Temporary.
#[allow(dead_code)]
fn topology(ents: &xt_parser::entity::Entities) {
    use std::collections::BTreeMap;
    let index: FxHashMap<usize, &RawEntity> = ents.iter().map(|e| (e.index, e)).collect();
    let p = |e: &RawEntity, i: usize| ents.fields(e).get(i).map(|f| f.as_ptr()).unwrap_or(0);
    let mut by_type: BTreeMap<u16, usize> = BTreeMap::new();
    for e in ents.iter() {
        *by_type.entry(e.type_id).or_default() += 1;
    }
    println!("entities by type: {by_type:?}");
    for b in ents.iter().filter(|e| e.type_id == 12) {
        let fields: Vec<String> = ents
            .fields(b)
            .iter()
            .enumerate()
            .map(|(i, f)| {
                let h = f.as_ptr();
                match index.get(&h) {
                    Some(t) if matches!(f, xt_parser::entity::FieldVal::Ptr(_)) => format!("[{i}]→{}#{h}", t.type_id),
                    _ => format!("[{i}]={f:?}"),
                }
            })
            .collect();
        println!("BODY #{}: {}", b.index, fields.join(" "));
    }
    for r in ents.iter().filter(|e| e.type_id == 19) {
        println!("REGION #{}: type {:?} body {} next {} prev {} shell {}", r.index, ents.fields(r).get(6).or(ents.fields(r).get(2)), p(r, 2), p(r, 3), p(r, 4), p(r, 5));
        let fields: Vec<String> = ents.fields(r).iter().enumerate().map(|(i, f)| format!("[{i}]={f:?}")).collect();
        println!("   {}", fields.join(" "));
    }
    for s in ents.iter().filter(|e| e.type_id == 13) {
        // walk the face chain as the reader does
        let mut n = 0usize;
        let mut face = p(s, 4);
        let mut seen = rustc_hash::FxHashSet::default();
        while face != 0 && seen.insert(face) {
            let Some(fe) = index.get(&face).filter(|e| e.type_id == 14) else { break };
            n += 1;
            face = p(fe, 3);
        }
        let back = ents.iter().filter(|e| e.type_id == 14 && p(e, 6) == s.index).count();
        let fields: Vec<String> = ents.fields(s).iter().enumerate().map(|(i, f)| format!("[{i}]={f:?}")).collect();
        println!("SHELL #{}: chain {n} faces, {back} faces point back at it;  {}", s.index, fields.join(" "));
    }
}
