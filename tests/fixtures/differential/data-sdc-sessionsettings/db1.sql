CREATE TABLE dbo.T (Id int NOT NULL CONSTRAINT PK_T PRIMARY KEY, Name nvarchar(50) NULL)
GO
INSERT dbo.T VALUES (1, N'same'), (2, N'new value'), (3, N'only in 1')
