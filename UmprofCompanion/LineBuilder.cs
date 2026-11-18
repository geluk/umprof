using System;
using System.IO;

namespace UmprofCompanion;

/// <summary>
/// An allocation-free line builder, as a lightweight alternative to StringBuilder.
/// </summary>
internal sealed class LineBuilder
{
    private char[] buffer;
    private int length;

    public LineBuilder(int capacity)
    {
        buffer = new char[capacity];
    }

    public void Reset()
    {
        length = 0;
    }

    public void Append(char c)
    {
        Reserve(1);
        buffer[length++] = c;
    }

    public void Append(long value)
    {
        var digits = 1;
        for (var rest = value / 10; rest != 0; rest /= 10)
            digits++;
        Reserve(digits);
        length += digits;
        for (var i = length - 1; i >= length - digits; i--)
        {
            buffer[i] = (char)('0' + value % 10);
            value /= 10;
        }
    }

    public void WriteTo(TextWriter writer)
    {
        writer.Write(buffer, 0, length);
    }

    private void Reserve(int count)
    {
        if (length + count > buffer.Length)
            Array.Resize(ref buffer, Math.Max(buffer.Length * 2, length + count));
    }
}
