using System.Collections.Generic;
using Microsoft.Data.SqlClient;

namespace QuillSql.Vsix
{
    /// <summary>Queries run on the active query window's connection.</summary>
    internal static class Database
    {
        public static string ProcedureDefinition(ConnectionTarget target, string name)
        {
            using (var connection = target.CreateConnection())
            using (var command = new SqlCommand("SELECT OBJECT_DEFINITION(OBJECT_ID(@name, N'P'));", connection) { CommandTimeout = 30 })
            {
                command.Parameters.AddWithValue("@name", name);
                connection.Open();
                return command.ExecuteScalar() as string;
            }
        }

        /// <summary>
        /// Modules that reference objects which no longer exist: `schema.object -> missing name`.
        /// Only references SQL Server could not resolve, inside this database, are reported.
        /// </summary>
        public static List<string> InvalidObjects(ConnectionTarget target)
        {
            const string query = @"
SELECT DISTINCT OBJECT_SCHEMA_NAME(d.referencing_id), OBJECT_NAME(d.referencing_id), o.type_desc,
    CONCAT(QUOTENAME(d.referenced_schema_name) + '.', QUOTENAME(d.referenced_entity_name))
FROM sys.sql_expression_dependencies AS d
JOIN sys.objects AS o ON o.object_id = d.referencing_id
WHERE d.referenced_id IS NULL
  AND d.referenced_server_name IS NULL
  AND d.referenced_database_name IS NULL
  AND d.is_ambiguous = 0
  AND d.referencing_class = 1
  AND d.referenced_entity_name NOT LIKE '#%'
  AND OBJECT_ID(ISNULL(QUOTENAME(d.referenced_schema_name) + '.', '') + QUOTENAME(d.referenced_entity_name)) IS NULL
  AND TYPE_ID(d.referenced_entity_name) IS NULL
  AND NOT EXISTS (SELECT 1 FROM sys.objects AS t WHERE t.name = d.referenced_entity_name AND t.type = 'TT')
ORDER BY 1, 2;";
            var rows = new List<string>();
            using (var connection = target.CreateConnection())
            using (var command = new SqlCommand(query, connection) { CommandTimeout = 120 })
            {
                connection.Open();
                using (var r = command.ExecuteReader())
                {
                    while (r.Read())
                    {
                        rows.Add(r.GetString(0) + "." + r.GetString(1) + " (" + r.GetString(2) + ") references missing " + r.GetString(3));
                    }
                }
            }
            return rows;
        }
    }
}
