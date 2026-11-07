CREATE TABLE dbo.T (Id int NOT NULL CONSTRAINT PK_T PRIMARY KEY, A int NULL)
GO
EXEC sp_addextendedproperty N'MS_Description', N'Old text', 'SCHEMA', N'dbo', 'TABLE', N'T'
GO
EXEC sp_addextendedproperty N'Owner', N'team', 'SCHEMA', N'dbo', 'TABLE', N'T', 'COLUMN', N'Id'
