SELECT name, collation_name FROM sys.columns WHERE object_id = OBJECT_ID('dbo.T') ORDER BY column_id
