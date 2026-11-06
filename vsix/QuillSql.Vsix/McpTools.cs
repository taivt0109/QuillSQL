using System;
using System.Collections.Generic;
using System.Globalization;
using System.Linq;
using System.Text;
using System.Xml.Linq;
using Microsoft.Data.SqlClient;

namespace QuillSql.Vsix
{
    /// <summary>
    /// The in-depth tools of the AI chat: plans and regressions, blocking and deadlocks, index and
    /// statistics health, storage and the server. Every one only reads.
    /// </summary>
    internal static partial class McpServer
    {
        private static IEnumerable<XElement> Named(XContainer root, string name) => root.Descendants().Where(e => e.Name.LocalName == name);

        private static string Attr(XElement e, string name) => e?.Attribute(name)?.Value;

        private static XDocument ParsePlan(string xml)
        {
            try
            {
                return string.IsNullOrEmpty(xml) ? null : XDocument.Parse(xml);
            }
            catch (System.Xml.XmlException)
            {
                return null;
            }
        }

        /// <summary>"[name]" without its brackets, so names can be given either way.</summary>
        private static string Bare(string name) => name.Trim().TrimStart('[').TrimEnd(']');

        /// <summary>How a plan uses an index: its operators on it, such as "Index Seek, Key Lookup".</summary>
        private static string IndexAccess(string planXml, string index)
        {
            var plan = ParsePlan(planXml);
            if (plan == null)
            {
                return "?";
            }
            var ops = Named(plan, "Object")
                .Where(o => string.Equals(Bare(Attr(o, "Index") ?? ""), index, StringComparison.OrdinalIgnoreCase))
                .Select(o => o.Ancestors().FirstOrDefault(a => a.Name.LocalName == "RelOp"))
                .Where(r => r != null)
                .Select(r => Attr(r, "PhysicalOp") + (Attr(r, "Lookup") == "1" || r.Descendants().Any(d => d.Name.LocalName == "IndexScan" && Attr(d, "Lookup") == "1") ? " (lookup)" : ""))
                .Distinct()
                .ToList();
            return ops.Count == 0 ? "?" : string.Join(", ", ops);
        }

        /// <summary>The warnings of a plan, in a few words each.</summary>
        private static List<string> Warnings(string planXml)
        {
            var found = new List<string>();
            var plan = ParsePlan(planXml);
            if (plan == null)
            {
                return found;
            }
            foreach (var c in Named(plan, "PlanAffectingConvert"))
            {
                found.Add("implicit conversion (" + Attr(c, "ConvertIssue") + "): " + Attr(c, "Expression"));
            }
            foreach (var s in Named(plan, "SpillToTempDb"))
            {
                found.Add("spill to tempdb" + (Attr(s, "SpillLevel") != null ? " (level " + Attr(s, "SpillLevel") + ")" : ""));
            }
            if (Named(plan, "SortSpillDetails").Any() || Named(plan, "HashSpillDetails").Any())
            {
                found.Add("sort or hash spill");
            }
            if (Named(plan, "NoJoinPredicate").Any())
            {
                found.Add("join without a predicate (cartesian product)");
            }
            foreach (var c in Named(plan, "ColumnsWithNoStatistics").SelectMany(x => Named(x, "ColumnReference")).Take(5))
            {
                found.Add("no statistics on " + Bare(Attr(c, "Table") ?? "") + "." + Bare(Attr(c, "Column") ?? ""));
            }
            foreach (var m in Named(plan, "MemoryGrantWarning"))
            {
                found.Add("memory grant " + Attr(m, "GrantWarningKind") + " (requested " + Attr(m, "RequestedMemory") + " KB, used " + Attr(m, "MaxUsedMemory") + " KB)");
            }
            if (Named(plan, "UnmatchedIndexes").Any())
            {
                found.Add("a filtered index could not be used (parameterized predicate)");
            }
            var lookups = Named(plan, "IndexScan").Count(s => Attr(s, "Lookup") == "1");
            if (lookups > 0)
            {
                found.Add(lookups + " key lookup(s)");
            }
            foreach (var g in Named(plan, "MissingIndexGroup").Take(3))
            {
                var index = Named(g, "MissingIndex").FirstOrDefault();
                var columns = Named(g, "ColumnGroup").Select(cg => Attr(cg, "Usage") + ": " + string.Join(", ", Named(cg, "Column").Select(c => Bare(Attr(c, "Name") ?? ""))));
                found.Add("missing index (impact " + Attr(g, "Impact") + "%) on " + Bare(Attr(index, "Table") ?? "") + " " + string.Join("; ", columns));
            }
            if (Named(plan, "TableScan").Any() || Named(plan, "RelOp").Any(r => Attr(r, "PhysicalOp") == "Table Scan"))
            {
                found.Add("table scan of a heap");
            }
            return found;
        }

        /// <summary>The compiled value of each parameter of a plan.</summary>
        private static string CompiledParameters(string planXml)
        {
            var plan = ParsePlan(planXml);
            var values = plan == null ? new List<string>() : Named(plan, "ParameterList").SelectMany(p => Named(p, "ColumnReference"))
                .Select(c => Attr(c, "Column") + " = " + (Attr(c, "ParameterCompiledValue") ?? "?") + (Attr(c, "ParameterRuntimeValue") != null ? " (runtime " + Attr(c, "ParameterRuntimeValue") + ")" : ""))
                .Distinct().ToList();
            return values.Count == 0 ? "(no parameters)" : string.Join(", ", values);
        }

        private static bool IsAzureSqlDatabase(ToolContext s) =>
            Convert.ToInt32(Rows(s, "SELECT CAST(SERVERPROPERTY('EngineEdition') AS int);")[0][0], CultureInfo.InvariantCulture) == 5;

        private static readonly Tool[] DeepTools =
        {
            // ---- Query performance and plans ----
            new Tool
            {
                Name = "queries_using_index",
                Description = "Which queries use an index, and how (seek, scan, lookup): searches the Query Store plans run in the last days and the plan cache, with executions and average duration. Prefer this to searching plan XML yourself.",
                Parameters = new[]
                {
                    new[] { "index", "string", "The index name, with or without brackets.", "required" },
                    new[] { "days", "integer", "Query Store plans run in the last days (1 to 30, default 7).", "" },
                },
                Run = (s, a) =>
                {
                    var index = Bare(Text(a, "index"));
                    var needle = "Index=\"[" + index + "]\"";
                    var sb = new StringBuilder();
                    try
                    {
                        var stored = Rows(s, @"
WITH recent AS (
    SELECT p.plan_id, p.query_id, p.query_plan FROM sys.query_store_plan AS p
    WHERE p.last_execution_time >= DATEADD(day, -@days, SYSUTCDATETIME()) AND CHARINDEX(@needle, p.query_plan) > 0)
SELECT TOP (30) r.query_id, r.plan_id, OBJECT_SCHEMA_NAME(q.object_id) + '.' + OBJECT_NAME(q.object_id),
    SUM(rs.count_executions), CAST(SUM(rs.avg_duration * rs.count_executions) / NULLIF(SUM(rs.count_executions), 0) / 1000.0 AS decimal(18, 1)),
    MAX(rs.last_execution_time), LEFT(qt.query_sql_text, 300), r.query_plan
FROM recent AS r
JOIN sys.query_store_query AS q ON q.query_id = r.query_id
JOIN sys.query_store_query_text AS qt ON qt.query_text_id = q.query_text_id
LEFT JOIN sys.query_store_runtime_stats AS rs ON rs.plan_id = r.plan_id
WHERE " + NotOurs + @"
GROUP BY r.query_id, r.plan_id, q.object_id, qt.query_sql_text, r.query_plan
ORDER BY SUM(rs.avg_duration * rs.count_executions) DESC;", new SqlParameter("@needle", needle), new SqlParameter("@days", Number(a, "days", 7, 1, 30)));
                        sb.Append("Query Store (plans run in the last ").Append(Number(a, "days", 7, 1, 30)).Append(" days): ").Append(stored.Count).Append(" plan(s)\n");
                        foreach (var r in stored)
                        {
                            sb.Append("- query_id ").Append(r[0]).Append(", plan ").Append(r[1]).Append(r[2] != null ? ", in " + r[2] : "").Append(": ").Append(IndexAccess(r[7] as string, index))
                              .Append("; executions ").Append(r[3]).Append(", avg ").Append(r[4]).Append(" ms, last run ").Append(r[5]).Append("\n  ").Append(((r[6] as string) ?? "").Replace("\n", " ")).Append('\n');
                        }
                    }
                    catch (SqlException ex)
                    {
                        sb.Append("Query Store: not searched (" + (ex.Number == -2 ? "timed out; try fewer days" : ex.Message) + ").\n");
                    }
                    try
                    {
                        var cached = Rows(s, @"
SELECT TOP (30) qs.execution_count, CAST(qs.total_elapsed_time / 1000.0 / qs.execution_count AS decimal(18, 1)),
    OBJECT_SCHEMA_NAME(st.objectid, st.dbid) + '.' + OBJECT_NAME(st.objectid, st.dbid),
    SUBSTRING(st.text, qs.statement_start_offset / 2 + 1, 300), tp.query_plan
FROM (SELECT TOP (500) * FROM sys.dm_exec_query_stats ORDER BY total_worker_time DESC) AS qs
CROSS APPLY sys.dm_exec_sql_text(qs.sql_handle) AS st
CROSS APPLY sys.dm_exec_text_query_plan(qs.plan_handle, qs.statement_start_offset, qs.statement_end_offset) AS tp
WHERE st.dbid = DB_ID() AND CHARINDEX(@needle, tp.query_plan) > 0 AND st.text NOT LIKE '%/* QuillSQL tool */%'
ORDER BY qs.total_worker_time DESC;", new SqlParameter("@needle", needle));
                        sb.Append("\nPlan cache (the 500 most expensive statements): ").Append(cached.Count).Append(" statement(s)\n");
                        foreach (var r in cached)
                        {
                            sb.Append("- ").Append(r[2] != null ? r[2] + ": " : "").Append(IndexAccess(r[4] as string, index)).Append("; executions ").Append(r[0]).Append(", avg ").Append(r[1])
                              .Append(" ms\n  ").Append(((r[3] as string) ?? "").Replace("\n", " ")).Append('\n');
                        }
                    }
                    catch (SqlException ex)
                    {
                        sb.Append("\nPlan cache: not searched (" + (ex.Number == -2 ? "timed out" : ex.Message) + ").\n");
                    }
                    return sb.ToString();
                },
            },
            new Tool
            {
                Name = "query_store_regressed",
                Description = "Queries that got slower: average duration in the last hours against the days before, from Query Store, with how many times slower, executions and number of plans. Follow up with query_store_query.",
                Parameters = new[]
                {
                    new[] { "recent_hours", "integer", "The recent period in hours (1 to 168, default 24).", "" },
                    new[] { "days", "integer", "The whole period in days, the rest being the baseline (2 to 60, default 14).", "" },
                },
                Run = (s, a) => Tables(s, @"
DECLARE @now datetimeoffset = SYSDATETIMEOFFSET();
WITH s AS (
    SELECT p.query_id, CASE WHEN i.start_time >= DATEADD(hour, -@hours, @now) THEN 1 ELSE 0 END AS recent,
        SUM(rs.count_executions) AS n, SUM(rs.avg_duration * rs.count_executions) AS total_us, COUNT(DISTINCT p.plan_id) AS plans
    FROM sys.query_store_runtime_stats AS rs
    JOIN sys.query_store_runtime_stats_interval AS i ON i.runtime_stats_interval_id = rs.runtime_stats_interval_id
    JOIN sys.query_store_plan AS p ON p.plan_id = rs.plan_id
    WHERE i.start_time >= DATEADD(day, -@days, @now)
    GROUP BY p.query_id, CASE WHEN i.start_time >= DATEADD(hour, -@hours, @now) THEN 1 ELSE 0 END)
SELECT TOP (20) r.query_id, OBJECT_SCHEMA_NAME(q.object_id) + '.' + OBJECT_NAME(q.object_id) AS [procedure],
    CAST(b.total_us / b.n / 1000.0 AS decimal(18, 1)) AS before_avg_ms, b.n AS before_executions,
    CAST(r.total_us / r.n / 1000.0 AS decimal(18, 1)) AS recent_avg_ms, r.n AS recent_executions,
    CAST((r.total_us / r.n) / NULLIF(b.total_us / b.n, 0) AS decimal(10, 1)) AS times_slower, r.plans AS recent_plans, b.plans AS before_plans,
    qt.query_sql_text AS text
FROM s AS r JOIN s AS b ON b.query_id = r.query_id AND b.recent = 0 AND r.recent = 1
JOIN sys.query_store_query AS q ON q.query_id = r.query_id
JOIN sys.query_store_query_text AS qt ON qt.query_text_id = q.query_text_id
WHERE r.n >= 2 AND b.n >= 2 AND r.total_us / r.n > 1.5 * (b.total_us / b.n) AND r.total_us / r.n > 1000 AND " + NotOurs + @"
ORDER BY (r.total_us / r.n - b.total_us / b.n) * r.n DESC;", 20, 400, null,
                    new SqlParameter("@hours", Number(a, "recent_hours", 24, 1, 168)), new SqlParameter("@days", Number(a, "days", 14, 2, 60))),
            },
            new Tool
            {
                Name = "query_store_waits",
                Description = "What queries waited on (CPU, lock, buffer IO, memory, parallelism, network...) according to Query Store, for one query or the queries that waited most. Needs SQL Server 2017 or later with wait capture.",
                Parameters = new[]
                {
                    new[] { "query_id", "integer", "One query; omit for the queries that waited most.", "" },
                    new[] { "days", "integer", "The last days (1 to 30, default 7).", "" },
                },
                Run = (s, a) =>
                {
                    var q = Number(a, "query_id", 0, 0, int.MaxValue);
                    return Tables(s, @"
SELECT TOP (40) p.query_id, ws.wait_category_desc AS wait_category, SUM(ws.total_query_wait_time_ms) AS wait_ms,
    CAST(SUM(ws.total_query_wait_time_ms) * 1.0 / NULLIF((SELECT SUM(rs.count_executions) FROM sys.query_store_runtime_stats AS rs WHERE rs.plan_id = p.plan_id), 0) AS decimal(18, 1)) AS wait_ms_per_execution,
    LEFT(qt.query_sql_text, 160) AS text
FROM sys.query_store_wait_stats AS ws
JOIN sys.query_store_plan AS p ON p.plan_id = ws.plan_id
JOIN sys.query_store_query AS qq ON qq.query_id = p.query_id
JOIN sys.query_store_query_text AS qt ON qt.query_text_id = qq.query_text_id
JOIN sys.query_store_runtime_stats_interval AS i ON i.runtime_stats_interval_id = ws.runtime_stats_interval_id
WHERE i.start_time >= DATEADD(day, -@days, SYSUTCDATETIME()) AND ws.wait_category_desc <> 'Idle' AND (@q = 0 OR p.query_id = @q) AND " + NotOurs + @"
GROUP BY p.query_id, p.plan_id, ws.wait_category_desc, qt.query_sql_text
ORDER BY wait_ms DESC;", 40, 200, null, new SqlParameter("@q", (long)q), new SqlParameter("@days", Number(a, "days", 7, 1, 30)));
                },
            },
            new Tool
            {
                Name = "plan_warnings",
                Description = "The most expensive cached statements whose plans carry warnings: implicit conversions that prevent seeks, spills to tempdb, joins without a predicate, columns without statistics, excessive memory grants, key lookups, heap scans and missing indexes.",
                Parameters = new[] { new[] { "top", "integer", "How many of the most expensive statements to examine (10 to 200, default 50).", "" } },
                Run = (s, a) =>
                {
                    var rows = Rows(s, @"
SELECT TOP (@top) qs.execution_count, CAST(qs.total_elapsed_time / 1000.0 / qs.execution_count AS decimal(18, 1)),
    CAST(qs.total_worker_time / 1000000.0 AS decimal(18, 1)),
    OBJECT_SCHEMA_NAME(st.objectid, st.dbid) + '.' + OBJECT_NAME(st.objectid, st.dbid),
    SUBSTRING(st.text, qs.statement_start_offset / 2 + 1, 400), tp.query_plan
FROM sys.dm_exec_query_stats AS qs
CROSS APPLY sys.dm_exec_sql_text(qs.sql_handle) AS st
CROSS APPLY sys.dm_exec_text_query_plan(qs.plan_handle, qs.statement_start_offset, qs.statement_end_offset) AS tp
WHERE st.dbid = DB_ID() AND st.text NOT LIKE '%/* QuillSQL tool */%'
ORDER BY qs.total_worker_time DESC;", new SqlParameter("@top", Number(a, "top", 50, 10, 200)));
                    var sb = new StringBuilder();
                    var clean = 0;
                    foreach (var r in rows)
                    {
                        var warnings = Warnings(r[5] as string);
                        if (warnings.Count == 0)
                        {
                            clean++;
                            continue;
                        }
                        sb.Append("- ").Append(r[3] != null ? r[3] + ", " : "").Append("executions ").Append(r[0]).Append(", avg ").Append(r[1]).Append(" ms, total CPU ").Append(r[2]).Append(" s\n  ")
                          .Append(((r[4] as string) ?? "").Replace("\n", " ").Trim()).Append("\n  warnings: ").Append(string.Join("; ", warnings)).Append('\n');
                    }
                    return (sb.Length == 0 ? "No warnings in the plans of the " + rows.Count + " most expensive cached statements." : sb.ToString())
                        + (clean > 0 && sb.Length > 0 ? "\n(" + clean + " other statements had no warnings.)" : "");
                },
            },
            new Tool
            {
                Name = "parameter_sniffing",
                Description = "Signs of parameter sniffing for a procedure (plan cache) or a Query Store query: the parameter values each plan was compiled for, and how much the duration varies (min, max, standard deviation) per plan.",
                Parameters = new[]
                {
                    new[] { "procedure", "string", "schema.name of a procedure.", "" },
                    new[] { "query_id", "integer", "A Query Store query_id.", "" },
                },
                Run = (s, a) =>
                {
                    var procedure = Text(a, "procedure", false);
                    var q = Number(a, "query_id", 0, 0, int.MaxValue);
                    if (procedure == null && q == 0)
                    {
                        throw new ToolException("Give a procedure or a query_id.");
                    }
                    var sb = new StringBuilder();
                    if (procedure != null)
                    {
                        var rows = Rows(s, @"
SELECT ps.execution_count, CAST(ps.total_elapsed_time / 1000.0 / ps.execution_count AS decimal(18, 1)),
    CAST(ps.min_elapsed_time / 1000.0 AS decimal(18, 1)), CAST(ps.max_elapsed_time / 1000.0 AS decimal(18, 1)),
    ps.min_logical_reads, ps.max_logical_reads, ps.cached_time, ps.last_execution_time, qp.query_plan
FROM sys.dm_exec_procedure_stats AS ps OUTER APPLY sys.dm_exec_query_plan(ps.plan_handle) AS qp
WHERE ps.database_id = DB_ID() AND ps.object_id = OBJECT_ID(@p);", new SqlParameter("@p", procedure));
                        if (rows.Count == 0)
                        {
                            sb.Append("The plan cache has no plan of ").Append(procedure).Append(" (not run since it was cached, or no such procedure).\n");
                        }
                        foreach (var r in rows)
                        {
                            sb.Append("Cached plan of ").Append(procedure).Append(": executions ").Append(r[0]).Append(", avg ").Append(r[1]).Append(" ms, min ").Append(r[2]).Append(" ms, max ").Append(r[3])
                              .Append(" ms, reads ").Append(r[4]).Append(" to ").Append(r[5]).Append(", cached ").Append(r[6]).Append(", last run ").Append(r[7])
                              .Append("\n  compiled for: ").Append(CompiledParameters(r[8] as string)).Append('\n');
                        }
                    }
                    if (q > 0)
                    {
                        var plans = Rows(s, @"
SELECT p.plan_id, SUM(rs.count_executions), CAST(SUM(rs.avg_duration * rs.count_executions) / NULLIF(SUM(rs.count_executions), 0) / 1000.0 AS decimal(18, 1)),
    CAST(MIN(rs.min_duration) / 1000.0 AS decimal(18, 1)), CAST(MAX(rs.max_duration) / 1000.0 AS decimal(18, 1)), CAST(MAX(rs.stdev_duration) / 1000.0 AS decimal(18, 1)),
    MIN(rs.min_logical_io_reads), MAX(rs.max_logical_io_reads), p.query_plan
FROM sys.query_store_plan AS p JOIN sys.query_store_runtime_stats AS rs ON rs.plan_id = p.plan_id
WHERE p.query_id = @q GROUP BY p.plan_id, p.query_plan;", new SqlParameter("@q", (long)q));
                        foreach (var r in plans)
                        {
                            sb.Append("Query ").Append(q).Append(", plan ").Append(r[0]).Append(": executions ").Append(r[1]).Append(", avg ").Append(r[2]).Append(" ms, min ").Append(r[3]).Append(" ms, max ").Append(r[4])
                              .Append(" ms, stdev ").Append(r[5]).Append(" ms, reads ").Append(r[6]).Append(" to ").Append(r[7]).Append("\n  compiled for: ").Append(CompiledParameters(r[8] as string)).Append('\n');
                        }
                        if (plans.Count == 0)
                        {
                            sb.Append("Query Store has no plans for query ").Append(q).Append(".\n");
                        }
                    }
                    return sb.ToString();
                },
            },
            new Tool
            {
                Name = "procedure_stats",
                Description = "Stored procedures from the plan cache: executions, average, minimum and maximum duration, CPU and reads, last run. The most expensive, or one procedure.",
                Parameters = new[]
                {
                    new[] { "procedure", "string", "schema.name for one procedure; omit for the most expensive.", "" },
                    new[] { "top", "integer", "How many (1 to 50, default 15).", "" },
                },
                Run = (s, a) => Table(s, @"
SELECT TOP (@top) OBJECT_SCHEMA_NAME(ps.object_id, ps.database_id) + '.' + OBJECT_NAME(ps.object_id, ps.database_id) AS [procedure], ps.execution_count AS executions,
    CAST(ps.total_elapsed_time / 1000.0 / ps.execution_count AS decimal(18, 1)) AS avg_ms, CAST(ps.min_elapsed_time / 1000.0 AS decimal(18, 1)) AS min_ms,
    CAST(ps.max_elapsed_time / 1000.0 AS decimal(18, 1)) AS max_ms, CAST(ps.total_worker_time / 1000.0 / ps.execution_count AS decimal(18, 1)) AS avg_cpu_ms,
    ps.total_logical_reads / ps.execution_count AS avg_reads, CAST(ps.total_elapsed_time / 1000000.0 AS decimal(18, 1)) AS total_s, ps.last_execution_time, ps.cached_time
FROM sys.dm_exec_procedure_stats AS ps
WHERE ps.database_id = DB_ID() AND (@p IS NULL OR ps.object_id = OBJECT_ID(@p))
ORDER BY ps.total_elapsed_time DESC;", 50,
                    new SqlParameter("@top", Number(a, "top", 15, 1, 50)), new SqlParameter("@p", (object)Text(a, "procedure", false) ?? DBNull.Value)),
            },
            new Tool
            {
                Name = "memory_grants",
                Description = "Queries holding or waiting for a memory grant now (requested, granted, used, wait), and the state of the memory semaphores. Use it when queries wait on RESOURCE_SEMAPHORE.",
                Parameters = new string[0][],
                Run = (s, a) => Tables(s, @"
SELECT TOP (25) mg.session_id, mg.requested_memory_kb, mg.granted_memory_kb, mg.used_memory_kb, mg.max_used_memory_kb, mg.wait_time_ms, mg.dop,
    CASE WHEN mg.grant_time IS NULL THEN 'waiting' ELSE 'granted' END AS state, LEFT(t.text, 200) AS text
FROM sys.dm_exec_query_memory_grants AS mg OUTER APPLY sys.dm_exec_sql_text(mg.sql_handle) AS t
WHERE t.text IS NULL OR t.text NOT LIKE '%/* QuillSQL tool */%'
ORDER BY mg.requested_memory_kb DESC;
SELECT resource_semaphore_id, target_memory_kb, available_memory_kb, granted_memory_kb, grantee_count, waiter_count, timeout_error_count
FROM sys.dm_exec_query_resource_semaphores;", 25, 200, new[] { "Memory grants", "Semaphores" }),
            },
            new Tool
            {
                Name = "resource_usage",
                Description = "How busy the server or database has been: on Azure SQL Database the CPU, data IO, log write and memory percentages of the last hour; on SQL Server the CPU of the last half hour, plus memory and page life expectancy.",
                Parameters = new string[0][],
                Run = (s, a) =>
                {
                    if (IsAzureSqlDatabase(s))
                    {
                        return "Azure SQL Database, last hour (15-second samples, newest first):\n" + Tables(s, @"
SELECT CAST(AVG(avg_cpu_percent) AS decimal(5, 1)) AS avg_cpu, CAST(MAX(avg_cpu_percent) AS decimal(5, 1)) AS max_cpu,
    CAST(AVG(avg_data_io_percent) AS decimal(5, 1)) AS avg_data_io, CAST(MAX(avg_data_io_percent) AS decimal(5, 1)) AS max_data_io,
    CAST(AVG(avg_log_write_percent) AS decimal(5, 1)) AS avg_log_write, CAST(MAX(avg_log_write_percent) AS decimal(5, 1)) AS max_log_write,
    CAST(MAX(avg_memory_usage_percent) AS decimal(5, 1)) AS max_memory, CAST(MAX(max_worker_percent) AS decimal(5, 1)) AS max_workers, CAST(MAX(max_session_percent) AS decimal(5, 1)) AS max_sessions
FROM sys.dm_db_resource_stats;
SELECT TOP (20) end_time, avg_cpu_percent AS cpu, avg_data_io_percent AS data_io, avg_log_write_percent AS log_write, avg_memory_usage_percent AS memory, max_worker_percent AS workers
FROM sys.dm_db_resource_stats ORDER BY end_time DESC;", 25, 100, new[] { "Summary", "Recent samples" });
                    }
                    return Tables(s, @"
SELECT TOP (30) DATEADD(ms, -1 * (si.ms_ticks - rb.timestamp), GETDATE()) AS time,
    rb.record.value('(./Record/SchedulerMonitorEvent/SystemHealth/ProcessUtilization)[1]', 'int') AS sql_cpu,
    100 - rb.record.value('(./Record/SchedulerMonitorEvent/SystemHealth/SystemIdle)[1]', 'int') - rb.record.value('(./Record/SchedulerMonitorEvent/SystemHealth/ProcessUtilization)[1]', 'int') AS other_cpu
FROM (SELECT timestamp, CONVERT(xml, record) AS record FROM sys.dm_os_ring_buffers WHERE ring_buffer_type = N'RING_BUFFER_SCHEDULER_MONITOR' AND record LIKE N'%<SystemHealth>%') AS rb
CROSS JOIN sys.dm_os_sys_info AS si ORDER BY rb.timestamp DESC;
SELECT CAST(total_physical_memory_kb / 1024 AS bigint) AS total_mb, CAST(available_physical_memory_kb / 1024 AS bigint) AS available_mb, system_memory_state_desc AS state FROM sys.dm_os_sys_memory;
SELECT RTRIM(counter_name) AS counter, cntr_value AS value FROM sys.dm_os_performance_counters
WHERE (object_name LIKE '%Buffer Manager%' AND counter_name IN ('Page life expectancy', 'Lazy writes/sec'))
   OR (object_name LIKE '%Memory Manager%' AND counter_name IN ('Memory Grants Pending', 'Target Server Memory (KB)', 'Total Server Memory (KB)'));", 30, 100, new[] { "CPU, last 30 minutes (percent)", "Memory", "Counters" });
                },
            },

            // ---- Concurrency ----
            new Tool
            {
                Name = "blocking_chains",
                Description = "Who blocks whom right now, as a tree from each head blocker: session, login, program, wait type and time, wait resource, open transactions and statement. Head blockers that are idle but hold a transaction open are included.",
                Parameters = new string[0][],
                Run = (s, a) =>
                {
                    var rows = Rows(s, @"
WITH blocked AS (SELECT session_id, blocking_session_id FROM sys.dm_exec_requests WHERE blocking_session_id > 0),
involved AS (SELECT session_id FROM blocked UNION SELECT blocking_session_id FROM blocked)
SELECT s.session_id, ISNULL(r.blocking_session_id, 0), r.wait_type, r.wait_time, r.wait_resource, ISNULL(r.status, s.status), s.login_name, s.host_name, s.program_name,
    s.open_transaction_count, DATEDIFF(second, s.last_request_start_time, GETDATE()),
    COALESCE(SUBSTRING(t.text, r.statement_start_offset / 2 + 1,
        (CASE WHEN r.statement_end_offset = -1 THEN DATALENGTH(t.text) ELSE r.statement_end_offset END - r.statement_start_offset) / 2 + 1), lt.text)
FROM involved AS i
JOIN sys.dm_exec_sessions AS s ON s.session_id = i.session_id
LEFT JOIN sys.dm_exec_requests AS r ON r.session_id = s.session_id
OUTER APPLY sys.dm_exec_sql_text(r.sql_handle) AS t
LEFT JOIN sys.dm_exec_connections AS c ON c.session_id = s.session_id
OUTER APPLY sys.dm_exec_sql_text(c.most_recent_sql_handle) AS lt;");
                    if (rows.Count == 0)
                    {
                        return "No blocking right now.";
                    }
                    var bySession = rows.ToDictionary(r => Convert.ToInt32(r[0], CultureInfo.InvariantCulture));
                    var sb = new StringBuilder();
                    void print(int session, int depth)
                    {
                        if (depth > 20 || !bySession.TryGetValue(session, out var r))
                        {
                            return;
                        }
                        var statement = ((r[11] as string) ?? "").Replace("\r", " ").Replace("\n", " ").Trim();
                        sb.Append(new string(' ', depth * 2)).Append(depth == 0 ? "HEAD " : "└ ").Append("session ").Append(r[0]).Append(" (").Append(r[5]).Append(", ").Append(r[6]).Append(", ").Append(r[8])
                          .Append(r[2] != null ? ", waits " + r[2] + " " + r[3] + " ms on " + r[4] : "").Append(", open transactions ").Append(r[9]).Append(", running for ").Append(r[10]).Append(" s): ")
                          .Append(statement.Length > 200 ? statement.Substring(0, 200) + "..." : statement).Append('\n');
                        foreach (var child in rows.Where(x => Convert.ToInt32(x[1], CultureInfo.InvariantCulture) == session))
                        {
                            print(Convert.ToInt32(child[0], CultureInfo.InvariantCulture), depth + 1);
                        }
                    }
                    foreach (var head in rows.Where(r => Convert.ToInt32(r[1], CultureInfo.InvariantCulture) == 0 || !bySession.ContainsKey(Convert.ToInt32(r[1], CultureInfo.InvariantCulture))))
                    {
                        print(Convert.ToInt32(head[0], CultureInfo.InvariantCulture), 0);
                    }
                    return sb.ToString();
                },
            },
            new Tool
            {
                Name = "locks",
                Description = "Locks held or waited for in this database, grouped by session, object, lock type and mode, most first; or only those on one table.",
                Parameters = new[] { new[] { "table", "string", "schema.name to see only one table.", "" } },
                Run = (s, a) => Table(s, @"
SELECT TOP (100) l.request_session_id AS session_id, l.resource_type,
    COALESCE(OBJECT_SCHEMA_NAME(CASE WHEN l.resource_type = 'OBJECT' THEN l.resource_associated_entity_id END) + '.' + OBJECT_NAME(CASE WHEN l.resource_type = 'OBJECT' THEN l.resource_associated_entity_id END),
        OBJECT_SCHEMA_NAME(p.object_id) + '.' + OBJECT_NAME(p.object_id)) AS object,
    l.request_mode AS mode, l.request_status AS status, COUNT(*) AS locks
FROM sys.dm_tran_locks AS l
LEFT JOIN sys.partitions AS p ON p.hobt_id = l.resource_associated_entity_id AND l.resource_type IN ('PAGE', 'KEY', 'RID', 'HOBT')
WHERE l.resource_database_id = DB_ID() AND l.resource_type <> 'DATABASE'
  AND (@t IS NULL OR OBJECT_ID(@t) = CASE WHEN l.resource_type = 'OBJECT' THEN l.resource_associated_entity_id ELSE p.object_id END)
GROUP BY l.request_session_id, l.resource_type, l.resource_associated_entity_id, p.object_id, l.request_mode, l.request_status
ORDER BY CASE l.request_status WHEN 'WAIT' THEN 0 ELSE 1 END, locks DESC;", 100, new SqlParameter("@t", (object)Text(a, "table", false) ?? DBNull.Value)),
            },
            new Tool
            {
                Name = "open_transactions",
                Description = "Transactions open now, oldest first: session, login, program, how long open, state, log used in this database and the last statement. Long-open idle transactions block others and keep the log from shrinking.",
                Parameters = new string[0][],
                Run = (s, a) => Tables(s, @"
SELECT TOP (30) st.session_id, s.login_name, s.host_name, s.program_name, s.status, at.transaction_begin_time AS began,
    DATEDIFF(second, at.transaction_begin_time, GETDATE()) AS open_s,
    CASE at.transaction_state WHEN 0 THEN 'initializing' WHEN 1 THEN 'not started' WHEN 2 THEN 'active' WHEN 3 THEN 'ended (read-only)' WHEN 4 THEN 'committing'
        WHEN 5 THEN 'prepared' WHEN 6 THEN 'committed' WHEN 7 THEN 'rolling back' WHEN 8 THEN 'rolled back' END AS state,
    CAST(dt.database_transaction_log_bytes_used / 1024.0 AS decimal(18, 1)) AS log_kb, LEFT(lt.text, 200) AS last_statement
FROM sys.dm_tran_session_transactions AS st
JOIN sys.dm_tran_active_transactions AS at ON at.transaction_id = st.transaction_id
LEFT JOIN sys.dm_tran_database_transactions AS dt ON dt.transaction_id = st.transaction_id AND dt.database_id = DB_ID()
JOIN sys.dm_exec_sessions AS s ON s.session_id = st.session_id
LEFT JOIN sys.dm_exec_connections AS c ON c.session_id = st.session_id
OUTER APPLY sys.dm_exec_sql_text(c.most_recent_sql_handle) AS lt
WHERE st.session_id <> @@SPID
ORDER BY at.transaction_begin_time;", 30, 200, null),
            },
            new Tool
            {
                Name = "deadlocks",
                Description = "Recent deadlocks from the system_health session (SQL Server; not available on Azure SQL Database): when, the victim, each process's statement, wait and locks, and the contested resources.",
                Parameters = new[] { new[] { "top", "integer", "How many recent deadlocks (1 to 10, default 5).", "" } },
                Run = (s, a) =>
                {
                    if (IsAzureSqlDatabase(s))
                    {
                        throw new ToolException("Azure SQL Database does not expose the system_health session. Deadlocks there are captured with an Extended Events session on the database, or Azure SQL Analytics.");
                    }
                    var rows = Rows(s, @"
SELECT TOP (@top) CAST(event_data AS nvarchar(max))
FROM sys.fn_xe_file_target_read_file('system_health*.xel', NULL, NULL, NULL)
WHERE object_name = 'xml_deadlock_report'
ORDER BY timestamp_utc DESC;", new SqlParameter("@top", Number(a, "top", 5, 1, 10)));
                    if (rows.Count == 0)
                    {
                        return "No deadlocks in the system_health session (it keeps a few days, depending on its file size).";
                    }
                    var sb = new StringBuilder();
                    foreach (var r in rows)
                    {
                        var doc = ParsePlan(r[0] as string);
                        if (doc == null)
                        {
                            continue;
                        }
                        var time = Attr(doc.Root, "timestamp");
                        var victim = Attr(Named(doc, "victimProcess").FirstOrDefault(), "id");
                        sb.Append("Deadlock at ").Append(time).Append(":\n");
                        foreach (var p in Named(doc, "process"))
                        {
                            var input = (Named(p, "inputbuf").FirstOrDefault()?.Value ?? "").Replace("\r", " ").Replace("\n", " ").Trim();
                            var frame = Named(p, "frame").Select(f => Attr(f, "procname")).FirstOrDefault(n => !string.IsNullOrEmpty(n) && n != "adhoc" && n != "unknown");
                            sb.Append(Attr(p, "id") == victim ? "  VICTIM " : "  ").Append("spid ").Append(Attr(p, "spid")).Append(", ").Append(Attr(p, "clientapp")).Append(", ").Append(Attr(p, "loginname"))
                              .Append(", waiting ").Append(Attr(p, "waitresource")).Append(" for ").Append(Attr(p, "lockMode")).Append(", isolation ").Append(Attr(p, "isolationlevel"))
                              .Append(frame != null ? ", in " + frame : "").Append(": ").Append(input.Length > 250 ? input.Substring(0, 250) + "..." : input).Append('\n');
                        }
                        foreach (var resource in Named(doc, "resource-list").SelectMany(l => l.Elements()))
                        {
                            sb.Append("  resource ").Append(resource.Name.LocalName).Append(" on ").Append(Attr(resource, "objectname")).Append(Attr(resource, "indexname") != null ? " index " + Attr(resource, "indexname") : "")
                              .Append(": owners ").Append(string.Join(", ", Named(resource, "owner").Select(o => Attr(o, "id") + " " + Attr(o, "mode"))))
                              .Append("; waiters ").Append(string.Join(", ", Named(resource, "waiter").Select(w => Attr(w, "id") + " " + Attr(w, "mode")))).Append('\n');
                        }
                    }
                    return sb.ToString();
                },
            },

            // ---- Index and statistics health ----
            new Tool
            {
                Name = "unused_indexes",
                Description = "Nonclustered indexes that nobody reads (no seeks, scans or lookups since the server started) but that every write must maintain, by writes and size. Check queries_using_index and the uptime before advising to drop one.",
                Parameters = new string[0][],
                Run = (s, a) => Table(s, @"
SELECT TOP (50) OBJECT_SCHEMA_NAME(i.object_id) + '.' + OBJECT_NAME(i.object_id) AS [table], i.name AS [index], i.type_desc AS kind, i.is_unique AS [unique],
    ISNULL(us.user_updates, 0) AS writes, sz.size_mb, us.last_user_update
FROM sys.indexes AS i
JOIN sys.objects AS o ON o.object_id = i.object_id AND o.type = 'U' AND o.is_ms_shipped = 0
LEFT JOIN sys.dm_db_index_usage_stats AS us ON us.object_id = i.object_id AND us.index_id = i.index_id AND us.database_id = DB_ID()
CROSS APPLY (SELECT CAST(SUM(au.used_pages) * 8 / 1024.0 AS decimal(18, 1)) AS size_mb FROM sys.partitions AS p JOIN sys.allocation_units AS au ON au.container_id = p.partition_id
             WHERE p.object_id = i.object_id AND p.index_id = i.index_id) AS sz
WHERE i.index_id > 1 AND i.is_primary_key = 0 AND i.is_unique_constraint = 0 AND i.is_hypothetical = 0
  AND ISNULL(us.user_seeks, 0) + ISNULL(us.user_scans, 0) + ISNULL(us.user_lookups, 0) = 0
ORDER BY writes DESC, sz.size_mb DESC;", 50),
            },
            new Tool
            {
                Name = "duplicate_indexes",
                Description = "Indexes made redundant by another on the same table: the same key columns, or keys that are the leading keys of a wider index, with the includes of both so you can check the wider one covers it.",
                Parameters = new[] { new[] { "table", "string", "schema.name to check one table; omit for the database.", "" } },
                Run = (s, a) => Table(s, @"
WITH k AS (
    SELECT i.object_id, i.index_id, i.name, i.type_desc, i.is_unique, ISNULL(i.filter_definition, '') AS filter,
        (SELECT c.name + CASE WHEN ic.is_descending_key = 1 THEN ' DESC' ELSE '' END + ','
         FROM sys.index_columns AS ic JOIN sys.columns AS c ON c.object_id = ic.object_id AND c.column_id = ic.column_id
         WHERE ic.object_id = i.object_id AND ic.index_id = i.index_id AND ic.key_ordinal > 0 ORDER BY ic.key_ordinal FOR XML PATH('')) AS keys,
        ISNULL((SELECT c.name + ','
         FROM sys.index_columns AS ic JOIN sys.columns AS c ON c.object_id = ic.object_id AND c.column_id = ic.column_id
         WHERE ic.object_id = i.object_id AND ic.index_id = i.index_id AND ic.is_included_column = 1 ORDER BY c.name FOR XML PATH('')), '') AS includes
    FROM sys.indexes AS i JOIN sys.objects AS o ON o.object_id = i.object_id AND o.type = 'U' AND o.is_ms_shipped = 0
    WHERE i.type IN (1, 2) AND i.is_hypothetical = 0 AND (@t IS NULL OR i.object_id = OBJECT_ID(@t)))
SELECT TOP (60) OBJECT_SCHEMA_NAME(a.object_id) + '.' + OBJECT_NAME(a.object_id) AS [table], a.name AS [index], a.keys, a.includes,
    b.name AS covered_by, b.type_desc AS covered_by_kind, b.keys AS its_keys, b.includes AS its_includes,
    CASE WHEN a.keys = b.keys THEN 'same keys' ELSE 'leading keys' END AS overlap
FROM k AS a JOIN k AS b ON a.object_id = b.object_id AND a.index_id <> b.index_id
    AND LEFT(b.keys, LEN(a.keys)) = a.keys AND (a.keys <> b.keys OR a.index_id > b.index_id) AND a.filter = b.filter
WHERE a.type_desc = 'NONCLUSTERED' AND a.is_unique = 0
ORDER BY [table], a.name;", 60, new SqlParameter("@t", (object)Text(a, "table", false) ?? DBNull.Value)),
            },
            new Tool
            {
                Name = "unindexed_foreign_keys",
                Description = "Foreign keys whose columns are not the leading keys of any index on the referencing table, with its row count: joins on them and deletes of the referenced rows must scan.",
                Parameters = new string[0][],
                Run = (s, a) => Table(s, @"
SELECT TOP (100) OBJECT_SCHEMA_NAME(fk.parent_object_id) + '.' + OBJECT_NAME(fk.parent_object_id) AS [table], fk.name AS foreign_key,
    STUFF((SELECT ', ' + COL_NAME(x.parent_object_id, x.parent_column_id) FROM sys.foreign_key_columns AS x
           WHERE x.constraint_object_id = fk.object_id ORDER BY x.constraint_column_id FOR XML PATH('')), 1, 2, '') AS columns,
    OBJECT_SCHEMA_NAME(fk.referenced_object_id) + '.' + OBJECT_NAME(fk.referenced_object_id) AS [references],
    (SELECT SUM(p.rows) FROM sys.partitions AS p WHERE p.object_id = fk.parent_object_id AND p.index_id IN (0, 1)) AS rows
FROM sys.foreign_keys AS fk
WHERE NOT EXISTS (
    SELECT 1 FROM sys.indexes AS i
    WHERE i.object_id = fk.parent_object_id AND i.type IN (1, 2)
      AND NOT EXISTS (
        SELECT 1 FROM sys.foreign_key_columns AS fc
        WHERE fc.constraint_object_id = fk.object_id
          AND NOT EXISTS (
            SELECT 1 FROM sys.index_columns AS ic
            WHERE ic.object_id = i.object_id AND ic.index_id = i.index_id AND ic.column_id = fc.parent_column_id
              AND ic.key_ordinal BETWEEN 1 AND (SELECT COUNT(*) FROM sys.foreign_key_columns AS n WHERE n.constraint_object_id = fk.object_id))))
ORDER BY rows DESC;", 100),
            },
            new Tool
            {
                Name = "heaps",
                Description = "Tables without a clustered index (heaps), largest first, with their nonclustered indexes, scans, RID lookups and writes since the server started.",
                Parameters = new string[0][],
                Run = (s, a) => Table(s, @"
SELECT TOP (50) OBJECT_SCHEMA_NAME(i.object_id) + '.' + OBJECT_NAME(i.object_id) AS [table],
    (SELECT SUM(p.rows) FROM sys.partitions AS p WHERE p.object_id = i.object_id AND p.index_id = 0) AS rows,
    (SELECT CAST(SUM(au.used_pages) * 8 / 1024.0 AS decimal(18, 1)) FROM sys.partitions AS p JOIN sys.allocation_units AS au ON au.container_id = p.partition_id
     WHERE p.object_id = i.object_id AND p.index_id = 0) AS size_mb,
    (SELECT COUNT(*) FROM sys.indexes AS x WHERE x.object_id = i.object_id AND x.index_id > 1) AS nonclustered_indexes,
    us.user_scans AS scans, us.user_lookups AS rid_lookups, us.user_updates AS writes
FROM sys.indexes AS i
JOIN sys.objects AS o ON o.object_id = i.object_id AND o.type = 'U' AND o.is_ms_shipped = 0
LEFT JOIN sys.dm_db_index_usage_stats AS us ON us.object_id = i.object_id AND us.index_id = 0 AND us.database_id = DB_ID()
WHERE i.index_id = 0
ORDER BY rows DESC;", 50),
            },
            new Tool
            {
                Name = "stale_statistics",
                Description = "Statistics on tables with many rows modified since their last update, or sampled thinly, worst first: when updated, rows, sample percent, rows modified and the modified percent.",
                Parameters = new[] { new[] { "min_rows", "integer", "Only tables with at least this many rows (default 10000).", "" } },
                Run = (s, a) => Table(s, @"
SELECT TOP (50) OBJECT_SCHEMA_NAME(st.object_id) + '.' + OBJECT_NAME(st.object_id) AS [table], st.name AS statistic, st.auto_created, p.last_updated, p.rows,
    CAST(100.0 * p.rows_sampled / NULLIF(p.rows, 0) AS decimal(5, 1)) AS sample_percent, p.modification_counter AS modified,
    CAST(100.0 * p.modification_counter / NULLIF(p.rows, 0) AS decimal(10, 1)) AS modified_percent
FROM sys.stats AS st
JOIN sys.objects AS o ON o.object_id = st.object_id AND o.type = 'U' AND o.is_ms_shipped = 0
CROSS APPLY sys.dm_db_stats_properties(st.object_id, st.stats_id) AS p
WHERE p.rows >= @min
ORDER BY 1.0 * p.modification_counter / NULLIF(p.rows, 0) DESC, p.last_updated;", 50, new SqlParameter("@min", (long)Number(a, "min_rows", 10000, 0, int.MaxValue))),
            },
            new Tool
            {
                Name = "index_operational_stats",
                Description = "How each index of a table is worked since its metadata was cached: inserts, updates, deletes, leaf page allocations (page splits), range scans, singleton lookups, row and page lock waits, latch waits and lock escalations.",
                Parameters = new[] { new[] { "table", "string", "schema.name of the table.", "required" } },
                Run = (s, a) => Table(s, @"
DECLARE @id int = OBJECT_ID(@t);
IF @id IS NULL THROW 50000, 'No table with that name; use search_objects to find it.', 1;
SELECT i.name AS [index], SUM(os.leaf_insert_count) AS inserts, SUM(os.leaf_update_count) AS updates, SUM(os.leaf_delete_count) AS deletes,
    SUM(os.leaf_allocation_count) AS leaf_page_allocations, SUM(os.range_scan_count) AS range_scans, SUM(os.singleton_lookup_count) AS lookups,
    SUM(os.row_lock_wait_count) AS row_lock_waits, SUM(os.row_lock_wait_in_ms) AS row_lock_wait_ms, SUM(os.page_lock_wait_count) AS page_lock_waits,
    SUM(os.page_latch_wait_count) AS page_latch_waits, SUM(os.page_io_latch_wait_in_ms) AS io_latch_wait_ms, SUM(os.index_lock_promotion_count) AS lock_escalations
FROM sys.dm_db_index_operational_stats(DB_ID(), @id, NULL, NULL) AS os
LEFT JOIN sys.indexes AS i ON i.object_id = os.object_id AND i.index_id = os.index_id
GROUP BY os.index_id, i.name ORDER BY os.index_id;", 100, new SqlParameter("@t", Text(a, "table"))),
            },

            // ---- Storage and server ----
            new Tool
            {
                Name = "file_io_latency",
                Description = "Reads, writes and average read and write latency of each file of the database since it started, with sizes: slow data or log files show here.",
                Parameters = new string[0][],
                Run = (s, a) => Table(s, @"
SELECT f.name AS [file], f.type_desc AS kind, CAST(vfs.size_on_disk_bytes / 1048576.0 AS decimal(18, 1)) AS size_mb,
    vfs.num_of_reads AS reads, CAST(vfs.io_stall_read_ms * 1.0 / NULLIF(vfs.num_of_reads, 0) AS decimal(10, 1)) AS avg_read_ms,
    vfs.num_of_writes AS writes, CAST(vfs.io_stall_write_ms * 1.0 / NULLIF(vfs.num_of_writes, 0) AS decimal(10, 1)) AS avg_write_ms,
    CAST(vfs.num_of_bytes_read / 1048576.0 AS decimal(18, 1)) AS read_mb, CAST(vfs.num_of_bytes_written / 1048576.0 AS decimal(18, 1)) AS written_mb
FROM sys.dm_io_virtual_file_stats(DB_ID(), NULL) AS vfs JOIN sys.database_files AS f ON f.file_id = vfs.file_id;", 50),
            },
            new Tool
            {
                Name = "log_usage",
                Description = "The transaction log: size, percent used, why it cannot be reused now (log_reuse_wait), the recovery model and the number of virtual log files.",
                Parameters = new string[0][],
                Run = (s, a) => Table(s, @"
SELECT CAST(u.total_log_size_in_bytes / 1048576.0 AS decimal(18, 1)) AS log_mb, CAST(u.used_log_space_in_percent AS decimal(5, 1)) AS used_percent,
    d.log_reuse_wait_desc AS reuse_waits_on, d.recovery_model_desc AS recovery,
    (SELECT COUNT(*) FROM sys.dm_db_log_info(DB_ID())) AS virtual_log_files
FROM sys.dm_db_log_space_usage AS u CROSS JOIN sys.databases AS d WHERE d.database_id = DB_ID();", 5),
            },
            new Tool
            {
                Name = "tempdb_usage",
                Description = "Sessions using tempdb now: user objects (temp tables, table variables) and internal objects (sorts, hashes, spools), in MB, with login, program and statement.",
                Parameters = new string[0][],
                Run = (s, a) => Tables(s, @"
SELECT TOP (20) su.session_id, s.login_name, s.program_name,
    CAST((su.user_objects_alloc_page_count - su.user_objects_dealloc_page_count) * 8 / 1024.0 AS decimal(18, 1)) AS user_objects_mb,
    CAST((su.internal_objects_alloc_page_count - su.internal_objects_dealloc_page_count) * 8 / 1024.0 AS decimal(18, 1)) AS internal_objects_mb,
    LEFT(lt.text, 200) AS last_statement
FROM sys.dm_db_session_space_usage AS su
JOIN sys.dm_exec_sessions AS s ON s.session_id = su.session_id
LEFT JOIN sys.dm_exec_connections AS c ON c.session_id = su.session_id
OUTER APPLY sys.dm_exec_sql_text(c.most_recent_sql_handle) AS lt
WHERE su.user_objects_alloc_page_count + su.internal_objects_alloc_page_count > 0
ORDER BY (su.user_objects_alloc_page_count - su.user_objects_dealloc_page_count) + (su.internal_objects_alloc_page_count - su.internal_objects_dealloc_page_count) DESC;", 20, 200, null),
            },
            new Tool
            {
                Name = "identity_usage",
                Description = "Identity columns and how close their last value is to the maximum of their type, closest first: an int identity at 90% will soon fail inserts.",
                Parameters = new string[0][],
                Run = (s, a) => Table(s, @"
SELECT TOP (50) OBJECT_SCHEMA_NAME(ic.object_id) + '.' + OBJECT_NAME(ic.object_id) AS [table], ic.name AS [column], TYPE_NAME(ic.system_type_id) AS type,
    CAST(ic.last_value AS decimal(38, 0)) AS last_value, m.max_value,
    CAST(100.0 * CAST(ic.last_value AS decimal(38, 0)) / m.max_value AS decimal(7, 2)) AS used_percent
FROM sys.identity_columns AS ic
JOIN sys.objects AS o ON o.object_id = ic.object_id AND o.type = 'U' AND o.is_ms_shipped = 0
CROSS APPLY (SELECT CAST(CASE TYPE_NAME(ic.system_type_id) WHEN 'tinyint' THEN 255 WHEN 'smallint' THEN 32767 WHEN 'int' THEN 2147483647
                                WHEN 'bigint' THEN 9223372036854775807 END AS decimal(38, 0)) AS max_value) AS m
WHERE ic.last_value IS NOT NULL AND m.max_value IS NOT NULL
ORDER BY used_percent DESC;", 50),
            },
            new Tool
            {
                Name = "recent_changes",
                Description = "Objects created or changed (ALTER, index or constraint changes) in the last days, newest first: useful when something got slower or broke after a deployment.",
                Parameters = new[] { new[] { "days", "integer", "The last days (1 to 90, default 7).", "" } },
                Run = (s, a) => Table(s, @"
SELECT TOP (150) SCHEMA_NAME(schema_id) + '.' + name AS object, type_desc AS kind, create_date AS created, modify_date AS modified,
    CASE WHEN create_date >= DATEADD(day, -@days, GETDATE()) THEN 'new' ELSE 'changed' END AS what
FROM sys.objects
WHERE is_ms_shipped = 0 AND modify_date >= DATEADD(day, -@days, GETDATE()) AND type NOT IN ('S', 'IT', 'SQ')
ORDER BY modify_date DESC;", 150, new SqlParameter("@days", Number(a, "days", 7, 1, 90))),
            },
            new Tool
            {
                Name = "sessions_summary",
                Description = "Connections to this database grouped by login, host and program: how many, how many running, open transactions and last activity.",
                Parameters = new string[0][],
                Run = (s, a) => Table(s, @"
SELECT TOP (40) login_name, host_name, program_name, COUNT(*) AS sessions, SUM(CASE WHEN status = 'running' THEN 1 ELSE 0 END) AS running,
    SUM(open_transaction_count) AS open_transactions, MAX(last_request_end_time) AS last_activity
FROM sys.dm_exec_sessions
WHERE is_user_process = 1 AND database_id = DB_ID()
GROUP BY login_name, host_name, program_name
ORDER BY sessions DESC;", 40),
            },

            // ---- Data ----
            new Tool
            {
                Name = "column_profile",
                Description = "The values of one column: rows, NULLs, distinct values, minimum, maximum and the most frequent values, to judge selectivity and skew. Reads the table (a sample of it when large), so it needs 'Let AI read data'.",
                Parameters = new[]
                {
                    new[] { "table", "string", "schema.name of the table.", "required" },
                    new[] { "column", "string", "The column name.", "required" },
                },
                Run = (s, a) =>
                {
                    if (!s.AllowData)
                    {
                        throw new ToolException("Profiling reads the table, so it is turned off. Use stats_histogram, or ask the developer to turn on 'Let AI read data' below the chat box.");
                    }
                    var info = Rows(s, @"
SELECT QUOTENAME(OBJECT_SCHEMA_NAME(c.object_id)) + '.' + QUOTENAME(OBJECT_NAME(c.object_id)), QUOTENAME(c.name), TYPE_NAME(c.user_type_id),
    (SELECT SUM(p.rows) FROM sys.partitions AS p WHERE p.object_id = c.object_id AND p.index_id IN (0, 1))
FROM sys.columns AS c WHERE c.object_id = OBJECT_ID(@t) AND c.name = @c;", new SqlParameter("@t", Text(a, "table")), new SqlParameter("@c", Bare(Text(a, "column"))));
                    if (info.Count == 0)
                    {
                        throw new ToolException("No column " + Text(a, "column") + " in " + Text(a, "table") + ".");
                    }
                    string table = (string)info[0][0], column = (string)info[0][1], type = (string)info[0][2];
                    var rows = Convert.ToInt64(info[0][3] ?? 0L, CultureInfo.InvariantCulture);
                    if (new[] { "xml", "text", "ntext", "image", "geography", "geometry", "hierarchyid", "sql_variant", "varbinary" }.Contains(type))
                    {
                        throw new ToolException("Columns of type " + type + " cannot be profiled.");
                    }
                    var sample = rows > 2000000 ? " TABLESAMPLE (1000000 ROWS)" : "";
                    // Read uncommitted: a profile is approximate, and must not block or wait on writers.
                    var result = Tables(s, @"
SET TRANSACTION ISOLATION LEVEL READ UNCOMMITTED;
SELECT COUNT_BIG(*) AS rows_read, COUNT_BIG(*) - COUNT_BIG(" + column + ") AS nulls, COUNT_BIG(DISTINCT " + column + @") AS distinct_values,
    CONVERT(nvarchar(200), MIN(" + column + ")) AS minimum, CONVERT(nvarchar(200), MAX(" + column + @")) AS maximum
FROM " + table + sample + @";
SELECT TOP (15) CONVERT(nvarchar(200), " + column + ") AS value, COUNT_BIG(*) AS rows FROM " + table + sample + " GROUP BY " + column + " ORDER BY COUNT_BIG(*) DESC;", 15, 200,
                        new[] { "Profile", "Most frequent values" });
                    return (sample.Length > 0 ? "The table has about " + rows.ToString("N0", CultureInfo.InvariantCulture) + " rows; a sample of about 1,000,000 was read.\n" : "") + result;
                },
            },
        };
    }
}
