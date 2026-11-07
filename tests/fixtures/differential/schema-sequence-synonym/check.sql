SELECT name, CAST(increment AS bigint) inc, CAST(start_value AS bigint) st FROM sys.sequences ORDER BY name
SELECT name, base_object_name FROM sys.synonyms ORDER BY name
