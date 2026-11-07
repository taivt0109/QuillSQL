CREATE ROLE R
GO
CREATE TABLE dbo.T (Id int NOT NULL)
GO
CREATE VIEW dbo.V AS SELECT Id FROM dbo.T
GO
CREATE TYPE dbo.TT AS TABLE (c int NULL)
GO
EXEC sp_addextendedproperty N'Ver', 1, 'SCHEMA', N'dbo', 'TABLE', N'T'
GO
EXEC sp_addextendedproperty N'MS_Description', N'col', 'SCHEMA', N'dbo', 'VIEW', N'V', 'COLUMN', N'Id'
GO
EXEC sp_addextendedproperty N'MS_Description', N'role', 'USER', N'R'
GO
EXEC sp_addextendedproperty N'MS_Description', N'tt', 'SCHEMA', N'dbo', 'TYPE', N'TT', 'COLUMN', N'c'
