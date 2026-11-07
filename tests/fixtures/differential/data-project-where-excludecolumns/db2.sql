CREATE TABLE dbo.Orders (Id int NOT NULL CONSTRAINT PK_Orders PRIMARY KEY, Region char(2) NOT NULL, Amount int NOT NULL, Note nvarchar(20) NULL)
GO
INSERT dbo.Orders VALUES (1, 'EU', 11, N'x'), (4, 'US', 40, N'd'), (5, 'US', 50, N'e')
