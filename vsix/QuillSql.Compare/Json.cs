using System;
using System.Collections;
using System.Collections.Generic;
using System.Globalization;
using System.Text;

namespace QuillSql.Compare
{
    /// <summary>
    /// A small JSON reader and writer. Objects are <see cref="JsonObject"/>, arrays
    /// <see cref="List{Object}"/>, numbers <see cref="double"/>, plus string, bool and null.
    /// </summary>
    public static class Json
    {
        public static object Parse(string text)
        {
            var p = new Parser(text);
            p.SkipSpace();
            var value = p.Value();
            p.SkipSpace();
            if (p.Pos != text.Length)
            {
                throw new FormatException("Unexpected text after JSON value at " + p.Pos);
            }
            return value;
        }

        public static string Write(object value)
        {
            var sb = new StringBuilder();
            WriteValue(sb, value, 0, false);
            return sb.ToString();
        }

        public static string Indented(object value)
        {
            var sb = new StringBuilder();
            WriteValue(sb, value, 0, true);
            return sb.ToString();
        }

        private static void WriteValue(StringBuilder sb, object value, int depth, bool indent)
        {
            switch (value)
            {
                case null:
                    sb.Append("null");
                    break;
                case string s:
                    WriteString(sb, s);
                    break;
                case bool b:
                    sb.Append(b ? "true" : "false");
                    break;
                case double d:
                    sb.Append(d.ToString("R", CultureInfo.InvariantCulture));
                    break;
                case int i:
                    sb.Append(i.ToString(CultureInfo.InvariantCulture));
                    break;
                case long l:
                    sb.Append(l.ToString(CultureInfo.InvariantCulture));
                    break;
                case IDictionary<string, object> map:
                    sb.Append('{');
                    var first = true;
                    foreach (var kv in map)
                    {
                        if (!first) sb.Append(',');
                        first = false;
                        NewLine(sb, depth + 1, indent);
                        WriteString(sb, kv.Key);
                        sb.Append(indent ? ": " : ":");
                        WriteValue(sb, kv.Value, depth + 1, indent);
                    }
                    if (!first) NewLine(sb, depth, indent);
                    sb.Append('}');
                    break;
                case IEnumerable list:
                    sb.Append('[');
                    var firstItem = true;
                    foreach (var item in list)
                    {
                        if (!firstItem) sb.Append(',');
                        firstItem = false;
                        NewLine(sb, depth + 1, indent);
                        WriteValue(sb, item, depth + 1, indent);
                    }
                    if (!firstItem) NewLine(sb, depth, indent);
                    sb.Append(']');
                    break;
                default:
                    WriteString(sb, Convert.ToString(value, CultureInfo.InvariantCulture));
                    break;
            }
        }

        private static void NewLine(StringBuilder sb, int depth, bool indent)
        {
            if (indent)
            {
                sb.Append('\n').Append(' ', depth * 2);
            }
        }

        private static void WriteString(StringBuilder sb, string s)
        {
            sb.Append('"');
            foreach (var c in s)
            {
                switch (c)
                {
                    case '"': sb.Append("\\\""); break;
                    case '\\': sb.Append("\\\\"); break;
                    case '\n': sb.Append("\\n"); break;
                    case '\r': sb.Append("\\r"); break;
                    case '\t': sb.Append("\\t"); break;
                    default:
                        if (c < 0x20) sb.Append("\\u").Append(((int)c).ToString("x4"));
                        else sb.Append(c);
                        break;
                }
            }
            sb.Append('"');
        }

        private sealed class Parser
        {
            private readonly string _s;
            public int Pos;

            public Parser(string s) => _s = s;

            public void SkipSpace()
            {
                while (Pos < _s.Length && char.IsWhiteSpace(_s[Pos])) Pos++;
            }

            public object Value()
            {
                if (Pos >= _s.Length) throw new FormatException("Unexpected end of JSON");
                var c = _s[Pos];
                switch (c)
                {
                    case '{': return Object();
                    case '[': return Array();
                    case '"': return String();
                    case 't': Expect("true"); return true;
                    case 'f': Expect("false"); return false;
                    case 'n': Expect("null"); return null;
                    default: return Number();
                }
            }

            private void Expect(string word)
            {
                if (string.CompareOrdinal(_s, Pos, word, 0, word.Length) != 0) throw new FormatException("Bad JSON at " + Pos);
                Pos += word.Length;
            }

            private JsonObject Object()
            {
                var o = new JsonObject();
                Pos++;
                SkipSpace();
                if (_s[Pos] == '}') { Pos++; return o; }
                while (true)
                {
                    SkipSpace();
                    var key = String();
                    SkipSpace();
                    if (_s[Pos] != ':') throw new FormatException("Expected ':' at " + Pos);
                    Pos++;
                    SkipSpace();
                    o[key] = Value();
                    SkipSpace();
                    if (_s[Pos] == ',') { Pos++; continue; }
                    if (_s[Pos] == '}') { Pos++; return o; }
                    throw new FormatException("Expected ',' or '}' at " + Pos);
                }
            }

            private List<object> Array()
            {
                var list = new List<object>();
                Pos++;
                SkipSpace();
                if (_s[Pos] == ']') { Pos++; return list; }
                while (true)
                {
                    SkipSpace();
                    list.Add(Value());
                    SkipSpace();
                    if (_s[Pos] == ',') { Pos++; continue; }
                    if (_s[Pos] == ']') { Pos++; return list; }
                    throw new FormatException("Expected ',' or ']' at " + Pos);
                }
            }

            private string String()
            {
                if (_s[Pos] != '"') throw new FormatException("Expected a string at " + Pos);
                Pos++;
                var sb = new StringBuilder();
                while (true)
                {
                    var c = _s[Pos++];
                    if (c == '"') return sb.ToString();
                    if (c != '\\') { sb.Append(c); continue; }
                    var e = _s[Pos++];
                    switch (e)
                    {
                        case 'n': sb.Append('\n'); break;
                        case 'r': sb.Append('\r'); break;
                        case 't': sb.Append('\t'); break;
                        case 'b': sb.Append('\b'); break;
                        case 'f': sb.Append('\f'); break;
                        case 'u':
                            sb.Append((char)Convert.ToInt32(_s.Substring(Pos, 4), 16));
                            Pos += 4;
                            break;
                        default: sb.Append(e); break;
                    }
                }
            }

            private double Number()
            {
                var start = Pos;
                while (Pos < _s.Length && "+-0123456789.eE".IndexOf(_s[Pos]) >= 0) Pos++;
                if (start == Pos) throw new FormatException("Bad JSON at " + Pos);
                return double.Parse(_s.Substring(start, Pos - start), NumberStyles.Float, CultureInfo.InvariantCulture);
            }
        }
    }

    /// <summary>A JSON object with typed getters that never throw.</summary>
    public sealed class JsonObject : Dictionary<string, object>
    {
        public JsonObject() : base(StringComparer.Ordinal) { }

        public string Str(string key) => TryGetValue(key, out var v) && v != null ? Convert.ToString(v, CultureInfo.InvariantCulture) : null;

        public bool Bool(string key) => TryGetValue(key, out var v) && v is bool b && b;

        public long Long(string key) => TryGetValue(key, out var v) && v is double d ? (long)d : 0;

        public JsonObject Obj(string key) => TryGetValue(key, out var v) ? v as JsonObject : null;

        public List<object> List(string key) => TryGetValue(key, out var v) && v is List<object> l ? l : new List<object>();

        public List<string> Strings(string key)
        {
            var result = new List<string>();
            foreach (var item in List(key))
            {
                if (item != null) result.Add(Convert.ToString(item, CultureInfo.InvariantCulture));
            }
            return result;
        }

        public JsonObject Clone() => (JsonObject)Json.Parse(Json.Write(this));
    }
}
