SELECT name, is_sparse, TYPE_NAME(system_type_id) t FROM sys.columns WHERE object_id = OBJECT_ID('dbo.T') ORDER BY column_id
