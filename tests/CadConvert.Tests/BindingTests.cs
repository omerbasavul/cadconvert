using System;
using System.IO;
using System.Linq;
using Xunit;

namespace CadConvert.Tests
{
    /// <summary>That the managed side and the native side agree: the library
    /// loads at all, and what crosses the ABI arrives intact.</summary>
    public class BindingTests
    {
        [Fact]
        public void TheNativeLibraryLoadsAndNamesItsVersion()
        {
            // This is the test that fails first when the runtimes/{rid}/native
            // layout is wrong, which is a mistake that produces no build error.
            string version = CadConverter.NativeVersion;

            Assert.False(string.IsNullOrWhiteSpace(version));
            Assert.Matches(@"^\d+\.\d+\.\d+", version);
        }

        [Fact]
        public void TheNativeLibraryCannotBeMistakenForThisAssembly()
        {
            // The native library sits in this directory beside the managed one.
            // Windows does not distinguish cadconvert.dll from CadConvert.dll,
            // so when the native library was called "cadconvert" the copy that
            // put it here overwrote the assembly this test is running from, and
            // all 26 tests died on "Could not load file or assembly". The two
            // unix runtimes never saw it — libcadconvert.so collides with
            // nothing — so it survived every local run and appeared at the
            // first push.
            //
            // This runs everywhere and fails everywhere, which is the point.
            var managed = Path.GetFileNameWithoutExtension(
                typeof(CadConverter).Assembly.Location);

            foreach (string native in Directory.GetFiles(AppContext.BaseDirectory)
                         .Where(f => f.EndsWith(".dll", StringComparison.OrdinalIgnoreCase)
                                     || f.EndsWith(".so", StringComparison.OrdinalIgnoreCase)
                                     || f.EndsWith(".dylib", StringComparison.OrdinalIgnoreCase))
                         .Where(f => Path.GetFileName(f).Contains("cadconvert",
                                     StringComparison.OrdinalIgnoreCase))
                         .Select(Path.GetFileNameWithoutExtension)
                         .Where(f => !string.Equals(f, managed, StringComparison.Ordinal)))
            {
                string bare = native.StartsWith("lib", StringComparison.Ordinal)
                    ? native.Substring(3)
                    : native;

                Assert.False(
                    string.Equals(bare, managed, StringComparison.OrdinalIgnoreCase),
                    $"'{native}' and '{managed}' are the same file name on a "
                    + "case-insensitive filesystem, and they share a directory.");
            }
        }

        [Fact]
        public void APathThatIsNotAsciiSurvivesTheCrossing()
        {
            // The binding marshals paths as UTF-8 by hand because
            // Marshal.PtrToStringAnsi mangles anything outside ASCII and
            // PtrToStringUTF8 is not in netstandard2.0. A Turkish dotted i and
            // a Chinese character are the cheap way to prove it.
            using var dir = new TempDirectory();
            string input = Path.Combine(dir.Path, "yivli-şaft-零件.x_t");
            File.Copy(Sample.SmallXt, input);
            string output = dir.File("çıktı-输出.glb");

            var result = CadConverter.Convert(input, output);

            Assert.True(File.Exists(output));
            Assert.Equal(output, result.OutputPath);
        }

        [Fact]
        public void ANullPathIsRefusedBeforeItReachesTheAbi()
        {
            using var dir = new TempDirectory();

            Assert.Throws<ArgumentNullException>(
                () => CadConverter.Convert(null, dir.File("out.glb")));
            Assert.Throws<ArgumentNullException>(
                () => CadConverter.Convert(Sample.SmallXt, null));
        }

        [Fact]
        public void TheSummaryCountsAreTheOnesTheFileHolds()
        {
            using var dir = new TempDirectory();
            string output = dir.File("counts.glb");

            var result = CadConverter.Convert(Sample.SmallXt, output);

            // Every count crosses as a ulong and is read back as a long. A
            // marshalling mistake here shows up as a wild or negative number,
            // not as an error.
            Assert.True(result.Bodies > 0, $"bodies: {result.Bodies}");
            Assert.True(result.Faces > 0, $"faces: {result.Faces}");
            Assert.True(result.Triangles > 0, $"triangles: {result.Triangles}");
            Assert.InRange(result.FacesMeshed, 1, result.Faces);

            // Bytes is the native side's own count of what it wrote. If it
            // disagrees with the file on disk, the two sides are reading the
            // struct differently.
            Assert.Equal(new FileInfo(output).Length, result.Bytes);
        }

        [Fact]
        public void WarningsAreAListRatherThanOneRunOnString()
        {
            using var dir = new TempDirectory();

            var result = CadConverter.Convert(Sample.SmallXt, dir.File("warn.glb"));

            Assert.NotNull(result.Warnings);
            Assert.DoesNotContain(result.Warnings, w => w.Contains("\n"));
        }
    }
}
