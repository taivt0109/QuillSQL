SELECT 'Filled' AS t, * FROM dbo.Filled UNION ALL SELECT 'Empty', * FROM dbo.Empty ORDER BY 1, 2
