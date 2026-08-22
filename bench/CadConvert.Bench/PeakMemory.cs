using System.Diagnostics;
using System.Runtime.InteropServices;

namespace CadConvert.Bench;

/// <summary>The largest resident set this process has reached, in bytes.</summary>
/// <remarks>
/// This is what a host sees as the converter's memory, and .NET has no one way
/// to ask for it: <c>Process.PeakWorkingSet64</c> is populated on Windows and
/// reads back zero on macOS. Unix answers through <c>getrusage</c>, which is
/// the same figure <c>/usr/bin/time -l</c> prints — in bytes on macOS and in
/// kilobytes on Linux, a difference the manual pages state and this accounts
/// for.
/// </remarks>
internal static class PeakMemory
{
    public static long Bytes()
    {
        if (RuntimeInformation.IsOSPlatform(OSPlatform.Windows))
        {
            var self = Process.GetCurrentProcess();
            self.Refresh();
            return self.PeakWorkingSet64;
        }

        try
        {
            if (getrusage(RUSAGE_SELF, out var usage) == 0)
            {
                return RuntimeInformation.IsOSPlatform(OSPlatform.OSX)
                    ? usage.ru_maxrss
                    : usage.ru_maxrss * 1024;
            }
        }
        catch (DllNotFoundException) { /* fall through */ }
        catch (EntryPointNotFoundException) { /* fall through */ }

        return 0;
    }

    private const int RUSAGE_SELF = 0;

    [DllImport("libc", SetLastError = true)]
    private static extern int getrusage(int who, out RUsage usage);

    /// <summary>The head of <c>struct rusage</c>. Only the first three fields
    /// are read; the rest of the structure is reserved so the marshaller
    /// allocates enough for what the kernel writes.</summary>
    [StructLayout(LayoutKind.Sequential)]
    private struct RUsage
    {
        public long ru_utime_sec;
        public long ru_utime_usec;
        public long ru_stime_sec;
        public long ru_stime_usec;
        public long ru_maxrss;
        private readonly long _ixrss;
        private readonly long _idrss;
        private readonly long _isrss;
        private readonly long _minflt;
        private readonly long _majflt;
        private readonly long _nswap;
        private readonly long _inblock;
        private readonly long _oublock;
        private readonly long _msgsnd;
        private readonly long _msgrcv;
        private readonly long _nsignals;
        private readonly long _nvcsw;
        private readonly long _nivcsw;
    }
}
