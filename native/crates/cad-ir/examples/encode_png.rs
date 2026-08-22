//! Write raw RGBA as a PNG with this crate's own encoder.
//!
//! `cargo run -p cad-ir --example encode_png -- rgba.bin WIDTH HEIGHT out.png`
//!
//! Here for `tools/make_grain.py`, so that an asset prepared offline is
//! written by the same code that writes one at conversion time.

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let [raw, width, height, out] = &args[..] else {
        eprintln!("usage: encode_png <rgba.bin> <width> <height> <out.png>");
        std::process::exit(2);
    };
    let rgba = std::fs::read(raw).expect("read the pixels");
    let (w, h) = (width.parse().expect("width"), height.parse().expect("height"));
    assert_eq!(rgba.len(), (w as usize) * (h as usize) * 4, "not RGBA");
    std::fs::write(out, cad_ir::image::encode_png(w, h, &rgba)).expect("write");
}
