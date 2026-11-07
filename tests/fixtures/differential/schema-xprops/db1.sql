CREATE TABLE dbo.T (Id int NOT NULL CONSTRAINT PK_T PRIMARY KEY, A int NULL)
GO
EXEC sp_addextendedproperty N'MS_Description', N'The T table', 'SCHEMA', N'dbo', 'TABLE', N'T'
GO
EXEC sp_addextendedproperty N'MS_Description', N'Column A', 'SCHEMA', N'dbo', 'TABLE', N'T', 'COLUMN', N'A'
