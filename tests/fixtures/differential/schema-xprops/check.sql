SELECT name, minor_id, CAST(value AS nvarchar(100)) v FROM sys.extended_properties WHERE major_id = OBJECT_ID('dbo.T') ORDER BY 1, 2
