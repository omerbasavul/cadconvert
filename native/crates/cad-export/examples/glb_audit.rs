//! Check a finished GLB for the faults a viewer shows: holes and seams.
//!
//! `cargo run --release -p cad-export --example glb_audit -- file.glb|file.obj`
//!
//! Reads OBJ too, so the same test can be put to a reference mesher: an open
//! edge means a hole only if the reference closes it.
//!
//! Works on the written file rather than on the tessellator's own bookkeeping,
//! so it cannot inherit a mistake from the thing it is checking. Vertices are
//! matched by position, welded on a grid finer than any real feature, because
//! a GLB splits a vertex per normal and two triangles that meet in space need
//! not share an index.

// Shared with the other tools here; each uses the part of it it needs.
#[allow(dead_code)]
#[path = "common/glb_read.rs"]
mod glb_read;

use std::collections::HashMap;

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let Some(path) = std::env::args().nth(1) else {
        eprintln!("usage: glb_audit <file.glb>");
        std::process::exit(2);
    };
    // Both readers reduce to the same thing: named bodies, each a list of
    // triangles given as three positions.
    let bodies: Vec<(String, Vec<[[f32; 3]; 3]>)> = if path.to_lowercase().ends_with(".obj") {
        load_obj(&path)?
    } else {
        load_glb(&path)?
    };

    let mut open = 0usize;
    let mut non_manifold = 0usize;
    let mut triangles = 0usize;
    let mut degenerate = 0usize;
    let mut bodies_open = 0usize;
    let mut worst: Vec<(usize, String)> = Vec::new();
    let mut stretched: Vec<(f64, f64, f64, String)> = Vec::new();
    let mut openness: Vec<(usize, f64, f64, f64, String)> = Vec::new();
    let mut tees = 0usize;
    let mut reversed = 0usize;
    let mut holes = 0usize;
    let mut inverted: Vec<(f64, String)> = Vec::new();
    let mut tee_by_body: Vec<(usize, String)> = Vec::new();

    for (name, tris) in &bodies {
        // A body is watertight or not on its own; summing first would hide
        // which one leaks.
        let mut signed: HashMap<(u64, u64), i32> = HashMap::new();
        let mut uses: HashMap<(u64, u64), usize> = HashMap::new();
        for t in tris {
            let k = [key(t[0]), key(t[1]), key(t[2])];
            if k[0] == k[1] || k[1] == k[2] || k[2] == k[0] {
                degenerate += 1;
                continue;
            }
            triangles += 1;
            for e in [(k[0], k[1]), (k[1], k[2]), (k[2], k[0])] {
                // One counter per undirected edge, signed by direction: two
                // opposite uses cancel, which is what a closed surface does
                // everywhere.
                let (lo, hi, dir) = if e.0 < e.1 { (e.0, e.1, 1) } else { (e.1, e.0, -1) };
                *signed.entry((lo, hi)).or_insert(0) += dir;
                *uses.entry((lo, hi)).or_insert(0) += 1;
            }
        }
        // How long a triangle edge is against the body it belongs to. A mesh
        // that chords across a whole cylinder has an edge the width of the
        // part, and no tolerance asked for it — so this is checked on the
        // written file rather than taken on trust from the tessellator.
        let mut lo = [f32::MAX; 3];
        let mut hi = [f32::MIN; 3];
        for t in tris {
            for v in t {
                for k in 0..3 {
                    lo[k] = lo[k].min(v[k]);
                    hi[k] = hi[k].max(v[k]);
                }
            }
        }
        let extent = (0..3).map(|k| (hi[k] - lo[k]) as f64).fold(0.0f64, f64::max);
        let mut longest = 0.0f64;
        for t in tris {
            for k in 0..3 {
                let (a, b) = (t[k], t[(k + 1) % 3]);
                let d = (((a[0] - b[0]) as f64).powi(2)
                    + ((a[1] - b[1]) as f64).powi(2)
                    + ((a[2] - b[2]) as f64).powi(2))
                .sqrt();
                longest = longest.max(d);
            }
        }
        if extent > 0.0 && longest > extent * 0.5 {
            stretched.push((longest / extent, longest, extent, name.clone()));
        }

        // What the open edges are, not just how many. A hairline where two
        // faces sampled a shared edge differently is short and its ends land
        // on another triangle's edge; a genuine hole is long and bounded by
        // nothing. Telling them apart decides where to look.
        let open_keys: Vec<(u64, u64)> =
            signed.iter().filter(|(_, v)| **v != 0).map(|(k, _)| *k).collect();
        if !open_keys.is_empty() {
            let mut length = 0.0f64;
            let mut longest = 0.0f64;
            let want: std::collections::HashSet<(u64, u64)> = open_keys.iter().copied().collect();
            for t in tris {
                let k = [key(t[0]), key(t[1]), key(t[2])];
                for e in 0..3 {
                    let (a, b) = (k[e], k[(e + 1) % 3]);
                    let id = if a < b { (a, b) } else { (b, a) };
                    if want.contains(&id) {
                        let (p, q) = (t[e], t[(e + 1) % 3]);
                        let d = (((p[0] - q[0]) as f64).powi(2)
                            + ((p[1] - q[1]) as f64).powi(2)
                            + ((p[2] - q[2]) as f64).powi(2))
                        .sqrt();
                        length += d;
                        longest = longest.max(d);
                    }
                }
            }
            // A hole is bounded by edges whose ends meet only each other. A
            // T-junction is different: one face split a shared edge and its
            // neighbour did not, so the neighbour's whole edge and both halves
            // are all left open, and the split point sits in the middle of the
            // long one. Counting the ends that land inside another open edge
            // separates the two, and they want opposite fixes.
            let mut ends: Vec<[f32; 3]> = Vec::new();
            let mut segs: Vec<([f32; 3], [f32; 3])> = Vec::new();
            for t in tris {
                let k = [key(t[0]), key(t[1]), key(t[2])];
                for e in 0..3 {
                    let (a, b) = (k[e], k[(e + 1) % 3]);
                    let id = if a < b { (a, b) } else { (b, a) };
                    if want.contains(&id) {
                        segs.push((t[e], t[(e + 1) % 3]));
                        ends.push(t[e]);
                        ends.push(t[(e + 1) % 3]);
                    }
                }
            }
            // The split point sits on the curve while the edge it splits is a
            // chord, so the two are apart by that chord's own sagitta — which
            // is the meshing tolerance, not zero. Testing at machine precision
            // finds none of them.
            let tol = extent * 1e-3;
            let mut tee = 0usize;
            for (a, b) in &segs {
                let d = [
                    (b[0] - a[0]) as f64,
                    (b[1] - a[1]) as f64,
                    (b[2] - a[2]) as f64,
                ];
                let len2 = d[0] * d[0] + d[1] * d[1] + d[2] * d[2];
                if len2 <= 0.0 {
                    continue;
                }
                if ends.iter().any(|p| {
                    let w = [
                        (p[0] - a[0]) as f64,
                        (p[1] - a[1]) as f64,
                        (p[2] - a[2]) as f64,
                    ];
                    let t = (w[0] * d[0] + w[1] * d[1] + w[2] * d[2]) / len2;
                    // Strictly inside, so the segment's own ends do not count.
                    if !(t > 1e-6 && t < 1.0 - 1e-6) {
                        return false;
                    }
                    let off = [w[0] - d[0] * t, w[1] - d[1] * t, w[2] - d[2] * t];
                    (off[0] * off[0] + off[1] * off[1] + off[2] * off[2]).sqrt() <= tol
                }) {
                    tee += 1;
                }
            }
            tees += tee;
            if std::env::var_os("GLB_AUDIT_DUMP").is_some() {
                for (a, b) in segs.iter().take(400) {
                    println!(
                        "[open] [{:.4},{:.4},{:.4}] .. [{:.4},{:.4},{:.4}]   {name}",
                        a[0], a[1], a[2], b[0], b[1], b[2]
                    );
                }
            }
            openness.push((open_keys.len(), length, longest, extent, name.clone()));
            tee_by_body.push((tee, name.clone()));
        }

        // An edge used twice the same way round is not a hole: both triangles
        // are there, one of them is simply wound backwards. It renders as an
        // inside-out face, and it is a different fault with a different fix,
        // so the two are counted apart.
        for (k, v) in &signed {
            if *v != 0 {
                if uses.get(k) == Some(&2) {
                    reversed += 1;
                } else {
                    holes += 1;
                }
            }
        }

        // The signed volume a closed shell encloses. Outward-wound it is
        // positive; a shell wound inward reports the same volume negative, and
        // that is the only way to tell an inside-out body from a correct one
        // once its faces all agree with each other.
        let mut volume = 0.0f64;
        for t in tris {
            let a = [t[0][0] as f64, t[0][1] as f64, t[0][2] as f64];
            let b = [t[1][0] as f64, t[1][1] as f64, t[1][2] as f64];
            let c = [t[2][0] as f64, t[2][1] as f64, t[2][2] as f64];
            volume += (a[0] * (b[1] * c[2] - b[2] * c[1])
                - a[1] * (b[0] * c[2] - b[2] * c[0])
                + a[2] * (b[0] * c[1] - b[1] * c[0]))
                / 6.0;
        }
        // Volume answers a question no distance can: whether a disagreement
        // with another mesher is material we added or material we left out.
        if std::env::var("GLB_AUDIT_VOLUME").is_ok_and(|w| name.contains(&w)) {
            println!("  {name} encloses {:.4} cubic units", volume / 6.0);
        }
        if volume < 0.0 {
            inverted.push((volume, name.clone()));
        }

        let body_open = signed.values().filter(|v| **v != 0).count();
        if body_open > 0 {
            bodies_open += 1;
            worst.push((body_open, name.clone()));
        }
        open += body_open;
        let nm: Vec<(u64, u64)> =
            uses.iter().filter(|(_, n)| **n > 2).map(|(k, _)| *k).collect();
        non_manifold += nm.len();
        if !nm.is_empty() && std::env::var_os("GLB_AUDIT_DUMP").is_some() {
            let want: std::collections::HashSet<(u64, u64)> = nm.into_iter().collect();
            let mut shown = 0;
            for t in tris {
                let k = [key(t[0]), key(t[1]), key(t[2])];
                for e in 0..3 {
                    let (a, b) = (k[e], k[(e + 1) % 3]);
                    let id = if a < b { (a, b) } else { (b, a) };
                    if want.contains(&id) && shown < 40 {
                        let (p, q) = (t[e], t[(e + 1) % 3]);
                        let r = t[(e + 2) % 3];
                        println!(
                            "[nm] [{:.4},{:.4},{:.4}] .. [{:.4},{:.4},{:.4}]  used {}  third corner [{:.4},{:.4},{:.4}], {:.5} mm off the edge  {name}",
                            p[0], p[1], p[2], q[0], q[1], q[2],
                            uses[&id],
                            r[0], r[1], r[2],
                            {
                                // Distance from the third corner to the edge's
                                // line: a sliver on the boundary shows as ~0.
                                let d = [q[0] - p[0], q[1] - p[1], q[2] - p[2]];
                                let w = [r[0] - p[0], r[1] - p[1], r[2] - p[2]];
                                let l2 = d[0] * d[0] + d[1] * d[1] + d[2] * d[2];
                                let t = if l2 > 0.0 {
                                    (w[0] * d[0] + w[1] * d[1] + w[2] * d[2]) / l2
                                } else {
                                    0.0
                                };
                                let o = [w[0] - d[0] * t, w[1] - d[1] * t, w[2] - d[2] * t];
                                (o[0] * o[0] + o[1] * o[1] + o[2] * o[2]).sqrt()
                            }
                        );
                        shown += 1;
                    }
                }
            }
        }
    }

    println!("{path}");
    println!("  triangles      {triangles}  ({degenerate} degenerate, dropped)");
    println!("  bodies         {}", bodies.len());
    println!("  open edges     {open}  across {bodies_open} bodies");
    println!("  non-manifold   {non_manifold}");
    println!(
        "  of those, {reversed} are edges whose two triangles are wound the same way \
         (a reversed face, not a hole) and {holes} bound something missing"
    );
    println!("  bodies enclosing a negative volume (wound inside out): {}", inverted.len());
    for (v, name) in inverted.iter().take(6) {
        println!("    {v:14.2}   {name}");
    }
    println!("  of the open edges, {tees} are T-junctions (a split one face made and its neighbour did not)");
    stretched.sort_by(|a, b| b.0.partial_cmp(&a.0).unwrap());
    println!("  triangle edges longer than half their body: {}", stretched.len());
    for (ratio, longest, extent, name) in stretched.iter().take(8) {
        println!("    {ratio:5.2}x   edge {longest:8.4}  body {extent:8.4}   {name}");
    }
    openness.sort_by(|a, b| b.0.cmp(&a.0));
    let tee_of = |name: &str| {
        tee_by_body.iter().find(|(_, n)| n == name).map_or(0, |(t, _)| *t)
    };
    for (n, total, longest, extent, name) in openness.iter().take(10) {
        let _ = tee_of(name);
        println!(
            "    {n:6} open ({:4} T)  total {total:9.3}  longest {longest:7.4}  body {extent:8.3}   {name}",
            tee_of(name)
        );
    }
    Ok(())
}

type Body = (String, Vec<[[f32; 3]; 3]>);

fn load_glb(path: &str) -> Result<Vec<Body>, Box<dyn std::error::Error>> {
    let bytes = std::fs::read(path)?;
    let (doc, buffers) = glb_read::open(&bytes)?;
    let mut out = Vec::new();
    for mesh in doc.meshes() {
        let mut tris = Vec::new();
        for p in mesh.primitives() {
            let r = p.reader(|b| Some(&buffers[b.index()]));
            let pos: Vec<[f32; 3]> = glb_read::positions(&p, &buffers);
            let Some(ix) = r.read_indices() else { continue };
            for c in ix.into_u32().collect::<Vec<u32>>().chunks_exact(3) {
                tris.push([pos[c[0] as usize], pos[c[1] as usize], pos[c[2] as usize]]);
            }
        }
        out.push((mesh.name().unwrap_or("(unnamed)").to_string(), tris));
    }
    Ok(out)
}

fn load_obj(path: &str) -> Result<Vec<Body>, Box<dyn std::error::Error>> {
    let text = std::fs::read_to_string(path)?;
    let mut verts: Vec<[f32; 3]> = Vec::new();
    let mut out: Vec<Body> = Vec::new();
    let mut current = String::from("(none)");
    for line in text.lines() {
        let mut it = line.split_ascii_whitespace();
        match it.next() {
            Some("v") => {
                let c: Vec<f32> = it.take(3).filter_map(|s| s.parse().ok()).collect();
                if c.len() == 3 {
                    verts.push([c[0], c[1], c[2]]);
                }
            }
            Some("g") | Some("o") => {
                current = it.collect::<Vec<_>>().join(" ");
                if !out.iter().any(|(n, _)| *n == current) {
                    out.push((current.clone(), Vec::new()));
                }
            }
            Some("f") => {
                let idx: Vec<usize> = it
                    .filter_map(|s| s.split('/').next().and_then(|n| n.parse::<i64>().ok()))
                    .map(|i| if i < 0 { (verts.len() as i64 + i) as usize } else { i as usize - 1 })
                    .collect();
                if out.is_empty() {
                    out.push((current.clone(), Vec::new()));
                }
                let body = out.iter_mut().rev().find(|(n, _)| *n == current).unwrap();
                for k in 1..idx.len().saturating_sub(1) {
                    if idx[0] < verts.len() && idx[k] < verts.len() && idx[k + 1] < verts.len() {
                        body.1.push([verts[idx[0]], verts[idx[k]], verts[idx[k + 1]]]);
                    }
                }
            }
            _ => {}
        }
    }
    Ok(out)
}

/// A position quantised to a grid far finer than any feature but coarser than
/// the last bits of an f32, so two writers of the same point agree.
fn key(p: [f32; 3]) -> u64 {
    // The weld grid, in units of the file. Coarsening it separates a hairline
    // where two faces sampled a shared edge a few nanometres apart from a hole
    // where a face is simply missing: the first closes as the grid grows, the
    // second never does.
    let grid: f64 = std::env::var("GLB_AUDIT_WELD")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(1e7);
    let q = |v: f32| (v as f64 * grid).round() as i64;
    let (a, b, c) = (q(p[0]), q(p[1]), q(p[2]));
    let mut h = 1469598103934665603u64;
    for v in [a, b, c] {
        for byte in v.to_le_bytes() {
            h ^= byte as u64;
            h = h.wrapping_mul(1099511628211);
        }
    }
    h
}
