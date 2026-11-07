CREATE TABLE dbo.I (Id int IDENTITY(1,1) NOT NULL CONSTRAINT PK_I PRIMARY KEY, Name nvarchar(20) NULL)
GO
INSERT dbo.I (Name) VALUES (N'a'), (N'b'), (N'c'), (N'd')
GO
DELETE dbo.I WHERE Id = 2
