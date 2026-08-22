using System;
using System.Collections.Generic;
using System.IO;
using System.Runtime.InteropServices;
using System.Text;

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
        /// same materials; USD's text form spells every coordinate out, so the
        /// file runs several times the size of the equivalent glTF.</summary>
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
        /// vertex exactly where it was computed.</summary>
        /// <summary>What to write. The output path's extension wins over this:
        /// a path ending <c>.usdz</c> writes a USD package whatever is set
        /// here, and one ending <c>.glb</c> writes glTF.</summary>
        public MeshTarget Target { get; set; } = MeshTarget.Lean;

        /// <summary>Read a STEP file's <c>.x_t</c> twin, when one sits beside it,
        /// for the designer's own metal-versus-matte per face. A STEP carries a
        /// colour and nothing about finish.</summary>
        public bool UseParasolidTwin { get; set; } = true;
    }

    /// <summary>What one conversion produced.</summary>
    public sealed class ConvertResult
    {
        /// <summary>Where the mesh was written.</summary>
        public string OutputPath { get; internal set; }
        /// <summary>The size of that file.</summary>
        public long Bytes { get; internal set; }
        /// <summary>Solids read from the file.</summary>
        public long Bodies { get; internal set; }
        /// <summary>Faces the file declared.</summary>
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

    /// <summary>Parasolid and STEP to glTF binary.</summary>
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
            if (input == null) throw new ArgumentNullException(nameof(input));
            if (output == null) throw new ArgumentNullException(nameof(output));
            options = options ?? new ConvertOptions();

            var native = new Native.Options
            {
                // Zero is the ABI's way of saying "yours, not mine".
                SagMm = options.SagMillimetres ?? 0.0,
                AngleDeg = options.AngleDegrees ?? 0.0,
                Target = (int)options.Target,
                UseParasolidTwin = options.UseParasolidTwin ? 1 : 0,
            };

            var summary = new Native.Summary();
            IntPtr message = IntPtr.Zero;
            int code;
            try
            {
                code = Native.cadconvert_convert(
                    Utf8(input), Utf8(output), ref native, ref summary, ref message);
                string text = message == IntPtr.Zero ? null : ReadUtf8(message);

                if (code != Native.Ok)
                {
                    throw new CadConvertException(code, text ?? $"conversion failed with code {code}");
                }

                return new ConvertResult
                {
                    OutputPath = output,
                    Bytes = (long)summary.Bytes,
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

            // Paths cross as nul-terminated UTF-8 bytes rather than as
            // strings: netstandard2.0 has no `LPUTF8Str`, and `LPStr` is the
            // ANSI code page, which turns any path that is not ASCII into a
            // file the converter cannot find.
            [DllImport(Library, CallingConvention = CallingConvention.Cdecl)]
            public static extern int cadconvert_convert(
                byte[] input,
                byte[] output,
                ref Options options,
                ref Summary summary,
                ref IntPtr message);

            [DllImport(Library, CallingConvention = CallingConvention.Cdecl)]
            public static extern void cadconvert_string_free(IntPtr text);

            [DllImport(Library, CallingConvention = CallingConvention.Cdecl)]
            public static extern IntPtr cadconvert_version();
        }
    }
}
