using System;
using System.Collections.Generic;
using System.Globalization;
using System.Linq;
using System.Text;
using Microsoft.Data.SqlClient;

namespace QuillSql.Vsix
{
    /// <summary>One Query Store query with its runtime totals over the last days.</summary>
    internal sealed class SlowQuery
    {
        public long QueryId;
        public string Text;
        public long Executions;
        public double AvgMs, AvgCpuMs, AvgReads, BestPlanMs, WorstPlanMs;
        public int Plans;
        public string WorstPlanXml;
    }

    /// <summary>
    /// What the AI needs to judge performance, read from the database: indexes, sizes and
    /// statistics dates, execution plans, and Query Store history. Everything here only reads,
    /// except actual plans, which run the script (callers allow that only for read-only scripts).
    /// </summary>
    internal static class PlanCapture
    {
        private const int Timeout = 120;

        private static string In(SqlCommand command, IList<string> objects)
        {
            var names = new List<string>();
            for (var i = 0; i < objects.Count && i < 60; i++)
            {
                command.Parameters.AddWithValue("@p" + i, objects[i]);
                names.Add("OBJECT_ID(@p" + i + ")");
            }
            return names.Count == 0 ? "NULL" : string.Join(", ", names);
        }

        /// <summary>Rows, size and every index with its keys, included columns and statistics date.</summary>
        public static string Indexes(ConnectionTarget target, IList<string> objects)
        {
            if (objects.Count == 0)
            {
                return "(no tables found in the script)";
            }
            var sb = new StringBuilder();
            using (var connection = new SqlConnection(target.ConnectionString))
            using (var command = new SqlCommand { Connection = connection, CommandTimeout = Timeout })
            {
                var list = In(command, objects);
                command.CommandText = @"
SET NOCOUNT ON;
SELECT o.object_id, SCHEMA_NAME(o.schema_id), o.name, o.type_desc,
    (SELECT SUM(p.rows) FROM sys.partitions AS p WHERE p.object_id = o.object_id AND p.index_id IN (0, 1)),
    (SELECT SUM(a.total_pages) * 8 / 1024 FROM sys.partitions AS p JOIN sys.allocation_units AS a ON a.container_id = p.partition_id WHERE p.object_id = o.object_id)
FROM sys.objects AS o WHERE o.object_id IN (" + list + @") AND o.type IN ('U', 'V')
ORDER BY 2, 3;
SELECT i.object_id, i.name, i.type_desc, i.is_unique, i.is_primary_key, i.filter_definition, STATS_DATE(i.object_id, i.index_id),
    STUFF((SELECT ', ' + c.name + CASE WHEN ic.is_descending_key = 1 THEN ' DESC' ELSE '' END
           FROM sys.index_columns AS ic JOIN sys.columns AS c ON c.object_id = ic.object_id AND c.column_id = ic.column_id
           WHERE ic.object_id = i.object_id AND ic.index_id = i.index_id AND ic.is_included_column = 0 AND ic.key_ordinal > 0
           ORDER BY ic.key_ordinal FOR XML PATH(''), TYPE).value('.', 'nvarchar(max)'), 1, 2, ''),
    STUFF((SELECT ', ' + c.name
           FROM sys.index_columns AS ic JOIN sys.columns AS c ON c.object_id = ic.object_id AND c.column_id = ic.column_id
           WHERE ic.object_id = i.object_id AND ic.index_id = i.index_id AND ic.is_included_column = 1
           FOR XML PATH(''), TYPE).value('.', 'nvarchar(max)'), 1, 2, '')
FROM sys.indexes AS i
WHERE i.object_id IN (" + list + @") AND i.type > 0 AND i.is_hypothetical = 0 AND i.is_disabled = 0
ORDER BY i.object_id, i.index_id;";
                connection.Open();
                var tables = new Dictionary<int, StringBuilder>();
                using (var r = command.ExecuteReader())
                {
                    while (r.Read())
                    {
                        var line = new StringBuilder(r.GetString(1) + "." + r.GetString(2) + " (" + (r.GetString(3) == "VIEW" ? "view" : "table"));
                        if (!r.IsDBNull(4))
                        {
                            line.Append(", " + r.GetInt64(4).ToString("N0", CultureInfo.InvariantCulture) + " rows");
                        }
                        if (!r.IsDBNull(5))
                        {
                            line.Append(", " + r.GetInt64(5) + " MB");
                        }
                        tables[r.GetInt32(0)] = line.Append(")\n");
                    }
                    r.NextResult();
                    while (r.Read())
                    {
                        if (!tables.TryGetValue(r.GetInt32(0), out var line))
                        {
                            continue;
                        }
                        line.Append("    ").Append(r.IsDBNull(1) ? "(heap)" : r.GetString(1)).Append(' ').Append(r.GetString(2));
                        if (r.GetBoolean(4)) line.Append(" PRIMARY KEY");
                        else if (r.GetBoolean(3)) line.Append(" UNIQUE");
                        line.Append(" (").Append(r.IsDBNull(7) ? "" : r.GetString(7)).Append(')');
                        if (!r.IsDBNull(8)) line.Append(" INCLUDE (").Append(r.GetString(8)).Append(')');
                        if (!r.IsDBNull(5)) line.Append(" WHERE ").Append(r.GetString(5));
                        line.Append(r.IsDBNull(6) ? ", statistics never updated" : ", statistics updated " + r.GetDateTime(6).ToString("yyyy-MM-dd HH:mm", CultureInfo.InvariantCulture));
                        line.Append('\n');
                    }
                }
                foreach (var t in tables.Values)
                {
                    sb.Append(t);
                }
            }
            return sb.Length == 0 ? "(none of the objects are tables or views)" : sb.ToString();
        }

        private static bool IsPlanColumn(SqlDataReader r) =>
            r.FieldCount == 1 && r.GetName(0).IndexOf("Showplan", StringComparison.OrdinalIgnoreCase) >= 0;

        /// <summary>
        /// Showplan XML for each statement. Estimated plans only compile the script; actual plans
        /// run it and discard its results. Batches that fail are reported in `errors`.
        /// </summary>
        public static List<string> Plans(ConnectionTarget target, IList<string> batches, bool actual, List<string> errors)
        {
            var plans = new List<string>();
            using (var connection = new SqlConnection(target.ConnectionString))
            {
                connection.Open();
                using (var on = new SqlCommand(actual ? "SET STATISTICS XML ON;" : "SET SHOWPLAN_XML ON;", connection))
                {
                    on.ExecuteNonQuery();
                }
                for (var i = 0; i < batches.Count; i++)
                {
                    try
                    {
                        using (var command = new SqlCommand(batches[i], connection) { CommandTimeout = Timeout })
                        using (var r = command.ExecuteReader())
                        {
                            do
                            {
                                var plan = IsPlanColumn(r);
                                while (r.Read())
                                {
                                    if (plan && !r.IsDBNull(0))
                                    {
                                        plans.Add(r.GetString(0));
                                    }
                                }
                            }
                            while (r.NextResult());
                        }
                    }
                    catch (SqlException ex)
                    {
                        errors.Add("batch " + (i + 1) + ": " + ex.Message);
                    }
                }
            }
            return plans;
        }

        /// <summary>Query Store's most expensive queries over the last `days`, or throws if it is off.</summary>
        public static List<SlowQuery> QueryStore(ConnectionTarget target, int days, int top)
        {
            var queries = new List<SlowQuery>();
            using (var connection = new SqlConnection(target.ConnectionString))
            {
                connection.Open();
                using (var state = new SqlCommand("SELECT actual_state_desc FROM sys.database_query_store_options;", connection))
                {
                    var value = state.ExecuteScalar() as string;
                    if (value == null || value == "OFF" || value == "ERROR")
                    {
                        throw new InvalidOperationException("Query Store is " + (value ?? "not available") + " in this database. Turn it on with ALTER DATABASE ... SET QUERY_STORE = ON.");
                    }
                }
                const string query = @"
SET NOCOUNT ON;
WITH per_plan AS (
    SELECT p.query_id, p.plan_id,
        SUM(rs.count_executions) AS executions,
        SUM(rs.avg_duration * rs.count_executions) AS total_duration,
        SUM(rs.avg_cpu_time * rs.count_executions) AS total_cpu,
        SUM(rs.avg_logical_io_reads * rs.count_executions) AS total_reads
    FROM sys.query_store_plan AS p
    JOIN sys.query_store_runtime_stats AS rs ON rs.plan_id = p.plan_id
    JOIN sys.query_store_runtime_stats_interval AS i ON i.runtime_stats_interval_id = rs.runtime_stats_interval_id
    WHERE i.start_time >= DATEADD(DAY, -@days, SYSUTCDATETIME())
    GROUP BY p.query_id, p.plan_id
), per_query AS (
    SELECT query_id, SUM(executions) AS executions, SUM(total_duration) AS total_duration,
        SUM(total_cpu) AS total_cpu, SUM(total_reads) AS total_reads, COUNT(*) AS plans,
        MIN(total_duration / NULLIF(executions, 0)) AS best_plan, MAX(total_duration / NULLIF(executions, 0)) AS worst_plan
    FROM per_plan GROUP BY query_id
)
SELECT TOP (@top) q.query_id, qt.query_sql_text, pq.executions,
    pq.total_duration / NULLIF(pq.executions, 0) / 1000.0, pq.total_cpu / NULLIF(pq.executions, 0) / 1000.0,
    pq.total_reads / NULLIF(pq.executions, 0), pq.plans, pq.best_plan / 1000.0, pq.worst_plan / 1000.0,
    (SELECT TOP (1) p.query_plan FROM per_plan AS pp JOIN sys.query_store_plan AS p ON p.plan_id = pp.plan_id
     WHERE pp.query_id = q.query_id ORDER BY pp.total_duration / NULLIF(pp.executions, 0) DESC)
FROM per_query AS pq
JOIN sys.query_store_query AS q ON q.query_id = pq.query_id
JOIN sys.query_store_query_text AS qt ON qt.query_text_id = q.query_text_id
ORDER BY pq.total_duration DESC;";
                using (var command = new SqlCommand(query, connection) { CommandTimeout = Timeout })
                {
                    command.Parameters.AddWithValue("@days", days);
                    command.Parameters.AddWithValue("@top", top);
                    using (var r = command.ExecuteReader())
                    {
                        double num(int i) => r.IsDBNull(i) ? 0 : Convert.ToDouble(r.GetValue(i), CultureInfo.InvariantCulture);
                        while (r.Read())
                        {
                            queries.Add(new SlowQuery
                            {
                                QueryId = r.GetInt64(0),
                                Text = r.GetString(1),
                                Executions = r.IsDBNull(2) ? 0 : Convert.ToInt64(r.GetValue(2), CultureInfo.InvariantCulture),
                                AvgMs = num(3),
                                AvgCpuMs = num(4),
                                AvgReads = num(5),
                                Plans = r.GetInt32(6),
                                BestPlanMs = num(7),
                                WorstPlanMs = num(8),
                                WorstPlanXml = r.IsDBNull(9) ? null : r.GetString(9),
                            });
                        }
                    }
                }
            }
            return queries;
        }

        public static string Describe(SlowQuery q) => string.Format(CultureInfo.InvariantCulture,
            "query_id {0}: {1:N0} executions, avg {2:N1} ms (CPU {3:N1} ms), {4:N0} logical reads per run, {5} plan(s){6}\n{7}",
            q.QueryId, q.Executions, q.AvgMs, q.AvgCpuMs, q.AvgReads, q.Plans,
            q.Plans > 1 ? string.Format(CultureInfo.InvariantCulture, " (best plan avg {0:N1} ms, worst {1:N1} ms)", q.BestPlanMs, q.WorstPlanMs) : "",
            q.Text.Trim());
    }
}
