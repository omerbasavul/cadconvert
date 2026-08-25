# CadConvert

Parasolid (`.x_t`), STEP (`.stp`) or glTF (`.glb`) to **glTF 2.0** or
**USDZ**. Reads the B-Rep, tessellates it watertight, and writes a mesh with
materials — no CAD kernel to install. A glTF input is already a mesh and is
written as it is, which is how a part held as GLB gets its USDZ.

```csharp
using CadConvert;

var result = CadConverter.Convert("housing.x_t", "housing.glb");
// The output's extension chooses the container: .glb or .usdz.

Console.WriteLine($"{result.Bodies} bodies, {result.Triangles:N0} triangles");
foreach (var warning in result.Warnings)
    Console.WriteLine($"warning: {warning}");
```

## Several outputs from one reading

A part wanted as both is read and meshed once:

```csharp
var result = CadConverter.ConvertMany(
    "housing.x_t",
    new[] { "housing.glb", "housing.usdz" },
    progress: p => Console.Error.WriteLine(p),   // "Mesh 3/46: bracket"
    cancellationToken: token);

foreach (var o in result.Outputs)
    Console.WriteLine($"{o.Path}: {o.Bytes:N0} bytes");
```

`progress` is called on the calling thread between units of work — every
stage as it opens, each body as it is meshed, each file as it is written —
with `Done` of `Total` and the unit about to start in `Detail`. A cancelled
token stops the work at the next unit with `OperationCanceledException`;
outputs already written stay on disk. An exception thrown by the handler stops
it the same way and comes back as itself.

## Defaults

The mesh is held to **0.04 % of the model's own diagonal** and **8°** between
adjacent facet normals. Both are needed: a distance alone is satisfied by almost
no subdivision on a small radius, which is how a 1 mm hole becomes a triangle.
`SagMillimetres` and `AngleDegrees` are `null` by default and mean "the
converter's own"; setting a distance also turns the relative reading off.

`MeshTarget.Lean`, the default, encodes normals a byte a component and leaves
every vertex exactly where it was computed. `Compact` also puts positions on
each mesh's own 16-bit grid — about a quarter smaller again, and it collapses
the mesh's finest slivers, so it is for delivery rather than for modelling with.
`Plain` compresses nothing.

## glTF in

`.glb` and `.gltf` are read as they are: the node tree, every triangle
primitive, the metallic-roughness materials with their colour and normal maps.
What the scene has no word for — an occlusion map, a line primitive, an
animation — is named in `Warnings` rather than dropped in silence. A file that
*requires* something the reader does not implement (Draco or meshopt
compression, KTX2 textures) is refused by that thing's name: export it without
compression. `Faces` is zero for a glTF input, which has triangles and no faces.

## Warnings are not failures

A call that returns normally produced a file. `Warnings` lists what the readers
or the tessellator could not do — faces that produced no triangles, bodies that
were skipped. It is usually empty, and when it is not, ignoring it is how a hole
ships.

## Threads and robustness

Conversions are independent: four threads converting at once produce identical
results. A malformed file returns `CadConvertException` with a reason rather
than taking the process down — the native side catches any panic at the
boundary, because unwinding into a host is undefined behaviour.

## Runtimes

The package carries the native library for every runtime it was built for,
under `runtimes/{rid}/native`: `linux-x64`, `linux-arm64`, `osx-arm64`,
`osx-x64`, `win-x64`. If a P/Invoke fails at load time, the runtime you are on
was not among them.

A .NET Core host finds those on its own. .NET Framework does not — it copies a
runtime-specific file only when the project names a runtime identifier, and
most do not — so the package brings a `build/CadConvert.targets` that puts the
right one beside your executable, and warns at build time if it has none for
the platform you are building for.

`build/native/include/cadconvert.h` is there for a caller that is not .NET at
all; it describes the same library.

Source, benchmarks and the engineering record: <https://github.com/omerbasavul/cadconvert>
