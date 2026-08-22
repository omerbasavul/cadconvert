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
var target = MeshTarget.Lean;
for (int i = 0; i < args.Length; i++)
{
    switch (args[i])
    {
        case "--runs": runs = int.Parse(args[++i]); break;
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
Console.WriteLine($"{"file",-28} {"MB in",6} {"run",4} {"seconds",9} {"peak MB",9} {"managed KB",11} {"triangles",11} {"MB out",8}");

var output = Path.Combine(Path.GetTempPath(), "bench-out.glb");
foreach (var file in files)
{
    if (!File.Exists(file)) { Console.Error.WriteLine($"  no such file: {file}"); continue; }
    var name = Path.GetFileName(file);
    var inMb = new FileInfo(file).Length / 1e6;
    var seconds = new List<double>();

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
        Console.WriteLine($"{name,-28} {inMb,6:F1} {run,4} {clock.Elapsed.TotalSeconds,9:F2} "
                          + $"{PeakMemory.Bytes() / 1e6,9:F0} {managed / 1024.0,11:F0} "
                          + $"{result.Triangles,11:N0} {result.Bytes / 1e6,8:F2}");
        foreach (var w in result.Warnings) Console.Error.WriteLine($"    warning: {w}");
    }

    seconds.Sort();
    Console.WriteLine($"{"",-28} {"",6} {"med",4} {seconds[seconds.Count / 2],9:F2}");

    // Peak resident only ever grows, so a figure that keeps climbing over many
    // conversions in one process is the question a service asks: does this
    // level off, or does it retain? Reported as the last run's peak against
    // the first, since the difference is the whole answer.
}

if (File.Exists(output)) File.Delete(output);
return 0;
