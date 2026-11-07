SELECT name, type_desc, default_schema_name FROM sys.database_principals WHERE principal_id BETWEEN 5 AND 16383 ORDER BY name
SELECT USER_NAME(role_principal_id) r, USER_NAME(member_principal_id) m FROM sys.database_role_members WHERE member_principal_id > 4 ORDER BY 1, 2
SELECT class_desc, OBJECT_NAME(major_id) o, permission_name, state_desc, USER_NAME(grantee_principal_id) g FROM sys.database_permissions WHERE grantee_principal_id > 4 AND permission_name <> 'CONNECT' ORDER BY 2, 3, 5
