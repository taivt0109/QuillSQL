SELECT name, TYPE_NAME(system_type_id) t, max_length FROM sys.columns WHERE object_id = OBJECT_ID('dbo.Customer') ORDER BY column_id
SELECT * FROM dbo.Customer ORDER BY Id
