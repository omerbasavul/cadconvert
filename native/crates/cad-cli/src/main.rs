//! `cadconvert` — turn a CAD file into a mesh file.
//!
//! Arguments are parsed by hand rather than with a crate. The whole surface is
//! an input, an output and four numbers; a dependency to read them would be
//! larger than the program.

// mimalloc was tried here as a global allocator, on the reading that the
// converter's millions of small short-lived allocations are what set the
// process's high-water mark. Measured on the pilot, it is worse: whole
// conversion of the STEP 346–450 MB → 524–549 MB, of the Parasolid 395–496 →
// 440–523. It trades memory for speed by holding freed pages, and memory is
// what this needed. The system allocator stays.
use std::path::PathBuf;
use std::process::ExitCode;

/// How far to take the work.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Phase {
    Read,
    Mesh,
}

const USAGE: &str = "\
cadconvert — Parasolid (.x_t) and STEP (.stp) to glTF binary or USDZ

  cadconvert <input> [output] [options]

  The output's extension chooses the container: .glb for glTF binary,
  .usdz for a USD package. Without an output, the input's name with .glb.

  -q, --quality <plain|lean|compact>   how a glTF stores its vertices
                                       (default: lean). USD has no such
                                       choice and ignores this.
      --sag <mm>                       how far the mesh may sit from the
                                       surface. The default is 0.04% of the
                                       model's own diagonal, which scales with
                                       the part; giving a number here fixes it
                                       in millimetres instead.
      --angle <deg>                    largest angle between adjacent facet
                                       normals (default: 8)
      --materials <file>               a table of colour and part-number rules,
                                       which outranks every guess below it. It
                                       is also how a STEP is given the finish
                                       its .x_t twin would have supplied,
                                       without reading the twin.
      --no-twin                        do not read a STEP file's .x_t twin for
                                       the designer's metal/matte
      --stop-after <read|mesh>         do the work and stop, writing nothing.
                                       Reading alone validates a file; stopping
                                       after meshing is how the memory each
                                       phase costs is measured, since peak
                                       resident size only ever grows.
  -h, --help                           this

`lean` encodes normals a byte a component and leaves every vertex exactly where
it was computed; `compact` also puts positions on each mesh's own 16-bit grid,
which is smaller and collapses the finest slivers. `plain` compresses nothing.
";

fn main() -> ExitCode {
    match run() {
        Ok(()) => ExitCode::SUCCESS,
        Err(message) => {
            eprintln!("cadconvert: {message}");
            ExitCode::FAILURE
        }
    }
}

fn run() -> Result<(), String> {
    let mut args = std::env::args().skip(1);
    let mut input: Option<PathBuf> = None;
    let mut output: Option<PathBuf> = None;
    let mut options = cad_convert::Options {
        target: cad_convert::Target::GlbLean,
        ..Default::default()
    };
    // Unset means the tessellator's own default, which is measured rather
    // than chosen: 0.04% of the model diagonal and 8° between facet normals.
    // An earlier version of this program invented 0.05 mm and 20° here, which
    // is eight times coarser and left 607 open half-edges in the pilot — the
    // converter's defaults must not be the one place the mesh is filed down.
    let mut sag: Option<f64> = None;
    let mut angle_deg: Option<f64> = None;
    let mut stop_after: Option<Phase> = None;

    while let Some(arg) = args.next() {
        let mut value = || {
            args.next()
                .ok_or_else(|| format!("{arg} needs a value"))
        };
        match arg.as_str() {
            "-h" | "--help" => {
                print!("{USAGE}");
                return Ok(());
            }
            "-q" | "--quality" => {
                options.target = match value()?.as_str() {
                    "plain" => cad_convert::Target::Glb,
                    "lean" => cad_convert::Target::GlbLean,
                    "compact" => cad_convert::Target::GlbCompact,
                    "usdz" => cad_convert::Target::Usdz,
                    other => return Err(format!("unknown quality {other:?}")),
                }
            }
            "--sag" => sag = Some(value()?.parse().map_err(|_| "--sag wants a number")?),
            "--angle" => {
                angle_deg = Some(value()?.parse().map_err(|_| "--angle wants a number")?)
            }
            "--materials" => {
                let path = value()?;
                let text = std::fs::read_to_string(&path)
                    .map_err(|e| format!("reading {path}: {e}"))?;
                let (table, errors) = cad_ir::MaterialTable::parse(&text);
                for e in &errors {
                    eprintln!("cadconvert: {path}: {e}");
                }
                options.materials.table = table;
            }
            "--no-twin" => options.use_parasolid_twin = false,
            "--stop-after" => {
                stop_after = Some(match value()?.as_str() {
                    "read" => Phase::Read,
                    "mesh" => Phase::Mesh,
                    other => return Err(format!("--stop-after wants read or mesh, not {other:?}")),
                })
            }
            other if other.starts_with('-') => return Err(format!("unknown option {other:?}")),
            other if input.is_none() => input = Some(PathBuf::from(other)),
            other if output.is_none() => output = Some(PathBuf::from(other)),
            other => return Err(format!("unexpected argument {other:?}")),
        }
    }

    let Some(input) = input else {
        print!("{USAGE}");
        return Err("no input file".into());
    };
    let output = output
        .unwrap_or_else(|| input.with_extension(options.target.extension()));

    if let Some(mm) = sag {
        // A number in millimetres is absolute; the default is a fraction.
        options.quality.linear_deflection = mm;
        options.quality.relative = false;
    }
    if let Some(deg) = angle_deg {
        options.quality.angular_deflection = deg.to_radians();
    }

    let started = std::time::Instant::now();

    // Stopping early writes nothing, and is how each phase's cost is
    // attributed: peak resident size only ever grows, so reading alone gives
    // the reader's peak, reading and meshing gives the larger of the two, and
    // the whole run gives the mesh writer's.
    if let Some(phase) = stop_after {
        let mut scene = cad_convert::read(&input, &options).map_err(|e| e.to_string())?;
        let bodies = scene.geometry.len();
        let mut triangles = 0;
        if phase == Phase::Mesh {
            let report = cad_tess::tessellate_scene(&mut scene, &options.quality);
            triangles = report.triangles;
        }
        println!(
            "{} read{}: {bodies} bodies, {triangles} triangles, nothing written, {:.1} s",
            input.display(),
            if phase == Phase::Mesh { " and meshed" } else { "" },
            started.elapsed().as_secs_f64()
        );
        // The scene is dropped after the timing is printed, so the peak an
        // external measurement sees is the peak this phase actually reached.
        drop(scene);
        return Ok(());
    }

    let summary = cad_convert::convert(&input, &output, &options).map_err(|e| e.to_string())?;
    let elapsed = started.elapsed().as_secs_f64();

    println!(
        "{} -> {}  {} bodies, {}/{} faces, {} triangles, {:.2} MB in {elapsed:.1} s",
        input.display(),
        summary.output.display(),
        summary.bodies,
        summary.faces_meshed,
        summary.faces,
        summary.triangles,
        summary.bytes as f64 / 1e6,
    );
    for w in &summary.warnings {
        eprintln!("  warning: {w}");
    }
    Ok(())
}
