SELECT name, TYPE_NAME(system_type_id) t, max_length, precision, scale FROM sys.columns WHERE object_id = OBJECT_ID('dbo.T') ORDER BY column_id
SELECT * FROM dbo.T
