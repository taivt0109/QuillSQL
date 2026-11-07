SELECT 'dbo.Parent' AS t, CHECKSUM_AGG(CHECKSUM(*)) c, COUNT(*) n FROM dbo.Parent
UNION ALL
SELECT 'dbo.Child' AS t, CHECKSUM_AGG(CHECKSUM(*)) c, COUNT(*) n FROM dbo.Child
SELECT name FROM sys.indexes WHERE object_id IN (OBJECT_ID('dbo.Parent'), OBJECT_ID('dbo.Child')) ORDER BY name
