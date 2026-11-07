CREATE TABLE dbo.Heap (A int, B int)
GO
INSERT dbo.Heap VALUES (1, 5)
GO
CREATE TABLE dbo.T (Id int NOT NULL CONSTRAINT PK_T PRIMARY KEY, Name nvarchar(50) NULL, Amount decimal(10,2) NULL)
GO
INSERT dbo.T VALUES (1, N'b', 1)
