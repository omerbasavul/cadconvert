using System.Diagnostics;
using CadConvert;
using CadConvert.Bench;

// What a conversion costs, from the side a caller sees it from.
//
// Wall time and peak working set are the process's, read from the OS, because
// that is what a host measures: nearly all of the work happens in native code
// and none of it shows in the managed heap. GC.GetTotalAllocatedBytes is
// printed beside them to make that point rather than to measure the converter.

if (args.Length == 0)
{
    Console.Error.WriteLine("usage: bench <file> [file...] [--runs N] [--quality plain|lean|compact]");
    return 2;
}

var files = new List<string>();
int runs = 3;
// Seconds to wait after the last conversion before reading held memory again.
// An allocator that returns pages lazily has not finished when `Convert` has:
// mimalloc holds a freed page for its purge delay and an arena for ten times
// that, so a reading taken the instant a conversion returns says nothing about
// whether the memory comes back. Zero skips the wait.
int settle = 0;
var target = MeshTarget.Lean;
for (int i = 0; i < args.Length; i++)
{
    switch (args[i])
    {
        case "--runs": runs = int.Parse(args[++i]); break;
        case "--settle": settle = int.Parse(args[++i]); break;
        case "--quality":
            target = args[++i] switch
            {
                "plain" => MeshTarget.Plain,
                "compact" => MeshTarget.Compact,
                _ => MeshTarget.Lean,
            };
            break;
        default: files.Add(args[i]); break;
    }
}

Console.WriteLine($"cadconvert {CadConverter.NativeVersion} on {Environment.OSVersion.Platform} "
                  + $"{System.Runtime.InteropServices.RuntimeInformation.ProcessArchitecture}, "
                  + $"{Environment.ProcessorCount} processors, quality {target}");
Console.WriteLine();
Console.WriteLine($"{"file",-28} {"MB in",6} {"run",4} {"seconds",9} {"peak MB",9} {"held MB",8} {"managed KB",11} {"triangles",11} {"MB out",8}");

// What the host holds before any conversion — the runtime, the JIT and the
// loaded libraries. Every figure below is only meaningful against it.
Console.WriteLine($"held before any conversion: {PeakMemory.CurrentBytes() / 1e6:F0} MB");
Console.WriteLine();

var output = Path.Combine(Path.GetTempPath(), "bench-out.glb");
foreach (var file in files)
{
    if (!File.Exists(file)) { Console.Error.WriteLine($"  no such file: {file}"); continue; }
    var name = Path.GetFileName(file);
    var inMb = new FileInfo(file).Length / 1e6;
    var seconds = new List<double>();
    var heldPerRun = new List<double>();

    for (int run = 1; run <= runs; run++)
    {
        // Peak resident size only ever grows within a process, so every run
        // after the first reports the high-water mark of all of them. That is
        // the honest reading: the first run's figure is this file's cost, and
        // a later run showing the same number means it did not cost more.
        long managedBefore = GC.GetTotalAllocatedBytes(precise: false);

        var clock = Stopwatch.StartNew();
        var result = CadConverter.Convert(file, output, new ConvertOptions { Target = target });
        clock.Stop();
        seconds.Add(clock.Elapsed.TotalSeconds);

        long managed = GC.GetTotalAllocatedBytes(precise: false) - managedBefore;
        // Read after the conversion has returned and the managed heap has been
        // collected, so this is what the native side is still holding.
        double held = PeakMemory.CurrentBytes() / 1e6;
        heldPerRun.Add(held);
        Console.WriteLine($"{name,-28} {inMb,6:F1} {run,4} {clock.Elapsed.TotalSeconds,9:F2} "
                          + $"{PeakMemory.Bytes() / 1e6,9:F0} {held,8:F0} {managed / 1024.0,11:F0} "
                          + $"{result.Triangles,11:N0} {result.Bytes / 1e6,8:F2}");
        foreach (var w in result.Warnings) Console.Error.WriteLine($"    warning: {w}");
    }

    var sorted = new List<double>(seconds);
    sorted.Sort();
    Console.WriteLine($"{"",-28} {"",6} {"med",4} {sorted[sorted.Count / 2],9:F2}");

    // The question a service asks is not what one conversion costs but whether
    // the next one starts where the first did.
    //
    // The first reading against the last is not the way to answer it, and this
    // printed exactly that until eight runs gave 328, 230, 260, 282, 272, 344,
    // 253, 422 — a series that does not trend at all, reported as "13.5 MB per
    // conversion" purely because the last sample happened to be high. The
    // spread is the answer: a range that stays put is memory coming back, and
    // a floor that walks upwards is memory that is not.
    if (heldPerRun.Count > 1)
    {
        var held = new List<double>(heldPerRun);
        held.Sort();
        Console.WriteLine($"{"",-28} held between conversions: {held[0],5:F0} to {held[^1],5:F0} MB, "
                          + $"median {held[held.Count / 2],5:F0}");
    }

    if (settle > 0)
    {
        Thread.Sleep(TimeSpan.FromSeconds(settle));
        // After the allocator's own delays have run out, which is what a host
        // sees between bursts of work rather than during one.
        Console.WriteLine($"{"",-28} held {settle}s after the last: {PeakMemory.CurrentBytes() / 1e6,5:F0} MB");
    }
}

if (File.Exists(output)) File.Delete(output);
return 0;
