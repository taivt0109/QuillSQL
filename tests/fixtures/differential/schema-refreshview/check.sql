SELECT OBJECT_NAME(object_id) AS v, name FROM sys.columns WHERE object_id IN (OBJECT_ID('dbo.V'), OBJECT_ID('dbo.VS')) ORDER BY 1, column_id
