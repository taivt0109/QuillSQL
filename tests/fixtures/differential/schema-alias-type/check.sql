SELECT name, max_length FROM sys.types WHERE is_user_defined = 1
SELECT c.name, c.max_length, TYPE_NAME(c.user_type_id) t FROM sys.columns c WHERE object_id = OBJECT_ID('dbo.T') ORDER BY column_id
SELECT * FROM dbo.T
