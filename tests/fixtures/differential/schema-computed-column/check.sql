SELECT name, definition, is_persisted FROM sys.computed_columns WHERE object_id = OBJECT_ID('dbo.T') ORDER BY name
SELECT * FROM dbo.T
