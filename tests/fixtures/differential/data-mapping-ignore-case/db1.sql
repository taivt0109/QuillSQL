CREATE TABLE dbo.Person (Id int NOT NULL CONSTRAINT PK_Person PRIMARY KEY, FullName nvarchar(50) NULL)
GO
INSERT dbo.Person VALUES (1, N'a'), (2, N'b')
