SELECT p.permission_name, p.state_desc, USER_NAME(p.grantee_principal_id) AS grantee FROM sys.database_permissions p WHERE p.class = 3 AND p.major_id = SCHEMA_ID('dbo') ORDER BY 1, 3
