using System.Globalization;
using System.Text;

namespace UmprofCompanion;

internal static class MarkersFileSpec
{
    public const string DirectoryVariable = "MONO_JIT_MAP_DIR";

    public static readonly Encoding Encoding = new UTF8Encoding(false);

    public const string NewLine = "\n";

    public enum EventKind
    {
        Begin = 'B',
        End = 'E',
        Instant = 'I',
    }

    public static string FileNameFor(uint pid)
    {
        return "markers-" + pid.ToString(CultureInfo.InvariantCulture) + ".txt";
    }

    public static string HeaderFor(uint pid, long qpcFrequency)
    {
        return "# umprof-markers pid=" + pid.ToString(CultureInfo.InvariantCulture)
            + " qpc-freq=" + qpcFrequency.ToString(CultureInfo.InvariantCulture);
    }

    public static void BuildEvent(LineBuilder line, long qpc, uint tid, EventKind kind, long id, string name)
    {
        line.Reset();
        line.Append(qpc);
        line.Append(' ');
        line.Append(tid);
        line.Append(' ');
        line.Append((char)kind);
        line.Append(' ');
        line.Append(id);
        if (kind != EventKind.End)
        {
            line.Append(' ');
            foreach (var c in name)
                line.Append(c is '\r' or '\n' ? ' ' : c);
        }
        line.Append('\n');
    }
}
