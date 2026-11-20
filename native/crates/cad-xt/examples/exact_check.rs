//! Do a file's own points agree with what its definitions compute to?
//!
//! `cargo run --release -p cad-xt --example exact_check -- file.x_t`
//!
//! For every intersection curve: how far its chart's points sit from the two
//! fields it is the meeting of — which is whether the definitions are being
//! read as the file means them — and whether the curve between those points
//! can be computed, to how many points, and how far it bows from the chart.

use cad_xt::exact::{self, Fields};
use rustc_hash::FxHashMap;
use std::collections::BTreeMap;
use std::time::Instant;
use xt_parser::entity::RawEntity;

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let path = std::env::args().nth(1).expect("usage: exact_check <file.x_t>");
    let file = xt_parser::parse_raw_owned(xt_parser::decode(std::fs::read(&path)?))?;
    let ents = &file.entities;
    let index: FxHashMap<usize, &RawEntity> = ents.iter().map(|e| (e.index, e)).collect();
    let fields = Fields::new(ents, &index);
    let type_of = |h: usize| index.get(&h).map(|e| e.type_id).unwrap_or(0);
    let chord: f64 = std::env::var("CHORD").ok().and_then(|v| v.parse().ok()).unwrap_or(1e-6);
    let verbose = std::env::var_os("VERBOSE").is_some();

    #[derive(Default)]
    struct Row {
        curves: usize,
        read: usize,
        on: usize,
        refined: usize,
        worst_off: f64,
        worst_bow: f64,
        points: usize,
        ms: f64,
    }
    let mut rows: BTreeMap<String, Row> = BTreeMap::new();
    if let Some(h) = std::env::var("MATES").ok().and_then(|v| v.parse::<usize>().ok()) {
        // One blend's two surfaces as they are lowered, and where its
        // centre line's own points sit against each.
        let e = index.get(&h).expect("no such blend");
        let p = |i: usize| ents.fields(e).get(i).map(|f| f.as_ptr()).unwrap_or(0);
        let spine = index.get(&p(10)).expect("no spine");
        let chart = index.get(&ents.fields(spine).get(8).map(|f| f.as_ptr()).unwrap_or(0)).expect("no chart");
        let mut pts: Vec<cad_ir::math::Vec3> = Vec::new();
        if let Some(f0) = ents.fields(chart).get(6).map(|f| f.as_vec3()) {
            pts.push(cad_ir::math::Vec3::new(f0[0], f0[1], f0[2]));
        }
        for q in ents.var_f64(chart).chunks_exact(3) {
            pts.push(cad_ir::math::Vec3::new(q[0], q[1], q[2]));
        }
        for i in [8usize, 9] {
            let Some(se) = index.get(&p(i)) else { continue };
            match cad_xt::geom::surface(&ents, se, &index) {
                Ok(surface) => {
                    let d = surface.domain();
                    println!("mate [{i}] #{} type {}: domain u [{:.5},{:.5}] v [{:.5},{:.5}] periods {:?} {:?}", se.index, se.type_id, d.u.lo, d.u.hi, d.v.lo, d.v.hi, d.u_period, d.v_period);
                    println!("   {:?}", format!("{surface:?}").chars().take(600).collect::<String>());
                    for q in pts.iter().step_by((pts.len() / 8).max(1)) {
                        match surface.invert(*q, None) {
                            Some(uv) => {
                                let foot = surface.point_at(uv);
                                println!("   centre [{:8.3},{:8.3},{:8.3}] mm -> uv ({:.5},{:.5}), foot [{:8.3},{:8.3},{:8.3}], {:.4} mm away", q.x * 1e3, q.y * 1e3, q.z * 1e3, uv.u, uv.v, foot.x * 1e3, foot.y * 1e3, foot.z * 1e3, (foot - *q).length() * 1e3);
                            }
                            None => println!("   centre does not invert"),
                        }
                    }
                }
                Err(why) => println!("mate [{i}] #{} type {}: {why}", se.index, se.type_id),
            }
        }
        return Ok(());
    }
    if std::env::var_os("SHELLS").is_some() {
        // Each body's regions and the shells each owns, with how many faces
        // name each shell as theirs.
        let p = |e: &xt_parser::entity::RawEntity, i: usize| ents.fields(e).get(i).map(|f| f.as_ptr()).unwrap_or(0);
        let mut faces_of: BTreeMap<usize, usize> = BTreeMap::new();
        for f in ents.iter().filter(|e| e.type_id == 14) {
            *faces_of.entry(p(f, 6)).or_default() += 1;
        }
        for body in ents.iter().filter(|e| e.type_id == 12) {
            let mut region = ents.fields(body).iter().map(|f| f.as_ptr()).find(|h| type_of(*h) == 19).unwrap_or(0);
            let direct: Vec<usize> = ents.fields(body).iter().map(|f| f.as_ptr()).filter(|h| type_of(*h) == 13).collect();
            println!("body #{}: shells named outright {:?}", body.index, direct);
            let mut n = 0;
            while let Some(re) = index.get(&region).filter(|e| e.type_id == 19) {
                let mut shell = p(re, 5);
                let (mut count, mut faces, mut first) = (0usize, 0usize, 0usize);
                let mut seen = std::collections::HashSet::new();
                while let Some(se) = index.get(&shell).filter(|e| e.type_id == 13) {
                    if !seen.insert(shell) { break; }
                    if count == 0 { first = faces_of.get(&shell).copied().unwrap_or(0); }
                    count += 1;
                    faces += faces_of.get(&shell).copied().unwrap_or(0);
                    shell = p(se, 3);
                }
                println!("   region {n} #{} {:?}: {count} shell(s), {faces} faces in all, {first} in the first", re.index, ents.fields(re).iter().take(8).map(|f| format!("{f:?}")).collect::<Vec<_>>().join(" "));
                n += 1;
                region = p(re, 3);
                if n > 8 { println!("   ..."); break; }
            }
        }
        return Ok(());
    }
    if std::env::var_os("LIMITS").is_some() {
        // What kinds of end the file's intersection curves have, and how far
        // each kind's own point is from the end of the chart it ends.
        let mut kinds: BTreeMap<String, (usize, f64, f64)> = BTreeMap::new();
        for c in ents.iter().filter(|e| e.type_id == 38) {
            let chart = index.get(&ents.fields(c).get(8).map(|f| f.as_ptr()).unwrap_or(0));
            let mut pts: Vec<cad_ir::math::Vec3> = Vec::new();
            if let Some(ch) = chart {
                if let Some(f0) = ents.fields(ch).get(6).map(|f| f.as_vec3()) {
                    pts.push(cad_ir::math::Vec3::new(f0[0], f0[1], f0[2]));
                }
                for q in ents.var_f64(ch).chunks_exact(3) {
                    pts.push(cad_ir::math::Vec3::new(q[0], q[1], q[2]));
                }
            }
            for (which, i) in [("start", 9usize), ("end", 10usize)] {
                let Some(l) = index.get(&ents.fields(c).get(i).map(|f| f.as_ptr()).unwrap_or(0)) else { continue };
                let kind = ents.fields(l).first().map(|f| f.as_char()).unwrap_or('?');
                let term = ents.fields(l).get(1).map(|f| f.as_char()).unwrap_or('?');
                let own = ents.fields(l).get(2).map(|f| f.as_vec3()).map(|v| cad_ir::math::Vec3::new(v[0], v[1], v[2]));
                let end = if which == "start" { pts.first() } else { pts.last() };
                let gap = match (own, end) {
                    (Some(a), Some(b)) => (a - *b).length(),
                    _ => f64::NAN,
                };
                let tail = ents.var_f64(l).len() / 3;
                let entry = kinds.entry(format!("{kind}{term} with {tail} more point(s)")).or_insert((0, 0.0, f64::INFINITY));
                entry.0 += 1;
                if gap.is_finite() {
                    entry.1 = entry.1.max(gap);
                    entry.2 = entry.2.min(gap);
                }
            }
        }
        for (k, (n, far, near)) in &kinds {
            println!("{n:>6}  limit {k}: its point is {near:.3e} to {far:.3e} from the chart's end");
        }
        return Ok(());
    }
    if let Ok(at) = std::env::var("VERTEX_AT") {
        // The vertices at a place (in millimetres), with what the file says
        // about how exactly they are there.
        let v: Vec<f64> = at.split(',').filter_map(|t| t.trim().parse::<f64>().ok()).collect();
        let x = cad_ir::math::Vec3::new(v[0] * 1e-3, v[1] * 1e-3, v[2] * 1e-3);
        for pe in ents.iter().filter(|e| e.type_id == 29) {
            let Some(q) = ents.fields(pe).iter().find_map(|f| match f {
                xt_parser::entity::FieldVal::Vec3(q) => Some(**q),
                _ => None,
            }) else { continue };
            let q = cad_ir::math::Vec3::new(q[0], q[1], q[2]);
            if (q - x).length() > 2e-6 {
                continue;
            }
            println!("point #{} at {:?}", pe.index, q);
            for ve in ents.iter().filter(|e| e.type_id == 18) {
                if ents.fields(ve).iter().any(|f| matches!(f, xt_parser::entity::FieldVal::Ptr(_)) && f.as_ptr() == pe.index) {
                    println!("  vertex #{}: {:?}", ve.index, ents.fields(ve));
                }
            }
        }
        return Ok(());
    }
    if let Some(list) = std::env::var("RAW").ok() {
        for h in list.split(',').filter_map(|v| v.trim().parse::<usize>().ok()) {
            let Some(e) = index.get(&h) else { continue };
            let fields: Vec<String> = ents
                .fields(e)
                .iter()
                .enumerate()
                .map(|(i, f)| {
                    let t = type_of(f.as_ptr());
                    if matches!(f, xt_parser::entity::FieldVal::Ptr(_)) && t != 0 {
                        format!("[{i}]→{t}#{}", f.as_ptr())
                    } else {
                        format!("[{i}]={f:?}")
                    }
                })
                .collect();
            println!("#{h} type {}: {} extra {:?} tail {:?}", e.type_id, fields.join(" "), ents.extra(e), &ents.var_f64(e)[..ents.var_f64(e).len().min(9)]);
            if std::env::var_os("RAW_TAIL").is_some() {
                for (i, c) in ents.var_f64(e).chunks(3).enumerate() {
                    println!("   {i:4}: {:?}", c.iter().map(|v| (v * 1e6).round() / 1e3).collect::<Vec<_>>());
                }
            }
        }
        return Ok(());
    }
    if let Some(h) = std::env::var("SECTIONS").ok().and_then(|v| v.parse::<usize>().ok()) {
        // One blend's sections along its centre line: how wide each opens and
        // which way round, and where given points sit in theirs.
        let Some(blend) = fields.blend(h, &[]) else {
            println!("blend #{h}: definition not read");
            return Ok(());
        };
        let Some(line) = blend.centre_line() else {
            println!("blend #{h}: no centre line; radius {:.5}, ra {:.5} rb {:.5}; its own points read, each from nothing and then each from the last:", blend.radius, blend.ra, blend.rb);
            for following in [false, true] {
                blend.a.forget();
                blend.b.forget();
                for q in &blend.spine {
                    if !following {
                        blend.a.forget();
                        blend.b.forget();
                    }
                    let a = blend.a.read_near(*q, blend.ra).map(|r| r.distance - blend.ra);
                    let b = blend.b.read_near(*q, blend.rb).map(|r| r.distance - blend.rb);
                    println!("   [{:8.3},{:8.3},{:8.3}] mm: off a {:?}, off b {:?}", q.x * 1e3, q.y * 1e3, q.z * 1e3, a.map(|v| format!("{v:.2e}")), b.map(|v| format!("{v:.2e}")));
                }
                println!("   --");
            }
            return Ok(());
        };
        println!("blend #{h}: radius {:.5}, ra {:.5} rb {:.5}, {} stations", blend.radius, blend.ra, blend.rb, line.len());
        let about = |along: cad_ir::math::Vec3, from: cad_ir::math::Vec3, to: cad_ir::math::Vec3| from.cross(to).dot(along).atan2(from.dot(to)).to_degrees();
        let every = (line.len() / 24).max(1);
        let mut s = 0.0;
        for (i, st) in line.iter().enumerate() {
            if i > 0 {
                s += (st.at - line[i - 1].at).length();
            }
            let window: Option<(f64, f64)> = std::env::var("WINDOW").ok().and_then(|w| {
                let mut it = w.split(',').filter_map(|t| t.trim().parse::<f64>().ok());
                Some((it.next()?, it.next()?))
            });
            match window {
                Some((lo, hi)) => {
                    if s * 1e3 < lo || s * 1e3 > hi {
                        continue;
                    }
                }
                None => {
                    if i % every != 0 && i + 1 != line.len() {
                        continue;
                    }
                }
            }
            let Some((pa, pb)) = blend.contacts(st.at) else {
                println!("  s {s:.5}: no contacts");
                continue;
            };
            let (ua, ub) = ((pa - st.at) * (1.0 / blend.radius), (pb - st.at) * (1.0 / blend.radius));
            println!(
                "  s {:8.3} mm at [{:8.3},{:8.3},{:8.3}] heading [{:6.3},{:6.3},{:6.3}]: second contact {:8.2} degrees round; contacts [{:8.3},{:8.3},{:8.3}] [{:8.3},{:8.3},{:8.3}]",
                s * 1e3, st.at.x * 1e3, st.at.y * 1e3, st.at.z * 1e3, st.along.x, st.along.y, st.along.z, about(st.along, ua, ub),
                pa.x * 1e3, pa.y * 1e3, pa.z * 1e3, pb.x * 1e3, pb.y * 1e3, pb.z * 1e3
            );
        }
        if let Ok(points) = std::env::var("POINTS") {
            let first = line[0].at;
            for text in points.split(';') {
                let v: Vec<f64> = text.split(',').filter_map(|t| t.trim().parse::<f64>().ok()).collect();
                if v.len() != 3 {
                    continue;
                }
                let x = cad_ir::math::Vec3::new(v[0] * 1e-3, v[1] * 1e-3, v[2] * 1e-3);
                let Some((c, along)) = blend.centre_for(x) else {
                    println!("  point {text}: no section through it");
                    continue;
                };
                // The line's own direction of travel, to compare with above.
                let nearest = line.iter().min_by(|a, b| (a.at - c).length().total_cmp(&(b.at - c).length())).unwrap();
                let along = if along.dot(nearest.along) < 0.0 { -along } else { along };
                let Some((pa, pb)) = blend.contacts(c) else { continue };
                let (ua, ub) = ((pa - c) * (1.0 / blend.radius), (pb - c) * (1.0 / blend.radius));
                let out = x - c;
                println!(
                    "  point {text}: section {:.3} mm from the line's start, {:.4} radii out, {:8.2} degrees round from the first contact (second at {:8.2}); a {:.2e} b {:.2e}",
                    (c - first).length() * 1e3,
                    out.length() / blend.radius,
                    about(along, ua, out * (1.0 / out.length())),
                    about(along, ua, ub),
                    blend.a.read(x).map(|r| r.distance).unwrap_or(f64::NAN),
                    blend.b.read(x).map(|r| r.distance).unwrap_or(f64::NAN),
                );
            }
        }
        return Ok(());
    }
    if std::env::var_os("SURFACES").is_some() {
        // Every blend a face lies on, built as a surface and then asked, at
        // points it was not built from, whether it is the blend.
        let (mut built, mut failed, mut worst_all, mut rows_all) = (0usize, 0usize, 0.0f64, 0usize);
        let t_all = Instant::now();
        for e in ents.iter().filter(|e| e.type_id == 56) {
            let faces = ents.iter().filter(|f| f.type_id == 14 && ents.fields(f).get(7).map(|x| x.as_ptr()) == Some(e.index)).count();
            if faces == 0 {
                continue;
            }
            if std::env::var("BLEND").ok().and_then(|v| v.parse::<usize>().ok()).is_some_and(|h| h != e.index) {
                continue;
            }
            let t = Instant::now();
            let Some(blend) = fields.blend(e.index, &[]) else {
                failed += 1;
                println!("blend #{}: {faces} faces, definition not read", e.index);
                continue;
            };
            let Some(n) = blend.surface(&[]) else {
                failed += 1;
                println!("blend #{}: {faces} faces, radius {:.5}, surface did not build ({:.1} s)", e.index, blend.radius, t.elapsed().as_secs_f64());
                continue;
            };
            built += 1;
            let rows = n.control_points.len();
            rows_all += rows;
            let worst = blend.departure(&[], 48, 6).unwrap_or(f64::INFINITY);
            worst_all = worst_all.max(worst);
            if verbose || worst > 1e-7 {
                println!(
                    "blend #{}: {faces} faces, radius {:.5}, {rows} rows; worst departure {worst:.2e} ({:.1} s)",
                    e.index, blend.radius, t.elapsed().as_secs_f64()
                );
            }
        }
        println!("blends with faces: {built} built, {failed} not; {rows_all} rows of poles; worst departure {worst_all:.2e} m; {:.1} s", t_all.elapsed().as_secs_f64());
        return Ok(());
    }
    if std::env::var_os("CLIFFS").is_some() {
        let mut shown = 0;
        let mut faces_on = 0usize;
        for e in ents.iter().filter(|e| e.type_id == 56) {
            if ents.fields(e).get(7).map(|f| f.as_char()) != Some('E') {
                continue;
            }
            let used = ents.iter().filter(|f| f.type_id == 14 && ents.fields(f).get(7).map(|x| x.as_ptr()) == Some(e.index)).count();
            faces_on += used;
            if shown < 6 {
                shown += 1;
                let fields: Vec<String> = ents
                    .fields(e)
                    .iter()
                    .enumerate()
                    .skip(6)
                    .map(|(i, f)| {
                        let t = type_of(f.as_ptr());
                        if matches!(f, xt_parser::entity::FieldVal::Ptr(_)) && t != 0 { format!("[{i}]→{t}#{}", f.as_ptr()) } else { format!("[{i}]={f:?}") }
                    })
                    .collect();
                println!("cliff #{} on {used} faces: {}", e.index, fields.join(" "));
            }
        }
        println!("faces on cliff blends: {faces_on}");
        return Ok(());
    }
    if std::env::var_os("BLENDS").is_some() {
        // Every blend the file has, and for one that will not read, why.
        let mut why: BTreeMap<String, usize> = BTreeMap::new();
        let mut shown = 0;
        for e in ents.iter().filter(|e| e.type_id == 56) {
            if fields.blend(e.index, &[]).is_some() {
                *why.entry("read".into()).or_default() += 1;
                continue;
            }
            let p = |i: usize| ents.fields(e).get(i).map(|f| f.as_ptr()).unwrap_or(0);
            let r = |i: usize| ents.fields(e).get(i).map(|f| f.as_f64()).unwrap_or(f64::NAN);
            let kind = ents.fields(e).get(7).map(|f| f.as_char()).unwrap_or('?');
            let (ra, rb) = (r(11), r(12));
            let spine = index.get(&p(10));
            let chart = spine
                .and_then(|s| index.get(&ents.fields(s).get(8).map(|f| f.as_ptr()).unwrap_or(0)))
                .filter(|c| c.type_id == 40);
            let reason = if !matches!(kind, 'R' | 'E') {
                format!("type {kind:?}")
            } else if spine.is_some_and(|s| s.type_id == 30) {
                "straight centre line: how far it runs comes from its faces".to_string()
            } else if (ra.abs() - rb.abs()).abs() > ra.abs() * 1e-9 {
                "two different ranges".to_string()
            } else if spine.is_none() {
                "no spine".to_string()
            } else {
                let _ = chart;
                let (a, b) = (fields.of(p(8), &[], 0.0), fields.of(p(9), &[], 0.0));
                match (a.is_some(), b.is_some()) {
                    (true, true) => format!("the centre line is not the radius from both surfaces (type {kind:?}, spine type {})", spine.map(|s| s.type_id).unwrap_or(0)),
                    _ => format!(
                        "a blended surface does not read: types {} {} ({} {})",
                        type_of(p(8)), type_of(p(9)), a.is_some(), b.is_some()
                    ),
                }
            };
            if shown < 12 && reason.starts_with("the centre") {
                shown += 1;
                // how far off
                if let (Some(a), Some(b), Some(c)) = (fields.of(p(8), &[], 0.0), fields.of(p(9), &[], 0.0), chart) {
                    let mut pts: Vec<cad_ir::math::Vec3> = Vec::new();
                    if let Some(f0) = ents.fields(c).get(6).map(|f| f.as_vec3()) {
                        pts.push(cad_ir::math::Vec3::new(f0[0], f0[1], f0[2]));
                    }
                    for q in ents.var_f64(c).chunks_exact(3) {
                        pts.push(cad_ir::math::Vec3::new(q[0], q[1], q[2]));
                    }
                    let da: Vec<String> = pts.iter().map(|q| a.read(*q).map(|r| format!("{:.6}", r.distance)).unwrap_or("-".into())).collect();
                    let db: Vec<String> = pts.iter().map(|q| b.read(*q).map(|r| format!("{:.6}", r.distance)).unwrap_or("-".into())).collect();
                    println!("blend #{} ranges {ra:.6} {rb:.6} mates {} {}: centre line off a [{}] off b [{}]", e.index, type_of(p(8)), type_of(p(9)), da.join(" "), db.join(" "));
                }
            }
            *why.entry(reason).or_default() += 1;
        }
        for (k, n) in &why {
            println!("{n:>6}  {k}");
        }
        return Ok(());
    }
    let one: Option<usize> = std::env::var("CURVE").ok().and_then(|v| v.parse().ok());
    if let Some(h) = one {
        let e = index.get(&h).expect("no such curve");
        let s0 = ents.fields(e).get(7).map(|f| f.as_ptr()).unwrap_or(0);
        let s1 = ents.extra(e).first().map(|f| f.as_ptr()).unwrap_or(0);
        println!("curve #{h}: surfaces #{s0} (type {}) and #{s1} (type {})", type_of(s0), type_of(s1));
        for s in [s0, s1] {
            let b = match type_of(s) {
                56 => fields.blend(s, &[]),
                59 => fields.blend(index.get(&s).and_then(|e| ents.fields(e).get(8)).map(|f| f.as_ptr()).unwrap_or(0), &[]),
                _ => None,
            };
            if let Some(b) = b {
                println!("  blend behind #{s}: radius {:.5} ra {:+.5} rb {:+.5} spine {} points", b.radius, b.ra, b.rb, b.spine.len());
                for (i, c) in b.spine.iter().enumerate() {
                    let (ra, rb) = (b.a.read(*c).map(|r| r.distance), b.b.read(*c).map(|r| r.distance));
                    println!("    spine {i}: [{:.6},{:.6},{:.6}] off a {:?} off b {:?}", c.x, c.y, c.z, ra, rb);
                }
            }
        }
        let Some((chart, f, g, _)) = fields.intersection(h) else {
            println!("  fields not read");
            return Ok(());
        };
        // Where each chart point lands on the base of an offset, and how.
        let bases: Vec<(usize, cad_ir::brep::Surface)> = [s0, s1]
            .iter()
            .filter(|s| type_of(**s) == 60)
            .filter_map(|s| {
                let base = index.get(s).and_then(|e| ents.fields(e).get(9)).map(|f| f.as_ptr())?;
                let lowered = cad_xt::geom::surface(ents, index.get(&base)?, &index).ok()?;
                Some((base, lowered))
            })
            .collect();
        for l in [9usize, 10] {
            if let Some(le) = index.get(&ents.fields(e).get(l).map(|f| f.as_ptr()).unwrap_or(0)) {
                println!("  limit [{l}] #{}: {:?} tail {:?}", le.index, ents.fields(le), ents.var_f64(le));
            }
        }
        println!("  chart off the fields, read as the curve is: {:?}", exact::chart_off(&f, &g, &chart));
        if let Some(seg) = std::env::var("WALK").ok().and_then(|v| v.parse::<usize>().ok()) {
            let (a, b) = (chart[seg], chart[seg + 1]);
            f.forget();
            g.forget();
            println!("  walking {seg} -> {}: start reads {:?} {:?}", seg + 1, f.read(a).map(|r| r.distance), g.read(a).map(|r| r.distance));
            let n = 44;
            for k in 1..=n {
                let y = a + (b - a) * (k as f64 / n as f64);
                println!("    {k:>2}/{n}: {:?} {:?}", f.read(y).map(|r| r.distance), g.read(y).map(|r| r.distance));
            }
        }
        for (i, w) in chart.windows(2).enumerate() {
            let got = exact::stations(&f, &g, w);
            println!(
                "  span {i}..{}: {:.3} mm apart: {}",
                i + 1,
                (w[1] - w[0]).length() * 1e3,
                match &got {
                    Some(st) => format!("{} stations", st.len()),
                    None => "did not compute".to_string(),
                }
            );
            if got.is_none() {
                // Where along the span the fields stop agreeing.
                let along = (w[1] - w[0]).try_normalized().unwrap();
                for k in 1..8 {
                    let x0 = w[0] + (w[1] - w[0]) * (k as f64 / 8.0);
                    f.forget();
                    g.forget();
                    let _ = (f.read(w[0]), g.read(w[0]));
                    let met = exact::meet(&f, &g, x0, along, (w[1] - w[0]).length());
                    println!(
                        "      at {k}/8: fields read {:?} {:?}; meet {}",
                        f.read(x0).map(|r| r.distance),
                        g.read(x0).map(|r| r.distance),
                        match met {
                            Some(x) => format!("found {:.4} mm off the chord", (x - x0).length() * 1e3),
                            None => "failed".to_string(),
                        }
                    );
                }
            }
        }
        for (i, p) in chart.iter().enumerate() {
            println!("  chart {i}: [{:.6},{:.6},{:.6}]", p.x, p.y, p.z);
            for (h, base) in &bases {
                if let Some(uv) = base.nearest(*p, None) {
                    let q = base.point_at(uv);
                    let n = base.normal_at(uv);
                    println!(
                        "     base #{h}: nearest uv ({:.6},{:.6}) gap {:.6} along normal {:.6}",
                        uv.u, uv.v, (*p - q).length(), (*p - q).dot(n)
                    );
                }
            }
            println!("     first {:?}", f.read(*p).map(|r| r.distance));
            println!("     second {:?}", g.read(*p).map(|r| r.distance));
        }
        return Ok(());
    }
    for e in ents.iter().filter(|e| e.type_id == 38) {
        let s0 = ents.fields(e).get(7).map(|f| f.as_ptr()).unwrap_or(0);
        let s1 = ents.extra(e).first().map(|f| f.as_ptr()).unwrap_or(0);
        let key = format!("{} x {}", type_of(s0), type_of(s1));
        let row = rows.entry(key.clone()).or_default();
        row.curves += 1;
        let t = Instant::now();
        let Some((chart, f, g, _)) = fields.intersection(e.index) else {
            if verbose {
                println!("curve #{} {key}: fields not read", e.index);
            }
            continue;
        };
        row.read += 1;
        let off = exact::chart_off(&f, &g, &chart).unwrap_or(f64::INFINITY);
        row.worst_off = row.worst_off.max(if off.is_finite() { off } else { 9.0 });
        if off > exact::RESOLUTION {
            if verbose {
                let each = |h: &exact::Field| {
                    chart
                        .iter()
                        .map(|p| h.read(*p).map(|r| r.distance.abs()).unwrap_or(f64::INFINITY))
                        .fold(0.0f64, f64::max)
                };
                println!(
                    "curve #{} {key} (#{s0} x #{s1}): chart points off the first field by {:.3e}, off the second by {:.3e}",
                    e.index,
                    each(&f),
                    each(&g)
                );
            }
            continue;
        }
        row.on += 1;
        let _ = chord;
        match exact::stations(&f, &g, &chart) {
            Some(stations) => {
                row.refined += 1;
                row.points += stations.len();
                // The furthest any computed point stands from the chart's own
                // chords: how much the chart was hiding.
                let bow = stations
                    .iter()
                    .map(|p| {
                        chart
                            .windows(2)
                            .map(|w| {
                                let s = w[1] - w[0];
                                let l2 = s.length_squared();
                                let t = if l2 > 0.0 { ((p.at - w[0]).dot(s) / l2).clamp(0.0, 1.0) } else { 0.0 };
                                (p.at - (w[0] + s * t)).length()
                            })
                            .fold(f64::INFINITY, f64::min)
                    })
                    .fold(0.0f64, f64::max);
                row.worst_bow = row.worst_bow.max(bow);
                if verbose && t.elapsed().as_secs_f64() > 1.0 {
                    println!("curve #{} {key}: {} stations from {} chart points in {:.1} s", e.index, stations.len(), chart.len(), t.elapsed().as_secs_f64());
                }
            }
            None => {
                if verbose {
                    println!("curve #{} {key}: {} chart points on the fields, but the curve between them did not compute", e.index, chart.len());
                }
            }
        }
        row.ms += t.elapsed().as_secs_f64() * 1e3;
    }
    println!("{:<10} {:>6} {:>6} {:>6} {:>8} {:>12} {:>10} {:>9} {:>8}", "pair", "curves", "read", "on", "computed", "worst off", "bow mm", "points", "ms");
    let mut total = Row::default();
    for (k, r) in &rows {
        println!(
            "{k:<10} {:>6} {:>6} {:>6} {:>8} {:>12.2e} {:>10.3} {:>9} {:>8.0}",
            r.curves, r.read, r.on, r.refined, r.worst_off, r.worst_bow * 1e3, r.points, r.ms
        );
        total.curves += r.curves;
        total.read += r.read;
        total.on += r.on;
        total.refined += r.refined;
        total.points += r.points;
        total.ms += r.ms;
    }
    println!(
        "{:<10} {:>6} {:>6} {:>6} {:>8} {:>12} {:>10} {:>9} {:>8.0}",
        "all", total.curves, total.read, total.on, total.refined, "", "", total.points, total.ms
    );
    Ok(())
}
