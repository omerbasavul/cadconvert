//! The two encodings the crate format is built out of.
//!
//! # LZ4, of a sort
//!
//! Every compressed block in a crate file is LZ4 behind one leading byte: zero
//! for a single block, otherwise a count followed by that many length-prefixed
//! blocks. The split is at 2 GB and nothing here comes near it, so one block it
//! is.
//!
//! What is written here is **literals only** — a legal LZ4 stream that finds no
//! matches. That is deliberate rather than lazy. The sections it applies to are
//! the token table, the path tables and the value representations, and on the
//! pilot assembly those come to a few hundred kilobytes against a file of
//! tens of megabytes. The bulk is arrays, and an array of positions is
//! incompressible while an array of indices is compressed by the integer
//! coding below, which does the work LZ4 would not. A matcher here would buy
//! back a fraction of a per cent for a great deal of code that has to be right.
//!
//! # Integer coding
//!
//! The crate format's own, and the reason a mesh's indices cost a third of what
//! they would raw. A run of integers is written as one common delta, two bits
//! of code for each integer, and then a delta only for those that are not the
//! common one:
//!
//! ```text
//! [common: i32 or i64][codes: 2 bits each][deltas: 1, 2, 4 or 8 bytes]
//! ```
//!
//! Code 0 means "the common delta", 1, 2 and 3 mean a delta of one, two or
//! four bytes (eight for the 64-bit form). Triangle indices step by one far
//! more often than not, so most of a mesh's index array comes out as two bits
//! per index. The whole thing then goes through the LZ4 above.

/// Compress into the crate format's block wrapper.
pub fn compress(data: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(data.len() + data.len() / 200 + 16);
    out.push(0u8); // one block
    lz4_literals(data, &mut out);
    out
}

/// An LZ4 block that is all literals and no matches.
///
/// The format: a token byte whose high nibble is the literal count and whose
/// low nibble is the match length, then any length extension bytes, then the
/// literals. A block may end on literals, which is what every block here does.
fn lz4_literals(data: &[u8], out: &mut Vec<u8>) {
    // The last match must begin at least 12 bytes from the end and a block
    // must end with at least 5 literals. With no matches at all neither rule
    // can be broken, so the whole input is one literal run.
    // One run, however long. A second would need a match between them, and a
    // decoder reading the two bytes after a literal run as a match offset is
    // exactly what it would find.
    let run = data.len();
    if run < 15 {
        out.push((run as u8) << 4);
    } else {
        out.push(0xF0);
        let mut remaining = run - 15;
        while remaining >= 255 {
            out.push(255);
            remaining -= 255;
        }
        out.push(remaining as u8);
    }
    out.extend_from_slice(data);
}

/// Integer coding, 32 bits wide.
pub fn compress_ints32(values: &[i32]) -> Vec<u8> {
    let deltas: Vec<i64> = deltas(values.iter().map(|&v| v as i64));
    let common = most_common(&deltas);
    encode(&deltas, common, 4)
}

/// Integer coding, 64 bits wide. Used for nothing this writes yet; here so the
/// pair reads as one thing.
#[allow(dead_code)]
pub fn compress_ints64(values: &[i64]) -> Vec<u8> {
    let deltas: Vec<i64> = deltas(values.iter().copied());
    let common = most_common(&deltas);
    encode(&deltas, common, 8)
}

fn deltas(values: impl Iterator<Item = i64>) -> Vec<i64> {
    let mut out = Vec::new();
    let mut previous = 0i64;
    for v in values {
        out.push(v - previous);
        previous = v;
    }
    out
}

/// The delta worth spending zero bytes on.
///
/// Whichever occurs most often, which for a triangle index array is 1 and for
/// a run of spec indices is also 1. Counted rather than assumed: a scene whose
/// arrays step by something else should still get the short encoding.
fn most_common(deltas: &[i64]) -> i64 {
    use std::collections::HashMap;
    let mut counts: HashMap<i64, usize> = HashMap::new();
    for &d in deltas {
        *counts.entry(d).or_insert(0) += 1;
    }
    counts
        .into_iter()
        // Ties go to the smaller value so the encoding is the same every run.
        .max_by_key(|&(value, count)| (count, std::cmp::Reverse(value)))
        .map(|(value, _)| value)
        .unwrap_or(1)
}

fn encode(deltas: &[i64], common: i64, width: usize) -> Vec<u8> {
    let count = deltas.len();
    let code_bytes = count.div_ceil(4);
    let mut working = Vec::with_capacity(width + code_bytes + count * width);

    if width == 4 {
        working.extend_from_slice(&(common as i32).to_le_bytes());
    } else {
        working.extend_from_slice(&common.to_le_bytes());
    }
    working.resize(width + code_bytes, 0);

    let mut payload = Vec::with_capacity(count * width);
    for (i, &delta) in deltas.iter().enumerate() {
        let code: u8 = if delta == common {
            0
        } else if let Ok(v) = i8::try_from(delta) {
            payload.push(v as u8);
            1
        } else if let Ok(v) = i16::try_from(delta) {
            payload.extend_from_slice(&v.to_le_bytes());
            2
        } else if width == 4 {
            payload.extend_from_slice(&(delta as i32).to_le_bytes());
            3
        } else {
            payload.extend_from_slice(&delta.to_le_bytes());
            3
        };
        working[width + (i >> 2)] |= code << (2 * (i & 3));
    }
    working.extend_from_slice(&payload);
    compress(&working)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The decoder the tests read with: the same walk `tools/usdc_decode.py`
    /// does, so that what is asserted here is what that tool would report.
    fn lz4_decompress(src: &[u8]) -> Vec<u8> {
        let mut out = Vec::new();
        let mut i = 0;
        while i < src.len() {
            let token = src[i];
            i += 1;
            let mut literal = (token >> 4) as usize;
            if literal == 15 {
                loop {
                    let b = src[i];
                    i += 1;
                    literal += b as usize;
                    if b != 255 {
                        break;
                    }
                }
            }
            out.extend_from_slice(&src[i..i + literal]);
            i += literal;
            if i >= src.len() {
                break;
            }
            let offset = u16::from_le_bytes([src[i], src[i + 1]]) as usize;
            i += 2;
            let mut length = (token & 0xF) as usize;
            if length == 15 {
                loop {
                    let b = src[i];
                    i += 1;
                    length += b as usize;
                    if b != 255 {
                        break;
                    }
                }
            }
            length += 4;
            let start = out.len() - offset;
            for k in 0..length {
                let byte = out[start + k];
                out.push(byte);
            }
        }
        out
    }

    fn decompress(buf: &[u8]) -> Vec<u8> {
        assert_eq!(buf[0], 0, "one block");
        lz4_decompress(&buf[1..])
    }

    fn decode_ints32(buf: &[u8], count: usize) -> Vec<i32> {
        let raw = decompress(buf);
        let common = i32::from_le_bytes(raw[..4].try_into().unwrap()) as i64;
        let code_bytes = count.div_ceil(4);
        let codes = &raw[4..4 + code_bytes];
        let mut at = 4 + code_bytes;
        let mut out = Vec::with_capacity(count);
        let mut previous = 0i64;
        for i in 0..count {
            let code = (codes[i >> 2] >> (2 * (i & 3))) & 3;
            let delta = match code {
                0 => common,
                1 => {
                    at += 1;
                    raw[at - 1] as i8 as i64
                }
                2 => {
                    at += 2;
                    i16::from_le_bytes(raw[at - 2..at].try_into().unwrap()) as i64
                }
                _ => {
                    at += 4;
                    i32::from_le_bytes(raw[at - 4..at].try_into().unwrap()) as i64
                }
            };
            previous += delta;
            out.push(previous as i32);
        }
        out
    }

    #[test]
    fn a_literal_only_block_is_still_lz4() {
        for size in [0usize, 1, 14, 15, 16, 254, 255, 270, 4096, 70_000] {
            let data: Vec<u8> = (0..size).map(|i| (i * 31 % 251) as u8).collect();
            let round = decompress(&compress(&data));
            assert_eq!(round, data, "{size} bytes did not survive");
        }
    }

    #[test]
    fn a_run_that_steps_by_one_costs_two_bits_an_integer() {
        // What a triangle index array mostly is, and the reason the crate
        // format's own coding beats writing them raw.
        let values: Vec<i32> = (0..40_000).collect();
        let packed = compress_ints32(&values);
        assert_eq!(decode_ints32(&packed, values.len()), values);

        // Four bytes each raw; two bits each here, plus the block's own
        // overhead. Anything near the raw size means the common delta was not
        // found.
        assert!(
            packed.len() < values.len() / 3,
            "{} bytes for {} integers",
            packed.len(),
            values.len()
        );
    }

    #[test]
    fn every_width_of_delta_round_trips() {
        let values: Vec<i32> = vec![
            0, 1, 2, 3,            // the common step
            5, 260, -260,          // one and two byte deltas
            100_000, -100_000,     // four byte
            i32::MAX / 2, i32::MIN / 2,
            0, 0, 0,
        ];
        let packed = compress_ints32(&values);
        assert_eq!(decode_ints32(&packed, values.len()), values);
    }

    #[test]
    fn a_run_of_one_value_and_an_empty_run_both_encode() {
        assert_eq!(decode_ints32(&compress_ints32(&[7]), 1), vec![7]);
        assert!(decode_ints32(&compress_ints32(&[]), 0).is_empty());
    }

    #[test]
    fn the_common_delta_is_counted_rather_than_assumed() {
        // Steps of three. If the encoder assumed one, every entry would need
        // a byte of its own.
        let values: Vec<i32> = (0..8_000).map(|i| i * 3).collect();
        let packed = compress_ints32(&values);
        assert_eq!(decode_ints32(&packed, values.len()), values);
        assert!(packed.len() < values.len() / 3, "{} bytes", packed.len());
    }
}
