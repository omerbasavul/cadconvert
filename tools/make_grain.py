#!/usr/bin/env python3
"""Separate an appearance's grain from its colour, once, offline.

A SolidWorks colour image is the appearance's own colour times a grain whose
mean is one: `powdercoat_dark.jpg` has a linear mean of 0.1830 and the
appearance states col1 0.1843. Multiplying a part's own colour by that image
applies the level twice, and glTF's base colour factor stops at one, so any
part brighter than the appearance cannot be compensated for — the pilot's
dominant paint needs 1.12 and its blue needs 2.44.

So the level comes out of the image here. What is written is the grain alone,
scaled so its mean is as near one as clipping allows, which the converter then
multiplies by the colour the CAD file states. The result is our own derived
data rather than SolidWorks' image, which is the better thing to be shipping
in any case.

    python3 tools/make_grain.py <in.jpg> <out.png>

Needs `sips` (macOS) to decode the JPEG, and the crate's own PNG writer to
emit the result — the converter has neither a JPEG decoder nor a reason for
one, since it passes JPEGs through untouched.
"""
import pathlib, struct, subprocess, sys, tempfile, zlib


def read_png(path):
    b = pathlib.Path(path).read_bytes()
    pos, idat = 8, b""
    w = h = colour = 0
    while pos < len(b):
        ln = struct.unpack_from(">I", b, pos)[0]
        typ, data = b[pos + 4:pos + 8], b[pos + 8:pos + 8 + ln]
        if typ == b"IHDR":
            w, h, _, colour, _, _, _ = struct.unpack(">IIBBBBB", data)
        if typ == b"IDAT":
            idat += data
        pos += 12 + ln
    raw = zlib.decompress(idat)
    ch = {0: 1, 2: 3, 4: 2, 6: 4}[colour]
    stride, prev, out, i = w * ch, bytearray(w * ch), [], 0
    for _ in range(h):
        f = raw[i]; i += 1
        line = bytearray(raw[i:i + stride]); i += stride
        for x in range(stride):
            a = line[x - ch] if x >= ch else 0
            b_ = prev[x]
            c = prev[x - ch] if x >= ch else 0
            if f == 1: line[x] = (line[x] + a) & 255
            elif f == 2: line[x] = (line[x] + b_) & 255
            elif f == 3: line[x] = (line[x] + (a + b_) // 2) & 255
            elif f == 4:
                p = a + b_ - c
                pa, pb, pc = abs(p - a), abs(p - b_), abs(p - c)
                line[x] = (line[x] + (a if (pa <= pb and pa <= pc) else (b_ if pb <= pc else c))) & 255
        prev = line
        for x in range(w):
            px = line[x * ch:x * ch + ch]
            out.append(tuple(px[:3]) if ch >= 3 else (px[0],) * 3)
    return w, h, out


def to_linear(c):
    c /= 255.0
    return c / 12.92 if c <= 0.04045 else ((c + 0.055) / 1.055) ** 2.4


def to_srgb(c):
    c = max(0.0, min(1.0, c))
    return round((12.92 * c if c <= 0.0031308 else 1.055 * c ** (1 / 2.4) - 0.055) * 255)


def main() -> int:
    if len(sys.argv) < 3:
        print(__doc__)
        return 2
    src, dst = sys.argv[1], str(pathlib.Path(sys.argv[2]).resolve())

    with tempfile.TemporaryDirectory() as tmp:
        png = f"{tmp}/decoded.png"
        subprocess.run(["sips", "-s", "format", "png", src, "--out", png],
                       check=True, capture_output=True)
        w, h, pixels = read_png(png)

    linear = [[to_linear(c) for c in p] for p in pixels]
    mean = [sum(p[k] for p in linear) / len(linear) for k in range(3)]
    print(f"  {w}x{h}, linear mean {mean[0]:.4f} {mean[1]:.4f} {mean[2]:.4f}")
    # By the image's own mean, not by the appearance's col1. The two agree to
    # 0.7% — 0.1830 against 0.1843 — but col1 is faintly blue and dividing by
    # it tints the grain, which then tints whatever colour the part carries.
    # A grain should be neutral; the level is what is being removed.
    level = sum(mean) / 3
    print(f"  dividing by its own mean, {level:.4f}")

    grain = [[p[k] / level for k in range(3)] for p in linear]
    clipped_mean = [sum(min(1.0, p[k]) for p in grain) / len(grain) for k in range(3)]
    print(f"  grain after clipping, mean {clipped_mean[0]:.4f} {clipped_mean[1]:.4f} "
          f"{clipped_mean[2]:.4f}   ({100 * (1 - clipped_mean[0]):.1f}% darker than one)")

    rgba = bytearray()
    for p in grain:
        for k in range(3):
            rgba.append(to_srgb(p[k]))
        rgba.append(255)

    # The crate's own PNG writer, so what ships is what it would have written.
    with tempfile.TemporaryDirectory() as tmp:
        raw = f"{tmp}/rgba.bin"
        pathlib.Path(raw).write_bytes(bytes(rgba))
        subprocess.run(
            ["cargo", "run", "--release", "-q", "-p", "cad-ir", "--example", "encode_png",
             "--", raw, str(w), str(h), dst],
            cwd="native", check=True)
    print(f"  wrote {dst}, {pathlib.Path(dst).stat().st_size} bytes")

    # The level the converter has to divide by. Clipping means the grain's own
    # mean is under one, and a part multiplied by it would come out that much
    # darker; recording the number here is what lets the converter put it back.
    out = pathlib.Path(dst)
    manifest = out.parent / "grains.txt"
    key = pathlib.Path(src).name.lower()
    lines = [l for l in manifest.read_text().splitlines() if not l.startswith(key + "\t")] \
        if manifest.exists() else [
            "# Colour images with their level divided out, by tools/make_grain.py.",
            "# <original file>\t<grain>\t<mean after clipping>",
        ]
    lines.append(f"{key}\t{out.name}\t{clipped_mean[0]:.6f}")
    manifest.write_text("\n".join(lines) + "\n")
    print(f"  recorded in {manifest}")
    return 0


if __name__ == "__main__":
    sys.exit(main())
