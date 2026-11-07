SELECT OBJECT_NAME(object_id) t, name, type_desc FROM sys.indexes WHERE object_id IN (OBJECT_ID('dbo.Fact'), OBJECT_ID('dbo.T')) ORDER BY 1, 2
