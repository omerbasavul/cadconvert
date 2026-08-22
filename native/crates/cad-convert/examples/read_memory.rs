//! Resident memory at each step of reading a file.
//!
//! Peak resident size after reading the pilot is around 550 MB while the scene
//! it produces holds 19 MB — see `scene_bytes`. The difference is transient,
//! and a peak measured from outside cannot say which step reached it. This
//! prints the resident size at each step so it can.
//!
//! `ps` is asked rather than a crate: this is a diagnostic that runs by hand on
//! a developer's machine, and a dependency for it would ship to every user.

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
            let text = String::from_utf8_lossy(&bytes).into_owned();
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
                let fields: usize = entities.iter().map(|e| e.fields.len()).sum();
                step("after parsing the entity graph", &mut last);
                println!(
                    "  ({} entities, {fields} fields between them — {:.0} bytes each as parsed)",
                    entities.len(),
                    (rss_mb() * 1e6) / entities.len().max(1) as f64
                );
            }
            step("after dropping the entity graph", &mut last);
            drop(text);
            drop(bytes);
            step("after dropping the text", &mut last);

            let opts = cad_xt::LowerOptions {
                materials: options.materials.clone(),
            };
            let (scene, _) = cad_xt::scene_from_file(path, &opts)?;
            step("after parsing and lowering", &mut last);
            println!("  ({} bodies kept)", scene.geometry.len());
            drop(scene);
            step("after dropping the scene", &mut last);
        }
        None => return Err("not a file this reads".into()),
    }
    Ok(())
}
