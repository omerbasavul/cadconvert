using System;
using System.IO;

namespace CadConvert.Tests
{
    /// <summary>The test inputs, and somewhere to write outputs that is cleaned
    /// up afterwards.</summary>
    internal static class Sample
    {
        /// <summary>A real 6 KB Parasolid part, copied beside the test assembly
        /// by the project file. Small enough to convert many times in a test
        /// run, real enough that its geometry exercises the reader.</summary>
        public static string SmallXt =>
            Path.Combine(AppContext.BaseDirectory, "samples", "small.x_t");

        /// <summary>The first four bytes of a glTF binary file.</summary>
        public static readonly byte[] GlbMagic = { 0x67, 0x6C, 0x54, 0x46 }; // "glTF"

        /// <summary>Write <paramref name="bytes"/> to a new file under
        /// <paramref name="dir"/> and return its path.</summary>
        public static string Write(TempDirectory dir, string name, byte[] bytes)
        {
            string path = Path.Combine(dir.Path, name);
            File.WriteAllBytes(path, bytes);
            return path;
        }

        /// <summary>The sample truncated to its first <paramref name="bytes"/>
        /// bytes — a file that starts out looking entirely valid.</summary>
        public static byte[] TruncatedXt(int bytes)
        {
            byte[] whole = File.ReadAllBytes(SmallXt);
            var cut = new byte[Math.Min(bytes, whole.Length)];
            Array.Copy(whole, cut, cut.Length);
            return cut;
        }
    }

    /// <summary>A directory that deletes itself. Outputs go here rather than
    /// into the source tree.</summary>
    internal sealed class TempDirectory : IDisposable
    {
        public string Path { get; }

        public TempDirectory()
        {
            Path = System.IO.Path.Combine(
                System.IO.Path.GetTempPath(),
                "cadconvert-tests-" + Guid.NewGuid().ToString("N"));
            Directory.CreateDirectory(Path);
        }

        public string File(string name) => System.IO.Path.Combine(Path, name);

        public void Dispose()
        {
            try { Directory.Delete(Path, recursive: true); }
            catch (IOException) { /* a leftover temp directory is not a failure */ }
        }
    }
}
