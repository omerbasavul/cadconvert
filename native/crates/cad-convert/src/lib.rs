//! One call from a CAD file to a mesh file.
//!
//! Every other crate here does one job and exposes it as its own vocabulary:
//! `xt-parser` reads Parasolid text, `cad-xt` and `cad-step` lower two very
//! different formats onto one B-Rep, `cad-gltf` reads a mesh that needs no
//! lowering, `cad-tess` meshes what does, `cad-export` writes it out. That
//! layering is right for the work and wrong for a caller, who has one question
//! — *turn this file into that one* — and should not have to learn six crates
//! to ask it.
//!
//! This crate is the answer to that question and nothing else. It holds no
//! geometry of its own; it chooses a reader by looking at the file, hands the
//! scene to the tessellator, and writes the result — to one file, or to
//! several from the same reading, since a caller who wants a glTF and a USDZ
//! of the same part has no reason to read and mesh it twice. It is also the
//! only thing the C ABI and the .NET package need to wrap, which is why it
//! exists as a crate rather than as a function in a binary.

#![forbid(unsafe_code)]

use std::path::{Path, PathBuf};

/// Which reader a file needs.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Format {
    /// Parasolid transmit, text (`.x_t`, `.xmt_txt`).
    ParasolidText,
    /// ISO 10303-21 (`.stp`, `.step`).
    Step,
    /// glTF 2.0, binary or JSON (`.glb`, `.gltf`). Already a mesh: read and
    /// written, never tessellated.
    Gltf,
}

impl Format {
    /// The format of a file, from its extension and then from its first line.
    ///
    /// The extension is a convention and the header is a fact, so the header
    /// wins where it says anything: a `.stp` that begins `**ABCDEF` is a
    /// Parasolid file someone renamed, and reading it as STEP fails with a
    /// parse error that says nothing useful.
    pub fn of(path: &Path) -> Option<Format> {
        let bytes = std::fs::read(path).unwrap_or_default();
        let bytes = &bytes[..bytes.len().min(512)];
        // A binary glTF opens with its four magic bytes, before any text.
        if bytes.starts_with(b"glTF") {
            return Some(Format::Gltf);
        }
        let head = String::from_utf8_lossy(bytes).into_owned();
        let head = head.trim_start();
        if head.starts_with("ISO-10303-21") {
            return Some(Format::Step);
        }
        // A Parasolid transmit file opens with its own banner.
        if head.starts_with("**ABCDEFGHIJKLMNOPQRSTUVWXYZ") || head.contains("PS_SCHEMA") {
            return Some(Format::ParasolidText);
        }
        // A JSON glTF is an object whose one required member is `asset`.
        if head.starts_with('{') && head.contains("\"asset\"") {
            return Some(Format::Gltf);
        }
        match path
            .extension()
            .map(|e| e.to_string_lossy().to_lowercase())
            .as_deref()
        {
            Some("x_t" | "xmt_txt") => Some(Format::ParasolidText),
            Some("stp" | "step" | "p21") => Some(Format::Step),
            Some("glb" | "gltf") => Some(Format::Gltf),
            _ => None,
        }
    }
}

/// What to write.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Target {
    /// glTF binary, positions and normals exactly as computed.
    #[default]
    Glb,
    /// glTF binary with normals encoded a byte a component. Every vertex is
    /// where it was; see [`cad_export::Compression::Normals`].
    GlbLean,
    /// glTF binary with positions on each mesh's own 16-bit grid as well.
    GlbCompact,
    /// A USDZ package: USD in its text form, with the images beside it.
    ///
    /// The same scene and the same materials — `UsdPreviewSurface` and glTF's
    /// metallic-roughness model are one model with two spellings. It costs
    /// size: USD's text form spells every coordinate out, so the file runs
    /// several times its glTF.
    Usdz,
}

impl Target {
    /// The extension a target writes.
    pub fn extension(self) -> &'static str {
        match self {
            Target::Usdz => "usdz",
            _ => "glb",
        }
    }

    /// The target a file name asks for, where it asks for one.
    ///
    /// Only the container. Which of the three glTF targets to use is a
    /// question about size and precision that a file name does not answer.
    pub fn of_extension(path: &Path) -> Option<Target> {
        match path
            .extension()
            .map(|e| e.to_string_lossy().to_lowercase())
            .as_deref()
        {
            Some("usdz" | "usda" | "usdc" | "usd") => Some(Target::Usdz),
            Some("glb" | "gltf") => Some(Target::Glb),
            _ => None,
        }
    }

    /// The target an output file gets: its own extension where it has one,
    /// the caller's choice where it does not.
    ///
    /// A caller who asks for `out.usdz` and a glTF target meant the USDZ:
    /// the extension is the explicit statement and the target's default is
    /// not. The other way round, a USDZ target and a `.glb` name means the
    /// plain glTF — the caller said nothing about compression.
    fn for_output(self, output: &Path) -> Target {
        match Target::of_extension(output) {
            Some(Target::Usdz) => Target::Usdz,
            Some(_) if self == Target::Usdz => Target::Glb,
            _ => self,
        }
    }
}

/// How finely to mesh, and what to write.
#[derive(Debug, Clone)]
pub struct Options {
    pub quality: cad_tess::Options,
    pub target: Target,
    /// Resolve materials with this; the default reads the bundled SolidWorks
    /// library and the designer's own colours.
    pub materials: cad_ir::MaterialResolver,
    /// A Parasolid twin of a STEP file carries per-face reflectivity the STEP
    /// dropped. Looked for beside the input as `<stem>.x_t` unless this is off.
    pub use_parasolid_twin: bool,
    /// Write USD in its text form rather than its binary one. Only reaches a
    /// USDZ; glTF has one encoding.
    pub usd_text: bool,
}

impl Default for Options {
    fn default() -> Self {
        Options {
            quality: cad_tess::Options::default(),
            target: Target::default(),
            materials: cad_ir::MaterialResolver::default(),
            use_parasolid_twin: true,
            usd_text: false,
        }
    }
}

/// One file a conversion wrote.
#[derive(Debug, Clone, Default)]
pub struct Written {
    pub path: PathBuf,
    pub bytes: u64,
}

/// What one conversion produced.
#[derive(Debug, Clone, Default)]
pub struct Summary {
    /// The first output. A conversion with one output has one; see
    /// [`Summary::outputs`] for the rest.
    pub output: PathBuf,
    /// Every output's size, added up.
    pub bytes: u64,
    /// Each file written, in the order asked for.
    pub outputs: Vec<Written>,
    pub bodies: usize,
    /// Faces the file declared. Zero for a file that arrived as a mesh, which
    /// has triangles and no faces.
    pub faces: usize,
    pub faces_meshed: usize,
    pub triangles: usize,
    /// Anything the readers or the tessellator could not do, in words. A
    /// conversion that produced a file and a warning is not a failure, and
    /// silently dropping the warning is how a caller ships a hole.
    pub warnings: Vec<String>,
}

/// Where a conversion is.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Stage {
    /// Reading the input. One unit.
    Read,
    /// Meshing. A unit is a body with a boundary representation; a file that
    /// arrived meshed has none, and the stage reports zero of zero.
    Mesh,
    /// Writing the outputs. A unit is a file.
    Write,
}

/// A report between units of work, on the calling thread.
///
/// `done` of `total` units are finished; `detail` names the unit about to
/// start — the input, a body, an output path — and is empty when `done ==
/// total`. Every stage reports at its start and after each unit, so a caller
/// who shows the last report sees "meshing body 3 of 46: bracket" and then
/// "writing 1 of 2: part.usdz" without keeping any state of its own.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Progress<'a> {
    pub stage: Stage,
    pub done: usize,
    pub total: usize,
    pub detail: &'a str,
}

#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("{0} is not a file this converts: expected Parasolid text, ISO 10303-21 or glTF")]
    UnknownFormat(PathBuf),
    #[error("reading {path}: {detail}")]
    Read { path: PathBuf, detail: String },
    #[error("writing {path}: {detail}")]
    Write { path: PathBuf, detail: String },
    /// The caller's progress callback said stop. Outputs written before that
    /// point are on disk; the rest were never started.
    #[error("cancelled while {stage:?}")]
    Cancelled { stage: Stage },
    /// No output at all was asked for.
    #[error("no output path was given")]
    NoOutput,
}

pub type Result<T> = std::result::Result<T, Error>;

/// Read `input`, mesh it, and write it to `output`.
pub fn convert(input: &Path, output: &Path, options: &Options) -> Result<Summary> {
    convert_many(input, &[output.to_path_buf()], options, &mut |_| true)
}

/// Read `input` once, mesh it once, and write it to every path in `outputs`.
///
/// Each output's container comes from its own extension, so `[part.glb,
/// part.usdz]` is the usual request. `progress` is told where the work is
/// between units, on this thread; returning `false` from it stops the
/// conversion with [`Error::Cancelled`].
pub fn convert_many(
    input: &Path,
    outputs: &[PathBuf],
    options: &Options,
    progress: &mut dyn FnMut(&Progress) -> bool,
) -> Result<Summary> {
    if outputs.is_empty() {
        return Err(Error::NoOutput);
    }
    let stop = |stage: Stage| Error::Cancelled { stage };

    // ── read ──
    let input_name = input.display().to_string();
    if !progress(&Progress { stage: Stage::Read, done: 0, total: 1, detail: &input_name }) {
        return Err(stop(Stage::Read));
    }
    let (mut scene, mut warnings) = read_scene(input, options)?;
    if !progress(&Progress { stage: Stage::Read, done: 1, total: 1, detail: "" }) {
        return Err(stop(Stage::Read));
    }
    let mut summary = Summary {
        output: outputs[0].clone(),
        bodies: scene.geometry.len(),
        ..Default::default()
    };

    // ── mesh ──
    // This function writes files and drops the scene; the exact geometry
    // cannot be observed after it returns, so there is no caller to surprise
    // by handing it back early. `read` is where a caller who wants the
    // boundary representation goes.
    let quality = cad_tess::Options {
        release_brep: true,
        ..options.quality
    };
    let names: Vec<String> = scene
        .geometry
        .iter()
        .filter(|g| g.brep.is_some())
        .map(|g| g.name.clone())
        .collect();
    let report = cad_tess::tessellate_scene_with(&mut scene, &quality, &mut |done, total| {
        let detail = names.get(done).map(String::as_str).unwrap_or("");
        progress(&Progress { stage: Stage::Mesh, done, total, detail })
    });
    if report.cancelled {
        return Err(stop(Stage::Mesh));
    }
    summary.faces = report.faces_ok + report.failed.len();
    summary.faces_meshed = report.faces_ok;
    // What is there to write, whichever reader put it there: the tessellator's
    // count is only the triangles it made, and a glTF arrives with its own.
    summary.triangles = scene.stored_triangle_count();
    if !report.failed.is_empty() {
        warnings.push(format!(
            "{} of {} faces could not be meshed",
            report.failed.len(),
            summary.faces
        ));
        // Naming a few of them is the difference between a caller who can look
        // and one who can only shrug. The rest are counted above.
        for f in report.failed.iter().take(5) {
            warnings.push(format!("  {} face {}: {}", f.geometry, f.face.0, f.reason));
        }
    }

    // Textures after meshing, coordinates after textures. The appearance
    // library states a physical tile size, so the coordinates are a projection
    // at world scale — and there is no point computing them for a mesh whose
    // materials name no image, which is most of them. A mesh that arrived
    // with coordinates of its own keeps them: they are the file's statement
    // of where its images sit, and a projection would paint over it.
    warnings.extend(cad_ir::material_resolve::attach_appearance_textures(&mut scene));
    let textured: Vec<u32> = scene
        .materials
        .iter()
        .enumerate()
        .filter(|(_, m)| !m.textures.is_empty())
        .map(|(i, _)| i as u32)
        .collect();
    if !textured.is_empty() {
        for geometry in &mut scene.geometry {
            let Some(mesh) = geometry.mesh.as_mut() else {
                continue;
            };
            if mesh.uvs.is_empty() && mesh.parts.iter().any(|p| textured.contains(&p.material)) {
                cad_ir::uv::project(mesh);
            }
        }
    }
    summary.warnings = warnings;

    // ── write ──
    let total = outputs.len();
    for (i, output) in outputs.iter().enumerate() {
        let name = output.display().to_string();
        if !progress(&Progress { stage: Stage::Write, done: i, total, detail: &name }) {
            return Err(stop(Stage::Write));
        }
        let target = options.target.for_output(output);
        let write = cad_export::Options {
            compression: match target {
                Target::GlbLean => cad_export::Compression::Normals,
                Target::GlbCompact => cad_export::Compression::Quantized,
                // USD's text form has no encoding to choose; it writes what it
                // is given.
                Target::Glb | Target::Usdz => cad_export::Compression::None,
            },
            usd_text: options.usd_text,
            ..cad_export::Options::default()
        };
        let written = match target {
            Target::Usdz => cad_export::usd::write_file(&scene, &write, output),
            _ => cad_export::glb::write_file(&scene, &write, output),
        };
        let bytes = written.map_err(|e| Error::Write {
            path: output.clone(),
            detail: e.to_string(),
        })?;
        summary.bytes += bytes;
        summary.outputs.push(Written {
            path: output.clone(),
            bytes,
        });
    }
    if !progress(&Progress { stage: Stage::Write, done: total, total, detail: "" }) {
        return Err(stop(Stage::Write));
    }
    Ok(summary)
}

/// Read a file into a scene, without meshing it.
///
/// A glTF comes back already meshed, since that is what it holds.
pub fn read(input: &Path, options: &Options) -> Result<cad_ir::Scene> {
    read_scene(input, options).map(|(scene, _)| scene)
}

/// The scene and what the reader could not carry across.
fn read_scene(input: &Path, options: &Options) -> Result<(cad_ir::Scene, Vec<String>)> {
    let failed = |e: &dyn std::fmt::Display| Error::Read {
        path: input.to_path_buf(),
        detail: e.to_string(),
    };
    match Format::of(input).ok_or_else(|| Error::UnknownFormat(input.to_path_buf()))? {
        Format::ParasolidText => {
            let opts = cad_xt::LowerOptions {
                materials: options.materials.clone(),
            };
            let (scene, _report) = cad_xt::scene_from_file(input, &opts).map_err(|e| failed(&e))?;
            Ok((scene, Vec::new()))
        }
        Format::Step => {
            let file = cad_step::StepFile::open(input).map_err(|e| failed(&e))?;
            let mut opts = cad_step::lower::asm::LowerOptions {
                materials: options.materials.clone(),
                ..Default::default()
            };
            // The STEP carries a colour per face and nothing about finish. Its
            // Parasolid twin, where the exporter wrote one, states the
            // designer's own metal-versus-matte per face — the one finish
            // signal in either file that is not inferred.
            if options.use_parasolid_twin {
                let twin = input.with_extension("x_t");
                if twin.exists()
                    && let Ok(bytes) = std::fs::read(&twin)
                {
                    // The twin is tens of megabytes and is read for fourteen
                    // colour-to-finish lines. `decode` takes the bytes by
                    // value, so a clean file becomes the string it already is
                    // and a dirty one is converted into a buffer sized in
                    // advance rather than doubled into.
                    let text = xt_parser::decode(bytes);
                    // By value: the STEP's own 36 MB buffer is resident for
                    // the whole of this, and holding the twin's text as well
                    // put the STEP read peak at 176 MB where the STEP alone is
                    // 80.
                    if let Ok(hints) = xt_parser::appearance::appearance_hints_owned(text) {
                        opts.materials.reflectivity_by_colour = hints.reflectivity_by_colour();
                    }
                }
            }
            let (scene, _report) =
                cad_step::lower::asm::to_scene_with(&file, &opts).map_err(|e| failed(&e))?;
            Ok((scene, Vec::new()))
        }
        Format::Gltf => {
            let (scene, report) = cad_gltf::scene_from_file(input).map_err(|e| failed(&e))?;
            Ok((scene, report.warnings))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn scratch(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join("cad-convert-tests");
        let _ = std::fs::create_dir_all(&dir);
        dir.join(name)
    }

    fn sample() -> PathBuf {
        Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/samples/small.x_t")
    }

    #[test]
    fn the_header_outranks_the_extension() {
        let step_named_xt = scratch("mislabelled.x_t");
        std::fs::write(&step_named_xt, "ISO-10303-21;\nHEADER;\n").unwrap();
        assert_eq!(Format::of(&step_named_xt), Some(Format::Step));
        let _ = std::fs::remove_file(&step_named_xt);
    }

    #[test]
    fn an_extension_decides_when_the_header_says_nothing() {
        let f = scratch("plain.stp");
        std::fs::write(&f, "nothing recognisable here").unwrap();
        assert_eq!(Format::of(&f), Some(Format::Step));
        let g = scratch("plain.dxf");
        std::fs::write(&g, "nothing recognisable here").unwrap();
        assert_eq!(Format::of(&g), None);
        let _ = std::fs::remove_file(&f);
        let _ = std::fs::remove_file(&g);
    }

    #[test]
    fn a_gltf_is_known_by_its_magic_or_its_json_whatever_it_is_called() {
        let glb = scratch("renamed.bin");
        std::fs::write(&glb, b"glTF\x02\x00\x00\x00").unwrap();
        assert_eq!(Format::of(&glb), Some(Format::Gltf));
        let json = scratch("renamed.json");
        std::fs::write(&json, r#"{"asset":{"version":"2.0"}}"#).unwrap();
        assert_eq!(Format::of(&json), Some(Format::Gltf));
        let _ = std::fs::remove_file(&glb);
        let _ = std::fs::remove_file(&json);
    }

    #[test]
    fn one_reading_writes_every_output() {
        let outputs = [scratch("many.glb"), scratch("many.usdz")];
        let s = convert_many(&sample(), &outputs, &Options::default(), &mut |_| true).unwrap();
        assert_eq!(s.outputs.len(), 2);
        assert_eq!(s.output, outputs[0]);
        assert_eq!(s.bytes, s.outputs.iter().map(|w| w.bytes).sum::<u64>());
        for (w, path) in s.outputs.iter().zip(&outputs) {
            assert_eq!(&w.path, path);
            assert_eq!(std::fs::metadata(path).unwrap().len(), w.bytes);
        }
        assert!(std::fs::read(&outputs[0]).unwrap().starts_with(b"glTF"));
        assert!(std::fs::read(&outputs[1]).unwrap().starts_with(b"PK"));
        for path in &outputs {
            let _ = std::fs::remove_file(path);
        }
    }

    #[test]
    fn progress_walks_read_then_mesh_then_write_and_ends_complete() {
        let outputs = [scratch("progress.glb"), scratch("progress.usdz")];
        let mut seen: Vec<(Stage, usize, usize, String)> = Vec::new();
        convert_many(&sample(), &outputs, &Options::default(), &mut |p| {
            seen.push((p.stage, p.done, p.total, p.detail.to_string()));
            true
        })
        .unwrap();
        let stages: Vec<Stage> = seen.iter().map(|s| s.0).collect();
        let mut order = stages.clone();
        order.dedup();
        assert_eq!(order, [Stage::Read, Stage::Mesh, Stage::Write]);
        // Every stage opens at zero and closes complete.
        for stage in [Stage::Read, Stage::Mesh, Stage::Write] {
            let of_stage: Vec<_> = seen.iter().filter(|s| s.0 == stage).collect();
            assert_eq!(of_stage.first().unwrap().1, 0, "{stage:?} opens at zero");
            let last = of_stage.last().unwrap();
            assert_eq!(last.1, last.2, "{stage:?} closes complete");
            assert!(last.3.is_empty(), "nothing is in flight at the end of {stage:?}");
        }
        // Bodies are named while they are meshed, outputs while written.
        assert!(seen.iter().any(|s| s.0 == Stage::Mesh && !s.3.is_empty()));
        assert!(seen.iter().any(|s| s.0 == Stage::Write && s.3.ends_with("progress.usdz")));
        for path in &outputs {
            let _ = std::fs::remove_file(path);
        }
    }

    #[test]
    fn a_cancel_while_meshing_writes_nothing() {
        let output = scratch("cancelled.glb");
        let _ = std::fs::remove_file(&output);
        let err = convert_many(&sample(), &[output.clone()], &Options::default(), &mut |p| {
            p.stage != Stage::Mesh
        })
        .err()
        .expect("stopped");
        assert!(matches!(err, Error::Cancelled { stage: Stage::Mesh }), "{err}");
        assert!(!output.exists(), "a cancelled conversion leaves no file");
    }

    #[test]
    fn a_glb_converts_onward_to_a_usdz_with_the_same_triangles() {
        let glb = scratch("onward.glb");
        let from_cad = convert(&sample(), &glb, &Options::default()).unwrap();
        let usdz = scratch("onward.usdz");
        let from_glb = convert(&glb, &usdz, &Options::default()).unwrap();
        assert_eq!(from_glb.triangles, from_cad.triangles);
        assert_eq!(from_glb.bodies, from_cad.bodies);
        assert_eq!(from_glb.faces, 0, "a mesh has triangles and no faces");
        assert!(std::fs::read(&usdz).unwrap().starts_with(b"PK"));
        let _ = std::fs::remove_file(&glb);
        let _ = std::fs::remove_file(&usdz);
    }

    #[test]
    fn asking_for_no_output_is_refused_before_anything_is_read() {
        let err = convert_many(&sample(), &[], &Options::default(), &mut |_| true)
            .err()
            .unwrap();
        assert!(matches!(err, Error::NoOutput));
    }
}
