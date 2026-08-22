using System;
using System.IO;
using System.Linq;
using System.Threading.Tasks;
using Xunit;

namespace CadConvert.Tests
{
    /// <summary>A plug-in host may well call this from a worker pool. The
    /// library holds no mutable global state, and these tests are what says
    /// so.</summary>
    public class ConcurrencyTests
    {
        [Fact]
        public void FourThreadsConvertingAtOnceAllAgree()
        {
            using var dir = new TempDirectory();

            var results = Enumerable.Range(0, 4)
                .AsParallel()
                .WithDegreeOfParallelism(4)
                .Select(i => CadConverter.Convert(Sample.SmallXt, dir.File($"thread-{i}.glb")))
                .ToArray();

            foreach (var result in results)
            {
                Assert.Equal(results[0].Triangles, result.Triangles);
                Assert.Equal(results[0].Bytes, result.Bytes);
                Assert.Equal(results[0].Faces, result.Faces);
            }

            // Byte-for-byte, not just count-for-count: a shared buffer would
            // show up here and nowhere else.
            byte[] first = File.ReadAllBytes(dir.File("thread-0.glb"));
            for (int i = 1; i < 4; i++)
            {
                Assert.Equal(first, File.ReadAllBytes(dir.File($"thread-{i}.glb")));
            }
        }

        [Fact]
        public async Task AFailingConversionDoesNotDisturbASucceedingOne()
        {
            using var dir = new TempDirectory();
            string junk = Sample.Write(dir, "junk.x_t", Sample.TruncatedXt(400));

            var failing = Task.Run(() =>
            {
                for (int i = 0; i < 8; i++)
                {
                    Record.Exception(() => CadConverter.Convert(junk, dir.File($"junk-{i}.glb")));
                }
            });

            var succeeding = Task.Run(() =>
            {
                var counts = new long[8];
                for (int i = 0; i < 8; i++)
                {
                    counts[i] = CadConverter.Convert(Sample.SmallXt, dir.File($"good-{i}.glb")).Triangles;
                }
                return counts;
            });

            await failing;
            long[] triangles = await succeeding;

            Assert.All(triangles, t => Assert.Equal(triangles[0], t));
            Assert.True(triangles[0] > 0);
        }

        [Fact]
        public void RepeatedConversionsDoNotGrowWithoutBound()
        {
            // Not a leak detector — the allocator keeps its high-water mark and
            // will not give it back. What this catches is the other thing: a
            // handle or a buffer retained per call, which shows as growth that
            // does not settle.
            using var dir = new TempDirectory();

            CadConverter.Convert(Sample.SmallXt, dir.File("warm.glb"));
            GC.Collect();
            GC.WaitForPendingFinalizers();
            long settled = GC.GetTotalMemory(forceFullCollection: true);

            for (int i = 0; i < 20; i++)
            {
                CadConverter.Convert(Sample.SmallXt, dir.File("again.glb"));
            }

            GC.Collect();
            GC.WaitForPendingFinalizers();
            long after = GC.GetTotalMemory(forceFullCollection: true);

            // Twenty conversions of a 6 KB part must not add megabytes of
            // managed heap. The margin is wide because this is a smoke alarm,
            // not a scale.
            Assert.True(after - settled < 4 * 1024 * 1024,
                $"managed heap grew {(after - settled) / 1024} KB over 20 conversions");
        }
    }
}
