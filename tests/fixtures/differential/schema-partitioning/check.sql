SELECT name, fanout FROM sys.partition_functions
SELECT COUNT(*) FROM sys.partitions WHERE object_id = OBJECT_ID('dbo.P')
