//! What the bundled SolidWorks material library contains.
//!
//! Reads the copy compiled into the crate rather than one on disk: there is
//! only one library, it travels with the binary, and a second copy at a path
//! that only resolves from the repository root is a copy that will drift.
fn main() {
    let lib = cad_ir::sldmat::SldLibrary::bundled();
    println!("materials: {}", lib.materials.len());
    let mut by_class: std::collections::BTreeMap<String, (usize, usize)> = Default::default();
    for m in lib.materials.values() {
        let e = by_class.entry(m.classification.clone()).or_default();
        e.0 += 1;
        if m.is_metal() { e.1 += 1; }
    }
    for (c, (n, metal)) in &by_class {
        println!("  {c:<26} {n:>4} materials, {metal:>4} metal");
    }
    // Every entry, with the finish its shader names beside the roughness its
    // optics produce. Where the two disagree the library is arguing with
    // itself, and that is what this listing is for.
    let polished = ["polish", "chrome", "verchrom", "plate", "mirror", "shiny"];
    let matte = ["cast", "galvan", "brushed", "satin", "dull", "burnish", "soft", "wrought"];
    let mut rows: Vec<_> = lib.materials.values().collect();
    rows.sort_by(|a, b| a.name.cmp(&b.name));
    println!("\n  {:<34} {:>6} {:>7}  {}", "material", "metal", "rough", "shaders");
    let mut odd = Vec::new();
    for m in &rows {
        let g = m.to_material();
        let says_polished = polished.iter().any(|t| m.shader_names.contains(t));
        let says_matte = matte.iter().any(|t| m.shader_names.contains(t));
        let flag = if says_polished && g.roughness > 0.35 {
            odd.push((m.name.clone(), g.roughness, "its shader says polished"));
            "  <-- polished, but rough"
        } else if says_matte && g.roughness < 0.15 {
            odd.push((m.name.clone(), g.roughness, "its shader says a matte finish"));
            "  <-- matte finish, but mirror"
        } else {
            ""
        };
        if std::env::args().any(|a| a == "--all") || !flag.is_empty() {
            println!("  {:<34} {:>6.0} {:>7.3}  {}{flag}", m.name, g.metallic, g.roughness, m.shader_names.trim());
        }
    }
    println!("\n  entries whose optics and shader disagree: {}", odd.len());
}
