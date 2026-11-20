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

/// A parsed file at the raw-entity level, before any typed IR is built.
///
/// This is the surface a lowering layer wants: every entity with its fields,
/// plus whether the stream stopped early. `parse_xt` builds the legacy typed
/// bodies on top of the same entities.
pub struct RawFile {
    pub header: XtHeader,
    /// Every entity, and the one array their fields live in. Derefs to the
    /// entities; ask it for a field.
    pub entities: entity::Entities,
    pub truncated: Option<entity::Truncation>,
}

/// A file's bytes as text, in one allocation where the bytes allow it.
///
/// `String::from_utf8_lossy(&bytes).into_owned()` is the obvious spelling and
/// it costs twice: the original bytes stay live, and the string it builds
/// grows by doubling. Measured on the pilot — a 35 MB file — that step alone
/// moved the process 72 MB, because a 35 MB string that doubles its way there
/// has touched 64 MB of pages by the time it arrives.
///
/// Here the bytes are handed over instead. A transmit file is ASCII but for
/// the letters of its header and of the names in it, and most files are clean
/// throughout — those move into the string without a copy at all.
/// A file that is not gets a conversion sized in advance from the length in
/// hand, which is exact for every replacement-free stretch and never doubles.
///
/// **What a character of the result stands for.** A string in a transmit file
/// is written with its length, and the length is in the file's bytes. The
/// parser reads characters, so it has to know how many bytes each one was:
///
/// * a character that was valid UTF-8 in the file stands for as many bytes as
///   its own encoding has, and is itself;
/// * a byte that was not — a letter in the writer's code page — goes through
///   as one of 256 private-use characters that remember it, [`carried`] says
///   which, and it stands for that one byte. `U+FFFD` here lost both the
///   letter and, where two such bytes stood together, the count.
///
/// A private-use character that was really in the file would be taken for a
/// carried byte, so the few that could be are carried as the bytes of their
/// own encoding instead. The count stays right and the name comes out as
/// three letters where it had one, which is the least a name with a
/// private-use character in it can expect.
pub fn decode(bytes: Vec<u8>) -> String {
    match String::from_utf8(bytes) {
        // Every character in the carrying range begins with this byte, and a
        // file without it — which is every file but the odd one — is the
        // string it already was.
        Ok(text) if !text.as_bytes().contains(&CARRIER_LEAD) => text,
        Ok(text) => {
            let mut out = String::with_capacity(text.len());
            push_as_itself(&mut out, &text);
            out
        }
        Err(e) => {
            let bytes = e.into_bytes();
            let mut text = String::with_capacity(bytes.len());
            for chunk in bytes.utf8_chunks() {
                push_as_itself(&mut text, chunk.valid());
                for &byte in chunk.invalid() {
                    text.push(carrier(byte));
                }
            }
            text
        }
    }
}

/// The private-use characters that stand for one byte each: `U+E000 + byte`.
const CARRIERS: u32 = 0xE000;
/// The first byte of the UTF-8 of every one of them.
const CARRIER_LEAD: u8 = 0xEE;

fn carrier(byte: u8) -> char {
    char::from_u32(CARRIERS + byte as u32).expect("the private-use area has no gaps")
}

/// The byte a character of [`decode`]'s output stands for, where it stands
/// for one and is not itself.
pub fn carried(c: char) -> Option<u8> {
    let at = (c as u32).wrapping_sub(CARRIERS);
    (at < 0x100).then_some(at as u8)
}

/// Valid text, with any character of its own that lies in the carrying range
/// spelled as the bytes it was.
fn push_as_itself(out: &mut String, valid: &str) {
    if !valid.as_bytes().contains(&CARRIER_LEAD) {
        out.push_str(valid);
        return;
    }
    for c in valid.chars() {
        if carried(c).is_some() {
            let mut utf8 = [0u8; 4];
            for &byte in c.encode_utf8(&mut utf8).as_bytes() {
                out.push(carrier(byte));
            }
        } else {
            out.push(c);
        }
    }
}

/// The letter a byte is in Windows-1252, the code page a Western SolidWorks
/// writes its names in. It is Latin-1 but for 0x80 to 0x9F, where Latin-1 has
/// control codes and Windows has the euro sign, the dashes and the quotation
/// marks that do turn up in part names.
pub fn letter(byte: u8) -> char {
    const HIGH: [char; 32] = [
        '\u{20AC}', '\u{81}', '\u{201A}', '\u{192}', '\u{201E}', '\u{2026}', '\u{2020}', '\u{2021}',
        '\u{2C6}', '\u{2030}', '\u{160}', '\u{2039}', '\u{152}', '\u{8D}', '\u{17D}', '\u{8F}',
        '\u{90}', '\u{2018}', '\u{2019}', '\u{201C}', '\u{201D}', '\u{2022}', '\u{2013}', '\u{2014}',
        '\u{2DC}', '\u{2122}', '\u{161}', '\u{203A}', '\u{153}', '\u{9D}', '\u{17E}', '\u{178}',
    ];
    match byte {
        0x80..=0x9F => HIGH[(byte - 0x80) as usize],
        _ => byte as char,
    }
}

/// A run of [`decode`]'s characters as the text it was.
///
/// The bytes are put back together first, because a letter of several bytes
/// can have been split: a transmit file is wrapped at a fixed width counted
/// in bytes, and a line that ends between the two bytes of an `ä` leaves
/// neither of them valid where it stands. Whole again they are read as UTF-8
/// if that is what they are, and otherwise each carried byte is the letter
/// its code page says.
pub fn plain(run: &[char]) -> String {
    if !run.iter().any(|c| carried(*c).is_some()) {
        return run.iter().collect();
    }
    let mut bytes = Vec::with_capacity(run.len() * 2);
    for &c in run {
        match carried(c) {
            Some(byte) => bytes.push(byte),
            None => {
                let mut utf8 = [0u8; 4];
                bytes.extend_from_slice(c.encode_utf8(&mut utf8).as_bytes());
            }
        }
    }
    match String::from_utf8(bytes) {
        Ok(text) => text,
        Err(_) => run.iter().map(|&c| carried(c).map_or(c, letter)).collect(),
    }
}

/// Parse an XT file to raw entities.
/// Parse, and give up the text as soon as nothing needs it.
///
/// `parse_tline` strips the newlines into a string of its own, so from that
/// point the caller's copy is a second 35 MB of the same file held for the
/// length of the parse — and the parse is where the peak is. Owning it here
/// lets it go.
pub fn parse_raw_owned(text: String) -> Result<RawFile> {
    let (header, tline) = {
        let (header_text, body_text) = header::split_header(&text)?;
        let header = header::parse_header(&plain(&header_text.chars().collect::<Vec<_>>()))?;
        reject_unsupported(&header, body_text)?;
        (header, schema::parse_tline(body_text)?)
    };
    drop(text);
    parse_from_tline(header, tline)
}

pub fn parse_raw(text: &str) -> Result<RawFile> {
    let (header_text, body_text) = header::split_header(text)?;
    let header = header::parse_header(&plain(&header_text.chars().collect::<Vec<_>>()))?;

    if body_text.starts_with("PS") || body_text.starts_with("\x50\x53") {
        return Err(XtError::UnsupportedEncoding(
            "binary X_B format not supported".into(),
        ));
    }
    if header.user_field_size != 0 {
        return Err(XtError::UnsupportedEncoding(format!(
            "USFLD_SIZE={} not supported (entity parser cannot skip user fields)",
            header.user_field_size,
        )));
    }

    let tline = schema::parse_tline(body_text)?;
    parse_from_tline(header, tline)
}

fn reject_unsupported(header: &crate::types::XtHeader, body_text: &str) -> Result<()> {
    if body_text.starts_with("PS") || body_text.starts_with("\x50\x53") {
        return Err(XtError::UnsupportedEncoding(
            "binary X_B format not supported".into(),
        ));
    }
    if header.user_field_size != 0 {
        return Err(XtError::UnsupportedEncoding(format!(
            "USFLD_SIZE={} not supported (entity parser cannot skip user fields)",
            header.user_field_size,
        )));
    }
    Ok(())
}

fn parse_from_tline(header: crate::types::XtHeader, tline: schema::TLine) -> Result<RawFile> {
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
    let (entities, truncated) = entity::parse_entities_opt(
        &mut input,
        partition_count,
        tline.has_base_schema,
        tline.key_major,
    )?;
    Ok(RawFile {
        header,
        entities,
        truncated,
    })
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

    /// How many bytes of the file a decoded string stands for, which is what
    /// the length written before a string counts.
    fn bytes_stood_for(text: &str) -> usize {
        text.chars().map(|c| if carried(c).is_some() { 1 } else { c.len_utf8() }).sum()
    }

    #[test]
    fn a_decoded_file_stands_for_as_many_bytes_as_it_had() {
        // Clean text is the string it already was.
        let clean = "Gehäuse 12 – links".to_string();
        assert_eq!(decode(clean.clone().into_bytes()), clean);
        assert_eq!(bytes_stood_for(&clean), clean.len());

        // A code page's letters are not UTF-8 where they stand, and each is
        // one byte: U+FFFD made one character of two such bytes together.
        let written = b"Geh\xE4use f\xFC\xDFe".to_vec();
        let text = decode(written.clone());
        assert_eq!(bytes_stood_for(&text), written.len());
        assert_eq!(plain(&text.chars().collect::<Vec<_>>()), "Gehäuse füße");

        // The same file can hold both, and each keeps its own count.
        let mut mixed = "Gehäuse ".as_bytes().to_vec();
        mixed.extend_from_slice(b"f\xFC\xDFe");
        let text = decode(mixed.clone());
        assert_eq!(bytes_stood_for(&text), mixed.len());
    }

    #[test]
    fn a_letter_split_by_the_end_of_a_line_is_put_back_together() {
        // The two bytes of an `ä` with a line break between them: neither is
        // valid where it stands, and with the break taken out they are the
        // letter again.
        let written = b"Geh\xC3\n\xA4use".to_vec();
        let text: Vec<char> = decode(written).chars().filter(|c| *c != '\n').collect();
        assert_eq!(plain(&text), "Gehäuse");
    }

    #[test]
    fn a_private_use_character_of_the_files_own_is_not_taken_for_a_byte() {
        // U+E041 is three bytes in the file. Read as the byte it would stand
        // for, the count would be two short and the read out of step.
        let written = "a\u{E041}b".to_string().into_bytes();
        let text = decode(written.clone());
        assert_eq!(bytes_stood_for(&text), written.len());
        assert_eq!(plain(&text.chars().collect::<Vec<_>>()), "a\u{E041}b");
    }

    #[test]
    fn a_byte_is_the_letter_windows_says_it_is() {
        assert_eq!(letter(0xE4), 'ä');
        assert_eq!(letter(0x96), '–');
        assert_eq!(letter(0x80), '€');
        assert_eq!(letter(b'A'), 'A');
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

    fn sample_dir() -> Option<std::path::PathBuf> {
        let path = std::path::PathBuf::from(std::env::var("XT_SAMPLES").ok()?);
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
