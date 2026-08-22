//! One call from a CAD file to a mesh file.
//!
//! Every other crate here does one job and exposes it as its own vocabulary:
//! `xt-parser` reads Parasolid text, `cad-xt` and `cad-step` lower two very
//! different formats onto one B-Rep, `cad-tess` meshes it, `cad-export` writes
//! it out. That layering is right for the work and wrong for a caller, who has
//! one question — *turn this file into that one* — and should not have to
//! learn five crates to ask it.
//!
//! This crate is the answer to that question and nothing else. It holds no
//! geometry of its own; it chooses a reader by looking at the file, hands the
//! scene to the tessellator, and writes the result. It is also the only thing
//! the C ABI and the .NET package need to wrap, which is why it exists as a
//! crate rather than as a function in a binary.

#![forbid(unsafe_code)]

use std::path::{Path, PathBuf};

/// Which reader a file needs.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Format {
    /// Parasolid transmit, text (`.x_t`, `.xmt_txt`).
    ParasolidText,
    /// ISO 10303-21 (`.stp`, `.step`).
    Step,
}

impl Format {
    /// The format of a file, from its extension and then from its first line.
    ///
    /// The extension is a convention and the header is a fact, so the header
    /// wins where it says anything: a `.stp` that begins `**ABCDEF` is a
    /// Parasolid file someone renamed, and reading it as STEP fails with a
    /// parse error that says nothing useful.
    pub fn of(path: &Path) -> Option<Format> {
        let head = std::fs::read(path)
            .ok()
            .map(|b| String::from_utf8_lossy(&b[..b.len().min(512)]).into_owned())
            .unwrap_or_default();
        let head = head.trim_start();
        if head.starts_with("ISO-10303-21") {
            return Some(Format::Step);
        }
        // A Parasolid transmit file opens with its own banner.
        if head.starts_with("**ABCDEFGHIJKLMNOPQRSTUVWXYZ") || head.contains("PS_SCHEMA") {
            return Some(Format::ParasolidText);
        }
        match path
            .extension()
            .map(|e| e.to_string_lossy().to_lowercase())
            .as_deref()
        {
            Some("x_t" | "xmt_txt") => Some(Format::ParasolidText),
            Some("stp" | "step" | "p21") => Some(Format::Step),
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
}

impl Default for Options {
    fn default() -> Self {
        Options {
            quality: cad_tess::Options::default(),
            target: Target::default(),
            materials: cad_ir::MaterialResolver::default(),
            use_parasolid_twin: true,
        }
    }
}

/// What one conversion produced.
#[derive(Debug, Clone, Default)]
pub struct Summary {
    pub output: PathBuf,
    pub bytes: u64,
    pub bodies: usize,
    pub faces: usize,
    pub faces_meshed: usize,
    pub triangles: usize,
    /// Anything the readers or the tessellator could not do, in words. A
    /// conversion that produced a file and a warning is not a failure, and
    /// silently dropping the warning is how a caller ships a hole.
    pub warnings: Vec<String>,
}

#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("{0} is not a file this converts: expected Parasolid text or ISO 10303-21")]
    UnknownFormat(PathBuf),
    #[error("reading {path}: {detail}")]
    Read { path: PathBuf, detail: String },
    #[error("writing {path}: {detail}")]
    Write { path: PathBuf, detail: String },
}

pub type Result<T> = std::result::Result<T, Error>;

/// Read `input`, mesh it, and write it to `output`.
pub fn convert(input: &Path, output: &Path, options: &Options) -> Result<Summary> {
    let mut scene = read(input, options)?;
    let mut summary = Summary {
        output: output.to_path_buf(),
        bodies: scene.geometry.len(),
        ..Default::default()
    };

    let report = cad_tess::tessellate_scene(&mut scene, &options.quality);
    summary.faces = report.faces_ok + report.failed.len();
    summary.faces_meshed = report.faces_ok;
    summary.triangles = report.triangles;
    if !report.failed.is_empty() {
        summary.warnings.push(format!(
            "{} of {} faces could not be meshed",
            report.failed.len(),
            summary.faces
        ));
        // Naming a few of them is the difference between a caller who can look
        // and one who can only shrug. The rest are counted above.
        for f in report.failed.iter().take(5) {
            summary
                .warnings
                .push(format!("  {} face {}: {}", f.geometry, f.face.0, f.reason));
        }
    }

    let write = cad_export::Options {
        compression: match options.target {
            Target::Glb => cad_export::Compression::None,
            Target::GlbLean => cad_export::Compression::Normals,
            Target::GlbCompact => cad_export::Compression::Quantized,
        },
        ..cad_export::Options::default()
    };
    summary.bytes = cad_export::glb::write_file(&scene, &write, output).map_err(|e| Error::Write {
        path: output.to_path_buf(),
        detail: e.to_string(),
    })?;
    Ok(summary)
}

/// Read a file into a scene, without meshing it.
pub fn read(input: &Path, options: &Options) -> Result<cad_ir::Scene> {
    match Format::of(input).ok_or_else(|| Error::UnknownFormat(input.to_path_buf()))? {
        Format::ParasolidText => {
            let opts = cad_xt::LowerOptions {
                materials: options.materials.clone(),
            };
            let (scene, _report) = cad_xt::scene_from_file(input, &opts).map_err(|e| Error::Read {
                path: input.to_path_buf(),
                detail: e.to_string(),
            })?;
            Ok(scene)
        }
        Format::Step => {
            let file = cad_step::StepFile::open(input).map_err(|e| Error::Read {
                path: input.to_path_buf(),
                detail: e.to_string(),
            })?;
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
                    // These files are not always valid UTF-8, so the lossy
                    // conversion allocates a second full copy of a file that
                    // is tens of megabytes. Owning it and dropping the bytes
                    // before the parse keeps one copy rather than two.
                    let text = String::from_utf8_lossy(&bytes).into_owned();
                    drop(bytes);
                    if let Ok(hints) = xt_parser::appearance::appearance_hints(&text) {
                        opts.materials.reflectivity_by_colour = hints.reflectivity_by_colour();
                    }
                }
            }
            let (scene, _report) =
                cad_step::lower::asm::to_scene_with(&file, &opts).map_err(|e| Error::Read {
                    path: input.to_path_buf(),
                    detail: e.to_string(),
                })?;
            Ok(scene)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_header_outranks_the_extension() {
        let dir = std::env::temp_dir().join("cad-convert-format-test");
        let _ = std::fs::create_dir_all(&dir);
        let step_named_xt = dir.join("mislabelled.x_t");
        std::fs::write(&step_named_xt, "ISO-10303-21;\nHEADER;\n").unwrap();
        assert_eq!(Format::of(&step_named_xt), Some(Format::Step));
        let _ = std::fs::remove_file(&step_named_xt);
    }

    #[test]
    fn an_extension_decides_when_the_header_says_nothing() {
        let dir = std::env::temp_dir().join("cad-convert-format-test");
        let _ = std::fs::create_dir_all(&dir);
        let f = dir.join("plain.stp");
        std::fs::write(&f, "nothing recognisable here").unwrap();
        assert_eq!(Format::of(&f), Some(Format::Step));
        let g = dir.join("plain.dxf");
        std::fs::write(&g, "nothing recognisable here").unwrap();
        assert_eq!(Format::of(&g), None);
        let _ = std::fs::remove_file(&f);
        let _ = std::fs::remove_file(&g);
    }
}
