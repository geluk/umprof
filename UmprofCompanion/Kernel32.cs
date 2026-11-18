using System.Runtime.InteropServices;

namespace UmprofCompanion;

internal static class Kernel32
{
    [DllImport("kernel32.dll")]
    internal static extern bool QueryPerformanceCounter(out long value);

    [DllImport("kernel32.dll")]
    internal static extern bool QueryPerformanceFrequency(out long value);

    [DllImport("kernel32.dll")]
    internal static extern uint GetCurrentThreadId();

    [DllImport("kernel32.dll")]
    internal static extern uint GetCurrentProcessId();
}
