//! What the converter does with a file that is not what it says it is.
//!
//! A converter runs on files someone else produced, and some of them are
//! truncated by a failed download, corrupted in transit, or simply not the
//! format their extension claims. The contract is not that every such file
//! converts — most cannot — but that **none of them panics**. A panic in a
//! library is an abort in the CLI and a dead host process wherever the C ABI
//! is loaded into one, which is the difference between a file that fails and a
//! service that falls over.
//!
//! The sample is a real 6 KB Parasolid part, mutated here in the ways files
//! actually arrive broken.

use cad_convert::{Options, Target};
use std::path::{Path, PathBuf};

fn sample() -> Vec<u8> {
    std::fs::read(Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/samples/small.x_t"))
        .expect("the bundled sample")
}

fn scratch(name: &str, bytes: &[u8]) -> PathBuf {
    let dir = std::env::temp_dir().join("cad-convert-malformed");
    std::fs::create_dir_all(&dir).expect("a scratch directory");
    let path = dir.join(name);
    std::fs::write(&path, bytes).expect("writing the scratch file");
    path
}

/// Read it, mesh it and write it. Returns whether a file came out; the point
/// is that neither outcome is a panic.
fn convert(name: &str, bytes: &[u8]) -> bool {
    let input = scratch(name, bytes);
    let output = input.with_extension("glb");
    let options = Options {
        target: Target::GlbLean,
        // The twin lookup would read whatever sits beside the scratch file.
        use_parasolid_twin: false,
        ..Default::default()
    };
    let made = cad_convert::convert(&input, &output, &options).is_ok();
    let _ = std::fs::remove_file(&input);
    let _ = std::fs::remove_file(&output);
    made
}

#[test]
fn the_sample_itself_converts() {
    let input = scratch("good.x_t", &sample());
    let output = input.with_extension("glb");
    let summary = cad_convert::convert(&input, &output, &Options::default())
        .expect("the sample is a real part and must convert");
    assert!(summary.bodies >= 1, "{summary:?}");
    assert!(summary.triangles > 100, "{summary:?}");
    assert_eq!(summary.faces_meshed, summary.faces, "every face should mesh");
    let _ = std::fs::remove_file(&input);
    let _ = std::fs::remove_file(&output);
}

#[test]
fn a_truncated_file_does_not_panic() {
    let whole = sample();
    // Every tenth of the file, plus the very first and last slivers: a download
    // can stop anywhere, and the interesting places are the boundaries between
    // the header, the schema and the entity stream.
    for tenth in 0..=20 {
        let n = whole.len() * tenth / 20;
        convert(&format!("trunc-{tenth}.x_t"), &whole[..n]);
    }
}

#[test]
fn corrupted_bytes_do_not_panic() {
    let whole = sample();
    // A deterministic scatter of flipped bytes over the body of the file —
    // no randomness, so a failure here is reproducible from the test alone.
    for seed in 0..8u64 {
        let mut bytes = whole.clone();
        let mut x = seed.wrapping_mul(6364136223846793005).wrapping_add(1);
        for _ in 0..64 {
            x = x.wrapping_mul(6364136223846793005).wrapping_add(1442695040888963407);
            let at = (x >> 33) as usize % bytes.len();
            bytes[at] ^= 0xA5;
        }
        convert(&format!("flip-{seed}.x_t"), &bytes);
    }
}

#[test]
fn nothing_at_all_does_not_panic() {
    assert!(!convert("empty.x_t", b""), "an empty file cannot convert");
    assert!(!convert("empty.stp", b""), "an empty file cannot convert");
    assert!(
        !convert("noise.x_t", &(0u16..8192).map(|i| (i * 37) as u8).collect::<Vec<_>>()),
        "arbitrary bytes are not a Parasolid file"
    );
}

#[test]
fn a_step_with_a_header_and_no_bodies_does_not_panic() {
    let step = b"ISO-10303-21;\nHEADER;\nENDSEC;\nDATA;\nENDSEC;\nEND-ISO-10303-21;\n";
    convert("bare.stp", step);
}

#[test]
fn a_file_of_no_known_format_is_refused_by_name() {
    let input = scratch("mystery.dat", b"not a CAD file at all");
    let err = cad_convert::convert(&input, &input.with_extension("glb"), &Options::default())
        .expect_err("this is not a format");
    assert!(
        matches!(err, cad_convert::Error::UnknownFormat(_)),
        "a file of no known format should say so, not fail as a read error: {err}"
    );
    let _ = std::fs::remove_file(&input);
}
