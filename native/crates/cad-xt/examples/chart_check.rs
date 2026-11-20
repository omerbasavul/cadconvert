//! Are a file's intersection charts on the surfaces they are the meeting of?
//!
//! `cargo run --release -p cad-xt --example chart_check -- file.x_t`
//!
//! An INTERSECTION is written twice: as two surfaces, and as a chart of
//! sampled points with a stated chordal error. This measures the chart against
//! the surfaces — how far its points sit off each, and how far the true curve
//! bows from the chord between two neighbouring points — for every curve whose
//! two surfaces lower.

use cad_ir::brep::Surface;
use cad_ir::math::{Vec2, Vec3};
use rustc_hash::FxHashMap;
use std::collections::BTreeMap;
use xt_parser::entity::RawEntity;

fn foot(s: &Surface, x: Vec3, hint: Option<Vec2>) -> Option<(Vec3, Vec3, Vec2)> {
    let uv = s.invert(x, hint)?;
    Some((s.point_at(uv), s.normal_at(uv), uv))
}

/// The point of both surfaces in the plane through `x0` square to `t`.
fn project(a: &Surface, b: &Surface, x0: Vec3, t: Vec3) -> Option<(Vec3, f64)> {
    let mut x = x0;
    let (mut ha, mut hb) = (None, None);
    for _ in 0..40 {
        let (pa, na, ua) = foot(a, x, ha)?;
        let (pb, nb, ub) = foot(b, x, hb)?;
        ha = Some(ua);
        hb = Some(ub);
        let (da, db, dt) = ((x - pa).dot(na), (x - pb).dot(nb), (x - x0).dot(t));
        let det = na.dot(nb.cross(t));
        if det.abs() < 1e-9 {
            return None;
        }
        let step = (nb.cross(t) * -da + t.cross(na) * -db + na.cross(nb) * -dt) * (1.0 / det);
        x = x + step;
        if step.length() < 1e-11 {
            break;
        }
    }
    let (pa, _, _) = foot(a, x, ha)?;
    let (pb, _, _) = foot(b, x, hb)?;
    Some((x, (x - pa).length().max((x - pb).length())))
}

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let path = std::env::args().nth(1).expect("usage: chart_check <file.x_t>");
    let file = xt_parser::parse_raw_owned(xt_parser::decode(std::fs::read(&path)?))?;
    let ents = &file.entities;
    let index: FxHashMap<usize, &RawEntity> = ents.iter().map(|e| (e.index, e)).collect();
    let p = |e: &RawEntity, i: usize| ents.fields(e).get(i).map(|f| f.as_ptr()).unwrap_or(0);
    let f = |e: &RawEntity, i: usize| ents.fields(e).get(i).map(|f| f.as_f64()).unwrap_or(f64::NAN);

    let mut surfaces: FxHashMap<usize, Option<Surface>> = FxHashMap::default();
    let mut lower = |h: usize| -> Option<Surface> {
        surfaces
            .entry(h)
            .or_insert_with(|| index.get(&h).and_then(|e| cad_xt::geom::surface(ents, e, &index).ok()))
            .clone()
    };

    let mut by_pair: BTreeMap<String, (usize, usize)> = BTreeMap::new();
    let (mut worst_on, mut worst_bow, mut failed, mut solved) = (0.0f64, 0.0f64, 0usize, 0usize);
    let limit_types = |h: usize| -> String {
        index
            .get(&h)
            .map(|l| {
                format!(
                    "{}x{}",
                    ents.fields(l).first().map(|f| f.as_char()).unwrap_or('?'),
                    1 + ents.var_f64(l).len() / 3
                )
            })
            .unwrap_or_else(|| "-".into())
    };
    for ie in ents.iter().filter(|e| e.type_id == 38) {
        let (s0, s1) = (p(ie, 7), ents.extra(ie).first().map(|f| f.as_ptr()).unwrap_or(0));
        let (t0, t1) = (
            index.get(&s0).map(|e| e.type_id).unwrap_or(0),
            index.get(&s1).map(|e| e.type_id).unwrap_or(0),
        );
        let key = format!("{t0} x {t1}");
        let slot = by_pair.entry(key.clone()).or_default();
        slot.0 += 1;
        let Some(chart) = index.get(&p(ie, 8)) else { continue };
        let mut pts: Vec<Vec3> = Vec::new();
        if let Some(first) = ents.fields(chart).get(6).map(|f| f.as_vec3()) {
            pts.push(Vec3::new(first[0], first[1], first[2]));
        }
        for c in ents.var_f64(chart).chunks_exact(3) {
            pts.push(Vec3::new(c[0], c[1], c[2]));
        }
        let (Some(a), Some(b)) = (lower(s0), lower(s1)) else { continue };
        slot.1 += 1;
        let on = |s: &Surface, q: Vec3| foot(s, q, None).map(|(pp, _, _)| (pp - q).length()).unwrap_or(f64::NAN);
        if std::env::var_os("CHART_BRUTE").is_some() {
            for (which, (s, h)) in [(&a, s0), (&b, s1)].iter().enumerate() {
                let ty = index.get(h).map(|e| e.type_id).unwrap_or(0);
                if matches!(ty, 56 | 59 | 60) {
                    continue;
                }
                let mut shown = 0usize;
                for (k, q) in pts.iter().enumerate() {
                    let d = on(s, *q);
                    if !(d > 1e-6) || shown >= 3 {
                        continue;
                    }
                    shown += 1;
                    // Brute force: sample the whole domain finely, then polish.
                    let dom = s.domain();
                    let (nu, nv) = (240usize, 60usize);
                    let mut best = (f64::INFINITY, Vec2::new(0.0, 0.0));
                    for i in 0..=nu {
                        for j in 0..=nv {
                            let uv = Vec2::new(dom.u.at(i as f64 / nu as f64), dom.v.at(j as f64 / nv as f64));
                            let dd = (s.point_at(uv) - *q).length();
                            if dd < best.0 {
                                best = (dd, uv);
                            }
                        }
                    }
                    let polished = s
                        .invert(*q, Some(best.1))
                        .map(|uv| ((s.point_at(uv) - *q).length(), uv))
                        .unwrap_or((f64::NAN, best.1));
                    let inv = s.invert(*q, None).unwrap_or_default();
                    println!(
                        "  brute curve #{} side {which} surface #{h} type {ty} point {k}/{} [{:.5},{:.5},{:.5}]: invert {:.3e} at ({:.5},{:.5})  brute {:.3e} at ({:.5},{:.5})  polished {:.3e} at ({:.5},{:.5})",
                        ie.index, pts.len(), q.x, q.y, q.z, d, inv.u, inv.v, best.0, best.1.u, best.1.v, polished.0, polished.1.u, polished.1.v
                    );
                }
            }
        }
        let off_a = pts.iter().map(|q| on(&a, *q)).fold(0.0f64, f64::max);
        let off_b = pts.iter().map(|q| on(&b, *q)).fold(0.0f64, f64::max);
        let (mut bow, mut bad, mut resid) = (0.0f64, 0usize, 0.0f64);
        let solve = std::env::var_os("CHART_NO_SOLVE").is_none();
        for w in pts.windows(2).filter(|_| solve) {
            let mid = (w[0] + w[1]) * 0.5;
            let Some(t) = (w[1] - w[0]).try_normalized() else { continue };
            match project(&a, &b, mid, t) {
                Some((x, r)) if r < 1e-7 => {
                    bow = bow.max((x - mid).length());
                    resid = resid.max(r);
                    solved += 1;
                }
                _ => {
                    bad += 1;
                    failed += 1;
                }
            }
        }
        worst_on = worst_on.max(off_a.max(off_b));
        worst_bow = worst_bow.max(bow);
        println!(
            "curve #{:<6} {key:<10} s0=#{s0} s1=#{s1} chart n={:<3} stated chord {:.6}  points off A {:.2e} off B {:.2e}  true bow {:.6}  mid-solves failed {bad}/{}  limits {} {}",
            ie.index,
            pts.len(),
            f(chart, 3),
            off_a,
            off_b,
            bow,
            pts.len().saturating_sub(1),
            limit_types(p(ie, 9)),
            limit_types(p(ie, 10)),
        );
    }
    println!("pairs (curves, both surfaces lower): {by_pair:?}");
    for e in ents.iter().filter(|e| e.type_id == 68 || e.type_id == 67) {
        let prof = p(e, 7);
        let fields: Vec<String> = ents.fields(e).iter().enumerate().skip(6).map(|(i, f)| format!("[{i}]={f:?}")).collect();
        println!("spun/swept #{} type {}: profile type {} | {}", e.index, e.type_id, index.get(&prof).map(|x| x.type_id).unwrap_or(0), fields.join(" "));
        if let Some(pe) = index.get(&prof) {
            let pf: Vec<String> = ents.fields(pe).iter().enumerate().skip(6).map(|(i, f)| format!("[{i}]={f:?}")).collect();
            println!("      profile #{prof}: {}", pf.join(" "));
        }
    }
    let mut handles: Vec<usize> = surfaces.keys().copied().collect();
    handles.sort_unstable();
    for h in handles {
        let Some(Some(s)) = surfaces.get(&h) else { continue };
        let d = s.domain();
        let kind = match s {
            Surface::Nurbs(n) => format!(
                "nurbs {}x{} deg {}x{} closed {}/{} rational {} knots u[{:.4},{:.4}] v[{:.4},{:.4}]",
                n.control_points.len(),
                n.control_points.first().map_or(0, |r| r.len()),
                n.u_degree,
                n.v_degree,
                n.u_closed,
                n.v_closed,
                !n.weights.is_empty(),
                n.u_knots.first().copied().unwrap_or(0.0),
                n.u_knots.last().copied().unwrap_or(0.0),
                n.v_knots.first().copied().unwrap_or(0.0),
                n.v_knots.last().copied().unwrap_or(0.0),
            ),
            Surface::Revolution { profile, .. } => format!("revolution of {:?}", std::mem::discriminant(&**profile)),
            Surface::Offset { distance, .. } => format!("offset {distance:.5}"),
            other => format!("{:?}", std::mem::discriminant(other)),
        };
        println!(
            "surface #{h} type {} {kind} domain u[{:.4},{:.4}] v[{:.4},{:.4}] period {:?}/{:?}",
            index.get(&h).map(|e| e.type_id).unwrap_or(0),
            d.u.lo, d.u.hi, d.v.lo, d.v.hi, d.u_period, d.v_period
        );
    }
    println!("worst chart point off a surface {worst_on:.3e} m; worst bow {worst_bow:.6} m; mid-chord solves {solved} ok, {failed} failed");
    Ok(())
}
