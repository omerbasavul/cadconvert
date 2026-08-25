using System;
using System.Collections.Generic;
using System.IO;
using System.Linq;
using System.Threading;
using Xunit;

namespace CadConvert.Tests
{
    /// <summary>Several outputs from one reading, the progress that reports
    /// it, and the two ways to stop it.</summary>
    public class ManyOutputsTests
    {
        [Fact]
        public void OneReadingWritesEveryOutput()
        {
            using var dir = new TempDirectory();
            string[] outputs = { dir.File("part.glb"), dir.File("part.usdz") };

            var result = CadConverter.ConvertMany(Sample.SmallXt, outputs);

            Assert.Equal(outputs, result.Outputs.Select(o => o.Path));
            Assert.Equal(outputs[0], result.OutputPath);
            foreach (var o in result.Outputs)
                Assert.Equal(new FileInfo(o.Path).Length, o.Bytes);
            Assert.Equal(result.Outputs.Sum(o => o.Bytes), result.Bytes);

            byte[] glb = File.ReadAllBytes(outputs[0]);
            Assert.Equal(Sample.GlbMagic, glb.Take(4).ToArray());
            byte[] usdz = File.ReadAllBytes(outputs[1]);
            Assert.Equal(new byte[] { 0x50, 0x4B }, usdz.Take(2).ToArray()); // "PK": a zip
        }

        [Fact]
        public void TheSingleOutputOverloadIsTheManyOverloadWithOne()
        {
            using var dir = new TempDirectory();
            string output = dir.File("one.glb");

            var result = CadConverter.Convert(Sample.SmallXt, output);

            Assert.Single(result.Outputs);
            Assert.Equal(output, result.Outputs[0].Path);
            Assert.Equal(result.Outputs[0].Bytes, result.Bytes);
        }

        [Fact]
        public void ProgressWalksReadThenMeshThenWriteAndEndsComplete()
        {
            using var dir = new TempDirectory();
            string[] outputs = { dir.File("p.glb"), dir.File("p.usdz") };
            var seen = new List<ConvertProgress>();

            CadConverter.ConvertMany(Sample.SmallXt, outputs, progress: seen.Add);

            var stages = seen.Select(p => p.Stage).Distinct().ToArray();
            Assert.Equal(new[] { ConvertStage.Read, ConvertStage.Mesh, ConvertStage.Write }, stages);
            foreach (var stage in stages)
            {
                var ofStage = seen.Where(p => p.Stage == stage).ToList();
                Assert.Equal(0, ofStage.First().Done);
                Assert.Equal(ofStage.Last().Total, ofStage.Last().Done);
                Assert.Equal(string.Empty, ofStage.Last().Detail);
            }
            // The unit in flight is named: a body while meshing, a file while
            // writing — which is what a caller shows a person who is waiting.
            Assert.Contains(seen, p => p.Stage == ConvertStage.Mesh && p.Detail.Length > 0);
            Assert.Contains(seen, p => p.Stage == ConvertStage.Write && p.Detail.EndsWith("p.usdz"));
            Assert.Equal(2, seen.Last(p => p.Stage == ConvertStage.Write).Total);
        }

        [Fact]
        public void ACancelledTokenStopsTheConversionAndWritesNothing()
        {
            using var dir = new TempDirectory();
            string output = dir.File("stopped.glb");
            using var cts = new CancellationTokenSource();
            // Cancel at the first report from meshing: the input is read, no
            // file is written.
            void OnProgress(ConvertProgress p)
            {
                if (p.Stage == ConvertStage.Mesh) cts.Cancel();
            }

            var error = Assert.Throws<OperationCanceledException>(
                () => CadConverter.ConvertMany(Sample.SmallXt, new[] { output }, progress: OnProgress, cancellationToken: cts.Token));

            Assert.Equal(cts.Token, error.CancellationToken);
            Assert.False(File.Exists(output));
        }

        [Fact]
        public void AnExceptionInTheProgressHandlerComesBackAsItselfAndStopsTheWork()
        {
            using var dir = new TempDirectory();
            string output = dir.File("thrown.glb");

            var error = Assert.Throws<InvalidOperationException>(
                () => CadConverter.ConvertMany(Sample.SmallXt, new[] { output },
                    progress: p => { if (p.Stage == ConvertStage.Write) throw new InvalidOperationException("no, thank you"); }));

            Assert.Equal("no, thank you", error.Message);
            Assert.False(File.Exists(output), "the handler refused before the first write");
        }

        [Fact]
        public void TheLibraryStillWorksAfterACancellation()
        {
            using var dir = new TempDirectory();
            using var cts = new CancellationTokenSource();
            cts.Cancel();
            Assert.Throws<OperationCanceledException>(
                () => CadConverter.ConvertMany(Sample.SmallXt, new[] { dir.File("no.glb") }, cancellationToken: cts.Token));

            var result = CadConverter.Convert(Sample.SmallXt, dir.File("yes.glb"));
            Assert.True(result.Triangles > 0);
        }

        [Fact]
        public void AGlbConvertsOnwardToAUsdzWithTheSameTriangles()
        {
            // The glTF this library writes is a file it also reads: a part
            // held as GLB can be given a USDZ later without the CAD file.
            using var dir = new TempDirectory();
            string glb = dir.File("held.glb");
            var fromCad = CadConverter.Convert(Sample.SmallXt, glb);

            var fromGlb = CadConverter.Convert(glb, dir.File("later.usdz"));

            Assert.Equal(fromCad.Triangles, fromGlb.Triangles);
            Assert.Equal(fromCad.Bodies, fromGlb.Bodies);
            Assert.Equal(0, fromGlb.Faces); // a mesh has triangles and no faces
            Assert.Empty(fromGlb.Warnings);
        }

        [Fact]
        public void NoOutputsIsRefusedBeforeTheAbi()
        {
            Assert.Throws<ArgumentException>(() => CadConverter.ConvertMany(Sample.SmallXt, Array.Empty<string>()));
            Assert.Throws<ArgumentNullException>(() => CadConverter.ConvertMany(Sample.SmallXt, new string[] { null }));
            Assert.Throws<ArgumentNullException>(() => CadConverter.ConvertMany(Sample.SmallXt, (IReadOnlyList<string>)null));
        }
    }
}
