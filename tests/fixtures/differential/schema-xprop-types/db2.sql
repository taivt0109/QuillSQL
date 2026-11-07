CREATE ROLE R
GO
CREATE TABLE dbo.T (Id int NOT NULL)
GO
CREATE VIEW dbo.V AS SELECT Id FROM dbo.T
GO
CREATE TYPE dbo.TT AS TABLE (c int NULL)
GO
EXEC sp_addextendedproperty N'Ver', N'1', 'SCHEMA', N'dbo', 'TABLE', N'T'
