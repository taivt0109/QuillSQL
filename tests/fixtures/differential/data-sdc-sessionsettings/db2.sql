CREATE TABLE dbo.T (Id int NOT NULL CONSTRAINT PK_T PRIMARY KEY, Name nvarchar(50) NULL)
GO
INSERT dbo.T VALUES (1, N'same'), (2, N'old value'), (4, N'only in 2')
