using System;
using System.Collections.Generic;
using Microsoft.Data.SqlClient;
using Microsoft.VisualStudio.Shell;
using Microsoft.VisualStudio.Threading;
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
-- With the description (MS_Description) people give objects, columns and parameters.
SELECT o.object_id, s.name, o.name, o.type, CONVERT(nvarchar(1000), ep.value)
FROM sys.objects o JOIN sys.schemas s ON s.schema_id = o.schema_id
LEFT JOIN sys.extended_properties ep ON ep.class = 1 AND ep.major_id = o.object_id AND ep.minor_id = 0 AND ep.name = 'MS_Description'
WHERE o.type IN ('U', 'V', 'P', 'PC', 'FN', 'FS', 'IF', 'TF', 'FT', 'SN') AND o.is_ms_shipped = 0
UNION ALL
-- The system views, procedures and functions (sp_who2, xp_fixeddrives, fn_my_permissions...).
SELECT o.object_id, s.name, o.name, o.type, NULL
FROM sys.system_objects o JOIN sys.schemas s ON s.schema_id = o.schema_id
WHERE (o.type = 'V' AND s.name IN ('sys', 'INFORMATION_SCHEMA'))
   OR (o.type IN ('P', 'X', 'PC', 'FN', 'FS', 'IF', 'TF', 'FT') AND s.name = 'sys');
SELECT c.object_id, c.name, TYPE_NAME(c.user_type_id), c.max_length, c.precision, c.scale, c.is_nullable, c.is_identity, c.is_computed, dc.definition,
    CONVERT(nvarchar(1000), ep.value)
FROM sys.columns c JOIN sys.objects o ON o.object_id = c.object_id
LEFT JOIN sys.default_constraints dc ON dc.object_id = c.default_object_id
LEFT JOIN sys.extended_properties ep ON ep.class = 1 AND ep.major_id = c.object_id AND ep.minor_id = c.column_id AND ep.name = 'MS_Description'
WHERE o.is_ms_shipped = 0 AND o.type IN ('U', 'V', 'IF', 'TF', 'FT')
ORDER BY c.object_id, c.column_id;
SELECT object_id, name, type, max_length, precision, scale, is_output, description FROM (
    SELECT p.object_id, p.parameter_id, p.name, TYPE_NAME(p.user_type_id) AS type, p.max_length, p.precision, p.scale, p.is_output,
        CONVERT(nvarchar(1000), ep.value) AS description
    FROM sys.parameters p JOIN sys.objects o ON o.object_id = p.object_id
    LEFT JOIN sys.extended_properties ep ON ep.class = 2 AND ep.major_id = p.object_id AND ep.minor_id = p.parameter_id AND ep.name = 'MS_Description'
    WHERE o.is_ms_shipped = 0 AND p.parameter_id > 0
    UNION ALL
    SELECT p.object_id, p.parameter_id, p.name, TYPE_NAME(p.user_type_id), p.max_length, p.precision, p.scale, p.is_output, NULL
    FROM sys.system_parameters p JOIN sys.system_objects o ON o.object_id = p.object_id
    WHERE o.type IN ('P', 'X', 'PC', 'FN', 'FS', 'IF', 'TF', 'FT') AND p.parameter_id > 0
) p
ORDER BY object_id, parameter_id;
SELECT fk.object_id, SCHEMA_NAME(pt.schema_id), pt.name, SCHEMA_NAME(rt.schema_id), rt.name, pc.name, rc.name
FROM sys.foreign_keys fk
JOIN sys.foreign_key_columns fkc ON fkc.constraint_object_id = fk.object_id
JOIN sys.objects pt ON pt.object_id = fk.parent_object_id
JOIN sys.objects rt ON rt.object_id = fk.referenced_object_id
JOIN sys.columns pc ON pc.object_id = fkc.parent_object_id AND pc.column_id = fkc.parent_column_id
JOIN sys.columns rc ON rc.object_id = fkc.referenced_object_id AND rc.column_id = fkc.referenced_column_id
ORDER BY fk.object_id, fkc.constraint_column_id;
-- Azure SQL Database has no sys.servers (nor linked servers): an empty list there.
IF OBJECT_ID('sys.servers') IS NOT NULL EXEC('SELECT name FROM sys.servers WHERE is_linked = 1 ORDER BY name');
ELSE SELECT CAST(name AS sysname) AS name FROM sys.objects WHERE 1 = 0;";

        /// <summary>The databases on each connection's server, by connection key, once its metadata is read.</summary>
        public static readonly System.Collections.Concurrent.ConcurrentDictionary<string, List<string>> Databases =
            new System.Collections.Concurrent.ConcurrentDictionary<string, List<string>>();

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
            public string Description;
            public readonly List<string> Columns = new List<string>();
            public readonly List<string> Params = new List<string>();
        }

        internal static string KindOf(string type)
        {
            switch (type.Trim())
            {
                case "U": return "table";
                case "V": return "view";
                case "P": case "PC": case "X": return "procedure";
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
            var linked = new List<string>();
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
                        objects[r.GetInt32(0)] = new DbObject { Schema = r.GetString(1), Name = r.GetString(2), Kind = KindOf(r.GetString(3)), Description = r.IsDBNull(4) ? null : r.GetString(4) };
                    }
                    r.NextResult();
                    while (r.Read())
                    {
                        if (objects.TryGetValue(r.GetInt32(0), out var o) && !r.IsDBNull(1))
                        {
                            var type = r.IsDBNull(2) ? "" : TypeText(r.GetString(2), r.GetInt16(3), r.GetByte(4), r.GetByte(5));
                            o.Columns.Add("{\"name\":" + Json.Str(r.GetString(1)) + ",\"type\":" + Json.Str(type)
                                + ",\"nullable\":" + Bool(r.GetBoolean(6)) + ",\"identity\":" + Bool(r.GetBoolean(7)) + ",\"computed\":" + Bool(r.GetBoolean(8))
                                + (r.IsDBNull(9) ? "" : ",\"default\":" + Json.Str(r.GetString(9)))
                                + (r.IsDBNull(10) ? "" : ",\"description\":" + Json.Str(r.GetString(10))) + "}");
                        }
                    }
                    r.NextResult();
                    while (r.Read())
                    {
                        if (objects.TryGetValue(r.GetInt32(0), out var o) && !r.IsDBNull(1))
                        {
                            var type = r.IsDBNull(2) ? "" : TypeText(r.GetString(2), r.GetInt16(3), r.GetByte(4), r.GetByte(5));
                            o.Params.Add("{\"name\":" + Json.Str(r.GetString(1)) + ",\"type\":" + Json.Str(type) + ",\"output\":" + Bool(r.GetBoolean(6))
                                + (r.IsDBNull(7) ? "" : ",\"description\":" + Json.Str(r.GetString(7))) + "}");
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
                    r.NextResult();
                    while (r.Read()) linked.Add(r.GetString(0));
                }
            }

            Databases[target.Key] = databases;
            Log.Write("metadata query: connected in " + opened + " ms, first rows at " + executed + " ms, read all by " + watch.ElapsedMilliseconds + " ms");
            var sb = new StringBuilder();
            sb.Append("{\"database\":").Append(Json.Str(database))
              .Append(",\"defaultSchema\":").Append(Json.Str(defaultSchema))
              .Append(",\"schemas\":[").Append(string.Join(",", schemas.ConvertAll(Json.Str))).Append(']')
              .Append(",\"databases\":[").Append(string.Join(",", databases.ConvertAll(Json.Str))).Append(']')
              .Append(",\"linkedServers\":[").Append(string.Join(",", linked.ConvertAll(Json.Str))).Append(']')
              .Append(",\"objects\":[");
            var first = true;
            foreach (var o in objects.Values)
            {
                if (!first) sb.Append(',');
                first = false;
                sb.Append("{\"schema\":").Append(Json.Str(o.Schema))
                  .Append(",\"name\":").Append(Json.Str(o.Name))
                  .Append(",\"kind\":").Append(Json.Str(o.Kind))
                  .Append(o.Description == null ? "" : ",\"description\":" + Json.Str(o.Description))
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

    /// <summary>
    /// Loads each connection's metadata in the background, and loads it again when the objects of the
    /// database change: checked at most once a minute while suggestions are asked for, or at once on
    /// Ctrl+Space. The metadata already loaded keeps serving until the new one is in.
    /// </summary>
    internal static class MetadataCache
    {
        private const string StampQuery = @"SELECT ISNULL(CONVERT(varchar(33), MAX(modify_date), 126), '') + '|' + CONVERT(varchar(12), COUNT(*))
FROM sys.objects WHERE is_ms_shipped = 0;";
        private static readonly TimeSpan CheckEvery = TimeSpan.FromSeconds(60);
        private static readonly object Gate = new object();
        private static readonly Dictionary<string, string> State = new Dictionary<string, string>();
        private static readonly Dictionary<string, string> Stamps = new Dictionary<string, string>();
        private static readonly Dictionary<string, DateTime> Checked = new Dictionary<string, DateTime>();
        private static readonly HashSet<string> Checking = new HashSet<string>();

        /// <summary>Starts loading if needed; returns true once the engine has the metadata.</summary>
        public static bool EnsureLoaded(ConnectionTarget target, bool checkNow = false)
        {
            if (!target.CanQuery)
            {
                return false;
            }
            lock (Gate)
            {
                if (State.TryGetValue(target.Key, out var state))
                {
                    if (state == "loaded")
                    {
                        if (!Checking.Contains(target.Key) && (checkNow || !Checked.TryGetValue(target.Key, out var at) || DateTime.UtcNow - at > CheckEvery))
                        {
                            Checking.Add(target.Key);
                            Checked[target.Key] = DateTime.UtcNow;
                            _ = System.Threading.Tasks.Task.Run(() => CheckForChanges(target));
                        }
                        return true;
                    }
                    // After a failure, wait before trying the server again; a login that may prompt to sign
                    // in waits for Refresh Suggestions.
                    if (target.Interactive || !(state.StartsWith("failed:") && DateTime.UtcNow.Ticks - long.Parse(state.Substring(7)) > TimeSpan.FromSeconds(30).Ticks))
                    {
                        return false;
                    }
                }
                State[target.Key] = "loading";
            }
            StartLoad(target, false);
            return false;
        }

        /// <summary>True while the target's metadata is being read.</summary>
        public static bool IsLoading(ConnectionTarget target)
        {
            lock (Gate)
            {
                return target != null && State.TryGetValue(target.Key, out var state) && state == "loading";
            }
        }

        /// <summary>
        /// Reads the target's metadata in the background, the status bar saying that it is reading
        /// and then how it went (how many objects, how long), so a new window shows when its
        /// suggestions are ready.
        /// </summary>
        private static void StartLoad(ConnectionTarget target, bool refresh)
        {
            _ = ThreadHelper.JoinableTaskFactory.RunAsync(async () =>
            {
                await ThreadHelper.JoinableTaskFactory.SwitchToMainThreadAsync();
                var dte = EditorContext.Dte;
                try
                {
                    dte?.StatusBar.Animate(true, EnvDTE.vsStatusAnimation.vsStatusAnimationSync);
                }
                catch (System.Runtime.InteropServices.COMException)
                {
                }
                EditorContext.Status((refresh ? "refreshing" : "reading") + " suggestions for " + target.Description + "...");
                await System.Threading.Tasks.TaskScheduler.Default;
                var watch = System.Diagnostics.Stopwatch.StartNew();
                var loaded = Load(target, out var count, out var problem);
                lock (Gate)
                {
                    State[target.Key] = loaded ? "loaded" : "failed:" + DateTime.UtcNow.Ticks;
                    Checked[target.Key] = DateTime.UtcNow;
                }
                await ThreadHelper.JoinableTaskFactory.SwitchToMainThreadAsync();
                try
                {
                    dte?.StatusBar.Animate(false, EnvDTE.vsStatusAnimation.vsStatusAnimationSync);
                }
                catch (System.Runtime.InteropServices.COMException)
                {
                }
                var seconds = (watch.ElapsedMilliseconds / 1000.0).ToString("0.0", System.Globalization.CultureInfo.InvariantCulture);
                EditorContext.Status(loaded
                    ? "suggestions " + (refresh ? "refreshed" : "ready") + " for " + target.Description + ": " + count + " objects in " + seconds + " s."
                    : "suggestions could not be " + (refresh ? "refreshed" : "read") + " for " + target.Description + ": " + problem);
            });
        }

        private static bool Load(ConnectionTarget target) => Load(target, out _, out _);

        /// <summary>Reads the metadata and hands it to the engine; how many objects, or why not.</summary>
        private static bool Load(ConnectionTarget target, out int count, out string problem)
        {
            count = 0;
            problem = null;
            try
            {
                var started = DateTime.Now;
                // Read before the metadata, so a change made while it loads is seen by the next check.
                var stamp = Stamp(target);
                var schema = MetadataLoader.Load(target, out count);
                var loadedAt = DateTime.Now;
                var reply = EngineProcess.Shared.Request("load", "\"key\":" + Json.Str(target.Key) + ",\"schema\":" + schema);
                var error = reply == null ? "no reply" : Json.Parse<ErrorOnlyReply>(reply)?.Error;
                if (error != null)
                {
                    Log.Write("engine rejected metadata: " + error);
                    problem = "the engine did not take the metadata (" + error + ")";
                    return false;
                }
                lock (Gate)
                {
                    Stamps[target.Key] = stamp;
                }
                Log.Write("metadata for " + target.Description + ": " + count + " objects in " + (int)(DateTime.Now - started).TotalMilliseconds + " ms (engine took " + (int)(DateTime.Now - loadedAt).TotalMilliseconds + " ms)");
                return true;
            }
            catch (Exception ex)
            {
                Log.Error("loading metadata for " + target.Description, ex);
                problem = ex.Message;
                return false;
            }
        }

        /// <summary>
        /// Refresh Suggestions: reads the window's database again now, with the status bar saying
        /// it is reading and then how it went (how many objects, how long).
        /// </summary>
        public static void Refresh(ConnectionTarget target)
        {
            ThreadHelper.ThrowIfNotOnUIThread();
            lock (Gate)
            {
                State[target.Key] = "loading";
            }
            StartLoad(target, true);
        }

        private static void CheckForChanges(ConnectionTarget target)
        {
            try
            {
                var stamp = Stamp(target);
                string known;
                lock (Gate)
                {
                    Stamps.TryGetValue(target.Key, out known);
                }
                if (stamp != known)
                {
                    Log.Write("objects changed on " + target.Description + ": reloading suggestions");
                    Load(target);
                }
            }
            catch (Exception ex)
            {
                Log.Verbose("checking " + target.Description + " for changes: " + ex.Message);
            }
            finally
            {
                lock (Gate)
                {
                    Checking.Remove(target.Key);
                }
            }
        }

        /// <summary>The latest change and the number of the database's objects: any CREATE, ALTER or DROP moves it.</summary>
        private static string Stamp(ConnectionTarget target)
        {
            using (var connection = target.CreateConnection())
            using (var command = new SqlCommand(StampQuery, connection) { CommandTimeout = 15 })
            {
                connection.Open();
                return command.ExecuteScalar() as string;
            }
        }

        public static void Clear()
        {
            lock (Gate)
            {
                State.Clear();
                Stamps.Clear();
                Checked.Clear();
            }
        }
    }
}
