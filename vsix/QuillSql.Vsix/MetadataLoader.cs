using System;
using System.Collections.Generic;
using Microsoft.Data.SqlClient;
using System.Text;

namespace QuillSql.Vsix
{
    /// <summary>Reads schema metadata for suggestions and serializes it for the engine.</summary>
    internal static class MetadataLoader
    {
        private const string Query = @"
SET NOCOUNT ON;
SELECT DB_NAME(), SCHEMA_NAME();
SELECT name FROM sys.schemas WHERE schema_id < 16384 ORDER BY name;
-- Not HAS_DBACCESS: it opens every AUTO_CLOSE database, which takes minutes on servers with many.
SELECT name FROM sys.databases WHERE state = 0 ORDER BY name;
SELECT o.object_id, s.name, o.name, o.type
FROM sys.objects o JOIN sys.schemas s ON s.schema_id = o.schema_id
WHERE o.type IN ('U', 'V', 'P', 'PC', 'FN', 'FS', 'IF', 'TF', 'FT', 'SN') AND o.is_ms_shipped = 0
UNION ALL
SELECT o.object_id, s.name, o.name, o.type
FROM sys.system_objects o JOIN sys.schemas s ON s.schema_id = o.schema_id
WHERE o.type = 'V' AND s.name IN ('sys', 'INFORMATION_SCHEMA');
SELECT c.object_id, c.name, TYPE_NAME(c.user_type_id), c.max_length, c.precision, c.scale, c.is_nullable, c.is_identity, c.is_computed
FROM sys.columns c JOIN sys.objects o ON o.object_id = c.object_id
WHERE o.is_ms_shipped = 0 AND o.type IN ('U', 'V', 'IF', 'TF', 'FT')
ORDER BY c.object_id, c.column_id;
SELECT p.object_id, p.name, TYPE_NAME(p.user_type_id), p.max_length, p.precision, p.scale, p.is_output
FROM sys.parameters p JOIN sys.objects o ON o.object_id = p.object_id
WHERE o.is_ms_shipped = 0 AND p.parameter_id > 0
ORDER BY p.object_id, p.parameter_id;
SELECT fk.object_id, SCHEMA_NAME(pt.schema_id), pt.name, SCHEMA_NAME(rt.schema_id), rt.name, pc.name, rc.name
FROM sys.foreign_keys fk
JOIN sys.foreign_key_columns fkc ON fkc.constraint_object_id = fk.object_id
JOIN sys.objects pt ON pt.object_id = fk.parent_object_id
JOIN sys.objects rt ON rt.object_id = fk.referenced_object_id
JOIN sys.columns pc ON pc.object_id = fkc.parent_object_id AND pc.column_id = fkc.parent_column_id
JOIN sys.columns rc ON rc.object_id = fkc.referenced_object_id AND rc.column_id = fkc.referenced_column_id
ORDER BY fk.object_id, fkc.constraint_column_id;";

        private sealed class ForeignKey
        {
            public string Schema, Table, RefSchema, RefTable;
            public readonly List<string> Columns = new List<string>();
            public readonly List<string> RefColumns = new List<string>();
        }

        private sealed class DbObject
        {
            public string Schema;
            public string Name;
            public string Kind;
            public readonly List<string> Columns = new List<string>();
            public readonly List<string> Params = new List<string>();
        }

        internal static string KindOf(string type)
        {
            switch (type.Trim())
            {
                case "U": return "table";
                case "V": return "view";
                case "P": case "PC": return "procedure";
                case "FN": case "FS": return "scalar_function";
                case "IF": case "TF": case "FT": return "table_function";
                case "SN": return "synonym";
                default: return "object";
            }
        }

        internal static string TypeText(string name, short maxLength, byte precision, byte scale)
        {
            switch (name)
            {
                case "varchar": case "char": case "varbinary": case "binary":
                    return name + "(" + (maxLength == -1 ? "max" : maxLength.ToString()) + ")";
                case "nvarchar": case "nchar":
                    return name + "(" + (maxLength == -1 ? "max" : (maxLength / 2).ToString()) + ")";
                case "decimal": case "numeric":
                    return name + "(" + precision + "," + scale + ")";
                default:
                    return name;
            }
        }

        private static string Bool(bool value) => value ? "true" : "false";

        /// <summary>Returns the engine's schema JSON and the number of objects read.</summary>
        public static string Load(ConnectionTarget target, out int objectCount)
        {
            var objects = new Dictionary<int, DbObject>();
            var keys = new Dictionary<int, ForeignKey>();
            var schemas = new List<string>();
            var databases = new List<string>();
            string database = "", defaultSchema = "dbo";

            var watch = System.Diagnostics.Stopwatch.StartNew();
            long opened, executed;
            using (var connection = target.CreateConnection())
            using (var command = new SqlCommand(Query, connection) { CommandTimeout = 60 })
            {
                connection.Open();
                opened = watch.ElapsedMilliseconds;
                using (var r = command.ExecuteReader())
                {
                    executed = watch.ElapsedMilliseconds;
                    if (r.Read())
                    {
                        database = r.IsDBNull(0) ? "" : r.GetString(0);
                        defaultSchema = r.IsDBNull(1) ? "dbo" : r.GetString(1);
                    }
                    r.NextResult();
                    while (r.Read()) schemas.Add(r.GetString(0));
                    r.NextResult();
                    while (r.Read()) databases.Add(r.GetString(0));
                    r.NextResult();
                    while (r.Read())
                    {
                        objects[r.GetInt32(0)] = new DbObject { Schema = r.GetString(1), Name = r.GetString(2), Kind = KindOf(r.GetString(3)) };
                    }
                    r.NextResult();
                    while (r.Read())
                    {
                        if (objects.TryGetValue(r.GetInt32(0), out var o) && !r.IsDBNull(1))
                        {
                            var type = r.IsDBNull(2) ? "" : TypeText(r.GetString(2), r.GetInt16(3), r.GetByte(4), r.GetByte(5));
                            o.Columns.Add("{\"name\":" + Json.Str(r.GetString(1)) + ",\"type\":" + Json.Str(type)
                                + ",\"nullable\":" + Bool(r.GetBoolean(6)) + ",\"identity\":" + Bool(r.GetBoolean(7)) + ",\"computed\":" + Bool(r.GetBoolean(8)) + "}");
                        }
                    }
                    r.NextResult();
                    while (r.Read())
                    {
                        if (objects.TryGetValue(r.GetInt32(0), out var o) && !r.IsDBNull(1))
                        {
                            var type = r.IsDBNull(2) ? "" : TypeText(r.GetString(2), r.GetInt16(3), r.GetByte(4), r.GetByte(5));
                            o.Params.Add("{\"name\":" + Json.Str(r.GetString(1)) + ",\"type\":" + Json.Str(type) + ",\"output\":" + Bool(r.GetBoolean(6)) + "}");
                        }
                    }
                    r.NextResult();
                    while (r.Read())
                    {
                        var id = r.GetInt32(0);
                        if (!keys.TryGetValue(id, out var key))
                        {
                            keys[id] = key = new ForeignKey { Schema = r.GetString(1), Table = r.GetString(2), RefSchema = r.GetString(3), RefTable = r.GetString(4) };
                        }
                        key.Columns.Add(Json.Str(r.GetString(5)));
                        key.RefColumns.Add(Json.Str(r.GetString(6)));
                    }
                }
            }

            Log.Write("metadata query: connected in " + opened + " ms, first rows at " + executed + " ms, read all by " + watch.ElapsedMilliseconds + " ms");
            var sb = new StringBuilder();
            sb.Append("{\"database\":").Append(Json.Str(database))
              .Append(",\"defaultSchema\":").Append(Json.Str(defaultSchema))
              .Append(",\"schemas\":[").Append(string.Join(",", schemas.ConvertAll(Json.Str))).Append(']')
              .Append(",\"databases\":[").Append(string.Join(",", databases.ConvertAll(Json.Str))).Append(']')
              .Append(",\"objects\":[");
            var first = true;
            foreach (var o in objects.Values)
            {
                if (!first) sb.Append(',');
                first = false;
                sb.Append("{\"schema\":").Append(Json.Str(o.Schema))
                  .Append(",\"name\":").Append(Json.Str(o.Name))
                  .Append(",\"kind\":").Append(Json.Str(o.Kind))
                  .Append(",\"columns\":[").Append(string.Join(",", o.Columns)).Append(']')
                  .Append(",\"params\":[").Append(string.Join(",", o.Params)).Append("]}");
            }
            sb.Append("],\"foreignKeys\":[");
            first = true;
            foreach (var k in keys.Values)
            {
                if (!first) sb.Append(',');
                first = false;
                sb.Append("{\"schema\":").Append(Json.Str(k.Schema))
                  .Append(",\"table\":").Append(Json.Str(k.Table))
                  .Append(",\"refSchema\":").Append(Json.Str(k.RefSchema))
                  .Append(",\"refTable\":").Append(Json.Str(k.RefTable))
                  .Append(",\"columns\":[").Append(string.Join(",", k.Columns)).Append(']')
                  .Append(",\"refColumns\":[").Append(string.Join(",", k.RefColumns)).Append("]}");
            }
            sb.Append("]}");
            objectCount = objects.Count;
            return sb.ToString();
        }
    }

    /// <summary>Loads each connection's metadata once, in the background.</summary>
    internal static class MetadataCache
    {
        private static readonly object Gate = new object();
        private static readonly Dictionary<string, string> State = new Dictionary<string, string>();

        /// <summary>Starts loading if needed; returns true once the engine has the metadata.</summary>
        public static bool EnsureLoaded(ConnectionTarget target)
        {
            if (!target.CanQuery)
            {
                return false;
            }
            lock (Gate)
            {
                if (State.TryGetValue(target.Key, out var state))
                {
                    // After a failure, wait before trying the server again.
                    if (!(state.StartsWith("failed:") && DateTime.UtcNow.Ticks - long.Parse(state.Substring(7)) > TimeSpan.FromSeconds(30).Ticks))
                    {
                        return state == "loaded";
                    }
                }
                State[target.Key] = "loading";
            }
            _ = System.Threading.Tasks.Task.Run(() =>
            {
                var result = "failed";
                try
                {
                    var started = DateTime.Now;
                    var schema = MetadataLoader.Load(target, out var count);
                    var loadedAt = DateTime.Now;
                    var reply = EngineProcess.Shared.Request("load", "\"key\":" + Json.Str(target.Key) + ",\"schema\":" + schema);
                    var error = reply == null ? "no reply" : Json.Parse<ErrorOnlyReply>(reply)?.Error;
                    if (error == null)
                    {
                        result = "loaded";
                        Log.Write("metadata for " + target.Description + ": " + count + " objects in " + (int)(DateTime.Now - started).TotalMilliseconds + " ms (engine took " + (int)(DateTime.Now - loadedAt).TotalMilliseconds + " ms)");
                    }
                    else
                    {
                        Log.Write("engine rejected metadata: " + error);
                    }
                }
                catch (Exception ex)
                {
                    Log.Error("loading metadata for " + target.Description, ex);
                }
                lock (Gate)
                {
                    State[target.Key] = result == "loaded" ? result : "failed:" + DateTime.UtcNow.Ticks;
                }
            });
            return false;
        }

        public static void Clear()
        {
            lock (Gate)
            {
                State.Clear();
            }
        }
    }
}
