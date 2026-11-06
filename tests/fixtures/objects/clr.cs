// Source of clr.hex, the CLR assembly the objects live tests deploy (built with
// csc /target:library /out:QuillClr.dll clr.cs, then written out as hex; clr2.hex is the same
// with "Hi " and value + 2).
using System;
using System.Data.SqlTypes;
using System.IO;
using Microsoft.SqlServer.Server;

public static class QuillFunctions
{
    [SqlFunction(IsDeterministic = true)]
    public static SqlInt32 AddOne(SqlInt32 value) { return value.IsNull ? SqlInt32.Null : value + 1; }

    [SqlProcedure]
    public static void Hello(SqlString name) { SqlContext.Pipe.Send("Hello " + name.Value); }

    [SqlTrigger]
    public static void Audit() { }
}

[Serializable]
[SqlUserDefinedAggregate(Format.Native)]
public struct CountAll
{
    private int count;
    public void Init() { count = 0; }
    public void Accumulate(SqlInt32 value) { count++; }
    public void Merge(CountAll other) { count += other.count; }
    public SqlInt32 Terminate() { return count; }
}

[Serializable]
[SqlUserDefinedType(Format.Native)]
public struct Point : INullable
{
    private bool isNull;
    private int x;
    private int y;
    public bool IsNull { get { return isNull; } }
    public static Point Null { get { Point p = new Point(); p.isNull = true; return p; } }
    public override string ToString() { return isNull ? "NULL" : x + "," + y; }
    public static Point Parse(SqlString s)
    {
        if (s.IsNull) return Null;
        string[] parts = s.Value.Split(',');
        Point p = new Point();
        p.x = int.Parse(parts[0]);
        p.y = int.Parse(parts[1]);
        return p;
    }
}
