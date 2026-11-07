SELECT OBJECT_NAME(object_id) t, name, TYPE_NAME(system_type_id) ty FROM sys.columns WHERE object_id IN (OBJECT_ID('dbo.Parent'), OBJECT_ID('dbo.Child')) ORDER BY 1, column_id
SELECT name FROM sys.foreign_keys
SELECT * FROM dbo.Child
