SELECT CAST(seed_value AS int) s, CAST(increment_value AS int) i FROM sys.identity_columns WHERE object_id = OBJECT_ID('dbo.T')
SELECT * FROM dbo.T
