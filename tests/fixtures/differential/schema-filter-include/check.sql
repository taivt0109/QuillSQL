SELECT OBJECT_NAME(object_id) t, name FROM sys.columns WHERE object_id IN (OBJECT_ID('dbo.Customer'), OBJECT_ID('dbo.Other')) ORDER BY 1, 2
SELECT name FROM sys.procedures
