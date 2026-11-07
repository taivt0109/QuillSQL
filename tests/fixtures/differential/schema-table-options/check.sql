SELECT name, large_value_types_out_of_row, USER_NAME(principal_id) AS owner FROM sys.tables ORDER BY name
SELECT name, USER_NAME(principal_id) AS owner FROM sys.views ORDER BY name
