//! Report what a STEP file contains: timing, keyword histogram, header fields,
//! units, colours and the assembly's product names.
//!
//! `cargo run --release -p cad-step --example step_info -- file.stp`

use cad_step::{Kind, StepFile, presentation, units};
use std::time::Instant;

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let Some(path) = std::env::args().nth(1) else {
        eprintln!("usage: step_info <file.stp>");
        std::process::exit(2);
    };

    let bytes = std::fs::metadata(&path).map(|m| m.len()).unwrap_or(0);
    let t0 = Instant::now();
    let file = StepFile::open(&path)?;
    let elapsed = t0.elapsed();

    println!("file          {path}");
    println!(
        "size          {:.1} MB   scanned in {:.1} ms  ({:.0} MB/s)",
        bytes as f64 / 1e6,
        elapsed.as_secs_f64() * 1e3,
        bytes as f64 / 1e6 / elapsed.as_secs_f64()
    );
    println!("entities      {}", file.len());
    println!("name          {}", file.file_name());
    println!("written by    {}", file.originating_system());
    for s in file.schemas() {
        println!("schema        {s}");
    }

    println!("\n-- top keywords --");
    for (kw, n) in file.keyword_histogram().iter().take(18) {
        println!("{n:>8}  {kw}");
    }

    let unmodelled: Vec<_> = file
        .keyword_histogram()
        .into_iter()
        .filter(|(kw, _)| Kind::intern(kw.as_bytes()) == Kind::Other && kw != "(complex)")
        .collect();
    if !unmodelled.is_empty() {
        let total: usize = unmodelled.iter().map(|(_, n)| n).sum();
        println!("\n-- unmodelled keywords ({} kinds, {total} instances) --", unmodelled.len());
        for (kw, n) in unmodelled.iter().take(12) {
            println!("{n:>8}  {kw}");
        }
    }

    let t1 = Instant::now();
    let u = units::resolve(&file)?;
    let styles = presentation::resolve(&file)?;
    let resolve_ms = t1.elapsed().as_secs_f64() * 1e3;

    println!("\n-- units --");
    println!(
        "  length      x{} -> mm{}",
        u.length_to_mm,
        if u.resolved { "" } else { "  (ASSUMED - no unit found)" }
    );
    println!("  angle       x{} -> rad", u.angle_to_rad);
    println!("  tolerance   {} file units", u.uncertainty);

    println!("\n-- resolved styles --   ({resolve_ms:.1} ms)");
    println!("  {} items carry an appearance", styles.len());
    if !styles.unresolved.is_empty() {
        println!("  {} styled items yielded no colour", styles.unresolved.len());
    }
    let mut by_target: std::collections::BTreeMap<&str, usize> = Default::default();
    for e in file.by_kind(Kind::StyledItem) {
        let mut a = file.args_of(e);
        a.skip()?;
        a.skip()?;
        if let Ok(item) = a.next_ref() {
            let k = file.kind_of(item);
            let name = if k == Kind::Other {
                file.get(item).map(|x| file.keyword(x)).unwrap_or("(dangling)")
            } else {
                k.as_str()
            };
            *by_target.entry(name).or_default() += 1;
        }
    }
    println!("  styled item targets:");
    for (k, n) in &by_target {
        println!("    {n:>7}  {k}");
    }
    println!("  palette:");
    for (app, n) in styles.palette() {
        println!(
            "    {n:>7} items  sRGB #{}  linear({:.4},{:.4},{:.4})  alpha {:.2}",
            app.srgb_hex(),
            app.linear_rgb()[0],
            app.linear_rgb()[1],
            app.linear_rgb()[2],
            app.alpha
        );
    }

    println!("\n-- colours --");
    for e in file.by_kind(Kind::ColourRgb) {
        let mut a = file.args_of(e);
        let name = a.next_str()?;
        let (r, g, b) = (a.next_f64()?, a.next_f64()?, a.next_f64()?);
        println!(
            "  #{:<8} file({r:.4}, {g:.4}, {b:.4})  sRGB #{}  {name}",
            e.id,
            cad_step::Appearance {
                rgb: [r as f32, g as f32, b as f32],
                alpha: 1.0
            }
            .srgb_hex(),
        );
    }

    println!("\n-- products --");
    let mut names = Vec::new();
    for e in file.by_kind(Kind::Product) {
        let mut a = file.args_of(e);
        let id = a.next_str()?;
        let name = a.next_str()?;
        names.push(if id == name {
            id.into_owned()
        } else {
            format!("{id} ({name})")
        });
    }
    println!("  {} products", names.len());
    for n in names.iter().take(10) {
        println!("    {n}");
    }
    if names.len() > 10 {
        println!("    … {} more", names.len() - 10);
    }

    println!("\n-- shape carriers --");
    for k in [
        Kind::ManifoldSolidBrep,
        Kind::BrepWithVoids,
        Kind::ShellBasedSurfaceModel,
        Kind::ClosedShell,
        Kind::OpenShell,
        Kind::AdvancedFace,
        Kind::FaceSurface,
        Kind::NextAssemblyUsageOccurrence,
        Kind::StyledItem,
    ] {
        println!("{:>8}  {}", file.by_kind(k).count(), k.as_str());
    }

    Ok(())
}
