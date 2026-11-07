SELECT name, type_desc FROM sys.key_constraints UNION ALL SELECT name, type_desc FROM sys.foreign_keys ORDER BY 1
SELECT * FROM dbo.T
