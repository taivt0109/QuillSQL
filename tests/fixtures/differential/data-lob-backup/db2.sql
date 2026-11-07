CREATE TABLE dbo.Blob (Id int NOT NULL CONSTRAINT PK_Blob PRIMARY KEY, Data varbinary(max) NULL, Text nvarchar(max) NULL)
GO
INSERT dbo.Blob VALUES (1, CAST(REPLICATE(CAST('b' AS varchar(max)), 2100000) AS varbinary(max)), REPLICATE(CAST(N'y' AS nvarchar(max)), 700000)), (2, 0x01, N'small')
