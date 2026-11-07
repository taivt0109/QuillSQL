SELECT 'dbo.Big' AS t, CHECKSUM_AGG(CHECKSUM(*)) c, COUNT(*) n FROM dbo.Big
