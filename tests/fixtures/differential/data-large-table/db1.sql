CREATE TABLE dbo.Big (Id int NOT NULL CONSTRAINT PK_Big PRIMARY KEY, V int NOT NULL, S nvarchar(40) NULL)
GO
INSERT dbo.Big SELECT n, n % 97, CONCAT(N'row ', n) FROM (SELECT TOP (20000) ROW_NUMBER() OVER (ORDER BY (SELECT 1)) n FROM sys.all_columns a CROSS JOIN sys.all_columns b) x
GO
