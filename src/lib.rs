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
pub fn parse_xt_file<P: AsRef<Path>>(path: P) -> Result<XtFile> {
    let path = path.as_ref();
    let text = std::fs::read_to_string(path).map_err(|e| XtError::Io {
        path: path.to_path_buf(),
        source: e,
    })?;
    parse_xt(&text)
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
    let entities = entity::parse_entities_opt(
        &mut input,
        partition_count,
        tline.has_base_schema,
        tline.key_major,
    )?;

    // Phase 4: Build typed IR.
    let bodies = build::build_bodies(&entities)?;

    Ok(XtFile { header, bodies })
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
mod entity_tests {
    #[test]
    fn attribute_parse_trace() {
        // Simulate exact stream after BODY: "81 255 1 2 299 10 1 11 0 0 0 12 70 ..."
        let mut input = "81 255 1 2 299 10 1 11 0 0 0 12 70 11 1 0";
        let entities = crate::entity::parse_entities(&mut input, 0).unwrap();
        eprintln!("Parsed {} entities from ATTRIBUTE test", entities.len());
        for e in &entities {
            eprintln!("  type={} idx={} fields={}", e.type_id, e.index, e.fields.len());
        }
        eprintln!("Remaining: {:?}", input);
    }
}

#[cfg(test)]
mod batch_test {
    #[test]
    fn entity_counts() {
        use std::collections::HashMap;
        let text = std::fs::read_to_string(
            "/home/kiselev/cadatomic/xt-parser/test-data/abc/xt_files/Part Studio 1 - Part 1.x_t"
        ).unwrap();
        let file = crate::parse_xt(&text).unwrap();
        // Count entities by type
        eprintln!("Bodies: {}", file.bodies.len());
        for (i, b) in file.bodies.iter().enumerate() {
            eprintln!("  body[{}]: shells={} surfaces={} curves={} edges={} vertices={} points={}",
                i, b.shells.len(), b.surfaces.len(), b.curves.len(),
                b.edges.len(), b.vertices.len(), b.points.len());
            for (j, s) in b.shells.iter().enumerate() {
                eprintln!("    shell[{}]: faces={}", j, s.faces.len());
            }
        }
    }
}
