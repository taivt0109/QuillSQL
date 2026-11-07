CREATE TABLE dbo.T (Id int NOT NULL CONSTRAINT PK_T PRIMARY KEY, Name nvarchar(50) NULL, Amount decimal(10,2) NULL)
GO
INSERT dbo.T VALUES (1, N'a', 1)
