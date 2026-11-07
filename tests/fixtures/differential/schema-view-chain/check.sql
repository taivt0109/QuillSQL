SELECT OBJECT_NAME(object_id) v, name FROM sys.columns WHERE object_id IN (OBJECT_ID('dbo.V1'), OBJECT_ID('dbo.V2')) ORDER BY 1, column_id
