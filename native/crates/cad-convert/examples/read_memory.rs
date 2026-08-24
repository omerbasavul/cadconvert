//! Resident memory at each step of reading a file.
//!
//! Peak resident size after reading the pilot is around 550 MB while the scene
//! it produces holds 19 MB — see `scene_bytes`. The difference is transient,
//! and a peak measured from outside cannot say which step reached it. This
//! prints the resident size at each step so it can.
//!
//! `ps` is asked rather than a crate: this is a diagnostic that runs by hand on
//! a developer's machine, and a dependency for it would ship to every user.
//!
//! # One run of this is not a measurement
//!
//! Resident size is what the allocator has taken from the kernel, not what the
//! program is using, and it moves with size-class luck and with whatever the
//! machine was doing. The same binary has reported the same stage as anything
//! from 80 to 306 MB. Read the *shape* here — which step costs what, in what
//! order — and take any number that matters from the median of at least five
//! interleaved runs. Deterministic counts, `entity_bytes` and `scene_bytes`,
//! are evidence; this is a map.

// The allocator the converters run on. Measured under the system allocator
// this map answers a question no shipped binary asks — and the two disagree:
// 570 000 small allocations cost 40 MB there and nothing at all here.
#[global_allocator]
static ALLOC: mimalloc::MiMalloc = mimalloc::MiMalloc;

fn rss_mb() -> f64 {
    let pid = std::process::id();
    std::process::Command::new("ps")
        .args(["-o", "rss=", "-p", &pid.to_string()])
        .output()
        .ok()
        .and_then(|o| String::from_utf8(o.stdout).ok())
        .and_then(|s| s.trim().parse::<f64>().ok())
        .map(|kb| kb / 1024.0)
        .unwrap_or(f64::NAN)
}

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let Some(path) = std::env::args().nth(1) else {
        eprintln!("usage: read_memory <file.x_t|file.stp>");
        std::process::exit(2);
    };
    let path = std::path::Path::new(&path);
    let mut last = rss_mb();
    let step = |what: &str, last: &mut f64| {
        let now = rss_mb();
        println!("  {:<34} {:>8.0} MB   {:+.0}", what, now, now - *last);
        *last = now;
    };
    println!("{}", path.display());
    step("at the start", &mut last);

    let options = cad_convert::Options::default();
    step("after the material libraries", &mut last);

    match cad_convert::Format::of(path) {
        Some(cad_convert::Format::Step) => {
            let file = cad_step::StepFile::open(path)?;
            step("after parsing the STEP", &mut last);
            let opts = cad_step::lower::asm::LowerOptions {
                materials: options.materials.clone(),
                ..Default::default()
            };
            let (scene, _) = cad_step::lower::asm::to_scene_with(&file, &opts)?;
            step("after lowering to a scene", &mut last);
            drop(file);
            step("after dropping the parsed file", &mut last);
            println!("  ({} bodies kept)", scene.geometry.len());
            drop(scene);
            step("after dropping the scene", &mut last);
        }
        Some(cad_convert::Format::ParasolidText) => {
            // The entity graph on its own, so the parse and the lowering can be
            // told apart. `String::from_utf8_lossy` is what the readers use:
            // these files are not always valid UTF-8.
            let bytes = std::fs::read(path)?;
            step("after reading the bytes", &mut last);
            let text = xt_parser::decode(bytes.clone());
            step("after making it a string", &mut last);
            {
                let (header, body) = xt_parser::header::split_header(&text)?;
                let _ = xt_parser::header::parse_header(header)?;
                let tline = xt_parser::schema::parse_tline(body)?;
                let mut input = tline.body.as_str();
                let partitions = if tline.has_base_schema {
                    xt_parser::schema::parse_schema_preamble(&mut input)
                        .map(|p| p.partition_count)
                        .unwrap_or(0)
                } else {
                    0
                };
                let (entities, _) = xt_parser::entity::parse_entities_opt(
                    &mut input,
                    partitions,
                    tline.has_base_schema,
                    tline.key_major,
                )?;
                let fields = entities.arena_len();
                let before = last;
                step("after parsing the entity graph", &mut last);
                // Per entity means the graph's own cost, which is the step's
                // delta. Dividing total resident size by the entity count —
                // which this did — charges the graph for the file's two copies
                // and the process baseline as well, and reported 638 bytes an
                // entity where the graph's own share is nearer 490.
                println!(
                    "  ({} entities, {fields} fields between them — {:.0} bytes each, by this step's delta)",
                    entities.len(),
                    ((last - before) * 1e6) / entities.len().max(1) as f64
                );
            }
            step("after dropping the entity graph", &mut last);
            drop(text);
            drop(bytes);
            step("after dropping the text", &mut last);

            let opts = cad_xt::LowerOptions {
                materials: options.materials.clone(),
            };
            let (mut scene, _) = cad_xt::scene_from_file(path, &opts)?;
            step("after parsing and lowering", &mut last);
            println!("  ({} bodies kept)", scene.geometry.len());

            // Tessellation too, because the peak is here and not in the read,
            // and because the question this instrument exists to answer is
            // whether what the reader gave back can be used again.
            // As `convert` runs it: each body's boundary representation goes
            // back the moment its mesh exists, so the delta printed below is
            // the mesh's own cost and not the mesh plus a brep with no reader.
            let quality = cad_tess::Options {
                release_brep: true,
                ..cad_tess::Options::default()
            };
            let report = cad_tess::tessellate_scene(&mut scene, &quality);
            step("after tessellating", &mut last);
            let (mut used, mut taken) = (0usize, 0usize);
            for g in &scene.geometry {
                let Some(m) = g.mesh.as_ref() else { continue };
                used += m.positions.len() * 12 + m.normals.len() * 12
                    + m.uvs.len() * 8 + m.indices.len() * 4;
                taken += m.positions.capacity() * 12 + m.normals.capacity() * 12
                    + m.uvs.capacity() * 8 + m.indices.capacity() * 4;
            }
            println!(
                "  ({} triangles; the meshes hold {:.0} MB and were given {:.0} MB)",
                report.triangles,
                used as f64 / 1e6,
                taken as f64 / 1e6
            );
            drop(scene);
            step("after dropping the scene", &mut last);
        }
        None => return Err("not a file this reads".into()),
    }
    Ok(())
}
