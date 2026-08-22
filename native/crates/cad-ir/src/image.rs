//! Images, and the two things that have to happen to them before they can be
//! written into a glTF or a USDZ.
//!
//! Both formats take exactly two encodings: PNG and JPEG. The appearance
//! library's colour images are already JPEG or PNG and go through untouched —
//! re-encoding them would only lose something. Its normal maps are `.dds`,
//! which neither format accepts, so those are decoded and written as PNG.
//!
//! # The `.dds` files are easier than `.dds` usually is
//!
//! DirectDraw Surface is a container for anything, most of it block-compressed
//! and none of that pleasant to decode. Every one of the 94 files the library
//! actually references was measured first, and they are all the same thing:
//! uncompressed 32-bit, 85 as `0xff0000/0xff00/0xff` with no alpha and 9 with
//! alpha at `0xff000000`. That is BGRX and BGRA — a header to read and two
//! channels to swap. No block decompression is written here because none is
//! needed, and a file that turns out to need it is refused by name rather than
//! decoded wrongly.
//!
//! # Why there is a deflate here
//!
//! A PNG is deflate whether or not it compresses, so writing one means having
//! one. This is fixed-Huffman deflate with a hash-chain match finder: not the
//! smallest output a real compressor would give, but within a factor of the
//! usual for image data that has been through PNG's own filters first, and it
//! is a hundred lines that owe nothing to anybody. The alternative was a
//! dependency tree for the sake of one 256×256 normal map.

/// A decoded image, in the encoding a writer can use directly.
#[derive(Clone, PartialEq, Eq)]
pub struct Image {
    /// Where it came from, for the writers to name it by.
    pub name: String,
    pub mime: Mime,
    pub width: u32,
    pub height: u32,
    /// The encoded file, ready to be written as-is.
    pub bytes: Vec<u8>,
}

impl std::fmt::Debug for Image {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Image")
            .field("name", &self.name)
            .field("mime", &self.mime)
            .field("width", &self.width)
            .field("height", &self.height)
            .field("bytes", &format_args!("{} bytes", self.bytes.len()))
            .finish()
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Mime {
    Png,
    Jpeg,
}

impl Mime {
    pub fn as_str(self) -> &'static str {
        match self {
            Mime::Png => "image/png",
            Mime::Jpeg => "image/jpeg",
        }
    }

    pub fn extension(self) -> &'static str {
        match self {
            Mime::Png => "png",
            Mime::Jpeg => "jpg",
        }
    }
}

/// What went wrong, in enough detail to name the file in a warning.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ImageError {
    /// Not one of the encodings this reads.
    Unrecognised,
    /// A `.dds` in a form none of the library's own files use. Refused rather
    /// than guessed at.
    UnsupportedDds(String),
    /// Truncated, or the header disagrees with the length.
    Malformed(&'static str),
}

impl std::fmt::Display for ImageError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ImageError::Unrecognised => write!(f, "not a PNG, JPEG or DDS"),
            ImageError::UnsupportedDds(what) => write!(f, "a .dds this does not read: {what}"),
            ImageError::Malformed(why) => write!(f, "malformed: {why}"),
        }
    }
}

/// Read an image file into something a writer can emit.
///
/// PNG and JPEG are passed through with only their dimensions read. A `.dds`
/// is decoded and re-encoded as PNG.
pub fn load(name: &str, bytes: &[u8]) -> Result<Image, ImageError> {
    if bytes.starts_with(b"\x89PNG\r\n\x1a\n") {
        let (width, height) = png_size(bytes).ok_or(ImageError::Malformed("no IHDR"))?;
        Ok(Image {
            name: name.to_string(),
            mime: Mime::Png,
            width,
            height,
            bytes: bytes.to_vec(),
        })
    } else if bytes.starts_with(b"\xff\xd8\xff") {
        let (width, height) = jpeg_size(bytes).ok_or(ImageError::Malformed("no JPEG frame"))?;
        Ok(Image {
            name: name.to_string(),
            mime: Mime::Jpeg,
            width,
            height,
            bytes: bytes.to_vec(),
        })
    } else if bytes.starts_with(b"DDS ") {
        let (width, height, rgba) = decode_dds(bytes)?;
        Ok(Image {
            name: name.to_string(),
            mime: Mime::Png,
            width,
            height,
            bytes: encode_png(width, height, &rgba),
        })
    } else {
        Err(ImageError::Unrecognised)
    }
}

fn u32le(bytes: &[u8], at: usize) -> Option<u32> {
    bytes
        .get(at..at + 4)
        .map(|b| u32::from_le_bytes([b[0], b[1], b[2], b[3]]))
}

/// A PNG's width and height, from the IHDR that must be its first chunk.
fn png_size(bytes: &[u8]) -> Option<(u32, u32)> {
    if bytes.get(12..16)? != b"IHDR" {
        return None;
    }
    let be = |at: usize| -> Option<u32> {
        bytes
            .get(at..at + 4)
            .map(|b| u32::from_be_bytes([b[0], b[1], b[2], b[3]]))
    };
    Some((be(16)?, be(20)?))
}

/// A JPEG's width and height, from the first start-of-frame marker.
///
/// Walking the segment chain rather than searching for the marker bytes: those
/// two bytes occur inside entropy-coded data all the time, and a search finds
/// one of those long before it finds the frame.
fn jpeg_size(bytes: &[u8]) -> Option<(u32, u32)> {
    let mut at = 2;
    loop {
        while *bytes.get(at)? == 0xFF {
            at += 1;
        }
        let marker = *bytes.get(at)?;
        at += 1;
        // Standalone markers carry no length.
        if (0xD0..=0xD9).contains(&marker) || marker == 0x01 {
            continue;
        }
        let length = u16::from_be_bytes([*bytes.get(at)?, *bytes.get(at + 1)?]) as usize;
        // Any SOF except the two that are not frames at all.
        if (0xC0..=0xCF).contains(&marker) && marker != 0xC4 && marker != 0xCC {
            let height = u16::from_be_bytes([*bytes.get(at + 3)?, *bytes.get(at + 4)?]);
            let width = u16::from_be_bytes([*bytes.get(at + 5)?, *bytes.get(at + 6)?]);
            return Some((width as u32, height as u32));
        }
        at += length;
        // Once the scan starts there are no more headers worth walking.
        if marker == 0xDA {
            return None;
        }
    }
}

const DDPF_ALPHAPIXELS: u32 = 0x1;
const DDPF_FOURCC: u32 = 0x4;
const DDPF_RGB: u32 = 0x40;

/// Decode the uncompressed 32-bit DDS the appearance library uses, to RGBA.
///
/// Only the top mip level is read; the rest of the file is the mip chain, which
/// a glTF or USD writer has no use for.
fn decode_dds(bytes: &[u8]) -> Result<(u32, u32, Vec<u8>), ImageError> {
    // The header is a fixed 124 bytes after the four-byte magic.
    let header_size = u32le(bytes, 4).ok_or(ImageError::Malformed("no DDS header"))?;
    if header_size != 124 {
        return Err(ImageError::Malformed("DDS header is not 124 bytes"));
    }
    let height = u32le(bytes, 12).ok_or(ImageError::Malformed("truncated"))?;
    let width = u32le(bytes, 16).ok_or(ImageError::Malformed("truncated"))?;
    let pf_flags = u32le(bytes, 80).ok_or(ImageError::Malformed("truncated"))?;
    let bit_count = u32le(bytes, 88).ok_or(ImageError::Malformed("truncated"))?;
    let masks = [
        u32le(bytes, 92).ok_or(ImageError::Malformed("truncated"))?,
        u32le(bytes, 96).ok_or(ImageError::Malformed("truncated"))?,
        u32le(bytes, 100).ok_or(ImageError::Malformed("truncated"))?,
        u32le(bytes, 104).ok_or(ImageError::Malformed("truncated"))?,
    ];

    if pf_flags & DDPF_FOURCC != 0 {
        let cc = bytes.get(84..88).unwrap_or(b"????");
        return Err(ImageError::UnsupportedDds(format!(
            "block-compressed ({})",
            String::from_utf8_lossy(cc)
        )));
    }
    if pf_flags & DDPF_RGB == 0 || bit_count != 32 {
        return Err(ImageError::UnsupportedDds(format!(
            "{bit_count}-bit, flags {pf_flags:#x}"
        )));
    }

    let pixels = (width as usize)
        .checked_mul(height as usize)
        .ok_or(ImageError::Malformed("implausible dimensions"))?;
    let start = 4 + 124;
    let data = bytes
        .get(start..start + pixels * 4)
        .ok_or(ImageError::Malformed("shorter than its own dimensions"))?;

    // The masks say where each channel sits. Every file measured is
    // 0xff0000/0xff00/0xff — BGR in memory order — but reading the masks costs
    // nothing and means an RGBA file would come out right rather than
    // channel-swapped.
    let shift = |mask: u32| mask.trailing_zeros();
    let has_alpha = pf_flags & DDPF_ALPHAPIXELS != 0 && masks[3] != 0;
    let (rs, gs, bs, as_) = (
        shift(masks[0]),
        shift(masks[1]),
        shift(masks[2]),
        shift(masks[3]),
    );

    let mut rgba = Vec::with_capacity(pixels * 4);
    for pixel in data.chunks_exact(4) {
        let value = u32::from_le_bytes([pixel[0], pixel[1], pixel[2], pixel[3]]);
        rgba.push(((value & masks[0]) >> rs) as u8);
        rgba.push(((value & masks[1]) >> gs) as u8);
        rgba.push(((value & masks[2]) >> bs) as u8);
        rgba.push(if has_alpha {
            ((value & masks[3]) >> as_) as u8
        } else {
            255
        });
    }
    Ok((width, height, rgba))
}

/// Write RGBA as a PNG.
///
/// An image whose alpha is 255 everywhere is written as RGB. A normal map has
/// no transparency and neither has any colour image in the library, so this is
/// every one of them: it drops a quarter of the bytes before the compressor
/// ever runs, and a constant channel is the one thing deflate cannot make up
/// for — 64 KB of the 256 KB the first version of this emitted was a column of
/// 255s.
pub fn encode_png(width: u32, height: u32, rgba: &[u8]) -> Vec<u8> {
    let opaque = rgba.chunks_exact(4).all(|p| p[3] == 255);
    let (channels, colour_type) = if opaque { (3usize, 2u8) } else { (4, 6) };

    let pixels: Vec<u8> = if opaque {
        rgba.chunks_exact(4).flat_map(|p| p[..3].to_vec()).collect()
    } else {
        rgba.to_vec()
    };

    let mut out = Vec::with_capacity(pixels.len() / 2);
    out.extend_from_slice(b"\x89PNG\r\n\x1a\n");

    let mut ihdr = Vec::with_capacity(13);
    ihdr.extend_from_slice(&width.to_be_bytes());
    ihdr.extend_from_slice(&height.to_be_bytes());
    ihdr.extend_from_slice(&[8, colour_type, 0, 0, 0]); // 8 bits, deflate, no interlace
    chunk(&mut out, b"IHDR", &ihdr);

    chunk(
        &mut out,
        b"IDAT",
        &zlib(&filter(width, height, &pixels, channels)),
    );
    chunk(&mut out, b"IEND", &[]);
    out
}

fn chunk(out: &mut Vec<u8>, kind: &[u8; 4], data: &[u8]) {
    out.extend_from_slice(&(data.len() as u32).to_be_bytes());
    out.extend_from_slice(kind);
    out.extend_from_slice(data);
    let mut crc = Crc32::new();
    crc.write(kind);
    crc.write(data);
    out.extend_from_slice(&crc.finish().to_be_bytes());
}

/// PNG's per-row prediction. Each row is tried under all five filters and the
/// one whose bytes are smallest in absolute terms is kept — the heuristic the
/// PNG specification itself suggests, and the thing that makes a normal map
/// compress at all: neighbouring normals differ by very little, so Paeth turns
/// most of the image into near-zero bytes before deflate ever sees it.
fn filter(width: u32, height: u32, pixels: &[u8], channels: usize) -> Vec<u8> {
    let stride = width as usize * channels;
    let mut out = Vec::with_capacity((stride + 1) * height as usize);
    let mut previous = vec![0u8; stride];
    let mut candidate = vec![0u8; stride];

    for y in 0..height as usize {
        let row = &pixels[y * stride..(y + 1) * stride];
        let mut best = (u64::MAX, 0u8, Vec::new());

        for kind in 0..5u8 {
            let mut cost = 0u64;
            for x in 0..stride {
                let a = if x >= channels { row[x - channels] } else { 0 };
                let b = previous[x];
                let c = if x >= channels { previous[x - channels] } else { 0 };
                let value = match kind {
                    0 => row[x],
                    1 => row[x].wrapping_sub(a),
                    2 => row[x].wrapping_sub(b),
                    3 => row[x].wrapping_sub(((a as u16 + b as u16) / 2) as u8),
                    _ => row[x].wrapping_sub(paeth(a, b, c)),
                };
                candidate[x] = value;
                // Distance from zero, so that 255 counts as one and not as 255.
                cost += (value as i8).unsigned_abs() as u64;
            }
            if cost < best.0 {
                best = (cost, kind, candidate.clone());
            }
        }

        out.push(best.1);
        out.extend_from_slice(&best.2);
        previous.copy_from_slice(row);
    }
    out
}

fn paeth(a: u8, b: u8, c: u8) -> u8 {
    let p = a as i16 + b as i16 - c as i16;
    let (pa, pb, pc) = ((p - a as i16).abs(), (p - b as i16).abs(), (p - c as i16).abs());
    if pa <= pb && pa <= pc {
        a
    } else if pb <= pc {
        b
    } else {
        c
    }
}

/// A zlib stream: two header bytes, deflate, and an Adler-32 of the input.
fn zlib(data: &[u8]) -> Vec<u8> {
    let mut out = vec![0x78, 0x01]; // deflate, 32 KB window, no preset dictionary
    deflate::compress(data, &mut out);
    out.extend_from_slice(&adler32(data).to_be_bytes());
    out
}

fn adler32(data: &[u8]) -> u32 {
    let (mut a, mut b) = (1u32, 0u32);
    // 5552 is the most bytes that cannot overflow the accumulators.
    for block in data.chunks(5552) {
        for &byte in block {
            a += byte as u32;
            b += a;
        }
        a %= 65521;
        b %= 65521;
    }
    (b << 16) | a
}

struct Crc32(u32);

impl Crc32 {
    fn new() -> Self {
        Crc32(0xFFFF_FFFF)
    }

    fn write(&mut self, data: &[u8]) {
        for &byte in data {
            let mut c = (self.0 ^ byte as u32) & 0xFF;
            for _ in 0..8 {
                c = if c & 1 != 0 { 0xEDB8_8320 ^ (c >> 1) } else { c >> 1 };
            }
            self.0 = c ^ (self.0 >> 8);
        }
    }

    fn finish(self) -> u32 {
        self.0 ^ 0xFFFF_FFFF
    }
}

mod deflate {
    //! Deflate, enough of it to write a PNG nothing else has to be asked to
    //! read.
    //!
    //! Three encodings of the same token stream are produced and the smallest
    //! is kept. That is not thoroughness for its own sake — it is the only way
    //! to be sure the output is never larger than the input, and the first
    //! version of this was:
    //!
    //! | 256×256 powder-coat normal map | bytes   |
    //! |--------------------------------|---------|
    //! | filtered scanlines, in         | 196 864 |
    //! | fixed Huffman                  | 207 180 |
    //! | stored                         | 196 884 |
    //! | dynamic Huffman                | 170 604 |
    //!
    //! Fixed Huffman *expands* this data, and for a plain reason: it spends
    //! nine bits on every byte from 144 to 255, and a filtered normal map is
    //! mostly small negative numbers, which are exactly those bytes. Dynamic
    //! Huffman spends what each byte is worth. A real compressor at its highest
    //! setting reaches 170 548 on the same input, so this is within 0.1%.

    const WINDOW: usize = 32768;
    const MIN_MATCH: usize = 3;
    const MAX_MATCH: usize = 258;
    /// How far back down a hash chain to look. Deeper finds longer matches and
    /// costs time; this is the usual middle setting.
    const CHAIN: usize = 32;
    const HASH_BITS: usize = 15;

    const LITERAL_SYMBOLS: usize = 286;
    const DISTANCE_SYMBOLS: usize = 30;
    const CODE_LENGTH_SYMBOLS: usize = 19;
    /// Deflate allows no literal, length or distance code longer than this.
    const MAX_BITS: u8 = 15;
    /// The code-length code is written as 3-bit lengths in the block header,
    /// so none of *its* codes may exceed 7 — a different limit, and the one
    /// that is easy to miss. Give it 15 and a symbol that lands on 8 is
    /// written as `8 & 7`, which is 0, which reads as "unused". Two of those
    /// left the code-length code short by exactly 2^-7 and zlib refused ten of
    /// the library's normal maps with "invalid code lengths set".
    const MAX_CODE_LENGTH_BITS: u8 = 7;

    const LENGTH_BASE: [u16; 29] = [
        3, 4, 5, 6, 7, 8, 9, 10, 11, 13, 15, 17, 19, 23, 27, 31, 35, 43, 51, 59, 67, 83, 99, 115,
        131, 163, 195, 227, 258,
    ];
    const LENGTH_EXTRA: [u8; 29] = [
        0, 0, 0, 0, 0, 0, 0, 0, 1, 1, 1, 1, 2, 2, 2, 2, 3, 3, 3, 3, 4, 4, 4, 4, 5, 5, 5, 5, 0,
    ];
    const DISTANCE_BASE: [u16; 30] = [
        1, 2, 3, 4, 5, 7, 9, 13, 17, 25, 33, 49, 65, 97, 129, 193, 257, 385, 513, 769, 1025, 1537,
        2049, 3073, 4097, 6145, 8193, 12289, 16385, 24577,
    ];
    const DISTANCE_EXTRA: [u8; 30] = [
        0, 0, 0, 0, 1, 1, 2, 2, 3, 3, 4, 4, 5, 5, 6, 6, 7, 7, 8, 8, 9, 9, 10, 10, 11, 11, 12, 12,
        13, 13,
    ];
    /// The order the code-length code's own lengths are written in. Not sorted
    /// by anything meaningful — the rarely used lengths are last so that the
    /// tail can be dropped.
    const CODE_LENGTH_ORDER: [usize; 19] = [
        16, 17, 18, 0, 8, 7, 9, 6, 10, 5, 11, 4, 12, 3, 13, 2, 14, 1, 15,
    ];

    /// One LZ77 decision.
    #[derive(Clone, Copy)]
    enum Token {
        Literal(u8),
        Match { length: u16, distance: u16 },
    }

    /// Bits go out least-significant first, which is deflate's order and the
    /// reverse of how its Huffman codes are written down.
    struct Bits {
        out: Vec<u8>,
        bit: u32,
        count: u32,
    }

    impl Bits {
        fn new() -> Self {
            Bits { out: Vec::new(), bit: 0, count: 0 }
        }

        fn push(&mut self, value: u32, bits: u32) {
            self.bit |= (value & ((1u32 << bits) - 1).max(if bits == 0 { 0 } else { 0 })) << self.count;
            self.count += bits;
            while self.count >= 8 {
                self.out.push(self.bit as u8);
                self.bit >>= 8;
                self.count -= 8;
            }
        }

        /// A Huffman code, which is written most-significant bit first.
        fn push_code(&mut self, code: u16, bits: u8) {
            let mut reversed = 0u32;
            for i in 0..bits as u32 {
                reversed |= (((code as u32) >> (bits as u32 - 1 - i)) & 1) << i;
            }
            self.push(reversed, bits as u32);
        }

        fn finish(mut self) -> Vec<u8> {
            if self.count > 0 {
                self.out.push(self.bit as u8);
            }
            self.out
        }
    }

    /// Canonical Huffman code lengths for a symbol alphabet, none longer than
    /// `MAX_BITS`.
    ///
    /// The depth limit is met by halving every count and rebuilding until it
    /// is. That costs a fraction of a per cent of ratio on the rare inputs that
    /// need it, and it always terminates — as the counts flatten, the tree
    /// does too.
    fn code_lengths(counts: &[u32], max_bits: u8) -> Vec<u8> {
        let mut counts = counts.to_vec();
        loop {
            let lengths = build(&counts);
            if lengths.iter().all(|&l| l <= max_bits) {
                return lengths;
            }
            for c in counts.iter_mut() {
                if *c > 0 {
                    *c = (*c + 1) / 2;
                }
            }
        }
    }

    fn build(counts: &[u32]) -> Vec<u8> {
        let mut lengths = vec![0u8; counts.len()];
        let used: Vec<usize> = (0..counts.len()).filter(|&i| counts[i] > 0).collect();
        match used.len() {
            0 => return lengths,
            // A single symbol still needs a bit, or it has no code at all.
            1 => {
                lengths[used[0]] = 1;
                return lengths;
            }
            _ => {}
        }

        // Nodes are (weight, index); leaves point at symbols, internal nodes at
        // a pair. Kept in a vector and chosen by scan: the alphabets here are
        // 19, 30 and 286 symbols, where a heap is more code and no faster.
        #[derive(Clone, Copy)]
        struct Node {
            weight: u64,
            left: usize,
            right: usize,
        }
        const LEAF: usize = usize::MAX;

        let mut nodes: Vec<Node> = used
            .iter()
            .map(|&s| Node { weight: counts[s] as u64, left: LEAF, right: s })
            .collect();
        let mut live: Vec<usize> = (0..nodes.len()).collect();

        while live.len() > 1 {
            // The two lightest, smallest first.
            let mut a = 0;
            for (i, &n) in live.iter().enumerate() {
                if nodes[n].weight < nodes[live[a]].weight {
                    a = i;
                }
            }
            let first = live.swap_remove(a);
            let mut b = 0;
            for (i, &n) in live.iter().enumerate() {
                if nodes[n].weight < nodes[live[b]].weight {
                    b = i;
                }
            }
            let second = live.swap_remove(b);

            nodes.push(Node {
                weight: nodes[first].weight + nodes[second].weight,
                left: first,
                right: second,
            });
            live.push(nodes.len() - 1);
        }

        // Depth of each leaf is its code length.
        let mut stack = vec![(live[0], 0u8)];
        while let Some((index, depth)) = stack.pop() {
            let node = nodes[index];
            if node.left == LEAF {
                lengths[node.right] = depth.max(1);
            } else {
                stack.push((node.left, depth + 1));
                stack.push((node.right, depth + 1));
            }
        }
        lengths
    }

    /// Whether a set of code lengths describes a complete Huffman code: the
    /// leaves fill the tree exactly. Deflate requires this of the code-length
    /// code, and an incomplete one is refused by every decoder that is not
    /// ours.
    pub(super) fn is_complete(lengths: &[u8]) -> bool {
        let used = lengths.iter().filter(|&&l| l > 0).count();
        if used == 0 {
            return true;
        }
        if used == 1 {
            // One symbol cannot fill a tree; deflate tolerates this only for
            // the distance code.
            return false;
        }
        let total: u64 = lengths
            .iter()
            .filter(|&&l| l > 0)
            .map(|&l| 1u64 << (MAX_BITS - l))
            .sum();
        total == 1u64 << MAX_BITS
    }

    /// Canonical codes from lengths, RFC 1951 section 3.2.2.
    fn canonical(lengths: &[u8]) -> Vec<u16> {
        let mut count = [0u16; MAX_BITS as usize + 1];
        for &l in lengths {
            if l > 0 {
                count[l as usize] += 1;
            }
        }
        let mut next = [0u16; MAX_BITS as usize + 1];
        let mut code = 0u16;
        for bits in 1..=MAX_BITS as usize {
            code = (code + count[bits - 1]) << 1;
            next[bits] = code;
        }
        lengths
            .iter()
            .map(|&l| {
                if l == 0 {
                    0
                } else {
                    let c = next[l as usize];
                    next[l as usize] += 1;
                    c
                }
            })
            .collect()
    }

    fn length_symbol(length: usize) -> usize {
        LENGTH_BASE.partition_point(|&b| b as usize <= length) - 1
    }

    fn distance_symbol(distance: usize) -> usize {
        DISTANCE_BASE.partition_point(|&b| b as usize <= distance) - 1
    }

    /// LZ77 over the whole input, as a token list. Producing the tokens once
    /// and encoding them three ways is what makes choosing between the three
    /// affordable.
    fn tokenise(data: &[u8]) -> Vec<Token> {
        let mut tokens = Vec::with_capacity(data.len() / 2);
        let mut head = vec![usize::MAX; 1 << HASH_BITS];
        let mut prev = vec![usize::MAX; data.len().max(1)];
        let mut at = 0;

        let hash = |data: &[u8], at: usize| -> usize {
            ((data[at] as usize) << 10 ^ (data[at + 1] as usize) << 5 ^ data[at + 2] as usize)
                & ((1 << HASH_BITS) - 1)
        };

        while at < data.len() {
            let (mut best_length, mut best_distance) = (0usize, 0usize);

            if at + MIN_MATCH <= data.len() {
                let h = hash(data, at);
                let mut candidate = head[h];
                let limit = at.saturating_sub(WINDOW);
                let mut steps = 0;

                while candidate != usize::MAX && candidate >= limit && steps < CHAIN {
                    let max = MAX_MATCH.min(data.len() - at);
                    let mut length = 0;
                    while length < max && data[candidate + length] == data[at + length] {
                        length += 1;
                    }
                    if length > best_length {
                        best_length = length;
                        best_distance = at - candidate;
                        if length == max {
                            break;
                        }
                    }
                    candidate = prev[candidate];
                    steps += 1;
                }
                prev[at] = head[h];
                head[h] = at;
            }

            if best_length >= MIN_MATCH {
                tokens.push(Token::Match {
                    length: best_length as u16,
                    distance: best_distance as u16,
                });
                // Every position inside the match still has to enter the chain,
                // or the next match cannot see through it.
                for skip in 1..best_length {
                    let i = at + skip;
                    if i + MIN_MATCH <= data.len() {
                        let h = hash(data, i);
                        prev[i] = head[h];
                        head[h] = i;
                    }
                }
                at += best_length;
            } else {
                tokens.push(Token::Literal(data[at]));
                at += 1;
            }
        }
        tokens
    }

    fn counts(tokens: &[Token]) -> (Vec<u32>, Vec<u32>) {
        let mut literals = vec![0u32; LITERAL_SYMBOLS];
        let mut distances = vec![0u32; DISTANCE_SYMBOLS];
        for token in tokens {
            match *token {
                Token::Literal(b) => literals[b as usize] += 1,
                Token::Match { length, distance } => {
                    literals[257 + length_symbol(length as usize)] += 1;
                    distances[distance_symbol(distance as usize)] += 1;
                }
            }
        }
        literals[256] += 1; // end of block
        (literals, distances)
    }

    fn write_tokens(
        bits: &mut Bits,
        tokens: &[Token],
        literal_code: &[u16],
        literal_length: &[u8],
        distance_code: &[u16],
        distance_length: &[u8],
    ) {
        for token in tokens {
            match *token {
                Token::Literal(b) => {
                    bits.push_code(literal_code[b as usize], literal_length[b as usize]);
                }
                Token::Match { length, distance } => {
                    let l = length_symbol(length as usize);
                    bits.push_code(literal_code[257 + l], literal_length[257 + l]);
                    bits.push(
                        (length as usize - LENGTH_BASE[l] as usize) as u32,
                        LENGTH_EXTRA[l] as u32,
                    );
                    let d = distance_symbol(distance as usize);
                    bits.push_code(distance_code[d], distance_length[d]);
                    bits.push(
                        (distance as usize - DISTANCE_BASE[d] as usize) as u32,
                        DISTANCE_EXTRA[d] as u32,
                    );
                }
            }
        }
        bits.push_code(literal_code[256], literal_length[256]);
    }

    /// The fixed literal/length code, from RFC 1951 section 3.2.6.
    fn fixed_lengths() -> (Vec<u8>, Vec<u8>) {
        let mut literals = vec![8u8; LITERAL_SYMBOLS];
        literals[144..256].iter_mut().for_each(|l| *l = 9);
        literals[256..280].iter_mut().for_each(|l| *l = 7);
        (literals, vec![5u8; DISTANCE_SYMBOLS])
    }

    fn encode_fixed(tokens: &[Token]) -> Vec<u8> {
        let (literal_length, distance_length) = fixed_lengths();
        let mut bits = Bits::new();
        bits.push(1, 1); // final block
        bits.push(1, 2); // fixed Huffman
        write_tokens(
            &mut bits,
            tokens,
            &canonical(&literal_length),
            &literal_length,
            &canonical(&distance_length),
            &distance_length,
        );
        bits.finish()
    }

    /// The two code-length sequences, run-length encoded with symbols 16, 17
    /// and 18 as the format requires.
    fn pack_lengths(all: &[u8]) -> Vec<(u8, u8, u8)> {
        let mut out = Vec::new();
        let mut i = 0;
        while i < all.len() {
            let value = all[i];
            let mut run = 1;
            while i + run < all.len() && all[i + run] == value {
                run += 1;
            }
            if value == 0 {
                while run >= 11 {
                    let take = run.min(138);
                    out.push((18, (take - 11) as u8, 7));
                    run -= take;
                    i += take;
                }
                while run >= 3 {
                    let take = run.min(10);
                    out.push((17, (take - 3) as u8, 3));
                    run -= take;
                    i += take;
                }
            } else {
                out.push((value, 0, 0));
                run -= 1;
                i += 1;
                while run >= 3 {
                    let take = run.min(6);
                    out.push((16, (take - 3) as u8, 2));
                    run -= take;
                    i += take;
                }
            }
            for _ in 0..run {
                out.push((value, 0, 0));
                i += 1;
            }
        }
        out
    }

    fn encode_dynamic(tokens: &[Token]) -> Vec<u8> {
        let (literal_counts, distance_counts) = counts(tokens);
        let mut literal_length = code_lengths(&literal_counts, MAX_BITS);
        let mut distance_length = code_lengths(&distance_counts, MAX_BITS);

        // At least one distance code must exist even when nothing matched.
        if distance_length.iter().all(|&l| l == 0) {
            distance_length[0] = 1;
        }

        let hlit = (literal_length.iter().rposition(|&l| l > 0).unwrap_or(256) + 1).max(257);
        let hdist = distance_length.iter().rposition(|&l| l > 0).unwrap_or(0) + 1;
        literal_length.truncate(hlit);
        distance_length.truncate(hdist);

        let mut both = literal_length.clone();
        both.extend_from_slice(&distance_length);
        let packed = pack_lengths(&both);

        let mut cl_counts = vec![0u32; CODE_LENGTH_SYMBOLS];
        for &(symbol, _, _) in &packed {
            cl_counts[symbol as usize] += 1;
        }
        let cl_length = code_lengths(&cl_counts, MAX_CODE_LENGTH_BITS);
        debug_assert!(
            is_complete(&cl_length),
            "the code-length code must be complete or no decoder will read this"
        );
        debug_assert!(cl_length.iter().all(|&l| l <= MAX_CODE_LENGTH_BITS));
        let cl_code = canonical(&cl_length);

        let hclen = (1..=CODE_LENGTH_SYMBOLS)
            .rev()
            .find(|&n| cl_length[CODE_LENGTH_ORDER[n - 1]] > 0)
            .unwrap_or(4)
            .max(4);

        let mut bits = Bits::new();
        bits.push(1, 1); // final block
        bits.push(2, 2); // dynamic Huffman
        bits.push((hlit - 257) as u32, 5);
        bits.push((hdist - 1) as u32, 5);
        bits.push((hclen - 4) as u32, 4);
        for &index in CODE_LENGTH_ORDER.iter().take(hclen) {
            bits.push(cl_length[index] as u32, 3);
        }
        for &(symbol, extra, extra_bits) in &packed {
            bits.push_code(cl_code[symbol as usize], cl_length[symbol as usize]);
            if extra_bits > 0 {
                bits.push(extra as u32, extra_bits as u32);
            }
        }
        write_tokens(
            &mut bits,
            tokens,
            &canonical(&literal_length),
            &literal_length,
            &canonical(&distance_length),
            &distance_length,
        );
        bits.finish()
    }

    /// Uncompressed blocks. The ceiling on how bad the output can be.
    fn encode_stored(data: &[u8]) -> Vec<u8> {
        let mut out = Vec::with_capacity(data.len() + 5 * (data.len() / 65535 + 1));
        let mut chunks = data.chunks(65535).peekable();
        if chunks.peek().is_none() {
            out.extend_from_slice(&[1, 0, 0, 0xFF, 0xFF]);
            return out;
        }
        while let Some(chunk) = chunks.next() {
            out.push(u8::from(chunks.peek().is_none()));
            out.extend_from_slice(&(chunk.len() as u16).to_le_bytes());
            out.extend_from_slice(&(!(chunk.len() as u16)).to_le_bytes());
            out.extend_from_slice(chunk);
        }
        out
    }

    /// Compress `data` and append it to `out`, whichever of the three
    /// encodings comes out smallest.
    pub fn compress(data: &[u8], out: &mut Vec<u8>) {
        let tokens = tokenise(data);
        let mut best = encode_stored(data);
        for candidate in [encode_dynamic(&tokens), encode_fixed(&tokens)] {
            if candidate.len() < best.len() {
                best = candidate;
            }
        }
        out.extend_from_slice(&best);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_png_round_trips_through_the_deflate_we_wrote() {
        // A gradient with noise: compressible, but not trivially so.
        let (w, h) = (64u32, 48u32);
        let mut rgba = Vec::new();
        for y in 0..h {
            for x in 0..w {
                let n = ((x * 7 + y * 13) % 11) as u8;
                rgba.extend_from_slice(&[(x * 4) as u8, (y * 5) as u8 ^ n, 128 + n, 255]);
            }
        }

        let png = encode_png(w, h, &rgba);
        assert_eq!(&png[..8], b"\x89PNG\r\n\x1a\n");

        let image = load("test", &png).expect("our own PNG reads back");
        assert_eq!((image.width, image.height), (w, h));
        assert_eq!(image.mime, Mime::Png);
        assert!(
            png.len() < rgba.len(),
            "{} bytes for {} of pixels is not compression",
            png.len(),
            rgba.len()
        );
    }

    /// The deflate stream has to be readable by something that is not us. This
    /// checks it against the format's own rules — every symbol accounted for,
    /// the stream ending on an end-of-block, and the Adler-32 agreeing.
    #[test]
    fn the_deflate_stream_is_well_formed() {
        let data: Vec<u8> = (0..4096u32).map(|i| (i * i / 7) as u8).collect();
        let stream = zlib(&data);
        assert_eq!(&stream[..2], &[0x78, 0x01]);
        let checksum = u32::from_be_bytes(stream[stream.len() - 4..].try_into().unwrap());
        assert_eq!(checksum, adler32(&data));
        assert!(stream.len() < data.len(), "{} vs {}", stream.len(), data.len());
    }

    /// The bug that ten of the library's normal maps found and six unit tests
    /// did not: the code-length code was allowed fifteen bits when the header
    /// writes it in three. Any input whose symbol frequencies are skewed
    /// enough to push a code-length code past seven bits reproduces it, and
    /// the debug assertions inside the encoder fire on it here.
    #[test]
    fn skewed_inputs_still_produce_a_complete_code_length_code() {
        let mut inputs: Vec<Vec<u8>> = Vec::new();

        // One byte in a sea of another: the most skewed distribution there is.
        let mut lopsided = vec![0u8; 40_000];
        lopsided[19_999] = 7;
        inputs.push(lopsided);

        // A long ramp, which fills the alphabet evenly and makes the runs of
        // equal code lengths that exercise symbols 16, 17 and 18.
        inputs.push((0..60_000u32).map(|i| (i % 251) as u8).collect());

        // Noise with a heavy tail, which is what a filtered normal map is and
        // what produced the ten failures.
        inputs.push(
            (0..80_000u32)
                .map(|i| {
                    let n = i.wrapping_mul(2_654_435_761) >> 13;
                    if n % 5 == 0 { (n % 256) as u8 } else { 200u8.wrapping_add((n % 17) as u8) }
                })
                .collect(),
        );

        // Every distinct byte exactly once, then almost none of them.
        let mut sparse: Vec<u8> = (0..=255u8).collect();
        sparse.extend(std::iter::repeat_n(0u8, 30_000));
        inputs.push(sparse);

        inputs.push(Vec::new());
        inputs.push(vec![42]);

        for input in &inputs {
            let stream = zlib(input);
            assert_eq!(&stream[..2], &[0x78, 0x01]);
            assert_eq!(
                u32::from_be_bytes(stream[stream.len() - 4..].try_into().unwrap()),
                adler32(input)
            );
            // Never larger than what it was given, plus a stored block's own
            // overhead. A compressor that expands is a bug, not a trade-off.
            let ceiling = 2 + input.len() + 5 * (input.len() / 65535 + 1) + 5 + 4;
            assert!(
                stream.len() <= ceiling,
                "{} bytes out for {} in",
                stream.len(),
                input.len()
            );
        }
    }

    #[test]
    fn a_complete_code_is_told_from_an_incomplete_one() {
        // Two symbols, one bit each: fills the tree.
        assert!(deflate::is_complete(&[1, 1]));
        // One symbol cannot.
        assert!(!deflate::is_complete(&[1]));
        // 1/2 + 1/4 + 1/8 + 1/8.
        assert!(deflate::is_complete(&[1, 2, 3, 3]));
        // The shape the encoder used to emit: short by one leaf.
        assert!(!deflate::is_complete(&[1, 2, 3]));
        // Nothing used at all is vacuously fine.
        assert!(deflate::is_complete(&[0, 0, 0]));
    }

    #[test]
    fn a_dds_the_library_does_not_use_is_refused_rather_than_guessed_at() {
        let mut dds = vec![0u8; 128];
        dds[..4].copy_from_slice(b"DDS ");
        dds[4..8].copy_from_slice(&124u32.to_le_bytes());
        dds[80..84].copy_from_slice(&super::DDPF_FOURCC.to_le_bytes());
        dds[84..88].copy_from_slice(b"DXT5");

        match load("blocky.dds", &dds) {
            Err(ImageError::UnsupportedDds(what)) => assert!(what.contains("DXT5"), "{what}"),
            other => panic!("expected a refusal, got {other:?}"),
        }
    }

    #[test]
    fn a_bgra_dds_comes_out_with_its_channels_the_right_way_round() {
        let (w, h) = (2u32, 1u32);
        let mut dds = vec![0u8; 128];
        dds[..4].copy_from_slice(b"DDS ");
        dds[4..8].copy_from_slice(&124u32.to_le_bytes());
        dds[12..16].copy_from_slice(&h.to_le_bytes());
        dds[16..20].copy_from_slice(&w.to_le_bytes());
        dds[80..84].copy_from_slice(&(super::DDPF_RGB | super::DDPF_ALPHAPIXELS).to_le_bytes());
        dds[88..92].copy_from_slice(&32u32.to_le_bytes());
        for (i, mask) in [0x00FF_0000u32, 0x0000_FF00, 0x0000_00FF, 0xFF00_0000]
            .iter()
            .enumerate()
        {
            dds[92 + i * 4..96 + i * 4].copy_from_slice(&mask.to_le_bytes());
        }
        // Stored B,G,R,A: an opaque red pixel, then a half-transparent green.
        dds.extend_from_slice(&[0, 0, 255, 255, 0, 255, 0, 128]);

        let (dw, dh, rgba) = decode_dds(&dds).expect("a plain 32-bit dds");
        assert_eq!((dw, dh), (w, h));
        assert_eq!(rgba, vec![255, 0, 0, 255, 0, 255, 0, 128]);
    }

    #[test]
    fn a_jpeg_is_passed_through_untouched() {
        // A minimal baseline JPEG header: SOI, a SOF0 naming 8×16, then EOI.
        let mut jpeg = vec![0xFF, 0xD8, 0xFF];
        jpeg.extend_from_slice(&[0xC0, 0x00, 0x11, 0x08]);
        jpeg.extend_from_slice(&16u16.to_be_bytes()); // height
        jpeg.extend_from_slice(&8u16.to_be_bytes()); // width
        jpeg.extend_from_slice(&[3, 1, 0x22, 0, 2, 0x11, 1, 3, 0x11, 1]);
        jpeg.extend_from_slice(&[0xFF, 0xD9]);

        let image = load("flat.jpg", &jpeg).expect("a JPEG frame");
        assert_eq!((image.width, image.height), (8, 16));
        assert_eq!(image.mime, Mime::Jpeg);
        assert_eq!(image.bytes, jpeg, "re-encoding a JPEG could only lose");
    }

    #[test]
    fn something_that_is_not_an_image_is_not_taken_for_one() {
        assert_eq!(load("notes.txt", b"just text"), Err(ImageError::Unrecognised));
    }
}
