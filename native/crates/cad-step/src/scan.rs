//! Byte-level structural scan of a Part 21 exchange file.
//!
//! Part 21 is line-noise-tolerant: newlines are ordinary whitespace, comments
//! `/* … */` may appear anywhere between tokens, and the record terminator `;`
//! may legally appear *inside* a string literal. So record boundaries cannot be
//! found by splitting on `;` — a single linear state machine over the bytes is
//! both the correct way and, at one pass with no allocation, the fast way.
//!
//! The scan produces spans only. Nothing is decoded here; argument bytes are
//! parsed on demand by [`crate::value`], and the hot geometry entities are read
//! by the specialised extractors that skip the generic value tree entirely.

use crate::error::{Result, StepError};
use std::ops::Range;

/// Where a record's pieces live inside the file buffer.
#[derive(Debug, Clone)]
pub struct RecordSpan {
    /// Entity instance name, the `N` in `#N=`.
    pub id: u32,
    /// The keyword bytes, e.g. `ADVANCED_FACE`. Empty for a complex record.
    pub keyword: Range<u32>,
    /// The bytes strictly between the outermost `(` and its matching `)`.
    ///
    /// For a complex record — `#5=(A(…)B(…));` — this is the whole
    /// `A(…)B(…)` run, and [`RecordSpan::complex`] is set.
    pub args: Range<u32>,
    /// True when the record is a complex instance: `#N=(KW1(…)KW2(…));`.
    pub complex: bool,
}

/// The three sections a Part 21 file is built from.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Section {
    /// Before `HEADER;`.
    Preamble,
    Header,
    Data,
    /// After the data section's `ENDSEC;`.
    Tail,
}

/// Result of scanning a whole file.
pub struct Scan {
    /// Header records, in file order. These are `KEYWORD(args);` with no `#N=`.
    pub header: Vec<HeaderSpan>,
    /// Data records, in file order.
    pub records: Vec<RecordSpan>,
    /// Largest entity id seen, so callers can size an id-indexed table.
    pub max_id: u32,
}

/// A header record: `FILE_NAME('x', …);` — keyword plus argument bytes.
#[derive(Debug, Clone)]
pub struct HeaderSpan {
    pub keyword: Range<u32>,
    pub args: Range<u32>,
}

/// Scan `buf` into record spans.
///
/// Returns an error only for structural damage — an unterminated string or
/// comment, a record with no `=`, unbalanced parentheses. Unknown keywords are
/// *not* an error at this layer; the reader decides what it cares about.
pub fn scan(buf: &[u8]) -> Result<Scan> {
    if !starts_with_iso_marker(buf) {
        return Err(StepError::NotPart21);
    }

    let mut header = Vec::new();
    let mut records = Vec::with_capacity(buf.len() / 48);
    let mut max_id = 0u32;
    let mut section = Section::Preamble;

    let mut i = 0usize;
    let n = buf.len();

    while i < n {
        i = skip_trivia(buf, i)?;
        if i >= n {
            break;
        }

        let start = i;
        let end = find_record_end(buf, i)?;
        // `end` indexes the `;`. The record body is buf[start..end].
        let body = start..end;
        i = end + 1;

        if is_keyword(buf, &body, b"ENDSEC") {
            section = match section {
                Section::Data => Section::Tail,
                _ => Section::Preamble,
            };
            continue;
        }
        if is_keyword(buf, &body, b"HEADER") {
            section = Section::Header;
            continue;
        }
        if is_keyword(buf, &body, b"DATA") || starts_with_keyword(buf, &body, b"DATA") {
            // `DATA;` or `DATA('name');` — both open the data section.
            section = Section::Data;
            continue;
        }
        if is_keyword(buf, &body, b"END-ISO-10303-21") {
            break;
        }

        match section {
            Section::Header => header.push(scan_header_record(buf, &body)?),
            Section::Data => {
                let rec = scan_data_record(buf, &body)?;
                max_id = max_id.max(rec.id);
                records.push(rec);
            }
            // The `ISO-10303-21;` marker and anything after the data section.
            Section::Preamble | Section::Tail => {}
        }
    }

    Ok(Scan {
        header,
        records,
        max_id,
    })
}

fn starts_with_iso_marker(buf: &[u8]) -> bool {
    // Tolerate a UTF-8 BOM and leading whitespace, which some writers emit.
    let mut i = 0;
    if buf.starts_with(&[0xEF, 0xBB, 0xBF]) {
        i = 3;
    }
    while i < buf.len() && buf[i].is_ascii_whitespace() {
        i += 1;
    }
    buf[i..].starts_with(b"ISO-10303-21")
}

/// Advance past whitespace and `/* … */` comments.
fn skip_trivia(buf: &[u8], mut i: usize) -> Result<usize> {
    let n = buf.len();
    loop {
        while i < n && buf[i].is_ascii_whitespace() {
            i += 1;
        }
        if i + 1 < n && buf[i] == b'/' && buf[i + 1] == b'*' {
            let start = i;
            i += 2;
            loop {
                match memchr::memchr(b'*', &buf[i..]) {
                    Some(off) => {
                        i += off + 1;
                        if i < n && buf[i] == b'/' {
                            i += 1;
                            break;
                        }
                    }
                    None => {
                        return Err(StepError::Unterminated {
                            what: "comment",
                            offset: start,
                        });
                    }
                }
            }
            continue;
        }
        return Ok(i);
    }
}

/// Find the `;` that terminates the record beginning at `start`.
///
/// Skips over string literals (where `;` is data, not structure) and comments.
fn find_record_end(buf: &[u8], start: usize) -> Result<usize> {
    let n = buf.len();
    let mut i = start;
    while i < n {
        match buf[i] {
            b';' => return Ok(i),
            b'\'' => i = skip_string(buf, i)?,
            b'/' if i + 1 < n && buf[i + 1] == b'*' => i = skip_trivia(buf, i)?,
            _ => i += 1,
        }
    }
    Err(StepError::Unterminated {
        what: "record",
        offset: start,
    })
}

/// `i` points at the opening quote; returns the index just past the closing one.
///
/// A doubled `''` is an escaped quote and does not close the literal.
fn skip_string(buf: &[u8], i: usize) -> Result<usize> {
    let n = buf.len();
    let start = i;
    let mut i = i + 1;
    loop {
        match memchr::memchr(b'\'', &buf[i..n]) {
            Some(off) => {
                i += off + 1;
                if i < n && buf[i] == b'\'' {
                    i += 1; // escaped quote, keep going
                    continue;
                }
                return Ok(i);
            }
            None => {
                return Err(StepError::Unterminated {
                    what: "string literal",
                    offset: start,
                });
            }
        }
    }
}

/// True when `body` is exactly `kw`, ignoring surrounding whitespace.
fn is_keyword(buf: &[u8], body: &Range<usize>, kw: &[u8]) -> bool {
    let s = trim(buf, body.clone());
    s.len() == kw.len() && buf[s.clone()].eq_ignore_ascii_case(kw)
}

/// True when `body` begins with `kw` followed by `(`.
fn starts_with_keyword(buf: &[u8], body: &Range<usize>, kw: &[u8]) -> bool {
    let s = trim(buf, body.clone());
    let bytes = &buf[s.clone()];
    bytes.len() > kw.len()
        && bytes[..kw.len()].eq_ignore_ascii_case(kw)
        && bytes[kw.len()..].iter().copied().find(|b| !b.is_ascii_whitespace()) == Some(b'(')
}

fn trim(buf: &[u8], mut r: Range<usize>) -> Range<usize> {
    while r.start < r.end && buf[r.start].is_ascii_whitespace() {
        r.start += 1;
    }
    while r.end > r.start && buf[r.end - 1].is_ascii_whitespace() {
        r.end -= 1;
    }
    r
}

/// `KEYWORD(args)` with no instance name.
fn scan_header_record(buf: &[u8], body: &Range<usize>) -> Result<HeaderSpan> {
    let b = trim(buf, body.clone());
    let open = memchr::memchr(b'(', &buf[b.clone()])
        .map(|o| b.start + o)
        .ok_or_else(|| StepError::record(body, "header record has no `(`"))?;
    let close = matching_paren(buf, open)?;
    let kw = trim(buf, b.start..open);
    Ok(HeaderSpan {
        keyword: kw.start as u32..kw.end as u32,
        args: (open + 1) as u32..close as u32,
    })
}

/// `#N=KEYWORD(args)` or `#N=(KW1(…)KW2(…))`.
fn scan_data_record(buf: &[u8], body: &Range<usize>) -> Result<RecordSpan> {
    let b = trim(buf, body.clone());
    if buf[b.start] != b'#' {
        return Err(StepError::record(body, "data record does not start with `#`"));
    }

    let mut p = b.start + 1;
    let mut id: u32 = 0;
    let digits_start = p;
    while p < b.end && buf[p].is_ascii_digit() {
        id = id
            .checked_mul(10)
            .and_then(|v| v.checked_add(u32::from(buf[p] - b'0')))
            .ok_or_else(|| StepError::record(body, "entity id overflows u32"))?;
        p += 1;
    }
    if p == digits_start {
        return Err(StepError::record(body, "entity id has no digits"));
    }

    while p < b.end && buf[p].is_ascii_whitespace() {
        p += 1;
    }
    if p >= b.end || buf[p] != b'=' {
        return Err(StepError::record(body, "expected `=` after entity id"));
    }
    p += 1;
    while p < b.end && buf[p].is_ascii_whitespace() {
        p += 1;
    }
    if p >= b.end {
        return Err(StepError::record(body, "record has no value after `=`"));
    }

    if buf[p] == b'(' {
        // Complex instance. The outer parens wrap a run of `KW(…)` groups.
        let close = matching_paren(buf, p)?;
        return Ok(RecordSpan {
            id,
            keyword: 0..0,
            args: (p + 1) as u32..close as u32,
            complex: true,
        });
    }

    let open = memchr::memchr(b'(', &buf[p..b.end])
        .map(|o| p + o)
        .ok_or_else(|| StepError::record(body, "simple record has no `(`"))?;
    let close = matching_paren(buf, open)?;
    let kw = trim(buf, p..open);
    if kw.is_empty() {
        return Err(StepError::record(body, "record has an empty keyword"));
    }
    Ok(RecordSpan {
        id,
        keyword: kw.start as u32..kw.end as u32,
        args: (open + 1) as u32..close as u32,
        complex: false,
    })
}

/// Given the index of a `(`, return the index of the `)` that closes it.
///
/// Parens inside string literals and comments do not count.
pub(crate) fn matching_paren(buf: &[u8], open: usize) -> Result<usize> {
    debug_assert_eq!(buf[open], b'(');
    let n = buf.len();
    let mut depth = 0usize;
    let mut i = open;
    while i < n {
        match buf[i] {
            b'(' => {
                depth += 1;
                i += 1;
            }
            b')' => {
                depth -= 1;
                if depth == 0 {
                    return Ok(i);
                }
                i += 1;
            }
            b'\'' => i = skip_string(buf, i)?,
            b'/' if i + 1 < n && buf[i + 1] == b'*' => i = skip_trivia(buf, i)?,
            _ => i += 1,
        }
    }
    Err(StepError::Unterminated {
        what: "parenthesis",
        offset: open,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    const MINIMAL: &str = "ISO-10303-21;\nHEADER;\nFILE_NAME('a','b');\nENDSEC;\nDATA;\n\
        #1=CARTESIAN_POINT('',(0.,1.,2.));\n#2=PLANE('',#1);\nENDSEC;\nEND-ISO-10303-21;\n";

    #[test]
    fn scans_a_minimal_file() {
        let s = scan(MINIMAL.as_bytes()).unwrap();
        assert_eq!(s.header.len(), 1);
        assert_eq!(s.records.len(), 2);
        assert_eq!(s.max_id, 2);
        assert_eq!(s.records[0].id, 1);
        assert_eq!(
            &MINIMAL.as_bytes()[s.records[0].keyword.start as usize..s.records[0].keyword.end as usize],
            b"CARTESIAN_POINT"
        );
        assert_eq!(
            &MINIMAL.as_bytes()[s.records[1].args.start as usize..s.records[1].args.end as usize],
            b"'',#1"
        );
    }

    #[test]
    fn semicolon_inside_a_string_does_not_end_the_record() {
        let src = "ISO-10303-21;DATA;#7=PRODUCT('a;b','c;;d',());ENDSEC;END-ISO-10303-21;";
        let s = scan(src.as_bytes()).unwrap();
        assert_eq!(s.records.len(), 1);
        assert_eq!(
            &src.as_bytes()[s.records[0].args.start as usize..s.records[0].args.end as usize],
            b"'a;b','c;;d',()"
        );
    }

    #[test]
    fn escaped_quote_does_not_close_the_string() {
        let src = "ISO-10303-21;DATA;#1=X('it''s');ENDSEC;END-ISO-10303-21;";
        let s = scan(src.as_bytes()).unwrap();
        assert_eq!(s.records.len(), 1);
        assert_eq!(
            &src.as_bytes()[s.records[0].args.start as usize..s.records[0].args.end as usize],
            b"'it''s'"
        );
    }

    #[test]
    fn comments_are_skipped_everywhere() {
        let src = "ISO-10303-21;\n/* lead */\nDATA;\n#1=/*mid*/PLANE('',/*in*/#2);\nENDSEC;\nEND-ISO-10303-21;";
        let s = scan(src.as_bytes()).unwrap();
        assert_eq!(s.records.len(), 1);
        assert_eq!(s.records[0].id, 1);
    }

    #[test]
    fn complex_instances_are_flagged() {
        let src = "ISO-10303-21;DATA;#3=(NAMED_UNIT(*)SI_UNIT($,.METRE.)LENGTH_UNIT());ENDSEC;END-ISO-10303-21;";
        let s = scan(src.as_bytes()).unwrap();
        assert_eq!(s.records.len(), 1);
        assert!(s.records[0].complex);
        assert!(s.records[0].keyword.is_empty());
    }

    #[test]
    fn unterminated_string_is_an_error() {
        let src = "ISO-10303-21;DATA;#1=X('oops);ENDSEC;";
        assert!(matches!(
            scan(src.as_bytes()),
            Err(StepError::Unterminated { what: "string literal", .. })
        ));
    }

    #[test]
    fn rejects_non_part21() {
        assert!(matches!(scan(b"solid foo\n"), Err(StepError::NotPart21)));
    }
}
