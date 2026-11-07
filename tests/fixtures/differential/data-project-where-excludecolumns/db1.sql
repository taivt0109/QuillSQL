CREATE TABLE dbo.Orders (Id int NOT NULL CONSTRAINT PK_Orders PRIMARY KEY, Region char(2) NOT NULL, Amount int NOT NULL, Note nvarchar(20) NULL)
GO
INSERT dbo.Orders VALUES (1, 'EU', 10, N'a'), (2, 'EU', 20, N'b'), (3, 'US', 30, N'c')
