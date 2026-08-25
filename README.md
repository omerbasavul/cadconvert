# CadConvert

Turn a CAD file into a mesh: **Parasolid** (`.x_t`) and **STEP** (`.stp`) in,
**glTF 2.0** or **USDZ** out — the B-Rep read, tessellated watertight, with the
materials the designer chose. No CAD kernel to install and no licence server;
one native library of about 2.5 MB. A **glTF** (`.glb`) goes in too: already a
mesh, it is written as it is, which is how a part held as GLB gets its USDZ.

```csharp
var result = CadConvert.CadConverter.Convert("housing.x_t", "housing.glb");
Console.WriteLine($"{result.Bodies} bodies, {result.Triangles:N0} triangles");

// Both containers from one reading, told where the work is, cancellable.
CadConvert.CadConverter.ConvertMany("housing.x_t", new[] { "housing.glb", "housing.usdz" },
    progress: p => Console.Error.WriteLine(p), cancellationToken: token);
```

```sh
cadconvert housing.x_t                       # → housing.glb
cadconvert assembly.stp out.glb -q compact
cadconvert assembly.stp out.usdz             # the extension picks the format
cadconvert assembly.stp out.glb out.usdz     # both, from one reading
cadconvert held.glb held.usdz --progress     # a GLB onward, saying where it is
```

```c
cadconvert_options o; cadconvert_default_options(&o);
cadconvert_summary s; char *message = NULL;
if (cadconvert_convert("housing.x_t", "housing.glb", &o, &s, &message) != CADCONVERT_OK)
    fprintf(stderr, "%s\n", message);
cadconvert_string_free(message);
```

## Is it any good

The test model is a 46-body truck air compressor, delivered as a STEP assembly
and a Parasolid twin of the same design. Both are read, and both are measured
against **OpenCASCADE**'s reading of the STEP at matching density — its
vertices to our surface, which is the direction that catches a surface we got
wrong:

|                                    |     STEP | Parasolid |
|------------------------------------|---------:|----------:|
| triangles                          | 1 970 318 | 2 043 886 |
| faces meshed                       | 11 214 / 11 214 | 11 214 / 11 214 |
| open edges                         |        0 |         0 |
| non-manifold edges                 |        0 |        10 |
| mean deviation                     | 0.0047 mm | 0.0076 mm |
| 99th percentile                    | 0.0582 mm | 0.0991 mm |
| over 0.2 mm, of 445 576 points     |      301 |     1 646 |

And the two readers agree with **each other** to **2.4 µm** everywhere — two
independent readers, of two unrelated file formats, producing the same solid.

Where they do not agree with OpenCASCADE, the difference is named in
[`docs/parasolid-xt-notes.md`](docs/parasolid-xt-notes.md), which records every
defect found, what it measured before and after, and what was tried and
reverted.

## Quality

The mesh is held to **0.04 % of the model's own diagonal** and **8°** between
adjacent facet normals. Both are needed and both were measured: a distance
alone is satisfied by almost no subdivision on a small radius, which is how a
1 mm hole becomes a triangle.

| `-q` | what it does | size |
|---|---|---|
| `plain` | positions and normals exactly as computed | 100 % |
| `lean` *(default)* | normals a byte a component; **no vertex moves** | 77 % |
| `compact` | positions on each mesh's own 16-bit grid as well | 65 % |

`compact` is for delivery: it also collapses the mesh's finest slivers, so stay
on `lean` if the mesh will be worked on further.

## Materials

Neither format names a material. What is recovered, in order:

1. **the file's own per-face colour** — verified body by body against
   OpenCASCADE, every one matching;
2. **the designer's per-face reflectivity**, from the Parasolid file — the one
   statement of metal-versus-matte in either format that is not a guess. A STEP
   file's `.x_t` twin is read for it when one sits beside it, or
   `--materials` supplies the same fourteen facts without it;
3. **the finish**, from SolidWorks' own libraries, both carried in the binary:
   115 materials from a `.sldmat` and 619 appearances from `.p2m` files,
   including the powder coat a machine casting is actually delivered in.

An appearance also names images, and those are read now: 229 of the 619 name a
colour image and 139 a tangent-space normal map. The powder coat brings both,
tiled every **6.35 mm** — its own `initTextureWidth`, which is why texture
coordinates are a projection at world scale rather than anything derived from
surface parameters.

The colour image is not a colour. `powdercoat_dark.jpg` has a linear mean of
0.1830 and the appearance states `col1` 0.1843 — the same number, because the
image is the appearance's own colour times a grain whose mean is one.
Multiplying the part's colour by it as it stands applies that level twice and
costs a fifth of the brightness; dividing by `col1` leaves the grain and keeps
the colour the file gave.

## USDZ

The same scene, for the tools that read USD. `UsdPreviewSurface` and glTF's
metallic-roughness model are one model with two spellings, so the colour, the
metal, the roughness, the index of refraction, the grain and the normal map all
survive the crossing. Every package is checked against Apple's own
`usdchecker`, including `--arkit`.

The package carries USD's **binary** encoding. That is most of what a USDZ
weighs, because a USDZ may not compress anything and USD's text form spells
every coordinate out:

| the pilot assembly, 1 970 388 triangles | |
|---|---:|
| glTF binary | 40 MB |
| USD, text | 172 MB |
| USD, binary — what this writes | 50 MB |
| USD, binary — what `usdcat` makes of our text | 43 MB |

The crate format was learned by taking apart files USD wrote rather than from a
specification; `tools/usdc_decode.py` is what did the taking apart and is kept
because it is also how a file written here is checked against one that was not.
The remaining 15% against `usdcat` is two things given up on purpose: matches
in the LZ4, which would buy back a fraction of a per cent of a file that is
mostly incompressible coordinates, and instancing, which needs a composition
arc whose encoding has not been read off a file yet — it costs under eight per
cent, since the parts the pilot repeats are the small ones.

`--usd-text` writes the text form instead, which is worth having when a reader
and a writer disagree about what is in a file and you want to open it and look.

## glTF in

`.glb` and `.gltf` are read by `cad-gltf` into the same scene the CAD readers
produce, already meshed: the node tree with its transforms, every triangle
primitive (strips and fans included), the metallic-roughness materials with
their colour and normal maps, and the extensions this converter's own writer
emits — `KHR_mesh_quantization`, `KHR_texture_transform`, `KHR_materials_ior`,
`KHR_materials_transmission`. Metres become millimetres and Y-up becomes Z-up
on the way in, baked into the vertices and conjugated into every node, so the
mesh a writer sees is in the space every other reader produces and the
tolerances downstream mean what they say. A GLB this converter wrote reads back
to the same triangles and, to float precision, the same millimetres.

What the scene has no word for is named in the warnings rather than dropped in
silence: an occlusion or metallic-roughness map, a line primitive, a second
texture set, an animation. What the reader does not implement and the file
*requires* is refused by name — Draco and meshopt compression, KTX2 textures —
with the instruction to export without it.

## Several outputs, and where the work is

A part wanted as both glTF and USDZ is read and meshed once:
`cadconvert_convert_many` in C, `CadConverter.ConvertMany(input, outputs, …)` in
.NET, and simply more than one output on the command line. Reading a
30 MB Parasolid is most of the wall clock, and it is not paid twice.

Between units of work — each stage as it opens, each body as it is meshed,
each file as it is written — the converter reports where it is, on the calling
thread: `done` of `total` and the unit about to start. A caller shows "meshing
body 3 of 46: bracket" without keeping state of its own, and can answer *stop*,
which returns `CADCONVERT_ERR_CANCELLED` (an `OperationCanceledException` in
.NET) at the next unit. It is between bodies rather than between faces because
a body's faces run in parallel, and a callback from inside that would arrive
on a worker thread — which a caller across a foreign ABI cannot be handed.

## Robustness and cost

Twenty-two mutations of the test model — truncated anywhere from 0.1 % to
99.9 %, bytes flipped, empty, arbitrary noise — put through the whole pipeline:
**none crashed.** Fourteen were refused with a reason, eight produced a file
from what could be recovered, and even the one built from corrupted bytes came
out watertight. Four threads converting at once agree to the triangle.

That is 245 Rust tests over the readers and the tessellator, and 28 .NET tests
over the crossing itself — that the library loads from where the build put it,
that a non-ASCII path survives the marshalling, that making the file smaller
never costs a triangle, and that a malformed part returns an error code rather
than unwinding into the host. CI runs the Rust tests on Linux, macOS and
Windows, and the .NET tests on each of the five runtimes — unwinding across
the ABI is exactly where platforms differ.

On an M-series laptop, at the default quality:

| | STEP, 32.7 MB | Parasolid, 36.3 MB |
|---|---:|---:|
| wall clock | 11 s | 45 s |
| peak resident, first conversion | ~400 MB | ~470 MB |
| in a long-running process | levels off at 598 MB | 709 MB |

Peak memory plateaus at the fourth conversion and does not move again — it is
the allocator's high-water mark, not retention.

## Building

```sh
dotnet build CadConvert.slnx -c Release    # builds the Rust side too
dotnet pack src/CadConvert -c Release -o dist
```

`dotnet build` runs `cargo build` and stages the native library under
`runtimes/{rid}/native`, which is where the package and the .NET host both look
for it. A second build takes about a second — incrementality is cargo's.

The Rust side on its own lives in [`native/`](native); layout and conventions
are in [`docs/CONVENTIONS.md`](docs/CONVENTIONS.md).

## Platforms

`linux-x64`, `linux-arm64`, `osx-arm64`, `osx-x64`, `win-x64`. Each runtime is
built on its own machine by CI and collected into one package; a release refuses
to pack if any is missing, because a package without a runtime for the machine
it lands on fails at the first call with no build error anywhere.

## Licence

MIT.
