using System;
using CadConvert;

if (args.Length < 2)
{
    Console.Error.WriteLine("usage: smoke <input> <output>");
    return 2;
}

Console.WriteLine($"native cadconvert {CadConverter.NativeVersion}");
try
{
    var result = CadConverter.Convert(args[0], args[1], new ConvertOptions
    {
        Target = MeshTarget.Compact,
    });
    Console.WriteLine($"{result.Bodies} bodies, {result.FacesMeshed}/{result.Faces} faces, "
                      + $"{result.Triangles} triangles, {result.Bytes / 1e6:F2} MB -> {result.OutputPath}");
    foreach (var w in result.Warnings) Console.WriteLine($"  warning: {w}");
    return 0;
}
catch (CadConvertException e)
{
    Console.Error.WriteLine($"failed ({e.Code}): {e.Message}");
    return 1;
}
