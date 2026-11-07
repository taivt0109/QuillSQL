SELECT name, type, OBJECT_DEFINITION(object_id) d FROM sys.objects WHERE is_ms_shipped = 0 AND type IN ('V','P','FN','IF','TF') ORDER BY name
