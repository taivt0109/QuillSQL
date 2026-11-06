using System;
using System.Collections.Generic;
using System.Globalization;
using System.Linq;
using System.Text;
using System.Windows;
using System.Windows.Controls;
using System.Windows.Media;
using Microsoft.Data.SqlClient;
using Microsoft.VisualStudio.Shell;
using Microsoft.VisualStudio.TextManager.Interop;

namespace QuillSql.Vsix
{
    internal sealed class ColumnInfo
    {
        public string Name, Type, Identity, Computed, DefaultName, DefaultDefinition, Collation;
        public bool Nullable, Persisted, PrimaryKey;
        public string References;
    }

    internal sealed class IndexInfo
    {
        public string Name, Kind, Keys, Includes, Filter, Filegroup, Scheme, Column;
        public bool PrimaryKey, Unique, UniqueConstraint;
    }

    internal sealed class PartitionInfo
    {
        public string Function, Scheme, Column, ParameterType, Range;
        public readonly List<string> Boundaries = new List<string>();
        public readonly List<string> Filegroups = new List<string>();
        public readonly List<long> Rows = new List<long>();
    }

    /// <summary>What the object card and the scripts show about one object, read from the catalog.</summary>
    internal sealed class ObjectDetails
    {
        public string Schema, Name, TypeCode, Definition;
        public long? Rows;
        public long? SizeKb;
        public DateTime Created, Modified;
        public readonly List<ColumnInfo> Columns = new List<ColumnInfo>();
        public readonly List<IndexInfo> Indexes = new List<IndexInfo>();
        public readonly List<string[]> ForeignKeys = new List<string[]>();
        public readonly List<string[]> Checks = new List<string[]>();
        public readonly List<string> Parameters = new List<string>();
        public PartitionInfo Partitioning;
        public string Filegroup;

        public bool IsTable => TypeCode == "U";
        public string Kind => MetadataLoader.KindOf(TypeCode).Replace('_', ' ');
        public string FullName => Schema + "." + Name;

        /// <summary>The details as the engine's `table_script` expects them.</summary>
        public string ToEngineJson()
        {
            string str(string v) => v == null ? "null" : Json.Str(v);
            string storage(string fg, string scheme, string column) =>
                "{\"filegroup\":" + str(fg) + ",\"scheme\":" + str(scheme) + ",\"column\":" + str(column) + "}";
            var sb = new StringBuilder("{\"schema\":" + str(Schema) + ",\"name\":" + str(Name) + ",\"columns\":[");
            sb.Append(string.Join(",", Columns.Select(c =>
                "{\"name\":" + str(c.Name) + ",\"dataType\":" + str(c.Type) + ",\"nullable\":" + Json.Bool(c.Nullable)
                + (c.Identity != null ? ",\"identitySeed\":" + str(c.Identity.Split(',')[0]) + ",\"identityIncrement\":" + str(c.Identity.Split(',')[1]) : "")
                + (c.Computed != null ? ",\"computed\":" + str(c.Computed) + ",\"persisted\":" + Json.Bool(c.Persisted) : "")
                + (c.DefaultDefinition != null ? ",\"defaultName\":" + str(c.DefaultName) + ",\"defaultDefinition\":" + str(c.DefaultDefinition) : "")
                + (c.Collation != null ? ",\"collation\":" + str(c.Collation) : "") + "}")));
            sb.Append("],\"indexes\":[");
            sb.Append(string.Join(",", Indexes.Where(i => i.Kind != "HEAP").Select(i =>
                "{\"name\":" + str(i.Name) + ",\"kind\":" + str(i.Kind) + ",\"primaryKey\":" + Json.Bool(i.PrimaryKey) + ",\"unique\":" + Json.Bool(i.Unique)
                + ",\"uniqueConstraint\":" + Json.Bool(i.UniqueConstraint) + ",\"keys\":" + str(i.Keys ?? "") + ",\"includes\":" + str(i.Includes ?? "")
                + ",\"filter\":" + str(i.Filter) + ",\"storage\":" + storage(i.Filegroup, i.Scheme, i.Column) + "}")));
            sb.Append("],\"foreignKeys\":[");
            sb.Append(string.Join(",", ForeignKeys.Select(f =>
                "{\"name\":" + str(f[0]) + ",\"columns\":" + str(f[1]) + ",\"refSchema\":" + str(f[2]) + ",\"refTable\":" + str(f[3])
                + ",\"refColumns\":" + str(f[4]) + ",\"onDelete\":" + str(f[5]) + ",\"onUpdate\":" + str(f[6]) + "}")));
            sb.Append("],\"checks\":[");
            sb.Append(string.Join(",", Checks.Select(c => "{\"name\":" + str(c[0]) + ",\"definition\":" + str(c[1]) + "}")));
            sb.Append("],\"storage\":");
            var main = Indexes.FirstOrDefault(i => i.Kind == "HEAP" || i.Kind == "CLUSTERED" || i.Kind == "CLUSTERED COLUMNSTORE");
            sb.Append(storage(main?.Filegroup ?? Filegroup, main?.Scheme, main?.Column));
            if (Partitioning != null)
            {
                var p = Partitioning;
                sb.Append(",\"partitioning\":{\"function\":" + str(p.Function) + ",\"scheme\":" + str(p.Scheme) + ",\"column\":" + str(p.Column)
                    + ",\"parameterType\":" + str(p.ParameterType) + ",\"range\":" + str(p.Range)
                    + ",\"boundaries\":[" + string.Join(",", p.Boundaries.Select(Json.Str)) + "]"
                    + ",\"filegroups\":[" + string.Join(",", p.Filegroups.Select(Json.Str)) + "]"
                    + ",\"rows\":[" + string.Join(",", p.Rows) + "]}");
            }
            return sb.Append('}').ToString();
        }
    }

    /// <summary>Object details from the database, object cards, and scripts of objects.</summary>
    internal static class ObjectScripts
    {
        private static readonly Dictionary<string, Tuple<DateTime, ObjectDetails>> Cache = new Dictionary<string, Tuple<DateTime, ObjectDetails>>();

        private const string Query = @"
SET NOCOUNT ON;
DECLARE @id int = OBJECT_ID(@name);
DECLARE @ps int = (SELECT TOP (1) i.data_space_id FROM sys.indexes AS i JOIN sys.partition_schemes AS s ON s.data_space_id = i.data_space_id
                   WHERE i.object_id = @id AND i.index_id IN (0, 1));
DECLARE @pf int = (SELECT function_id FROM sys.partition_schemes WHERE data_space_id = @ps);
-- The partition stats give the size cheaply on large tables, when the login may read them.
DECLARE @kb bigint;
IF HAS_PERMS_BY_NAME(NULL, 'DATABASE', 'VIEW DATABASE STATE') = 1
    SET @kb = (SELECT SUM(reserved_page_count) * 8 FROM sys.dm_db_partition_stats WHERE object_id = @id);
IF @kb IS NULL
    SET @kb = (SELECT SUM(a.total_pages) * 8 FROM sys.partitions AS p JOIN sys.allocation_units AS a ON a.container_id = p.partition_id WHERE p.object_id = @id);
SELECT o.type, SCHEMA_NAME(o.schema_id), o.name, OBJECT_DEFINITION(@id), o.create_date, o.modify_date,
    (SELECT SUM(p.rows) FROM sys.partitions AS p WHERE p.object_id = @id AND p.index_id IN (0, 1)),
    @kb
FROM sys.objects AS o WHERE o.object_id = @id;
SELECT c.name, TYPE_NAME(c.user_type_id), c.max_length, c.precision, c.scale, c.is_nullable,
    CASE WHEN ic.object_id IS NULL THEN NULL ELSE CONCAT(CAST(ic.seed_value AS nvarchar(40)), ',', CAST(ic.increment_value AS nvarchar(40))) END,
    cc.definition, ISNULL(cc.is_persisted, 0), dc.name, dc.definition,
    CASE WHEN c.collation_name <> CAST(DATABASEPROPERTYEX(DB_NAME(), 'Collation') AS nvarchar(128)) THEN c.collation_name END,
    CASE WHEN EXISTS (SELECT 1 FROM sys.index_columns AS x JOIN sys.indexes AS i ON i.object_id = x.object_id AND i.index_id = x.index_id
                      WHERE i.is_primary_key = 1 AND x.object_id = c.object_id AND x.column_id = c.column_id) THEN 1 ELSE 0 END,
    (SELECT TOP (1) OBJECT_SCHEMA_NAME(f.referenced_object_id) + '.' + OBJECT_NAME(f.referenced_object_id) + '.' + COL_NAME(f.referenced_object_id, f.referenced_column_id)
     FROM sys.foreign_key_columns AS f WHERE f.parent_object_id = c.object_id AND f.parent_column_id = c.column_id)
FROM sys.columns AS c
LEFT JOIN sys.identity_columns AS ic ON ic.object_id = c.object_id AND ic.column_id = c.column_id
LEFT JOIN sys.computed_columns AS cc ON cc.object_id = c.object_id AND cc.column_id = c.column_id
LEFT JOIN sys.default_constraints AS dc ON dc.parent_object_id = c.object_id AND dc.parent_column_id = c.column_id
WHERE c.object_id = @id ORDER BY c.column_id;
SELECT i.name, i.type_desc, i.is_primary_key, i.is_unique, i.is_unique_constraint, i.filter_definition,
    STUFF((SELECT ', ' + QUOTENAME(c.name) + CASE WHEN x.is_descending_key = 1 THEN ' DESC' ELSE '' END
           FROM sys.index_columns AS x JOIN sys.columns AS c ON c.object_id = x.object_id AND c.column_id = x.column_id
           WHERE x.object_id = i.object_id AND x.index_id = i.index_id AND x.is_included_column = 0 AND x.key_ordinal > 0
           ORDER BY x.key_ordinal FOR XML PATH(''), TYPE).value('.', 'nvarchar(max)'), 1, 2, ''),
    STUFF((SELECT ', ' + QUOTENAME(c.name)
           FROM sys.index_columns AS x JOIN sys.columns AS c ON c.object_id = x.object_id AND c.column_id = x.column_id
           WHERE x.object_id = i.object_id AND x.index_id = i.index_id AND x.is_included_column = 1
           FOR XML PATH(''), TYPE).value('.', 'nvarchar(max)'), 1, 2, ''),
    d.type, d.name,
    (SELECT TOP (1) c.name FROM sys.index_columns AS x JOIN sys.columns AS c ON c.object_id = x.object_id AND c.column_id = x.column_id
     WHERE x.object_id = i.object_id AND x.index_id = i.index_id AND x.partition_ordinal = 1)
FROM sys.indexes AS i JOIN sys.data_spaces AS d ON d.data_space_id = i.data_space_id
WHERE i.object_id = @id AND i.is_hypothetical = 0 ORDER BY i.index_id;
SELECT f.name,
    STUFF((SELECT ', ' + QUOTENAME(COL_NAME(x.parent_object_id, x.parent_column_id)) FROM sys.foreign_key_columns AS x
           WHERE x.constraint_object_id = f.object_id ORDER BY x.constraint_column_id FOR XML PATH(''), TYPE).value('.', 'nvarchar(max)'), 1, 2, ''),
    OBJECT_SCHEMA_NAME(f.referenced_object_id), OBJECT_NAME(f.referenced_object_id),
    STUFF((SELECT ', ' + QUOTENAME(COL_NAME(x.referenced_object_id, x.referenced_column_id)) FROM sys.foreign_key_columns AS x
           WHERE x.constraint_object_id = f.object_id ORDER BY x.constraint_column_id FOR XML PATH(''), TYPE).value('.', 'nvarchar(max)'), 1, 2, ''),
    f.delete_referential_action_desc, f.update_referential_action_desc
FROM sys.foreign_keys AS f WHERE f.parent_object_id = @id ORDER BY f.name;
SELECT name, definition FROM sys.check_constraints WHERE parent_object_id = @id ORDER BY name;
SELECT pf.name, ps.name,
    (SELECT TOP (1) c.name FROM sys.indexes AS i JOIN sys.index_columns AS x ON x.object_id = i.object_id AND x.index_id = i.index_id AND x.partition_ordinal = 1
     JOIN sys.columns AS c ON c.object_id = x.object_id AND c.column_id = x.column_id WHERE i.object_id = @id AND i.index_id IN (0, 1)),
    TYPE_NAME(pp.user_type_id), pp.max_length, pp.precision, pp.scale,
    CASE pf.boundary_value_on_right WHEN 1 THEN 'RIGHT' ELSE 'LEFT' END
FROM sys.partition_functions AS pf JOIN sys.partition_schemes AS ps ON ps.function_id = pf.function_id
JOIN sys.partition_parameters AS pp ON pp.function_id = pf.function_id AND pp.parameter_id = 1
WHERE ps.data_space_id = @ps;
SELECT CASE WHEN CAST(SQL_VARIANT_PROPERTY(v.value, 'BaseType') AS sysname) IN ('tinyint', 'smallint', 'int', 'bigint', 'decimal', 'numeric', 'float', 'real', 'money', 'smallmoney', 'bit')
            THEN CONVERT(nvarchar(4000), v.value)
            ELSE N'''' + REPLACE(CONVERT(nvarchar(4000), v.value, 121), N'''', N'''''') + N'''' END
FROM sys.partition_range_values AS v WHERE v.function_id = @pf ORDER BY v.boundary_id;
SELECT p.partition_number, p.rows, fg.name
FROM sys.partitions AS p
JOIN sys.destination_data_spaces AS dds ON dds.partition_scheme_id = @ps AND dds.destination_id = p.partition_number
JOIN sys.filegroups AS fg ON fg.data_space_id = dds.data_space_id
WHERE p.object_id = @id AND p.index_id IN (0, 1) ORDER BY p.partition_number;
SELECT name, TYPE_NAME(user_type_id), max_length, precision, scale, is_output
FROM sys.parameters WHERE object_id = @id AND parameter_id > 0 ORDER BY parameter_id;";

        private static readonly TimeSpan Fresh = TimeSpan.FromMinutes(10);

        /// <summary>Details read in the last ten minutes, without asking the database; null if there are none.</summary>
        public static ObjectDetails Cached(ConnectionTarget target, string fullName)
        {
            lock (Cache)
            {
                return Cache.TryGetValue(target.Key + "|" + fullName, out var hit) && DateTime.UtcNow - hit.Item1 < Fresh ? hit.Item2 : null;
            }
        }

        /// <summary>Catalog details of `schema.name`, cached for ten minutes; null if it does not exist.</summary>
        public static ObjectDetails Load(ConnectionTarget target, string fullName)
        {
            var key = target.Key + "|" + fullName;
            var cached = Cached(target, fullName);
            if (cached != null)
            {
                return cached;
            }
            var watch = System.Diagnostics.Stopwatch.StartNew();
            var d = new ObjectDetails();
            using (var connection = new SqlConnection(target.ConnectionString))
            using (var command = new SqlCommand(Query, connection) { CommandTimeout = 30 })
            {
                command.Parameters.AddWithValue("@name", fullName);
                connection.Open();
                using (var r = command.ExecuteReader())
                {
                    if (!r.Read())
                    {
                        return null;
                    }
                    d.TypeCode = r.GetString(0).Trim();
                    d.Schema = r.GetString(1);
                    d.Name = r.GetString(2);
                    d.Definition = r.IsDBNull(3) ? null : r.GetString(3);
                    d.Created = r.GetDateTime(4);
                    d.Modified = r.GetDateTime(5);
                    d.Rows = r.IsDBNull(6) ? (long?)null : Convert.ToInt64(r.GetValue(6), CultureInfo.InvariantCulture);
                    d.SizeKb = r.IsDBNull(7) ? (long?)null : Convert.ToInt64(r.GetValue(7), CultureInfo.InvariantCulture);
                    string text(int i) => r.IsDBNull(i) ? null : r.GetString(i);
                    r.NextResult();
                    while (r.Read())
                    {
                        d.Columns.Add(new ColumnInfo
                        {
                            Name = r.GetString(0),
                            Type = r.IsDBNull(1) ? "" : MetadataLoader.TypeText(r.GetString(1), r.GetInt16(2), r.GetByte(3), r.GetByte(4)),
                            Nullable = r.GetBoolean(5),
                            Identity = text(6),
                            Computed = text(7),
                            Persisted = r.GetBoolean(8),
                            DefaultName = text(9),
                            DefaultDefinition = text(10),
                            Collation = text(11),
                            PrimaryKey = r.GetInt32(12) == 1,
                            References = text(13),
                        });
                    }
                    r.NextResult();
                    while (r.Read())
                    {
                        var partitioned = text(8) == "PS";
                        d.Indexes.Add(new IndexInfo
                        {
                            Name = text(0),
                            Kind = r.GetString(1),
                            PrimaryKey = r.GetBoolean(2),
                            Unique = r.GetBoolean(3),
                            UniqueConstraint = r.GetBoolean(4),
                            Filter = text(5),
                            Keys = text(6),
                            Includes = text(7),
                            Filegroup = partitioned ? null : text(9),
                            Scheme = partitioned ? text(9) : null,
                            Column = partitioned ? text(10) : null,
                        });
                    }
                    r.NextResult();
                    while (r.Read())
                    {
                        d.ForeignKeys.Add(Enumerable.Range(0, 7).Select(text).ToArray());
                    }
                    r.NextResult();
                    while (r.Read())
                    {
                        d.Checks.Add(new[] { text(0), text(1) });
                    }
                    r.NextResult();
                    if (r.Read())
                    {
                        d.Partitioning = new PartitionInfo
                        {
                            Function = r.GetString(0),
                            Scheme = r.GetString(1),
                            Column = text(2),
                            ParameterType = MetadataLoader.TypeText(r.GetString(3), r.GetInt16(4), r.GetByte(5), r.GetByte(6)),
                            Range = r.GetString(7),
                        };
                    }
                    r.NextResult();
                    while (r.Read())
                    {
                        d.Partitioning?.Boundaries.Add(r.GetString(0));
                    }
                    r.NextResult();
                    while (r.Read())
                    {
                        d.Partitioning?.Rows.Add(r.GetInt64(1));
                        d.Partitioning?.Filegroups.Add(r.GetString(2));
                    }
                    r.NextResult();
                    while (r.Read())
                    {
                        d.Parameters.Add(r.GetString(0) + " " + MetadataLoader.TypeText(r.GetString(1), r.GetInt16(2), r.GetByte(3), r.GetByte(4)) + (r.GetBoolean(5) ? " OUTPUT" : ""));
                    }
                }
            }
            d.Filegroup = d.Indexes.FirstOrDefault()?.Filegroup;
            Log.Write("details of " + fullName + " read in " + watch.ElapsedMilliseconds + " ms");
            lock (Cache)
            {
                Cache[key] = Tuple.Create(DateTime.UtcNow, d);
            }
            return d;
        }

        /// <summary>CREATE TABLE for tables; the definition, as ALTER when asked, for modules.</summary>
        public static string Script(ObjectDetails d, bool alter)
        {
            if (d.IsTable)
            {
                var reply = EngineProcess.Shared.Request("table_script", "\"details\":" + d.ToEngineJson() + ",\"newline\":\"\\r\\n\"");
                return reply == null ? null : Json.Parse<TextReply>(reply)?.Result?.Text;
            }
            if (d.Definition == null)
            {
                return "-- The definition of " + d.FullName + " is encrypted or not available.";
            }
            if (!alter)
            {
                return d.Definition;
            }
            var altered = EngineProcess.Shared.Request("to_alter", "\"text\":" + Json.Str(d.Definition));
            return (altered == null ? null : Json.Parse<TextReply>(altered)?.Result?.Text) ?? d.Definition;
        }

        private static string ObjectAt(EditorContext editor, out ConnectionTarget target)
        {
            ThreadHelper.ThrowIfNotOnUIThread();
            target = ConnectionResolver.TryGetActive();
            var key = Commands.MetadataKey(out _);
            if (target == null || !target.CanQuery)
            {
                return null;
            }
            var reply = EngineProcess.Shared.Request("object_at", "\"key\":" + Json.Str(key) + ",\"text\":" + Json.Str(editor.Text) + ",\"caret\":" + editor.Caret);
            var found = reply == null ? null : Json.Parse<ObjectAtReply>(reply)?.Result;
            return found == null ? null : found.Schema + "." + found.Name;
        }

        /// <summary>
        /// Opens the script of the object at the caret in a new window: CREATE TABLE for a table,
        /// ALTER for a view, procedure or function. Returns false when the caret is on no object.
        /// </summary>
        public static bool ScriptAtCaret(bool report)
        {
            ThreadHelper.ThrowIfNotOnUIThread();
            var editor = EditorContext.Active();
            var name = editor == null ? null : ObjectAt(editor, out var target);
            if (name == null)
            {
                if (report)
                {
                    EditorContext.Status("put the caret on a table, view, procedure or function.");
                }
                return false;
            }
            target = ConnectionResolver.TryGetActive();
            var details = Load(target, name);
            var script = details == null ? null : Script(details, true);
            if (script == null)
            {
                EditorContext.Status("could not script " + name + ".");
                return true;
            }
            EditorContext.OpenInNewQuery(script);
            EditorContext.Status("script of " + name + " opened in a new window.");
            return true;
        }

        /// <summary>Replaces `ALTER PROCEDURE name` (from `start` to `end`) with the object's ALTER script.</summary>
        public static void ReplaceWithAlter(IVsTextView view, int start, int end, string name)
        {
            ThreadHelper.ThrowIfNotOnUIThread();
            var target = ConnectionResolver.TryGetActive();
            var editor = EditorContext.For(view);
            var details = target == null || !target.CanQuery ? null : Load(target, name);
            var script = details == null ? null : Script(details, true);
            if (editor == null || script == null)
            {
                EditorContext.Status("could not read the definition of " + name + ".");
                return;
            }
            editor.Replace(start, end, script.TrimEnd());
            editor.Select(start, start);
        }

        // ---- The card shown when the mouse rests on an object ----

        private static TextBlock Cell(string text, bool bold = false, bool dim = false, bool mono = false)
        {
            var cell = new TextBlock
            {
                Text = text ?? "",
                FontWeight = bold ? FontWeights.SemiBold : FontWeights.Normal,
                Opacity = bold ? 0.8 : dim ? 0.7 : 1,
                Margin = new Thickness(8, 3, 8, 3),
                VerticalAlignment = VerticalAlignment.Center,
                TextTrimming = TextTrimming.CharacterEllipsis,
                MaxWidth = 340,
            };
            if (mono)
            {
                cell.FontFamily = Theme.Mono;
            }
            return cell;
        }

        /// <summary>A table with a header row and subtle alternating rows; `mono` columns use a code font.</summary>
        private static UIElement Table(IList<string[]> rows, IList<string> header, params int[] mono)
        {
            var grid = new Grid();
            for (var c = 0; c < header.Count; c++)
            {
                grid.ColumnDefinitions.Add(new ColumnDefinition { Width = GridLength.Auto });
            }
            for (var r = 0; r <= rows.Count; r++)
            {
                grid.RowDefinitions.Add(new RowDefinition { Height = GridLength.Auto });
                if (r == 0 || r % 2 == 0)
                {
                    var band = new Border { Background = r == 0 ? Theme.Subtle : Theme.Faint, BorderBrush = Theme.Line, BorderThickness = new Thickness(0, 0, 0, r == 0 ? 1 : 0) };
                    Grid.SetRow(band, r);
                    Grid.SetColumnSpan(band, header.Count);
                    grid.Children.Add(band);
                }
                for (var c = 0; c < header.Count; c++)
                {
                    var cell = r == 0 ? Cell(header[c], bold: true) : Cell(rows[r - 1][c], dim: c > 1, mono: Array.IndexOf(mono, c) >= 0);
                    Grid.SetRow(cell, r);
                    Grid.SetColumn(cell, c);
                    grid.Children.Add(cell);
                }
            }
            return new Border
            {
                BorderBrush = Theme.Line,
                BorderThickness = new Thickness(1),
                CornerRadius = new CornerRadius(3),
                HorizontalAlignment = HorizontalAlignment.Left,
                Child = new ScrollViewer { Content = grid, MaxHeight = 300, VerticalScrollBarVisibility = ScrollBarVisibility.Auto, HorizontalScrollBarVisibility = ScrollBarVisibility.Auto },
            };
        }

        private static string Number(long? value) => value?.ToString("N0", CultureInfo.InvariantCulture) ?? "?";

        /// <summary>The partitioning summary, or where an unpartitioned table is stored.</summary>
        public static string StorageLine(ObjectDetails d)
        {
            var p = d.Partitioning;
            if (p != null)
            {
                return Loc.T("Partitioned on " + p.Column + " by " + p.Function + " (RANGE " + p.Range + ", " + (p.Boundaries.Count + 1) + " partitions) through scheme " + p.Scheme);
            }
            return d.IsTable ? Loc.T("Not partitioned; stored on filegroup " + (d.Filegroup ?? "?")) : null;
        }

        /// <summary>The card's title alone, shown while the details are read.</summary>
        public static UIElement Loading(string schema, string name, string kind)
        {
            var title = new StackPanel { Orientation = Orientation.Horizontal };
            title.Children.Add(Theme.Badge((kind ?? "object").Replace('_', ' ').ToUpperInvariant(), Theme.Slate, 9.5));
            title.Children.Add(new TextBlock { Text = schema + ".", FontSize = 14, Opacity = 0.7, Margin = new Thickness(8, 0, 0, 0), VerticalAlignment = VerticalAlignment.Center });
            title.Children.Add(new TextBlock { Text = name, FontSize = 14, FontWeight = FontWeights.SemiBold, VerticalAlignment = VerticalAlignment.Center });
            var panel = new StackPanel { Margin = new Thickness(4), MinWidth = 360 };
            panel.Children.Add(title);
            panel.Children.Add(new TextBlock { Text = Loc.T("Reading the details from the database..."), Opacity = 0.7, Margin = new Thickness(0, 6, 0, 0) });
            return panel;
        }

        /// <summary>A card with the object's columns, indexes, partitions and script.</summary>
        public static UIElement Card(ObjectDetails d)
        {
            var header = new StackPanel { Margin = new Thickness(0, 0, 0, 8) };
            var kindColor = d.IsTable ? Theme.Blue : d.TypeCode == "V" ? Color.FromRgb(0x1E, 0x95, 0xA3) : d.Parameters.Count > 0 || d.TypeCode == "P" ? Theme.Purple : Theme.Slate;
            var title = new StackPanel { Orientation = Orientation.Horizontal, Margin = new Thickness(0, 0, 0, 3) };
            title.Children.Add(Theme.Badge(d.Kind.ToUpperInvariant(), kindColor, 9.5));
            title.Children.Add(new TextBlock { Text = d.Schema + ".", FontSize = 14, Opacity = 0.7, Margin = new Thickness(8, 0, 0, 0), VerticalAlignment = VerticalAlignment.Center });
            title.Children.Add(new TextBlock { Text = d.Name, FontSize = 14, FontWeight = FontWeights.SemiBold, VerticalAlignment = VerticalAlignment.Center });
            header.Children.Add(title);
            var facts = new List<string>();
            if (d.Rows != null)
            {
                facts.Add(Loc.T(Number(d.Rows) + " rows"));
            }
            if (d.SizeKb != null)
            {
                facts.Add((d.SizeKb.Value / 1024.0).ToString("N1", CultureInfo.InvariantCulture) + " MB");
            }
            facts.Add(Loc.T("modified " + d.Modified.ToString("yyyy-MM-dd HH:mm", CultureInfo.InvariantCulture)));
            header.Children.Add(new TextBlock { Text = string.Join("  ·  ", facts), Opacity = 0.7 });
            var storageLine = StorageLine(d);
            if (storageLine != null && d.Partitioning != null)
            {
                var info = new StackPanel { Orientation = Orientation.Horizontal };
                info.Children.Add(Theme.Icon(Theme.InfoGlyph, 12, Theme.Frozen(Theme.Blue)));
                info.Children.Add(new TextBlock { Text = storageLine, Margin = new Thickness(6, 0, 0, 0), TextWrapping = TextWrapping.Wrap, MaxWidth = 660 });
                var pill = Theme.Pill(info, Theme.Blue);
                pill.Margin = new Thickness(0, 6, 0, 0);
                header.Children.Add(pill);
            }
            else if (storageLine != null)
            {
                header.Children.Add(new TextBlock { Text = storageLine, Opacity = 0.7, Margin = new Thickness(0, 2, 0, 0) });
            }

            var tabs = new List<Tuple<string, UIElement>>();
            if (d.Columns.Count > 0)
            {
                var rows = d.Columns.Select(c => new[]
                {
                    c.Name + (c.PrimaryKey ? "  (PK)" : ""),
                    c.Computed != null ? "AS " + c.Computed : c.Type,
                    c.Computed != null ? Loc.T(c.Persisted ? "persisted" : "computed") : (c.Nullable ? "NULL" : "NOT NULL"),
                    string.Join("  ", new[]
                    {
                        c.Identity != null ? "IDENTITY(" + c.Identity.Replace(",", ", ") + ")" : null,
                        c.DefaultDefinition != null ? "DEFAULT " + c.DefaultDefinition : null,
                        c.References != null ? "→ " + c.References : null,
                    }.Where(x => x != null)),
                }).ToList();
                tabs.Add(Tuple.Create(Loc.T("Columns (" + d.Columns.Count + ")"), Table(rows, new[] { Loc.T("Name"), Loc.T("Type"), Loc.T("Null"), "" }, 1)));
            }
            if (d.Parameters.Count > 0)
            {
                tabs.Add(Tuple.Create(Loc.T("Parameters (" + d.Parameters.Count + ")"), Table(d.Parameters.Select(p => new[] { p }).ToList(), new[] { Loc.T("Parameter") }, 0)));
            }
            var indexes = d.Indexes.Where(i => i.Kind != "HEAP").ToList();
            if (indexes.Count > 0)
            {
                var rows = indexes.Select(i => new[]
                {
                    i.Name,
                    (i.PrimaryKey ? "PK " : i.Unique ? "UNIQUE " : "") + i.Kind,
                    i.Keys + (string.IsNullOrEmpty(i.Includes) ? "" : " INCLUDE (" + i.Includes + ")") + (i.Filter == null ? "" : " WHERE " + i.Filter),
                    i.Scheme != null ? i.Scheme + "(" + i.Column + ")" : i.Filegroup,
                }).ToList();
                tabs.Add(Tuple.Create(Loc.T("Indexes (" + indexes.Count + ")"), Table(rows, new[] { Loc.T("Name"), Loc.T("Type"), Loc.T("Columns"), Loc.T("Stored on") }, 2)));
            }
            if (d.Partitioning != null)
            {
                var p = d.Partitioning;
                var rows = new List<string[]>();
                for (var i = 0; i < p.Rows.Count; i++)
                {
                    var low = i == 0 ? "" : p.Boundaries[i - 1];
                    var high = i < p.Boundaries.Count ? p.Boundaries[i] : "";
                    // RANGE RIGHT: a boundary starts the next partition; RANGE LEFT: it ends this one.
                    var range = p.Range == "RIGHT"
                        ? (low.Length > 0 ? ">= " + low : "") + (low.Length > 0 && high.Length > 0 ? " and " : "") + (high.Length > 0 ? "< " + high : "")
                        : (low.Length > 0 ? "> " + low : "") + (low.Length > 0 && high.Length > 0 ? " and " : "") + (high.Length > 0 ? "<= " + high : "");
                    rows.Add(new[] { (i + 1).ToString(CultureInfo.InvariantCulture), range.Length == 0 ? Loc.T("all") : range, Number(p.Rows[i]), p.Filegroups[i] });
                }
                tabs.Add(Tuple.Create(Loc.T("Partitions (" + p.Rows.Count + ")"), Table(rows, new[] { "#", p.Column, Loc.T("Rows"), Loc.T("Filegroup") }, 1)));
            }

            var script = new TextBox
            {
                IsReadOnly = true,
                FontFamily = Theme.Mono,
                FontSize = 12,
                MaxHeight = 300,
                MinWidth = 420,
                Padding = new Thickness(4),
                Background = Theme.Faint,
                BorderBrush = Theme.Line,
                VerticalScrollBarVisibility = ScrollBarVisibility.Auto,
                HorizontalScrollBarVisibility = ScrollBarVisibility.Auto,
                Text = Loc.T("(loading)"),
            };
            // The default text box style is black text; follow the card's text color instead.
            var cardText = new System.Windows.Data.Binding { Path = new PropertyPath(System.Windows.Documents.TextElement.ForegroundProperty), RelativeSource = new System.Windows.Data.RelativeSource(System.Windows.Data.RelativeSourceMode.FindAncestor, typeof(DockPanel), 1) };
            script.SetBinding(Control.ForegroundProperty, cardText);
            script.SetBinding(System.Windows.Controls.Primitives.TextBoxBase.CaretBrushProperty, cardText);
            var copy = Theme.FlatButton(Loc.T("Copy"), Theme.CopyGlyph);
            var open = Theme.FlatButton(Loc.T("Open in new window"), Theme.OpenGlyph);
            open.Margin = new Thickness(6, 0, 0, 0);
            var buttons = new StackPanel { Orientation = Orientation.Horizontal, Margin = new Thickness(0, 6, 0, 0) };
            buttons.Children.Add(copy);
            buttons.Children.Add(open);
            var scriptPanel = new DockPanel();
            DockPanel.SetDock(buttons, Dock.Bottom);
            scriptPanel.Children.Add(buttons);
            scriptPanel.Children.Add(script);
            tabs.Add(Tuple.Create(Loc.T("Script"), (UIElement)scriptPanel));
            string scriptText = null;
            string ensureScript()
            {
                if (scriptText == null)
                {
                    scriptText = Script(d, false) ?? Loc.T("-- The script could not be made.");
                    script.Text = scriptText;
                }
                return scriptText;
            }
            copy.Click += (s, e) => Clipboard.SetText(ensureScript());
            open.Click += (s, e) =>
            {
                ThreadHelper.ThrowIfNotOnUIThread();
                EditorContext.OpenInNewQuery(ensureScript());
            };

            // Flat tabs: the selected one is underlined in the accent color.
            var strip = new StackPanel { Orientation = Orientation.Horizontal };
            var host = new Border { Margin = new Thickness(0, 8, 0, 0) };
            var heads = new List<Border>();
            var accent = Theme.Frozen(Theme.Blue);
            void select(int index)
            {
                for (var i = 0; i < heads.Count; i++)
                {
                    heads[i].BorderThickness = new Thickness(0, 0, 0, i == index ? 2 : 0);
                    heads[i].Child.Opacity = i == index ? 1 : 0.65;
                }
                host.Child = tabs[index].Item2;
                if (tabs[index].Item2 == scriptPanel)
                {
                    ensureScript();
                }
            }
            for (var i = 0; i < tabs.Count; i++)
            {
                var index = i;
                var head = new Border
                {
                    Child = new TextBlock { Text = tabs[i].Item1 },
                    Padding = new Thickness(2, 4, 2, 4),
                    Margin = new Thickness(0, 0, 14, 0),
                    Background = Brushes.Transparent,
                    BorderBrush = accent,
                    Cursor = System.Windows.Input.Cursors.Hand,
                };
                head.MouseLeftButtonUp += (s, e) => select(index);
                head.MouseEnter += (s, e) => head.Child.Opacity = 1;
                head.MouseLeave += (s, e) => head.Child.Opacity = host.Child == tabs[index].Item2 ? 1 : 0.65;
                heads.Add(head);
                strip.Children.Add(head);
            }
            select(0);
            var stripLine = new Border { BorderBrush = Theme.Line, BorderThickness = new Thickness(0, 0, 0, 1), Child = strip };

            var panel = new StackPanel { Margin = new Thickness(4), MinWidth = 360, MaxWidth = 760 };
            TextOptions.SetTextFormattingMode(panel, TextFormattingMode.Display);
            panel.Children.Add(header);
            panel.Children.Add(stripLine);
            panel.Children.Add(host);
            return panel;
        }
    }
}
