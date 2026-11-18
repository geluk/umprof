using System;
using System.IO;
using System.Threading;
using static UmprofCompanion.MarkersFileSpec;

namespace UmprofCompanion;

/// <summary>
/// Writes markers that umprof shows in its profiles. Does nothing unless MONO_JIT_MAP_DIR is set.
/// </summary>
public static class Marker
{
    /// <summary>Writes an interval end marker upon being disposed.</summary>
    public struct Scope : IDisposable
    {
        private readonly long id;
        private readonly bool flush;

        internal Scope(long id, bool flush)
        {
            this.id = id;
            this.flush = flush;
        }

        /// <summary>Writes the interval end marker.</summary>
        public void Dispose()
        {
            if (id != 0)
                // End events are matched by ID, so they don't need a name
                Write(EventKind.End, id, "", flush);
        }
    }


    // will be set to null on failure
    private static StreamWriter? writer = Open();
    private static long lastId;

    private static readonly LineBuilder line = new LineBuilder(256);

    // must be held while writing to the file or the line
    private static readonly object writeLock = new object();

    static Marker()
    {
        // makes the runtime open the file on first use, rather than whenever it likes
    }

    /// <summary>
    /// True if markers will be written. Will be set to false on failure, or if the
    /// MONO_JIT_MAP_DIR environment variable is not set.</summary>
    public static bool Enabled => writer != null;

    /// <summary>
    /// Starts measuring an interval. The interval ends when the returned scope is disposed.
    /// When <paramref name="flush"/> is true, markers are written to disk immediately.
    /// This costs a small amount of performance, but it ensures that no markers are lost
    /// if the process is killed.
    /// </summary>
    /// <exception cref="ArgumentNullException"><paramref name="name"/> is null.</exception>
    public static Scope Begin(string name, bool flush = true)
    {
        if (name == null)
            throw new ArgumentNullException(nameof(name));
        if (!Enabled)
            return default;
        var id = Interlocked.Increment(ref lastId);
        Write(EventKind.Begin, id, name, flush);
        return new Scope(id, flush);
    }

    /// <summary>
    /// Marks a single moment.
    /// When <paramref name="flush"/> is true, markers are written to disk immediately.
    /// This costs a small amount of performance, but it ensures that no markers are lost
    /// if the process is killed.
    /// </summary>
    /// <exception cref="ArgumentNullException"><paramref name="name"/> is null.</exception>
    public static void Mark(string name, bool flush = true)
    {
        if (name == null)
            throw new ArgumentNullException(nameof(name));
        if (Enabled)
            Write(EventKind.Instant, 0, name, flush);
    }

    /// <summary>
    /// Writes any unwritten markers to disk. You only have to call this if you set
    /// flush to false in calls to any other method.
    /// </summary>
    public static void Flush()
    {
        lock (writeLock)
        {
            if (writer == null)
                return;
            try
            {
                writer.Flush();
            }
            catch (IOException)
            {
                writer = null;
            }
        }
    }

    private static void Write(EventKind kind, long id, string name, bool flush)
    {
        // this must stay outside the lock to report accurate timestamps!
        Kernel32.QueryPerformanceCounter(out var qpc);
        var tid = Kernel32.GetCurrentThreadId();

        lock (writeLock)
        {
            if (writer == null)
                return;
            BuildEvent(line, qpc, tid, kind, id, name);
            try
            {
                line.WriteTo(writer);
                if (flush)
                    writer.Flush();
            }
            catch (IOException)
            {
                // the safest thing we can do at this point is just stop writing markers
                writer = null;
            }
        }
    }

    private static StreamWriter? Open()
    {
        try
        {
            return TryOpen();
        }
        catch (Exception e)
        {
            // only runs once, so this exception is fine to log
            Console.Error.WriteLine("UmprofMarkers: can't write markers: " + e.Message);
            return null;
        }
    }

    private static StreamWriter? TryOpen()
    {
        // enough to handle a decent amount of lines before flushing
        const int bufferSize = 1024 * 16;

        var dir = Environment.GetEnvironmentVariable(DirectoryVariable);
        if (string.IsNullOrEmpty(dir))
            return null;

        Kernel32.QueryPerformanceFrequency(out var freq);
        var pid = Kernel32.GetCurrentProcessId();
        var path = Path.Combine(dir, FileNameFor(pid));

        var stream = new FileStream(path, FileMode.Create, FileAccess.Write, FileShare.Read);
        var result = new StreamWriter(stream, Encoding, bufferSize)
        {
            NewLine = NewLine,
        };
        result.WriteLine(HeaderFor(pid, freq));
        result.Flush();
        AppDomain.CurrentDomain.ProcessExit += (_, _) => Flush();
        return result;
    }
}
