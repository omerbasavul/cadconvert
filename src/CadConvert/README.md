# CadConvert

Parasolid (`.x_t`) and STEP (`.stp`) to glTF 2.0. Reads the B-Rep, tessellates
it watertight, and writes a `.glb` with materials — no CAD kernel to install.

```csharp
using CadConvert;

var result = CadConverter.Convert("housing.x_t", "housing.glb");

Console.WriteLine($"{result.Bodies} bodies, {result.Triangles:N0} triangles");
foreach (var warning in result.Warnings)
    Console.WriteLine($"warning: {warning}");
```

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

Source, benchmarks and the engineering record: <https://github.com/omerbasavul/cadconvert>
