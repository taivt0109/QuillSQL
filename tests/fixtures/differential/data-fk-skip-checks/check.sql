SELECT 'dbo.Parent' AS t, CHECKSUM_AGG(CHECKSUM(*)) c, COUNT(*) n FROM dbo.Parent
UNION ALL
SELECT 'dbo.Child' AS t, CHECKSUM_AGG(CHECKSUM(*)) c, COUNT(*) n FROM dbo.Child
SELECT name, is_disabled, is_not_trusted FROM sys.foreign_keys
