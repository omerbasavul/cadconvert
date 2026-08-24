//! Diagnose what the Solid Edge PS37 lowering losses actually are.
//!
//! `cargo run --release -p cad-xt --example xt_diag -- file.x_t`

use rustc_hash::FxHashMap;
use std::collections::BTreeMap;
use xt_parser::entity::RawEntity;

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let path = std::env::args().nth(1).expect("usage: xt_diag <file.x_t>");
    let bytes = std::fs::read(&path)?;
    let file = xt_parser::parse_raw(&String::from_utf8_lossy(&bytes))?;
    let index: FxHashMap<usize, &RawEntity> = file.entities.iter().map(|e| (e.index, e)).collect();
    let type_of = |h: usize| index.get(&h).map(|e| e.type_id);

    // 1. EDGE.curve field targets: what types do edges point at?
    println!("-- EDGE[6] (curve) target types --");
    let mut targets: BTreeMap<String, usize> = BTreeMap::new();
    for e in file.entities.iter().filter(|e| e.type_id == 16) {
        let p = file.entities.fields(e).get(6).map(|f| f.as_ptr()).unwrap_or(0);
        let key = match (p, type_of(p)) {
            (0, _) => "null".into(),
            (_, Some(t)) => format!("type {t}"),
            (_, None) => "NOT AN ENTITY".into(),
        };
        *targets.entry(key).or_default() += 1;
    }
    for (k, n) in &targets {
        println!("  {n:>7}  {k}");
    }

    // For edges pointing at nothing: dump all fields of a few and see whether
    // some OTHER field points at a curve.
    println!("\n-- sample EDGE whose [6] is not an entity --");
    for e in file
        .entities
        .iter()
        .filter(|e| {
            e.type_id == 16 && {
                let p = file.entities.fields(e).get(6).map(|f| f.as_ptr()).unwrap_or(0);
                p != 0 && type_of(p).is_none()
            }
        })
        .take(3)
    {
        print!("  edge #{}: ", e.index);
        for (i, f) in file.entities.fields(e).iter().enumerate() {
            let p = f.as_ptr();
            let t = type_of(p).map(|t| format!("→{t}")).unwrap_or_default();
            print!("[{i}]={p}{t} ");
        }
        println!();
    }

    // 2. GEOMETRIC_OWNER (141): field layout, what do its pointers reach?
    println!("\n-- GEOMETRIC_OWNER (141) samples --");
    for e in file.entities.iter().filter(|e| e.type_id == 141).take(3) {
        print!("  #{}: ", e.index);
        for (i, f) in file.entities.fields(e).iter().enumerate() {
            let p = f.as_ptr();
            let t = type_of(p).map(|t| format!("→{t}")).unwrap_or_default();
            print!("[{i}]={p}{t} ");
        }
        println!("  var_ptr={:?}", &file.entities.var_ptr(e)[..file.entities.var_ptr(e).len().min(6)]);
    }

    // 3. CHART (40): fixed fields + var lengths.
    println!("\n-- CHART (40) shape --");
    let mut lens: BTreeMap<(usize, usize), usize> = BTreeMap::new();
    for e in file.entities.iter().filter(|e| e.type_id == 40) {
        *lens.entry((file.entities.fields(e).len(), file.entities.var_f64(e).len().min(50) / 10 * 10)).or_default() += 1;
    }
    for ((nf, nv), n) in lens.iter().take(8) {
        println!("  {n:>6}  fixed={nf} var_f64≈{nv}");
    }
    if let Some(e) = file.entities.iter().find(|e| e.type_id == 40 && !file.entities.var_f64(e).is_empty()) {
        println!(
            "  sample #{}: fields={} var_f64.len={} first 8: {:?}",
            e.index,
            file.entities.fields(e).len(),
            file.entities.var_f64(e).len(),
            &file.entities.var_f64(e)[..file.entities.var_f64(e).len().min(8)]
        );
        print!("  fixed: ");
        for (i, f) in file.entities.fields(e).iter().enumerate() {
            print!("[{i}]={f:?} ");
        }
        println!();
    }

    // 4. NURBS_SURF periodic/closed flags.
    println!("\n-- NURBS_SURF flags --");
    let mut flags: BTreeMap<(bool, bool, bool, bool), usize> = BTreeMap::new();
    for e in file.entities.iter().filter(|e| e.type_id == 126) {
        let b = |i: usize| file.entities.fields(e).get(i).map(|f| f.as_bool()).unwrap_or(false);
        *flags.entry((b(0), b(1), b(11), b(12))).or_default() += 1;
    }
    for ((up, vp, uc, vc), n) in &flags {
        println!("  {n:>6}  u_per={up} v_per={vp} u_closed={uc} v_closed={vc}");
    }

    // 5. What surface types do faces reference that we don't lower?
    println!("\n-- FACE[7] surface types --");
    let mut st: BTreeMap<u16, usize> = BTreeMap::new();
    for e in file.entities.iter().filter(|e| e.type_id == 14) {
        let p = file.entities.fields(e).get(7).map(|f| f.as_ptr()).unwrap_or(0);
        if let Some(t) = type_of(p) {
            *st.entry(t).or_default() += 1;
        }
    }
    for (t, n) in &st {
        println!("  {n:>6}  type {t}");
    }

    // 5b. FIN pcurve target types (tolerant-edge geometry lives there).
    println!("\n-- FIN pcurve field target types (only fins whose edge has a null curve) --");
    let mut pt: BTreeMap<String, usize> = BTreeMap::new();
    for e in file.entities.iter().filter(|e| e.type_id == 17) {
        let a = if file.entities.fields(e).len() < 10 { 1 } else { 0 };
        let edge = file.entities.fields(e).get(6 - a).map(|f| f.as_ptr()).unwrap_or(0);
        let Some(ee) = index.get(&edge) else { continue };
        if file.entities.fields(ee).get(6).map(|f| f.as_ptr()).unwrap_or(0) != 0 {
            continue;
        }
        let p = file.entities.fields(e).get(7 - a).map(|f| f.as_ptr()).unwrap_or(0);
        let key = match (p, type_of(p)) {
            (0, _) => "null".into(),
            (_, Some(t)) => format!("type {t}"),
            (_, None) => "NOT AN ENTITY".into(),
        };
        *pt.entry(key).or_default() += 1;
    }
    for (k, n) in &pt {
        println!("  {n:>7}  {k}");
    }

    // 5c. TRIMMED_CURVE parameter fields: are [10]/[11] usable?
    println!("\n-- TRIMMED_CURVE (133) samples --");
    for e in file.entities.iter().filter(|e| e.type_id == 133).take(4) {
        print!("  #{}: ", e.index);
        for (i, f) in file.entities.fields(e).iter().enumerate() {
            match f {
                xt_parser::entity::FieldVal::Vec3(v) => {
                    print!("[{i}]=v({:.4},{:.4},{:.4}) ", v[0], v[1], v[2])
                }
                other => {
                    let p = other.as_ptr();
                    let t = type_of(p).map(|t| format!("→{t}")).unwrap_or_default();
                    print!("[{i}]={other:?}{t} ");
                }
            }
        }
        println!();
    }
    // Range degeneracy per basis type: how many trimmed curves have t0==t1?
    let mut degen: BTreeMap<String, (usize, usize)> = BTreeMap::new();
    for e in file.entities.iter().filter(|e| e.type_id == 133) {
        let basis = file.entities.fields(e).get(7).map(|f| f.as_ptr()).unwrap_or(0);
        let bt = type_of(basis).map(|t| t.to_string()).unwrap_or("?".into());
        let t0 = file.entities.fields(e).get(10).map(|f| f.as_f64()).unwrap_or(f64::NAN);
        let t1 = file.entities.fields(e).get(11).map(|f| f.as_f64()).unwrap_or(f64::NAN);
        let slot = degen.entry(bt).or_default();
        slot.1 += 1;
        if !(t1 - t0).abs().is_finite() || (t1 - t0).abs() < 1e-12 {
            slot.0 += 1;
        }
    }
    println!("  trimmed degenerate/total by basis type:");
    for (t, (d, n)) in &degen {
        println!("    basis type {t}: {d}/{n}");
    }

    // 5d. Forensics on one failing edge: geometry vs vertices.
    if let Ok(target) = std::env::var("XT_EDGE") {
        let h: usize = target.parse().unwrap();
        if let Some(ee) = index.get(&h) {
            println!("\n-- edge #{h} --");
            for (i, f) in file.entities.fields(ee).iter().enumerate() {
                let p = f.as_ptr();
                let t = type_of(p).map(|t| format!("→{t}")).unwrap_or_default();
                println!("  [{i}]={f:?}{t}");
            }
            // fins referencing this edge
            for fin in file.entities.iter().filter(|e| e.type_id == 17) {
                let a = if file.entities.fields(fin).len() < 10 { 1 } else { 0 };
                if file.entities.fields(fin).get(6 - a).map(|f| f.as_ptr()).unwrap_or(0) != h {
                    continue;
                }
                let vx = file.entities.fields(fin).get(4 - a).map(|f| f.as_ptr()).unwrap_or(0);
                let sense = file.entities.fields(fin).get(9 - a).map(|f| f.as_char()).unwrap_or('?');
                let pos = index
                    .get(&vx)
                    .and_then(|ve| file.entities.fields(ve).get(5).map(|f| f.as_ptr()))
                    .and_then(|pp| index.get(&pp))
                    .and_then(|pe| file.entities.fields(pe).get(5).map(|f| f.as_vec3()));
                println!(
                    "  fin #{} sense={sense} vertex #{vx} pos={pos:?} fwd→{}",
                    fin.index,
                    file.entities.fields(fin).get(2 - a).map(|f| f.as_ptr()).unwrap_or(0)
                );
            }
            // the curve
            let c = file.entities.fields(ee).get(6).map(|f| f.as_ptr()).unwrap_or(0);
            if let Some(ce) = index.get(&c) {
                println!("  curve #{c} type {}:", ce.type_id);
                for (i, f) in file.entities.fields(ce).iter().enumerate() {
                    println!("    [{i}]={f:?}");
                }
            }
        }
    }

    // 6. Assembly structure: ASSEMBLY (10) / INSTANCE (11) / TRANSFORM (100).
    println!("\n-- assembly --");
    for t in [10u16, 11, 100] {
        let n = file.entities.iter().filter(|e| e.type_id == t).count();
        println!("  type {t}: {n}");
    }
    for e in file.entities.iter().filter(|e| e.type_id == 11).take(3) {
        print!("  INSTANCE #{}: ", e.index);
        for (i, f) in file.entities.fields(e).iter().enumerate() {
            let p = f.as_ptr();
            let t = type_of(p).map(|t| format!("→{t}")).unwrap_or_default();
            print!("[{i}]={p}{t} ");
        }
        println!();
    }
    // 6b. Raw NURBS_SURF (126) and NURBS_CURVE (136) field dumps.
    for t in [126u16, 136] {
        if let Some(e) = file.entities.iter().find(|e| e.type_id == t) {
            println!("\n-- raw type {t} #{} --", e.index);
            for (i, f) in file.entities.fields(e).iter().enumerate() {
                let pt = f.as_ptr();
                let tt = type_of(pt).map(|t| format!("→{t}")).unwrap_or_default();
                println!("  [{i}]={f:?}{tt}");
            }
        }
    }

    // 6c. Raw TRANSFORM (100) dumps.
    for e in file.entities.iter().filter(|e| e.type_id == 100).take(3) {
        println!("\n-- raw TRANSFORM #{} --", e.index);
        for (i, f) in file.entities.fields(e).iter().enumerate() {
            println!("  [{i}]={f:?}");
        }
    }

    // 6d. Translation magnitudes across every TRANSFORM.
    {
        let mut mags: Vec<f64> = file
            .entities
            .iter()
            .filter(|e| e.type_id == 100)
            .map(|e| {
                let v = file.entities.fields(e).get(5).map(|f| f.as_vec3()).unwrap_or([0.0; 3]);
                (v[0] * v[0] + v[1] * v[1] + v[2] * v[2]).sqrt()
            })
            .collect();
        mags.sort_by(|a, b| a.partial_cmp(b).unwrap());
        println!(
            "\n-- transform |t| (m): min={:.4} median={:.4} max={:.4} --",
            mags.first().copied().unwrap_or(0.0),
            mags.get(mags.len() / 2).copied().unwrap_or(0.0),
            mags.last().copied().unwrap_or(0.0)
        );
        // Rotation orthonormality check: row lengths of each matrix.
        let mut worst = 0.0f64;
        for e in file.entities.iter().filter(|e| e.type_id == 100) {
            if let Some(m) = file.entities.fields(e).get(4).and_then(|f| f.as_mat3()) {
                for r in 0..3 {
                    let len = (m[r * 3] * m[r * 3]
                        + m[r * 3 + 1] * m[r * 3 + 1]
                        + m[r * 3 + 2] * m[r * 3 + 2])
                        .sqrt();
                    worst = worst.max((len - 1.0).abs());
                }
            }
        }
        println!("  worst row-length deviation: {worst:.3e}");
    }

    // 6e. Charts containing far coordinates.
    {
        let mut shown = 0;
        for e in file.entities.iter().filter(|e| e.type_id == 40) {
            let fixed = file.entities.fields(e).get(6).map(|f| f.as_vec3()).unwrap_or([0.0; 3]);
            let far_fixed = fixed.iter().any(|v| v.abs() > 100.0);
            let far_var = file.entities.var_f64(e).iter().any(|v| v.abs() > 100.0);
            if (far_fixed || far_var) && shown < 3 {
                shown += 1;
                println!("\n-- far CHART #{} fixed[6]={fixed:?} --", e.index);
                for (i, f) in file.entities.fields(e).iter().enumerate() {
                    println!("  [{i}]={f:?}");
                }
                println!("  var_f64[{}] first 12: {:?}", file.entities.var_f64(e).len(),
                         &file.entities.var_f64(e)[..file.entities.var_f64(e).len().min(12)]);
            }
        }
        if shown == 0 {
            println!("\n-- no charts with far coordinates --");
        }
    }

    // 6f. Chain dump: pcurve → SP_CURVE → surface + 2D spline.
    if let Ok(t) = std::env::var("XT_PCURVE") {
        let h: usize = t.parse().unwrap();
        let mut cur = h;
        for _ in 0..4 {
            let Some(e) = index.get(&cur) else { break };
            println!("\n-- chain #{} type {} --", cur, e.type_id);
            for (i, f) in file.entities.fields(e).iter().enumerate() {
                let pt = f.as_ptr();
                let tt = type_of(pt).map(|t| format!("→{t}")).unwrap_or_default();
                println!("  [{i}]={f:?}{tt}");
            }
            cur = match e.type_id {
                133 => file.entities.fields(e).get(7).map(|f| f.as_ptr()).unwrap_or(0),
                137 => {
                    // show surface AND the 2D spline's control points
                    let surf = file.entities.fields(e).get(7).map(|f| f.as_ptr()).unwrap_or(0);
                    let bc = file.entities.fields(e).get(8).map(|f| f.as_ptr()).unwrap_or(0);
                    if let Some(b) = index.get(&bc) {
                        let inner = if b.type_id == 134 {
                            file.entities.fields(b).get(7).map(|f| f.as_ptr()).unwrap_or(0)
                        } else { bc };
                        if let Some(n) = index.get(&inner) {
                            let vp = file.entities.fields(n).get(9).map(|f| f.as_ptr()).unwrap_or(0);
                            if let Some(v) = index.get(&vp) {
                                println!("  2D poles first 8: {:?}", &file.entities.var_f64(v)[..file.entities.var_f64(v).len().min(8)]);
                            }
                        }
                    }
                    surf
                }
                _ => 0,
            };
            if cur == 0 { break }
        }
    }

    // 6g. FIN field census: how many fields, and where does the VERTEX sit?
    {
        let mut widths: BTreeMap<usize, usize> = BTreeMap::new();
        let mut vertex_at: BTreeMap<usize, usize> = BTreeMap::new();
        let mut null_vertex_at4 = 0usize;
        for e in file.entities.iter().filter(|e| e.type_id == 17) {
            *widths.entry(file.entities.fields(e).len()).or_default() += 1;
            for (i, f) in file.entities.fields(e).iter().enumerate() {
                if type_of(f.as_ptr()) == Some(18) {
                    *vertex_at.entry(i).or_default() += 1;
                }
            }
            let a = if file.entities.fields(e).len() < 10 { 1 } else { 0 };
            if file.entities.fields(e).get(4 - a).map(|f| f.as_ptr()).unwrap_or(0) == 0 {
                null_vertex_at4 += 1;
            }
        }
        println!("\n-- FIN census --");
        println!("  field widths: {widths:?}");
        println!("  a VERTEX(18) is referenced from field index: {vertex_at:?}");
        println!("  fins whose field 4 (offset-adjusted) is null: {null_vertex_at4}");
    }

    // 7. Are the lowered spline surfaces where their boundaries are?
    // For each nurbs-surfaced face, invert its boundary chain points onto the
    // surface and measure the residual. A correct surface sits within model
    // tolerance; a transposed or garbled control grid sits millimetres off.
    if std::env::var_os("XT_NURBS_CHECK").is_some() {
        let (scene, _) = cad_xt::to_scene(
            &String::from_utf8_lossy(&std::fs::read(&path)?),
            &cad_xt::LowerOptions::default(),
        )?;
        let mut buckets = [0usize; 5]; // <0.01, <0.1, <1, <10, >=10 mm
        let mut checked = 0usize;
        for g in &scene.geometry {
            let Some(solid) = &g.brep else { continue };
            for f in &solid.faces {
                let cad_ir::Surface::Nurbs(_) = solid.surface(f.surface) else {
                    continue;
                };
                let surf = solid.surface(f.surface);
                let Some(b0) = f.bounds.first() else { continue };
                let Some(h0) = b0.halves.first() else { continue };
                let e = solid.edge(h0.edge);
                let c = solid.curve(e.curve);
                let pm = c.point_at(e.range.at(0.5));
                let uv = surf.invert(pm, None).unwrap_or_default();
                let d = (surf.point_at(uv) - pm).length();
                checked += 1;
                let slot = if d < 0.01 { 0 } else if d < 0.1 { 1 } else if d < 1.0 { 2 } else if d < 10.0 { 3 } else { 4 };
                buckets[slot] += 1;
                if checked <= 5 {
                    println!("  face residual {d:.4} mm");
                }
            }
        }
        // Raw fields of one good and one bad surface, to diff layouts.
        let mut shown_good = false;
        let mut shown_bad = false;
        for g in &scene.geometry {
            let Some(solid) = &g.brep else { continue };
            for f in &solid.faces {
                let cad_ir::Surface::Nurbs(n) = solid.surface(f.surface) else {
                    continue;
                };
                let surf = solid.surface(f.surface);
                let Some(b0) = f.bounds.first() else { continue };
                let Some(h0) = b0.halves.first() else { continue };
                let e = solid.edge(h0.edge);
                let pm = solid.curve(e.curve).point_at(e.range.at(0.5));
                let uv = surf.invert(pm, None).unwrap_or_default();
                let d = (surf.point_at(uv) - pm).length();
                let bad = d > 5.0;
                if (bad && shown_bad) || (!bad && shown_good) || (!bad && d > 0.001) {
                    continue;
                }
                println!(
                    "\n  {} spline: residual {d:.4} mm  u_deg={} v_deg={} grid {}x{} rational={} u_knots={} v_knots={}",
                    if bad { "BAD " } else { "GOOD" },
                    n.u_degree, n.v_degree,
                    n.control_points.len(),
                    n.control_points.first().map(|r| r.len()).unwrap_or(0),
                    !n.weights.is_empty(),
                    n.u_knots.len(), n.v_knots.len(),
                );
                if bad { shown_bad = true } else { shown_good = true }
            }
        }
        println!("\n-- spline boundary residuals ({checked} faces) --");
        println!("  <0.01mm: {}   <0.1: {}   <1: {}   <10: {}   >=10: {}",
                 buckets[0], buckets[1], buckets[2], buckets[3], buckets[4]);
    }

    Ok(())
}
