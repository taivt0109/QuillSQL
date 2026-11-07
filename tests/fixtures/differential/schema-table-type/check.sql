SELECT c.name FROM sys.table_types tt JOIN sys.columns c ON c.object_id = tt.type_table_object_id ORDER BY c.column_id
SELECT name FROM sys.procedures
