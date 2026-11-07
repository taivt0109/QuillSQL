SELECT 'dbo.Orders' AS t, CHECKSUM_AGG(CHECKSUM(*)) c, COUNT(*) n FROM dbo.Orders
UNION ALL
SELECT 'dbo.Other' AS t, CHECKSUM_AGG(CHECKSUM(*)) c, COUNT(*) n FROM dbo.Other
