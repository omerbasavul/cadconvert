//! Report what a STEP file contains: timing, keyword histogram, header fields,
//! units, colours and the assembly's product names.
//!
//! `cargo run --release -p cad-step --example step_info -- file.stp`

use cad_step::{Kind, StepFile};
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

    println!("\n-- colours --");
    for e in file.by_kind(Kind::ColourRgb) {
        let mut a = file.args_of(e);
        let name = a.next_str()?;
        let (r, g, b) = (a.next_f64()?, a.next_f64()?, a.next_f64()?);
        println!(
            "  #{:<8} rgb({:.3}, {:.3}, {:.3})  sRGB #{:02X}{:02X}{:02X}  {name}",
            e.id,
            r,
            g,
            b,
            (r.clamp(0.0, 1.0) * 255.0).round() as u8,
            (g.clamp(0.0, 1.0) * 255.0).round() as u8,
            (b.clamp(0.0, 1.0) * 255.0).round() as u8,
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
