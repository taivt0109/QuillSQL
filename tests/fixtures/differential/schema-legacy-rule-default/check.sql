SELECT OBJECT_NAME(rule_object_id) r, OBJECT_NAME(default_object_id) d FROM sys.columns WHERE object_id = OBJECT_ID('dbo.T') AND name = 'A'
