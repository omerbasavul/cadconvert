//! Parasolid XT text format parser.
//!
//! Parses `.x_t` files into a clean B-Rep intermediate representation.
//! Supports the compact transmit format (PS 30+, Onshape, T51) and
//! the modern annotated format (`#N = type ... ;`).
//!
//! # Example
//!
//! ```no_run
//! let file = xt_parser::parse_xt_file("model.x_t").unwrap();
//! for body in &file.bodies {
//!     println!("body type: {:?}, shells: {}", body.body_type, body.shells.len());
//!     for shell in &body.shells {
//!         for face in &shell.faces {
//!             println!("  face {} → surface {}", face.node_id, face.surface_key);
//!         }
//!     }
//! }
//! ```

#![forbid(unsafe_code)]

pub mod appearance;
pub mod build;
pub mod entity;
pub mod error;
pub mod header;
pub mod schema;
pub mod token;
pub mod types;

pub use error::{Result, XtError};
pub use types::*;

use std::path::Path;

/// Parse an XT file from a file path.
///
/// The header is not UTF-8. Its `DATE=` and `USER=` fields carry whatever the
/// writing machine's code page produced — a Solid Edge export from a Turkish
/// Windows holds CP1254 bytes for `Çar, Ağu` — so the file is read as bytes and
/// decoded lossily. The entity stream itself is pure ASCII, so replacing an
/// undecodable byte can only ever affect a header string, never geometry.
pub fn parse_xt_file<P: AsRef<Path>>(path: P) -> Result<XtFile> {
    let path = path.as_ref();
    let bytes = std::fs::read(path).map_err(|e| XtError::Io {
        path: path.to_path_buf(),
        source: e,
    })?;
    parse_xt(&String::from_utf8_lossy(&bytes))
}

/// Parse an XT file from a string.
pub fn parse_xt(text: &str) -> Result<XtFile> {
    // Phase 0: Split header and body.
    let (header_text, body_text) = header::split_header(text)?;
    let header = header::parse_header(header_text)?;

    // Check for binary format marker
    if body_text.starts_with("PS") || body_text.starts_with("\x50\x53") {
        return Err(XtError::UnsupportedEncoding(
            "binary X_B format not supported".into(),
        ));
    }

    // Reject non-zero user field sizes — the entity parser has no support
    // for skipping user field integers after each entity's regular fields.
    if header.user_field_size != 0 {
        return Err(XtError::UnsupportedEncoding(format!(
            "USFLD_SIZE={} not supported (entity parser cannot skip user fields)",
            header.user_field_size,
        )));
    }

    // Phase 1: Parse T-line and get the newline-stripped body.
    let tline = schema::parse_tline(body_text)?;

    // Phase 2: Parse schema preamble.
    //
    // Only files whose T-line key names a base schema to diff against carry a
    // preamble and inline schema annotations. When the key has two groups the
    // schema is fully determined by the key, and the entity stream starts
    // immediately after the T-line.
    let mut input = tline.body.as_str();
    let partition_count = if tline.has_base_schema {
        schema::parse_schema_preamble(&mut input)
            .map_err(|e| XtError::Parse {
                offset: 0,
                detail: format!("schema preamble: {}", e),
            })?
            .partition_count
    } else {
        0
    };

    // Phase 3: Parse entities.
    let (entities, truncated) = entity::parse_entities_opt(
        &mut input,
        partition_count,
        tline.has_base_schema,
        tline.key_major,
    )?;

    // Phase 4: Build typed IR.
    let bodies = build::build_bodies(&entities)?;

    Ok(XtFile {
        header,
        bodies,
        truncated,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_header_metadata() {
        let text = "\
**ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz**************************
**PARASOLID !\"#$%&'()*+,-./:;<=>?@[\\]^_`{|}~0123456789**************************
**PART1;
MC=unknown;
FRU=Parasolid 30.1.168;
APPL=TestApp;
FORMAT=text;
GUISE=transmit;
**PART2;
SCH=SCH_3001168_30100;
USFLD_SIZE=0;
**PART3;
**END_OF_HEADER*****************************************************************
T51 : TRANSMIT FILE created by modeller version 300116823 SCH_3001168_30100_13006
0 0 1 0
";
        let file = parse_xt(text).unwrap();
        assert_eq!(file.header.version, "30.1.168");
        assert_eq!(file.header.application, "TestApp");
        assert_eq!(file.header.schema_key, "SCH_3001168_30100");
    }
}

#[cfg(test)]
mod sample_tests {
    //! Regression over the real SolidWorks and Solid Edge exports.
    //!
    //! The sample directory is not part of the repository, so these skip rather
    //! than fail when it is absent — but when it is present they are the only
    //! check that covers both stream dialects and four modeller generations.
    //! Point `XT_SAMPLES` at a directory of `.x_t` files to run them.

    const DEFAULT_SAMPLES: &str = "/Users/omerbasavul/Downloads/3D Model bütün dosya formatları";

    fn sample_dir() -> Option<std::path::PathBuf> {
        let dir = std::env::var("XT_SAMPLES").unwrap_or_else(|_| DEFAULT_SAMPLES.to_string());
        let path = std::path::PathBuf::from(dir);
        path.is_dir().then_some(path)
    }

    fn samples() -> Vec<std::path::PathBuf> {
        let Some(dir) = sample_dir() else {
            return Vec::new();
        };
        let mut out: Vec<_> = std::fs::read_dir(dir)
            .into_iter()
            .flatten()
            .flatten()
            .map(|e| e.path())
            .filter(|p| p.extension().is_some_and(|e| e.eq_ignore_ascii_case("x_t")))
            .collect();
        out.sort();
        out
    }

    /// Every sample that parses must produce a body with at least one face, and
    /// the ones that do not parse must be named — a silent zero-body success is
    /// the failure mode this guards against.
    #[test]
    fn known_samples_parse_into_bodies() {
        let files = samples();
        if files.is_empty() {
            eprintln!("no sample directory; skipping");
            return;
        }

        // The 36 MB Solid Edge export parses to its end since the ATTRIB_DEF
        // annotation fix, but its PS 37 BODY layout is not yet built into the
        // typed IR, so it legitimately reports bodies without faces.
        const KNOWN_UNBUILT: &str = "910 2001 007.x_t";

        let mut problems = Vec::new();
        for path in &files {
            let name = path
                .file_name()
                .unwrap_or_default()
                .to_string_lossy()
                .into_owned();
            match crate::parse_xt_file(path) {
                Err(e) => problems.push(format!("{name}: failed to parse: {e}")),
                Ok(file) => {
                    let faces: usize = file
                        .bodies
                        .iter()
                        .flat_map(|b| b.shells.iter())
                        .map(|s| s.faces.len())
                        .sum();
                    match &file.truncated {
                        Some(t) => problems.push(format!("{name}: {t}")),
                        None if name == KNOWN_UNBUILT => {
                            assert!(
                                !file.bodies.is_empty(),
                                "{name} parsed but produced no bodies at all"
                            );
                        }
                        None if file.bodies.is_empty() || faces == 0 => problems.push(format!(
                            "{name}: parsed cleanly but produced {} bodies and {faces} faces",
                            file.bodies.len()
                        )),
                        None => {}
                    }
                }
            }
        }
        assert!(problems.is_empty(), "{problems:#?}");
    }

    /// The Solid Edge export must parse to its end — it stopped at its first
    /// ATTRIB_DEF for as long as the annotation cursor advanced one expanded
    /// slot per `C` instead of one logical field.
    #[test]
    fn the_solid_edge_export_parses_to_the_end() {
        let Some(dir) = sample_dir() else {
            eprintln!("no sample directory; skipping");
            return;
        };
        let big = dir.join("910 2001 007.x_t");
        if !big.exists() {
            return;
        }
        let file = crate::parse_xt_file(&big).expect("the header is not UTF-8 but must still read");
        assert!(
            file.truncated.is_none(),
            "regressed to truncating: {:?}",
            file.truncated
        );
        assert!(!file.bodies.is_empty());
    }
}
