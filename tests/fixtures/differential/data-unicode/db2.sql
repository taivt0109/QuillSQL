CREATE TABLE dbo.U (Id int NOT NULL CONSTRAINT PK_U PRIMARY KEY, N nvarchar(50) NULL)
GO
INSERT dbo.U VALUES (1, N'cafe'), (2, N'日本語'), (3, N'😃 smile'), (4, NCHAR(233))
