# Where the bundled libraries came from

## `solidworks-materials.sldmat`

The SolidWorks default material library, 115 materials in 12 classifications,
with the optical coefficients and shader names each was authored with.

Source: <https://ww3.cad.de/foren/ubb/uploads/Taiko/solidworksmaterials_adjusted.sldmat.txt>

Verified on 2026-08-22 by downloading it again and comparing: **the decoded
text is identical** — same 115 materials, same 12 classifications, 115 optical
blocks, 47 distinct `pwshader` names, 17 texture references. The two files
differ only in encoding, and their checksums differ only for that reason:

| | bytes | encoding | sha256 |
|---|---:|---|---|
| as downloaded | 275 456 | UTF-16 LE with a BOM | `afdce2dd…7a2019` |
| as bundled | 137 565 | UTF-8 | `919300b2…7013b0` |

`sldmat::SldLibrary::parse` reads the encoding from the byte-order mark rather
than from the XML declaration, because these files are inconsistent about it —
most are UTF-16 declaring UTF-16, some are UTF-8 still declaring UTF-16. Either
form of this file parses to the same 115 materials.

The attribute quoting is inconsistent too: 35 of the entries write their
`<sldcolorswatch:Optical>` attributes with double quotes and the other 80 with
single quotes. A reader that handles only one silently gives 80 materials all
zeros — which reads as fully matte, unlit and opaque. Ours handles both; a
Python check written during the survey did not, and reported that 80 materials
had no optics at all.

## `Materials/` — 619 `.p2m` appearance files

SolidWorks' appearance library: what a surface *looks* like, as against what it
is made of. This is where paint lives — a `.sldmat` has no entry for it, because
paint is an appearance in SolidWorks and not a material.

Only the `.p2m` files are kept and compiled in (332 KB of text, via
`build.rs`). The images they name — 935 MB of textures, HDR environments and
thumbnails — are excluded by `.gitignore`: a `.p2m` names them by a path inside
a SolidWorks installation, and nothing here resolves it.

See `native/crates/cad-ir/src/p2m.rs` for how a `.p2m`'s `roughness` is read,
which is not glTF roughness and gets clear glass wrong if taken for it.
