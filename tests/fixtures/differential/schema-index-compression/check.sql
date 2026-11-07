SELECT i.name, p.data_compression_desc FROM sys.partitions p JOIN sys.indexes i ON i.object_id = p.object_id AND i.index_id = p.index_id WHERE p.object_id = OBJECT_ID('dbo.T') ORDER BY 1
