//! Ask whether a part's triangles sit on the part's own surfaces.
//!
//! Every other check in this project compares our mesh with someone else's.
//! This one compares our mesh with our own reading of the geometry, which
//! separates the two ways a mesh can be wrong: the tessellation left the
//! surface, or the surface itself was read wrong. A vertex far from every
//! surface in its body is the first; a mesh that hugs our surfaces while
//! disagreeing with a reference mesher is the second.
//!
//! `cargo run --release -p cad-tess --example surface_check -- file.stp [part]`

use cad_ir::math::{Vec2, Vec3};
use cad_step::{StepFile, lower};
use cad_tess::{Options, surface_kind};

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let mut args = std::env::args().skip(1);
    let Some(path) = args.next() else {
        eprintln!("usage: surface_check <file.stp> [part-name-substring]");
        std::process::exit(2);
    };
    let want = args.next();

    let file = StepFile::open(&path)?;
    let (mut scene, _) = lower::asm::to_scene(&file)?;
    cad_tess::tessellate_scene(&mut scene, &Options::default());

    for g in &scene.geometry {
        let (Some(mesh), Some(solid)) = (&g.mesh, &g.brep) else {
            continue;
        };
        if let Some(w) = &want
            && !solid.name.contains(w.as_str())
        {
            continue;
        }

        // Nearest of the body's own surfaces, for every vertex. A vertex
        // belongs to one face, but the mesh does not say which; the nearest
        // surface is the kindest possible reading, so a large distance here
        // cannot be blamed on picking the wrong face.
        // One coarse sweep per surface, reused for every vertex. Spline
        // directions are swept by their own spans so a long tube is not
        // sampled at a hundredth of its length.
        let grids: Vec<Vec<(Vec2, Vec3)>> = solid
            .surfaces
            .iter()
            .map(|s| {
                let d = s.domain();
                let (nu, nv) = (24usize, 24usize);
                let mut g = Vec::with_capacity((nu + 1) * (nv + 1));
                for i in 0..=nu {
                    for j in 0..=nv {
                        let uv = Vec2::new(
                            d.u.at(i as f64 / nu as f64),
                            d.v.at(j as f64 / nv as f64),
                        );
                        g.push((uv, s.point_at(uv)));
                    }
                }
                g
            })
            .collect();

        let mut worst = 0.0f64;
        let mut worst_at = Vec3::ZERO;
        let mut worst_kind = "";
        let mut over = 0usize;
        let mut sum = 0.0f64;
        // Every vertex against every surface is quadratic in the part's size,
        // and the largest bodies here carry 1,300 surfaces. Sampling keeps a
        // whole-assembly run to a bounded cost; the question being asked is
        // whether the mesh leaves the surface anywhere, and a fault that shows
        // on no sampled vertex out of a hundred thousand comparisons is not
        // one this check was built to find.
        const BUDGET: usize = 400_000;
        let stride =
            (mesh.positions.len() * solid.surfaces.len().max(1) / BUDGET.max(1)).max(1);
        let mut looked = 0usize;
        for v in mesh.positions.iter().step_by(stride) {
            looked += 1;
            let p = Vec3::new(v[0] as f64, v[1] as f64, v[2] as f64);
            let mut best = f64::INFINITY;
            let mut best_kind = "";
            for (s, grid) in solid.surfaces.iter().zip(&grids) {
                // A blind inversion of a spline with hundreds of spans lands
                // wherever Newton happens to fall, and reports the distance to
                // that as if it were the surface's. Seeding from the nearest
                // of a coarse sweep of the surface asks the question this
                // check means to ask, rather than measuring the solver.
                let seed = grid
                    .iter()
                    .min_by(|a, b| {
                        (a.1 - p).length_squared().total_cmp(&(b.1 - p).length_squared())
                    })
                    .map(|(uv, _)| *uv);
                // Whichever of the two lands closer. Neither is trustworthy
                // alone on a spline with hundreds of spans — the blind solve
                // falls where it falls, and a sweep coarse enough to be cheap
                // seeds it into the wrong span — so the check reports the best
                // either could do. That makes it a lower bound, which is the
                // side a defect-finder should err on: it accuses only when no
                // reading of the surface puts the vertex near it.
                let mut here = f64::INFINITY;
                for uv in [
                    s.invert(p, None),
                    seed.and_then(|seed| {
                        s.invert_near(p, seed, 1e3).or_else(|| s.invert(p, Some(seed)))
                    }),
                ]
                .into_iter()
                .flatten()
                {
                    here = here.min((s.point_at(uv) - p).length());
                }
                if here < best {
                    best = here;
                    best_kind = surface_kind(s);
                }
            }
            if best.is_finite() {
                sum += best;
                if best > solid.tolerance.max(1e-6) * 10.0 {
                    over += 1;
                }
                if best > worst {
                    worst = best;
                    worst_at = p;
                    worst_kind = best_kind;
                }
            }
        }
        // How far around its tube a torus face was actually drawn. A flange
        // rim is a torus trimmed between two circles, and the face runs
        // between them on one side or the other: over the equator, where the
        // rim bulges out to major+minor, or under it, where it tucks in to
        // major-minor. Both readings meet the same two edges, so nothing in
        // the topology objects if the wrong one is taken — only the radius the
        // mesh reaches gives it away.
        // The same question for a sphere: which band of latitude the mesh
        // actually drew. A cap and everything-but-that-cap share one boundary
        // circle, so the two are told apart only by what was covered.
        if let Ok(want) = std::env::var("SURFACE_CHECK_SPHERE") {
            for (i, surf) in solid.surfaces.iter().enumerate() {
                let cad_ir::brep::Surface::Sphere { frame, radius } = surf else { continue };
                if !want.is_empty() && want != i.to_string() {
                    continue;
                }
                let (mut lo, mut hi) = (f64::MAX, f64::MIN);
                let mut on = 0usize;
                for q in &mesh.positions {
                    let p = Vec3::new(q[0] as f64, q[1] as f64, q[2] as f64);
                    let d = p - frame.origin;
                    if (d.length() - radius).abs() > 1e-6 {
                        continue;
                    }
                    on += 1;
                    let lat = (d.dot(frame.axis) / radius).clamp(-1.0, 1.0).asin().to_degrees();
                    lo = lo.min(lat);
                    hi = hi.max(lat);
                }
                println!(
                    "  sphere {i}: r {radius:.4}  {on} verts  latitude {:.1}..{:.1} deg",
                    if on > 0 { lo } else { 0.0 },
                    if on > 0 { hi } else { 0.0 }
                );
                // Latitude alone cannot show a missing wedge: a band drawn at
                // the right height but the wrong way round covers the same
                // latitudes. The longitude buckets show it.
                let mut bucket = [0usize; 12];
                for q in &mesh.positions {
                    let p = Vec3::new(q[0] as f64, q[1] as f64, q[2] as f64);
                    let d = p - frame.origin;
                    if (d.length() - radius).abs() > 1e-6 {
                        continue;
                    }
                    let x = d.dot(frame.ref_dir);
                    let y = d.dot(frame.y_dir());
                    let a = y.atan2(x).to_degrees();
                    let k = (((a + 360.0) % 360.0) / 30.0) as usize;
                    bucket[k.min(11)] += 1;
                }
                let marks: String = bucket
                    .iter()
                    .map(|n| if *n == 0 { '.' } else { '#' })
                    .collect();
                println!("      longitude 0..360 in twelfths: {marks}  {bucket:?}");
            }
        }

        // What a cone face's mesh actually covers: how far along the axis and
        // how far round. A band drawn between two rings that stops short shows
        // here and in no structural check at all.
        if let Ok(want) = std::env::var("SURFACE_CHECK_CONE") {
            for (i, surf) in solid.surfaces.iter().enumerate() {
                let cad_ir::brep::Surface::Cone { frame, radius, half_angle } = surf else {
                    continue;
                };
                if !want.is_empty() && want != i.to_string() {
                    continue;
                }
                let (mut lo, mut hi) = (f64::MAX, f64::MIN);
                let mut bucket = [0usize; 12];
                let mut on = 0usize;
                for q in &mesh.positions {
                    let p = Vec3::new(q[0] as f64, q[1] as f64, q[2] as f64);
                    let d = p - frame.origin;
                    let along = d.dot(frame.axis);
                    let radial = (d - frame.axis * along).length();
                    if (radial - (radius + along * half_angle.tan())).abs() > 1e-6 {
                        continue;
                    }
                    on += 1;
                    lo = lo.min(along);
                    hi = hi.max(along);
                    let a = d.dot(frame.y_dir()).atan2(d.dot(frame.ref_dir)).to_degrees();
                    bucket[((((a + 360.0) % 360.0) / 30.0) as usize).min(11)] += 1;
                }
                if on > 0 {
                    let marks: String =
                        bucket.iter().map(|n| if *n == 0 { '.' } else { '#' }).collect();
                    println!(
                        "  cone {i}: r {radius:.4} half angle {:.3} deg  {on} verts  along the axis {lo:.4}..{hi:.4}  round it {marks}",
                        half_angle.to_degrees()
                    );
                    // A band on a cone is drawn between two rings, and how far
                    // it reaches along the axis is a different answer at every
                    // longitude. One range for the whole face hides a stretch
                    // the band never covers.
                    let mut span = [(f64::MAX, f64::MIN); 12];
                    for q in &mesh.positions {
                        let p = Vec3::new(q[0] as f64, q[1] as f64, q[2] as f64);
                        let d = p - frame.origin;
                        let along = d.dot(frame.axis);
                        let radial = (d - frame.axis * along).length();
                        if (radial - (radius + along * half_angle.tan())).abs() > 1e-6 {
                            continue;
                        }
                        let a = d.dot(frame.y_dir()).atan2(d.dot(frame.ref_dir)).to_degrees();
                        let k = ((((a + 360.0) % 360.0) / 30.0) as usize).min(11);
                        span[k].0 = span[k].0.min(along);
                        span[k].1 = span[k].1.max(along);
                    }
                    for (k, (a, b)) in span.iter().enumerate() {
                        if a.is_finite() {
                            println!("      {:>3}..{:>3} deg  along the axis {a:.4}..{b:.4}", k * 30, k * 30 + 30);
                        }
                    }
                }
            }
        }

        if std::env::var_os("SURFACE_CHECK_TORUS").is_some() {
            for (i, surf) in solid.surfaces.iter().enumerate() {
                let cad_ir::brep::Surface::Torus { frame, major_radius, minor_radius } = surf
                else {
                    continue;
                };
                let mut on = 0usize;
                let (mut vlo, mut vhi) = (f64::MAX, f64::MIN);
                let (mut rlo, mut rhi) = (f64::MAX, f64::MIN);
                for q in &mesh.positions {
                    let p = Vec3::new(q[0] as f64, q[1] as f64, q[2] as f64);
                    let Some(uv) = surf.invert(p, None) else { continue };
                    if (surf.point_at(uv) - p).length() > 1e-6 {
                        continue;
                    }
                    on += 1;
                    let d = p - frame.origin;
                    let along = d.dot(frame.axis);
                    let radial = (d - frame.axis * along).length();
                    // Signed turn around the tube, zero at the outermost ring.
                    let v = (along).atan2(radial - major_radius).to_degrees();
                    vlo = vlo.min(v);
                    vhi = vhi.max(v);
                    rlo = rlo.min(radial);
                    rhi = rhi.max(radial);
                }
                if on > 0 {
                    println!(
                        "  torus {i}: major {major_radius:.4} minor {minor_radius:.4}  {on} verts  \
                         around the tube {vlo:.1}..{vhi:.1} deg  radius {rlo:.4}..{rhi:.4}  \
                         (the tube reaches {:.4} at its outermost)",
                        major_radius + minor_radius
                    );
                }
            }
        }

        // The mesh's own reach, beside the body's vertices. A face trimmed
        // short shows here and nowhere else: the vertices are still in the
        // right place, and every triangle still sits on its surface, but the
        // surface stops before it should.
        let b = mesh.bounds();
        let vb = solid.geometric_bounds();
        println!(
            "  mesh spans [{:.4}, {:.4}, {:.4}] .. [{:.4}, {:.4}, {:.4}]   the file's own vertices span [{:.4}, {:.4}, {:.4}] .. [{:.4}, {:.4}, {:.4}]",
            b.min.x, b.min.y, b.min.z, b.max.x, b.max.y, b.max.z,
            vb.min.x, vb.min.y, vb.min.z, vb.max.x, vb.max.y, vb.max.z,
        );
        println!(
            "{:<28} {:>7} verts  {:>5} surfaces  mean {:.6}  worst {:.6} mm on a {} at [{:.3}, {:.3}, {:.3}]  ({} over 10x tolerance {:.6})",
            solid.name,
            mesh.positions.len(),
            solid.surfaces.len(),
            sum / looked.max(1) as f64,
            worst,
            worst_kind,
            worst_at.x,
            worst_at.y,
            worst_at.z,
            over,
            solid.tolerance,
        );
    }
    Ok(())
}
