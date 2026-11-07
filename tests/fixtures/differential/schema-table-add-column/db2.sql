CREATE TABLE dbo.Customer (Id int NOT NULL CONSTRAINT PK_Customer PRIMARY KEY, Name nvarchar(50) NOT NULL)
GO
INSERT dbo.Customer VALUES (1, N'a'), (2, N'b')
GO
CREATE PROCEDURE dbo.Obsolete AS SELECT 1
