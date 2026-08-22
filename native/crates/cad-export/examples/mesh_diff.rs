//! Measure our mesh against a reference mesh, surface to surface.
//!
//! `cargo run --release -p cad-export --example mesh_diff -- ours.glb theirs.obj`
//!
//! Triangle counts say nothing about whether a surface is in the right place.
//! This samples every vertex of each mesh against the *triangles* of the other
//! and reports how far apart the two surfaces actually are, so a disagreement
//! can be located rather than guessed at.

// Shared with the other tools here; each uses the part of it it needs.
#[allow(dead_code)]
#[path = "common/glb_read.rs"]
mod glb_read;

use std::collections::HashMap;

type V3 = [f64; 3];

fn sub(a: V3, b: V3) -> V3 { [a[0] - b[0], a[1] - b[1], a[2] - b[2]] }
fn add(a: V3, b: V3) -> V3 { [a[0] + b[0], a[1] + b[1], a[2] + b[2]] }
fn mul(a: V3, s: f64) -> V3 { [a[0] * s, a[1] * s, a[2] * s] }
fn dot(a: V3, b: V3) -> f64 { a[0] * b[0] + a[1] * b[1] + a[2] * b[2] }

/// Squared distance from a point to a triangle, by the standard region search.
fn point_tri_sq(p: V3, a: V3, b: V3, c: V3) -> f64 {
    let ab = sub(b, a);
    let ac = sub(c, a);
    let ap = sub(p, a);
    let d1 = dot(ab, ap);
    let d2 = dot(ac, ap);
    if d1 <= 0.0 && d2 <= 0.0 {
        return dot(ap, ap);
    }
    let bp = sub(p, b);
    let d3 = dot(ab, bp);
    let d4 = dot(ac, bp);
    if d3 >= 0.0 && d4 <= d3 {
        return dot(bp, bp);
    }
    let vc = d1 * d4 - d3 * d2;
    if vc <= 0.0 && d1 >= 0.0 && d3 <= 0.0 {
        let v = d1 / (d1 - d3);
        let q = sub(p, add(a, mul(ab, v)));
        return dot(q, q);
    }
    let cp = sub(p, c);
    let d5 = dot(ab, cp);
    let d6 = dot(ac, cp);
    if d6 >= 0.0 && d5 <= d6 {
        return dot(cp, cp);
    }
    let vb = d5 * d2 - d1 * d6;
    if vb <= 0.0 && d2 >= 0.0 && d6 <= 0.0 {
        let w = d2 / (d2 - d6);
        let q = sub(p, add(a, mul(ac, w)));
        return dot(q, q);
    }
    let va = d3 * d6 - d5 * d4;
    if va <= 0.0 && (d4 - d3) >= 0.0 && (d5 - d6) >= 0.0 {
        let w = (d4 - d3) / ((d4 - d3) + (d5 - d6));
        let q = sub(p, add(b, mul(sub(c, b), w)));
        return dot(q, q);
    }
    let denom = 1.0 / (va + vb + vc);
    let v = vb * denom;
    let w = vc * denom;
    let q = sub(p, add(a, add(mul(ab, v), mul(ac, w))));
    dot(q, q)
}

/// Triangles bucketed onto a uniform grid, queried by growing shells.
struct Grid {
    cell: f64,
    min: V3,
    buckets: HashMap<(i32, i32, i32), Vec<u32>>,
    tris: Vec<[V3; 3]>,
}

impl Grid {
    fn build(tris: Vec<[V3; 3]>, target_per_cell: f64) -> Grid {
        let mut min = [f64::MAX; 3];
        let mut max = [f64::MIN; 3];
        for t in &tris {
            for v in t {
                for k in 0..3 {
                    min[k] = min[k].min(v[k]);
                    max[k] = max[k].max(v[k]);
                }
            }
        }
        let span = (0..3).map(|k| max[k] - min[k]).fold(0.0f64, f64::max);
        let cells = (tris.len() as f64 / target_per_cell).cbrt().max(1.0);
        let cell = (span / cells).max(1e-9);
        let mut buckets: HashMap<(i32, i32, i32), Vec<u32>> = HashMap::new();
        for (i, t) in tris.iter().enumerate() {
            let mut lo = [i32::MAX; 3];
            let mut hi = [i32::MIN; 3];
            for v in t {
                for k in 0..3 {
                    let c = ((v[k] - min[k]) / cell).floor() as i32;
                    lo[k] = lo[k].min(c);
                    hi[k] = hi[k].max(c);
                }
            }
            for x in lo[0]..=hi[0] {
                for y in lo[1]..=hi[1] {
                    for z in lo[2]..=hi[2] {
                        buckets.entry((x, y, z)).or_default().push(i as u32);
                    }
                }
            }
        }
        Grid { cell, min, buckets, tris }
    }

    /// Distance from `p` to the nearest triangle, searching outward until the
    /// shell already visited is farther than the best answer found.
    fn distance(&self, p: V3) -> f64 {
        let home = [
            ((p[0] - self.min[0]) / self.cell).floor() as i32,
            ((p[1] - self.min[1]) / self.cell).floor() as i32,
            ((p[2] - self.min[2]) / self.cell).floor() as i32,
        ];
        let mut best = f64::MAX;
        for r in 0..64 {
            if best.sqrt() <= (r as f64 - 1.0) * self.cell && r > 0 {
                break;
            }
            let mut touched = false;
            for x in home[0] - r..=home[0] + r {
                for y in home[1] - r..=home[1] + r {
                    for z in home[2] - r..=home[2] + r {
                        // Only the shell, not the solid block already searched.
                        let on_shell = (x - home[0]).abs() == r
                            || (y - home[1]).abs() == r
                            || (z - home[2]).abs() == r;
                        if !on_shell {
                            continue;
                        }
                        let Some(ids) = self.buckets.get(&(x, y, z)) else { continue };
                        touched = true;
                        for &i in ids {
                            let t = &self.tris[i as usize];
                            let d = point_tri_sq(p, t[0], t[1], t[2]);
                            if d < best {
                                best = d;
                            }
                        }
                    }
                }
            }
            if !touched && best < f64::MAX && r > 2 {
                break;
            }
        }
        best.sqrt()
    }
}

fn load_obj(path: &str) -> Result<(Vec<V3>, Vec<[V3; 3]>, Vec<String>), Box<dyn std::error::Error>> {
    let text = std::fs::read_to_string(path)?;
    let mut verts: Vec<V3> = Vec::new();
    let mut tris: Vec<[V3; 3]> = Vec::new();
    // A vertex is declared before the group that uses it, so the group is
    // recorded from the faces instead — that is what actually names a part.
    let mut owner: Vec<String> = Vec::new();
    let mut group = String::from("(none)");
    for line in text.lines() {
        let mut it = line.split_ascii_whitespace();
        match it.next() {
            Some("g") | Some("o") => {
                group = it.collect::<Vec<_>>().join(" ");
            }
            Some("v") => {
                owner.push(String::new());
                let c: Vec<f64> = it.take(3).filter_map(|s| s.parse().ok()).collect();
                if c.len() == 3 {
                    verts.push([c[0], c[1], c[2]]);
                }
            }
            Some("f") => {
                let idx: Vec<usize> = it
                    .filter_map(|s| s.split('/').next().and_then(|n| n.parse::<i64>().ok()))
                    .map(|i| if i < 0 { (verts.len() as i64 + i) as usize } else { i as usize - 1 })
                    .collect();
                for &i in &idx {
                    if i < owner.len() && owner[i].is_empty() {
                        owner[i] = group.clone();
                    }
                }
                for k in 1..idx.len().saturating_sub(1) {
                    if idx[0] < verts.len() && idx[k] < verts.len() && idx[k + 1] < verts.len() {
                        tris.push([verts[idx[0]], verts[idx[k]], verts[idx[k + 1]]]);
                    }
                }
            }
            _ => {}
        }
    }
    Ok((verts, tris, owner))
}

fn load_glb(path: &str) -> Result<(Vec<V3>, Vec<[V3; 3]>, Vec<String>), Box<dyn std::error::Error>> {
    let bytes = std::fs::read(path)?;
    let (doc, buffers) = glb_read::open(&bytes)?;
    let mut verts: Vec<V3> = Vec::new();
    let mut tris: Vec<[V3; 3]> = Vec::new();
    let mut owner: Vec<String> = Vec::new();

    fn walk(
        node: &gltf::Node,
        parent: [[f32; 4]; 4],
        path: &str,
        buffers: &[gltf::buffer::Data],
        verts: &mut Vec<V3>,
        tris: &mut Vec<[V3; 3]>,
        owner: &mut Vec<String>,
    ) {
        let here = match node.name() {
            Some(n) if path.is_empty() => n.to_string(),
            Some(n) => format!("{path}/{n}"),
            None => path.to_string(),
        };
        let local = node.transform().matrix();
        let m = mat_mul(parent, local);
        if let Some(mesh) = node.mesh() {
            for p in mesh.primitives() {
                let r = p.reader(|b| Some(&buffers[b.index()]));
                let pos: Vec<[f32; 3]> = glb_read::positions(&p, &buffers);
                let world: Vec<V3> = pos.iter().map(|v| apply(m, *v)).collect();
                verts.extend(world.iter().copied());
                owner.extend(std::iter::repeat(here.clone()).take(world.len()));
                if let Some(ix) = r.read_indices() {
                    let ix: Vec<u32> = ix.into_u32().collect();
                    for c in ix.chunks_exact(3) {
                        tris.push([
                            world[c[0] as usize],
                            world[c[1] as usize],
                            world[c[2] as usize],
                        ]);
                    }
                }
            }
        }
        for child in node.children() {
            walk(&child, m, &here, buffers, verts, tris, owner);
        }
    }

    for scene in doc.scenes() {
        for node in scene.nodes() {
            walk(&node, IDENTITY, "", &buffers, &mut verts, &mut tris, &mut owner);
        }
    }
    Ok((verts, tris, owner))
}

const IDENTITY: [[f32; 4]; 4] = [
    [1.0, 0.0, 0.0, 0.0],
    [0.0, 1.0, 0.0, 0.0],
    [0.0, 0.0, 1.0, 0.0],
    [0.0, 0.0, 0.0, 1.0],
];

/// glTF matrices are column-major: `m[c][r]`.
fn mat_mul(a: [[f32; 4]; 4], b: [[f32; 4]; 4]) -> [[f32; 4]; 4] {
    let mut out = [[0.0f32; 4]; 4];
    for c in 0..4 {
        for r in 0..4 {
            out[c][r] = (0..4).map(|k| a[k][r] * b[c][k]).sum();
        }
    }
    out
}

fn apply(m: [[f32; 4]; 4], v: [f32; 3]) -> V3 {
    let mut out = [0.0f64; 3];
    for r in 0..3 {
        out[r] = (m[0][r] * v[0] + m[1][r] * v[1] + m[2][r] * v[2] + m[3][r]) as f64;
    }
    out
}

fn bounds(v: &[V3]) -> (V3, V3) {
    let mut lo = [f64::MAX; 3];
    let mut hi = [f64::MIN; 3];
    for p in v {
        for k in 0..3 {
            lo[k] = lo[k].min(p[k]);
            hi[k] = hi[k].max(p[k]);
        }
    }
    (lo, hi)
}

/// The two writers need not agree on units or on which way is up. Recover the
/// uniform scale from the extents, then try every signed axis permutation and
/// keep whichever lands the bounding boxes on each other; anything else makes
/// every later number meaningless.
fn align(theirs: &mut Vec<V3>, tris: &mut Vec<[V3; 3]>, ours: &[V3]) -> (String, f64) {
    let (olo, ohi) = bounds(ours);
    let (tlo, thi) = bounds(theirs);
    let extent = |lo: V3, hi: V3| (0..3).map(|k| hi[k] - lo[k]).fold(0.0f64, f64::max);
    let scale = extent(olo, ohi) / extent(tlo, thi);

    let mut best: Option<(f64, [usize; 3], [f64; 3])> = None;
    for perm in [[0, 1, 2], [0, 2, 1], [1, 0, 2], [1, 2, 0], [2, 0, 1], [2, 1, 0]] {
        for signs in 0..8 {
            let s = [
                if signs & 1 != 0 { -1.0 } else { 1.0 },
                if signs & 2 != 0 { -1.0 } else { 1.0 },
                if signs & 4 != 0 { -1.0 } else { 1.0 },
            ];
            // A sign flip swaps that axis's low and high, so score the box
            // the permutation actually produces rather than assuming order.
            let mut lo = [0.0; 3];
            let mut hi = [0.0; 3];
            for k in 0..3 {
                let (a, b) = (tlo[perm[k]] * s[k] * scale, thi[perm[k]] * s[k] * scale);
                lo[k] = a.min(b);
                hi[k] = a.max(b);
            }
            let err: f64 = (0..3)
                .map(|k| (lo[k] - olo[k]).abs() + (hi[k] - ohi[k]).abs())
                .sum();
            if best.map_or(true, |b| err < b.0) {
                best = Some((err, perm, s));
            }
        }
    }
    let (err, perm, s) = best.unwrap();
    let axis = |i: usize| ["x", "y", "z"][i];
    let name = format!(
        "scale {scale:.6}, ({}{}, {}{}, {}{}), corner error {err:.6}",
        if s[0] < 0.0 { "-" } else { "" }, axis(perm[0]),
        if s[1] < 0.0 { "-" } else { "" }, axis(perm[1]),
        if s[2] < 0.0 { "-" } else { "" }, axis(perm[2]),
    );
    let map = |p: V3| -> V3 {
        [p[perm[0]] * s[0] * scale, p[perm[1]] * s[1] * scale, p[perm[2]] * s[2] * scale]
    };
    for p in theirs.iter_mut() {
        *p = map(*p);
    }
    for t in tris.iter_mut() {
        for p in t.iter_mut() {
            *p = map(*p);
        }
    }
    (name, 1.0 / scale)
}

fn report(
    name: &str,
    points: &[V3],
    owner: &[String],
    grid: &Grid,
    to_mm: f64,
    worst_out: &mut Vec<(f64, V3, String)>,
) {
    let d: Vec<f64> = points.iter().map(|p| grid.distance(*p) * to_mm).collect();

    let mut paired: Vec<(f64, V3, String)> = d
        .iter()
        .zip(points.iter())
        .enumerate()
        .map(|(i, (x, p))| (*x, *p, owner.get(i).cloned().unwrap_or_default()))
        .collect();
    paired.sort_by(|a, b| b.0.partial_cmp(&a.0).unwrap());
    worst_out.extend(paired.iter().take(400).cloned());

    let mut sorted = d.clone();
    sorted.sort_by(|a, b| a.partial_cmp(b).unwrap());
    let at = |q: f64| sorted[((sorted.len() - 1) as f64 * q) as usize];
    println!(
        "  {name:24}  mean {:.4}  p50 {:.4}  p99 {:.4}  p99.9 {:.4}  max {:.4}  (mm)",
        sorted.iter().sum::<f64>() / sorted.len() as f64,
        at(0.50),
        at(0.99),
        at(0.999),
        sorted[sorted.len() - 1]
    );
    let over = |t: f64| sorted.iter().filter(|x| **x > t).count();
    println!(
        "  {:24}  over 0.05mm {}  over 0.2mm {}  over 1mm {}  of {}",
        "",
        over(0.05),
        over(0.2),
        over(1.0),
        sorted.len()
    );

    // Which parts carry the disagreement, and how much of each part is in it.
    let mut per: HashMap<&str, (usize, usize, f64)> = HashMap::new();
    for (i, x) in d.iter().enumerate() {
        let e = per.entry(owner.get(i).map_or("", |s| s.as_str())).or_insert((0, 0, 0.0));
        e.0 += 1;
        if *x > 0.2 {
            e.1 += 1;
        }
        if *x > e.2 {
            e.2 = *x;
        }
    }
    let mut ranked: Vec<_> = per.into_iter().filter(|(_, v)| v.1 > 0).collect();
    ranked.sort_by(|a, b| b.1 .1.cmp(&a.1 .1));
    // Where the disagreeing points actually are. A percentage says how much
    // of a part is in dispute; only the coordinates say which feature.
    if let Some(want) = std::env::var("MESH_DIFF_DUMP").ok().filter(|_| !d.is_empty()) {
        let limit: f64 = want.parse().unwrap_or(0.2);
        for (i, x) in d.iter().enumerate() {
            if *x > limit {
                let q = points[i];
                println!(
                    "[far] {name} {:.4} mm at [{:.4}, {:.4}, {:.4}]  {}",
                    x,
                    q[0],
                    q[1],
                    q[2],
                    owner.get(i).map_or("", |s| s.as_str())
                );
            }
        }
    }
    println!("  parts carrying the disagreement (points over 0.2 mm):");
    for (part, (total, bad, worst)) in ranked.iter().take(12) {
        println!(
            "    {bad:7} / {total:<8} ({:5.1}%)  worst {worst:7.3} mm   {part}",
            100.0 * *bad as f64 / *total as f64
        );
    }
}

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let mut args = std::env::args().skip(1);
    let (Some(ours_path), Some(theirs_path)) = (args.next(), args.next()) else {
        eprintln!("usage: mesh_diff <ours.glb> <theirs.obj> [part-substring]");
        std::process::exit(2);
    };
    // Names differ in punctuation between the two writers, so a part is
    // matched on its digits alone.
    let digits = |s: &str| s.chars().filter(|c| c.is_ascii_digit()).collect::<String>();
    let only = args.next().map(|s| digits(&s));

    // Either side may be either writer's output, so the comparison can be run
    // in both directions — which is how a defect is told from a difference.
    let (ours_v, ours_t, ours_owner) = if ours_path.to_lowercase().ends_with(".obj") {
        load_obj(&ours_path)?
    } else {
        load_glb(&ours_path)?
    };
    // The reference may be either writer's output: OBJ from the comparison
    // mesher, GLB from our own other reader.
    let (mut theirs_v, mut theirs_t, theirs_owner) = if theirs_path.to_lowercase().ends_with(".obj") {
        load_obj(&theirs_path)?
    } else {
        load_glb(&theirs_path)?
    };
    // The whole-model bounding boxes are what align the two files, so keep
    // them before narrowing to one part.
    // Aligned on the whole model, before any narrowing. The scale and axis
    // mapping are recovered from the two bounding boxes, and one part's box
    // does not pin them down: a bolt is nearly square in section, so its own
    // box admits axis swaps the assembly's box rules out. Narrowing first
    // therefore reports a fault that is only the alignment's.
    let (how, to_mm) = align(&mut theirs_v, &mut theirs_t, &ours_v);

    let (ours_v, ours_owner, ours_t, theirs_v, theirs_owner, theirs_t) = match &only {
        None => (ours_v, ours_owner, ours_t, theirs_v, theirs_owner, theirs_t),
        Some(want) => {
            let keep = |v: Vec<V3>, o: Vec<String>, t: Vec<[V3; 3]>| {
                let inside: Vec<bool> = o.iter().map(|n| digits(n).contains(want)).collect();
                let mut lo = [f64::MAX; 3];
                let mut hi = [f64::MIN; 3];
                for (p, k) in v.iter().zip(&inside) {
                    if *k {
                        for a in 0..3 {
                            lo[a] = lo[a].min(p[a]);
                            hi[a] = hi[a].max(p[a]);
                        }
                    }
                }
                // Triangles are kept when they lie inside the part's own box,
                // and a triangle straddling that box belongs to the part just
                // as much as one wholly within it. Without room for them the
                // part's own boundary vertices have nothing to measure
                // against and read as metres out: on `204 201 013-51` against
                // the fine reference this alone produced a 4 mm "worst
                // disagreement" at a place where the two meshes are identical
                // to the pixel.
                let span = (0..3).map(|a| hi[a] - lo[a]).fold(0.0f64, f64::max);
                let pad = (span * 0.05).max(1e-9);
                let within = |p: &V3| (0..3).all(|a| p[a] >= lo[a] - pad && p[a] <= hi[a] + pad);
                let tris: Vec<[V3; 3]> =
                    t.into_iter().filter(|tr| tr.iter().all(within)).collect();
                let (vv, oo): (Vec<V3>, Vec<String>) = v
                    .into_iter()
                    .zip(o)
                    .zip(&inside)
                    .filter(|(_, k)| **k)
                    .map(|(x, _)| x)
                    .unzip();
                (vv, oo, tris)
            };
            let (av, ao, at) = keep(ours_v, ours_owner, ours_t);
            let (bv, bo, bt) = keep(theirs_v, theirs_owner, theirs_t);
            println!("narrowed to parts whose digits contain \"{want}\"");
            (av, ao, at, bv, bo, bt)
        }
    };
    println!("ours   {ours_path}  {} verts  {} tris", ours_v.len(), ours_t.len());
    println!("theirs {theirs_path}  {} verts  {} tris", theirs_v.len(), theirs_t.len());

    println!("aligned by: {how}");
    let (alo, ahi) = bounds(&ours_v);
    let (blo, bhi) = bounds(&theirs_v);
    println!("  ours   bbox {alo:?} .. {ahi:?}");
    println!("  theirs bbox {blo:?} .. {bhi:?}");

    let ours_grid = Grid::build(ours_t, 8.0);
    let theirs_grid = Grid::build(theirs_t, 8.0);

    let mut worst_ours = Vec::new();
    let mut worst_theirs = Vec::new();
    // A vertex of ours sits on the true surface even when the triangle it
    // belongs to does not: a chord straight through a cylinder has both its
    // ends in the right place. So the triangles are sampled at their centroids
    // as well, which is the only way a membrane spanning the inside of a part
    // shows up at all.
    let ours_centroids: Vec<V3> = ours_grid
        .tris
        .iter()
        .map(|t| {
            [
                (t[0][0] + t[1][0] + t[2][0]) / 3.0,
                (t[0][1] + t[1][1] + t[2][1]) / 3.0,
                (t[0][2] + t[1][2] + t[2][2]) / 3.0,
            ]
        })
        .collect();
    let blank: Vec<String> = Vec::new();

    println!("distance from each mesh's vertices to the other's surface:");
    report("ours -> their surface", &ours_v, &ours_owner, &theirs_grid, to_mm, &mut worst_ours);
    report("theirs -> our surface", &theirs_v, &theirs_owner, &ours_grid, to_mm, &mut worst_theirs);

    let mut worst_centroid: Vec<(f64, V3, String)> = Vec::new();
    println!("distance from our triangle centroids to their surface:");
    report(
        "ours (centroids)",
        &ours_centroids,
        &blank,
        &theirs_grid,
        to_mm,
        &mut worst_centroid,
    );

    worst_centroid.sort_by(|a, b| b.0.partial_cmp(&a.0).unwrap());
    println!("worst triangle centroids — where our surface has geometry theirs does not:");
    for (d, p, _) in worst_centroid.iter().take(10) {
        println!(
            "  {d:8.4} mm at [{:.2}, {:.2}, {:.2}]",
            p[0] * to_mm,
            p[1] * to_mm,
            p[2] * to_mm
        );
    }

    println!("worst places where our surface leaves theirs:");
    worst_theirs.sort_by(|a, b| b.0.partial_cmp(&a.0).unwrap());
    for (d, p, part) in worst_theirs.iter().take(10) {
        println!(
            "  {d:8.4} mm at [{:.2}, {:.2}, {:.2}]   {part}",
            p[0] * to_mm,
            p[1] * to_mm,
            p[2] * to_mm
        );
    }
    Ok(())
}
