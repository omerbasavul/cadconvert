# Conventions

How this repository is laid out and what the code holds itself to. The
engineering record — every defect found, what it measured before and after, and
what was tried and reverted — is in [`parasolid-xt-notes.md`](parasolid-xt-notes.md).

## Layout

The .NET solution is the container: one `dotnet build` produces everything,
including the Rust side.

```
CadConvert.slnx    the solution
src/CadConvert/    the NuGet package
tests/             the .NET test suite: the ABI crossing, not the geometry
bench/             the benchmark: time and peak resident memory, per platform
build/             Native.props (where the library is) and Native.targets (how
                   to build it)
native/            the Rust workspace — Cargo.toml and crates/
docs/              this, the engineering record, the XT format reference
runtimes/, out/    build output; nothing here is source
```

## The Rust crates

Each holds one vocabulary and depends only on `cad-ir`. A new input format
costs a crate on the left; a new output format costs one on the right.

```
xt-parser    Parasolid XT text → raw entities            no dependencies
cad-ir       B-Rep, scene, materials, .sldmat, .p2m      no dependencies
cad-xt       XT → cad-ir
cad-step     ISO 10303-21 → cad-ir
cad-gltf     glTF 2.0 → cad-ir, already meshed; the tessellator passes it by
cad-tess     cad-ir → watertight triangles
cad-export   cad-ir → GLB / OBJ / USDZ
cad-convert  read → tessellate → write, in one call
cad-cli      cadconvert
cad-ffi      libcadconvert_native, a C ABI over cad-convert
```

## What the code holds itself to

- **Rust edition 2024**, 1.85 or later.
- **Fail fast on unknowns.** An unknown entity type or field code is a parse
  error, never a silent skip. A reader that guesses produces a mesh nobody can
  trust.
- **No placeholders.** Implement it fully or not at all.
- **`#![forbid(unsafe_code)]`** in every crate except `cad-ffi`, where each
  `unsafe` block states what makes it sound.
- **Zero compiler warnings**, examples included — that is where the measuring
  tools live. CI builds with `-D warnings`.
- **Measure, then keep or revert.** Every change to geometry is measured
  against OpenCASCADE's reading of the same model, both directions, and
  anything that does not improve the numbers is reverted and the measurement
  recorded. The record is full of reverted attempts; they are as useful as the
  kept ones.
- **Never coarsen the mesh to make a number look better.** The tessellation
  tolerances are measured, not chosen, and a wrapper must not invent its own.

## Working on it

```sh
dotnet build CadConvert.slnx -c Release    # builds the Rust side too
dotnet test  CadConvert.slnx -c Release
```

Testing the **package** by hand needs one more step, and skipping it is
misleading rather than merely useless:

```sh
rm -rf ~/.nuget/packages/cadconvert          # or the cached one is what loads
dotnet pack src/CadConvert -c Release -o dist
```

NuGet caches by identifier and version. Repacking 0.1.0 twenty times and
restoring it into an application gives whichever copy landed in the cache
first, however long ago — which is how a converter that had gained USDZ came
out writing glTF into a file called `.usdz` on this machine while CI, whose
runners have no cache, was right about it all along.

The Rust side on its own, from `native/`:

```sh
cargo test --release --workspace
cargo run --release -p cad-cli -- part.x_t out.glb
cargo run --release -p xt-parser --example parse_xt -- file.x_t   # topology
cargo run --release -p xt-parser --example dump_types -- file.x_t # entity types
cargo run --release -p cad-convert --example scene_bytes -- file  # where memory goes
cargo run --release -p cad-convert --example read_memory -- file  # per phase
```

The environment variables every diagnostic is behind — `CAD_TESS_*`, `XT_*` —
are listed where they are used, and named in the engineering record beside the
defect each was written to find.
