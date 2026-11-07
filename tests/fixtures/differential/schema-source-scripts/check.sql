SELECT s.name + '.' + o.name n, o.type FROM sys.objects o JOIN sys.schemas s ON s.schema_id = o.schema_id WHERE o.is_ms_shipped = 0 ORDER BY 1
SELECT name, max_length FROM sys.columns WHERE object_id = OBJECT_ID('App.Item') ORDER BY column_id
