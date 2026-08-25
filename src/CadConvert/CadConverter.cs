using System;
using System.Collections.Generic;
using System.IO;
using System.Runtime.ExceptionServices;
using System.Runtime.InteropServices;
using System.Text;
using System.Threading;

namespace CadConvert
{
    /// <summary>What to write.</summary>
    public enum MeshTarget
    {
        /// <summary>Positions and normals exactly as computed.</summary>
        Plain = 0,
        /// <summary>Normals encoded a byte a component. No vertex moves.</summary>
        Lean = 1,
        /// <summary>Positions on each mesh's own 16-bit grid as well. Smallest,
        /// and it collapses the finest slivers.</summary>
        Compact = 2,
        /// <summary>A USDZ package rather than glTF. The same scene and the
        /// same materials in USD's binary encoding.</summary>
        Usdz = 3,
    }

    /// <summary>How finely to mesh, and what to write.</summary>
    public sealed class ConvertOptions
    {
        /// <summary>How far the mesh may sit from the true surface, in
        /// millimetres. <c>null</c>, the default, means the converter's own,
        /// which is 0.04% of the model's diagonal — a fraction rather than a
        /// fixed distance, so it scales with the part.</summary>
        public double? SagMillimetres { get; set; }

        /// <summary>Largest angle between adjacent facet normals, in degrees.
        /// This is what keeps a small hole from becoming a triangle: a distance
        /// alone is satisfied by almost no subdivision on a small radius.
        /// <c>null</c>, the default, means the converter's own, which is 8°.
        /// </summary>
        public double? AngleDegrees { get; set; }

        /// <summary>What to write. Lean is the default: smaller, with every
        /// vertex exactly where it was computed. An output path's extension
        /// wins over this: a path ending <c>.usdz</c> writes a USD package
        /// whatever is set here, and one ending <c>.glb</c> writes glTF.</summary>
        public MeshTarget Target { get; set; } = MeshTarget.Lean;

        /// <summary>Read a STEP file's <c>.x_t</c> twin, when one sits beside it,
        /// for the designer's own metal-versus-matte per face. A STEP carries a
        /// colour and nothing about finish.</summary>
        public bool UseParasolidTwin { get; set; } = true;
    }

    /// <summary>Where a conversion is.</summary>
    public enum ConvertStage
    {
        /// <summary>Reading the input. One unit.</summary>
        Read = 1,
        /// <summary>Meshing. A unit is a body with a boundary representation; a
        /// glTF input arrives meshed and reports zero of zero.</summary>
        Mesh = 2,
        /// <summary>Writing. A unit is an output file.</summary>
        Write = 3,
    }

    /// <summary>A report between units of work, on the calling thread.</summary>
    public readonly struct ConvertProgress
    {
        internal ConvertProgress(ConvertStage stage, long done, long total, string detail)
        {
            Stage = stage;
            Done = done;
            Total = total;
            Detail = detail;
        }

        /// <summary>The stage the work is in.</summary>
        public ConvertStage Stage { get; }
        /// <summary>Units of <see cref="Stage"/> finished so far.</summary>
        public long Done { get; }
        /// <summary>Units the stage has in all.</summary>
        public long Total { get; }
        /// <summary>The unit about to start — the input, a body, an output
        /// path — or empty when <see cref="Done"/> equals <see cref="Total"/>.
        /// </summary>
        public string Detail { get; }

        /// <summary>"Mesh 3/46: bracket" — what a person waiting wants to read.</summary>
        public override string ToString() =>
            Detail.Length == 0 ? $"{Stage} {Done}/{Total}" : $"{Stage} {Done}/{Total}: {Detail}";
    }

    /// <summary>One file a conversion wrote.</summary>
    public sealed class ConvertOutput
    {
        /// <summary>Where the file was written, as it was asked for.</summary>
        public string Path { get; internal set; }
        /// <summary>The size of that file.</summary>
        public long Bytes { get; internal set; }
    }

    /// <summary>What one conversion produced.</summary>
    public sealed class ConvertResult
    {
        /// <summary>The first output. A conversion with one output has one; see
        /// <see cref="Outputs"/> for the rest.</summary>
        public string OutputPath { get; internal set; }
        /// <summary>Every output's size, added up.</summary>
        public long Bytes { get; internal set; }
        /// <summary>Each file written, in the order asked for.</summary>
        public IReadOnlyList<ConvertOutput> Outputs { get; internal set; } = Array.Empty<ConvertOutput>();
        /// <summary>Solids read from the file.</summary>
        public long Bodies { get; internal set; }
        /// <summary>Faces the file declared. Zero for a glTF input, which has
        /// triangles and no faces.</summary>
        public long Faces { get; internal set; }
        /// <summary>Faces that produced triangles. Fewer than <see cref="Faces"/>
        /// means the difference is named in <see cref="Warnings"/>.</summary>
        public long FacesMeshed { get; internal set; }
        /// <summary>Triangles written.</summary>
        public long Triangles { get; internal set; }

        /// <summary>Anything the readers or the tessellator could not do.
        /// A conversion that produced a file and a warning is not a failure,
        /// and dropping the warning is how a caller ships a hole.</summary>
        public IReadOnlyList<string> Warnings { get; internal set; } = Array.Empty<string>();
    }

    /// <summary>A conversion that did not produce a file.</summary>
    public class CadConvertException : Exception
    {
        /// <summary>The native error code.</summary>
        public int Code { get; }
        /// <summary>A failure with the reason the converter gave.</summary>
        public CadConvertException(int code, string message) : base(message) => Code = code;
    }

    /// <summary>Parasolid, STEP and glTF to glTF binary or USDZ.</summary>
    public static class CadConverter
    {
        /// <summary>The native library's version.</summary>
        public static string NativeVersion => ReadUtf8(Native.cadconvert_version());

        /// <summary>Read <paramref name="input"/>, mesh it, and write it to
        /// <paramref name="output"/>.</summary>
        /// <exception cref="CadConvertException">The file could not be read,
        /// was of no format this knows, or could not be written.</exception>
        public static ConvertResult Convert(string input, string output, ConvertOptions options = null)
        {
            if (output == null) throw new ArgumentNullException(nameof(output));
            return ConvertMany(input, new[] { output }, options);
        }

        /// <summary>Read <paramref name="input"/>, mesh it, and write it to
        /// <paramref name="output"/>, told where the work is and able to stop
        /// it. See <see cref="ConvertMany"/> for what <paramref name="progress"/>
        /// and <paramref name="cancellationToken"/> do.</summary>
        /// <exception cref="CadConvertException">The file could not be read,
        /// was of no format this knows, or could not be written.</exception>
        /// <exception cref="OperationCanceledException">The token was
        /// cancelled.</exception>
        public static ConvertResult Convert(
            string input,
            string output,
            ConvertOptions options,
            Action<ConvertProgress> progress,
            CancellationToken cancellationToken = default)
        {
            if (output == null) throw new ArgumentNullException(nameof(output));
            return ConvertMany(input, new[] { output }, options, progress, cancellationToken);
        }

        /// <summary>Read <paramref name="input"/> once, mesh it once, and write
        /// it to every path in <paramref name="outputs"/>.</summary>
        /// <remarks>Each output's extension chooses its container, so
        /// <c>["part.glb", "part.usdz"]</c> is the usual request: one reading and
        /// one meshing where two calls would cost two. <paramref name="progress"/>
        /// is told where the work is between units, on this thread — every
        /// stage as it opens, each body as it is meshed, each file as it is
        /// written. A cancelled <paramref name="cancellationToken"/> stops the
        /// work at the next unit with <see cref="OperationCanceledException"/>;
        /// outputs written before that point are on disk. An exception thrown
        /// by <paramref name="progress"/> stops it the same way and is rethrown
        /// as itself.</remarks>
        /// <exception cref="CadConvertException">The file could not be read,
        /// was of no format this knows, or could not be written.</exception>
        /// <exception cref="OperationCanceledException">The token was
        /// cancelled.</exception>
        public static ConvertResult ConvertMany(
            string input,
            IReadOnlyList<string> outputs,
            ConvertOptions options = null,
            Action<ConvertProgress> progress = null,
            CancellationToken cancellationToken = default)
        {
            if (input == null) throw new ArgumentNullException(nameof(input));
            if (outputs == null) throw new ArgumentNullException(nameof(outputs));
            if (outputs.Count == 0) throw new ArgumentException("at least one output is required", nameof(outputs));
            for (int i = 0; i < outputs.Count; i++)
                if (outputs[i] == null) throw new ArgumentNullException($"{nameof(outputs)}[{i}]");
            options = options ?? new ConvertOptions();

            var native = new Native.Options
            {
                // Zero is the ABI's way of saying "yours, not mine".
                SagMm = options.SagMillimetres ?? 0.0,
                AngleDeg = options.AngleDegrees ?? 0.0,
                Target = (int)options.Target,
                UseParasolidTwin = options.UseParasolidTwin ? 1 : 0,
            };

            // The callback runs on this thread, inside the native call, and
            // must not throw across it: an exception is kept here, the native
            // side is told to stop, and it is rethrown once the call is back.
            ExceptionDispatchInfo failure = null;
            Native.ProgressFn callback = null;
            if (progress != null || cancellationToken.CanBeCanceled)
            {
                callback = (user, stage, done, total, detail) =>
                {
                    if (cancellationToken.IsCancellationRequested) return 1;
                    if (progress == null) return 0;
                    try
                    {
                        progress(new ConvertProgress((ConvertStage)stage, (long)done, (long)total, ReadUtf8(detail) ?? string.Empty));
                        return 0;
                    }
                    catch (Exception e)
                    {
                        failure = ExceptionDispatchInfo.Capture(e);
                        return 1;
                    }
                };
            }

            var summary = new Native.Summary();
            IntPtr message = IntPtr.Zero;
            var paths = new IntPtr[outputs.Count];
            try
            {
                for (int i = 0; i < outputs.Count; i++) paths[i] = AllocUtf8(outputs[i]);

                int code = Native.cadconvert_convert_many(
                    Utf8(input), paths, (UIntPtr)paths.Length, ref native,
                    callback, IntPtr.Zero, ref summary, ref message);
                string text = message == IntPtr.Zero ? null : ReadUtf8(message);

                if (code == Native.ErrCancelled)
                {
                    failure?.Throw();
                    throw new OperationCanceledException("the conversion was cancelled", cancellationToken);
                }
                if (code != Native.Ok)
                {
                    throw new CadConvertException(code, text ?? $"conversion failed with code {code}");
                }

                var written = new ConvertOutput[outputs.Count];
                for (int i = 0; i < outputs.Count; i++)
                {
                    written[i] = new ConvertOutput { Path = outputs[i], Bytes = new FileInfo(outputs[i]).Length };
                }
                return new ConvertResult
                {
                    OutputPath = outputs[0],
                    Bytes = (long)summary.Bytes,
                    Outputs = written,
                    Bodies = (long)summary.Bodies,
                    Faces = (long)summary.Faces,
                    FacesMeshed = (long)summary.FacesMeshed,
                    Triangles = (long)summary.Triangles,
                    Warnings = string.IsNullOrEmpty(text)
                        ? (IReadOnlyList<string>)Array.Empty<string>()
                        : text.Split('\n'),
                };
            }
            finally
            {
                // The delegate must outlive the native call that holds its
                // thunk; nothing else references it once the lambda is built.
                GC.KeepAlive(callback);
                foreach (var p in paths) if (p != IntPtr.Zero) Marshal.FreeHGlobal(p);
                // The string is the native side's allocation and comes back to
                // it whether the call succeeded or threw.
                if (message != IntPtr.Zero) Native.cadconvert_string_free(message);
            }
        }

        /// <summary>A managed string as the nul-terminated UTF-8 the ABI wants.</summary>
        private static byte[] Utf8(string text)
        {
            int n = Encoding.UTF8.GetByteCount(text);
            var bytes = new byte[n + 1];
            Encoding.UTF8.GetBytes(text, 0, text.Length, bytes, 0);
            bytes[n] = 0;
            return bytes;
        }

        /// <summary>The same, in unmanaged memory, for an array of them: a
        /// <c>char **</c> cannot be marshalled from managed strings without
        /// choosing a code page, and the code page is not UTF-8.</summary>
        private static IntPtr AllocUtf8(string text)
        {
            byte[] bytes = Utf8(text);
            IntPtr p = Marshal.AllocHGlobal(bytes.Length);
            Marshal.Copy(bytes, 0, p, bytes.Length);
            return p;
        }

        /// <summary>Read a nul-terminated UTF-8 string the native library owns.
        /// <c>Marshal.PtrToStringUTF8</c> is not in netstandard2.0, and
        /// <c>PtrToStringAnsi</c> mangles any path that is not ASCII.</summary>
        private static string ReadUtf8(IntPtr p)
        {
            if (p == IntPtr.Zero) return null;
            int length = 0;
            while (Marshal.ReadByte(p, length) != 0) length++;
            if (length == 0) return string.Empty;
            var bytes = new byte[length];
            Marshal.Copy(p, bytes, 0, length);
            return Encoding.UTF8.GetString(bytes);
        }

        private static class Native
        {
            public const int Ok = 0;
            public const int ErrCancelled = 7;
            // The file, not the package: cadconvert_native.dll,
            // libcadconvert_native.so, libcadconvert_native.dylib. The suffix
            // keeps it from colliding with this assembly, CadConvert.dll, on a
            // filesystem that does not distinguish the two.
            private const string Library = "cadconvert_native";

            [StructLayout(LayoutKind.Sequential)]
            public struct Options
            {
                public double SagMm;
                public double AngleDeg;
                public int Target;
                public int UseParasolidTwin;
            }

            [StructLayout(LayoutKind.Sequential)]
            public struct Summary
            {
                public ulong Bytes;
                public ulong Bodies;
                public ulong Faces;
                public ulong FacesMeshed;
                public ulong Triangles;
            }

            /// <summary>Return 0 to continue, anything else to stop.</summary>
            [UnmanagedFunctionPointer(CallingConvention.Cdecl)]
            public delegate int ProgressFn(IntPtr user, int stage, ulong done, ulong total, IntPtr detail);

            // Paths cross as nul-terminated UTF-8 bytes rather than as
            // strings: netstandard2.0 has no `LPUTF8Str`, and `LPStr` is the
            // ANSI code page, which turns any path that is not ASCII into a
            // file the converter cannot find.
            [DllImport(Library, CallingConvention = CallingConvention.Cdecl)]
            public static extern int cadconvert_convert_many(
                byte[] input,
                IntPtr[] outputs,
                UIntPtr outputCount,
                ref Options options,
                ProgressFn progress,
                IntPtr user,
                ref Summary summary,
                ref IntPtr message);

            [DllImport(Library, CallingConvention = CallingConvention.Cdecl)]
            public static extern void cadconvert_string_free(IntPtr text);

            [DllImport(Library, CallingConvention = CallingConvention.Cdecl)]
            public static extern IntPtr cadconvert_version();
        }
    }
}
