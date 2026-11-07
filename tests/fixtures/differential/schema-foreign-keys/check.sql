SELECT name, OBJECT_NAME(parent_object_id) t, delete_referential_action_desc FROM sys.foreign_keys ORDER BY name
