//! Low-level winnow parsers for the XT token stream.
//!
//! The compact transmit format is a whitespace-delimited token stream where
//! newlines are invisible (stripped before parsing). Tokens include integers,
//! floats, entity pointers, 3-vectors, single characters, and booleans.

use winnow::prelude::*;
use winnow::ascii;
use winnow::combinator::{alt, preceded, opt};
use winnow::token::{take_while, one_of};

/// Skip whitespace (spaces, tabs — newlines already stripped).
pub fn ws<'a>(input: &mut &'a str) -> ModalResult<&'a str> {
    take_while(0.., |c: char| c == ' ' || c == '\t').parse_next(input)
}

/// Parse a signed decimal integer, skipping leading whitespace.
pub fn xt_int(input: &mut &str) -> ModalResult<i64> {
    ws(input)?;
    // winnow's dec_int handles optional leading sign
    ascii::dec_int::<_, i64, _>.parse_next(input)
}

/// Parse an unsigned decimal integer, skipping leading whitespace.
pub fn xt_uint(input: &mut &str) -> ModalResult<u64> {
    ws(input)?;
    ascii::dec_uint::<_, u64, _>.parse_next(input)
}

/// Parse a 16-bit signed integer, skipping leading whitespace.
pub fn xt_short(input: &mut &str) -> ModalResult<i16> {
    ws(input)?;
    ascii::dec_int::<_, i16, _>.parse_next(input)
}

/// Parse a floating-point number, skipping leading whitespace.
/// Handles: `3.14`, `1e-8`, `.004572`, `-0`, `1e3`.
pub fn xt_float(input: &mut &str) -> ModalResult<f64> {
    ws(input)?;
    xt_float_raw(input)
}

/// Parse float without leading whitespace skip.
fn xt_float_raw(input: &mut &str) -> ModalResult<f64> {
    // XT floats can be: 1.0, .004572, 1e-8, -3.14, 472e-31, -0
    // We'll parse the text manually then convert.
    let start = *input;
    // optional sign
    opt(one_of(['+', '-'])).parse_next(input)?;
    // digits before decimal (may be empty if starts with '.')
    let pre: &str = take_while(0.., |c: char| c.is_ascii_digit()).parse_next(input)?;
    // optional decimal + fraction
    let has_dot = opt('.').parse_next(input)?.is_some();
    let frac: &str = if has_dot {
        take_while(0.., |c: char| c.is_ascii_digit()).parse_next(input)?
    } else {
        ""
    };
    // optional exponent
    let has_exp = opt(one_of(['e', 'E'])).parse_next(input)?.is_some();
    if has_exp {
        opt(one_of(['+', '-'])).parse_next(input)?;
        take_while(1.., |c: char| c.is_ascii_digit()).parse_next(input)?;
    }
    // Must have consumed something meaningful
    if pre.is_empty() && frac.is_empty() && !has_dot && !has_exp {
        return Err(winnow::error::ErrMode::Backtrack(
            winnow::error::ContextError::new(),
        ));
    }
    let consumed = &start[..start.len() - input.len()];
    consumed
        .parse::<f64>()
        .map_err(|_| winnow::error::ErrMode::Backtrack(winnow::error::ContextError::new()))
}

/// Parse a number that could be either int or float.
/// Returns (value, is_float).
pub fn xt_number(input: &mut &str) -> ModalResult<XtNumber> {
    ws(input)?;
    let checkpoint = *input;
    // Try float first (will match things with dots or exponents)
    if let Ok(f) = xt_float_raw(input) {
        let consumed = &checkpoint[..checkpoint.len() - input.len()];
        // If it contained a dot or exponent, it's definitely a float
        if consumed.contains('.') || consumed.contains('e') || consumed.contains('E') {
            return Ok(XtNumber::Float(f));
        }
        // Otherwise it's an integer that also parses as float
        *input = checkpoint;
    }
    // Try integer
    match ascii::dec_int::<_, i64, _>.parse_next(input) {
        Ok(i) => Ok(XtNumber::Int(i)),
        Err(e) => {
            // Fall back to float
            *input = checkpoint;
            xt_float_raw(input).map(XtNumber::Float).map_err(|_| e)
        }
    }
}

#[derive(Debug, Clone, Copy)]
pub enum XtNumber {
    Int(i64),
    Float(f64),
}

impl XtNumber {
    pub fn as_i64(self) -> i64 {
        match self {
            XtNumber::Int(i) => i,
            XtNumber::Float(f) => f as i64,
        }
    }
    pub fn as_f64(self) -> f64 {
        match self {
            XtNumber::Int(i) => i as f64,
            XtNumber::Float(f) => f,
        }
    }
}

/// Parse a 3-component vector (3 bare floats, no parens in compact format).
pub fn xt_vec3(input: &mut &str) -> ModalResult<[f64; 3]> {
    let x = xt_float(input)?;
    let y = xt_float(input)?;
    let z = xt_float(input)?;
    Ok([x, y, z])
}

/// Parse a single character token (sense, face_sense, region_type, etc.).
/// Skips leading whitespace, reads one non-whitespace char.
pub fn xt_char(input: &mut &str) -> ModalResult<char> {
    ws(input)?;
    one_of(|c: char| !c.is_ascii_whitespace()).parse_next(input)
}

/// Parse a sense character: '+' (forward) or '-' (reversed).
pub fn xt_sense(input: &mut &str) -> ModalResult<crate::types::XtSense> {
    ws(input)?;
    alt((
        '+'.value(crate::types::XtSense::Forward),
        '-'.value(crate::types::XtSense::Reversed),
    ))
    .parse_next(input)
}

/// Parse an entity pointer in compact format: bare unsigned integer.
/// 0 = null.
pub fn xt_ptr(input: &mut &str) -> ModalResult<usize> {
    ws(input)?;
    ascii::dec_uint::<_, usize, _>.parse_next(input)
}

/// Parse a boolean: 0/1 or T/F in compact format.
pub fn xt_bool(input: &mut &str) -> ModalResult<bool> {
    ws(input)?;
    alt((
        '1'.value(true),
        '0'.value(false),
        'T'.value(true),
        'F'.value(false),
    ))
    .parse_next(input)
}

/// Parse a question-mark prefixed optional pointer: `?<int>`.
/// Returns Some(ptr) if `?` present, otherwise parses a bare pointer.
pub fn xt_optional_ptr(input: &mut &str) -> ModalResult<usize> {
    ws(input)?;
    let _ = opt('?').parse_next(input)?;
    ascii::dec_uint::<_, usize, _>.parse_next(input)
}

/// Parse a byte value (unsigned 8-bit).
pub fn xt_byte(input: &mut &str) -> ModalResult<u8> {
    ws(input)?;
    ascii::dec_uint::<_, u8, _>.parse_next(input)
}

/// Parse one complete whitespace-delimited token, skipping leading whitespace.
/// Reads all non-whitespace characters and returns them as a String.
/// Used for opaque PS30 attribute flag strings (e.g. `FFFFTFTFFFFFFF2`).
pub fn xt_token(input: &mut &str) -> ModalResult<String> {
    ws(input)?;
    let s: &str = take_while(1.., |c: char| !c.is_ascii_whitespace()).parse_next(input)?;
    Ok(s.to_owned())
}

// ── Modern format helpers ────────────────────────────────────────────────────

/// Parse a `#N` pointer (modern format). Returns the entity index.
pub fn xt_hash_ptr(input: &mut &str) -> ModalResult<usize> {
    ws(input)?;
    preceded('#', ascii::dec_uint::<_, usize, _>).parse_next(input)
}

/// Parse a parenthesized vector `( x y z )` (modern format).
pub fn xt_paren_vec3(input: &mut &str) -> ModalResult<[f64; 3]> {
    ws(input)?;
    '('.parse_next(input)?;
    let v = xt_vec3(input)?;
    ws(input)?;
    ')'.parse_next(input)?;
    Ok(v)
}

/// Parse a parenthesized list of floats `( f1 f2 ... )` (modern format).
pub fn xt_paren_floats(input: &mut &str) -> ModalResult<Vec<f64>> {
    ws(input)?;
    '('.parse_next(input)?;
    let mut vals = Vec::new();
    loop {
        ws(input)?;
        if let Ok(_) = winnow::Parser::<&str, char, winnow::error::ErrMode<winnow::error::ContextError>>::parse_next(&mut ')', input) {
            break;
        }
        vals.push(xt_float(input)?);
    }
    Ok(vals)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_int() {
        let mut i = "42 rest";
        assert_eq!(xt_int(&mut i).unwrap(), 42);
        assert_eq!(i, " rest");
    }

    #[test]
    fn parse_negative_int() {
        let mut i = "-7 x";
        assert_eq!(xt_int(&mut i).unwrap(), -7);
    }

    #[test]
    fn parse_float_with_dot() {
        let mut i = ".004572 rest";
        let f = xt_float(&mut i).unwrap();
        assert!((f - 0.004572).abs() < 1e-10);
    }

    #[test]
    fn parse_float_sci() {
        let mut i = "1e-8 x";
        let f = xt_float(&mut i).unwrap();
        assert!((f - 1e-8).abs() < 1e-20);
    }

    #[test]
    fn parse_float_negative_zero() {
        let mut i = "-0 x";
        let f = xt_float(&mut i).unwrap();
        assert!(f == 0.0);
    }

    #[test]
    fn parse_vec3() {
        let mut i = "1.0 2.0 3.0 rest";
        let v = xt_vec3(&mut i).unwrap();
        assert_eq!(v, [1.0, 2.0, 3.0]);
    }

    #[test]
    fn parse_sense() {
        let mut i = "+rest";
        assert_eq!(xt_sense(&mut i).unwrap(), crate::types::XtSense::Forward);
        let mut i = "- rest";
        assert_eq!(xt_sense(&mut i).unwrap(), crate::types::XtSense::Reversed);
    }

    #[test]
    fn parse_number_int_vs_float() {
        let mut i = "42 rest";
        assert!(matches!(xt_number(&mut i).unwrap(), XtNumber::Int(42)));

        let mut i = "3.14 rest";
        match xt_number(&mut i).unwrap() {
            XtNumber::Float(f) => assert!((f - 3.14).abs() < 1e-10),
            _ => panic!("expected float"),
        }
    }
}
