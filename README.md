# CadConvert

Turn a CAD file into a mesh: **Parasolid** (`.x_t`) and **STEP** (`.stp`) in,
**glTF 2.0** out — the B-Rep read, tessellated watertight, with the materials
the designer chose. No CAD kernel to install and no licence server; one native
library of about 2.5 MB.

```csharp
var result = CadConvert.CadConverter.Convert("housing.x_t", "housing.glb");
Console.WriteLine($"{result.Bodies} bodies, {result.Triangles:N0} triangles");
```

```sh
cadconvert housing.x_t                       # → housing.glb
cadconvert assembly.stp out.glb -q compact
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
