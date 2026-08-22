//! Convert a texture the appearance library names and report what came out.
//!
//! Usage: cargo run -p cad-ir --example texture_probe -- <file> [out.png]

fn main() {
    let mut args = std::env::args().skip(1);
    let Some(path) = args.next() else {
        eprintln!("usage: texture_probe <image> [out.png]");
        std::process::exit(2);
    };
    let bytes = std::fs::read(&path).expect("read the image");
    match cad_ir::image::load(&path, &bytes) {
        Ok(image) => {
            println!(
                "{} -> {} {}x{}, {} bytes in, {} out ({:.0}%)",
                path,
                image.mime.as_str(),
                image.width,
                image.height,
                bytes.len(),
                image.bytes.len(),
                100.0 * image.bytes.len() as f64 / bytes.len() as f64
            );
            if let Some(out) = args.next() {
                std::fs::write(&out, &image.bytes).expect("write");
                println!("wrote {out}");
            }
        }
        Err(e) => println!("{path}: {e}"),
    }
}
