//! Two ways a Parasolid file was read as something other than what it says,
//! each without a word: a name with a letter outside ASCII, and a body whose
//! own counters were followed as if they were pointers.
//!
//! Both were reported from assemblies that cannot be attached here, so each is
//! rebuilt on the bundled 6 KB part: the smallest change to it that the reader
//! used to get wrong.

use cad_convert::{Options, Summary};
use std::path::Path;

fn sample() -> Vec<u8> {
    std::fs::read(Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/samples/small.x_t"))
        .expect("the bundled sample")
}

/// Where `text` stands in `bytes`, each of its bytes found whether or not a
/// line ends before it: a transmit file is wrapped at a fixed width, and the
/// wrap falls wherever it falls.
fn find(bytes: &[u8], text: &[u8]) -> Vec<usize> {
    'starts: for start in 0..bytes.len() {
        let mut at = start;
        let mut places = Vec::with_capacity(text.len());
        for &wanted in text {
            while matches!(bytes.get(at), Some(b'\r' | b'\n')) {
                at += 1;
            }
            if bytes.get(at) != Some(&wanted) {
                continue 'starts;
            }
            places.push(at);
            at += 1;
        }
        return places;
    }
    panic!("{:?} is not in the sample", String::from_utf8_lossy(text));
}

/// The sample with one byte of it, found by the text around it, replaced.
fn with(mut bytes: Vec<u8>, around: &[u8], which: usize, by: &[u8]) -> Vec<u8> {
    let at = find(&bytes, around)[which];
    bytes.splice(at..at + 1, by.iter().copied());
    bytes
}

fn convert(name: &str, bytes: &[u8]) -> Summary {
    let dir = std::env::temp_dir().join("cad-convert-names-and-bodies");
    std::fs::create_dir_all(&dir).expect("a scratch directory");
    let input = dir.join(name);
    std::fs::write(&input, bytes).expect("writing the scratch file");
    let output = input.with_extension("glb");
    let options = Options {
        // The twin lookup would read whatever sits beside the scratch file.
        use_parasolid_twin: false,
        ..Default::default()
    };
    let summary = cad_convert::convert(&input, &output, &options).expect("the part converts");
    let _ = std::fs::remove_file(&input);
    let _ = std::fs::remove_file(&output);
    summary
}

/// Every string the file carries, as the reader has it.
fn strings(bytes: Vec<u8>) -> Vec<String> {
    let file = xt_parser::parse_raw_owned(xt_parser::decode(bytes)).expect("the part parses");
    assert!(file.truncated.is_none(), "{:?}", file.truncated);
    file.entities
        .iter()
        .map(|e| file.entities.var_char(e).iter().collect::<String>())
        .filter(|s| !s.is_empty())
        .collect()
}

const NAME: &[u8] = b"79 14 108 SWEntUnchanged";

/// The same name with its last `e` an `ä`: two bytes in UTF-8, so the length
/// written before it goes from 14 to 15.
fn utf8_name() -> Vec<u8> {
    let longer = with(sample(), NAME, 4, b"5");
    with(longer, NAME_15, NAME.len() - 2, "ä".as_bytes())
}
const NAME_15: &[u8] = b"79 15 108 SWEntUnchanged";

/// And as a Western Windows writes it: one byte, 0xE4, and the length as it
/// was.
fn code_page_name() -> Vec<u8> {
    with(sample(), NAME, NAME.len() - 2, &[0xE4])
}

#[test]
fn a_name_with_a_letter_outside_ascii_does_not_end_the_read() {
    let plain = convert("plain.x_t", &sample());
    assert_eq!(plain.faces, 10, "{plain:?}");

    // The length of a string is in bytes, and read as a length in characters
    // it took the first digit of the next entity with it. Here that left a
    // type the stream could not be read as; on the assembly it was reported
    // from it left a `1`, the terminator, and a third of the file went unread
    // with nothing said.
    for (name, bytes) in [("utf8.x_t", utf8_name()), ("code-page.x_t", code_page_name())] {
        let read = convert(name, &bytes);
        assert_eq!(
            (read.faces, read.faces_meshed, read.triangles),
            (plain.faces, plain.faces_meshed, plain.triangles),
            "{name}: {read:?}"
        );
        assert_eq!(read.warnings, plain.warnings, "{name}");
    }
}

#[test]
fn the_letter_is_read_whichever_way_it_was_written() {
    assert!(strings(sample()).iter().any(|s| s == "SWEntUnchanged"));
    // U+FFFD stood where the code page's letter was.
    for bytes in [utf8_name(), code_page_name()] {
        let names = strings(bytes);
        assert!(names.iter().any(|s| s == "SWEntUnchangäd"), "{names:?}");
    }
}

#[test]
fn a_terminator_with_more_of_the_stream_after_it_is_said() {
    // A `1` where a type should be ends the stream. One that is followed by
    // the rest of the file did not end it, and the conversion says so rather
    // than passing a part of the file off as the whole.
    let region = find(&sample(), b" 19 14 64 0 1 0 8 5 S")[0];
    let mut bytes = sample();
    bytes.splice(region..region, b" 1 0".iter().copied());
    let file = xt_parser::parse_raw_owned(xt_parser::decode(bytes)).expect("what there is of it parses");
    let stopped = file.truncated.expect("the stream stopped early");
    assert!(stopped.detail.contains("after what read as its terminator"), "{stopped}");
}

#[test]
fn a_body_follows_its_pointers_and_not_its_counters() {
    // A second shell that nothing points at, over the same faces as the
    // first, and the body's highest node id — a plain integer — set to that
    // shell's index. Followed as a pointer it gave the body every face twice;
    // in the assembly this was reported from, the number happened to be the
    // index of another body's shell, and a small plate was given every face
    // of a frame member.
    let mut bytes = sample();
    let region = find(&bytes, b" 19 14 64 0 1 0 8 5 S")[0];
    bytes.splice(region..region, b" 13 300 65 0 1 0 13 0 0 14 0".iter().copied());
    // 297 becomes 300, a digit at a time so that a line break between them
    // stays where it is.
    let mut around = *b"Z1 297 2 3 ";
    for (which, digit) in [(3, b'3'), (4, b'0'), (5, b'0')] {
        bytes = with(bytes, &around, which, &[digit]);
        around[which] = digit;
    }
    let read = convert("second-shell.x_t", &bytes);
    assert_eq!(read.faces, 10, "{read:?}");
    assert_eq!(read.faces_meshed, 10, "{read:?}");
}
