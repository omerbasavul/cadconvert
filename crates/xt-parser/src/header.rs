//! XT header parser.
//!
//! The header occupies lines 1 through `**END_OF_HEADER`:
//! ```text
//! **ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz**...
//! **PARASOLID !"#$%&'()*+,-./:;<=>?@[\]^_`{|}~0123456789**...
//! **PART1;
//! MC=unknown;
//! FRU=Parasolid 30.1.168;
//! APPL=Onshape;
//! FORMAT=text;
//! ...
//! **PART2;
//! SCH=SCH_3001168_30100;
//! ...
//! **PART3;
//! **END_OF_HEADER*****...
//! ```

use crate::error::{Result, XtError};
use crate::types::XtHeader;

const END_MARKER: &str = "**END_OF_HEADER";

/// Split the file text into (header_text, body_text).
/// Returns an error if the header terminator is missing.
pub fn split_header(text: &str) -> Result<(&str, &str)> {
    let pos = text
        .find(END_MARKER)
        .ok_or_else(|| XtError::InvalidHeader("missing **END_OF_HEADER".into()))?;
    // Skip past the END_OF_HEADER line (find the next newline after the marker)
    let rest_after_marker = &text[pos..];
    let line_end = rest_after_marker
        .find('\n')
        .map(|i| pos + i + 1)
        .unwrap_or(text.len());
    Ok((&text[..pos], &text[line_end..]))
}

/// Parse header metadata from the header text.
pub fn parse_header(header_text: &str) -> Result<XtHeader> {
    let mut h = XtHeader::default();

    let mut in_part = 0u8; // 0 = before PART1, 1 = PART1, 2 = PART2
    for line in header_text.lines() {
        let line = line.trim();
        if line.starts_with("**PART1") {
            in_part = 1;
            continue;
        }
        if line.starts_with("**PART2") {
            in_part = 2;
            continue;
        }
        if line.starts_with("**PART3") {
            in_part = 0;
            continue;
        }
        if line.starts_with("**") {
            // character set validation lines or other ** lines
            continue;
        }

        // Parse key=value; lines
        let line = line.trim_end_matches(';');
        if let Some((key, val)) = line.split_once('=') {
            let key = key.trim();
            let val = val.trim();
            match (in_part, key) {
                (1, "FRU") => {
                    // "Parasolid 30.1.168" → "30.1.168"
                    h.version = val
                        .strip_prefix("Parasolid ")
                        .unwrap_or(val)
                        .to_string();
                }
                (1, "APPL") => {
                    h.application = val.to_string();
                }
                (1, "DATE") => {
                    h.date = Some(val.to_string());
                }
                (2, "SCH") => {
                    h.schema_key = val.to_string();
                }
                (2, "USFLD_SIZE") => {
                    h.user_field_size = val.parse().map_err(|_| {
                        crate::error::XtError::InvalidHeader(
                            format!("invalid USFLD_SIZE value: {:?}", val),
                        )
                    })?;
                }
                _ => {}
            }
        }
    }
    Ok(h)
}

#[cfg(test)]
mod tests {
    use super::*;

    const SAMPLE_HEADER: &str = "\
**ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz**************************
**PARASOLID !\"#$%&'()*+,-./:;<=>?@[\\]^_`{|}~0123456789**************************
**PART1;
MC=unknown;
FRU=Parasolid 30.1.168;
APPL=Onshape;
FORMAT=text;
GUISE=transmit;
DATE=2018-04-27T08:24:19 (UTC);
**PART2;
SCH=SCH_3001168_30100;
USFLD_SIZE=0;
**PART3;
**END_OF_HEADER*****************************************************************
body data here";

    #[test]
    fn split_works() {
        let (hdr, body) = split_header(SAMPLE_HEADER).unwrap();
        assert!(hdr.contains("**PART1"));
        assert!(body.starts_with("body data here"));
    }

    #[test]
    fn parse_metadata() {
        let (hdr, _) = split_header(SAMPLE_HEADER).unwrap();
        let h = parse_header(hdr).unwrap();
        assert_eq!(h.version, "30.1.168");
        assert_eq!(h.application, "Onshape");
        assert_eq!(h.schema_key, "SCH_3001168_30100");
        assert_eq!(h.date.as_deref(), Some("2018-04-27T08:24:19 (UTC)"));
        assert_eq!(h.user_field_size, 0);
    }
}
