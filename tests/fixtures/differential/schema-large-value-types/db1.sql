CREATE TABLE dbo.T (Id int NOT NULL CONSTRAINT PK_T PRIMARY KEY, Doc nvarchar(max) NULL)
GO
EXEC sp_tableoption N'dbo.T', 'large value types out of row', 1
