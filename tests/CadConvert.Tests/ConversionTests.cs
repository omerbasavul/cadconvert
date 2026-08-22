using System;
using System.IO;
using System.Linq;
using Xunit;

namespace CadConvert.Tests
{
    /// <summary>What a conversion produces, and what the options are allowed to
    /// change about it.</summary>
    public class ConversionTests
    {
        [Fact]
        public void TheOutputIsAGltfBinaryFile()
        {
            using var dir = new TempDirectory();
            string output = dir.File("part.glb");

            CadConverter.Convert(Sample.SmallXt, output);

            byte[] head = new byte[12];
            using (var file = File.OpenRead(output)) file.Read(head, 0, head.Length);

            Assert.Equal(Sample.GlbMagic, head.Take(4).ToArray());
            Assert.Equal(2u, BitConverter.ToUInt32(head, 4));                    // version
            Assert.Equal(new FileInfo(output).Length, BitConverter.ToUInt32(head, 8));
        }

        [Fact]
        public void TheFormatIsReadFromTheFileRatherThanItsName()
        {
            // A Parasolid part called .stp is still a Parasolid part. The
            // converter reads the header first and the extension second, so a
            // file renamed on the way out of some other tool still converts.
            using var dir = new TempDirectory();
            string misnamed = dir.File("actually-parasolid.stp");
            File.Copy(Sample.SmallXt, misnamed);

            var result = CadConverter.Convert(misnamed, dir.File("out.glb"));

            Assert.True(result.Triangles > 0);
        }

        [Theory]
        [InlineData(MeshTarget.Plain)]
        [InlineData(MeshTarget.Lean)]
        [InlineData(MeshTarget.Compact)]
        public void EveryTargetWritesAReadableFile(MeshTarget target)
        {
            using var dir = new TempDirectory();
            string output = dir.File($"{target}.glb");

            var result = CadConverter.Convert(Sample.SmallXt, output,
                new ConvertOptions { Target = target });

            Assert.True(result.Bytes > 0);
            byte[] magic = new byte[4];
            using (var file = File.OpenRead(output)) file.Read(magic, 0, 4);
            Assert.Equal(Sample.GlbMagic, magic);
        }

        [Fact]
        public void TheSmallerTargetsCostBytes_NotTriangles()
        {
            // The rule this project holds itself to: making the file smaller
            // must not make the mesh coarser. Lean and Compact change how a
            // vertex is stored, never how many there are, so the triangle count
            // is the invariant that says a size win was honest.
            using var dir = new TempDirectory();

            var plain = CadConverter.Convert(Sample.SmallXt, dir.File("p.glb"),
                new ConvertOptions { Target = MeshTarget.Plain });
            var lean = CadConverter.Convert(Sample.SmallXt, dir.File("l.glb"),
                new ConvertOptions { Target = MeshTarget.Lean });
            var compact = CadConverter.Convert(Sample.SmallXt, dir.File("c.glb"),
                new ConvertOptions { Target = MeshTarget.Compact });

            Assert.Equal(plain.Triangles, lean.Triangles);
            Assert.Equal(plain.Triangles, compact.Triangles);
            Assert.Equal(plain.Faces, compact.Faces);
            Assert.Equal(plain.FacesMeshed, compact.FacesMeshed);

            Assert.True(lean.Bytes < plain.Bytes,
                $"lean {lean.Bytes} is not under plain {plain.Bytes}");
            Assert.True(compact.Bytes < lean.Bytes,
                $"compact {compact.Bytes} is not under lean {lean.Bytes}");
        }

        [Fact]
        public void ANullToleranceMeansTheConvertersOwn_NotZero()
        {
            // Zero crosses the ABI as "use yours". An earlier version of the
            // CLI and the ABI invented their own coarser defaults instead, and
            // silently opened six hundred edges in the pilot part. Leaving both
            // tolerances unset must give exactly what the library would choose
            // on its own.
            using var dir = new TempDirectory();

            var unset = CadConverter.Convert(Sample.SmallXt, dir.File("unset.glb"),
                new ConvertOptions { SagMillimetres = null, AngleDegrees = null });
            var alsoUnset = CadConverter.Convert(Sample.SmallXt, dir.File("also.glb"),
                new ConvertOptions());

            Assert.Equal(unset.Triangles, alsoUnset.Triangles);
            Assert.Equal(unset.Bytes, alsoUnset.Bytes);
        }

        [Fact]
        public void ACoarserToleranceGivesFewerTriangles()
        {
            // The tolerances have to actually reach the tessellator. If they
            // were dropped on the way across, every reading here would be
            // identical and nothing else in the suite would notice.
            using var dir = new TempDirectory();

            var fine = CadConverter.Convert(Sample.SmallXt, dir.File("fine.glb"),
                new ConvertOptions { SagMillimetres = 0.001, AngleDegrees = 2 });
            var coarse = CadConverter.Convert(Sample.SmallXt, dir.File("coarse.glb"),
                new ConvertOptions { SagMillimetres = 0.5, AngleDegrees = 30 });

            Assert.True(coarse.Triangles < fine.Triangles,
                $"coarse {coarse.Triangles} is not under fine {fine.Triangles}");
            Assert.Equal(fine.Faces, coarse.Faces);
        }

        [Fact]
        public void ConvertingTwiceGivesTheSameFileByte_ForByte()
        {
            // Rayon meshes faces in parallel. If any of that ordering reached
            // the output the file would differ run to run, which makes a build
            // unreproducible and a diff useless.
            using var dir = new TempDirectory();

            CadConverter.Convert(Sample.SmallXt, dir.File("once.glb"));
            CadConverter.Convert(Sample.SmallXt, dir.File("twice.glb"));

            Assert.Equal(File.ReadAllBytes(dir.File("once.glb")),
                         File.ReadAllBytes(dir.File("twice.glb")));
        }

        [Fact]
        public void TheOutputsExtensionChoosesTheContainer()
        {
            // A caller who names the file .usdz has said which format they
            // want more plainly than any option could, so the name wins over
            // the target — including over the default.
            using var dir = new TempDirectory();
            string output = dir.File("part.usdz");

            var result = CadConverter.Convert(Sample.SmallXt, output,
                new ConvertOptions { Target = MeshTarget.Lean });

            Assert.True(result.Triangles > 0);
            // A USDZ is a zip, and its first entry is the scene.
            byte[] head = new byte[4];
            using (var file = File.OpenRead(output)) file.Read(head, 0, 4);
            Assert.Equal(new byte[] { 0x50, 0x4B, 0x03, 0x04 }, head);
        }

        [Fact]
        public void AUsdzTargetStillWritesGltfWhenTheNameSaysGlb()
        {
            using var dir = new TempDirectory();
            string output = dir.File("part.glb");

            CadConverter.Convert(Sample.SmallXt, output,
                new ConvertOptions { Target = MeshTarget.Usdz });

            byte[] magic = new byte[4];
            using (var file = File.OpenRead(output)) file.Read(magic, 0, 4);
            Assert.Equal(Sample.GlbMagic, magic);
        }

        [Fact]
        public void AUsdzCarriesTheSameMeshAsTheGlb()
        {
            // Two containers, one scene. If the counts ever disagree, one of
            // the writers is dropping something.
            using var dir = new TempDirectory();

            var glb = CadConverter.Convert(Sample.SmallXt, dir.File("a.glb"));
            var usdz = CadConverter.Convert(Sample.SmallXt, dir.File("a.usdz"));

            Assert.Equal(glb.Triangles, usdz.Triangles);
            Assert.Equal(glb.Faces, usdz.Faces);
            Assert.Equal(glb.FacesMeshed, usdz.FacesMeshed);
            Assert.Equal(glb.Bodies, usdz.Bodies);
        }

        [Fact]
        public void AnExistingOutputFileIsReplaced()
        {
            using var dir = new TempDirectory();
            string output = dir.File("existing.glb");
            File.WriteAllText(output, "this is not a glb and must not survive");

            var result = CadConverter.Convert(Sample.SmallXt, output);

            Assert.Equal(new FileInfo(output).Length, result.Bytes);
            byte[] magic = new byte[4];
            using (var file = File.OpenRead(output)) file.Read(magic, 0, 4);
            Assert.Equal(Sample.GlbMagic, magic);
        }
    }
}
