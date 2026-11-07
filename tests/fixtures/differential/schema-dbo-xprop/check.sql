SELECT name, CAST(value AS nvarchar(200)) AS value FROM sys.extended_properties WHERE class = 3 ORDER BY name
