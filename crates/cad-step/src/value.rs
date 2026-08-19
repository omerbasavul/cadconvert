//! Argument decoding for Part 21 records.
//!
//! Two layers, deliberately:
//!
//! * [`Args`] — a forward cursor with typed readers that decode straight out of
//!   the file bytes and allocate nothing. This is what the geometry extractors
//!   use. A 32 MB assembly holds ~500 k records and ~560 k coordinates; building
//!   an owned tree for each one is the difference between tens of milliseconds
//!   and seconds.
//! * [`Value`] — an owned tree, for tooling, unknown entities and diagnostics.
//!
//! Both share one tokeniser, so they cannot disagree about what a record says.

use crate::error::{Result, StepError};
use std::borrow::Cow;

/// A decoded Part 21 parameter.
#[derive(Debug, Clone, PartialEq)]
pub enum Value<'a> {
    /// `#123`
    Ref(u32),
    /// An integer literal — no decimal point.
    Int(i64),
    /// A real literal — has a decimal point.
    Real(f64),
    /// A string literal, unescaped.
    Str(Cow<'a, str>),
    /// `.SOMETHING.`, without the dots.
    Enum(&'a str),
    /// A binary literal `"0A1B…"`, kept as its hex text.
    Binary(&'a str),
    /// `$` — an unset optional attribute.
    Unset,
    /// `*` — a derived attribute, redeclared by a subtype.
    Derived,
    /// `( … )` — an aggregate.
    List(Vec<Value<'a>>),
    /// `KEYWORD( … )` — a typed parameter (a SELECT wrapping its value).
    Typed(&'a str, Vec<Value<'a>>),
}

impl<'a> Value<'a> {
    /// The referenced entity id, if this is a `#N`.
    pub fn as_ref_id(&self) -> Option<u32> {
        match self {
            Value::Ref(id) => Some(*id),
            _ => None,
        }
    }

    /// The numeric value, accepting both integer and real literals.
    pub fn as_f64(&self) -> Option<f64> {
        match self {
            Value::Real(v) => Some(*v),
            Value::Int(v) => Some(*v as f64),
            _ => None,
        }
    }

    pub fn as_str(&self) -> Option<&str> {
        match self {
            Value::Str(s) => Some(s.as_ref()),
            Value::Enum(s) | Value::Binary(s) => Some(s),
            _ => None,
        }
    }

    pub fn as_list(&self) -> Option<&[Value<'a>]> {
        match self {
            Value::List(v) => Some(v),
            Value::Typed(_, v) => Some(v),
            _ => None,
        }
    }

    /// `.T.` / `.F.` as a bool; `.U.` (unknown) reads as `None`.
    pub fn as_bool(&self) -> Option<bool> {
        match self {
            Value::Enum("T") => Some(true),
            Value::Enum("F") => Some(false),
            _ => None,
        }
    }
}

/// A forward cursor over one record's argument bytes.
///
/// Every reader consumes exactly one parameter and the separating comma. The
/// readers do not backtrack, matching how STEP entity attributes are read:
/// positionally, in declaration order.
#[derive(Debug, Clone)]
pub struct Args<'a> {
    buf: &'a [u8],
    pos: usize,
    end: usize,
}

impl<'a> Args<'a> {
    /// Build a cursor over `buf[span]`, which must be the bytes strictly inside
    /// a record's outermost parentheses.
    pub fn new(buf: &'a [u8], span: std::ops::Range<u32>) -> Self {
        Args {
            buf,
            pos: span.start as usize,
            end: span.end as usize,
        }
    }

    /// Byte offset of the cursor, for error messages.
    pub fn offset(&self) -> usize {
        self.pos
    }

    /// True once every parameter has been consumed.
    pub fn is_empty(&mut self) -> bool {
        self.skip_ws();
        self.pos >= self.end
    }

    fn skip_ws(&mut self) {
        while self.pos < self.end {
            let b = self.buf[self.pos];
            if b.is_ascii_whitespace() {
                self.pos += 1;
            } else if b == b'/' && self.pos + 1 < self.end && self.buf[self.pos + 1] == b'*' {
                match find_comment_end(self.buf, self.pos, self.end) {
                    Some(next) => self.pos = next,
                    None => self.pos = self.end,
                }
            } else {
                return;
            }
        }
    }

    /// Consume the comma that separates two parameters, if present.
    fn eat_separator(&mut self) {
        self.skip_ws();
        if self.pos < self.end && self.buf[self.pos] == b',' {
            self.pos += 1;
        }
    }

    fn err(&self, detail: impl Into<String>) -> StepError {
        StepError::value(self.pos, detail)
    }

    /// Find the end of the parameter starting at the cursor, without decoding.
    fn param_span(&mut self) -> Result<std::ops::Range<usize>> {
        self.skip_ws();
        if self.pos >= self.end {
            return Err(self.err("expected a parameter, found end of record"));
        }
        let start = self.pos;
        let mut depth = 0usize;
        let mut i = self.pos;
        // End of the last byte that is part of the value itself. Trailing
        // whitespace and trailing comments must not land inside the span, or a
        // string's closing quote stops being the last byte and every reader
        // that trims the quotes slices the wrong bytes.
        let mut sig_end = self.pos;
        while i < self.end {
            match self.buf[i] {
                b'(' => {
                    depth += 1;
                    i += 1;
                    sig_end = i;
                }
                b')' => {
                    // Depth can only reach zero here if the record is damaged;
                    // the scan guarantees balance inside the span.
                    depth = depth.saturating_sub(1);
                    i += 1;
                    sig_end = i;
                }
                b',' if depth == 0 => break,
                b'\'' => {
                    i = string_end(self.buf, i, self.end)
                        .ok_or_else(|| StepError::value(i, "unterminated string literal"))?;
                    sig_end = i;
                }
                b'"' => {
                    i = match memchr::memchr(b'"', &self.buf[i + 1..self.end]) {
                        Some(off) => i + 1 + off + 1,
                        None => {
                            return Err(StepError::value(i, "unterminated binary literal"));
                        }
                    };
                    sig_end = i;
                }
                b'/' if i + 1 < self.end && self.buf[i + 1] == b'*' => {
                    i = find_comment_end(self.buf, i, self.end).unwrap_or(self.end);
                }
                b if b.is_ascii_whitespace() => i += 1,
                _ => {
                    i += 1;
                    sig_end = i;
                }
            }
        }
        self.pos = i;
        let mut span = start..sig_end.max(start);
        while span.start < span.end && self.buf[span.start].is_ascii_whitespace() {
            span.start += 1;
        }
        self.eat_separator();
        Ok(span)
    }

    /// Discard the next parameter.
    pub fn skip(&mut self) -> Result<()> {
        self.param_span().map(|_| ())
    }

    /// Discard `n` parameters.
    pub fn skip_n(&mut self, n: usize) -> Result<()> {
        for _ in 0..n {
            self.skip()?;
        }
        Ok(())
    }

    /// Read `#N`.
    pub fn next_ref(&mut self) -> Result<u32> {
        let span = self.param_span()?;
        parse_ref(self.buf, &span).ok_or_else(|| {
            StepError::value(
                span.start,
                format!("expected `#N`, found `{}`", show(self.buf, &span)),
            )
        })
    }

    /// Read `#N` or `$`.
    pub fn next_opt_ref(&mut self) -> Result<Option<u32>> {
        let span = self.param_span()?;
        if is_unset(self.buf, &span) {
            return Ok(None);
        }
        parse_ref(self.buf, &span).map(Some).ok_or_else(|| {
            StepError::value(
                span.start,
                format!("expected `#N` or `$`, found `{}`", show(self.buf, &span)),
            )
        })
    }

    /// Read a real or integer literal as `f64`.
    pub fn next_f64(&mut self) -> Result<f64> {
        let span = self.param_span()?;
        parse_f64(self.buf, &span).ok_or_else(|| {
            StepError::value(
                span.start,
                format!("expected a number, found `{}`", show(self.buf, &span)),
            )
        })
    }

    /// Read a real or integer literal as `f64`, mapping `$` to `None`.
    pub fn next_opt_f64(&mut self) -> Result<Option<f64>> {
        let span = self.param_span()?;
        if is_unset(self.buf, &span) {
            return Ok(None);
        }
        parse_f64(self.buf, &span).map(Some).ok_or_else(|| {
            StepError::value(
                span.start,
                format!("expected a number or `$`, found `{}`", show(self.buf, &span)),
            )
        })
    }

    /// Read a measure: either a bare number or a typed one like
    /// `LENGTH_MEASURE(25.4)`.
    ///
    /// STEP declares measures through SELECT types, so whether the wrapper is
    /// written is up to the exporter and both spellings appear in real files —
    /// often in the same file, for the same attribute.
    pub fn next_measure_f64(&mut self) -> Result<f64> {
        let span = self.param_span()?;
        if let Some(v) = parse_f64(self.buf, &span) {
            return Ok(v);
        }
        // `KEYWORD( value )` — take the single wrapped value.
        if let Some(off) = memchr::memchr(b'(', &self.buf[span.clone()]) {
            let open = span.start + off;
            if self.buf[span.end - 1] == b')' {
                let inner = trim_span(self.buf, open + 1..span.end - 1);
                if let Some(v) = parse_f64(self.buf, &inner) {
                    return Ok(v);
                }
            }
        }
        Err(StepError::value(
            span.start,
            format!("expected a measure, found `{}`", show(self.buf, &span)),
        ))
    }

    /// Read an integer literal.
    pub fn next_i64(&mut self) -> Result<i64> {
        let span = self.param_span()?;
        parse_i64(self.buf, &span).ok_or_else(|| {
            StepError::value(
                span.start,
                format!("expected an integer, found `{}`", show(self.buf, &span)),
            )
        })
    }

    /// Read a string literal, unescaping Part 21 control directives.
    pub fn next_str(&mut self) -> Result<Cow<'a, str>> {
        let span = self.param_span()?;
        if is_unset(self.buf, &span) {
            return Ok(Cow::Borrowed(""));
        }
        if span.end - span.start < 2 || self.buf[span.start] != b'\'' {
            return Err(StepError::value(
                span.start,
                format!("expected a string, found `{}`", show(self.buf, &span)),
            ));
        }
        Ok(decode_string(&self.buf[span.start + 1..span.end - 1]))
    }

    /// Read `.SOMETHING.`, returning the text between the dots.
    pub fn next_enum(&mut self) -> Result<&'a str> {
        let span = self.param_span()?;
        parse_enum(self.buf, &span).ok_or_else(|| {
            StepError::value(
                span.start,
                format!("expected `.ENUM.`, found `{}`", show(self.buf, &span)),
            )
        })
    }

    /// Read `.T.` or `.F.`; `.U.` yields `None`.
    pub fn next_bool(&mut self) -> Result<Option<bool>> {
        match self.next_enum()? {
            "T" => Ok(Some(true)),
            "F" => Ok(Some(false)),
            "U" => Ok(None),
            other => Err(self.err(format!("expected `.T.`/`.F.`/`.U.`, found `.{other}.`"))),
        }
    }

    /// Read `(#a,#b,…)` into `out`, which is cleared first.
    pub fn next_ref_list(&mut self, out: &mut Vec<u32>) -> Result<()> {
        out.clear();
        let span = self.param_span()?;
        let inner = list_body(self.buf, &span).ok_or_else(|| {
            StepError::value(
                span.start,
                format!("expected a list, found `{}`", show(self.buf, &span)),
            )
        })?;
        for item in split_top_level(self.buf, inner) {
            out.push(parse_ref(self.buf, &item).ok_or_else(|| {
                StepError::value(
                    item.start,
                    format!("expected `#N` in list, found `{}`", show(self.buf, &item)),
                )
            })?);
        }
        Ok(())
    }

    /// Read `(1.,2.,…)` into `out`, which is cleared first.
    pub fn next_f64_list(&mut self, out: &mut Vec<f64>) -> Result<()> {
        out.clear();
        let span = self.param_span()?;
        let inner = list_body(self.buf, &span).ok_or_else(|| {
            StepError::value(
                span.start,
                format!("expected a list, found `{}`", show(self.buf, &span)),
            )
        })?;
        for item in split_top_level(self.buf, inner) {
            out.push(parse_f64(self.buf, &item).ok_or_else(|| {
                StepError::value(
                    item.start,
                    format!("expected a number in list, found `{}`", show(self.buf, &item)),
                )
            })?);
        }
        Ok(())
    }

    /// Read `(1,2,…)` into `out`, which is cleared first.
    pub fn next_i64_list(&mut self, out: &mut Vec<i64>) -> Result<()> {
        out.clear();
        let span = self.param_span()?;
        let inner = list_body(self.buf, &span).ok_or_else(|| {
            StepError::value(
                span.start,
                format!("expected a list, found `{}`", show(self.buf, &span)),
            )
        })?;
        for item in split_top_level(self.buf, inner) {
            out.push(parse_i64(self.buf, &item).ok_or_else(|| {
                StepError::value(
                    item.start,
                    format!("expected an integer in list, found `{}`", show(self.buf, &item)),
                )
            })?);
        }
        Ok(())
    }

    /// Read `((1.,2.),(3.,4.),…)` into `out`, which is cleared first.
    ///
    /// This is the shape of a B-spline surface's control point grid.
    pub fn next_f64_grid(&mut self, out: &mut Vec<Vec<f64>>) -> Result<()> {
        out.clear();
        let span = self.param_span()?;
        let inner = list_body(self.buf, &span).ok_or_else(|| {
            StepError::value(
                span.start,
                format!("expected a list of lists, found `{}`", show(self.buf, &span)),
            )
        })?;
        for row in split_top_level(self.buf, inner) {
            let body = list_body(self.buf, &row).ok_or_else(|| {
                StepError::value(
                    row.start,
                    format!("expected an inner list, found `{}`", show(self.buf, &row)),
                )
            })?;
            let mut vals = Vec::new();
            for item in split_top_level(self.buf, body) {
                vals.push(parse_f64(self.buf, &item).ok_or_else(|| {
                    StepError::value(
                        item.start,
                        format!("expected a number, found `{}`", show(self.buf, &item)),
                    )
                })?);
            }
            out.push(vals);
        }
        Ok(())
    }

    /// Read `((#a,#b),(#c,#d),…)` into `out`, which is cleared first.
    pub fn next_ref_grid(&mut self, out: &mut Vec<Vec<u32>>) -> Result<()> {
        out.clear();
        let span = self.param_span()?;
        let inner = list_body(self.buf, &span).ok_or_else(|| {
            StepError::value(
                span.start,
                format!("expected a list of lists, found `{}`", show(self.buf, &span)),
            )
        })?;
        for row in split_top_level(self.buf, inner) {
            let body = list_body(self.buf, &row).ok_or_else(|| {
                StepError::value(
                    row.start,
                    format!("expected an inner list, found `{}`", show(self.buf, &row)),
                )
            })?;
            let mut refs = Vec::new();
            for item in split_top_level(self.buf, body) {
                refs.push(parse_ref(self.buf, &item).ok_or_else(|| {
                    StepError::value(
                        item.start,
                        format!("expected `#N`, found `{}`", show(self.buf, &item)),
                    )
                })?);
            }
            out.push(refs);
        }
        Ok(())
    }

    /// Decode the next parameter into an owned [`Value`] tree.
    pub fn next_value(&mut self) -> Result<Value<'a>> {
        let span = self.param_span()?;
        decode_value(self.buf, span)
    }

    /// Decode every remaining parameter into [`Value`]s.
    pub fn rest(&mut self) -> Result<Vec<Value<'a>>> {
        let mut out = Vec::new();
        while !self.is_empty() {
            out.push(self.next_value()?);
        }
        Ok(out)
    }
}

// ---------------------------------------------------------------------------
// Scalar decoders. Each takes a trimmed span and returns None on a type
// mismatch, so callers can produce an error that names the actual text.
// ---------------------------------------------------------------------------

fn is_unset(buf: &[u8], span: &std::ops::Range<usize>) -> bool {
    span.end - span.start == 1 && buf[span.start] == b'$'
}

fn parse_ref(buf: &[u8], span: &std::ops::Range<usize>) -> Option<u32> {
    let s = &buf[span.clone()];
    if s.len() < 2 || s[0] != b'#' {
        return None;
    }
    let mut v: u32 = 0;
    for &b in &s[1..] {
        if !b.is_ascii_digit() {
            return None;
        }
        v = v.checked_mul(10)?.checked_add(u32::from(b - b'0'))?;
    }
    Some(v)
}

fn parse_i64(buf: &[u8], span: &std::ops::Range<usize>) -> Option<i64> {
    std::str::from_utf8(&buf[span.clone()]).ok()?.trim().parse().ok()
}

fn parse_f64(buf: &[u8], span: &std::ops::Range<usize>) -> Option<f64> {
    let s = std::str::from_utf8(&buf[span.clone()]).ok()?.trim();
    if s.is_empty() {
        return None;
    }
    // `1.` and `1.E-3` are legal Part 21 reals that Rust's parser accepts, and
    // `+1.` needs no special handling either. Only a bare integer needs the
    // widening, which `parse::<f64>` also does.
    s.parse().ok()
}

fn parse_enum<'a>(buf: &'a [u8], span: &std::ops::Range<usize>) -> Option<&'a str> {
    let s = &buf[span.clone()];
    if s.len() >= 2 && s[0] == b'.' && s[s.len() - 1] == b'.' {
        std::str::from_utf8(&s[1..s.len() - 1]).ok()
    } else {
        None
    }
}

/// For a span that is `( … )`, the span of the bytes strictly inside.
fn list_body(buf: &[u8], span: &std::ops::Range<usize>) -> Option<std::ops::Range<usize>> {
    if span.end > span.start && buf[span.start] == b'(' && buf[span.end - 1] == b')' {
        Some(span.start + 1..span.end - 1)
    } else {
        None
    }
}

/// Split a list body on top-level commas.
fn split_top_level(buf: &[u8], span: std::ops::Range<usize>) -> Vec<std::ops::Range<usize>> {
    let mut out = Vec::new();
    let mut cur = span.start;
    let mut depth = 0usize;
    let mut i = span.start;
    while i < span.end {
        match buf[i] {
            b'(' => {
                depth += 1;
                i += 1;
            }
            b')' => {
                depth = depth.saturating_sub(1);
                i += 1;
            }
            b',' if depth == 0 => {
                out.push(trim_span(buf, cur..i));
                i += 1;
                cur = i;
            }
            b'\'' => i = string_end(buf, i, span.end).unwrap_or(span.end),
            b'"' => {
                i = match memchr::memchr(b'"', &buf[i + 1..span.end]) {
                    Some(off) => i + 1 + off + 1,
                    None => span.end,
                }
            }
            b'/' if i + 1 < span.end && buf[i + 1] == b'*' => {
                i = find_comment_end(buf, i, span.end).unwrap_or(span.end);
            }
            _ => i += 1,
        }
    }
    let last = trim_span(buf, cur..span.end);
    // A trailing empty slice means the list itself was empty, not that it had a
    // final empty element.
    if !(out.is_empty() && last.is_empty()) {
        out.push(last);
    }
    out
}

fn trim_span(buf: &[u8], mut r: std::ops::Range<usize>) -> std::ops::Range<usize> {
    while r.start < r.end && buf[r.start].is_ascii_whitespace() {
        r.start += 1;
    }
    while r.end > r.start && buf[r.end - 1].is_ascii_whitespace() {
        r.end -= 1;
    }
    r
}

/// `i` points at `'`; returns the index just past the closing quote.
fn string_end(buf: &[u8], i: usize, end: usize) -> Option<usize> {
    let mut i = i + 1;
    loop {
        let off = memchr::memchr(b'\'', &buf[i..end])?;
        i += off + 1;
        if i < end && buf[i] == b'\'' {
            i += 1;
            continue;
        }
        return Some(i);
    }
}

/// `i` points at `/*`; returns the index just past the closing `*/`.
fn find_comment_end(buf: &[u8], i: usize, end: usize) -> Option<usize> {
    let mut j = i + 2;
    loop {
        let off = memchr::memchr(b'*', &buf[j..end])?;
        j += off + 1;
        if j < end && buf[j] == b'/' {
            return Some(j + 1);
        }
    }
}

fn show(buf: &[u8], span: &std::ops::Range<usize>) -> String {
    let s = &buf[span.clone()];
    let cut = s.len().min(40);
    String::from_utf8_lossy(&s[..cut]).into_owned()
}

fn decode_value<'a>(buf: &'a [u8], span: std::ops::Range<usize>) -> Result<Value<'a>> {
    if span.is_empty() {
        return Ok(Value::Unset);
    }
    let first = buf[span.start];
    match first {
        b'#' => parse_ref(buf, &span)
            .map(Value::Ref)
            .ok_or_else(|| StepError::value(span.start, "malformed entity reference")),
        b'$' => Ok(Value::Unset),
        b'*' => Ok(Value::Derived),
        b'.' => parse_enum(buf, &span)
            .map(Value::Enum)
            .ok_or_else(|| StepError::value(span.start, "malformed enumeration")),
        b'\'' => Ok(Value::Str(decode_string(&buf[span.start + 1..span.end - 1]))),
        b'"' => std::str::from_utf8(&buf[span.start + 1..span.end - 1])
            .map(Value::Binary)
            .map_err(|_| StepError::value(span.start, "binary literal is not ASCII")),
        b'(' => {
            let body = list_body(buf, &span).expect("checked leading paren");
            let mut items = Vec::new();
            for item in split_top_level(buf, body) {
                items.push(decode_value(buf, item)?);
            }
            Ok(Value::List(items))
        }
        b'0'..=b'9' | b'+' | b'-' => {
            if buf[span.clone()].contains(&b'.') {
                parse_f64(buf, &span)
                    .map(Value::Real)
                    .ok_or_else(|| StepError::value(span.start, "malformed real"))
            } else {
                parse_i64(buf, &span)
                    .map(Value::Int)
                    .ok_or_else(|| StepError::value(span.start, "malformed integer"))
            }
        }
        _ => {
            // `KEYWORD( … )` — a typed parameter.
            let open = memchr::memchr(b'(', &buf[span.clone()])
                .map(|o| span.start + o)
                .ok_or_else(|| StepError::value(span.start, "unrecognised parameter"))?;
            let kw = std::str::from_utf8(&buf[trim_span(buf, span.start..open)])
                .map_err(|_| StepError::value(span.start, "typed parameter keyword is not ASCII"))?;
            let mut items = Vec::new();
            for item in split_top_level(buf, open + 1..span.end - 1) {
                items.push(decode_value(buf, item)?);
            }
            Ok(Value::Typed(kw, items))
        }
    }
}

/// Unescape a Part 21 string literal body (without its surrounding quotes).
///
/// Handles the control directives from clause 6.3.3.4: `''`, `\S\`, `\X\HH`,
/// `\X2\…\X0\`, `\X4\…\X0\`, `\N\`, `\T\`, and the `\P?\` page directive. The
/// borrowed fast path covers the overwhelmingly common case of a plain ASCII
/// name with no escapes.
pub fn decode_string(raw: &[u8]) -> Cow<'_, str> {
    let plain = !raw.iter().any(|&b| b == b'\'' || b == b'\\' || b >= 0x80);
    if plain {
        // Safe: every byte is ASCII and not a quote or backslash.
        return Cow::Borrowed(std::str::from_utf8(raw).unwrap_or(""));
    }

    let mut out = String::with_capacity(raw.len());
    let mut i = 0;
    // The `\P?\` directive selects an ISO 8859 page for subsequent `\S\`
    // characters. Page A is Latin-1, which is also the default.
    let mut page_high: u32 = 0x0000;

    while i < raw.len() {
        let b = raw[i];
        match b {
            b'\'' if i + 1 < raw.len() && raw[i + 1] == b'\'' => {
                out.push('\'');
                i += 2;
            }
            b'\\' if i + 1 < raw.len() => {
                let (consumed, ok) = decode_escape(&raw[i..], &mut out, &mut page_high);
                if ok {
                    i += consumed;
                } else {
                    // Not a recognised directive — many writers emit raw
                    // backslashes in Windows paths, which the spec forbids but
                    // reality contains. Take it literally.
                    out.push('\\');
                    i += 1;
                }
            }
            0x00..=0x7F => {
                out.push(b as char);
                i += 1;
            }
            _ => {
                // Bytes above 0x7F: prefer a valid UTF-8 sequence (the "raw
                // bytes" option many writers take), and fall back to Latin-1.
                match utf8_seq_len(&raw[i..]) {
                    Some(len) => {
                        out.push_str(std::str::from_utf8(&raw[i..i + len]).expect("validated"));
                        i += len;
                    }
                    None => {
                        out.push(char::from(b));
                        i += 1;
                    }
                }
            }
        }
    }
    Cow::Owned(out)
}

/// Decode one `\…\` directive. Returns (bytes consumed, recognised).
fn decode_escape(s: &[u8], out: &mut String, page_high: &mut u32) -> (usize, bool) {
    match s.get(1).copied() {
        Some(b'N') if s.get(2) == Some(&b'\\') => {
            out.push('\n');
            (3, true)
        }
        Some(b'T') if s.get(2) == Some(&b'\\') => {
            out.push('\t');
            (3, true)
        }
        Some(b'P') if s.len() >= 4 && s[3] == b'\\' => {
            // `\PA\` … `\PH\` select ISO 8859-1 … 8859-8.
            let page = s[2].to_ascii_uppercase();
            *page_high = if page.is_ascii_uppercase() {
                u32::from(page - b'A')
            } else {
                0
            };
            (4, true)
        }
        Some(b'S') if s.get(2) == Some(&b'\\') && s.len() >= 4 => {
            // The next character has 128 added to it, within the selected page.
            // Page A (Latin-1) is the only one that maps directly to Unicode;
            // for the others we still produce the Latin-1 codepoint rather than
            // dropping the character, and record nothing else about the page.
            let _ = *page_high;
            let ch = u32::from(s[3]) + 128;
            out.push(char::from_u32(ch).unwrap_or('\u{FFFD}'));
            (4, true)
        }
        Some(b'X') if s.get(2) == Some(&b'\\') && s.len() >= 5 => {
            // `\X\HH` — one octet, Latin-1.
            match hex_byte(&s[3..5]) {
                Some(v) => {
                    out.push(char::from(v));
                    (5, true)
                }
                None => (0, false),
            }
        }
        Some(b'X') if matches!(s.get(2), Some(b'2') | Some(b'4')) && s.get(3) == Some(&b'\\') => {
            let width = if s[2] == b'2' { 4 } else { 8 };
            let mut i = 4;
            while i + width <= s.len() {
                if s[i] == b'\\' {
                    break;
                }
                let Some(cp) = hex_u32(&s[i..i + width]) else {
                    return (0, false);
                };
                if width == 4 && (0xD800..0xDC00).contains(&cp) {
                    // A UTF-16 high surrogate: pair it with the low surrogate.
                    let Some(lo) = hex_u32(s.get(i + 4..i + 8).unwrap_or(&[])) else {
                        return (0, false);
                    };
                    let combined = 0x1_0000 + ((cp - 0xD800) << 10) + (lo - 0xDC00);
                    out.push(char::from_u32(combined).unwrap_or('\u{FFFD}'));
                    i += 8;
                } else {
                    out.push(char::from_u32(cp).unwrap_or('\u{FFFD}'));
                    i += width;
                }
            }
            // Consume the `\X0\` terminator when present.
            if s.get(i) == Some(&b'\\') && s.get(i + 1) == Some(&b'X') && s.get(i + 2) == Some(&b'0')
            {
                i += 4.min(s.len() - i);
            }
            (i, true)
        }
        Some(b'\\') => {
            out.push('\\');
            (2, true)
        }
        _ => (0, false),
    }
}

fn hex_byte(s: &[u8]) -> Option<u8> {
    hex_u32(s).map(|v| v as u8)
}

fn hex_u32(s: &[u8]) -> Option<u32> {
    if s.is_empty() {
        return None;
    }
    let mut v: u32 = 0;
    for &b in s {
        v = v.checked_mul(16)?.checked_add(u32::from((b as char).to_digit(16)? as u8))?;
    }
    Some(v)
}

/// Length of the UTF-8 sequence at the start of `s`, if it is well formed.
fn utf8_seq_len(s: &[u8]) -> Option<usize> {
    let len = match s[0] {
        0xC2..=0xDF => 2,
        0xE0..=0xEF => 3,
        0xF0..=0xF4 => 4,
        _ => return None,
    };
    if s.len() < len {
        return None;
    }
    std::str::from_utf8(&s[..len]).ok().map(|_| len)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn args(src: &str) -> Args<'_> {
        Args::new(src.as_bytes(), 0..src.len() as u32)
    }

    #[test]
    fn reads_scalars_positionally() {
        let mut a = args("'name',#12,1.5,-3,.T.,$,*");
        assert_eq!(a.next_str().unwrap(), "name");
        assert_eq!(a.next_ref().unwrap(), 12);
        assert_eq!(a.next_f64().unwrap(), 1.5);
        assert_eq!(a.next_i64().unwrap(), -3);
        assert_eq!(a.next_bool().unwrap(), Some(true));
        assert_eq!(a.next_opt_ref().unwrap(), None);
        assert!(!a.is_empty());
    }

    #[test]
    fn reads_a_cartesian_point_coordinate_list() {
        let mut a = args("'',(1.,2.5,-3.75)");
        assert_eq!(a.next_str().unwrap(), "");
        let mut xyz = Vec::new();
        a.next_f64_list(&mut xyz).unwrap();
        assert_eq!(xyz, vec![1.0, 2.5, -3.75]);
        assert!(a.is_empty());
    }

    #[test]
    fn reads_a_control_point_grid() {
        let mut a = args("((#1,#2),(#3,#4),(#5,#6))");
        let mut grid = Vec::new();
        a.next_ref_grid(&mut grid).unwrap();
        assert_eq!(grid, vec![vec![1, 2], vec![3, 4], vec![5, 6]]);
    }

    #[test]
    fn an_empty_list_has_no_elements() {
        let mut a = args("()");
        let mut v = Vec::new();
        a.next_ref_list(&mut v).unwrap();
        assert!(v.is_empty());
    }

    #[test]
    fn commas_inside_strings_and_lists_do_not_split_parameters() {
        let mut a = args("'a,b',(1.,2.),'c'");
        assert_eq!(a.next_str().unwrap(), "a,b");
        let mut v = Vec::new();
        a.next_f64_list(&mut v).unwrap();
        assert_eq!(v, vec![1.0, 2.0]);
        assert_eq!(a.next_str().unwrap(), "c");
        assert!(a.is_empty());
    }

    #[test]
    fn typed_parameters_decode_as_typed_values() {
        let mut a = args("COUNT_MEASURE(0.)");
        match a.next_value().unwrap() {
            Value::Typed(kw, items) => {
                assert_eq!(kw, "COUNT_MEASURE");
                assert_eq!(items, vec![Value::Real(0.0)]);
            }
            other => panic!("expected a typed value, got {other:?}"),
        }
    }

    #[test]
    fn string_escapes_decode() {
        assert_eq!(decode_string(b"plain"), "plain");
        assert_eq!(decode_string(b"it''s"), "it's");
        assert_eq!(decode_string(br"a\X\41b"), "aAb");
        assert_eq!(decode_string(br"\X2\00E7\X0\"), "ç");
        assert_eq!(decode_string(br"\X2\D83DDE00\X0\"), "\u{1F600}");
        assert_eq!(decode_string(br"line\N\two"), "line\ntwo");
        // A Windows path with unescaped backslashes, which real writers emit.
        assert_eq!(decode_string(br"C:\Users\tolga"), r"C:\Users\tolga");
        // Raw UTF-8 bytes, the option this project's sample files use.
        assert_eq!(decode_string("çelik".as_bytes()), "çelik");
    }

    #[test]
    fn a_real_with_a_trailing_point_parses() {
        let mut a = args("1.,2.E-3,.5,-0.");
        assert_eq!(a.next_f64().unwrap(), 1.0);
        assert_eq!(a.next_f64().unwrap(), 0.002);
        assert_eq!(a.next_f64().unwrap(), 0.5);
        assert_eq!(a.next_f64().unwrap(), 0.0);
    }

    #[test]
    fn measures_read_wrapped_or_bare() {
        let mut a = args("LENGTH_MEASURE(25.4),1.5,COUNT_MEASURE( 3. )");
        assert_eq!(a.next_measure_f64().unwrap(), 25.4);
        assert_eq!(a.next_measure_f64().unwrap(), 1.5);
        assert_eq!(a.next_measure_f64().unwrap(), 3.0);
    }

    #[test]
    fn comments_between_parameters_are_ignored() {
        let mut a = args("'x'/*c*/,#9");
        assert_eq!(a.next_str().unwrap(), "x");
        assert_eq!(a.next_ref().unwrap(), 9);
    }
}
