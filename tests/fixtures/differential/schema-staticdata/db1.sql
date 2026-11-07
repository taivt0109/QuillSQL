CREATE TABLE dbo.Lookup (Id int NOT NULL CONSTRAINT PK_Lookup PRIMARY KEY, Name nvarchar(50) NOT NULL)
GO
INSERT dbo.Lookup VALUES (1, N'one'), (2, N'two'), (3, N'three')
