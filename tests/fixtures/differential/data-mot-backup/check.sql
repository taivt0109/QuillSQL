SELECT 'hot' AS t, * FROM dbo.Hot UNION ALL SELECT 'cold', * FROM dbo.Cold ORDER BY 1, 2
