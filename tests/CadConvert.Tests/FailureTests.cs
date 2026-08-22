using System;
using System.IO;
using System.Text;
using Xunit;

namespace CadConvert.Tests
{
    /// <summary>Bad input reaches this library through a plug-in host that will
    /// not survive a panic crossing the ABI. Every one of these must come back
    /// as an exception with a code, and the process must still be standing
    /// afterwards.</summary>
    public class FailureTests
    {
        // The codes the C header defines. A managed copy of them is the point:
        // if the native side ever renumbers, these tests say so.
        private const int ErrUnknownFormat = 3;
        private const int ErrRead = 4;
        private const int ErrWrite = 5;
        private const int ErrPanic = 6;

        [Fact]
        public void AFileOfNoKnownFormatIsNamedAsSuch()
        {
            using var dir = new TempDirectory();
            string input = Sample.Write(dir, "notes.txt",
                Encoding.UTF8.GetBytes("just some text, no CAD anywhere in it"));

            var error = Assert.Throws<CadConvertException>(
                () => CadConverter.Convert(input, dir.File("out.glb")));

            Assert.Equal(ErrUnknownFormat, error.Code);
            Assert.False(string.IsNullOrWhiteSpace(error.Message));
        }

        [Fact]
        public void AnEmptyFileIsBlamedOnWhicheverEvidenceThereIs()
        {
            using var dir = new TempDirectory();

            // The header decides the format, and an empty file has none — so
            // the extension is all there is to go on. With one, the answer is
            // "this Parasolid file would not read"; without one it is "this is
            // not a format I know". Two different things to tell a user, and
            // the converter says the right one of them.
            string named = Sample.Write(dir, "empty.x_t", Array.Empty<byte>());
            var withExtension = Assert.Throws<CadConvertException>(
                () => CadConverter.Convert(named, dir.File("a.glb")));
            Assert.Equal(ErrRead, withExtension.Code);

            string anonymous = Sample.Write(dir, "empty", Array.Empty<byte>());
            var without = Assert.Throws<CadConvertException>(
                () => CadConverter.Convert(anonymous, dir.File("b.glb")));
            Assert.Equal(ErrUnknownFormat, without.Code);
        }

        [Fact]
        public void AMissingFileIsAReadFailure()
        {
            using var dir = new TempDirectory();

            var error = Assert.Throws<CadConvertException>(
                () => CadConverter.Convert(dir.File("nothing-here.x_t"), dir.File("out.glb")));

            Assert.Contains(error.Code, new[] { ErrRead, ErrUnknownFormat });
            Assert.NotEqual(ErrPanic, error.Code);
        }

        [Theory]
        [InlineData(16)]
        [InlineData(200)]
        [InlineData(1024)]
        [InlineData(6000)]
        public void ATruncatedPartFailsRatherThanCrashes(int keep)
        {
            // A file cut short still carries a valid Parasolid header, so the
            // reader commits to it and then runs out. These are the cuts that
            // used to reach an unwrap.
            using var dir = new TempDirectory();
            string input = Sample.Write(dir, $"cut-{keep}.x_t", Sample.TruncatedXt(keep));

            var error = Record.Exception(
                () => CadConverter.Convert(input, dir.File("out.glb")));

            // Some cuts land on a boundary that happens to be a complete part;
            // succeeding is fine. Panicking is not.
            if (error != null)
            {
                var failure = Assert.IsType<CadConvertException>(error);
                Assert.NotEqual(ErrPanic, failure.Code);
            }
        }

        [Fact]
        public void ACorruptedByteInTheMiddleFailsRatherThanCrashes()
        {
            using var dir = new TempDirectory();
            byte[] bytes = File.ReadAllBytes(Sample.SmallXt);

            // Deterministic, not random: a test that fails once a fortnight
            // tells nobody anything.
            for (int at = 700; at < bytes.Length; at += 373)
            {
                byte[] damaged = (byte[])bytes.Clone();
                damaged[at] ^= 0xFF;
                string input = Sample.Write(dir, $"flip-{at}.x_t", damaged);

                var error = Record.Exception(
                    () => CadConverter.Convert(input, dir.File($"flip-{at}.glb")));

                if (error != null)
                {
                    var failure = Assert.IsType<CadConvertException>(error);
                    Assert.True(failure.Code != ErrPanic,
                        $"byte {at} flipped panicked across the ABI: {failure.Message}");
                }
            }
        }

        [Fact]
        public void AnUnwritableOutputIsAWriteFailure()
        {
            using var dir = new TempDirectory();
            // A directory where the file should be: opening it for writing
            // fails on every platform this ships to.
            string output = dir.File("output.glb");
            Directory.CreateDirectory(output);

            var error = Assert.Throws<CadConvertException>(
                () => CadConverter.Convert(Sample.SmallXt, output));

            Assert.Equal(ErrWrite, error.Code);
        }

        [Fact]
        public void TheLibraryStillWorksAfterEveryFailureAbove()
        {
            // The reason all of this matters. A failed conversion must leave
            // nothing behind that stops the next one — no poisoned lock, no
            // half-freed string, no torn global state.
            using var dir = new TempDirectory();

            for (int i = 0; i < 5; i++)
            {
                string junk = Sample.Write(dir, $"junk-{i}.x_t", Sample.TruncatedXt(300 + i));
                Record.Exception(() => CadConverter.Convert(junk, dir.File($"junk-{i}.glb")));
            }

            var result = CadConverter.Convert(Sample.SmallXt, dir.File("after.glb"));

            Assert.True(result.Triangles > 0);
        }
    }
}
